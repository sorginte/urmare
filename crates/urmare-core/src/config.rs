use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;
use urmare_python::{ExcludePatternError, ModuleResolver, PathExcluder};

const CONFIG_FILE: &str = "pyproject.toml";

/// Optional repository configuration loaded from `[tool.urmare]`.
///
/// Absence of this table preserves Urmare's zero-configuration inference.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RepositoryConfig {
    source_roots: Option<Vec<PathBuf>>,
    test_roots: Option<Vec<PathBuf>>,
    excludes: Vec<String>,
}

impl RepositoryConfig {
    /// Loads Urmare's table from the repository's `pyproject.toml`, if present.
    pub fn load(repository_root: &Path) -> Result<Self, ConfigError> {
        let path = repository_root.join(CONFIG_FILE);
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => return Err(ConfigError::Read { path, source }),
        };
        let pyproject: PyProject =
            toml::from_str(&source).map_err(|source| ConfigError::Parse { path, source })?;
        let Some(urmare) = pyproject.tool.urmare else {
            return Ok(Self::default());
        };
        let source_roots = urmare
            .source_roots
            .map(|roots| normalize_roots(roots, "source-roots"))
            .transpose()?;
        let test_roots = urmare
            .test_roots
            .map(|roots| normalize_roots(roots, "test-roots"))
            .transpose()?;
        let excludes = normalize_excludes(urmare.exclude)?;
        PathExcluder::new(&excludes)
            .map_err(|source| ConfigError::InvalidExcludePattern { source })?;
        Ok(Self {
            source_roots,
            test_roots,
            excludes,
        })
    }

    /// Returns explicitly configured, normalized source roots.
    ///
    /// `None` means Urmare should use zero-configuration inference. An explicit
    /// repository root (`"."`) is represented internally by an empty path.
    pub fn source_roots(&self) -> Option<&[PathBuf]> {
        self.source_roots.as_deref()
    }

    /// Returns configured roots whose discovered Python files are tests.
    pub fn test_roots(&self) -> Option<&[PathBuf]> {
        self.test_roots.as_deref()
    }

    /// Returns normalized, repository-relative exclusion globs.
    pub fn excludes(&self) -> &[String] {
        &self.excludes
    }

    pub(crate) fn path_excluder(&self) -> Result<PathExcluder, ConfigError> {
        PathExcluder::new(&self.excludes)
            .map_err(|source| ConfigError::InvalidExcludePattern { source })
    }

    pub(crate) fn module_resolver(
        &self,
        repository_root: &Path,
        indexed_paths: &[PathBuf],
    ) -> Result<ModuleResolver, ConfigError> {
        let resolver = if let Some(source_roots) = &self.source_roots {
            validate_roots(repository_root, indexed_paths, source_roots, "source-roots")?;
            ModuleResolver::with_source_roots(source_roots.iter().cloned())
        } else {
            ModuleResolver::infer_with_files(
                repository_root,
                indexed_paths.iter().map(PathBuf::as_path),
            )
        };

        if let Some(test_roots) = &self.test_roots {
            validate_roots(repository_root, indexed_paths, test_roots, "test-roots")?;
            Ok(resolver.with_test_roots(test_roots.iter().cloned()))
        } else {
            Ok(resolver)
        }
    }
}

