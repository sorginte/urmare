use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use thiserror::Error;
use walkdir::{DirEntry, WalkDir};

const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    ".venv",
    "venv",
    ".tox",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
];

/// Work performed while validating a complete repository inventory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryStats {
    pub directories_inspected: usize,
    pub entries_inspected: usize,
}

/// Errors encountered while discovering repository Python files.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("repository root `{path}` does not exist or cannot be read: {source}")]
    RootAccess {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("repository root `{0}` is not a directory")]
    RootNotDirectory(PathBuf),

    #[error("failed while walking repository `{root}`: {source}")]
    Walk {
        root: PathBuf,
        #[source]
        source: walkdir::Error,
    },

    #[error("discovered path `{path}` is unexpectedly outside repository `{root}`")]
    OutsideRoot { path: PathBuf, root: PathBuf },
}

/// A compiled set of repository-relative, `/`-separated exclusion patterns.
///
/// Matching normalized strings instead of native path strings gives one
/// configuration file the same meaning on Unix and Windows. A matching
/// directory excludes its entire subtree.
#[derive(Clone, Debug)]
pub struct PathExcluder {
    patterns: GlobSet,
}

impl PathExcluder {
    /// Compiles portable glob patterns for repository discovery.
    pub fn new(patterns: &[String]) -> Result<Self, ExcludePatternError> {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            let glob = GlobBuilder::new(pattern)
                .literal_separator(true)
                .backslash_escape(false)
                .build()
                .map_err(|source| ExcludePatternError {
                    pattern: pattern.clone(),
                    source,
                })?;
            builder.add(glob);
        }
        let patterns = builder.build().map_err(|source| ExcludePatternError {
            pattern: "<pattern set>".to_owned(),
            source,
        })?;
        Ok(Self { patterns })
    }

    /// Returns whether a repository-relative path or one of its directories
    /// matches an exclusion pattern.
    pub fn is_excluded(&self, path: &Path) -> bool {
        let mut normalized = String::new();
        for component in path.components() {
            if !normalized.is_empty() {
                normalized.push('/');
            }
            normalized.push_str(&component.as_os_str().to_string_lossy());
            if self.patterns.is_match(&normalized) {
                return true;
            }
        }
        false
    }
}

impl Default for PathExcluder {
    fn default() -> Self {
        Self {
            patterns: GlobSet::empty(),
        }
    }
}

/// A configured exclusion glob could not be compiled.
#[derive(Debug, Error)]
#[error("invalid exclusion pattern `{pattern}`: {source}")]
pub struct ExcludePatternError {
    pub pattern: String,
    #[source]
    pub source: globset::Error,
}

/// Recursively discovers `.py` files and returns repository-relative paths.
///
/// Directory symlinks are not followed, avoiding cycles and keeping repository
/// identity tied to paths physically beneath the selected root.
pub fn discover_python_files(root: &Path) -> Result<Vec<PathBuf>, DiscoveryError> {
    discover_python_files_with_excluder(root, &PathExcluder::default())
}

/// Discovers `.py` files while pruning configured repository-relative paths.
pub fn discover_python_files_with_excluder(
    root: &Path,
    excluder: &PathExcluder,
) -> Result<Vec<PathBuf>, DiscoveryError> {
    discover_python_files_profiled(root, excluder).map(|(files, _)| files)
}

/// Discovers Python files and reports the amount of inventory work performed.
pub fn discover_python_files_profiled(
    root: &Path,
    excluder: &PathExcluder,
) -> Result<(Vec<PathBuf>, DiscoveryStats), DiscoveryError> {
    let metadata = root
        .metadata()
        .map_err(|source| DiscoveryError::RootAccess {
            path: root.to_path_buf(),
            source,
        })?;
    if !metadata.is_dir() {
        return Err(DiscoveryError::RootNotDirectory(root.to_path_buf()));
    }

    let root = root
        .canonicalize()
        .map_err(|source| DiscoveryError::RootAccess {
            path: root.to_path_buf(),
            source,
        })?;
    let mut files = Vec::new();
    let mut stats = DiscoveryStats::default();

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_visit(entry, &root, excluder))
    {
        let entry = entry.map_err(|source| DiscoveryError::Walk {
            root: root.clone(),
            source,
        })?;
        stats.entries_inspected += 1;
        if entry.file_type().is_dir() {
            stats.directories_inspected += 1;
        }
        if !entry.file_type().is_file() || entry.path().extension() != Some(OsStr::new("py")) {
            continue;
        }

        let relative =
            entry
                .path()
                .strip_prefix(&root)
                .map_err(|_| DiscoveryError::OutsideRoot {
                    path: entry.path().to_path_buf(),
                    root: root.clone(),
                })?;
        files.push(relative.to_path_buf());
    }

    files.sort();
    Ok((files, stats))
}

