use std::fmt;
use std::path::{Path, PathBuf};

use ruff_python_ast::visitor::Visitor;
use ruff_python_ast::{Stmt, visitor};
use ruff_python_parser::parse_module;
use ruff_source_file::{LineColumn, LineIndex};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Version tag for persisted structured-import data.
///
/// Change this whenever the parser version or import-extraction semantics
/// change in a way that can alter cached results.
pub const IMPORT_ANALYSIS_CACHE_TAG: &str = "ruff-python-parser-0.0.7-static-imports-v2";

/// One statically declared Python import target.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StaticImport {
    /// `import foo.bar`.
    Import { module: String },
    /// `from ..foo import bar`.
    From {
        module: Option<String>,
        name: String,
        level: u32,
    },
}

impl fmt::Display for StaticImport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Import { module } => write!(formatter, "import {module}"),
            Self::From {
                module,
                name,
                level,
            } => {
                formatter.write_str("from ")?;
                for _ in 0..*level {
                    formatter.write_str(".")?;
                }
                if let Some(module) = module {
                    formatter.write_str(module)?;
                }
                write!(formatter, " import {name}")
            }
        }
    }
}

/// A one-indexed position in Python source code.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceLocation {
    pub line: usize,
    pub column: usize,
}

/// One structured static import and the location of its imported target.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocatedImport {
    pub import: StaticImport,
    pub location: SourceLocation,
}

/// A Python source file could not be parsed.
#[derive(Debug, Error)]
#[error("unable to parse Python source `{path}`: {message}")]
pub struct ImportParseError {
    path: PathBuf,
    message: String,
}

/// Parses a source file and extracts static imports from all statement scopes.
///
/// Ruff's parser is pure Rust and parses current Python grammar without using
/// the machine's installed Python interpreter. Version `0.0.7` supports the
/// project's Python 3.9–3.14 syntax target, including 3.14 template strings.
pub fn parse_imports(source: &str, path: &Path) -> Result<Vec<StaticImport>, ImportParseError> {
    Ok(parse_imports_with_locations(source, path)?
        .into_iter()
        .map(|located| located.import)
        .collect())
}

/// Parses a source file and preserves one-indexed locations for every target.
pub fn parse_imports_with_locations(
    source: &str,
    path: &Path,
) -> Result<Vec<LocatedImport>, ImportParseError> {
    let parsed = parse_module(source).map_err(|error| ImportParseError {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let line_index = LineIndex::from_source_text(source);
    let mut collector = ImportCollector {
        source,
        line_index: &line_index,
        imports: Vec::new(),
    };
    collector.visit_body(parsed.suite());
    Ok(collector.imports)
}

struct ImportCollector<'source> {
    source: &'source str,
    line_index: &'source LineIndex,
    imports: Vec<LocatedImport>,
}

impl ImportCollector<'_> {
    fn located(&self, import: StaticImport, location: LineColumn) -> LocatedImport {
        LocatedImport {
            import,
            location: SourceLocation {
                line: location.line.get(),
                column: location.column.get(),
            },
        }
    }
}

impl<'a> Visitor<'a> for ImportCollector<'_> {
    fn visit_stmt(&mut self, statement: &'a Stmt) {
        match statement {
            Stmt::Import(import) => {
                for alias in &import.names {
                    self.imports.push(
                        self.located(
                            StaticImport::Import {
                                module: alias.name.to_string(),
                            },
                            self.line_index
                                .line_column(alias.range.start(), self.source),
                        ),
                    );
                }
            }
            Stmt::ImportFrom(import) => {
                for alias in &import.names {
                    self.imports.push(
                        self.located(
                            StaticImport::From {
                                module: import.module.as_ref().map(ToString::to_string),
                                name: alias.name.to_string(),
                                level: import.level,
                            },
                            self.line_index
                                .line_column(alias.range.start(), self.source),
                        ),
                    );
                }
            }
            _ => {}
        }
        visitor::walk_stmt(self, statement);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{SourceLocation, StaticImport, parse_imports, parse_imports_with_locations};

    #[test]
    fn extracts_absolute_aliased_and_relative_imports() {
        let imports = parse_imports(
            r#"
import foo
import foo.bar as baz
from foo import bar
from foo.bar import baz as renamed
from . import sibling
from .child import value
from ..shared import thing
"#,
            Path::new("package/module.py"),
        )
        .expect("valid source");

        assert_eq!(
            imports,
            vec![
                StaticImport::Import {
                    module: "foo".into()
                },
                StaticImport::Import {
                    module: "foo.bar".into()
                },
                StaticImport::From {
                    module: Some("foo".into()),
                    name: "bar".into(),
                    level: 0,
                },
                StaticImport::From {
                    module: Some("foo.bar".into()),
                    name: "baz".into(),
                    level: 0,
                },
                StaticImport::From {
                    module: None,
                    name: "sibling".into(),
                    level: 1,
                },
                StaticImport::From {
                    module: Some("child".into()),
                    name: "value".into(),
                    level: 1,
                },
                StaticImport::From {
                    module: Some("shared".into()),
                    name: "thing".into(),
                    level: 2,
                },
            ]
        );
    }

    #[test]
    fn traverses_imports_nested_in_function_and_conditional_scopes() {
        let imports = parse_imports(
            "def load():\n    if True:\n        import package.lazy\n",
            Path::new("module.py"),
        )
        .expect("valid source");

        assert_eq!(
            imports,
            vec![StaticImport::Import {
                module: "package.lazy".into()
            }]
        );
    }

    #[test]
    fn preserves_one_indexed_target_locations_and_formats_statements() {
        let imports = parse_imports_with_locations(
            "import external\nfrom .child import (\n    first,\n    second,\n)\n",
            Path::new("package/module.py"),
        )
        .expect("valid source");

        assert_eq!(
            imports
                .iter()
                .map(|import| import.location)
                .collect::<Vec<_>>(),
            vec![
                SourceLocation { line: 1, column: 8 },
                SourceLocation { line: 3, column: 5 },
                SourceLocation { line: 4, column: 5 },
            ]
        );
        assert_eq!(imports[0].import.to_string(), "import external");
        assert_eq!(imports[1].import.to_string(), "from .child import first");
    }

    #[test]
    fn accepts_syntax_features_through_python_3_14() {
        let source = r#"
if (value := 1):
    match value:
        case 1:
            pass

try:
    pass
except* ValueError:
    pass

type Vector[T = int] = list[T]
message = t"value = {value}"
"#;

        assert!(parse_imports(source, Path::new("modern.py")).is_ok());
    }

    #[test]
    fn reports_the_repository_relative_path_for_invalid_syntax() {
        let error = parse_imports("def broken(:\n", Path::new("src/broken.py"))
            .expect_err("syntax is invalid");
        assert!(error.to_string().contains("src/broken.py"));
    }
}
