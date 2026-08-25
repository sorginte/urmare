use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use thiserror::Error;
use urmare_python::PathExcluder;

use crate::{
    AnalysisError, DependencyPath, FullValidation, FullValidationReason, GitChange, GitChangeKind,
    ImpactResult, RepositoryAnalysis, RepositoryConfig, display_repository_path,
};

/// Errors produced while inspecting a Git working tree.
#[derive(Debug, Error)]
pub enum GitError {
    #[error("unable to access Git repository root `{path}`: {source}")]
    RootAccess {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("unable to run Git while {operation}: {source}")]
    Executable {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("`{root}` is not a Git repository: {details}")]
    NotRepository { root: PathBuf, details: String },

    #[error(
        "selected root `{selected}` is inside Git repository `{git_root}`; Git diff analysis currently requires the repository top level"
    )]
    RootMismatch {
        selected: PathBuf,
        git_root: PathBuf,
    },

    #[error("Git base `{base}` does not resolve to a commit: {details}")]
    InvalidBase { base: String, details: String },

    #[error("Git failed while {operation}: {details}")]
    CommandFailed {
        operation: &'static str,
        details: String,
    },

    #[error("Git returned non-UTF-8 {field} while {operation}")]
    NonUtf8Output {
        operation: &'static str,
        field: &'static str,
    },

    #[error("Git returned malformed name-status output near field {field}")]
    MalformedStatus { field: usize },

    #[error("Git returned unsafe repository path `{0}`")]
    UnsafePath(PathBuf),
}

/// Python and repository-root configuration changes relative to a Git merge base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitChangeSet {
    base: String,
    merge_base: String,
    changes: Vec<GitChange>,
    configuration_changes: Vec<GitChange>,
}

impl GitChangeSet {
    /// Discovers committed, staged, unstaged, and untracked analysis-input changes.
    pub fn discover(root: &Path, base: &str) -> Result<Self, GitError> {
        let root = root.canonicalize().map_err(|source| GitError::RootAccess {
            path: root.to_path_buf(),
            source,
        })?;
        verify_repository_root(&root)?;

        let verified_base = verify_base(&root, base)?;
        let merge_base = merge_base(&root, &verified_base)?;
        let diff = run_git(
            &root,
            &[
                OsStr::new("diff"),
                OsStr::new("--name-status"),
                OsStr::new("-z"),
                OsStr::new("--find-renames"),
                OsStr::new(&merge_base),
                OsStr::new("--"),
            ],
            "reading changed files",
        )?;
        ensure_success(diff.status.success(), &diff, "reading changed files")?;
        let mut discovered = parse_name_status(&diff.stdout)?;

        let untracked = run_git(
            &root,
            &[
                OsStr::new("ls-files"),
                OsStr::new("--others"),
                OsStr::new("--exclude-standard"),
                OsStr::new("-z"),
                OsStr::new("--"),
            ],
            "reading untracked files",
        )?;
        ensure_success(
            untracked.status.success(),
            &untracked,
            "reading untracked files",
        )?;
        discovered.extend(parse_untracked(&untracked.stdout)?);

        let (mut changes, mut configuration_changes) = partition_relevant_changes(discovered);

        changes.sort_by_key(change_sort_key);
        changes.dedup();
        configuration_changes.sort_by_key(change_sort_key);
        configuration_changes.dedup();

        Ok(Self {
            base: base.to_owned(),
            merge_base,
            changes,
            configuration_changes,
        })
    }

    /// The user-provided comparison base.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The resolved merge-base commit.
    pub fn merge_base(&self) -> &str {
        &self.merge_base
    }

    /// Python changes in deterministic path order.
    pub fn changes(&self) -> &[GitChange] {
        &self.changes
    }

    /// Repository-root configuration changes in deterministic path order.
    pub fn configuration_changes(&self) -> &[GitChange] {
        &self.configuration_changes
    }

    /// Whether repository analysis boundaries may have changed.
    pub fn requires_full_validation(&self) -> bool {
        !self.configuration_changes.is_empty()
    }

