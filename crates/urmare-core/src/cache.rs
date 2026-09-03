use std::path::{Path, PathBuf};

use directories::ProjectDirs;

/// Legacy parsed-import counters retained for API compatibility.
///
/// New code should use [`crate::IndexWorkStats`], which distinguishes reads,
/// hashes, parses, resolutions, graph mutations, and transactional writes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub metadata_hits: usize,
    pub content_hits: usize,
    pub misses: usize,
}

impl CacheStats {
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

pub(crate) fn content_hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

fn default_cache_directory(repository_root: &Path) -> Option<PathBuf> {
    let project = ProjectDirs::from("org", "Sorginte", "Urmare")?;
    Some(
        project
            .cache_dir()
            .join("repositories")
            .join(repository_fingerprint(repository_root)),
    )
}

pub(crate) fn repository_fingerprint(repository_root: &Path) -> String {
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn repository_identity_is_stable_and_path_specific() {
        let first = super::repository_fingerprint(Path::new("/repository/one"));
        assert_eq!(
            first,
            super::repository_fingerprint(Path::new("/repository/one"))
        );
        assert_ne!(
            first,
            super::repository_fingerprint(Path::new("/repository/two"))
        );
    }
}