/// Failures while loading or validating repository configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("unable to read Urmare configuration `{path}`: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("unable to parse Urmare configuration `{path}`: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("`tool.urmare.{field}` must contain at least one path")]
    EmptyRoots { field: &'static str },

    #[error("configured `{field}` path `{path}` must not be empty")]
    EmptyRoot { field: &'static str, path: PathBuf },

    #[error("configured `{field}` path `{path}` must be relative to the repository root")]
    AbsoluteRoot { field: &'static str, path: PathBuf },

    #[error("configured `{field}` path `{path}` must not contain `..`")]
    ParentRoot { field: &'static str, path: PathBuf },

    #[error("configured `{field}` path `{path}` is listed more than once")]
    DuplicateRoot { field: &'static str, path: PathBuf },

    #[error("configured `{field}` path `{path}` does not exist under the repository root")]
    RootNotFound { field: &'static str, path: PathBuf },

    #[error("configured `{field}` path `{path}` is not a directory")]
    RootNotDirectory { field: &'static str, path: PathBuf },

    #[error("configured exclude pattern must not be empty")]
    EmptyExcludePattern,

    #[error("configured exclude pattern `{0}` must use `/`, not `\\`, as its separator")]
    BackslashExcludePattern(String),

    #[error("configured exclude pattern `{0}` must be relative to the repository root")]
    AbsoluteExcludePattern(String),

    #[error("configured exclude pattern `{0}` must not contain `..`")]
    ParentExcludePattern(String),

    #[error("configured exclude pattern `{0}` is listed more than once")]
    DuplicateExcludePattern(String),

    #[error(transparent)]
    InvalidExcludePattern {
        #[from]
        source: ExcludePatternError,
    },
}

#[derive(Debug, Default, Deserialize)]
struct PyProject {
    #[serde(default)]
    tool: ToolTable,
}

#[derive(Debug, Default, Deserialize)]
struct ToolTable {
    urmare: Option<UrmareTable>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct UrmareTable {
    source_roots: Option<Vec<PathBuf>>,
    test_roots: Option<Vec<PathBuf>>,
    #[serde(default)]
    exclude: Vec<String>,
}

fn normalize_roots(roots: Vec<PathBuf>, field: &'static str) -> Result<Vec<PathBuf>, ConfigError> {
    if roots.is_empty() {
        return Err(ConfigError::EmptyRoots { field });
    }

    let mut normalized = Vec::with_capacity(roots.len());
    let mut seen = HashSet::with_capacity(roots.len());
    for root in roots {
        let path = normalize_root(&root, field)?;
        if !seen.insert(path.clone()) {
            return Err(ConfigError::DuplicateRoot { field, path: root });
        }
        normalized.push(path);
    }
    Ok(normalized)
}

fn normalize_root(root: &Path, field: &'static str) -> Result<PathBuf, ConfigError> {
    if root.as_os_str().is_empty() {
        return Err(ConfigError::EmptyRoot {
            field,
            path: root.to_path_buf(),
        });
    }

    let mut normalized = PathBuf::new();
    for component in root.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(ConfigError::ParentRoot {
                    field,
                    path: root.to_path_buf(),
                });
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(ConfigError::AbsoluteRoot {
                    field,
                    path: root.to_path_buf(),
                });
            }
        }
    }
    Ok(normalized)
}

fn normalize_excludes(patterns: Vec<String>) -> Result<Vec<String>, ConfigError> {
    let mut normalized = Vec::with_capacity(patterns.len());
    let mut seen = HashSet::with_capacity(patterns.len());
    for pattern in patterns {
        let candidate = normalize_exclude(&pattern)?;
        if !seen.insert(candidate.clone()) {
            return Err(ConfigError::DuplicateExcludePattern(pattern));
        }
        normalized.push(candidate);
    }
    Ok(normalized)
}

fn normalize_exclude(pattern: &str) -> Result<String, ConfigError> {
    if pattern.is_empty() {
        return Err(ConfigError::EmptyExcludePattern);
    }
    if pattern.contains('\\') {
        return Err(ConfigError::BackslashExcludePattern(pattern.to_owned()));
    }
    let bytes = pattern.as_bytes();
    if pattern.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return Err(ConfigError::AbsoluteExcludePattern(pattern.to_owned()));
    }

    let mut components = Vec::new();
    for component in pattern.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(ConfigError::ParentExcludePattern(pattern.to_owned())),
            component => components.push(component),
        }
    }
    if components.is_empty() {
        return Err(ConfigError::EmptyExcludePattern);
    }
    Ok(components.join("/"))
}

fn validate_roots(
    repository_root: &Path,
    indexed_paths: &[PathBuf],
    roots: &[PathBuf],
    field: &'static str,
) -> Result<(), ConfigError> {
    for root in roots {
        let absolute = repository_root.join(root);
        if absolute.is_dir() || indexed_paths.iter().any(|path| path.starts_with(root)) {
            continue;
        }
        if absolute.exists() {
            return Err(ConfigError::RootNotDirectory {
                field,
                path: root.clone(),
            });
        }
        return Err(ConfigError::RootNotFound {
            field,
            path: root.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::{ConfigError, RepositoryConfig};

    #[test]
    fn absent_configuration_preserves_inference() {
        let repository = tempdir().expect("temporary repository");
        let config = RepositoryConfig::load(repository.path()).expect("default configuration");
        assert_eq!(config.source_roots(), None);
        assert_eq!(config.test_roots(), None);
        assert!(config.excludes().is_empty());
    }

    #[test]
    fn loads_and_normalizes_multiple_source_roots() {
        let repository = tempdir().expect("temporary repository");
        fs::write(
            repository.path().join("pyproject.toml"),
            "[project]\nname = \"example\"\n\n[tool.urmare]\nsource-roots = [\"./src\", \"packages/api/src\"]\n",
        )
        .expect("configuration fixture");

        let config = RepositoryConfig::load(repository.path()).expect("Urmare configuration");
        assert_eq!(
            config.source_roots(),
            Some([PathBuf::from("src"), PathBuf::from("packages/api/src")].as_slice())
        );
    }

    #[test]
    fn loads_test_roots_and_normalizes_portable_excludes() {
        let config = load(
            "[tool.urmare]\ntest-roots = [\"./verification\"]\nexclude = [\"./generated/**\", \"vendor/\"]\n",
        )
        .expect("repository boundaries");

        assert_eq!(
            config.test_roots(),
            Some([PathBuf::from("verification")].as_slice())
        );
        assert_eq!(config.excludes(), ["generated/**", "vendor"]);
    }

    #[test]
    fn accepts_pyproject_without_an_urmare_table() {
        let repository = tempdir().expect("temporary repository");
        fs::write(
            repository.path().join("pyproject.toml"),
            "[tool.pytest.ini_options]\naddopts = \"-q\"\n",
        )
        .expect("configuration fixture");

        let config = RepositoryConfig::load(repository.path()).expect("default configuration");
        assert_eq!(config.source_roots(), None);
    }

    #[test]
    fn rejects_empty_escaping_and_duplicate_roots() {
        assert!(matches!(
            load_source_roots("[]"),
            Err(ConfigError::EmptyRoots {
                field: "source-roots"
            })
        ));
        assert!(matches!(
            load_source_roots("[\"../shared\"]"),
            Err(ConfigError::ParentRoot {
                field: "source-roots",
                ..
            })
        ));
        assert!(matches!(
            load_source_roots("[\"src\", \"./src\"]"),
            Err(ConfigError::DuplicateRoot {
                field: "source-roots",
                ..
            })
        ));
    }

    #[test]
    fn rejects_invalid_test_roots_and_exclude_patterns() {
        assert!(matches!(
            load("[tool.urmare]\ntest-roots = []\n"),
            Err(ConfigError::EmptyRoots {
                field: "test-roots"
            })
        ));
        assert!(matches!(
            load("[tool.urmare]\nexclude = [\"../generated\"]\n"),
            Err(ConfigError::ParentExcludePattern(_))
        ));
        assert!(matches!(
            load("[tool.urmare]\nexclude = [\"generated\\\\**\"]\n"),
            Err(ConfigError::BackslashExcludePattern(_))
        ));
        assert!(matches!(
            load("[tool.urmare]\nexclude = [\"generated/[\"]\n"),
            Err(ConfigError::InvalidExcludePattern { .. })
        ));
        assert!(matches!(
            load("[tool.urmare]\nexclude = [\"vendor\", \"./vendor/\"]\n"),
            Err(ConfigError::DuplicateExcludePattern(_))
        ));
    }

    #[test]
    fn reports_invalid_toml_and_unknown_urmare_options() {
        assert!(matches!(
            load("[tool.urmare\nsource-roots = [\"src\"]\n"),
            Err(ConfigError::Parse { .. })
        ));
        let error =
            load("[tool.urmare]\nsource-root = \"src\"\n").expect_err("unknown Urmare option");
        assert!(matches!(error, ConfigError::Parse { .. }));
        assert!(error.to_string().contains("unknown field `source-root`"));
    }

    fn load(source: &str) -> Result<RepositoryConfig, ConfigError> {
        let repository = tempdir().expect("temporary repository");
        fs::write(repository.path().join("pyproject.toml"), source).expect("configuration fixture");
        RepositoryConfig::load(repository.path())
    }

    fn load_source_roots(value: &str) -> Result<RepositoryConfig, ConfigError> {
        load(&format!("[tool.urmare]\nsource-roots = {value}\n"))
    }
}