    /// Configuration identities responsible for a full-validation fallback.
    pub fn configuration_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for change in &self.configuration_changes {
            if let Some(previous) = &change.previous_path
                && is_configuration(previous)
            {
                paths.push(previous.clone());
            }
            if is_configuration(&change.path) {
                paths.push(change.path.clone());
            }
        }
        paths.sort_by_key(|path| display_repository_path(path));
        paths.dedup();
        paths
    }

    /// Every path identity that should seed reverse impact traversal.
    pub fn analysis_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for change in &self.changes {
            if let Some(previous) = &change.previous_path {
                paths.push(previous.clone());
            }
            paths.push(change.path.clone());
        }
        paths.sort_by_key(|path| display_repository_path(path));
        paths.dedup();
        paths
    }

    /// Removed identities that need virtual graph nodes during current-tree analysis.
    pub fn virtual_paths(&self) -> Vec<PathBuf> {
        let mut paths = self
            .changes
            .iter()
            .filter_map(|change| match change.kind {
                GitChangeKind::Deleted => Some(change.path.clone()),
                GitChangeKind::Renamed => change.previous_path.clone(),
                GitChangeKind::Added | GitChangeKind::Modified => None,
            })
            .collect::<Vec<_>>();
        paths.sort_by_key(|path| display_repository_path(path));
        paths.dedup();
        paths
    }

    fn excluding(mut self, excluder: &PathExcluder) -> Self {
        self.changes = self
            .changes
            .into_iter()
            .filter_map(|change| exclude_change(change, excluder))
            .collect();
        self.changes.sort_by_key(change_sort_key);
        self.changes.dedup();
        self
    }
}

/// A repository graph prepared for one Git change set.
pub struct GitDiffAnalysis {
    repository: RepositoryAnalysis,
    changes: GitChangeSet,
}

impl GitDiffAnalysis {
    /// Discovers the Git change set and builds a graph that includes removed identities.
    pub fn build(root: &Path, base: &str) -> Result<Self, AnalysisError> {
        let configuration = RepositoryConfig::load(root)?;
        let excluder = configuration.path_excluder()?;
        let discovered = GitChangeSet::discover(root, base)?;
        let changes = if discovered.requires_full_validation() {
            discovered
        } else {
            discovered.excluding(&excluder)
        };
        let repository = if changes.requires_full_validation() {
            RepositoryAnalysis::build(root)?
        } else {
            let virtual_paths = changes.virtual_paths();
            RepositoryAnalysis::build_with_virtual_files(
                root,
                virtual_paths.iter().map(PathBuf::as_path),
            )?
        };
        Ok(Self {
            repository,
            changes,
        })
    }

    /// Returns the discovered Python and repository-configuration change set.
    pub fn changes(&self) -> &GitChangeSet {
        &self.changes
    }

    /// Calculates the unioned blast radius for all discovered Python changes.
    pub fn impact(&self) -> Result<ImpactResult, AnalysisError> {
        if self.changes.requires_full_validation() {
            return Ok(ImpactResult {
                changed: self.changes.analysis_paths(),
                directly_affected: Vec::new(),
                transitively_affected: Vec::new(),
                affected_tests: self.repository.current_tests()?,
                attributions: Vec::new(),
                full_validation: Some(FullValidation {
                    reason: FullValidationReason::ConfigurationChanged,
                    configuration_paths: self.changes.configuration_paths(),
                }),
            });
        }
        self.repository
            .impact_repository_paths(&self.changes.analysis_paths())
    }

    /// Returns current pytest-style files affected by the Git change set.
    pub fn affected_tests(&self) -> Result<Vec<PathBuf>, AnalysisError> {
        Ok(self.impact()?.affected_tests)
    }

