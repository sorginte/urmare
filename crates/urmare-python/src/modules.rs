use std::collections::{BTreeSet, HashSet};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::StaticImport;

/// A repository Python file with its importable module identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PythonFile {
    /// Canonical repository-relative path.
    pub path: PathBuf,
    /// Dotted Python module name.
    pub module: String,
    /// Whether this file is a package initializer.
    pub is_package: bool,
    /// Whether this file follows pytest naming conventions or is beneath a
    /// configured test root.
    pub is_test: bool,
}

/// A repository path could not be represented as a Python module.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ModuleResolutionError {
    #[error("Python path `{0}` must be relative to the repository root")]
    AbsolutePath(PathBuf),

    #[error("Python path `{0}` must end in `.py`")]
    NotPython(PathBuf),

    #[error("Python path `{0}` does not identify an importable module")]
    EmptyModule(PathBuf),

    #[error("Python path `{path}` contains non-Unicode module component `{component:?}`")]
    NonUnicodeComponent { path: PathBuf, component: PathBuf },
}

/// Maps canonical repository paths to Python module names.
///
/// Source roots affect import identity only; canonical file identity always
/// remains repository-relative. Files outside every source root, including
/// ordinary `tests/` directories, stay rooted at the repository root.
#[derive(Clone, Debug)]
pub struct ModuleResolver {
    source_roots: Vec<PathBuf>,
    test_roots: Vec<PathBuf>,
}

impl ModuleResolver {
    /// Infers `src/` when it exists, or uses the repository root otherwise.
    pub fn infer(repository_root: &Path) -> Self {
        Self::infer_with_files(repository_root, std::iter::empty())
    }

    /// Infers a source root while considering removed paths not on disk.
    pub fn infer_with_files<'a>(
        repository_root: &Path,
        files: impl IntoIterator<Item = &'a Path>,
    ) -> Self {
        let source_roots = if repository_root.join("src").is_dir()
            || files.into_iter().any(|path| path.starts_with("src"))
        {
            vec![PathBuf::from("src")]
        } else {
            vec![PathBuf::new()]
        };
        Self {
            source_roots,
            test_roots: Vec::new(),
        }
    }

    /// Creates a resolver with an explicit repository-relative source root.
    pub fn new(source_root: impl Into<PathBuf>) -> Self {
        Self {
            source_roots: vec![source_root.into()],
            test_roots: Vec::new(),
        }
    }

    /// Creates a resolver with explicit repository-relative source roots.
    pub fn with_source_roots(source_roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            source_roots: source_roots.into_iter().collect(),
            test_roots: Vec::new(),
        }
    }

    /// Adds repository-relative roots whose Python files are all tests.
    pub fn with_test_roots(mut self, test_roots: impl IntoIterator<Item = PathBuf>) -> Self {
        self.test_roots = test_roots.into_iter().collect();
        self
    }

    /// Returns the source roots relative to the repository.
    pub fn source_roots(&self) -> &[PathBuf] {
        &self.source_roots
    }

    /// Returns configured test roots relative to the repository.
    pub fn test_roots(&self) -> &[PathBuf] {
        &self.test_roots
    }

    /// Maps one repository-relative `.py` file to a Python module.
    pub fn module_for_path(&self, path: &Path) -> Result<PythonFile, ModuleResolutionError> {
        validate_python_path(path)?;

        // Nested configured roots are deterministic: the most specific match
        // wins. This lets a repository root coexist with a nested import root.
        let source_root = self
            .source_roots
            .iter()
            .filter(|source_root| {
                !source_root.as_os_str().is_empty() && path.starts_with(source_root)
            })
            .max_by_key(|source_root| source_root.components().count());
        let rooted_path = source_root.map_or_else(
            || path.to_path_buf(),
            |source_root| path.strip_prefix(source_root).unwrap_or(path).to_path_buf(),
        );

        let is_package = rooted_path.file_name() == Some(OsStr::new("__init__.py"));
        let mut without_extension = rooted_path.clone();
        without_extension.set_extension("");
        if is_package {
            without_extension.pop();
        }

        let mut components = Vec::new();
        for component in without_extension.components() {
            let Component::Normal(component) = component else {
                return Err(ModuleResolutionError::AbsolutePath(path.to_path_buf()));
            };
            let Some(component) = component.to_str() else {
                return Err(ModuleResolutionError::NonUnicodeComponent {
                    path: path.to_path_buf(),
                    component: PathBuf::from(component),
                });
            };
            components.push(component);
        }
        if components.is_empty() {
            return Err(ModuleResolutionError::EmptyModule(path.to_path_buf()));
        }

        python_file(
            path,
            components.join("."),
            is_package,
            self.is_test_path(path),
        )
    }

    /// Reconstructs file metadata from a previously resolved module identity.
    ///
    /// This keeps persistent-index reuse behind the module-resolution boundary
    /// instead of duplicating Python package and test conventions in callers.
    pub fn file_with_module_identity(
        &self,
        path: &Path,
        module: String,
    ) -> Result<PythonFile, ModuleResolutionError> {
        validate_python_path(path)?;
        let is_package = path.file_name() == Some(OsStr::new("__init__.py"));
        python_file(path, module, is_package, self.is_test_path(path))
    }

    fn is_test_path(&self, path: &Path) -> bool {
        is_test_file(path) || self.test_roots.iter().any(|root| path.starts_with(root))
    }
}

