use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, UNIX_EPOCH};

use redb::{
    Database, DatabaseError, ReadOnlyDatabase, ReadTransaction, ReadableDatabase, ReadableTable,
    TableDefinition,
};
use serde::{Deserialize, Serialize};
use urmare_python::{
    IMPORT_ANALYSIS_CACHE_TAG, ImportResolutionFailure, LocalImportResolution, LocatedImport,
    ModuleResolver, PathExcluder, PythonFile, discover_python_files_profiled,
    is_discoverable_python_path, parse_imports_with_locations, resolve_local_import_with,
};

use crate::{
    AnalysisError, GraphSummary, ImportProvenance, ImportResolutionStatus, ImportResolutionTrace,
    RepositoryConfig, ResolvedLocalModule, UnresolvedImport,
    cache::{CacheLocation, content_hash, repository_fingerprint},
    display_repository_path,
};

const INDEX_SCHEMA_VERSION: u32 = 1;
const INDEX_FILE_NAME: &str = "repository-index-v1.redb";
const RESOLVER_COMPATIBILITY_TAG: &str = "python-local-import-resolution-v4";
const META_KEY: &str = "current";

const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("metadata");
const FILES_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("files");
const MODULES_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("modules");
const REVERSE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("reverse");
const CANDIDATES_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("candidates");

/// How the current repository view was obtained.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IndexBuildKind {
    #[default]
    Full,
    Incremental,
    Reused,
}

/// Why a persistent index could not be updated incrementally.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexFallbackReason {
    CacheDisabled,
    MissingIndex,
    IncompatibleIndex,
    ConfigurationChanged,
    SourceRootRemapped,
    NonGitRepository,
    GitStateUnavailable,
    IndexLocked,
    IndexCorrupt,
    StorageFailure,
}

/// Bounded-work counters for index construction, validation, and mutation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndexWorkStats {
    pub directories_inspected: usize,
    pub inventory_entries_inspected: usize,
    pub files_statted: usize,
    pub files_read: usize,
    pub files_hashed: usize,
    pub files_parsed: usize,
    pub modules_added: usize,
    pub modules_removed: usize,
    pub modules_remapped: usize,
    pub modules_reused: usize,
    pub importers_reresolved: usize,
    pub records_added: usize,
    pub records_removed: usize,
    pub forward_edges_added: usize,
    pub forward_edges_removed: usize,
    pub reverse_edges_added: usize,
    pub reverse_edges_removed: usize,
    pub index_records_read: usize,
    pub index_records_written: usize,
    pub bytes_written: u64,
    pub build_kind: IndexBuildKind,
    pub fallback_reason: Option<IndexFallbackReason>,
}

/// Timings for the persistent-index pipeline.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndexTimings {
    pub load: Duration,
    pub delta_detection: Duration,
    pub update: Duration,
    pub persistence: Duration,
}

#[derive(Clone, Debug)]
pub(crate) enum IndexStore {
    Memory(MemoryIndex),
    Persistent(PathBuf),
}

