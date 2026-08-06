use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use urmare_python::{LocalImportResolution, LocatedImport, PythonFile};

use crate::{
    cache::{CacheConfiguration, CacheLocation, configuration_fingerprint, write_atomic},
    display_repository_path,
};

const GRAPH_CACHE_SCHEMA_VERSION: u32 = 3;
const GRAPH_CACHE_FILE_NAME: &str = "graph-v3.json";
const RESOLUTION_CACHE_TAG: &str = "python-local-import-resolution-v3";

/// Persistent graph-index activity for one repository build.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphCacheStats {
    /// Repository paths whose cached Python module identity was reused.
    pub module_hits: usize,
    /// Present files whose resolved local dependencies were reused.
    pub edge_hits: usize,
    /// Present files whose local dependencies had to be resolved again.
    pub edge_misses: usize,
}

#[derive(Debug)]
pub(crate) struct GraphCache {
    path: Option<PathBuf>,
    configuration: String,
    cached_module_set: Option<String>,
    current_module_set: Option<String>,
    entries: BTreeMap<String, CachedGraphFile>,
    seen: HashSet<String>,
    stats: GraphCacheStats,
    dirty: bool,
}

impl GraphCache {
    pub(crate) fn load(
        location: CacheLocation,
        repository_root: &Path,
        configuration: CacheConfiguration<'_>,
    ) -> Self {
        let path = location
            .directory(repository_root)
            .map(|directory| directory.join(GRAPH_CACHE_FILE_NAME));
        let configuration = configuration_fingerprint(configuration);
        let document = path.as_deref().and_then(read_document).filter(|document| {
            document.schema_version == GRAPH_CACHE_SCHEMA_VERSION
                && document.resolver == RESOLUTION_CACHE_TAG
                && document.configuration == configuration
        });
        let (cached_module_set, entries) = document.map_or_else(
            || (None, BTreeMap::new()),
            |document| (Some(document.module_set), document.files),
        );

        Self {
            path,
            configuration,
            cached_module_set,
            current_module_set: None,
            entries,
            seen: HashSet::new(),
            stats: GraphCacheStats::default(),
            dirty: false,
        }
    }

    pub(crate) fn cached_module(&mut self, path: &Path) -> Option<String> {
        let module = self
            .entries
            .get(&display_repository_path(path))?
            .module
            .clone();
        if module.is_empty() {
            return None;
        }
        self.stats.module_hits += 1;
        Some(module)
    }

    /// Records the current module universe and reports whether resolved edges
    /// can be reused. Any module addition, removal, rename, or remapping
    /// invalidates every cached resolution because an unchanged import may
    /// have changed between external and repository-local.
    pub(crate) fn begin_resolution(&mut self, files: &[PythonFile]) -> bool {
        let current = module_set_fingerprint(files);
        let reusable = self.cached_module_set.as_deref() == Some(current.as_str());
        self.dirty |= self.cached_module_set.as_deref() != Some(current.as_str());
        self.current_module_set = Some(current);
        reusable
    }

    pub(crate) fn cached_resolution(&self, path: &Path, module: &str) -> Option<CachedResolution> {
        let entry = self.entries.get(&display_repository_path(path))?;
        (entry.module == module).then(|| CachedResolution {
            imports: entry.imports.clone(),
        })
    }

    pub(crate) fn record_reused(&mut self, path: &Path) {
        self.seen.insert(display_repository_path(path));
        self.stats.edge_hits += 1;
    }

    pub(crate) fn record_resolved(
        &mut self,
        path: &Path,
        module: &str,
        imports: Vec<CachedImportResolution>,
    ) {
        let key = display_repository_path(path);
        if self.path.is_some() {
            let entry = CachedGraphFile {
                module: module.to_owned(),
                imports,
            };
            self.dirty |= self.entries.get(&key) != Some(&entry);
            self.entries.insert(key.clone(), entry);
            self.seen.insert(key);
        }
        self.stats.edge_misses += 1;
    }

    pub(crate) fn stats(&self) -> GraphCacheStats {
        self.stats
    }