/// Returns whether a repository-relative Python path is eligible for ordinary
/// discovery, without performing filesystem access.
pub fn is_discoverable_python_path(path: &Path, excluder: &PathExcluder) -> bool {
    path.extension() == Some(OsStr::new("py"))
        && !path.components().any(|component| {
            let component = component.as_os_str();
            IGNORED_DIRECTORIES
                .iter()
                .any(|ignored| component == OsStr::new(ignored))
        })
        && !excluder.is_excluded(path)
}

fn should_visit(entry: &DirEntry, root: &Path, excluder: &PathExcluder) -> bool {
    if entry.depth() == 0 {
        return true;
    }

    if entry.file_type().is_dir()
        && IGNORED_DIRECTORIES
            .iter()
            .any(|ignored| entry.file_name() == OsStr::new(ignored))
    {
        return false;
    }

    entry
        .path()
        .strip_prefix(root)
        .is_ok_and(|relative| !excluder.is_excluded(relative))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{PathExcluder, discover_python_files, discover_python_files_with_excluder};

    #[test]
    fn discovers_python_files_and_skips_generated_environments_and_caches() {
        let root = tempdir().expect("temporary repository");
        for directory in [
            "package",
            ".git",
            ".venv",
            "venv",
            ".tox",
            "package/__pycache__",
            ".mypy_cache",
            ".pytest_cache",
            ".ruff_cache",
        ] {
            fs::create_dir_all(root.path().join(directory)).expect("fixture directory");
        }
        fs::write(root.path().join("package/app.py"), "import package.lib\n")
            .expect("Python fixture");
        fs::write(root.path().join("package/notes.txt"), "not Python\n").expect("text fixture");
        for ignored in [
            ".git/hidden.py",
            ".venv/hidden.py",
            "venv/hidden.py",
            ".tox/hidden.py",
            "package/__pycache__/hidden.py",
            ".mypy_cache/hidden.py",
            ".pytest_cache/hidden.py",
            ".ruff_cache/hidden.py",
        ] {
            fs::write(root.path().join(ignored), "pass\n").expect("ignored fixture");
        }

        let files = discover_python_files(root.path()).expect("discovery succeeds");
        assert_eq!(files, vec![std::path::PathBuf::from("package/app.py")]);
    }

    #[test]
    fn applies_portable_file_and_directory_globs_before_discovery() {
        let root = tempdir().expect("temporary repository");
        for directory in ["src/generated/nested", "vendor", "tests/snapshots", "tests"] {
            fs::create_dir_all(root.path().join(directory)).expect("fixture directory");
        }
        for path in [
            "src/app.py",
            "src/generated/client.py",
            "src/generated/nested/model.py",
            "vendor/dependency.py",
            "tests/test_app.py",
            "tests/snapshots/test_old.py",
        ] {
            fs::write(root.path().join(path), "pass\n").expect("Python fixture");
        }

        let excluder = PathExcluder::new(&[
            "src/generated/**".to_owned(),
            "vendor".to_owned(),
            "**/snapshots/*.py".to_owned(),
        ])
        .expect("portable patterns");
        let files = discover_python_files_with_excluder(root.path(), &excluder)
            .expect("discovery succeeds");

        assert_eq!(
            files,
            vec![
                std::path::PathBuf::from("src/app.py"),
                std::path::PathBuf::from("tests/test_app.py"),
            ]
        );
    }

    #[test]
    fn reports_invalid_globs() {
        let error = PathExcluder::new(&["generated/[".to_owned()])
            .expect_err("unclosed character class is invalid");
        assert_eq!(error.pattern, "generated/[");
    }
}