fn validate_python_path(path: &Path) -> Result<(), ModuleResolutionError> {
    if path.is_absolute() {
        return Err(ModuleResolutionError::AbsolutePath(path.to_path_buf()));
    }
    if path.extension() != Some(OsStr::new("py")) {
        return Err(ModuleResolutionError::NotPython(path.to_path_buf()));
    }
    Ok(())
}

fn python_file(
    path: &Path,
    module: String,
    is_package: bool,
    is_test: bool,
) -> Result<PythonFile, ModuleResolutionError> {
    if module.is_empty() {
        return Err(ModuleResolutionError::EmptyModule(path.to_path_buf()));
    }
    Ok(PythonFile {
        path: path.to_path_buf(),
        module,
        is_package,
        is_test,
    })
}

/// Resolves structured imports against repository-local module names.
#[derive(Clone, Debug)]
pub struct LocalImportResolver {
    modules: HashSet<String>,
}

/// Why a static import could not be translated into local-module candidates.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportResolutionFailure {
    /// A relative import ascends above the importer's package.
    RelativeBeyondTopLevel,
}

/// Deterministic local-module candidates and matches for one static import.
///
/// Candidates record every module name considered by the MVP resolver, while
/// `resolved_modules` contains the subset present in the repository. Keeping
/// both makes resolution decisions inspectable without coupling diagnostics to
/// CLI presentation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalImportResolution {
    pub candidate_modules: Vec<String>,
    pub resolved_modules: Vec<String>,
    pub failure: Option<ImportResolutionFailure>,
}

impl LocalImportResolver {
    pub fn new<'a>(modules: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            modules: modules.into_iter().map(str::to_owned).collect(),
        }
    }

    /// Returns every certain local module loaded by an import.
    ///
    /// Existing package prefixes are included because importing `foo.bar`
    /// executes both `foo` and `foo.bar`. For `from foo import bar`, `foo.bar`
    /// is included when it exists; otherwise `bar` is conservatively treated as
    /// a symbol exported by `foo`.
    pub fn resolve(&self, importer: &PythonFile, import: &StaticImport) -> Vec<String> {
        self.resolve_with_trace(importer, import).resolved_modules
    }

    /// Resolves one import and retains every repository-local candidate tried.
    pub fn resolve_with_trace(
        &self,
        importer: &PythonFile,
        import: &StaticImport,
    ) -> LocalImportResolution {
        resolve_local_import_with(importer, import, |module| self.modules.contains(module))
    }
}

