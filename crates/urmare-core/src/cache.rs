use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use urmare_python::{IMPORT_ANALYSIS_CACHE_TAG, LocatedImport};

use crate::display_repository_path;

const CACHE_SCHEMA_VERSION: u32 = 2;
const CACHE_FILE_NAME: &str = "imports-v2.json";
static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Parsed-import cache activity for one repository build.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    /// Files reused because size and modification time were unchanged.
    pub metadata_hits: usize,
    /// Files reused after metadata changed but the content hash matched.
    pub content_hits: usize,
    /// Files whose imports had to be parsed again.
    pub misses: usize,
}

impl CacheStats {
    /// Total number of files whose parsed imports were reused.
    pub const fn hits(self) -> usize {
        self.metadata_hits + self.content_hits
    }
}

#[derive(Clone, Debug)]
pub(crate) enum CacheLocation {
    Disabled,
    Default,
    Directory(PathBuf),
}

impl CacheLocation {
    pub(crate) fn directory(&self, repository_root: &Path) -> Option<PathBuf> {
        match self {
            Self::Disabled => None,
            Self::Default => default_cache_directory(repository_root),
            Self::Directory(directory) => Some(directory.clone()),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ImportCache {
    path: Option<PathBuf>,
    configuration: String,
    entries: BTreeMap<String, CachedFile>,
    seen: HashSet<String>,
    stats: CacheStats,
    dirty: bool,
}

impl ImportCache {
    pub(crate) fn load(
        location: CacheLocation,
        repository_root: &Path,
        configuration: CacheConfiguration<'_>,
    ) -> Self {
        let path = location
            .directory(repository_root)
            .map(|directory| directory.join(CACHE_FILE_NAME));
        let configuration = configuration_fingerprint(configuration);
        let entries = path
            .as_deref()
            .and_then(read_document)
            .filter(|document| {
                document.schema_version == CACHE_SCHEMA_VERSION
                    && document.parser == IMPORT_ANALYSIS_CACHE_TAG
                    && document.configuration == configuration
            })
            .map(|document| document.files)
            .unwrap_or_default();

        Self {
            path,
            configuration,
            entries,
            seen: HashSet::new(),
            stats: CacheStats::default(),
            dirty: false,
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.path.is_some()
    }

    pub(crate) fn metadata_hit(
        &mut self,
        path: &Path,
        state: FileState,
    ) -> Option<Vec<LocatedImport>> {
        let key = display_repository_path(path);
        let entry = self.entries.get(&key)?;
        let unchanged = entry.size == state.size
            && entry.modified_ns.is_some()
            && entry.modified_ns == state.modified_ns;
        if !unchanged {
            return None;
        }

        self.seen.insert(key);
        self.stats.metadata_hits += 1;
        Some(entry.imports.clone())
    }

    pub(crate) fn content_hit(
        &mut self,
        path: &Path,
        state: FileState,
        content_hash: &str,
    ) -> Option<Vec<LocatedImport>> {
        let key = display_repository_path(path);
        let entry = self.entries.get_mut(&key)?;
        if entry.content_hash != content_hash {
            return None;
        }

        entry.size = state.size;
        entry.modified_ns = state.modified_ns;
        self.seen.insert(key);
        self.stats.content_hits += 1;
        self.dirty = true;
        Some(entry.imports.clone())
    }

    pub(crate) fn record_parsed(
        &mut self,
        path: &Path,
        state: FileState,
        content_hash: String,
        imports: &[LocatedImport],
    ) {
        let key = display_repository_path(path);
        if self.is_enabled() {
            self.entries.insert(
                key.clone(),
                CachedFile {
                    size: state.size,
                    modified_ns: state.modified_ns,
                    content_hash,
                    imports: imports.to_vec(),
                },
            );
            self.seen.insert(key);
            self.dirty = true;
        }
        self.stats.misses += 1;
    }

    pub(crate) fn record_uncached_parse(&mut self) {
        self.stats.misses += 1;
    }

    pub(crate) fn stats(&self) -> CacheStats {
        self.stats
    }

    /// Persists a complete current-tree cache. Failures are intentionally
    /// ignored by callers because caching must never prevent correct analysis.
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
        let document = CacheDocument {
            schema_version: CACHE_SCHEMA_VERSION,
            parser: IMPORT_ANALYSIS_CACHE_TAG.to_owned(),
            configuration: self.configuration.clone(),
            files: self.entries.clone(),
        };
        let serialized = serde_json::to_vec(&document).map_err(io::Error::other)?;
        write_atomic(path, &serialized)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FileState {
    size: u64,
    modified_ns: Option<u128>,
}

impl FileState {
    pub(crate) fn from_metadata(metadata: &fs::Metadata) -> Self {
        let modified_ns = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        Self {
            size: metadata.len(),
            modified_ns,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CacheDocument {
    schema_version: u32,
    parser: String,
    configuration: String,
    files: BTreeMap<String, CachedFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CachedFile {
    size: u64,
    modified_ns: Option<u128>,
    content_hash: String,
    imports: Vec<LocatedImport>,
}

fn read_document(path: &Path) -> Option<CacheDocument> {
    let source = fs::read(path).ok()?;
    serde_json::from_slice(&source).ok()
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CacheConfiguration<'a> {
    pub(crate) source_roots: &'a [PathBuf],
    pub(crate) test_roots: &'a [PathBuf],
    pub(crate) excludes: &'a [String],
}

pub(crate) fn configuration_fingerprint(configuration: CacheConfiguration<'_>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"urmare-repository-configuration-v2\0source-roots\0");
    for source_root in configuration.source_roots {
        hasher.update(display_repository_path(source_root).as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"test-roots\0");
    for test_root in configuration.test_roots {
        hasher.update(display_repository_path(test_root).as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"exclude\0");
    for pattern in configuration.excludes {
        hasher.update(pattern.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

pub(crate) fn content_hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

fn default_cache_directory(repository_root: &Path) -> Option<PathBuf> {
    let project = ProjectDirs::from("org", "Sorginte", "Urmare")?;
    let repository = repository_fingerprint(repository_root);
    Some(project.cache_dir().join("repositories").join(repository))
}

fn repository_fingerprint(repository_root: &Path) -> String {
    let mut hasher = blake3::Hasher::new();
    update_hasher_with_path(&mut hasher, repository_root);
    hasher.finalize().to_hex().to_string()
}

#[cfg(unix)]
fn update_hasher_with_path(hasher: &mut blake3::Hasher, path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    hasher.update(path.as_os_str().as_bytes());
}

#[cfg(windows)]
fn update_hasher_with_path(hasher: &mut blake3::Hasher, path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    for unit in path.as_os_str().encode_wide() {
        hasher.update(&unit.to_le_bytes());
    }
}

#[cfg(not(any(unix, windows)))]
fn update_hasher_with_path(hasher: &mut blake3::Hasher, path: &Path) {
    hasher.update(path.to_string_lossy().as_bytes());
}

pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cache path has no parent directory",
        ));
    };
    fs::create_dir_all(parent)?;
    let sequence = TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cache");
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence,
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);

        match fs::rename(&temporary, path) {
            Ok(()) => Ok(()),
            Err(_) if path.exists() => {
                fs::remove_file(path)?;
                fs::rename(&temporary, path)
            }
            Err(error) => Err(error),
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;
    use urmare_python::{LocatedImport, SourceLocation, StaticImport};

    use super::{
        CacheConfiguration, CacheDocument, CacheLocation, FileState, ImportCache, read_document,
    };

    const STATE: FileState = FileState {
        size: 12,
        modified_ns: Some(42),
    };

    fn configuration<'a>(source_roots: &'a [std::path::PathBuf]) -> CacheConfiguration<'a> {
        CacheConfiguration {
            source_roots,
            test_roots: &[],
            excludes: &[],
        }
    }

    #[test]
    fn persists_metadata_and_content_hits() {
        let directory = tempdir().expect("temporary cache");
        let location = CacheLocation::Directory(directory.path().to_path_buf());
        let path = Path::new("src/package/module.py");
        let imports = vec![LocatedImport {
            import: StaticImport::Import {
                module: "dependency".into(),
            },
            location: SourceLocation { line: 1, column: 8 },
        }];

        let source_roots = ["src".into()];
        let mut cache = ImportCache::load(
            location.clone(),
            Path::new("/repository"),
            configuration(&source_roots),
        );
        cache.record_parsed(path, STATE, "hash".into(), &imports);
        cache.persist().expect("cache persistence");

        let mut cache = ImportCache::load(
            location.clone(),
            Path::new("/repository"),
            configuration(&source_roots),
        );
        assert_eq!(cache.metadata_hit(path, STATE), Some(imports.clone()));
        assert_eq!(cache.stats().metadata_hits, 1);

        let changed_metadata = FileState {
            size: STATE.size,
            modified_ns: Some(43),
        };
        let mut cache = ImportCache::load(
            location,
            Path::new("/repository"),
            configuration(&source_roots),
        );
        assert_eq!(
            cache.content_hit(path, changed_metadata, "hash"),
            Some(imports)
        );
        assert_eq!(cache.stats().content_hits, 1);
    }

    #[test]
    fn invalidates_configuration_and_recovers_from_corruption() {
        let directory = tempdir().expect("temporary cache");
        let location = CacheLocation::Directory(directory.path().to_path_buf());
        let path = Path::new("src/module.py");
        let source_roots = ["src".into()];
        let mut cache = ImportCache::load(
            location.clone(),
            Path::new("/repository"),
            configuration(&source_roots),
        );
        cache.record_parsed(path, STATE, "hash".into(), &[]);
        cache.persist().expect("cache persistence");

        let changed_roots = ["lib".into()];
        let mut changed = ImportCache::load(
            location.clone(),
            Path::new("/repository"),
            configuration(&changed_roots),
        );
        assert_eq!(changed.metadata_hit(path, STATE), None);

        let cache_path = directory.path().join("imports-v2.json");
        let mut document: CacheDocument = read_document(&cache_path).expect("cache document");
        document.parser = "different-parser-version".into();
        std::fs::write(
            &cache_path,
            serde_json::to_vec(&document).expect("cache serialization"),
        )
        .expect("change parser version");
        let mut parser_changed = ImportCache::load(
            location.clone(),
            Path::new("/repository"),
            configuration(&source_roots),
        );
        assert_eq!(parser_changed.metadata_hit(path, STATE), None);

        std::fs::write(&cache_path, "not JSON").expect("corrupt cache");
        let mut recovered = ImportCache::load(
            location,
            Path::new("/repository"),
            configuration(&source_roots),
        );
        assert_eq!(recovered.metadata_hit(path, STATE), None);
    }

    #[test]
    fn test_roots_and_excludes_are_part_of_the_cache_identity() {
        let source_roots = ["src".into()];
        let baseline = CacheConfiguration {
            source_roots: &source_roots,
            test_roots: &["tests".into()],
            excludes: &["generated/**".to_owned()],
        };
        let changed_tests = CacheConfiguration {
            source_roots: &source_roots,
            test_roots: &["verification".into()],
            excludes: &["generated/**".to_owned()],
        };
        let changed_excludes = CacheConfiguration {
            source_roots: &source_roots,
            test_roots: &["tests".into()],
            excludes: &["vendor/**".to_owned()],
        };

        assert_ne!(
            super::configuration_fingerprint(baseline),
            super::configuration_fingerprint(changed_tests)
        );
        assert_ne!(
            super::configuration_fingerprint(baseline),
            super::configuration_fingerprint(changed_excludes)
        );
    }
}
