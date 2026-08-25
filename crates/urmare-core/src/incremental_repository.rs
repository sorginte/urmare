use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use urmare_python::{ModuleResolver, PathExcluder, resolve_local_import_with};

use crate::{
    AnalysisError, CacheStats, DependencyEdge, DependencyPath, DependencyStep, GraphCacheStats,
    GraphInspection, GraphSummary, ImpactAttribution, ImpactResult, ImportResolutionTrace,
    RepositoryConfig, RepositoryModule, UnresolvedImport,
    cache::{CacheLocation, content_hash},
    display_repository_path,
    index::{
        BuiltIndex, IndexRead, IndexStore, IndexTimings, IndexWorkStats, StoredFile,
        StoredResolution, build_index, build_index_with_boundary_paths, provenance,
    },
};

/// A query-facing view of one persistent or in-memory repository index.
pub struct RepositoryAnalysis {
    root: PathBuf,
    source_roots: Vec<PathBuf>,
    summary: GraphSummary,
    store: IndexStore,
    overlay: VirtualOverlay,
}

/// Wall-clock timings and bounded-work counts for repository indexing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AnalysisTimings {
    /// Retained for compatibility; full discovery is represented by index work counters.
    pub discovery: Duration,
    /// Retained for compatibility; use `index_work.files_parsed` for stable assertions.
    pub parsing: Duration,
    /// Retained for compatibility; incremental mutation is represented separately.
    pub graph_construction: Duration,
    /// Transactional point-update persistence time.
    pub cache_persistence: Duration,
    /// Legacy parsed-import counters.
    pub cache: CacheStats,
    /// Legacy graph-cache counters.
    pub graph_cache: GraphCacheStats,
    /// True incremental index work performed by this build.
    pub index_work: IndexWorkStats,
    /// Persistent index phase timings.
    pub index_timings: IndexTimings,
}

impl AnalysisTimings {
    pub fn total(self) -> Duration {
        self.index_timings.load
            + self.index_timings.delta_detection
            + self.index_timings.update
            + self.index_timings.persistence
    }
}

#[derive(Clone, Debug, Default)]
struct VirtualOverlay {
    files: BTreeMap<String, StoredFile>,
    forward_overrides: BTreeMap<String, StoredFile>,
    reverse_added: BTreeMap<String, BTreeSet<String>>,
    reverse_removed: BTreeMap<String, BTreeSet<String>>,
    modules: BTreeMap<String, BTreeSet<String>>,
}

impl RepositoryAnalysis {
    pub fn build(root: &Path) -> Result<Self, AnalysisError> {
        Self::build_profiled(root).map(|(repository, _)| repository)
    }

    pub fn build_profiled(root: &Path) -> Result<(Self, AnalysisTimings), AnalysisError> {
        Self::build_profiled_with_virtual_files(root, std::iter::empty(), CacheLocation::Default)
    }

    pub fn build_uncached_profiled(root: &Path) -> Result<(Self, AnalysisTimings), AnalysisError> {
        Self::build_profiled_with_virtual_files(root, std::iter::empty(), CacheLocation::Disabled)
    }

    pub fn build_profiled_with_cache_directory(
        root: &Path,
        cache_directory: &Path,
    ) -> Result<(Self, AnalysisTimings), AnalysisError> {
        Self::build_profiled_with_virtual_files(
            root,
            std::iter::empty(),
            CacheLocation::Directory(cache_directory.to_path_buf()),
        )
    }