/// Resolves one static import through a caller-provided local-module lookup.
///
/// Full repository construction and persistent incremental updates share this
/// function so candidate generation, relative-import handling, and prefix
/// semantics cannot drift between the two paths.
pub fn resolve_local_import_with(
    importer: &PythonFile,
    import: &StaticImport,
    mut is_local: impl FnMut(&str) -> bool,
) -> LocalImportResolution {
    let mut candidates = BTreeSet::new();
    let mut resolved = BTreeSet::new();

    match import {
        StaticImport::Import { module } => {
            add_prefixes(module, &mut candidates, |candidate| {
                if is_local(candidate) {
                    resolved.insert(candidate.to_owned());
                }
            });
        }
        StaticImport::From {
            module,
            name,
            level,
        } => {
            let Some(base) = absolute_from_base(importer, module.as_deref(), *level) else {
                return LocalImportResolution {
                    candidate_modules: Vec::new(),
                    resolved_modules: Vec::new(),
                    failure: Some(ImportResolutionFailure::RelativeBeyondTopLevel),
                };
            };
            add_prefixes(&base, &mut candidates, |candidate| {
                if is_local(candidate) {
                    resolved.insert(candidate.to_owned());
                }
            });

            if name != "*" {
                let candidate = if base.is_empty() {
                    name.clone()
                } else {
                    format!("{base}.{name}")
                };
                add_prefixes(&candidate, &mut candidates, |candidate| {
                    if is_local(candidate) {
                        resolved.insert(candidate.to_owned());
                    }
                });
            }
        }
    }

    LocalImportResolution {
        candidate_modules: candidates.into_iter().collect(),
        resolved_modules: resolved.into_iter().collect(),
        failure: None,
    }
}

fn add_prefixes(module: &str, candidates: &mut BTreeSet<String>, mut observe: impl FnMut(&str)) {
    let mut prefix = String::new();
    for component in module.split('.').filter(|component| !component.is_empty()) {
        if !prefix.is_empty() {
            prefix.push('.');
        }
        prefix.push_str(component);
        candidates.insert(prefix.clone());
        observe(&prefix);
    }
}

/// Returns whether a path follows pytest's test-file naming conventions.
pub fn is_test_file(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(OsStr::to_str) else {
        return false;
    };
    stem.starts_with("test_") || stem.ends_with("_test")
}

