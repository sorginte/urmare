use std::path::PathBuf;

use urmare_python::{SourceLocation, StaticImport};

use crate::display_repository_path;

/// High-level repository graph statistics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphSummary {
    pub python_files: usize,
    pub modules: usize,
    pub import_edges: usize,
    pub tests: usize,
    pub unresolved_imports: usize,
}

/// A static import that did not resolve to any repository-local module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnresolvedImport {
    /// Canonical repository-relative path containing the import.
    pub importer: PathBuf,
    /// One-indexed location of the imported target.
    pub location: SourceLocation,
    /// Structured static import target.
    pub import: StaticImport,
}

/// The source evidence that created one repository-local import edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportProvenance {
    /// One-indexed location of the imported target in the dependent file.
    pub location: SourceLocation,
    /// Structured static import responsible for the edge.
    pub import: StaticImport,
}

/// One resolved local module produced by a static import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedLocalModule {
    pub module: String,
    pub path: PathBuf,
}

/// Deterministic outcome of one local import-resolution attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportResolutionStatus {
    Resolved,
    Unresolved,
    InvalidRelativeImport,
}

/// An inspectable record of how one static import was resolved locally.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportResolutionTrace {
    /// Canonical repository-relative path containing the import.
    pub importer: PathBuf,
    pub location: SourceLocation,
    pub import: StaticImport,
    /// Dotted module names considered by the resolver.
    pub candidate_modules: Vec<String>,
    /// Repository-local modules matched by those candidates.
    pub resolved_modules: Vec<ResolvedLocalModule>,
    pub status: ImportResolutionStatus,
}

/// One file-level dependency edge and all static imports that created it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyEdge {
    /// Canonical repository-relative dependent containing the import.
    pub dependent: PathBuf,
    /// Canonical repository-relative dependency loaded by the import.
    pub dependency: PathBuf,
    /// Every import occurrence that produced this unique graph edge.
    pub imports: Vec<ImportProvenance>,
}

/// One Python module exposed by a repository path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryModule {
    pub path: PathBuf,
    pub module: String,
    pub is_package: bool,
    pub is_test: bool,
    pub dependencies: Vec<PathBuf>,
    pub dependents: Vec<PathBuf>,
}

/// Presentation-independent details for debugging a repository import graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphInspection {
    /// Optional canonical path used to scope the inspection.
    pub focus: Option<PathBuf>,
    /// Repository-relative module roots used during indexing.
    pub source_roots: Vec<PathBuf>,
    /// All modules, or the focused module when `focus` is present.
    pub modules: Vec<RepositoryModule>,
    /// All resolved edges, or edges incident to the focused module.
    pub edges: Vec<DependencyEdge>,
    /// All import attempts, or attempts originating in the focused module.
    pub resolution_traces: Vec<ImportResolutionTrace>,
}

/// One hop in a dependency explanation, from dependent to dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyStep {
    pub dependent: PathBuf,
    pub dependency: PathBuf,
    pub imports: Vec<ImportProvenance>,
}

/// One explainable dependency path from an affected file to its changed dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyPath {
    /// Canonical repository-relative changed dependency.
    pub changed: PathBuf,
    /// Canonical repository-relative affected dependent.
    pub affected: PathBuf,
    /// Ordered path from `affected` toward `changed`, including both endpoints.
    pub path: Vec<PathBuf>,
    /// Ordered import evidence for every adjacent pair in `path`.
    pub steps: Vec<DependencyStep>,
}

/// The deterministic file-level blast radius of one or more changed files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImpactResult {
    /// Canonical changed paths. Renames include both old and new identities.
    pub changed: Vec<PathBuf>,
    /// Immediate reverse neighbors of any changed file.
    pub directly_affected: Vec<PathBuf>,
    /// Indirect reverse closure, excluding direct dependents and changed files.
    pub transitively_affected: Vec<PathBuf>,
    /// Test files anywhere in the affected closure.
    pub affected_tests: Vec<PathBuf>,
    /// Changed-file attribution for each affected result.
    pub attributions: Vec<ImpactAttribution>,
    /// Conservative validation state when selective graph impact is unsafe.
    pub full_validation: Option<FullValidation>,
}

impl ImpactResult {
    /// Returns the changed files whose reverse closures contain `affected`.
    pub fn causes_for(&self, affected: &std::path::Path) -> &[PathBuf] {
        self.attributions
            .iter()
            .find(|attribution| attribution.affected == affected)
            .map_or(&[], |attribution| attribution.caused_by.as_slice())
    }
}

/// How a validation plan should invoke the repository's pytest environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationMode {
    /// Run only the affected pytest files selected by impact analysis.
    Selective,
    /// No pytest files are affected, so no validation step is required.
    None,
    /// Selective analysis is unsafe, so pytest must discover the complete suite.
    Full,
}

/// The executable family represented by one validation step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationStepKind {
    Pytest,
}

/// One structured validation action for an agent or CI system to execute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationStep {
    pub kind: ValidationStepKind,
    pub program: String,
    /// Repository-relative targets. An empty list requests full pytest discovery.
    pub args: Vec<PathBuf>,
}