    pub(crate) fn build_with_virtual_files<'a>(
        root: &Path,
        virtual_files: impl IntoIterator<Item = &'a Path>,
    ) -> Result<Self, AnalysisError> {
        Self::build_profiled_with_virtual_files(root, virtual_files, CacheLocation::Default)
            .map(|(repository, _)| repository)
    }

    fn build_profiled_with_virtual_files<'a>(
        root: &Path,
        virtual_files: impl IntoIterator<Item = &'a Path>,
        location: CacheLocation,
    ) -> Result<(Self, AnalysisTimings), AnalysisError> {
        let root = root
            .canonicalize()
            .map_err(|source| AnalysisError::RootCanonicalization {
                path: root.to_path_buf(),
                source,
            })?;
        let virtual_files: Vec<_> = virtual_files.into_iter().map(Path::to_path_buf).collect();
        let built = build_index_with_boundary_paths(&root, location, &virtual_files)?;
        let timings = legacy_timings(&built);
        let mut repository = Self {
            root,
            source_roots: built.source_roots,
            summary: built.summary,
            store: built.store,
            overlay: VirtualOverlay::default(),
        };
        if !virtual_files.is_empty() {
            repository.overlay = repository.build_virtual_overlay(&virtual_files)?;
        }
        Ok((repository, timings))
    }

    pub fn summary(&self) -> GraphSummary {
        self.summary.clone()
    }

    pub fn unresolved_imports(&self) -> Result<Vec<UnresolvedImport>, AnalysisError> {
        self.with_reader(|reader| {
            let mut unresolved = reader
                .files()
                .map_err(index_error)?
                .into_iter()
                .flat_map(|file| file.unresolved().collect::<Vec<_>>())
                .collect::<Vec<_>>();
            unresolved.sort_by_key(|item| {
                (
                    display_repository_path(&item.importer),
                    item.location.line,
                    item.location.column,
                )
            });
            Ok(unresolved)
        })
    }

    pub fn graph_inspection(&self, focus: Option<&Path>) -> Result<GraphInspection, AnalysisError> {
        let focus = focus
            .map(|path| self.resolve_input_path(path))
            .transpose()?;
        self.with_reader(|reader| {
            let selected = if let Some(path) = &focus {
                vec![self.file(reader, &display_repository_path(path))?]
            } else {
                reader.files().map_err(index_error)?
            };
            let mut modules = Vec::with_capacity(selected.len());
            for file in &selected {
                let dependencies = file
                    .dependencies
                    .keys()
                    .map(|path| portable_path(path))
                    .collect();
                let dependents = self
                    .reverse(reader, &file.path)?
                    .into_iter()
                    .map(|path| portable_path(&path))
                    .collect();
                modules.push(RepositoryModule {
                    path: file.repository_path(),
                    module: file.module.clone(),
                    is_package: file.is_package,
                    is_test: file.is_test,
                    dependencies,
                    dependents,
                });
            }

            let focus_key = focus.as_deref().map(display_repository_path);
            let all_files = if focus_key.is_some() {
                let focused = selected.first().cloned().ok_or_else(|| {
                    AnalysisError::FileNotIndexed(focus.clone().unwrap_or_default())
                })?;
                let mut files = vec![focused.clone()];
                for dependent in self.reverse(reader, &focused.path)? {
                    files.push(self.file(reader, &dependent)?);
                }
                files
            } else {
                selected.clone()
            };
            let mut edges = Vec::new();
            for file in &all_files {
                for (dependency, imports) in &file.dependencies {
                    if focus_key
                        .as_ref()
                        .is_none_or(|focus| focus == &file.path || focus == dependency)
                    {
                        edges.push(DependencyEdge {
                            dependent: file.repository_path(),
                            dependency: portable_path(dependency),
                            imports: provenance(imports),
                        });
                    }
                }
            }
            edges.sort_by_key(|edge| {
                (
                    display_repository_path(&edge.dependent),
                    display_repository_path(&edge.dependency),
                )
            });
            edges.dedup();

            let traces = selected
                .iter()
                .flat_map(|file| {
                    file.traces(|module| self.module_paths(reader, module).ok()?.first().cloned())
                })
                .collect::<Vec<ImportResolutionTrace>>();
            Ok(GraphInspection {
                focus: focus.clone(),
                source_roots: self.source_roots.clone(),
                modules,
                edges,
                resolution_traces: traces,
            })
        })
    }

    pub fn impact(&self, changed: &Path) -> Result<ImpactResult, AnalysisError> {
        self.impact_many(&[changed.to_path_buf()])
    }

    pub fn impact_many(&self, changed: &[PathBuf]) -> Result<ImpactResult, AnalysisError> {
        if changed.is_empty() {
            return Err(AnalysisError::MissingChangedInput);
        }
        let mut paths = Vec::with_capacity(changed.len());
        for path in changed {
            paths.push(self.resolve_input_path(path)?);
        }
        self.impact_repository_paths(&paths)
    }

    pub(crate) fn impact_repository_paths(
        &self,
        changed: &[PathBuf],
    ) -> Result<ImpactResult, AnalysisError> {
        if changed.is_empty() {
            return Ok(ImpactResult {
                changed: Vec::new(),
                directly_affected: Vec::new(),
                transitively_affected: Vec::new(),
                affected_tests: Vec::new(),
                attributions: Vec::new(),
                full_validation: None,
            });
        }
        self.with_reader(|reader| self.impact_paths(reader, changed))
    }

    pub fn affected_tests(&self, changed: &Path) -> Result<Vec<PathBuf>, AnalysisError> {
        Ok(self.impact(changed)?.affected_tests)
    }

    pub fn affected_tests_many(&self, changed: &[PathBuf]) -> Result<Vec<PathBuf>, AnalysisError> {
        Ok(self.impact_many(changed)?.affected_tests)
    }

    pub fn why(&self, changed: &Path, target: &Path) -> Result<DependencyPath, AnalysisError> {
        let changed = self.resolve_input_path(changed)?;
        let target = self.resolve_input_path(target)?;
        self.why_repository_path(&changed, &target)
    }

    pub(crate) fn why_repository_path(
        &self,
        changed: &Path,
        target: &Path,
    ) -> Result<DependencyPath, AnalysisError> {
        let changed = self.normalize_repository_path(changed)?;
        let target = self.resolve_input_path(target)?;
        self.with_reader(|reader| self.why_paths(reader, &changed, &target))
    }

    pub(crate) fn normalize_repository_path(&self, input: &Path) -> Result<PathBuf, AnalysisError> {
        let relative = if input.is_absolute() {
            input
                .strip_prefix(&self.root)
                .map_err(|_| AnalysisError::InputOutsideRepository {
                    input: input.to_path_buf(),
                    root: self.root.clone(),
                })?
        } else {
            input
        };
        let mut normalized = PathBuf::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(AnalysisError::InputOutsideRepository {
                        input: input.to_path_buf(),
                        root: self.root.clone(),
                    });
                }
            }
        }
        if normalized.as_os_str().is_empty() {
            return Err(AnalysisError::InputOutsideRepository {
                input: input.to_path_buf(),
                root: self.root.clone(),
            });
        }
        Ok(normalized)
    }

    pub(crate) fn current_tests(&self) -> Result<Vec<PathBuf>, AnalysisError> {
        self.with_reader(|reader| {
            Ok(reader
                .files()
                .map_err(index_error)?
                .into_iter()
                .filter(|file| file.is_test)
                .map(|file| file.repository_path())
                .collect())
        })
    }

    fn impact_paths(
        &self,
        reader: &dyn IndexRead,
        changed: &[PathBuf],
    ) -> Result<ImpactResult, AnalysisError> {
        let mut changed_keys: Vec<_> = changed
            .iter()
            .map(|path| display_repository_path(path))
            .collect();
        changed_keys.sort();
        changed_keys.dedup();
        for path in &changed_keys {
            self.file(reader, path)?;
        }
        let changed_set: BTreeSet<_> = changed_keys.iter().cloned().collect();
        let mut direct = BTreeSet::new();
        let mut closure = BTreeSet::new();
        let mut causes = BTreeMap::<String, BTreeSet<String>>::new();

        for changed in &changed_keys {
            let neighbors = self.reverse(reader, changed)?;
            direct.extend(neighbors.iter().cloned());
            let affected = self.reverse_closure(reader, changed)?;
            for path in &affected {
                causes
                    .entry(path.clone())
                    .or_default()
                    .insert(changed.clone());
            }
            closure.extend(affected);
            if self.is_current_test(reader, changed)? {
                causes
                    .entry(changed.clone())
                    .or_default()
                    .insert(changed.clone());
            }
        }
        direct.retain(|path| !changed_set.contains(path));
        closure.retain(|path| !changed_set.contains(path));
        let transitive: BTreeSet<_> = closure.difference(&direct).cloned().collect();
        let mut affected_tests = BTreeSet::new();
        for path in &closure {
            if self.is_current_test(reader, path)? {
                affected_tests.insert(path.clone());
            }
        }
        for changed in &changed_keys {
            if self.is_current_test(reader, changed)? {
                affected_tests.insert(changed.clone());
            }
        }

        let result_paths: BTreeSet<_> = direct
            .iter()
            .chain(&transitive)
            .chain(&affected_tests)
            .cloned()
            .collect();
        let attributions = result_paths
            .into_iter()
            .map(|affected| ImpactAttribution {
                affected: portable_path(&affected),
                caused_by: causes
                    .remove(&affected)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|path| portable_path(&path))
                    .collect(),
            })
            .collect();

        Ok(ImpactResult {
            changed: changed_keys
                .iter()
                .map(|path| portable_path(path))
                .collect(),
            directly_affected: direct.iter().map(|path| portable_path(path)).collect(),
            transitively_affected: transitive.iter().map(|path| portable_path(path)).collect(),
            affected_tests: affected_tests
                .iter()
                .map(|path| portable_path(path))
                .collect(),
            attributions,
            full_validation: None,
        })
    }

    fn reverse_closure(
        &self,
        reader: &dyn IndexRead,
        start: &str,
    ) -> Result<BTreeSet<String>, AnalysisError> {
        let mut visited = BTreeSet::from([start.to_owned()]);
        let mut queue = VecDeque::from([start.to_owned()]);
        while let Some(current) = queue.pop_front() {
            for dependent in self.reverse(reader, &current)? {
                if visited.insert(dependent.clone()) {
                    queue.push_back(dependent);
                }
            }
        }
        visited.remove(start);
        Ok(visited)
    }

    fn why_paths(
        &self,
        reader: &dyn IndexRead,
        changed: &Path,
        target: &Path,
    ) -> Result<DependencyPath, AnalysisError> {
        let changed_key = display_repository_path(changed);
        let target_key = display_repository_path(target);
        self.file(reader, &changed_key)?;
        self.file(reader, &target_key)?;

        let mut parents = BTreeMap::<String, String>::new();
        let mut visited = BTreeSet::from([target_key.clone()]);
        let mut queue = VecDeque::from([target_key.clone()]);
        let mut found = target_key == changed_key;
        while let Some(current) = queue.pop_front() {
            let file = self.file(reader, &current)?;
            for dependency in file.dependencies.keys() {
                if !visited.insert(dependency.clone()) {
                    continue;
                }
                parents.insert(dependency.clone(), current.clone());
                if dependency == &changed_key {
                    found = true;
                    queue.clear();
                    break;
                }
                queue.push_back(dependency.clone());
            }
        }
        if !found {
            return Err(AnalysisError::NoDependencyPath {
                changed: changed.to_path_buf(),
                target: target.to_path_buf(),
            });
        }

        let mut reversed = vec![changed_key.clone()];
        let mut current = changed_key.clone();
        while current != target_key {
            let Some(parent) = parents.get(&current) else {
                break;
            };
            reversed.push(parent.clone());
            current = parent.clone();
        }
        reversed.reverse();
        let mut steps = Vec::with_capacity(reversed.len().saturating_sub(1));
        for pair in reversed.windows(2) {
            let dependent = self.file(reader, &pair[0])?;
            let imports = dependent.dependencies.get(&pair[1]).ok_or_else(|| {
                AnalysisError::MissingEdgeProvenance {
                    dependent: portable_path(&pair[0]),
                    dependency: portable_path(&pair[1]),
                }
            })?;
            steps.push(DependencyStep {
                dependent: portable_path(&pair[0]),
                dependency: portable_path(&pair[1]),
                imports: provenance(imports),
            });
        }
        Ok(DependencyPath {
            changed: changed.to_path_buf(),
            affected: target.to_path_buf(),
            path: reversed.iter().map(|path| portable_path(path)).collect(),
            steps,
        })
    }

    fn resolve_input_path(&self, input: &Path) -> Result<PathBuf, AnalysisError> {
        let candidate = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.root.join(input)
        };
        let canonical = candidate
            .canonicalize()
            .map_err(|_| AnalysisError::InputNotFound(input.to_path_buf()))?;
        let relative = canonical.strip_prefix(&self.root).map_err(|_| {
            AnalysisError::InputOutsideRepository {
                input: input.to_path_buf(),
                root: self.root.clone(),
            }
        })?;
        let path = relative.to_path_buf();
        self.with_reader(|reader| {
            self.file(reader, &display_repository_path(&path))?;
            Ok(path.clone())
        })
    }

    fn file(&self, reader: &dyn IndexRead, path: &str) -> Result<StoredFile, AnalysisError> {
        if let Some(file) = self.overlay.forward_overrides.get(path) {
            return Ok(file.clone());
        }
        if let Some(file) = self.overlay.files.get(path) {
            return Ok(file.clone());
        }
        reader
            .file(path)
            .map_err(index_error)?
            .ok_or_else(|| AnalysisError::FileNotIndexed(portable_path(path)))
    }

    fn reverse(&self, reader: &dyn IndexRead, path: &str) -> Result<Vec<String>, AnalysisError> {
        let mut paths: BTreeSet<_> = reader
            .reverse_dependents(path)
            .map_err(index_error)?
            .into_iter()
            .collect();
        if let Some(removed) = self.overlay.reverse_removed.get(path) {
            paths.retain(|candidate| !removed.contains(candidate));
        }
        if let Some(added) = self.overlay.reverse_added.get(path) {
            paths.extend(added.iter().cloned());
        }
        Ok(paths.into_iter().collect())
    }

    fn module_paths(
        &self,
        reader: &dyn IndexRead,
        module: &str,
    ) -> Result<Vec<String>, AnalysisError> {
        let mut paths = BTreeSet::new();
        if let Some(current) = reader.module_path(module).map_err(index_error)? {
            paths.insert(current);
        }
        if let Some(virtual_paths) = self.overlay.modules.get(module) {
            paths.extend(virtual_paths.iter().cloned());
        }
        Ok(paths.into_iter().collect())
    }

    fn is_current_test(&self, reader: &dyn IndexRead, path: &str) -> Result<bool, AnalysisError> {
        Ok(reader
            .file(path)
            .map_err(index_error)?
            .is_some_and(|file| file.is_test))
    }

    fn with_reader<T>(
        &self,
        mut operation: impl FnMut(&dyn IndexRead) -> Result<T, AnalysisError>,
    ) -> Result<T, AnalysisError> {
        match self.store.read(&mut operation) {
            Ok(value) => Ok(value),
            Err(AnalysisError::IndexUnavailable(_)) => {
                let fallback = build_index(&self.root, CacheLocation::Disabled)?;
                fallback.store.read(operation)
            }
            Err(error) => Err(error),
        }
    }

    fn build_virtual_overlay(
        &self,
        virtual_paths: &[PathBuf],
    ) -> Result<VirtualOverlay, AnalysisError> {
        let configuration = RepositoryConfig::load(&self.root)?;
        let excluder = configuration.path_excluder()?;
        let resolver = ModuleResolver::with_source_roots(self.source_roots.iter().cloned())
            .with_test_roots(
                configuration
                    .test_roots()
                    .unwrap_or_default()
                    .iter()
                    .cloned(),
            );
        self.with_reader(|reader| {
            let mut overlay = VirtualOverlay::default();
            for path in virtual_paths {
                let path = self.normalize_repository_path(path)?;
                if excluded_virtual_path(&path, &excluder) {
                    continue;
                }
                let key = display_repository_path(&path);
                if reader.file(&key).map_err(index_error)?.is_some() {
                    continue;
                }
                let file = resolver.module_for_path(&path)?;
                overlay
                    .modules
                    .entry(file.module.clone())
                    .or_default()
                    .insert(key.clone());
                overlay.files.insert(
                    key.clone(),
                    StoredFile {
                        path: key,
                        module: file.module,
                        is_package: file.is_package,
                        is_test: file.is_test,
                        imports: Vec::new(),
                        resolutions: Vec::new(),
                        dependencies: BTreeMap::new(),
                        state: virtual_state(),
                        content_hash: content_hash(""),
                    },
                );
            }

            let mut importers = BTreeSet::new();
            for module in overlay.modules.keys() {
                importers.extend(reader.candidate_importers(module).map_err(index_error)?);
            }
            for importer in importers {
                let Some(base) = reader.file(&importer).map_err(index_error)? else {
                    continue;
                };
                let mut updated = base.clone();
                resolve_overlay_file(reader, &overlay, &mut updated)?;
                let old_dependencies: BTreeSet<_> = base.dependencies.keys().cloned().collect();
                let new_dependencies: BTreeSet<_> = updated.dependencies.keys().cloned().collect();
                for removed in old_dependencies.difference(&new_dependencies) {
                    overlay
                        .reverse_removed
                        .entry(removed.clone())
                        .or_default()
                        .insert(importer.clone());
                }
                for added in new_dependencies.difference(&old_dependencies) {
                    overlay
                        .reverse_added
                        .entry(added.clone())
                        .or_default()
                        .insert(importer.clone());
                }
                overlay.forward_overrides.insert(importer, updated);
            }
            Ok(overlay)
        })
    }
}