    /// Persists a complete current-tree graph index. Cache failures never
    /// prevent analysis because the in-memory graph is already authoritative.
    pub(crate) fn persist(&mut self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let previous_len = self.entries.len();
        self.entries.retain(|key, _| self.seen.contains(key));
        self.dirty |= self.entries.len() != previous_len;
        if !self.dirty {
            return Ok(());
        }
        let document = GraphCacheDocument {
            schema_version: GRAPH_CACHE_SCHEMA_VERSION,
            resolver: RESOLUTION_CACHE_TAG.to_owned(),
            configuration: self.configuration.clone(),
            module_set: self.current_module_set.clone().unwrap_or_default(),
            files: self.entries.clone(),
        };
        let serialized = serde_json::to_vec(&document).map_err(io::Error::other)?;
        write_atomic(path, &serialized)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CachedResolution {
    pub(crate) imports: Vec<CachedImportResolution>,
}

/// One located import and the deterministic result of local resolution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CachedImportResolution {
    pub(crate) import: LocatedImport,
    pub(crate) resolution: LocalImportResolution,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct GraphCacheDocument {
    schema_version: u32,
    resolver: String,
    configuration: String,
    module_set: String,
    files: BTreeMap<String, CachedGraphFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct CachedGraphFile {
    module: String,
    imports: Vec<CachedImportResolution>,
}

fn read_document(path: &Path) -> Option<GraphCacheDocument> {
    let source = fs::read(path).ok()?;
    serde_json::from_slice(&source).ok()
}

fn module_set_fingerprint(files: &[PythonFile]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"urmare-module-set-v1\0");
    for file in files {
        hasher.update(display_repository_path(&file.path).as_bytes());
        hasher.update(b"\0");
        hasher.update(file.module.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;
    use urmare_python::{
        LocalImportResolution, LocatedImport, ModuleResolver, SourceLocation, StaticImport,
    };

    use super::{CachedImportResolution, GraphCache, GraphCacheStats};
    use crate::cache::{CacheConfiguration, CacheLocation};

    fn configuration<'a>(source_roots: &'a [PathBuf]) -> CacheConfiguration<'a> {
        CacheConfiguration {
            source_roots,
            test_roots: &[],
            excludes: &[],
        }
    }

    fn file(path: &str) -> urmare_python::PythonFile {
        ModuleResolver::new("src")
            .module_for_path(Path::new(path))
            .expect("module")
    }

    #[test]
    fn reuses_identities_and_edges_only_for_the_same_module_set() {
        let directory = tempdir().expect("temporary cache");
        let location = CacheLocation::Directory(directory.path().to_path_buf());
        let files = vec![file("src/package/one.py"), file("src/package/two.py")];

        let source_roots = [PathBuf::from("src")];
        let resolutions = vec![
            CachedImportResolution {
                import: LocatedImport {
                    import: StaticImport::Import {
                        module: "package.two".into(),
                    },
                    location: SourceLocation { line: 1, column: 8 },
                },
                resolution: LocalImportResolution {
                    candidate_modules: vec!["package".into(), "package.two".into()],
                    resolved_modules: vec!["package.two".into()],
                    failure: None,
                },
            },
            CachedImportResolution {
                import: LocatedImport {
                    import: StaticImport::Import {
                        module: "external".into(),
                    },
                    location: SourceLocation { line: 3, column: 8 },
                },
                resolution: LocalImportResolution {
                    candidate_modules: vec!["external".into()],
                    resolved_modules: Vec::new(),
                    failure: None,
                },
            },
        ];
        let mut cold = GraphCache::load(
            location.clone(),
            Path::new("/repository"),
            configuration(&source_roots),
        );
        assert!(!cold.begin_resolution(&files));
        cold.record_resolved(&files[0].path, &files[0].module, resolutions.clone());
        cold.record_resolved(&files[1].path, &files[1].module, Vec::new());
        cold.persist().expect("graph cache persistence");

        let mut warm = GraphCache::load(
            location.clone(),
            Path::new("/repository"),
            configuration(&source_roots),
        );
        assert_eq!(
            warm.cached_module(&files[0].path),
            Some(files[0].module.clone())
        );
        assert!(warm.begin_resolution(&files));
        let resolution = warm
            .cached_resolution(&files[0].path, &files[0].module)
            .expect("cached resolution");
        assert_eq!(resolution.imports, resolutions);
        warm.record_reused(&files[0].path);
        assert_eq!(
            warm.stats(),
            GraphCacheStats {
                module_hits: 1,
                edge_hits: 1,
                edge_misses: 0,
            }
        );

        let mut expanded = files.clone();
        expanded.push(file("src/package/three.py"));
        assert!(!warm.begin_resolution(&expanded));
    }

    #[test]
    fn configuration_changes_and_corruption_disable_reuse() {
        let directory = tempdir().expect("temporary cache");
        let location = CacheLocation::Directory(directory.path().to_path_buf());
        let files = vec![file("src/package/one.py")];

        let source_roots = [PathBuf::from("src")];
        let mut cache = GraphCache::load(
            location.clone(),
            Path::new("/repository"),
            configuration(&source_roots),
        );
        cache.begin_resolution(&files);
        cache.record_resolved(&files[0].path, &files[0].module, Vec::new());
        cache.persist().expect("graph cache persistence");

        let changed_roots = [PathBuf::from("lib")];
        let mut changed = GraphCache::load(
            location.clone(),
            Path::new("/repository"),
            configuration(&changed_roots),
        );
        assert_eq!(changed.cached_module(&files[0].path), None);
        assert!(!changed.begin_resolution(&files));

        std::fs::write(directory.path().join("graph-v3.json"), "not JSON")
            .expect("corrupt graph cache");
        let mut recovered = GraphCache::load(
            location,
            Path::new("/repository"),
            configuration(&source_roots),
        );
        assert_eq!(recovered.cached_module(&files[0].path), None);
        assert!(!recovered.begin_resolution(&files));
    }
}