    /// Explains a selected Git change using the graph prepared for this change set.
    pub fn why(&self, changed: &Path, affected: &Path) -> Result<DependencyPath, AnalysisError> {
        if self.changes.requires_full_validation() {
            return Err(AnalysisError::ConfigurationChanged);
        }
        let changed = self.repository.normalize_repository_path(changed)?;
        if !self.changes.analysis_paths().contains(&changed) {
            return Err(AnalysisError::GitChangedPathNotSelected {
                path: changed,
                base: self.changes.base().to_owned(),
            });
        }
        self.repository.why_repository_path(&changed, affected)
    }
}

/// Finds the top level of the Git repository containing `start`.
///
/// The returned path is canonical and suitable for repository analysis. This
/// keeps Git-aware callers independent of their current working directory.
pub fn discover_git_repository_root(start: &Path) -> Result<PathBuf, GitError> {
    let start = start
        .canonicalize()
        .map_err(|source| GitError::RootAccess {
            path: start.to_path_buf(),
            source,
        })?;
    let output = run_git(
        &start,
        &[OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
        "finding the repository root",
    )?;
    if !output.status.success() {
        return Err(GitError::NotRepository {
            root: start,
            details: stderr_details(&output),
        });
    }

    let git_root = output_text(&output.stdout, "finding the repository root", "root path")?;
    let git_root = PathBuf::from(git_root.trim());
    git_root
        .canonicalize()
        .map_err(|source| GitError::RootAccess {
            path: git_root,
            source,
        })
}

fn exclude_change(change: GitChange, excluder: &PathExcluder) -> Option<GitChange> {
    if change.kind != GitChangeKind::Renamed {
        return (!excluder.is_excluded(&change.path)).then_some(change);
    }

    let previous = change.previous_path.as_ref()?;
    match (
        excluder.is_excluded(previous),
        excluder.is_excluded(&change.path),
    ) {
        (false, false) => Some(change),
        (true, false) => Some(GitChange {
            kind: GitChangeKind::Added,
            path: change.path,
            previous_path: None,
        }),
        (false, true) => Some(GitChange {
            kind: GitChangeKind::Deleted,
            path: previous.clone(),
            previous_path: None,
        }),
        (true, true) => None,
    }
}

fn verify_repository_root(root: &Path) -> Result<(), GitError> {
    let git_root = discover_git_repository_root(root)?;
    if git_root != root {
        return Err(GitError::RootMismatch {
            selected: root.to_path_buf(),
            git_root,
        });
    }
    Ok(())
}

fn verify_base(root: &Path, base: &str) -> Result<String, GitError> {
    let revision = format!("{base}^{{commit}}");
    let output = run_git(
        root,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new("--end-of-options"),
            OsStr::new(&revision),
        ],
        "resolving the Git base",
    )?;
    if !output.status.success() {
        return Err(GitError::InvalidBase {
            base: base.to_owned(),
            details: stderr_details(&output),
        });
    }
    Ok(
        output_text(&output.stdout, "resolving the Git base", "commit ID")?
            .trim()
            .to_owned(),
    )
}

fn merge_base(root: &Path, verified_base: &str) -> Result<String, GitError> {
    let output = run_git(
        root,
        &[
            OsStr::new("merge-base"),
            OsStr::new(verified_base),
            OsStr::new("HEAD"),
        ],
        "finding the merge base",
    )?;
    ensure_success(output.status.success(), &output, "finding the merge base")?;
    Ok(
        output_text(&output.stdout, "finding the merge base", "commit ID")?
            .trim()
            .to_owned(),
    )
}

fn run_git(root: &Path, args: &[&OsStr], operation: &'static str) -> Result<Output, GitError> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|source| GitError::Executable { operation, source })
}

fn ensure_success(success: bool, output: &Output, operation: &'static str) -> Result<(), GitError> {
    if success {
        Ok(())
    } else {
        Err(GitError::CommandFailed {
            operation,
            details: stderr_details(output),
        })
    }
}

fn output_text<'a>(
    output: &'a [u8],
    operation: &'static str,
    field: &'static str,
) -> Result<&'a str, GitError> {
    std::str::from_utf8(output).map_err(|_| GitError::NonUtf8Output { operation, field })
}

fn stderr_details(output: &Output) -> String {
    let details = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if details.is_empty() {
        format!("Git exited with status {}", output.status)
    } else {
        details
    }
}

