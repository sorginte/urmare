//! Shared, presentation-independent Urmare application operations.
//!
//! The CLI and protocol adapters select inputs and present results. This crate
//! owns root selection and dispatch to the one repository analysis required by
//! an impact, validation-plan, or dependency-explanation request.

use std::path::{Path, PathBuf};

use urmare_core::{
    AnalysisError, DependencyPath, GitDiffAnalysis, ImpactResult, RepositoryAnalysis,
    ValidationPlan, discover_git_repository_root,
};

/// Exactly one source of changed Python identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChangeSelection {
    /// One or more paths supplied directly by the caller.
    Files(Vec<PathBuf>),
    /// Staged, unstaged, and untracked changes relative to `HEAD`.
    Changed,
    /// Working-tree and committed changes since the merge base with `base`.
    GitDiff { base: String },
}

impl ChangeSelection {
    /// Whether selecting the repository root requires Git discovery.
    pub fn is_git_aware(&self) -> bool {
        matches!(self, Self::Changed | Self::GitDiff { .. })
    }
}

/// The repository state in which a dependency explanation is evaluated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExplanationContext {
    /// Explain the endpoints in the current repository graph.
    Current,
    /// Require the changed endpoint to be in changes relative to `HEAD`.
    Changed,
    /// Require the changed endpoint to be in changes since a Git merge base.
    GitDiff { base: String },
}

impl ExplanationContext {
    /// Whether selecting the repository root requires Git discovery.
    pub fn is_git_aware(&self) -> bool {
        !matches!(self, Self::Current)
    }
}

/// Application operations permanently bound to one canonical repository root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryApplication {
    root: PathBuf,
}

impl RepositoryApplication {
    /// Resolves and canonicalizes a repository root before binding operations.
    pub fn bind(root: &Path) -> Result<Self, AnalysisError> {
        let root = root
            .canonicalize()
            .map_err(|source| AnalysisError::RootCanonicalization {
                path: root.to_path_buf(),
                source,
            })?;
        if !root.is_dir() {
            return Err(AnalysisError::RootNotDirectory(root));
        }
        Ok(Self { root })
    }

    /// Selects an explicit root or discovers the containing Git root when the
    /// operation is Git-aware and no explicit root was supplied.
    pub fn select(root: Option<&Path>, git_aware: bool) -> Result<Self, AnalysisError> {
        let selected = root.unwrap_or_else(|| Path::new("."));
        let selected = if git_aware && root.is_none() {
            discover_git_repository_root(selected)?
        } else {
            selected.to_path_buf()
        };
        Self::bind(&selected)
    }

    /// The canonical root that forms this application's repository boundary.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Performs explicit-file impact analysis through one refreshed index.
    pub fn analyze_files(&self, files: &[PathBuf]) -> Result<ImpactResult, AnalysisError> {
        if files.is_empty() {
            return Err(AnalysisError::MissingChangedInput);
        }
        RepositoryAnalysis::build(&self.root)?.impact_many(files)
    }

    /// Performs working-tree impact analysis through one refreshed index.
    pub fn analyze_changed(&self) -> Result<ImpactResult, AnalysisError> {
        GitDiffAnalysis::build(&self.root, "HEAD")?.impact()
    }

    /// Performs merge-base impact analysis through one refreshed index.
    pub fn analyze_git_diff(&self, base: &str) -> Result<ImpactResult, AnalysisError> {
        validate_git_base(base)?;
        GitDiffAnalysis::build(&self.root, base)?.impact()
    }

    /// Dispatches exactly one selected impact analysis.
    pub fn analyze(&self, selection: &ChangeSelection) -> Result<ImpactResult, AnalysisError> {
        match selection {
            ChangeSelection::Files(files) => self.analyze_files(files),
            ChangeSelection::Changed => self.analyze_changed(),
            ChangeSelection::GitDiff { base } => self.analyze_git_diff(base),
        }
    }

    /// Builds a deterministic validation plan from one impact analysis.
    pub fn validation_plan(
        &self,
        selection: &ChangeSelection,
    ) -> Result<ValidationPlan, AnalysisError> {
        Ok(ValidationPlan::from_impact(self.analyze(selection)?))
    }