fn absolute_from_base(importer: &PythonFile, module: Option<&str>, level: u32) -> Option<String> {
    if level == 0 {
        return module.map_or_else(|| Some(String::new()), |module| Some(module.to_owned()));
    }

    let mut package: Vec<&str> = importer.module.split('.').collect();
    if !importer.is_package {
        package.pop();
    }

    let ascents = level.saturating_sub(1) as usize;
    if ascents > package.len() {
        return None;
    }
    package.truncate(package.len() - ascents);

    if let Some(module) = module {
        package.extend(module.split('.'));
    }
    Some(package.join("."))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::StaticImport;

    use super::{ImportResolutionFailure, LocalImportResolver, ModuleResolver};

    #[test]
    fn maps_src_and_test_paths_to_expected_modules() {
        let resolver = ModuleResolver::new("src");

        assert_eq!(
            resolver
                .module_for_path(Path::new("src/payments/stripe.py"))
                .expect("module")
                .module,
            "payments.stripe"
        );
        let package = resolver
            .module_for_path(Path::new("src/payments/__init__.py"))
            .expect("package");
        assert_eq!(package.module, "payments");
        assert!(package.is_package);
        assert_eq!(
            resolver
                .module_for_path(Path::new("tests/api/test_checkout.py"))
                .expect("test module")
                .module,
            "tests.api.test_checkout"
        );
    }

    #[test]
    fn maps_flat_layout_from_repository_root() {
        let resolver = ModuleResolver::new("");
        assert_eq!(
            resolver
                .module_for_path(Path::new("package/foo.py"))
                .expect("module")
                .module,
            "package.foo"
        );
    }

    #[test]
    fn maps_multiple_source_roots_and_keeps_tests_repository_relative() {
        let resolver = ModuleResolver::with_source_roots([
            PathBuf::from("packages/payments/src"),
            PathBuf::from("packages/api/src"),
        ]);

        assert_eq!(
            resolver
                .module_for_path(Path::new("packages/payments/src/payments/service.py"))
                .expect("payments module")
                .module,
            "payments.service"
        );
        assert_eq!(
            resolver
                .module_for_path(Path::new("packages/api/src/api/checkout.py"))
                .expect("API module")
                .module,
            "api.checkout"
        );
        assert_eq!(
            resolver
                .module_for_path(Path::new("tests/test_checkout.py"))
                .expect("test module")
                .module,
            "tests.test_checkout"
        );
    }

    #[test]
    fn configured_test_roots_supplement_pytest_filename_conventions() {
        let resolver = ModuleResolver::new("src").with_test_roots([PathBuf::from("verification")]);

        assert!(
            resolver
                .module_for_path(Path::new("verification/checkout_spec.py"))
                .expect("configured test")
                .is_test
        );
        assert!(
            resolver
                .module_for_path(Path::new("checks/test_checkout.py"))
                .expect("convention test")
                .is_test
        );
        assert!(
            !resolver
                .module_for_path(Path::new("src/app/service.py"))
                .expect("source module")
                .is_test
        );
    }

    #[test]
    fn chooses_the_most_specific_matching_source_root() {
        let resolver =
            ModuleResolver::with_source_roots([PathBuf::new(), PathBuf::from("packages/api/src")]);

        assert_eq!(
            resolver
                .module_for_path(Path::new("packages/api/src/api/checkout.py"))
                .expect("nested source root")
                .module,
            "api.checkout"
        );
    }

    #[test]
    fn rejects_a_repository_root_initializer_without_a_module_name() {
        let resolver = ModuleResolver::new("");
        let error = resolver
            .module_for_path(Path::new("__init__.py"))
            .expect_err("root initializer has no importable name");

        assert!(
            error
                .to_string()
                .contains("does not identify an importable module")
        );
    }

    #[test]
    fn resolves_relative_imports_from_modules_and_packages() {
        let file_resolver = ModuleResolver::new("src");
        let importer = file_resolver
            .module_for_path(Path::new("src/payments/api/checkout.py"))
            .expect("module");
        let modules = [
            "payments",
            "payments.api",
            "payments.api.checkout",
            "payments.shared",
            "payments.shared.formatting",
        ];
        let resolver = LocalImportResolver::new(modules);

        assert_eq!(
            resolver.resolve(
                &importer,
                &StaticImport::From {
                    module: Some("shared".into()),
                    name: "formatting".into(),
                    level: 2,
                }
            ),
            vec![
                "payments".to_string(),
                "payments.shared".to_string(),
                "payments.shared.formatting".to_string(),
            ]
        );
    }

    #[test]
    fn from_import_falls_back_to_the_containing_module_for_symbols() {
        let file_resolver = ModuleResolver::new("");
        let importer = file_resolver
            .module_for_path(Path::new("consumer.py"))
            .expect("module");
        let resolver = LocalImportResolver::new(["package", "package.service"]);

        assert_eq!(
            resolver.resolve(
                &importer,
                &StaticImport::From {
                    module: Some("package.service".into()),
                    name: "function".into(),
                    level: 0,
                }
            ),
            vec!["package".to_string(), "package.service".to_string()]
        );
    }

    #[test]
    fn traces_every_candidate_and_local_match() {
        let file_resolver = ModuleResolver::new("");
        let importer = file_resolver
            .module_for_path(Path::new("consumer.py"))
            .expect("module");
        let resolver = LocalImportResolver::new(["package", "package.service"]);

        let resolution = resolver.resolve_with_trace(
            &importer,
            &StaticImport::From {
                module: Some("package.service".into()),
                name: "function".into(),
                level: 0,
            },
        );

        assert_eq!(
            resolution.candidate_modules,
            ["package", "package.service", "package.service.function"]
        );
        assert_eq!(resolution.resolved_modules, ["package", "package.service"]);
        assert_eq!(resolution.failure, None);
    }

    #[test]
    fn traces_relative_imports_that_escape_the_package() {
        let importer = ModuleResolver::new("")
            .module_for_path(Path::new("package/module.py"))
            .expect("module");
        let resolver = LocalImportResolver::new(["package"]);

        let resolution = resolver.resolve_with_trace(
            &importer,
            &StaticImport::From {
                module: Some("outside".into()),
                name: "value".into(),
                level: 3,
            },
        );

        assert!(resolution.candidate_modules.is_empty());
        assert!(resolution.resolved_modules.is_empty());
        assert_eq!(
            resolution.failure,
            Some(ImportResolutionFailure::RelativeBeyondTopLevel)
        );
    }
}
