//! Urmare's presentation-independent impact-analysis domain.

mod cache;
mod config;
mod error;
mod git;
mod graph_cache;
mod model;
mod repository;

pub use cache::CacheStats;
pub use config::{ConfigError, RepositoryConfig};
pub use error::AnalysisError;
pub use git::{GitChangeSet, GitDiffAnalysis, GitError, discover_git_repository_root};
pub use graph_cache::GraphCacheStats;
pub use model::{
    DependencyEdge, DependencyPath, DependencyStep, FullValidation, FullValidationReason,
    GitChange, GitChangeKind, GraphInspection, GraphSummary, ImpactAttribution, ImpactResult,
    ImportProvenance, ImportResolutionStatus, ImportResolutionTrace, RepositoryModule,
    ResolvedLocalModule, UnresolvedImport,
};
pub use repository::{AnalysisTimings, RepositoryAnalysis};
pub use urmare_python::{SourceLocation, StaticImport};

use std::path::Path;

/// Formats a canonical repository-relative path with `/` separators.
///
/// Filesystem access continues to use native [`std::path::Path`] values; this
/// conversion is only for stable human and future machine-readable output.
pub fn display_repository_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