    /// Explains one dependency path in the explicitly selected repository state.
    pub fn explain_impact(
        &self,
        changed_file: &Path,
        affected_file: &Path,
        context: &ExplanationContext,
    ) -> Result<DependencyPath, AnalysisError> {
        match context {
            ExplanationContext::Current => {
                RepositoryAnalysis::build(&self.root)?.why(changed_file, affected_file)
            }
            ExplanationContext::Changed => {
                GitDiffAnalysis::build(&self.root, "HEAD")?.why(changed_file, affected_file)
            }
            ExplanationContext::GitDiff { base } => {
                validate_git_base(base)?;
                GitDiffAnalysis::build(&self.root, base)?.why(changed_file, affected_file)
            }
        }
    }
}

fn validate_git_base(base: &str) -> Result<(), AnalysisError> {
    if base.trim().is_empty() {
        Err(AnalysisError::EmptyGitBase)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tempfile::tempdir;
    use urmare_core::{AnalysisError, ValidationMode};

    use super::{ChangeSelection, ExplanationContext, RepositoryApplication};

    #[test]
    fn canonical_binding_and_shared_operations_preserve_analysis_semantics() {
        let repository = initialized_repository();
        let application =
            RepositoryApplication::bind(&repository.path().join(".")).expect("bound application");

        assert_eq!(
            application.root(),
            repository.path().canonicalize().expect("canonical root")
        );
        let plan = application
            .validation_plan(&ChangeSelection::Files(vec![PathBuf::from(
                "src/pkg/core.py",
            )]))
            .expect("validation plan");
        assert_eq!(plan.mode, ValidationMode::Selective);
        assert_eq!(
            plan.selected_test_targets,
            [PathBuf::from("tests/test_api.py")]
        );

        let explanation = application
            .explain_impact(
                Path::new("src/pkg/core.py"),
                Path::new("tests/test_api.py"),
                &ExplanationContext::Current,
            )
            .expect("dependency explanation");
        assert_eq!(
            explanation.path.last(),
            Some(&PathBuf::from("src/pkg/core.py"))
        );
    }

    #[test]
    fn explicit_root_authority_is_shared() {
        let repository = initialized_repository();
        let nested = repository.path().join("src/pkg");
        let discovered =
            RepositoryApplication::select(Some(&nested), false).expect("explicit nested root");
        assert_eq!(
            discovered.root(),
            nested.canonicalize().expect("canonical nested root")
        );
    }

    #[test]
    fn empty_files_and_git_bases_are_rejected_before_analysis() {
        let repository = initialized_repository();
        let application = RepositoryApplication::bind(repository.path()).expect("application");

        assert!(matches!(
            application.validation_plan(&ChangeSelection::Files(Vec::new())),
            Err(AnalysisError::MissingChangedInput)
        ));
        assert!(matches!(
            application.validation_plan(&ChangeSelection::GitDiff {
                base: "  ".to_owned(),
            }),
            Err(AnalysisError::EmptyGitBase)
        ));
    }

    fn initialized_repository() -> tempfile::TempDir {
        let repository = tempdir().expect("temporary repository");
        write(repository.path(), "src/pkg/__init__.py", "");
        write(repository.path(), "src/pkg/core.py", "VALUE = 1\n");
        write(repository.path(), "src/pkg/api.py", "from . import core\n");
        write(
            repository.path(),
            "tests/test_api.py",
            "from pkg import api\n",
        );
        git(repository.path(), &["init", "--quiet"]);
        git(repository.path(), &["add", "."]);
        git(
            repository.path(),
            &[
                "-c",
                "user.name=Urmare Tests",
                "-c",
                "user.email=urmare@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "base",
            ],
        );
        repository
    }

    fn write(root: &Path, path: &str, contents: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().expect("parent")).expect("fixture directory");
        fs::write(path, contents).expect("fixture file");
    }

    fn git(root: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(arguments)
            .output()
            .expect("Git available");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