fn resolve_overlay_file(
    reader: &dyn IndexRead,
    overlay: &VirtualOverlay,
    file: &mut StoredFile,
) -> Result<(), AnalysisError> {
    let python_file = file.python_file();
    file.resolutions.clear();
    file.dependencies.clear();
    for import in &file.imports {
        let resolution = resolve_local_import_with(&python_file, &import.import, |module| {
            overlay.modules.contains_key(module)
                || reader.module_path(module).ok().flatten().is_some()
        });
        for module in &resolution.resolved_modules {
            let mut paths = BTreeSet::new();
            if let Some(current) = reader.module_path(module).map_err(index_error)? {
                paths.insert(current);
            }
            if let Some(virtual_paths) = overlay.modules.get(module) {
                paths.extend(virtual_paths.iter().cloned());
            }
            for path in paths {
                if module == &file.module && path == file.path {
                    continue;
                }
                let evidence = file.dependencies.entry(path).or_default();
                if !evidence.contains(import) {
                    evidence.push(import.clone());
                }
            }
        }
        file.resolutions.push(StoredResolution {
            import: import.clone(),
            resolution,
        });
    }
    Ok(())
}

fn excluded_virtual_path(path: &Path, excluder: &PathExcluder) -> bool {
    excluder.is_excluded(path)
        || path.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some(
                    ".git"
                        | ".venv"
                        | "venv"
                        | ".tox"
                        | "__pycache__"
                        | ".mypy_cache"
                        | ".pytest_cache"
                        | ".ruff_cache"
                )
            )
        })
}

fn legacy_timings(built: &BuiltIndex) -> AnalysisTimings {
    AnalysisTimings {
        discovery: built.timings.delta_detection,
        parsing: Duration::ZERO,
        graph_construction: built.timings.update,
        cache_persistence: built.timings.persistence,
        cache: CacheStats {
            metadata_hits: 0,
            content_hits: 0,
            misses: built.work.files_parsed,
        },
        graph_cache: GraphCacheStats {
            module_hits: built.work.modules_reused,
            edge_hits: 0,
            edge_misses: built.work.importers_reresolved,
        },
        index_work: built.work,
        index_timings: built.timings,
    }
}

fn portable_path(path: &str) -> PathBuf {
    path.split('/').collect()
}

fn index_error(error: String) -> AnalysisError {
    AnalysisError::IndexUnavailable(error)
}

// Virtual records never represent a current-tree file, so their filesystem
// metadata is deliberately a neutral internal placeholder.
fn virtual_state() -> crate::index::StoredFileState {
    crate::index::StoredFileState {
        size: 0,
        modified_ns: None,
    }
}
