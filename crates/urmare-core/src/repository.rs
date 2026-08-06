use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use urmare_graph::{DirectedGraph, NodeId};
use urmare_python::{
    ImportResolutionFailure, LocalImportResolver, PythonFile, discover_python_files_with_excluder,
    parse_imports_with_locations,
};

use crate::{
    AnalysisError, CacheStats, DependencyEdge, DependencyPath, DependencyStep, GraphCacheStats,
    GraphInspection, GraphSummary, ImpactAttribution, ImpactResult, ImportProvenance,
    ImportResolutionStatus, ImportResolutionTrace, RepositoryConfig, RepositoryModule,
    ResolvedLocalModule, UnresolvedImport,
    cache::{CacheConfiguration, CacheLocation, FileState, ImportCache, content_hash},
    display_repository_path,
    graph_cache::{CachedImportResolution, GraphCache},
};

/// A fully indexed, immutable view of one Python repository.
pub struct RepositoryAnalysis {
    root: PathBuf,
    source_roots: Vec<PathBuf>,
    files: Vec<PythonFile>,
    graph: DirectedGraph,
    nodes_by_path: HashMap<PathBuf, NodeId>,
    present_nodes: HashSet<NodeId>,
    edge_provenance: HashMap<(NodeId, NodeId), Vec<ImportProvenance>>,
    resolution_traces: Vec<ImportResolutionTrace>,
    unresolved_imports: Vec<UnresolvedImport>,
}

/// Wall-clock timings for the independently measurable repository-build phases.
///
/// These timings are intended for reproducible development benchmarks, not as
/// stable machine-independent performance claims.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AnalysisTimings {
    /// Repository discovery, path normalization, and configuration loading.
    pub discovery: Duration,
    /// Python source reads and AST import extraction.
    pub parsing: Duration,
    /// Module mapping, local import resolution, and graph allocation.
    pub graph_construction: Duration,
    /// Best-effort persistence of changed parsed-import and graph-index entries.
    pub cache_persistence: Duration,
    /// Parsed-import cache reuse and invalidation counts for present Python files.
    pub cache: CacheStats,
    /// Persistent graph-index reuse and invalidation counts.
    pub graph_cache: GraphCacheStats,
}

impl AnalysisTimings {
    /// Total time represented by the measured build phases.
    pub fn total(self) -> Duration {
        self.discovery + self.parsing + self.graph_construction + self.cache_persistence
    }
}

impl RepositoryAnalysis {
    /// Discovers, parses, and indexes the selected repository root.
    pub fn build(root: &Path) -> Result<Self, AnalysisError> {
        Self::build_profiled(root).map(|(repository, _)| repository)
    }

    /// Builds a repository and returns timings from the real analysis phases.
    pub fn build_profiled(root: &Path) -> Result<(Self, AnalysisTimings), AnalysisError> {
        Self::build_profiled_with_virtual_files(root, std::iter::empty(), CacheLocation::Default)
    }

    /// Builds without reading or writing the persistent analysis caches.
    ///
    /// This is primarily useful for measuring cold analysis independently from
    /// persistent incremental behavior.
    pub fn build_uncached_profiled(root: &Path) -> Result<(Self, AnalysisTimings), AnalysisError> {
        Self::build_profiled_with_virtual_files(root, std::iter::empty(), CacheLocation::Disabled)
    }

    /// Builds with analysis state persisted beneath a caller-provided directory.
    pub fn build_profiled_with_cache_directory(
        root: &Path,
        cache_directory: &Path,
    ) -> Result<(Self, AnalysisTimings), AnalysisError> {
        RepositoryAnalysis::build_profiled_with_virtual_files(
            root,
            std::iter::empty(),
            CacheLocation::Directory(cache_directory.to_path_buf()),
        )
    }