fn parse_name_status(output: &[u8]) -> Result<Vec<GitChange>, GitError> {
    let mut fields = output.split(|byte| *byte == 0).enumerate();
    let mut changes = Vec::new();

    while let Some((field, status)) = fields.next() {
        if status.is_empty() {
            break;
        }
        let status = std::str::from_utf8(status).map_err(|_| GitError::NonUtf8Output {
            operation: "reading changed files",
            field: "status",
        })?;
        let (_, first) = fields.next().ok_or(GitError::MalformedStatus { field })?;
        let first = repository_path(first, "reading changed files")?;

        match status.as_bytes().first().copied() {
            Some(b'R') => {
                let (_, second) = fields.next().ok_or(GitError::MalformedStatus { field })?;
                let second = repository_path(second, "reading changed files")?;
                changes.push(GitChange {
                    kind: GitChangeKind::Renamed,
                    path: second,
                    previous_path: Some(first),
                });
            }
            Some(b'C') => {
                let (_, second) = fields.next().ok_or(GitError::MalformedStatus { field })?;
                let second = repository_path(second, "reading changed files")?;
                changes.push(GitChange {
                    kind: GitChangeKind::Added,
                    path: second,
                    previous_path: None,
                });
            }
            Some(b'A') => changes.push(GitChange {
                kind: GitChangeKind::Added,
                path: first,
                previous_path: None,
            }),
            Some(b'D') => changes.push(GitChange {
                kind: GitChangeKind::Deleted,
                path: first,
                previous_path: None,
            }),
            Some(b'M' | b'T' | b'U' | b'X' | b'B') => {
                changes.push(GitChange {
                    kind: GitChangeKind::Modified,
                    path: first,
                    previous_path: None,
                });
            }
            _ => return Err(GitError::MalformedStatus { field }),
        }
    }

    Ok(changes)
}

fn parse_untracked(output: &[u8]) -> Result<Vec<GitChange>, GitError> {
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| repository_path(path, "reading untracked files"))
        .map(|result| {
            result.map(|path| GitChange {
                kind: GitChangeKind::Added,
                path,
                previous_path: None,
            })
        })
        .collect()
}

fn partition_relevant_changes(discovered: Vec<GitChange>) -> (Vec<GitChange>, Vec<GitChange>) {
    let mut python = Vec::new();
    let mut configuration = Vec::new();
    for change in discovered {
        if change.kind == GitChangeKind::Renamed {
            let Some(previous) = change.previous_path.clone() else {
                let kind = input_kind(&change.path);
                push_classified(&mut python, &mut configuration, kind, change);
                continue;
            };
            let previous_kind = input_kind(&previous);
            let current_kind = input_kind(&change.path);
            if previous_kind == current_kind {
                push_classified(&mut python, &mut configuration, current_kind, change);
            } else {
                if let Some(kind) = previous_kind {
                    push_classified(
                        &mut python,
                        &mut configuration,
                        Some(kind),
                        GitChange {
                            kind: GitChangeKind::Deleted,
                            path: previous,
                            previous_path: None,
                        },
                    );
                }
                if let Some(kind) = current_kind {
                    push_classified(
                        &mut python,
                        &mut configuration,
                        Some(kind),
                        GitChange {
                            kind: GitChangeKind::Added,
                            path: change.path,
                            previous_path: None,
                        },
                    );
                }
            }
        } else {
            let kind = input_kind(&change.path);
            push_classified(&mut python, &mut configuration, kind, change);
        }
    }
    (python, configuration)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputKind {
    Python,
    Configuration,
}

fn input_kind(path: &Path) -> Option<InputKind> {
    if is_python(path) {
        Some(InputKind::Python)
    } else if is_configuration(path) {
        Some(InputKind::Configuration)
    } else {
        None
    }
}

fn push_classified(
    python: &mut Vec<GitChange>,
    configuration: &mut Vec<GitChange>,
    kind: Option<InputKind>,
    change: GitChange,
) {
    match kind {
        Some(InputKind::Python) => python.push(change),
        Some(InputKind::Configuration) => configuration.push(change),
        None => {}
    }
}

fn repository_path(bytes: &[u8], operation: &'static str) -> Result<PathBuf, GitError> {
    let text = std::str::from_utf8(bytes).map_err(|_| GitError::NonUtf8Output {
        operation,
        field: "path",
    })?;
    let path = PathBuf::from(text);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(GitError::UnsafePath(path));
    }
    Ok(path)
}

