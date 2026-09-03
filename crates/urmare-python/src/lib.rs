//! Python-specific discovery, parsing, and module resolution for Urmare.
//!
//! This crate deliberately returns structured imports before attempting to
//! connect them to repository-local modules. That boundary keeps Python module
//! semantics out of the generic graph crate and gives future source-root rules
//! one place to evolve.

mod discovery;
mod imports;
mod modules;

pub use discovery::{
    DEFAULT_IGNORED_DIRECTORY_NAMES, DiscoveryError, DiscoveryStats, ExcludePatternError,
    PathExcluder, discover_python_files, discover_python_files_profiled,
    discover_python_files_with_excluder, is_discoverable_python_path,
};
pub use imports::{
    IMPORT_ANALYSIS_CACHE_TAG, ImportParseError, LocatedImport, SourceLocation, StaticImport,
    parse_imports, parse_imports_with_locations,
};
pub use modules::{
    ImportResolutionFailure, LocalImportResolution, LocalImportResolver, ModuleResolutionError,
    ModuleResolver, PythonFile, is_test_file, resolve_local_import_with,
};
