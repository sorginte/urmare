use std::path::PathBuf;

use thiserror::Error;
use urmare_graph::InvalidNodeId;
use urmare_python::{DiscoveryError, ImportParseError, ModuleResolutionError};

use crate::{ConfigError, display_repository_path, git::GitError};

/// Failures that can occur while building or querying repository analysis.
#[derive(Debug, Error)]
pub enum AnalysisError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Git(#[from] GitError),

    #[error(transparent)]
    Discovery(#[from] DiscoveryError),

    #[error(transparent)]
    ModuleResolution(#[from] ModuleResolutionError),

    #[error(transparent)]
    Parse(#[from] ImportParseError),

    #[error("no Python files were found under repository `{root}`")]
    NoPythonFiles { root: PathBuf },

    #[error("unable to canonicalize repository root `{path}`: {source}")]
    RootCanonicalization {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("unable to read Python source `{path}`: {source}")]
    SourceRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "module `{module}` maps to both `{first}` and `{second}`; configure distinct `tool.urmare.source-roots` or remove the ambiguous module"
    )]
    DuplicateModule {
        module: String,
        first: PathBuf,
        second: PathBuf,
    },

    #[error("input file `{0}` does not exist or cannot be read")]
    InputNotFound(PathBuf),

    #[error("input file `{input}` is outside repository `{root}`")]
    InputOutsideRepository { input: PathBuf, root: PathBuf },

    #[error(
        "Python file `{}` was not indexed in this repository",
        display_repository_path(.0)
    )]
    FileNotIndexed(PathBuf),

    #[error("no dependency path exists from `{target}` to changed file `{changed}`")]
    NoDependencyPath { changed: PathBuf, target: PathBuf },

    #[error("repository graph is internally inconsistent: {0}")]
    InvalidGraph(#[from] InvalidNodeId),

    #[error("repository graph has no file metadata for node ID {0}")]
    MissingNodeMetadata(usize),

    #[error("repository graph resolved unknown module `{0}`")]
    MissingModule(String),

    #[error(
        "repository graph has no import provenance for dependency edge `{dependent}` -> `{dependency}`"
    )]
    MissingEdgeProvenance {
        dependent: PathBuf,
        dependency: PathBuf,
    },

    #[error("provide one or more changed files, `--changed`, or `--git-diff <base>`")]
    MissingChangedInput,

    #[error(
        "provide exactly one change source: changed files, `--changed`, or `--git-diff <base>`"
    )]
    ConflictingChangedInput,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::AnalysisError;

    #[test]
    fn repository_relative_error_paths_use_portable_separators() {
        let path = Path::new("src").join("generated").join("client.py");
        let error = AnalysisError::FileNotIndexed(path);

        assert_eq!(
            error.to_string(),
            "Python file `src/generated/client.py` was not indexed in this repository"
        );
    }
}