/// A deterministic, presentation-independent validation plan derived from impact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationPlan {
    /// The one impact result from which every validation decision was derived.
    pub impact: ImpactResult,
    pub mode: ValidationMode,
    pub steps: Vec<ValidationStep>,
    /// Every affected pytest file in deterministic repository-relative order.
    pub selected_test_targets: Vec<PathBuf>,
}

impl ValidationPlan {
    /// Derives pytest validation requirements without performing another analysis.
    pub fn from_impact(impact: ImpactResult) -> Self {
        let mut selected_test_targets = impact.affected_tests.clone();
        selected_test_targets.sort_by_key(|path| display_repository_path(path));
        selected_test_targets.dedup();

        let (mode, steps) = if impact.full_validation.is_some() {
            (
                ValidationMode::Full,
                vec![ValidationStep {
                    kind: ValidationStepKind::Pytest,
                    program: "pytest".to_owned(),
                    args: Vec::new(),
                }],
            )
        } else if selected_test_targets.is_empty() {
            (ValidationMode::None, Vec::new())
        } else {
            (
                ValidationMode::Selective,
                vec![ValidationStep {
                    kind: ValidationStepKind::Pytest,
                    program: "pytest".to_owned(),
                    args: selected_test_targets.clone(),
                }],
            )
        };

        Self {
            impact,
            mode,
            steps,
            selected_test_targets,
        }
    }

    /// Conservative fallback details, when full pytest discovery is required.
    pub fn full_validation(&self) -> Option<&FullValidation> {
        self.impact.full_validation.as_ref()
    }
}

/// Why a Git-aware analysis must conservatively validate the full repository.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FullValidationReason {
    /// The repository-root `pyproject.toml` changed and may redefine analysis boundaries.
    ConfigurationChanged,
}

/// Presentation-independent details for a conservative full-validation fallback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FullValidation {
    pub reason: FullValidationReason,
    /// Repository-relative configuration identities that triggered the fallback.
    pub configuration_paths: Vec<PathBuf>,
}

/// Attribution from one affected result to one or more changed files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImpactAttribution {
    pub affected: PathBuf,
    pub caused_by: Vec<PathBuf>,
}

/// The Git status represented by a repository change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// One repository analysis input changed relative to a Git merge base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitChange {
    pub kind: GitChangeKind,
    /// Current path, or the deleted path for [`GitChangeKind::Deleted`].
    pub path: PathBuf,
    /// Previous path for a rename.
    pub previous_path: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::{
        FullValidation, FullValidationReason, ImpactResult, ValidationMode, ValidationPlan,
        ValidationStep, ValidationStepKind,
    };
    use std::path::PathBuf;

    fn impact(tests: &[&str]) -> ImpactResult {
        ImpactResult {
            changed: vec![PathBuf::from("src/pkg/core.py")],
            directly_affected: Vec::new(),
            transitively_affected: Vec::new(),
            affected_tests: tests.iter().map(PathBuf::from).collect(),
            attributions: Vec::new(),
            full_validation: None,
        }
    }

    #[test]
    fn validation_plan_selects_tests_in_deterministic_order() {
        let plan = ValidationPlan::from_impact(impact(&[
            "tests/test_z.py",
            "tests/test_a.py",
            "tests/test_z.py",
        ]));

        let targets = vec![
            PathBuf::from("tests/test_a.py"),
            PathBuf::from("tests/test_z.py"),
        ];
        assert_eq!(plan.mode, ValidationMode::Selective);
        assert_eq!(plan.selected_test_targets, targets);
        assert_eq!(
            plan.steps,
            vec![ValidationStep {
                kind: ValidationStepKind::Pytest,
                program: "pytest".to_owned(),
                args: targets,
            }]
        );
        assert!(plan.full_validation().is_none());
    }

    #[test]
    fn validation_plan_omits_steps_when_no_tests_are_affected() {
        let plan = ValidationPlan::from_impact(impact(&[]));

        assert_eq!(plan.mode, ValidationMode::None);
        assert!(plan.selected_test_targets.is_empty());
        assert!(plan.steps.is_empty());
        assert!(plan.full_validation().is_none());
    }

    #[test]
    fn validation_plan_uses_targetless_pytest_for_full_validation() {
        let mut impact = impact(&["tests/test_core.py"]);
        impact.full_validation = Some(FullValidation {
            reason: FullValidationReason::ConfigurationChanged,
            configuration_paths: vec![PathBuf::from("pyproject.toml")],
        });

        let plan = ValidationPlan::from_impact(impact);

        assert_eq!(plan.mode, ValidationMode::Full);
        assert_eq!(
            plan.selected_test_targets,
            vec![PathBuf::from("tests/test_core.py")]
        );
        assert_eq!(
            plan.steps,
            vec![ValidationStep {
                kind: ValidationStepKind::Pytest,
                program: "pytest".to_owned(),
                args: Vec::new(),
            }]
        );
        assert_eq!(
            plan.full_validation()
                .expect("full validation details")
                .reason,
            FullValidationReason::ConfigurationChanged
        );
    }
}