#[derive(Clone, Debug)]
pub(crate) struct BuiltIndex {
    pub store: IndexStore,
    pub summary: GraphSummary,
    pub source_roots: Vec<PathBuf>,
    pub work: IndexWorkStats,
    pub timings: IndexTimings,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct StoredFileState {
    pub(crate) size: u64,
    pub(crate) modified_ns: Option<u128>,
}

impl StoredFileState {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            size: metadata.len(),
            modified_ns: metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct StoredResolution {
    pub import: LocatedImport,
    pub resolution: LocalImportResolution,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct StoredFile {
    pub path: String,
    pub module: String,
    pub is_package: bool,
    pub is_test: bool,
    pub(crate) state: StoredFileState,
    pub(crate) content_hash: String,
    pub imports: Vec<LocatedImport>,
    pub resolutions: Vec<StoredResolution>,
    /// Dependency path to every import occurrence producing that edge.
    pub dependencies: BTreeMap<String, Vec<LocatedImport>>,
}

impl StoredFile {
    pub fn repository_path(&self) -> PathBuf {
        portable_path(&self.path)
    }

    pub fn python_file(&self) -> PythonFile {
        PythonFile {
            path: self.repository_path(),
            module: self.module.clone(),
            is_package: self.is_package,
            is_test: self.is_test,
        }
    }

    pub fn unresolved(&self) -> impl Iterator<Item = UnresolvedImport> + '_ {
        self.resolutions
            .iter()
            .filter(|entry| entry.resolution.resolved_modules.is_empty())
            .map(|entry| UnresolvedImport {
                importer: self.repository_path(),
                location: entry.import.location,
                import: entry.import.import.clone(),
            })
    }

    pub fn traces(
        &self,
        mut module_path: impl FnMut(&str) -> Option<String>,
    ) -> Vec<ImportResolutionTrace> {
        self.resolutions
            .iter()
            .map(|entry| {
                let status = resolution_status(&entry.resolution);
                ImportResolutionTrace {
                    importer: self.repository_path(),
                    location: entry.import.location,
                    import: entry.import.import.clone(),
                    candidate_modules: entry.resolution.candidate_modules.clone(),
                    resolved_modules: entry
                        .resolution
                        .resolved_modules
                        .iter()
                        .filter_map(|module| {
                            module_path(module).map(|path| ResolvedLocalModule {
                                module: module.clone(),
                                path: portable_path(&path),
                            })
                        })
                        .collect(),
                    status,
                }
            })
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StoredSummary {
    python_files: usize,
    modules: usize,
    import_edges: usize,
    tests: usize,
    unresolved_imports: usize,
}

impl From<&GraphSummary> for StoredSummary {
    fn from(summary: &GraphSummary) -> Self {
        Self {
            python_files: summary.python_files,
            modules: summary.modules,
            import_edges: summary.import_edges,
            tests: summary.tests,
            unresolved_imports: summary.unresolved_imports,
        }
    }
}

impl From<&StoredSummary> for GraphSummary {
    fn from(summary: &StoredSummary) -> Self {
        Self {
            python_files: summary.python_files,
            modules: summary.modules,
            import_edges: summary.import_edges,
            tests: summary.tests,
            unresolved_imports: summary.unresolved_imports,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct GitBaseline {
    head: Option<String>,
    working_paths: Vec<String>,
    ignored_python_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct IndexMetadata {
    schema_version: u32,
    parser: String,
    resolver: String,
    repository: String,
    repository_path: String,
    configuration: String,
    generation: u64,
    source_roots: Vec<String>,
    summary: StoredSummary,
    git_baseline: Option<GitBaseline>,
}

impl IndexMetadata {
    fn semantic_tags_compatible(&self, repository: &str) -> bool {
        self.schema_version == INDEX_SCHEMA_VERSION
            && self.parser == IMPORT_ANALYSIS_CACHE_TAG
            && self.resolver == RESOLVER_COMPATIBILITY_TAG
            && self.repository == repository
    }

    fn compatible(&self, repository: &str, configuration: &str) -> bool {
        self.semantic_tags_compatible(repository) && self.configuration == configuration
    }

    fn source_root_paths(&self) -> Vec<PathBuf> {
        self.source_roots
            .iter()
            .map(|path| portable_path(path))
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct MemoryIndex {
    pub files: BTreeMap<String, StoredFile>,
    pub modules: BTreeMap<String, String>,
    pub reverse: BTreeMap<String, BTreeSet<String>>,
    pub candidates: BTreeMap<String, BTreeSet<String>>,
}

impl MemoryIndex {
    fn rebuild_relationships(&mut self) {
        self.modules.clear();
        self.reverse.clear();
        self.candidates.clear();
        for (path, file) in &self.files {
            self.modules.insert(file.module.clone(), path.clone());
            for dependency in file.dependencies.keys() {
                self.reverse
                    .entry(dependency.clone())
                    .or_default()
                    .insert(path.clone());
            }
            for candidate in file
                .resolutions
                .iter()
                .flat_map(|entry| &entry.resolution.candidate_modules)
            {
                self.candidates
                    .entry(candidate.clone())
                    .or_default()
                    .insert(path.clone());
            }
        }
    }

    fn summary(&self) -> GraphSummary {
        GraphSummary {
            python_files: self.files.len(),
            modules: self.modules.len(),
            import_edges: self
                .files
                .values()
                .map(|file| file.dependencies.len())
                .sum(),
            tests: self.files.values().filter(|file| file.is_test).count(),
            unresolved_imports: self.files.values().map(unresolved_count).sum(),
        }
    }
}

pub(crate) trait IndexRead {
    fn file(&self, path: &str) -> Result<Option<StoredFile>, String>;
    fn files(&self) -> Result<Vec<StoredFile>, String>;
    fn module_path(&self, module: &str) -> Result<Option<String>, String>;
    fn reverse_dependents(&self, path: &str) -> Result<Vec<String>, String>;
    fn candidate_importers(&self, module: &str) -> Result<Vec<String>, String>;
}

impl IndexRead for MemoryIndex {
    fn file(&self, path: &str) -> Result<Option<StoredFile>, String> {
        Ok(self.files.get(path).cloned())
    }

    fn files(&self) -> Result<Vec<StoredFile>, String> {
        Ok(self.files.values().cloned().collect())
    }

    fn module_path(&self, module: &str) -> Result<Option<String>, String> {
        Ok(self.modules.get(module).cloned())
    }

    fn reverse_dependents(&self, path: &str) -> Result<Vec<String>, String> {
        Ok(self
            .reverse
            .get(path)
            .map(|paths| paths.iter().cloned().collect())
            .unwrap_or_default())
    }

    fn candidate_importers(&self, module: &str) -> Result<Vec<String>, String> {
        Ok(self
            .candidates
            .get(module)
            .map(|paths| paths.iter().cloned().collect())
            .unwrap_or_default())
    }
}

struct PersistentReader<'transaction> {
    transaction: &'transaction ReadTransaction,
}

impl IndexRead for PersistentReader<'_> {
    fn file(&self, path: &str) -> Result<Option<StoredFile>, String> {
        read_json(self.transaction, FILES_TABLE, path)
    }

    fn files(&self) -> Result<Vec<StoredFile>, String> {
        read_all_json(self.transaction, FILES_TABLE)
    }

    fn module_path(&self, module: &str) -> Result<Option<String>, String> {
        read_json(self.transaction, MODULES_TABLE, module)
    }

    fn reverse_dependents(&self, path: &str) -> Result<Vec<String>, String> {
        Ok(read_json(self.transaction, REVERSE_TABLE, path)?.unwrap_or_default())
    }

    fn candidate_importers(&self, module: &str) -> Result<Vec<String>, String> {
        Ok(read_json(self.transaction, CANDIDATES_TABLE, module)?.unwrap_or_default())
    }
}

impl IndexStore {
    pub fn read<T>(
        &self,
        operation: impl FnOnce(&dyn IndexRead) -> Result<T, AnalysisError>,
    ) -> Result<T, AnalysisError> {
        match self {
            Self::Memory(index) => operation(index),
            Self::Persistent(path) => {
                let database = ReadOnlyDatabase::open(path).map_err(index_query_error)?;
                let transaction = database.begin_read().map_err(index_query_error)?;
                operation(&PersistentReader {
                    transaction: &transaction,
                })
            }
        }
    }
}

pub(crate) fn build_index(
    root: &Path,
    cache_location: CacheLocation,
) -> Result<BuiltIndex, AnalysisError> {
    build_index_with_boundary_paths(root, cache_location, &[])
}

pub(crate) fn build_index_with_boundary_paths(
    root: &Path,
    cache_location: CacheLocation,
    boundary_paths: &[PathBuf],
) -> Result<BuiltIndex, AnalysisError> {
    let root = root
        .canonicalize()
        .map_err(|source| AnalysisError::RootCanonicalization {
            path: root.to_path_buf(),
            source,
        })?;
    let configuration = RepositoryConfig::load(&root)?;
    let configuration_fingerprint = configuration_fingerprint(&configuration);
    let repository_identity = repository_fingerprint(&root);

    let Some(directory) = cache_location.directory(&root) else {
        let mut work = IndexWorkStats {
            fallback_reason: Some(IndexFallbackReason::CacheDisabled),
            ..IndexWorkStats::default()
        };
        let (index, source_roots) = build_full(&root, &configuration, boundary_paths, &mut work)?;
        let summary = index.summary();
        return Ok(BuiltIndex {
            store: IndexStore::Memory(index),
            summary,
            source_roots,
            work,
            timings: IndexTimings::default(),
        });
    };

    let index_path = directory.join(INDEX_FILE_NAME);
    if fs::create_dir_all(&directory).is_err() {
        return full_memory_fallback(
            &root,
            &configuration,
            boundary_paths,
            IndexFallbackReason::StorageFailure,
        );
    }

    let load_started = Instant::now();
    let database = match Database::create(&index_path) {
        Ok(database) => database,
        Err(DatabaseError::DatabaseAlreadyOpen) => {
            return full_memory_fallback(
                &root,
                &configuration,
                boundary_paths,
                IndexFallbackReason::IndexLocked,
            );
        }
        Err(_) => {
            let _ = fs::remove_file(&index_path);
            match Database::create(&index_path) {
                Ok(database) => database,
                Err(_) => {
                    return full_memory_fallback(
                        &root,
                        &configuration,
                        boundary_paths,
                        IndexFallbackReason::IndexCorrupt,
                    );
                }
            }
        }
    };
    let mut timings = IndexTimings {
        load: load_started.elapsed(),
        ..IndexTimings::default()
    };

    let metadata = match read_metadata(&database) {
        Ok(metadata) => metadata,
        Err(_) => {
            return rebuild_persistent(
                database,
                index_path,
                &root,
                &configuration,
                boundary_paths,
                &configuration_fingerprint,
                &repository_identity,
                IndexFallbackReason::IndexCorrupt,
                timings,
            );
        }
    };
    let Some(metadata) = metadata else {
        return rebuild_persistent(
            database,
            index_path,
            &root,
            &configuration,
            boundary_paths,
            &configuration_fingerprint,
            &repository_identity,
            IndexFallbackReason::MissingIndex,
            timings,
        );
    };
    if !metadata.compatible(&repository_identity, &configuration_fingerprint) {
        let reason = if metadata.semantic_tags_compatible(&repository_identity)
            && metadata.configuration != configuration_fingerprint
        {
            IndexFallbackReason::ConfigurationChanged
        } else {
            IndexFallbackReason::IncompatibleIndex
        };
        return rebuild_persistent(
            database,
            index_path,
            &root,
            &configuration,
            boundary_paths,
            &configuration_fingerprint,
            &repository_identity,
            reason,
            timings,
        );
    }

    let delta_started = Instant::now();
    let delta = match detect_delta(&root, metadata.git_baseline.as_ref()) {
        Ok(delta) => delta,
        Err(DeltaError::NonGit) => {
            timings.delta_detection = delta_started.elapsed();
            return rebuild_persistent(
                database,
                index_path,
                &root,
                &configuration,
                boundary_paths,
                &configuration_fingerprint,
                &repository_identity,
                IndexFallbackReason::NonGitRepository,
                timings,
            );
        }
        Err(_) => {
            timings.delta_detection = delta_started.elapsed();
            return rebuild_persistent(
                database,
                index_path,
                &root,
                &configuration,
                boundary_paths,
                &configuration_fingerprint,
                &repository_identity,
                IndexFallbackReason::GitStateUnavailable,
                timings,
            );
        }
    };
    timings.delta_detection = delta_started.elapsed();

    if delta.paths.contains("pyproject.toml") {
        return rebuild_persistent(
            database,
            index_path,
            &root,
            &configuration,
            boundary_paths,
            &configuration_fingerprint,
            &repository_identity,
            IndexFallbackReason::ConfigurationChanged,
            timings,
        );
    }

    let update_started = Instant::now();
    let update = match plan_incremental_update(
        &database,
        &root,
        &configuration,
        &metadata,
        delta,
        boundary_paths,
    ) {
        Ok(update) => update,
        Err(UpdateError::Analysis(error)) => return Err(error),
        Err(UpdateError::SourceRootRemapped) => {
            timings.update = update_started.elapsed();
            return rebuild_persistent(
                database,
                index_path,
                &root,
                &configuration,
                boundary_paths,
                &configuration_fingerprint,
                &repository_identity,
                IndexFallbackReason::SourceRootRemapped,
                timings,
            );
        }
        Err(UpdateError::Storage) => {
            timings.update = update_started.elapsed();
            return rebuild_persistent(
                database,
                index_path,
                &root,
                &configuration,
                boundary_paths,
                &configuration_fingerprint,
                &repository_identity,
                IndexFallbackReason::StorageFailure,
                timings,
            );
        }
    };
    timings.update = update_started.elapsed();
    let summary = GraphSummary::from(&update.metadata.summary);
    let source_roots = update.metadata.source_root_paths();
    let mut work = update.work;

    if update.has_writes() {
        let persistence_started = Instant::now();
        if persist_update(&database, &update).is_err() {
            drop(database);
            return full_memory_fallback(
                &root,
                &configuration,
                boundary_paths,
                IndexFallbackReason::StorageFailure,
            );
        }
        timings.persistence = persistence_started.elapsed();
        work.build_kind = IndexBuildKind::Incremental;
    } else {
        work.build_kind = IndexBuildKind::Reused;
    }
    drop(database);

    Ok(BuiltIndex {
        store: IndexStore::Persistent(index_path),
        summary,
        source_roots,
        work,
        timings,
    })
}

fn full_memory_fallback(
    root: &Path,
    configuration: &RepositoryConfig,
    boundary_paths: &[PathBuf],
    reason: IndexFallbackReason,
) -> Result<BuiltIndex, AnalysisError> {
    let mut work = IndexWorkStats {
        fallback_reason: Some(reason),
        ..IndexWorkStats::default()
    };
    let (index, source_roots) = build_full(root, configuration, boundary_paths, &mut work)?;
    let summary = index.summary();
    Ok(BuiltIndex {
        store: IndexStore::Memory(index),
        summary,
        source_roots,
        work,
        timings: IndexTimings::default(),
    })
}

#[allow(clippy::too_many_arguments)]
fn rebuild_persistent(
    database: Database,
    index_path: PathBuf,
    root: &Path,
    configuration: &RepositoryConfig,
    boundary_paths: &[PathBuf],
    configuration_fingerprint: &str,
    repository_identity: &str,
    reason: IndexFallbackReason,
    mut timings: IndexTimings,
) -> Result<BuiltIndex, AnalysisError> {
    let update_started = Instant::now();
    let mut work = IndexWorkStats {
        fallback_reason: Some(reason),
        build_kind: IndexBuildKind::Full,
        ..IndexWorkStats::default()
    };
    let (index, source_roots) = build_full(root, configuration, boundary_paths, &mut work)?;
    timings.update = update_started.elapsed();
    let summary = index.summary();
    let metadata = IndexMetadata {
        schema_version: INDEX_SCHEMA_VERSION,
        parser: IMPORT_ANALYSIS_CACHE_TAG.to_owned(),
        resolver: RESOLVER_COMPATIBILITY_TAG.to_owned(),
        repository: repository_identity.to_owned(),
        repository_path: root.to_string_lossy().into_owned(),
        configuration: configuration_fingerprint.to_owned(),
        generation: 1,
        source_roots: source_roots
            .iter()
            .map(|path| display_repository_path(path))
            .collect(),
        summary: StoredSummary::from(&summary),
        git_baseline: capture_git_baseline(root).ok(),
    };
    let persistence_started = Instant::now();
    let persisted = persist_full(&database, &index, &metadata, &mut work).is_ok();
    timings.persistence = persistence_started.elapsed();
    drop(database);
    Ok(BuiltIndex {
        store: if persisted {
            IndexStore::Persistent(index_path)
        } else {
            work.fallback_reason = Some(IndexFallbackReason::StorageFailure);
            IndexStore::Memory(index)
        },
        summary,
        source_roots,
        work,
        timings,
    })
}

fn build_full(
    root: &Path,
    configuration: &RepositoryConfig,
    boundary_paths: &[PathBuf],
    work: &mut IndexWorkStats,
) -> Result<(MemoryIndex, Vec<PathBuf>), AnalysisError> {
    let excluder = configuration.path_excluder()?;
    let (paths, discovery) = discover_python_files_profiled(root, &excluder)?;
    work.directories_inspected += discovery.directories_inspected;
    work.inventory_entries_inspected += discovery.entries_inspected;
    if paths.is_empty() {
        return Err(AnalysisError::NoPythonFiles {
            root: root.to_path_buf(),
        });
    }

    let mut resolver_paths = paths.clone();
    resolver_paths.extend(boundary_paths.iter().cloned());
    let resolver = configuration.module_resolver(root, &resolver_paths)?;
    let source_roots = resolver.source_roots().to_vec();
    let mut files = BTreeMap::new();
    let mut modules = BTreeMap::<String, String>::new();
    for path in paths {
        let file = resolver.module_for_path(&path)?;
        let key = display_repository_path(&path);
        if let Some(first) = modules.insert(file.module.clone(), key.clone()) {
            return Err(AnalysisError::DuplicateModule {
                module: file.module,
                first: portable_path(&first),
                second: path,
            });
        }
        let absolute = root.join(&path);
        let metadata = fs::metadata(&absolute).map_err(|source| AnalysisError::SourceRead {
            path: path.clone(),
            source,
        })?;
        work.files_statted += 1;
        let source = fs::read_to_string(&absolute).map_err(|source| AnalysisError::SourceRead {
            path: path.clone(),
            source,
        })?;
        work.files_read += 1;
        let hash = content_hash(&source);
        work.files_hashed += 1;
        let imports = parse_imports_with_locations(&source, &path)?;
        work.files_parsed += 1;
        files.insert(
            key.clone(),
            StoredFile {
                path: key,
                module: file.module,
                is_package: file.is_package,
                is_test: file.is_test,
                state: StoredFileState::from_metadata(&metadata),
                content_hash: hash,
                imports,
                resolutions: Vec::new(),
                dependencies: BTreeMap::new(),
            },
        );
    }

    for file in files.values_mut() {
        resolve_file(file, |module| modules.get(module).cloned());
        work.importers_reresolved += 1;
    }
    let mut index = MemoryIndex {
        files,
        modules,
        reverse: BTreeMap::new(),
        candidates: BTreeMap::new(),
    };
    index.rebuild_relationships();
    let summary = index.summary();
    work.modules_added += summary.modules;
    work.records_added += summary.python_files;
    work.forward_edges_added += summary.import_edges;
    work.reverse_edges_added += summary.import_edges;
    work.index_records_written += summary.python_files;
    Ok((index, source_roots))
}

fn resolve_file(file: &mut StoredFile, mut module_path: impl FnMut(&str) -> Option<String>) {
    let python_file = file.python_file();
    file.resolutions.clear();
    file.dependencies.clear();
    for import in &file.imports {
        let resolution = resolve_local_import_with(&python_file, &import.import, |module| {
            module_path(module).is_some()
        });
        for module in &resolution.resolved_modules {
            let Some(path) = module_path(module) else {
                continue;
            };
            if module == &file.module {
                continue;
            }
            let evidence = file.dependencies.entry(path).or_default();
            if !evidence.contains(import) {
                evidence.push(import.clone());
            }
        }
        file.resolutions.push(StoredResolution {
            import: import.clone(),
            resolution,
        });
    }
}

fn unresolved_count(file: &StoredFile) -> usize {
    file.resolutions
        .iter()
        .filter(|entry| entry.resolution.resolved_modules.is_empty())
        .count()
}

fn resolution_status(resolution: &LocalImportResolution) -> ImportResolutionStatus {
    if !resolution.resolved_modules.is_empty() {
        ImportResolutionStatus::Resolved
    } else if resolution.failure == Some(ImportResolutionFailure::RelativeBeyondTopLevel) {
        ImportResolutionStatus::InvalidRelativeImport
    } else {
        ImportResolutionStatus::Unresolved
    }
}

fn read_metadata(database: &Database) -> Result<Option<IndexMetadata>, String> {
    let transaction = database.begin_read().map_err(string_error)?;
    read_json(&transaction, META_TABLE, META_KEY)
}

fn read_json<T: for<'de> Deserialize<'de>>(
    transaction: &ReadTransaction,
    table_definition: TableDefinition<&str, &[u8]>,
    key: &str,
) -> Result<Option<T>, String> {
    let table = transaction
        .open_table(table_definition)
        .map_err(string_error)?;
    let Some(value) = table.get(key).map_err(string_error)? else {
        return Ok(None);
    };
    serde_json::from_slice(value.value())
        .map(Some)
        .map_err(string_error)
}

fn read_all_json<T: for<'de> Deserialize<'de>>(
    transaction: &ReadTransaction,
    table_definition: TableDefinition<&str, &[u8]>,
) -> Result<Vec<T>, String> {
    let table = transaction
        .open_table(table_definition)
        .map_err(string_error)?;
    let mut values = Vec::new();
    for entry in table.iter().map_err(string_error)? {
        let (_, value) = entry.map_err(string_error)?;
        values.push(serde_json::from_slice(value.value()).map_err(string_error)?);
    }
    Ok(values)
}

fn persist_full(
    database: &Database,
    index: &MemoryIndex,
    metadata: &IndexMetadata,
    work: &mut IndexWorkStats,
) -> Result<(), String> {
    let transaction = database.begin_write().map_err(string_error)?;
    transaction.delete_table(META_TABLE).ok();
    transaction.delete_table(FILES_TABLE).ok();
    transaction.delete_table(MODULES_TABLE).ok();
    transaction.delete_table(REVERSE_TABLE).ok();
    transaction.delete_table(CANDIDATES_TABLE).ok();
    {
        let mut table = transaction.open_table(META_TABLE).map_err(string_error)?;
        work.bytes_written += insert_json(&mut table, META_KEY, metadata)?;
        work.index_records_written += 1;
    }
    {
        let mut table = transaction.open_table(FILES_TABLE).map_err(string_error)?;
        for (path, file) in &index.files {
            work.bytes_written += insert_json(&mut table, path, file)?;
        }
    }
    {
        let mut table = transaction
            .open_table(MODULES_TABLE)
            .map_err(string_error)?;
        for (module, path) in &index.modules {
            work.bytes_written += insert_json(&mut table, module, path)?;
        }
    }
    {
        let mut table = transaction
            .open_table(REVERSE_TABLE)
            .map_err(string_error)?;
        for (path, dependents) in &index.reverse {
            let dependents: Vec<_> = dependents.iter().cloned().collect();
            work.bytes_written += insert_json(&mut table, path, &dependents)?;
        }
    }
    {
        let mut table = transaction
            .open_table(CANDIDATES_TABLE)
            .map_err(string_error)?;
        for (candidate, importers) in &index.candidates {
            let importers: Vec<_> = importers.iter().cloned().collect();
            work.bytes_written += insert_json(&mut table, candidate, &importers)?;
        }
    }
    transaction.commit().map_err(string_error)
}

fn insert_json<T: Serialize>(
    table: &mut redb::Table<'_, &str, &[u8]>,
    key: &str,
    value: &T,
) -> Result<u64, String> {
    let bytes = serde_json::to_vec(value).map_err(string_error)?;
    table.insert(key, bytes.as_slice()).map_err(string_error)?;
    Ok((key.len() + bytes.len()) as u64)
}

fn configuration_fingerprint(configuration: &RepositoryConfig) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"urmare-repository-configuration-v3\0");
    match configuration.source_roots() {
        Some(roots) => {
            hasher.update(b"source-roots\0explicit\0");
            for root in roots {
                hasher.update(display_repository_path(root).as_bytes());
                hasher.update(b"\0");
            }
        }
        None => {
            hasher.update(b"source-roots\0inferred\0");
        }
    }
    match configuration.test_roots() {
        Some(roots) => {
            hasher.update(b"test-roots\0explicit\0");
            for root in roots {
                hasher.update(display_repository_path(root).as_bytes());
                hasher.update(b"\0");
            }
        }
        None => {
            hasher.update(b"test-roots\0conventional\0");
        }
    }
    hasher.update(b"exclude\0");
    for pattern in configuration.excludes() {
        hasher.update(pattern.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

fn portable_path(path: &str) -> PathBuf {
    path.split('/').collect()
}

fn string_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn index_query_error(error: impl std::fmt::Display) -> AnalysisError {
    AnalysisError::IndexUnavailable(error.to_string())
}

#[derive(Debug)]
struct DetectedDelta {
    current: GitBaseline,
    paths: BTreeSet<String>,
    inventory_entries: usize,
}

#[derive(Clone, Copy, Debug)]
enum DeltaError {
    NonGit,
    Git,
}

fn detect_delta(root: &Path, previous: Option<&GitBaseline>) -> Result<DetectedDelta, DeltaError> {
    let Some(previous) = previous else {
        return Err(DeltaError::Git);
    };
    let current = capture_git_baseline(root)?;
    let mut paths = BTreeSet::new();
    paths.extend(previous.working_paths.iter().cloned());
    paths.extend(current.working_paths.iter().cloned());
    paths.extend(previous.ignored_python_paths.iter().cloned());
    paths.extend(current.ignored_python_paths.iter().cloned());
    let mut inventory_entries = paths.len();

    if previous.head != current.head {
        let (Some(previous_head), Some(current_head)) = (&previous.head, &current.head) else {
            return Err(DeltaError::Git);
        };
        let committed = git_name_status(
            root,
            &[
                "diff",
                "--name-status",
                "-z",
                "--find-renames",
                previous_head,
                current_head,
                "--",
            ],
        )?;
        inventory_entries += committed.len();
        paths.extend(committed);
    }

    Ok(DetectedDelta {
        current,
        paths,
        inventory_entries,
    })
}

fn capture_git_baseline(root: &Path) -> Result<GitBaseline, DeltaError> {
    let top_level = git_output(root, &["rev-parse", "--show-toplevel"])?;
    let top_level = PathBuf::from(top_level.trim())
        .canonicalize()
        .map_err(|_| DeltaError::Git)?;
    if top_level != root {
        return Err(DeltaError::NonGit);
    }

    let head_output = run_git(root, &["rev-parse", "--verify", "HEAD"])?;
    let head = head_output
        .status
        .success()
        .then(|| String::from_utf8(head_output.stdout).ok())
        .flatten()
        .map(|head| head.trim().to_owned());

    let mut working_paths = if head.is_some() {
        git_name_status(
            root,
            &[
                "diff",
                "--name-status",
                "-z",
                "--find-renames",
                "HEAD",
                "--",
            ],
        )?
    } else {
        Vec::new()
    };
    working_paths.extend(git_path_list(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
    )?);
    working_paths.retain(|path| is_analysis_input(path));
    working_paths.sort();
    working_paths.dedup();

    let mut ignored_python_paths = git_path_list(
        root,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
            "--",
            "*.py",
        ],
    )?;
    ignored_python_paths.retain(|path| path.ends_with(".py"));
    ignored_python_paths.sort();
    ignored_python_paths.dedup();

    Ok(GitBaseline {
        head,
        working_paths,
        ignored_python_paths,
    })
}

fn git_name_status(root: &Path, arguments: &[&str]) -> Result<Vec<String>, DeltaError> {
    let output = run_git(root, arguments)?;
    if !output.status.success() {
        return Err(if arguments.first() == Some(&"rev-parse") {
            DeltaError::NonGit
        } else {
            DeltaError::Git
        });
    }
    let fields: Vec<_> = output.stdout.split(|byte| *byte == 0).collect();
    let mut paths = Vec::new();
    let mut index = 0;
    while index < fields.len() && !fields[index].is_empty() {
        let status = std::str::from_utf8(fields[index]).map_err(|_| DeltaError::Git)?;
        index += 1;
        let Some(first) = fields.get(index) else {
            return Err(DeltaError::Git);
        };
        index += 1;
        let first = git_repository_path(first)?;
        if status.starts_with(['R', 'C']) {
            let Some(second) = fields.get(index) else {
                return Err(DeltaError::Git);
            };
            index += 1;
            let second = git_repository_path(second)?;
            if status.starts_with('R') {
                paths.push(first);
            }
            paths.push(second);
        } else {
            paths.push(first);
        }
    }
    paths.retain(|path| is_analysis_input(path));
    Ok(paths)
}

fn git_path_list(root: &Path, arguments: &[&str]) -> Result<Vec<String>, DeltaError> {
    let output = run_git(root, arguments)?;
    if !output.status.success() {
        return Err(DeltaError::Git);
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(git_repository_path)
        .collect()
}

fn git_repository_path(path: &[u8]) -> Result<String, DeltaError> {
    let path = std::str::from_utf8(path).map_err(|_| DeltaError::Git)?;
    let native = Path::new(path);
    if native.is_absolute()
        || native
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(DeltaError::Git);
    }
    Ok(display_repository_path(native))
}

fn git_output(root: &Path, arguments: &[&str]) -> Result<String, DeltaError> {
    let output = run_git(root, arguments)?;
    if !output.status.success() {
        return Err(DeltaError::NonGit);
    }
    String::from_utf8(output.stdout).map_err(|_| DeltaError::Git)
}

fn run_git(root: &Path, arguments: &[&str]) -> Result<Output, DeltaError> {
    Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .map_err(|_| DeltaError::NonGit)
}

fn is_analysis_input(path: &str) -> bool {
    path == "pyproject.toml" || path.ends_with(".py")
}

#[derive(Debug)]
enum UpdateError {
    Analysis(AnalysisError),
    SourceRootRemapped,
    Storage,
}

impl From<AnalysisError> for UpdateError {
    fn from(error: AnalysisError) -> Self {
        Self::Analysis(error)
    }
}

#[derive(Debug)]
struct IncrementalUpdate {
    metadata: IndexMetadata,
    files: BTreeMap<String, Option<StoredFile>>,
    modules: BTreeMap<String, Option<String>>,
    reverse: BTreeMap<String, BTreeSet<String>>,
    candidates: BTreeMap<String, BTreeSet<String>>,
    work: IndexWorkStats,
    metadata_changed: bool,
}

impl IncrementalUpdate {
    fn has_writes(&self) -> bool {
        self.metadata_changed
            || !self.files.is_empty()
            || !self.modules.is_empty()
            || !self.reverse.is_empty()
            || !self.candidates.is_empty()
    }
}

fn plan_incremental_update(
    database: &Database,
    root: &Path,
    configuration: &RepositoryConfig,
    metadata: &IndexMetadata,
    delta: DetectedDelta,
    boundary_paths: &[PathBuf],
) -> Result<IncrementalUpdate, UpdateError> {
    let excluder = configuration.path_excluder().map_err(AnalysisError::from)?;
    let resolver = incremental_resolver(root, configuration, metadata, boundary_paths)?;
    let transaction = database.begin_read().map_err(|_| UpdateError::Storage)?;
    let reader = PersistentReader {
        transaction: &transaction,
    };
    let mut work = IndexWorkStats {
        inventory_entries_inspected: delta.inventory_entries,
        ..IndexWorkStats::default()
    };
    let mut files = BTreeMap::<String, Option<StoredFile>>::new();
    let mut old_files = BTreeMap::<String, Option<StoredFile>>::new();
    let mut modules = BTreeMap::<String, Option<String>>::new();
    let mut changed_modules = BTreeSet::new();
    let mut needs_resolution = BTreeSet::new();

    for key in &delta.paths {
        if !key.ends_with(".py") {
            continue;
        }
        let path = portable_path(key);
        let old = reader.file(key).map_err(|_| UpdateError::Storage)?;
        work.index_records_read += usize::from(old.is_some());
        let current = current_file(root, &path, &excluder, &resolver, old.as_ref(), &mut work)?;
        old_files.insert(key.clone(), old.clone());

        match (old, current) {
            (None, None) => {}
            (Some(old), None) => {
                changed_modules.insert(old.module.clone());
                modules.insert(old.module.clone(), None);
                files.insert(key.clone(), None);
                work.modules_removed += 1;
                work.records_removed += 1;
            }
            (None, Some(mut current)) => {
                check_module_collision(&reader, &modules, &current.module, key)?;
                changed_modules.insert(current.module.clone());
                modules.insert(current.module.clone(), Some(key.clone()));
                needs_resolution.insert(key.clone());
                current.resolutions.clear();
                current.dependencies.clear();
                files.insert(key.clone(), Some(current));
                work.modules_added += 1;
                work.records_added += 1;
            }
            (Some(old), Some(mut current)) => {
                if old.module != current.module {
                    changed_modules.insert(old.module.clone());
                    changed_modules.insert(current.module.clone());
                    modules.insert(old.module.clone(), None);
                    check_module_collision(&reader, &modules, &current.module, key)?;
                    modules.insert(current.module.clone(), Some(key.clone()));
                    work.modules_remapped += 1;
                    needs_resolution.insert(key.clone());
                } else {
                    work.modules_reused += 1;
                }

                if current.content_hash == old.content_hash {
                    current.imports = old.imports.clone();
                    current.resolutions = old.resolutions.clone();
                    current.dependencies = old.dependencies.clone();
                } else if current.imports == old.imports
                    && current.module == old.module
                    && current.is_package == old.is_package
                {
                    current.resolutions = old.resolutions.clone();
                    current.dependencies = old.dependencies.clone();
                } else {
                    needs_resolution.insert(key.clone());
                }
                if current != old {
                    files.insert(key.clone(), Some(current));
                }
            }
        }
    }

    for module in &changed_modules {
        for importer in reader
            .candidate_importers(module)
            .map_err(|_| UpdateError::Storage)?
        {
            work.index_records_read += 1;
            if !matches!(files.get(&importer), Some(None)) {
                needs_resolution.insert(importer);
            }
        }
    }

    for path in needs_resolution {
        let mut file = match files.get(&path).cloned().flatten() {
            Some(file) => file,
            None => reader
                .file(&path)
                .map_err(|_| UpdateError::Storage)?
                .ok_or(UpdateError::Storage)?,
        };
        old_files
            .entry(path.clone())
            .or_insert_with(|| reader.file(&path).ok().flatten());
        resolve_file(&mut file, |module| {
            planned_module_path(&reader, &modules, module)
                .ok()
                .flatten()
        });
        work.importers_reresolved += 1;
        if old_files.get(&path).and_then(Option::as_ref) != Some(&file) {
            files.insert(path, Some(file));
        }
    }

    let mut reverse = BTreeMap::<String, BTreeSet<String>>::new();
    let mut candidates = BTreeMap::<String, BTreeSet<String>>::new();
    let updates: Vec<_> = files
        .iter()
        .map(|(path, new)| {
            (
                path.clone(),
                old_files.get(path).cloned().flatten(),
                new.clone(),
            )
        })
        .collect();
    for (path, old, new) in &updates {
        update_relationship_sets(
            &reader,
            path,
            old.as_ref(),
            new.as_ref(),
            &mut reverse,
            &mut candidates,
            &mut work,
        )?;
    }

    let mut summary = metadata.summary.clone();
    for (_, old, new) in &updates {
        apply_summary_delta(&mut summary, old.as_ref(), new.as_ref());
    }
    if summary.python_files == 0 {
        return Err(UpdateError::Analysis(AnalysisError::NoPythonFiles {
            root: root.to_path_buf(),
        }));
    }

    let metadata_changed = metadata.git_baseline.as_ref() != Some(&delta.current)
        || !files.is_empty()
        || !modules.is_empty();
    let mut updated_metadata = metadata.clone();
    if metadata_changed {
        updated_metadata.generation = metadata.generation.saturating_add(1);
        updated_metadata.summary = summary;
        updated_metadata.git_baseline = Some(delta.current);
    }
    work.index_records_written = files.len()
        + modules.len()
        + reverse.len()
        + candidates.len()
        + usize::from(metadata_changed);

    Ok(IncrementalUpdate {
        metadata: updated_metadata,
        files,
        modules,
        reverse,
        candidates,
        work,
        metadata_changed,
    })
}

fn incremental_resolver(
    root: &Path,
    configuration: &RepositoryConfig,
    metadata: &IndexMetadata,
    boundary_paths: &[PathBuf],
) -> Result<ModuleResolver, UpdateError> {
    // Passing no virtual inventory validates that configured roots still exist
    // in the current tree. Configuration itself is handled as a full rebuild.
    let resolver = configuration
        .module_resolver(root, boundary_paths)
        .map_err(AnalysisError::from)?;
    if resolver.source_roots() != metadata.source_root_paths() {
        return Err(UpdateError::SourceRootRemapped);
    }
    Ok(resolver)
}

fn current_file(
    root: &Path,
    path: &Path,
    excluder: &PathExcluder,
    resolver: &ModuleResolver,
    previous: Option<&StoredFile>,
    work: &mut IndexWorkStats,
) -> Result<Option<StoredFile>, UpdateError> {
    if !is_discoverable_python_path(path, excluder) {
        return Ok(None);
    }
    let absolute = root.join(path);
    let metadata = match fs::symlink_metadata(&absolute) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(UpdateError::Analysis(AnalysisError::SourceRead {
                path: path.to_path_buf(),
                source,
            }));
        }
    };
    work.files_statted += 1;
    if !metadata.file_type().is_file() {
        return Ok(None);
    }
    let python_file = resolver
        .module_for_path(path)
        .map_err(AnalysisError::from)?;
    let source = fs::read_to_string(&absolute).map_err(|source| AnalysisError::SourceRead {
        path: path.to_path_buf(),
        source,
    })?;
    work.files_read += 1;
    let hash = content_hash(&source);
    work.files_hashed += 1;
    let imports = if previous.is_some_and(|previous| previous.content_hash == hash) {
        previous
            .map(|previous| previous.imports.clone())
            .unwrap_or_default()
    } else {
        let imports = parse_imports_with_locations(&source, path).map_err(AnalysisError::from)?;
        work.files_parsed += 1;
        imports
    };
    Ok(Some(StoredFile {
        path: display_repository_path(path),
        module: python_file.module,
        is_package: python_file.is_package,
        is_test: python_file.is_test,
        state: StoredFileState::from_metadata(&metadata),
        content_hash: hash,
        imports,
        resolutions: Vec::new(),
        dependencies: BTreeMap::new(),
    }))
}

fn check_module_collision(
    reader: &dyn IndexRead,
    planned: &BTreeMap<String, Option<String>>,
    module: &str,
    path: &str,
) -> Result<(), UpdateError> {
    let existing =
        planned_module_path(reader, planned, module).map_err(|_| UpdateError::Storage)?;
    if let Some(existing) = existing
        && existing != path
    {
        return Err(UpdateError::Analysis(AnalysisError::DuplicateModule {
            module: module.to_owned(),
            first: portable_path(&existing),
            second: portable_path(path),
        }));
    }
    Ok(())
}

fn planned_module_path(
    reader: &dyn IndexRead,
    planned: &BTreeMap<String, Option<String>>,
    module: &str,
) -> Result<Option<String>, String> {
    match planned.get(module) {
        Some(path) => Ok(path.clone()),
        None => reader.module_path(module),
    }
}

#[allow(clippy::too_many_arguments)]
fn update_relationship_sets(
    reader: &dyn IndexRead,
    path: &str,
    old: Option<&StoredFile>,
    new: Option<&StoredFile>,
    reverse: &mut BTreeMap<String, BTreeSet<String>>,
    candidates: &mut BTreeMap<String, BTreeSet<String>>,
    work: &mut IndexWorkStats,
) -> Result<(), UpdateError> {
    let old_dependencies: BTreeSet<_> = old
        .into_iter()
        .flat_map(|file| file.dependencies.keys().cloned())
        .collect();
    let new_dependencies: BTreeSet<_> = new
        .into_iter()
        .flat_map(|file| file.dependencies.keys().cloned())
        .collect();
    for dependency in old_dependencies.union(&new_dependencies) {
        if !reverse.contains_key(dependency) {
            reverse.insert(
                dependency.clone(),
                reader
                    .reverse_dependents(dependency)
                    .map_err(|_| UpdateError::Storage)?
                    .into_iter()
                    .collect(),
            );
            work.index_records_read += 1;
        }
        let dependents = reverse.get_mut(dependency).ok_or(UpdateError::Storage)?;
        let was_present = old_dependencies.contains(dependency);
        let is_present = new_dependencies.contains(dependency);
        if was_present && !is_present && dependents.remove(path) {
            work.forward_edges_removed += 1;
            work.reverse_edges_removed += 1;
        } else if !was_present && is_present && dependents.insert(path.to_owned()) {
            work.forward_edges_added += 1;
            work.reverse_edges_added += 1;
        }
    }

    let old_candidates = file_candidates(old);
    let new_candidates = file_candidates(new);
    for candidate in old_candidates.union(&new_candidates) {
        if !candidates.contains_key(candidate) {
            candidates.insert(
                candidate.clone(),
                reader
                    .candidate_importers(candidate)
                    .map_err(|_| UpdateError::Storage)?
                    .into_iter()
                    .collect(),
            );
            work.index_records_read += 1;
        }
        let importers = candidates.get_mut(candidate).ok_or(UpdateError::Storage)?;
        if old_candidates.contains(candidate) && !new_candidates.contains(candidate) {
            importers.remove(path);
        } else if !old_candidates.contains(candidate) && new_candidates.contains(candidate) {
            importers.insert(path.to_owned());
        }
    }
    Ok(())
}

fn file_candidates(file: Option<&StoredFile>) -> BTreeSet<String> {
    file.into_iter()
        .flat_map(|file| &file.resolutions)
        .flat_map(|entry| &entry.resolution.candidate_modules)
        .cloned()
        .collect()
}

fn apply_summary_delta(
    summary: &mut StoredSummary,
    old: Option<&StoredFile>,
    new: Option<&StoredFile>,
) {
    if let Some(old) = old {
        summary.python_files = summary.python_files.saturating_sub(1);
        summary.modules = summary.modules.saturating_sub(1);
        summary.import_edges = summary.import_edges.saturating_sub(old.dependencies.len());
        summary.tests = summary.tests.saturating_sub(usize::from(old.is_test));
        summary.unresolved_imports = summary
            .unresolved_imports
            .saturating_sub(unresolved_count(old));
    }
    if let Some(new) = new {
        summary.python_files += 1;
        summary.modules += 1;
        summary.import_edges += new.dependencies.len();
        summary.tests += usize::from(new.is_test);
        summary.unresolved_imports += unresolved_count(new);
    }
}

fn persist_update(database: &Database, update: &IncrementalUpdate) -> Result<(), String> {
    let transaction = database.begin_write().map_err(string_error)?;
    if update.metadata_changed {
        let mut table = transaction.open_table(META_TABLE).map_err(string_error)?;
        insert_json(&mut table, META_KEY, &update.metadata)?;
    }
    if !update.files.is_empty() {
        let mut table = transaction.open_table(FILES_TABLE).map_err(string_error)?;
        for (path, file) in &update.files {
            match file {
                Some(file) => {
                    insert_json(&mut table, path, file)?;
                }
                None => {
                    table.remove(path.as_str()).map_err(string_error)?;
                }
            }
        }
    }
    if !update.modules.is_empty() {
        let mut table = transaction
            .open_table(MODULES_TABLE)
            .map_err(string_error)?;
        for (module, path) in &update.modules {
            match path {
                Some(path) => {
                    insert_json(&mut table, module, path)?;
                }
                None => {
                    table.remove(module.as_str()).map_err(string_error)?;
                }
            }
        }
    }
    if !update.reverse.is_empty() {
        let mut table = transaction
            .open_table(REVERSE_TABLE)
            .map_err(string_error)?;
        for (path, dependents) in &update.reverse {
            if dependents.is_empty() {
                table.remove(path.as_str()).map_err(string_error)?;
            } else {
                let dependents: Vec<_> = dependents.iter().cloned().collect();
                insert_json(&mut table, path, &dependents)?;
            }
        }
    }
    if !update.candidates.is_empty() {
        let mut table = transaction
            .open_table(CANDIDATES_TABLE)
            .map_err(string_error)?;
        for (candidate, importers) in &update.candidates {
            if importers.is_empty() {
                table.remove(candidate.as_str()).map_err(string_error)?;
            } else {
                let importers: Vec<_> = importers.iter().cloned().collect();
                insert_json(&mut table, candidate, &importers)?;
            }
        }
    }
    transaction.commit().map_err(string_error)
}

pub(crate) fn provenance(imports: &[LocatedImport]) -> Vec<ImportProvenance> {
    imports
        .iter()
        .map(|import| ImportProvenance {
            location: import.location,
            import: import.import.clone(),
        })
        .collect()
}