    /// Builds a current-tree graph augmented with identities removed by a change set.
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
        cache_location: CacheLocation,
    ) -> Result<(Self, AnalysisTimings), AnalysisError> {
        let discovery_started = Instant::now();
        let configuration = RepositoryConfig::load(root)?;
        let excluder = configuration.path_excluder()?;
        let discovered = discover_python_files_with_excluder(root, &excluder)?;
        let present_paths: HashSet<_> = discovered.iter().cloned().collect();
        let mut indexed_paths = discovered;
        indexed_paths.extend(
            virtual_files
                .into_iter()
                .filter(|path| !excluder.is_excluded(path))
                .map(Path::to_path_buf),
        );
        indexed_paths.sort();
        indexed_paths.dedup();

        if indexed_paths.is_empty() {
            return Err(AnalysisError::NoPythonFiles {
                root: root.to_path_buf(),
            });
        }

        let canonical_root =
            root.canonicalize()
                .map_err(|source| AnalysisError::RootCanonicalization {
                    path: root.to_path_buf(),
                    source,
                })?;
        let discovery = discovery_started.elapsed();

        let graph_setup_started = Instant::now();
        let module_resolver = configuration.module_resolver(&canonical_root, &indexed_paths)?;
        let cache_configuration = CacheConfiguration {
            source_roots: module_resolver.source_roots(),
            test_roots: module_resolver.test_roots(),
            excludes: configuration.excludes(),
        };
        let source_roots = module_resolver.source_roots().to_vec();
        let mut graph_cache =
            GraphCache::load(cache_location.clone(), &canonical_root, cache_configuration);
        let mut files = Vec::with_capacity(indexed_paths.len());
        let mut module_paths: HashMap<String, PathBuf> = HashMap::new();

        for path in indexed_paths {
            let file = match graph_cache.cached_module(&path) {
                Some(module) => module_resolver.file_with_module_identity(&path, module)?,
                None => module_resolver.module_for_path(&path)?,
            };
            if let Some(first) = module_paths.insert(file.module.clone(), file.path.clone()) {
                return Err(AnalysisError::DuplicateModule {
                    module: file.module,
                    first,
                    second: file.path,
                });
            }
            files.push(file);
        }

        // Discovery is sorted, but sorting by normalized output form keeps node
        // allocation deterministic even across native separator conventions.
        files.sort_by_key(|file| display_repository_path(&file.path));
        let reusable_resolutions = graph_cache.begin_resolution(&files);
        let graph_setup = graph_setup_started.elapsed();

        let parsing_started = Instant::now();
        let mut cache = ImportCache::load(cache_location, &canonical_root, cache_configuration);
        let mut imports_by_file = Vec::with_capacity(files.len());
        let mut imports_reused = Vec::with_capacity(files.len());
        for file in &files {
            if !present_paths.contains(&file.path) {
                imports_by_file.push(Vec::new());
                imports_reused.push(false);
                continue;
            }
            let absolute_path = canonical_root.join(&file.path);
            if cache.is_enabled() {
                let metadata =
                    fs::metadata(&absolute_path).map_err(|source| AnalysisError::SourceRead {
                        path: file.path.clone(),
                        source,
                    })?;
                let state = FileState::from_metadata(&metadata);
                if let Some(imports) = cache.metadata_hit(&file.path, state) {
                    imports_by_file.push(imports);
                    imports_reused.push(true);
                    continue;
                }

                let source = fs::read_to_string(&absolute_path).map_err(|source| {
                    AnalysisError::SourceRead {
                        path: file.path.clone(),
                        source,
                    }
                })?;
                let hash = content_hash(&source);
                if let Some(imports) = cache.content_hit(&file.path, state, &hash) {
                    imports_by_file.push(imports);
                    imports_reused.push(true);
                    continue;
                }
                let imports = parse_imports_with_locations(&source, &file.path)?;
                cache.record_parsed(&file.path, state, hash, &imports);
                imports_by_file.push(imports);
                imports_reused.push(false);
                continue;
            }

            let source =
                fs::read_to_string(&absolute_path).map_err(|source| AnalysisError::SourceRead {
                    path: file.path.clone(),
                    source,
                })?;
            imports_by_file.push(parse_imports_with_locations(&source, &file.path)?);
            imports_reused.push(false);
            cache.record_uncached_parse();
        }
        let parsing = parsing_started.elapsed();

        let graph_started = Instant::now();
        let mut graph = DirectedGraph::new();
        let mut nodes_by_path = HashMap::with_capacity(files.len());
        let mut nodes_by_module = HashMap::with_capacity(files.len());
        let mut present_nodes = HashSet::with_capacity(present_paths.len());
        let mut nodes = Vec::with_capacity(files.len());
        for file in &files {
            let node = graph.add_node();
            nodes.push(node);
            if present_paths.contains(&file.path) {
                present_nodes.insert(node);
            }
            nodes_by_path.insert(file.path.clone(), node);
            nodes_by_module.insert(file.module.clone(), node);
        }

        let local_resolver =
            LocalImportResolver::new(files.iter().map(|file| file.module.as_str()));
        let mut edge_provenance: HashMap<_, Vec<ImportProvenance>> = HashMap::new();
        let mut resolution_traces = Vec::new();
        let mut unresolved_imports = Vec::new();

        for (index, (file, imports)) in files.iter().zip(&imports_by_file).enumerate() {
            let Some(&importer) = nodes.get(index) else {
                return Err(AnalysisError::MissingNodeMetadata(index));
            };
            if !present_paths.contains(&file.path) {
                continue;
            }

            let cached_resolution = (reusable_resolutions
                && imports_reused.get(index).copied().unwrap_or(false))
            .then(|| graph_cache.cached_resolution(&file.path, &file.module))
            .flatten()
            .filter(|resolution| {
                resolution.imports.len() == imports.len()
                    && resolution
                        .imports
                        .iter()
                        .zip(imports)
                        .all(|(cached, current)| cached.import == *current)
                    && resolution
                        .imports
                        .iter()
                        .flat_map(|import| &import.resolution.resolved_modules)
                        .all(|module| nodes_by_module.contains_key(module))
            });

            let resolutions = if let Some(resolution) = cached_resolution {
                graph_cache.record_reused(&file.path);
                resolution.imports
            } else {
                let resolutions: Vec<_> = imports
                    .iter()
                    .map(|import| CachedImportResolution {
                        import: import.clone(),
                        resolution: local_resolver.resolve_with_trace(file, &import.import),
                    })
                    .collect();
                graph_cache.record_resolved(&file.path, &file.module, resolutions.clone());
                resolutions
            };

            for resolved_import in resolutions {
                let status = if !resolved_import.resolution.resolved_modules.is_empty() {
                    ImportResolutionStatus::Resolved
                } else if resolved_import.resolution.failure
                    == Some(ImportResolutionFailure::RelativeBeyondTopLevel)
                {
                    ImportResolutionStatus::InvalidRelativeImport
                } else {
                    ImportResolutionStatus::Unresolved
                };

                if status != ImportResolutionStatus::Resolved {
                    unresolved_imports.push(UnresolvedImport {
                        importer: file.path.clone(),
                        location: resolved_import.import.location,
                        import: resolved_import.import.import.clone(),
                    });
                }

                let mut resolved_modules =
                    Vec::with_capacity(resolved_import.resolution.resolved_modules.len());
                for module in &resolved_import.resolution.resolved_modules {
                    let Some(&dependency) = nodes_by_module.get(module) else {
                        return Err(AnalysisError::MissingModule(module.clone()));
                    };
                    let dependency_path = files
                        .get(dependency.index())
                        .map(|dependency_file| dependency_file.path.clone())
                        .ok_or(AnalysisError::MissingNodeMetadata(dependency.index()))?;
                    resolved_modules.push(ResolvedLocalModule {
                        module: module.clone(),
                        path: dependency_path,
                    });

                    if module == &file.module {
                        continue;
                    }
                    graph.add_edge(importer, dependency)?;
                    let provenance = ImportProvenance {
                        location: resolved_import.import.location,
                        import: resolved_import.import.import.clone(),
                    };
                    let evidence = edge_provenance.entry((importer, dependency)).or_default();
                    if !evidence.contains(&provenance) {
                        evidence.push(provenance);
                    }
                }

                resolution_traces.push(ImportResolutionTrace {
                    importer: file.path.clone(),
                    location: resolved_import.import.location,
                    import: resolved_import.import.import,
                    candidate_modules: resolved_import.resolution.candidate_modules,
                    resolved_modules,
                    status,
                });
            }
        }
        let graph_construction = graph_setup + graph_started.elapsed();
        let cache_stats = cache.stats();
        let graph_cache_stats = graph_cache.stats();
        let cache_started = Instant::now();
        let _ = cache.persist();
        let _ = graph_cache.persist();
        let cache_persistence = cache_started.elapsed();

        Ok((
            Self {
                root: canonical_root,
                source_roots,
                files,
                graph,
                nodes_by_path,
                present_nodes,
                edge_provenance,
                resolution_traces,
                unresolved_imports,
            },
            AnalysisTimings {
                discovery,
                parsing,
                graph_construction,
                cache_persistence,
                cache: cache_stats,
                graph_cache: graph_cache_stats,
            },
        ))
    }

    /// Returns repository graph statistics for presentation or automation.
    pub fn summary(&self) -> GraphSummary {
        GraphSummary {
            python_files: self.files.len(),
            modules: self.files.len(),
            import_edges: self.graph.edge_count(),
            tests: self.files.iter().filter(|file| file.is_test).count(),
            unresolved_imports: self.unresolved_imports.len(),
        }
    }

    /// Returns unresolved local import attempts in deterministic source order.
    pub fn unresolved_imports(&self) -> &[UnresolvedImport] {
        &self.unresolved_imports
    }

    /// Returns deterministic module mappings, resolved edges, and resolution traces.
    ///
    /// When `focus` is provided, modules and traces are restricted to that
    /// importer while edges include both its dependencies and dependents.
    pub fn graph_inspection(&self, focus: Option<&Path>) -> Result<GraphInspection, AnalysisError> {
        let focused = focus.map(|path| self.resolve_input(path)).transpose()?;
        let focus_path = focused.as_ref().map(|(path, _)| path.clone());
        let focus_node = focused.map(|(_, node)| node);

        let selected_nodes: Vec<_> = match focus_node {
            Some(node) => vec![node],
            None => self
                .files
                .iter()
                .filter_map(|file| self.nodes_by_path.get(&file.path).copied())
                .filter(|node| self.present_nodes.contains(node))
                .collect(),
        };
        let mut modules = Vec::with_capacity(selected_nodes.len());
        for node in selected_nodes {
            let file = self
                .files
                .get(node.index())
                .ok_or(AnalysisError::MissingNodeMetadata(node.index()))?;
            modules.push(RepositoryModule {
                path: file.path.clone(),
                module: file.module.clone(),
                is_package: file.is_package,
                is_test: file.is_test,
                dependencies: self
                    .sorted_paths(self.graph.forward_neighbors(node)?.iter().copied())?,
                dependents: self
                    .sorted_paths(self.graph.reverse_neighbors(node)?.iter().copied())?,
            });
        }

        let mut edges = self
            .edge_provenance
            .iter()
            .filter(|((dependent, dependency), _)| {
                focus_node.is_none_or(|focus| focus == *dependent || focus == *dependency)
            })
            .map(|(&(dependent, dependency), imports)| {
                Ok(DependencyEdge {
                    dependent: self.path_for_node(dependent)?,
                    dependency: self.path_for_node(dependency)?,
                    imports: imports.clone(),
                })
            })
            .collect::<Result<Vec<_>, AnalysisError>>()?;
        edges.sort_by_key(|edge| {
            (
                display_repository_path(&edge.dependent),
                display_repository_path(&edge.dependency),
            )
        });

        let resolution_traces = self
            .resolution_traces
            .iter()
            .filter(|trace| {
                focus_path
                    .as_ref()
                    .is_none_or(|focus| trace.importer == *focus)
            })
            .cloned()
            .collect();

        Ok(GraphInspection {
            focus: focus_path,
            source_roots: self.source_roots.clone(),
            modules,
            edges,
            resolution_traces,
        })
    }

    /// Calculates direct dependents, indirect dependents, and affected tests.
    pub fn impact(&self, changed: &Path) -> Result<ImpactResult, AnalysisError> {
        let (changed_path, changed_node) = self.resolve_input(changed)?;
        self.impact_resolved(vec![(changed_path, changed_node)])
    }

    /// Calculates the union of impact from one or more changed Python files.
    ///
    /// Inputs may be repository-relative or absolute. Equivalent and duplicate
    /// paths are normalized to one canonical repository-relative identity.
    pub fn impact_many(&self, changed: &[PathBuf]) -> Result<ImpactResult, AnalysisError> {
        if changed.is_empty() {
            return Err(AnalysisError::MissingChangedInput);
        }
        let mut resolved = Vec::with_capacity(changed.len());
        for path in changed {
            resolved.push(self.resolve_input(path)?);
        }
        self.impact_resolved(resolved)
    }

    /// Calculates impact from already-normalized repository paths.
    pub(crate) fn impact_repository_paths(
        &self,
        changed: &[PathBuf],
    ) -> Result<ImpactResult, AnalysisError> {
        let mut resolved = Vec::with_capacity(changed.len());
        for path in changed {
            let node = self
                .nodes_by_path
                .get(path)
                .copied()
                .ok_or_else(|| AnalysisError::FileNotIndexed(path.clone()))?;
            resolved.push((path.clone(), node));
        }
        self.impact_resolved(resolved)
    }

    /// Returns affected pytest-style files for one changed file.
    pub fn affected_tests(&self, changed: &Path) -> Result<Vec<PathBuf>, AnalysisError> {
        Ok(self.impact(changed)?.affected_tests)
    }

    /// Returns affected pytest-style files for one or more changed files.
    pub fn affected_tests_many(&self, changed: &[PathBuf]) -> Result<Vec<PathBuf>, AnalysisError> {
        Ok(self.impact_many(changed)?.affected_tests)
    }

    /// Explains one shortest dependency path from `target` to `changed`.
    ///
    /// The public argument order matches `urmare why <changed> <target>`, while
    /// the returned path reads from the affected target toward its dependency.
    pub fn why(&self, changed: &Path, target: &Path) -> Result<DependencyPath, AnalysisError> {
        let (changed_path, changed_node) = self.resolve_input(changed)?;
        let (target_path, target_node) = self.resolve_input(target)?;
        let Some(path) = self.graph.dependency_path(target_node, changed_node)? else {
            return Err(AnalysisError::NoDependencyPath {
                changed: changed_path,
                target: target_path,
            });
        };
        let mut steps = Vec::with_capacity(path.len().saturating_sub(1));
        for pair in path.windows(2) {
            let dependent = pair[0];
            let dependency = pair[1];
            let dependent_path = self.path_for_node(dependent)?;
            let dependency_path = self.path_for_node(dependency)?;
            let imports = self
                .edge_provenance
                .get(&(dependent, dependency))
                .cloned()
                .ok_or_else(|| AnalysisError::MissingEdgeProvenance {
                    dependent: dependent_path.clone(),
                    dependency: dependency_path.clone(),
                })?;
            steps.push(DependencyStep {
                dependent: dependent_path,
                dependency: dependency_path,
                imports,
            });
        }
        Ok(DependencyPath {
            changed: changed_path,
            affected: target_path,
            path: self.sorted_paths_in_order(path)?,
            steps,
        })
    }

    fn resolve_input(&self, input: &Path) -> Result<(PathBuf, NodeId), AnalysisError> {
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
        let relative = relative.to_path_buf();
        let node = self
            .nodes_by_path
            .get(&relative)
            .copied()
            .ok_or_else(|| AnalysisError::FileNotIndexed(relative.clone()))?;
        Ok((relative, node))
    }

    fn impact_resolved(
        &self,
        mut changed: Vec<(PathBuf, NodeId)>,
    ) -> Result<ImpactResult, AnalysisError> {
        changed.sort_by_key(|(path, _)| display_repository_path(path));
        changed.dedup();

        let changed_nodes: HashSet<_> = changed.iter().map(|(_, node)| *node).collect();
        let mut direct_nodes = HashSet::new();
        let mut closure_nodes = HashSet::new();
        let mut causes: HashMap<NodeId, HashSet<PathBuf>> = HashMap::new();

        for (changed_path, changed_node) in &changed {
            let direct = self.graph.reverse_neighbors(*changed_node)?;
            direct_nodes.extend(direct.iter().copied());

            let closure = self.graph.reverse_transitive_closure(*changed_node)?;
            for affected in &closure {
                causes
                    .entry(*affected)
                    .or_default()
                    .insert(changed_path.clone());
            }
            closure_nodes.extend(closure);

            if self.present_nodes.contains(changed_node)
                && self
                    .files
                    .get(changed_node.index())
                    .is_some_and(|file| file.is_test)
            {
                causes
                    .entry(*changed_node)
                    .or_default()
                    .insert(changed_path.clone());
            }
        }

        direct_nodes.retain(|node| !changed_nodes.contains(node));
        closure_nodes.retain(|node| !changed_nodes.contains(node));
        let transitive_nodes: HashSet<_> =
            closure_nodes.difference(&direct_nodes).copied().collect();

        let mut affected_test_nodes: HashSet<NodeId> = closure_nodes
            .iter()
            .copied()
            .filter(|node| {
                self.present_nodes.contains(node)
                    && self
                        .files
                        .get(node.index())
                        .is_some_and(|file| file.is_test)
            })
            .collect();
        affected_test_nodes.extend(changed.iter().filter_map(|(_, node)| {
            (self.present_nodes.contains(node)
                && self
                    .files
                    .get(node.index())
                    .is_some_and(|file| file.is_test))
            .then_some(*node)
        }));

        let directly_affected = self.sorted_paths(direct_nodes.iter().copied())?;
        let transitively_affected = self.sorted_paths(transitive_nodes.iter().copied())?;
        let affected_tests = self.sorted_paths(affected_test_nodes.iter().copied())?;

        let result_nodes: HashSet<_> = direct_nodes
            .union(&transitive_nodes)
            .copied()
            .chain(affected_test_nodes)
            .collect();
        let mut attributions = result_nodes
            .into_iter()
            .map(|node| {
                let affected = self.path_for_node(node)?;
                let mut caused_by: Vec<_> = causes
                    .remove(&node)
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                caused_by.sort_by_key(|path| display_repository_path(path));
                Ok(ImpactAttribution {
                    affected,
                    caused_by,
                })
            })
            .collect::<Result<Vec<_>, AnalysisError>>()?;
        attributions.sort_by_key(|item| display_repository_path(&item.affected));

        Ok(ImpactResult {
            changed: changed.into_iter().map(|(path, _)| path).collect(),
            directly_affected,
            transitively_affected,
            affected_tests,
            attributions,
        })
    }

    fn path_for_node(&self, node: NodeId) -> Result<PathBuf, AnalysisError> {
        self.files
            .get(node.index())
            .map(|file| file.path.clone())
            .ok_or(AnalysisError::MissingNodeMetadata(node.index()))
    }

    fn sorted_paths(
        &self,
        nodes: impl IntoIterator<Item = NodeId>,
    ) -> Result<Vec<PathBuf>, AnalysisError> {
        let mut paths = self.sorted_paths_in_order(nodes)?;
        paths.sort_by_key(|path| display_repository_path(path));
        Ok(paths)
    }

    fn sorted_paths_in_order(
        &self,
        nodes: impl IntoIterator<Item = NodeId>,
    ) -> Result<Vec<PathBuf>, AnalysisError> {
        nodes
            .into_iter()
            .map(|node| self.path_for_node(node))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;
    use urmare_python::{SourceLocation, StaticImport};

    use super::RepositoryAnalysis;
    use crate::{
        AnalysisError, DependencyStep, ImportProvenance, ImportResolutionStatus,
        cache::CacheLocation,
    };

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/python-projects")
            .join(name)
    }

    #[test]
    fn builds_summary_and_counts_unresolved_external_imports() {
        let repository =
            RepositoryAnalysis::build(&fixture("src-layout")).expect("fixture analysis");
        let summary = repository.summary();

        assert_eq!(summary.python_files, 14);
        assert_eq!(summary.modules, 14);
        assert_eq!(summary.import_edges, 14);
        assert_eq!(summary.tests, 4);
        assert_eq!(summary.unresolved_imports, 3);
        assert_eq!(
            repository.unresolved_imports(),
            [
                crate::UnresolvedImport {
                    importer: PathBuf::from("src/payments/stripe.py"),
                    location: SourceLocation { line: 1, column: 8 },
                    import: StaticImport::Import {
                        module: "requests".into(),
                    },
                },
                crate::UnresolvedImport {
                    importer: PathBuf::from("tests/analytics/test_reporting.py"),
                    location: SourceLocation { line: 1, column: 8 },
                    import: StaticImport::Import {
                        module: "analytics.reporting".into(),
                    },
                },
                crate::UnresolvedImport {
                    importer: PathBuf::from("tests/helpers_test.py"),
                    location: SourceLocation { line: 1, column: 8 },
                    import: StaticImport::Import {
                        module: "pytest".into(),
                    },
                },
            ]
        );
    }

    #[test]
    fn calculates_direct_transitive_and_test_impact() {
        let repository =
            RepositoryAnalysis::build(&fixture("src-layout")).expect("fixture analysis");
        let impact = repository
            .impact(Path::new("src/payments/stripe.py"))
            .expect("impact analysis");

        assert_eq!(
            impact.directly_affected,
            vec![
                PathBuf::from("src/payments/formatters/card.py"),
                PathBuf::from("src/payments/service.py"),
                PathBuf::from("tests/payments/test_stripe.py"),
            ]
        );
        assert_eq!(
            impact.transitively_affected,
            vec![
                PathBuf::from("src/api/checkout.py"),
                PathBuf::from("tests/api/test_checkout.py"),
            ]
        );
        assert_eq!(
            impact.affected_tests,
            vec![
                PathBuf::from("tests/api/test_checkout.py"),
                PathBuf::from("tests/payments/test_stripe.py"),
            ]
        );
    }

    #[test]
    fn calculates_impact_across_configured_source_roots() {
        let repository =
            RepositoryAnalysis::build(&fixture("multiple-roots")).expect("fixture analysis");
        let impact = repository
            .impact(Path::new("packages/payments/src/payments/pricing.py"))
            .expect("multi-root impact");

        assert_eq!(
            impact.directly_affected,
            vec![
                PathBuf::from("packages/api/src/api/checkout.py"),
                PathBuf::from("tests/payments/test_pricing.py"),
            ]
        );
        assert_eq!(
            impact.transitively_affected,
            vec![PathBuf::from("tests/api/test_checkout.py")]
        );
        assert_eq!(
            impact.affected_tests,
            vec![
                PathBuf::from("tests/api/test_checkout.py"),
                PathBuf::from("tests/payments/test_pricing.py"),
            ]
        );

        let path = repository
            .why(
                Path::new("packages/payments/src/payments/pricing.py"),
                Path::new("tests/api/test_checkout.py"),
            )
            .expect("multi-root dependency path");
        assert_eq!(
            path.path,
            vec![
                PathBuf::from("tests/api/test_checkout.py"),
                PathBuf::from("packages/api/src/api/checkout.py"),
                PathBuf::from("packages/payments/src/payments/pricing.py"),
            ]
        );
    }

    #[test]
    fn configured_boundaries_exclude_files_and_select_additional_tests() {
        let repository = RepositoryAnalysis::build(&fixture("configured-boundaries"))
            .expect("configured boundary analysis");

        assert_eq!(
            repository.summary(),
            crate::GraphSummary {
                python_files: 5,
                modules: 5,
                import_edges: 6,
                tests: 2,
                unresolved_imports: 0,
            }
        );
        let impact = repository
            .impact(Path::new("src/app/core.py"))
            .expect("configured test impact");
        assert_eq!(
            impact.directly_affected,
            vec![
                PathBuf::from("checks/test_conventional.py"),
                PathBuf::from("src/app/service.py"),
            ]
        );
        assert_eq!(
            impact.transitively_affected,
            vec![PathBuf::from("verification/checkout_spec.py")]
        );
        assert_eq!(
            impact.affected_tests,
            vec![
                PathBuf::from("checks/test_conventional.py"),
                PathBuf::from("verification/checkout_spec.py"),
            ]
        );

        let excluded = repository
            .impact(Path::new("src/generated/client.py"))
            .expect_err("excluded files are not indexed");
        assert!(matches!(excluded, AnalysisError::FileNotIndexed(_)));
    }

    #[test]
    fn rejects_modules_exposed_by_more_than_one_source_root() {
        let repository = tempdir().expect("temporary repository");
        fs::create_dir_all(repository.path().join("one/pkg")).expect("first source root");
        fs::create_dir_all(repository.path().join("two/pkg")).expect("second source root");
        fs::write(
            repository.path().join("pyproject.toml"),
            "[tool.urmare]\nsource-roots = [\"one\", \"two\"]\n",
        )
        .expect("configuration fixture");
        fs::write(repository.path().join("one/pkg/module.py"), "VALUE = 1\n")
            .expect("first module");
        fs::write(repository.path().join("two/pkg/module.py"), "VALUE = 2\n")
            .expect("second module");

        let error = RepositoryAnalysis::build(repository.path())
            .err()
            .expect("module collision");
        assert!(matches!(
            error,
            AnalysisError::DuplicateModule { ref module, .. } if module == "pkg.module"
        ));
        assert!(error.to_string().contains("tool.urmare.source-roots"));
    }

    #[test]
    fn explains_path_from_affected_test_toward_changed_dependency() {
        let repository =
            RepositoryAnalysis::build(&fixture("src-layout")).expect("fixture analysis");
        let path = repository
            .why(
                Path::new("src/payments/stripe.py"),
                Path::new("tests/api/test_checkout.py"),
            )
            .expect("dependency path");

        assert_eq!(path.changed, PathBuf::from("src/payments/stripe.py"));
        assert_eq!(path.affected, PathBuf::from("tests/api/test_checkout.py"));
        assert_eq!(
            path.path,
            vec![
                PathBuf::from("tests/api/test_checkout.py"),
                PathBuf::from("src/api/checkout.py"),
                PathBuf::from("src/payments/service.py"),
                PathBuf::from("src/payments/stripe.py"),
            ]
        );
        assert_eq!(
            path.steps,
            vec![
                DependencyStep {
                    dependent: PathBuf::from("tests/api/test_checkout.py"),
                    dependency: PathBuf::from("src/api/checkout.py"),
                    imports: vec![ImportProvenance {
                        location: SourceLocation {
                            line: 1,
                            column: 17,
                        },
                        import: StaticImport::From {
                            module: Some("api".into()),
                            name: "checkout".into(),
                            level: 0,
                        },
                    }],
                },
                DependencyStep {
                    dependent: PathBuf::from("src/api/checkout.py"),
                    dependency: PathBuf::from("src/payments/service.py"),
                    imports: vec![ImportProvenance {
                        location: SourceLocation {
                            line: 1,
                            column: 30,
                        },
                        import: StaticImport::From {
                            module: Some("payments.service".into()),
                            name: "create_payment".into(),
                            level: 0,
                        },
                    }],
                },
                DependencyStep {
                    dependent: PathBuf::from("src/payments/service.py"),
                    dependency: PathBuf::from("src/payments/stripe.py"),
                    imports: vec![ImportProvenance {
                        location: SourceLocation {
                            line: 1,
                            column: 15,
                        },
                        import: StaticImport::From {
                            module: None,
                            name: "stripe".into(),
                            level: 1,
                        },
                    }],
                },
            ]
        );
    }

    #[test]
    fn focused_graph_inspection_exposes_mappings_edges_and_resolution_attempts() {
        let repository =
            RepositoryAnalysis::build(&fixture("src-layout")).expect("fixture analysis");
        let inspection = repository
            .graph_inspection(Some(Path::new("src/payments/service.py")))
            .expect("focused graph inspection");

        assert_eq!(
            inspection.focus,
            Some(PathBuf::from("src/payments/service.py"))
        );
        assert_eq!(inspection.source_roots, [PathBuf::from("src")]);
        assert_eq!(inspection.modules.len(), 1);
        assert_eq!(inspection.modules[0].module, "payments.service");
        assert_eq!(
            inspection.modules[0].dependencies,
            [
                PathBuf::from("src/payments/__init__.py"),
                PathBuf::from("src/payments/stripe.py"),
            ]
        );
        assert_eq!(
            inspection.modules[0].dependents,
            [PathBuf::from("src/api/checkout.py")]
        );
        assert_eq!(inspection.edges.len(), 3);
        assert_eq!(inspection.resolution_traces.len(), 1);
        let trace = &inspection.resolution_traces[0];
        assert_eq!(trace.status, ImportResolutionStatus::Resolved);
        assert_eq!(trace.candidate_modules, ["payments", "payments.stripe"]);
        assert_eq!(
            trace
                .resolved_modules
                .iter()
                .map(|resolved| resolved.path.clone())
                .collect::<Vec<_>>(),
            [
                PathBuf::from("src/payments/__init__.py"),
                PathBuf::from("src/payments/stripe.py"),
            ]
        );
    }

    #[test]
    fn one_edge_retains_every_import_occurrence_and_survives_cache_reuse() {
        let repository = tempdir().expect("temporary repository");
        let cache = tempdir().expect("temporary cache");
        fs::write(repository.path().join("dependency.py"), "VALUE = 1\n").expect("dependency");
        fs::write(
            repository.path().join("consumer.py"),
            "import dependency\nfrom dependency import VALUE\n",
        )
        .expect("consumer");

        let (cold, cold_timings) = RepositoryAnalysis::build_profiled_with_cache_directory(
            repository.path(),
            cache.path(),
        )
        .expect("cold analysis");
        assert_eq!(cold_timings.graph_cache.edge_hits, 0);
        let cold_path = cold
            .why(Path::new("dependency.py"), Path::new("consumer.py"))
            .expect("cold dependency path");
        assert_eq!(cold.summary().import_edges, 1);
        assert_eq!(cold_path.steps[0].imports.len(), 2);

        let (warm, warm_timings) = RepositoryAnalysis::build_profiled_with_cache_directory(
            repository.path(),
            cache.path(),
        )
        .expect("warm analysis");
        assert_eq!(warm_timings.graph_cache.edge_hits, 2);
        let warm_path = warm
            .why(Path::new("dependency.py"), Path::new("consumer.py"))
            .expect("warm dependency path");
        assert_eq!(warm_path, cold_path);
        assert_eq!(
            warm.graph_inspection(None)
                .expect("warm graph inspection")
                .resolution_traces,
            cold.graph_inspection(None)
                .expect("cold graph inspection")
                .resolution_traces,
        );
    }

    #[test]
    fn traces_invalid_relative_imports_without_panicking() {
        let repository = tempdir().expect("temporary repository");
        fs::write(
            repository.path().join("module.py"),
            "from ..outside import value\n",
        )
        .expect("Python fixture");

        let analysis = RepositoryAnalysis::build(repository.path()).expect("analysis");
        assert_eq!(analysis.summary().unresolved_imports, 1);
        let inspection = analysis.graph_inspection(None).expect("graph inspection");
        assert_eq!(inspection.resolution_traces.len(), 1);
        assert_eq!(
            inspection.resolution_traces[0].status,
            ImportResolutionStatus::InvalidRelativeImport
        );
        assert!(inspection.resolution_traces[0].candidate_modules.is_empty());
    }

    #[test]
    fn excludes_unrelated_tests() {
        let repository =
            RepositoryAnalysis::build(&fixture("src-layout")).expect("fixture analysis");
        let tests = repository
            .affected_tests(Path::new("src/payments/stripe.py"))
            .expect("test impact");

        assert!(!tests.contains(&PathBuf::from("tests/analytics/test_reporting.py")));
        assert!(!tests.contains(&PathBuf::from("tests/helpers_test.py")));
    }

    #[test]
    fn traverses_circular_dependencies_without_including_the_changed_file() {
        let repository =
            RepositoryAnalysis::build(&fixture("src-layout")).expect("fixture analysis");
        let impact = repository
            .impact(Path::new("src/cycles/a.py"))
            .expect("cycle impact");

        assert_eq!(
            impact.directly_affected,
            vec![PathBuf::from("src/cycles/b.py")]
        );
        assert!(impact.transitively_affected.is_empty());
    }

    #[test]
    fn includes_a_changed_test_in_affected_test_selection() {
        let repository =
            RepositoryAnalysis::build(&fixture("src-layout")).expect("fixture analysis");
        let tests = repository
            .affected_tests(Path::new("tests/helpers_test.py"))
            .expect("test impact");

        assert_eq!(tests, vec![PathBuf::from("tests/helpers_test.py")]);
    }

    #[test]
    fn reports_no_python_files() {
        let error = RepositoryAnalysis::build(&fixture("no-python"))
            .err()
            .expect("empty repository is rejected");
        assert!(matches!(error, AnalysisError::NoPythonFiles { .. }));
    }

    #[test]
    fn reports_invalid_python_syntax() {
        let error = RepositoryAnalysis::build(&fixture("syntax-invalid"))
            .err()
            .expect("invalid source is rejected");
        assert!(matches!(error, AnalysisError::Parse(_)));
        assert!(error.to_string().contains("broken.py"));
    }

    #[test]
    fn reports_missing_input_and_missing_dependency_paths() {
        let repository =
            RepositoryAnalysis::build(&fixture("src-layout")).expect("fixture analysis");

        let missing = repository
            .impact(Path::new("src/does-not-exist.py"))
            .expect_err("missing input");
        assert!(matches!(missing, AnalysisError::InputNotFound(_)));

        let no_path = repository
            .why(
                Path::new("src/payments/stripe.py"),
                Path::new("tests/analytics/test_reporting.py"),
            )
            .expect_err("unrelated test has no path");
        assert!(matches!(no_path, AnalysisError::NoDependencyPath { .. }));
    }

    #[test]
    fn rejects_an_input_file_outside_the_repository() {
        let repository =
            RepositoryAnalysis::build(&fixture("src-layout")).expect("fixture analysis");
        let outside = fixture("flat-layout").join("package/foo.py");

        let error = repository
            .impact(&outside)
            .expect_err("outside file must be rejected");
        assert!(matches!(
            error,
            AnalysisError::InputOutsideRepository { .. }
        ));
    }

    #[test]
    fn unions_multiple_changed_files_and_records_attribution() {
        let root = fixture("src-layout");
        let repository = RepositoryAnalysis::build(&root).expect("fixture analysis");
        let impact = repository
            .impact_many(&[
                PathBuf::from("src/payments/stripe.py"),
                PathBuf::from("src/cycles/a.py"),
                root.join("src/payments/stripe.py"),
            ])
            .expect("multi-file impact");

        assert_eq!(
            impact.changed,
            vec![
                PathBuf::from("src/cycles/a.py"),
                PathBuf::from("src/payments/stripe.py"),
            ]
        );
        assert!(
            impact
                .directly_affected
                .contains(&PathBuf::from("src/cycles/b.py"))
        );
        assert!(
            impact
                .directly_affected
                .contains(&PathBuf::from("src/payments/service.py"))
        );
        assert_eq!(
            impact.causes_for(Path::new("src/cycles/b.py")),
            [PathBuf::from("src/cycles/a.py")]
        );
        assert_eq!(
            impact.causes_for(Path::new("tests/api/test_checkout.py")),
            [PathBuf::from("src/payments/stripe.py")]
        );

        let overlapping = repository
            .impact_many(&[
                PathBuf::from("src/payments/stripe.py"),
                PathBuf::from("src/payments/service.py"),
            ])
            .expect("overlapping multi-file impact");
        assert_eq!(
            overlapping.causes_for(Path::new("src/api/checkout.py")),
            [
                PathBuf::from("src/payments/service.py"),
                PathBuf::from("src/payments/stripe.py"),
            ]
        );
        assert_eq!(
            repository
                .affected_tests_many(&[
                    PathBuf::from("src/payments/stripe.py"),
                    PathBuf::from("src/cycles/a.py"),
                ])
                .expect("multi-file affected tests"),
            [
                PathBuf::from("tests/api/test_checkout.py"),
                PathBuf::from("tests/payments/test_stripe.py"),
            ]
        );

        let error = repository
            .impact_many(&[])
            .expect_err("empty explicit change set");
        assert!(matches!(error, AnalysisError::MissingChangedInput));
    }

    #[test]
    fn cached_edges_keep_a_deleted_virtual_module_connected() {
        let repository = tempdir().expect("temporary repository");
        let cache = tempdir().expect("temporary cache");
        fs::create_dir_all(repository.path().join("src")).expect("source directory");
        fs::create_dir_all(repository.path().join("tests")).expect("tests directory");
        fs::write(repository.path().join("src/dependency.py"), "VALUE = 1\n").expect("dependency");
        fs::write(
            repository.path().join("src/consumer.py"),
            "import dependency\n",
        )
        .expect("consumer");
        fs::write(
            repository.path().join("tests/test_consumer.py"),
            "import consumer\n",
        )
        .expect("test");
        let location = CacheLocation::Directory(cache.path().to_path_buf());

        RepositoryAnalysis::build_profiled_with_virtual_files(
            repository.path(),
            std::iter::empty(),
            location.clone(),
        )
        .expect("populate persistent index");
        fs::remove_file(repository.path().join("src/dependency.py")).expect("delete dependency");

        let (analysis, timings) = RepositoryAnalysis::build_profiled_with_virtual_files(
            repository.path(),
            [Path::new("src/dependency.py")],
            location,
        )
        .expect("analysis with deleted virtual identity");
        assert_eq!(timings.graph_cache.module_hits, 3);
        assert_eq!(timings.graph_cache.edge_hits, 2);
        assert_eq!(timings.graph_cache.edge_misses, 0);

        let impact = analysis
            .impact_repository_paths(&[PathBuf::from("src/dependency.py")])
            .expect("deleted-module impact");
        assert_eq!(impact.directly_affected, [PathBuf::from("src/consumer.py")]);
        assert_eq!(
            impact.affected_tests,
            [PathBuf::from("tests/test_consumer.py")]
        );
    }

    #[test]
    fn rename_virtual_identity_invalidates_cached_resolutions() {
        let repository = tempdir().expect("temporary repository");
        let cache = tempdir().expect("temporary cache");
        fs::create_dir_all(repository.path().join("src")).expect("source directory");
        fs::create_dir_all(repository.path().join("tests")).expect("tests directory");
        fs::write(repository.path().join("src/old_name.py"), "VALUE = 1\n").expect("dependency");
        fs::write(
            repository.path().join("src/consumer.py"),
            "import old_name\n",
        )
        .expect("consumer");
        fs::write(
            repository.path().join("tests/test_consumer.py"),
            "import consumer\n",
        )
        .expect("test");
        let location = CacheLocation::Directory(cache.path().to_path_buf());

        RepositoryAnalysis::build_profiled_with_virtual_files(
            repository.path(),
            std::iter::empty(),
            location.clone(),
        )
        .expect("populate persistent index");
        fs::rename(
            repository.path().join("src/old_name.py"),
            repository.path().join("src/new_name.py"),
        )
        .expect("rename dependency");

        let (analysis, timings) = RepositoryAnalysis::build_profiled_with_virtual_files(
            repository.path(),
            [Path::new("src/old_name.py")],
            location,
        )
        .expect("analysis with rename identities");
        assert_eq!(timings.graph_cache.module_hits, 3);
        assert_eq!(timings.graph_cache.edge_hits, 0);
        assert_eq!(timings.graph_cache.edge_misses, 3);

        let impact = analysis
            .impact_repository_paths(&[
                PathBuf::from("src/old_name.py"),
                PathBuf::from("src/new_name.py"),
            ])
            .expect("rename impact");
        assert_eq!(impact.directly_affected, [PathBuf::from("src/consumer.py")]);
        assert_eq!(
            impact.affected_tests,
            [PathBuf::from("tests/test_consumer.py")]
        );
    }
}