fn is_python(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("py"))
}

fn is_configuration(path: &Path) -> bool {
    path == Path::new("pyproject.toml")
}

fn change_sort_key(change: &GitChange) -> (String, String) {
    (
        change
            .previous_path
            .as_deref()
            .map_or_else(String::new, display_repository_path),
        display_repository_path(&change.path),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::{TempDir, tempdir};

    use super::{
        GitChange, GitChangeKind, GitChangeSet, GitDiffAnalysis, GitError,
        discover_git_repository_root,
    };
    use crate::FullValidationReason;
    use urmare_python::PathExcluder;

    #[test]
    fn discovers_staged_unstaged_untracked_deleted_and_renamed_python_files() {
        let repository = initialized_repository(&[
            ("src/pkg/modified.py", "VALUE = 1\n"),
            ("src/pkg/staged.py", "STAGED = 1\n"),
            ("src/pkg/old.py", "OLD = 1\n"),
            ("src/pkg/deleted.py", "DELETED = 1\n"),
            ("notes.txt", "tracked\n"),
            (".gitignore", "ignored.py\n"),
        ]);

        fs::write(repository.path().join("src/pkg/modified.py"), "VALUE = 2\n")
            .expect("modify fixture");
        fs::write(repository.path().join("src/pkg/staged.py"), "STAGED = 2\n")
            .expect("stage fixture");
        git(repository.path(), &["add", "src/pkg/staged.py"]);
        git(
            repository.path(),
            &["mv", "src/pkg/old.py", "src/pkg/renamed.py"],
        );
        fs::remove_file(repository.path().join("src/pkg/deleted.py")).expect("delete fixture");
        fs::write(repository.path().join("src/pkg/new.py"), "NEW = 1\n")
            .expect("untracked fixture");
        fs::write(repository.path().join("ignored.py"), "VALUE = 1\n").expect("ignored fixture");
        fs::write(
            repository.path().join("untracked.txt"),
            "ignored by Urmare\n",
        )
        .expect("non-Python fixture");

        let changes = GitChangeSet::discover(repository.path(), "HEAD").expect("Git changes");

        assert!(changes.changes().contains(&GitChange {
            kind: GitChangeKind::Modified,
            path: "src/pkg/modified.py".into(),
            previous_path: None,
        }));
        assert!(changes.changes().contains(&GitChange {
            kind: GitChangeKind::Modified,
            path: "src/pkg/staged.py".into(),
            previous_path: None,
        }));
        assert!(changes.changes().contains(&GitChange {
            kind: GitChangeKind::Added,
            path: "src/pkg/new.py".into(),
            previous_path: None,
        }));
        assert!(changes.changes().contains(&GitChange {
            kind: GitChangeKind::Deleted,
            path: "src/pkg/deleted.py".into(),
            previous_path: None,
        }));
        assert!(changes.changes().contains(&GitChange {
            kind: GitChangeKind::Renamed,
            path: "src/pkg/renamed.py".into(),
            previous_path: Some("src/pkg/old.py".into()),
        }));
        assert_eq!(changes.changes().len(), 5);
        assert_eq!(
            changes.virtual_paths(),
            vec![
                std::path::PathBuf::from("src/pkg/deleted.py"),
                std::path::PathBuf::from("src/pkg/old.py"),
            ]
        );
        assert!(!changes.analysis_paths().contains(&"ignored.py".into()));
    }

    #[test]
    fn deleted_module_keeps_surviving_dependents_and_tests_connected() {
        let repository = initialized_repository(&[
            ("src/pkg/__init__.py", ""),
            ("src/pkg/removed.py", "VALUE = 1\n"),
            ("src/pkg/service.py", "from . import removed\n"),
            ("tests/test_service.py", "from pkg import service\n"),
        ]);
        fs::remove_file(repository.path().join("src/pkg/removed.py")).expect("delete module");

        let analysis = GitDiffAnalysis::build(repository.path(), "HEAD").expect("Git analysis");
        let impact = analysis.impact().expect("impact");

        assert_eq!(
            impact.changed,
            vec![std::path::PathBuf::from("src/pkg/removed.py")]
        );
        assert_eq!(
            impact.directly_affected,
            vec![std::path::PathBuf::from("src/pkg/service.py")]
        );
        assert_eq!(
            impact.transitively_affected,
            vec![std::path::PathBuf::from("tests/test_service.py")]
        );
        assert_eq!(
            impact.affected_tests,
            vec![std::path::PathBuf::from("tests/test_service.py")]
        );
        assert_eq!(
            impact.causes_for(Path::new("tests/test_service.py")),
            [std::path::PathBuf::from("src/pkg/removed.py")]
        );

        let explanation = analysis
            .why(
                Path::new("src/pkg/removed.py"),
                Path::new("tests/test_service.py"),
            )
            .expect("deleted change remains explainable");
        assert_eq!(
            explanation.path,
            [
                std::path::PathBuf::from("tests/test_service.py"),
                std::path::PathBuf::from("src/pkg/service.py"),
                std::path::PathBuf::from("src/pkg/removed.py"),
            ]
        );
        assert_eq!(explanation.steps[1].imports[0].location.line, 1);
    }

    #[test]
    fn renamed_module_is_explainable_through_its_previous_identity() {
        let repository = initialized_repository(&[
            ("src/pkg/__init__.py", ""),
            ("src/pkg/old.py", "VALUE = 1\n"),
            ("src/pkg/service.py", "from . import old\n"),
            ("tests/test_service.py", "from pkg import service\n"),
        ]);
        git(
            repository.path(),
            &["mv", "src/pkg/old.py", "src/pkg/renamed.py"],
        );

        let analysis = GitDiffAnalysis::build(repository.path(), "HEAD").expect("Git analysis");
        let explanation = analysis
            .why(
                Path::new("src/pkg/old.py"),
                Path::new("tests/test_service.py"),
            )
            .expect("previous identity remains explainable");

        assert_eq!(explanation.changed, Path::new("src/pkg/old.py"));
        assert_eq!(
            explanation.path,
            [
                std::path::PathBuf::from("tests/test_service.py"),
                std::path::PathBuf::from("src/pkg/service.py"),
                std::path::PathBuf::from("src/pkg/old.py"),
            ]
        );
    }

    #[test]
    fn changed_test_is_selected_but_a_deleted_test_is_not() {
        let repository = initialized_repository(&[
            ("src/pkg/__init__.py", ""),
            ("tests/test_deleted.py", "def test_old(): pass\n"),
        ]);
        fs::remove_file(repository.path().join("tests/test_deleted.py")).expect("delete test");
        fs::write(
            repository.path().join("tests/test_added.py"),
            "def test_new(): pass\n",
        )
        .expect("add test");

        let analysis = GitDiffAnalysis::build(repository.path(), "HEAD").expect("Git analysis");
        let tests = analysis.affected_tests().expect("affected tests");

        assert_eq!(tests, vec![std::path::PathBuf::from("tests/test_added.py")]);
    }

    #[test]
    fn includes_committed_branch_changes_since_the_merge_base() {
        let repository = initialized_repository(&[("module.py", "VALUE = 1\n")]);
        git(repository.path(), &["branch", "baseline"]);
        fs::write(repository.path().join("module.py"), "VALUE = 2\n").expect("committed change");
        git(repository.path(), &["add", "module.py"]);
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
                "branch change",
            ],
        );

        let changes = GitChangeSet::discover(repository.path(), "baseline").expect("Git changes");

        assert_eq!(
            changes.changes(),
            [GitChange {
                kind: GitChangeKind::Modified,
                path: "module.py".into(),
                previous_path: None,
            }]
        );
    }

    #[test]
    fn detects_added_modified_deleted_and_renamed_root_configuration() {
        let modified = initialized_repository(&[
            ("pyproject.toml", "[tool.urmare]\n"),
            ("module.py", "VALUE = 1\n"),
        ]);
        fs::write(
            modified.path().join("pyproject.toml"),
            "[tool.urmare]\nexclude = [\"generated/**\"]\n",
        )
        .expect("modify configuration");
        assert_eq!(
            GitChangeSet::discover(modified.path(), "HEAD")
                .expect("modified configuration")
                .configuration_changes(),
            [GitChange {
                kind: GitChangeKind::Modified,
                path: "pyproject.toml".into(),
                previous_path: None,
            }]
        );

        let added = initialized_repository(&[("module.py", "VALUE = 1\n")]);
        fs::write(added.path().join("pyproject.toml"), "[tool.urmare]\n")
            .expect("add configuration");
        assert_eq!(
            GitChangeSet::discover(added.path(), "HEAD")
                .expect("added configuration")
                .configuration_changes(),
            [GitChange {
                kind: GitChangeKind::Added,
                path: "pyproject.toml".into(),
                previous_path: None,
            }]
        );

        let deleted = initialized_repository(&[
            ("pyproject.toml", "[tool.urmare]\n"),
            ("module.py", "VALUE = 1\n"),
        ]);
        fs::remove_file(deleted.path().join("pyproject.toml")).expect("delete configuration");
        assert_eq!(
            GitChangeSet::discover(deleted.path(), "HEAD")
                .expect("deleted configuration")
                .configuration_changes(),
            [GitChange {
                kind: GitChangeKind::Deleted,
                path: "pyproject.toml".into(),
                previous_path: None,
            }]
        );

        let renamed = initialized_repository(&[
            ("pyproject.toml", "[tool.urmare]\n"),
            ("module.py", "VALUE = 1\n"),
        ]);
        git(renamed.path(), &["mv", "pyproject.toml", "project.toml"]);
        assert_eq!(
            GitChangeSet::discover(renamed.path(), "HEAD")
                .expect("renamed configuration")
                .configuration_changes(),
            [GitChange {
                kind: GitChangeKind::Deleted,
                path: "pyproject.toml".into(),
                previous_path: None,
            }]
        );
    }

    #[test]
    fn configuration_change_returns_domain_level_full_validation_and_all_current_tests() {
        let repository = initialized_repository(&[
            (
                "pyproject.toml",
                concat!(
                    "[tool.urmare]\n",
                    "source-roots = [\"src\"]\n",
                    "test-roots = [\"tests\", \"verification\"]\n",
                    "exclude = [\"tests/excluded/**\"]\n",
                ),
            ),
            ("src/pkg/core.py", "VALUE = 1\n"),
            ("tests/test_core.py", "from pkg import core\n"),
            ("tests/excluded/test_hidden.py", "from pkg import core\n"),
            ("verification/check_core.py", "from pkg import core\n"),
        ]);
        let configuration = repository.path().join("pyproject.toml");
        let mut contents = fs::read_to_string(&configuration).expect("read configuration");
        contents.push_str("# changed\n");
        fs::write(configuration, contents).expect("modify configuration");

        let impact = GitDiffAnalysis::build(repository.path(), "HEAD")
            .expect("Git analysis")
            .impact()
            .expect("full-validation impact");

        assert!(impact.changed.is_empty());
        assert!(impact.directly_affected.is_empty());
        assert!(impact.transitively_affected.is_empty());
        assert!(impact.attributions.is_empty());
        assert_eq!(
            impact.affected_tests,
            [
                std::path::PathBuf::from("tests/test_core.py"),
                std::path::PathBuf::from("verification/check_core.py"),
            ]
        );
        let validation = impact.full_validation.expect("full-validation state");
        assert_eq!(
            validation.reason,
            FullValidationReason::ConfigurationChanged
        );
        assert_eq!(
            validation.configuration_paths,
            [std::path::PathBuf::from("pyproject.toml")]
        );
    }

    #[test]
    fn exclusions_filter_changes_and_preserve_rename_boundary_crossings() {
        let changes = GitChangeSet {
            base: "HEAD".into(),
            merge_base: "abc".into(),
            changes: vec![
                GitChange {
                    kind: GitChangeKind::Modified,
                    path: "generated/client.py".into(),
                    previous_path: None,
                },
                GitChange {
                    kind: GitChangeKind::Renamed,
                    path: "src/new.py".into(),
                    previous_path: Some("generated/old.py".into()),
                },
                GitChange {
                    kind: GitChangeKind::Renamed,
                    path: "generated/moved.py".into(),
                    previous_path: Some("src/old.py".into()),
                },
            ],
            configuration_changes: Vec::new(),
        };
        let excluder = PathExcluder::new(&["generated/**".to_owned()]).expect("exclude glob");

        let filtered = changes.excluding(&excluder);

        assert_eq!(
            filtered.changes(),
            [
                GitChange {
                    kind: GitChangeKind::Added,
                    path: "src/new.py".into(),
                    previous_path: None,
                },
                GitChange {
                    kind: GitChangeKind::Deleted,
                    path: "src/old.py".into(),
                    previous_path: None,
                },
            ]
        );
    }

    #[test]
    fn git_diff_analysis_ignores_configured_paths() {
        let repository = initialized_repository(&[
            (
                "pyproject.toml",
                "[tool.urmare]\nsource-roots = [\"src\"]\nexclude = [\"src/generated/**\"]\n",
            ),
            ("src/pkg/__init__.py", ""),
            ("src/pkg/core.py", "VALUE = 1\n"),
            ("src/generated/client.py", "VALUE = 1\n"),
        ]);
        fs::write(repository.path().join("src/pkg/core.py"), "VALUE = 2\n")
            .expect("included change");
        fs::write(
            repository.path().join("src/generated/client.py"),
            "VALUE = 2\n",
        )
        .expect("excluded change");

        let analysis = GitDiffAnalysis::build(repository.path(), "HEAD").expect("Git analysis");

        assert_eq!(
            analysis.changes().changes(),
            [GitChange {
                kind: GitChangeKind::Modified,
                path: "src/pkg/core.py".into(),
                previous_path: None,
            }]
        );
        assert_eq!(
            analysis.impact().expect("impact").changed,
            [std::path::PathBuf::from("src/pkg/core.py")]
        );
    }

    #[test]
    fn reports_non_repository_and_invalid_base_errors() {
        let directory = tempdir().expect("temporary directory");
        let error = GitChangeSet::discover(directory.path(), "HEAD")
            .expect_err("plain directory is rejected");
        assert!(matches!(error, GitError::NotRepository { .. }));

        let repository = initialized_repository(&[("module.py", "VALUE = 1\n")]);
        let error = GitChangeSet::discover(repository.path(), "missing-reference")
            .expect_err("invalid base is rejected");
        assert!(matches!(error, GitError::InvalidBase { .. }));
    }

    #[test]
    fn discovers_the_repository_root_from_a_subdirectory() {
        let repository = initialized_repository(&[("src/pkg/module.py", "VALUE = 1\n")]);
        let subdirectory = repository.path().join("src/pkg");

        let discovered = discover_git_repository_root(&subdirectory).expect("Git repository root");

        assert_eq!(
            discovered,
            repository.path().canonicalize().expect("canonical root")
        );
    }

    fn initialized_repository(files: &[(&str, &str)]) -> TempDir {
        let repository = tempdir().expect("temporary Git repository");
        git(repository.path(), &["init", "--quiet"]);
        for (path, contents) in files {
            let path = repository.path().join(path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("fixture parent");
            }
            fs::write(path, contents).expect("fixture file");
        }
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

    fn git(root: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(arguments)
            .output()
            .expect("Git is available for tests");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
