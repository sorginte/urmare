use std::path::PathBuf;

use urmare_python::{SourceLocation, StaticImport};

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

/// One Python file change discovered relative to a Git merge base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitChange {
    pub kind: GitChangeKind,
    /// Current path, or the deleted path for [`GitChangeKind::Deleted`].
    pub path: PathBuf,
    /// Previous path for a rename.
    pub previous_path: Option<PathBuf>,
}
