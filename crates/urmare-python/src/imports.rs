use std::fmt;
use std::path::{Path, PathBuf};

use ruff_python_ast::visitor::Visitor;
use ruff_python_ast::{Expr, Number, Stmt, visitor};
use ruff_python_parser::parse_module;
use ruff_source_file::{LineColumn, LineIndex};
use ruff_text_size::TextRange;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Version tag for persisted structured-import data.
///
/// Change this whenever the parser version or import-extraction semantics
/// change in a way that can alter cached results.
pub const IMPORT_ANALYSIS_CACHE_TAG: &str =
    "ruff-python-parser-0.0.7-static-imports-pytest-dynamic-v3";

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
    /// A statically recognized call to `importlib.import_module` or
    /// `__import__`. `module` is `None` when the target is computed and could
    /// not be resolved without executing repository code.
    Dynamic {
        api: DynamicImportApi,
        module: Option<String>,
        package: Option<String>,
        level: u32,
        fromlist: Vec<String>,
        reason: Option<String>,
    },
}

/// The standard-library import API used by a dynamic import call.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicImportApi {
    ImportModule,
    BuiltinImport,
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
            Self::Dynamic {
                api,
                module,
                package,
                level,
                fromlist,
                reason,
            } => {
                write!(formatter, "{api:?}(")?;
                if let Some(module) = module {
                    write!(formatter, "{module:?}")?;
                } else {
                    formatter.write_str("<computed>")?;
                }
                if let Some(package) = package {
                    write!(formatter, ", package={package:?}")?;
                }
                if *level > 0 {
                    write!(formatter, ", level={level}")?;
                }
                if !fromlist.is_empty() {
                    write!(formatter, ", fromlist={fromlist:?}")?;
                }
                if let Some(reason) = reason {
                    write!(formatter, ", reason={reason}")?;
                }
                formatter.write_str(")")
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

/// A repository-local export declaration extracted from a package initializer
/// or module-level import. This is intentionally narrower than Python's full
/// name-binding semantics.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExportDeclaration {
    pub name: Option<String>,
    pub module: Option<String>,
    pub imported_name: Option<String>,
    pub level: u32,
    pub wildcard: bool,
    pub location: SourceLocation,
}

/// A named pytest relationship discovered without importing or executing the
/// analyzed repository.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocatedName {
    pub name: String,
    pub location: SourceLocation,
    pub scope: Option<String>,
}

/// One statically declared pytest fixture.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FixtureDeclaration {
    pub name: String,
    pub autouse: bool,
    pub location: SourceLocation,
    pub scope: Option<String>,
    pub parameters: Vec<LocatedName>,
}

/// Parser facts used by the core pytest-aware and dynamic-import resolver.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PytestFacts {
    pub fixtures: Vec<FixtureDeclaration>,
    pub fixture_requests: Vec<LocatedName>,
    pub usefixtures: Vec<LocatedName>,
    pub pytest_plugins: Vec<LocatedName>,
    pub unsupported_plugins: Vec<SourceLocation>,
    pub unsupported_fixtures: Vec<SourceLocation>,
    pub unresolved_fixture_lookups: Vec<SourceLocation>,
    pub unsupported_exports: Vec<SourceLocation>,
    pub exports: Vec<ExportDeclaration>,
}

/// The complete syntax facts extracted from one Python source file.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParsedPython {
    pub imports: Vec<LocatedImport>,
    pub pytest: PytestFacts,
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
    Ok(parse_python(source, path)?.imports)
}

/// Parses one Python source file into all supported, presentation-independent
/// syntax facts. No project code is imported or evaluated.
pub fn parse_python(source: &str, path: &Path) -> Result<ParsedPython, ImportParseError> {
    let parsed = parse_module(source).map_err(|error| ImportParseError {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let line_index = LineIndex::from_source_text(source);
    let mut bindings = BindingCollector::default();
    bindings.visit_body(parsed.suite());
    let mut collector = ImportCollector {
        source,
        line_index: &line_index,
        imports: Vec::new(),
        pytest: PytestFacts::default(),
        importlib_names: bindings.importlib_names,
        import_module_names: bindings.import_module_names,
        pytest_names: bindings.pytest_names,
        mark_names: bindings.mark_names,
        fixture_names: bindings.fixture_names,
        shadowed_names: bindings.shadowed_names,
        scoped_shadowed: bindings.scoped_shadowed,
        class_scopes: Vec::new(),
    };
    collector.visit_body(parsed.suite());
    let mut pytest = collector.pytest;
    if path.file_name().and_then(|name| name.to_str()) != Some("__init__.py") {
        // A normal module's imports are dependencies, not package-boundary
        // exports. Keep import declarations when the module explicitly opts
        // into an export surface with a literal __all__.
        let has_literal_all = pytest
            .exports
            .iter()
            .any(|declaration| declaration.module.is_none() && declaration.imported_name.is_none());
        if !has_literal_all {
            pytest
                .exports
                .retain(|declaration| declaration.module.is_none());
        }
    }
    Ok(ParsedPython {
        imports: collector.imports,
        pytest,
    })
}

#[derive(Default)]
struct BindingCollector {
    importlib_names: std::collections::BTreeSet<String>,
    import_module_names: std::collections::BTreeSet<String>,
    pytest_names: std::collections::BTreeSet<String>,
    mark_names: std::collections::BTreeSet<String>,
    fixture_names: std::collections::BTreeSet<String>,
    shadowed_names: std::collections::BTreeSet<String>,
    scoped_shadowed: Vec<(String, TextRange)>,
    scopes: Vec<TextRange>,
}

impl<'a> Visitor<'a> for BindingCollector {
    fn visit_stmt(&mut self, statement: &'a Stmt) {
        match statement {
            Stmt::Import(import) => {
                for alias in &import.names {
                    let bound = alias.asname.as_ref().map_or_else(
                        || alias.name.split('.').next().unwrap_or_default().to_owned(),
                        ToString::to_string,
                    );
                    if alias.name.to_string() == "importlib" {
                        self.importlib_names.insert(bound.clone());
                        if self.scopes.is_empty() {
                            self.shadowed_names.remove(&bound);
                        }
                    } else if alias.name.to_string() == "pytest" {
                        self.pytest_names.insert(bound.clone());
                    } else if bound == "importlib"
                        || bound == "import_module"
                        || self.importlib_names.contains(&bound)
                        || self.import_module_names.contains(&bound)
                    {
                        self.record_shadow(bound);
                    }
                }
            }
            Stmt::ImportFrom(import) => {
                for alias in &import.names {
                    let bound = alias
                        .asname
                        .as_ref()
                        .map_or_else(|| alias.name.to_string(), ToString::to_string);
                    if import.module.as_deref() == Some("importlib")
                        && alias.name.to_string() == "import_module"
                    {
                        self.import_module_names.insert(bound.clone());
                        if self.scopes.is_empty() {
                            self.shadowed_names.remove(&bound);
                        }
                    } else if import.module.as_deref() == Some("pytest") {
                        if alias.name.to_string() == "fixture" {
                            self.fixture_names.insert(bound.clone());
                        } else if alias.name.to_string() == "mark" {
                            self.mark_names.insert(bound.clone());
                        }
                    } else if bound == "importlib"
                        || bound == "import_module"
                        || self.import_module_names.contains(&bound)
                        || self.importlib_names.contains(&bound)
                    {
                        self.record_shadow(bound);
                    }
                }
            }
            Stmt::FunctionDef(function) => {
                self.record_shadow(function.name.to_string());
                self.scopes.push(function.range);
                for parameter in function.parameters.iter() {
                    self.scoped_shadowed
                        .push((parameter.name().to_string(), function.range));
                }
                visitor::walk_stmt(self, statement);
                self.scopes.pop();
                return;
            }
            Stmt::ClassDef(class) => {
                self.record_shadow(class.name.to_string());
                self.scopes.push(class.range);
                visitor::walk_stmt(self, statement);
                self.scopes.pop();
                return;
            }
            Stmt::Assign(assign) => {
                for target in &assign.targets {
                    if let Expr::Name(name) = target {
                        self.record_shadow(name.id.to_string());
                    }
                }
            }
            Stmt::AnnAssign(assign) => {
                if let Expr::Name(name) = assign.target.as_ref() {
                    self.record_shadow(name.id.to_string());
                }
            }
            Stmt::AugAssign(assign) => {
                if let Expr::Name(name) = assign.target.as_ref()
                    && name.id == "__all__"
                {
                    self.record_shadow(name.id.to_string());
                }
            }
            _ => {}
        }
        visitor::walk_stmt(self, statement);
    }
}

impl BindingCollector {
    fn record_shadow(&mut self, name: String) {
        if let Some(scope) = self.scopes.last().copied() {
            self.scoped_shadowed.push((name, scope));
        } else {
            self.shadowed_names.insert(name);
        }
    }
}

struct ImportCollector<'source> {
    source: &'source str,
    line_index: &'source LineIndex,
    imports: Vec<LocatedImport>,
    pytest: PytestFacts,
    importlib_names: std::collections::BTreeSet<String>,
    import_module_names: std::collections::BTreeSet<String>,
    pytest_names: std::collections::BTreeSet<String>,
    mark_names: std::collections::BTreeSet<String>,
    fixture_names: std::collections::BTreeSet<String>,
    shadowed_names: std::collections::BTreeSet<String>,
    scoped_shadowed: Vec<(String, TextRange)>,
    class_scopes: Vec<String>,
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

    fn located_at(&self, import: StaticImport, location: SourceLocation) -> LocatedImport {
        LocatedImport { import, location }
    }

    fn location(&self, range: ruff_text_size::TextRange) -> SourceLocation {
        let location = self.line_index.line_column(range.start(), self.source);
        SourceLocation {
            line: location.line.get(),
            column: location.column.get(),
        }
    }

    fn decorator_name(expression: &Expr) -> Option<String> {
        match expression {
            Expr::Name(name) => Some(name.id.to_string()),
            Expr::Attribute(attribute) => {
                let prefix = Self::decorator_name(&attribute.value)?;
                Some(format!("{prefix}.{}", attribute.attr))
            }
            Expr::Call(call) => Self::decorator_name(&call.func),
            _ => None,
        }
    }

    fn literal_string(expression: &Expr) -> Option<String> {
        match expression {
            Expr::StringLiteral(string) => Some(string.value.to_str().to_owned()),
            _ => None,
        }
    }

    fn literal_strings(expression: &Expr) -> Option<Vec<String>> {
        match expression {
            Expr::List(list) => list.elts.iter().map(Self::literal_string).collect(),
            Expr::Tuple(tuple) => tuple.elts.iter().map(Self::literal_string).collect(),
            Expr::StringLiteral(_) => Self::literal_string(expression).map(|value| vec![value]),
            _ => None,
        }
    }

    fn literal_u32(expression: &Expr) -> Option<u32> {
        match expression {
            Expr::NumberLiteral(number) => match &number.value {
                Number::Int(value) => value.as_u32(),
                _ => None,
            },
            _ => None,
        }
    }

    fn expression_name(expression: &Expr) -> Option<String> {
        match expression {
            Expr::Name(name) => Some(name.id.to_string()),
            Expr::Attribute(attribute) => {
                let prefix = Self::expression_name(&attribute.value)?;
                Some(format!("{prefix}.{}", attribute.attr))
            }
            _ => None,
        }
    }

    fn is_shadowed(&self, name: &str, range: TextRange) -> bool {
        self.shadowed_names.contains(name)
            || self
                .scoped_shadowed
                .iter()
                .any(|(shadowed, scope)| shadowed == name && scope.contains_range(range))
    }

    fn current_scope(&self) -> Option<String> {
        self.class_scopes.last().cloned()
    }

    fn add_function_facts(
        &mut self,
        decorators: &[ruff_python_ast::Decorator],
        name: &str,
        parameters: &ruff_python_ast::Parameters,
        range: ruff_text_size::TextRange,
    ) {
        let location = self.location(range);
        let scope = self.current_scope();
        for parameter in parameters.iter() {
            self.pytest.fixture_requests.push(LocatedName {
                name: parameter.name().to_string(),
                location,
                scope: scope.clone(),
            });
        }
        for decorator in decorators {
            let Some(decorator_name) = Self::decorator_name(&decorator.expression) else {
                continue;
            };
            let (base, arguments) = match &decorator.expression {
                Expr::Call(call) => (Self::decorator_name(&call.func), Some(&call.arguments)),
                _ => (Some(decorator_name.clone()), None),
            };
            let Some(base) = base else { continue };
            let is_fixture = self.fixture_names.contains(&base)
                || base == "fixture"
                || base.ends_with(".fixture")
                    && self
                        .pytest_names
                        .contains(base.split('.').next().unwrap_or_default());
            if !is_fixture && decorator_name.ends_with("fixture") {
                self.pytest.unsupported_fixtures.push(location);
            }
            if is_fixture {
                let mut fixture_name = name.to_owned();
                let mut autouse = false;
                if let Some(arguments) = arguments {
                    if let Some(keyword) = arguments.find_keyword("name") {
                        if let Some(value) = Self::literal_string(&keyword.value) {
                            fixture_name = value;
                        } else {
                            self.pytest
                                .unsupported_fixtures
                                .push(self.location(keyword.range));
                        }
                    }
                    if let Some(keyword) = arguments.find_keyword("autouse") {
                        if let Expr::BooleanLiteral(value) = &keyword.value {
                            autouse = value.value;
                        } else {
                            self.pytest
                                .unsupported_fixtures
                                .push(self.location(keyword.range));
                        }
                    }
                }
                self.pytest.fixtures.push(FixtureDeclaration {
                    name: fixture_name,
                    autouse,
                    location,
                    scope: scope.clone(),
                    parameters: parameters
                        .iter()
                        .map(|parameter| LocatedName {
                            name: parameter.name().to_string(),
                            location,
                            scope: scope.clone(),
                        })
                        .collect(),
                });
            }
            let is_usefixtures = base == "pytest.mark.usefixtures"
                || base
                    .strip_suffix(".usefixtures")
                    .is_some_and(|prefix| self.mark_names.contains(prefix))
                || base.ends_with(".mark.usefixtures")
                    && self
                        .pytest_names
                        .contains(base.split('.').next().unwrap_or_default());
            if is_usefixtures && let Some(arguments) = arguments {
                for argument in &arguments.args {
                    if let Some(name) = Self::literal_string(argument) {
                        self.pytest.usefixtures.push(LocatedName {
                            name,
                            location,
                            scope: scope.clone(),
                        });
                    } else {
                        self.pytest.unsupported_fixtures.push(location);
                    }
                }
            }
        }
    }

    fn add_class_facts(
        &mut self,
        decorators: &[ruff_python_ast::Decorator],
        range: ruff_text_size::TextRange,
        scope: Option<String>,
    ) {
        let location = self.location(range);
        for decorator in decorators {
            let Expr::Call(call) = &decorator.expression else {
                continue;
            };
            let name = Self::decorator_name(&call.func);
            let is_usefixtures = name.as_deref() == Some("pytest.mark.usefixtures")
                || name
                    .as_deref()
                    .and_then(|name| name.strip_suffix(".usefixtures"))
                    .is_some_and(|prefix| self.mark_names.contains(prefix))
                || name.is_some_and(|name| {
                    name.ends_with(".mark.usefixtures")
                        && self
                            .pytest_names
                            .contains(name.split('.').next().unwrap_or_default())
                });
            if !is_usefixtures {
                continue;
            }
            for argument in &call.arguments.args {
                if let Some(name) = Self::literal_string(argument) {
                    self.pytest.usefixtures.push(LocatedName {
                        name,
                        location,
                        scope: scope.clone(),
                    });
                } else {
                    self.pytest.unsupported_fixtures.push(location);
                }
            }
        }
    }
}

impl<'a> Visitor<'a> for ImportCollector<'_> {
    fn visit_stmt(&mut self, statement: &'a Stmt) {
        match statement {
            Stmt::Import(import) => {
                for alias in &import.names {
                    let bound = alias.asname.as_ref().map_or_else(
                        || alias.name.split('.').next().unwrap_or_default().to_owned(),
                        ToString::to_string,
                    );
                    if alias.name.to_string() == "importlib" {
                        self.importlib_names.insert(bound.clone());
                    }
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
                    let bound = alias
                        .asname
                        .as_ref()
                        .map_or_else(|| alias.name.to_string(), ToString::to_string);
                    if import.module.as_deref() == Some("importlib")
                        && alias.name.to_string() == "import_module"
                    {
                        self.import_module_names.insert(bound.clone());
                    }
                    if import.module.as_deref() != Some("importlib") {
                        self.pytest.exports.push(ExportDeclaration {
                            name: alias.asname.as_ref().map_or_else(
                                || Some(alias.name.to_string()),
                                |name| Some(name.to_string()),
                            ),
                            module: import.module.as_ref().map(ToString::to_string),
                            imported_name: Some(alias.name.to_string()),
                            level: import.level,
                            wildcard: alias.name.to_string() == "*",
                            location: self.location(alias.range),
                        });
                    }
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
            Stmt::FunctionDef(function) => {
                if function.name.id == "__getattr__" {
                    self.pytest
                        .unsupported_exports
                        .push(self.location(function.range));
                }
                self.add_function_facts(
                    &function.decorator_list,
                    &function.name,
                    &function.parameters,
                    function.range,
                );
            }
            Stmt::ClassDef(class) => {
                let scope = Some(class.name.to_string());
                self.add_class_facts(&class.decorator_list, class.range, scope.clone());
                self.class_scopes.push(class.name.to_string());
                visitor::walk_stmt(self, statement);
                self.class_scopes.pop();
                return;
            }
            Stmt::Assign(assign) => {
                for target in &assign.targets {
                    if let Expr::Name(name) = target {
                        let name = name.id.to_string();
                        if name == "pytest_plugins" {
                            if let Some(values) = Self::literal_strings(&assign.value) {
                                let location = self.location(assign.range);
                                let scope = self.current_scope();
                                self.pytest
                                    .pytest_plugins
                                    .extend(values.into_iter().map(|name| LocatedName {
                                        name,
                                        location,
                                        scope: scope.clone(),
                                    }));
                            } else {
                                self.pytest
                                    .unsupported_plugins
                                    .push(self.location(assign.range));
                            }
                        } else if name == "__all__" {
                            if let Some(values) = Self::literal_strings(&assign.value) {
                                let location = self.location(assign.range);
                                self.pytest.exports.extend(values.into_iter().map(|name| {
                                    ExportDeclaration {
                                        name: Some(name),
                                        module: None,
                                        imported_name: None,
                                        level: 0,
                                        wildcard: false,
                                        location,
                                    }
                                }));
                            } else {
                                self.pytest
                                    .unsupported_exports
                                    .push(self.location(assign.range));
                            }
                        } else if name == "pytestmark"
                            && let Expr::Call(call) = assign.value.as_ref()
                        {
                            let name = Self::decorator_name(&call.func);
                            let is_usefixtures = name.as_deref() == Some("pytest.mark.usefixtures")
                                || name
                                    .as_deref()
                                    .and_then(|name| name.strip_suffix(".usefixtures"))
                                    .is_some_and(|prefix| self.mark_names.contains(prefix))
                                || name.is_some_and(|name| {
                                    name.ends_with(".mark.usefixtures")
                                        && self
                                            .pytest_names
                                            .contains(name.split('.').next().unwrap_or_default())
                                });
                            if is_usefixtures {
                                let location = self.location(assign.range);
                                for argument in &call.arguments.args {
                                    if let Some(name) = Self::literal_string(argument) {
                                        self.pytest.usefixtures.push(LocatedName {
                                            name,
                                            location,
                                            scope: self.current_scope(),
                                        });
                                    } else {
                                        self.pytest.unsupported_fixtures.push(location);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Stmt::AnnAssign(assign) => {
                if let Expr::Name(name) = assign.target.as_ref() {
                    let name = name.id.to_string();
                    if name == "__all__" && assign.value.is_none() {
                        self.pytest
                            .unsupported_exports
                            .push(self.location(assign.range));
                    }
                }
            }
            Stmt::AugAssign(assign) => {
                if let Expr::Name(name) = assign.target.as_ref()
                    && name.id == "__all__"
                {
                    self.pytest
                        .unsupported_exports
                        .push(self.location(assign.range));
                }
            }
            _ => {}
        }
        visitor::walk_stmt(self, statement);
    }

    fn visit_expr(&mut self, expression: &'a Expr) {
        if let Expr::Call(call) = expression {
            if let Some(name) = Self::expression_name(&call.func)
                && (name == "__all__.append" || name == "__all__.extend")
            {
                self.pytest
                    .unsupported_exports
                    .push(self.location(call.range));
            }
            if Self::expression_name(&call.func).as_deref() == Some("request.getfixturevalue") {
                let location = self.location(call.range);
                if let Some(name) = call
                    .arguments
                    .find_positional(0)
                    .and_then(Self::literal_string)
                {
                    self.pytest.fixture_requests.push(LocatedName {
                        name,
                        location,
                        scope: self.current_scope(),
                    });
                } else {
                    self.pytest.unresolved_fixture_lookups.push(location);
                }
            }
        }
        if let Expr::Call(call) = expression {
            let Some(name) = Self::expression_name(&call.func) else {
                visitor::walk_expr(self, expression);
                return;
            };
            let dynamic = if (self
                .importlib_names
                .contains(name.split('.').next().unwrap_or_default())
                && !self.is_shadowed(name.split('.').next().unwrap_or_default(), call.range)
                && name.ends_with(".import_module"))
                || (self.import_module_names.contains(&name)
                    && !self.is_shadowed(&name, call.range))
            {
                Some(DynamicImportApi::ImportModule)
            } else if name == "__import__" && !self.is_shadowed("__import__", call.range) {
                Some(DynamicImportApi::BuiltinImport)
            } else {
                None
            };
            if let Some(api) = dynamic {
                let module = call
                    .arguments
                    .find_argument_value("name", 0)
                    .and_then(Self::literal_string);
                let package = call
                    .arguments
                    .find_argument_value("package", 1)
                    .and_then(Self::literal_string);
                let fromlist = call
                    .arguments
                    .find_argument_value("fromlist", 3)
                    .and_then(Self::literal_strings)
                    .unwrap_or_default();
                let level = if api == DynamicImportApi::BuiltinImport {
                    call.arguments
                        .find_argument_value("level", 4)
                        .and_then(Self::literal_u32)
                        .unwrap_or(0)
                } else {
                    0
                };
                let location = self.location(call.range);
                let module = if api == DynamicImportApi::BuiltinImport
                    && level > 0
                    && module
                        .as_deref()
                        .is_some_and(|module| !module.starts_with('.'))
                {
                    module.map(|module| format!("{}{}", ".".repeat(level as usize), module))
                } else {
                    module
                };
                let reason = module
                    .is_none()
                    .then(|| "dynamic import target is not a literal string".to_owned());
                self.imports.push(self.located_at(
                    StaticImport::Dynamic {
                        api,
                        module,
                        package,
                        level,
                        fromlist,
                        reason,
                    },
                    location,
                ));
            }
        }
        visitor::walk_expr(self, expression);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        DynamicImportApi, SourceLocation, StaticImport, parse_imports,
        parse_imports_with_locations, parse_python,
    };

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

    #[test]
    fn recognizes_literal_dynamic_import_aliases_and_relative_targets() {
        let parsed = parse_python(
            "import importlib as il\nfrom importlib import import_module as load\nil.import_module(\"pkg.mod\")\nload(\".service\", package=\"pkg\")\n__import__(\"pkg.other\", fromlist=[\"value\"])\n",
            Path::new("pkg/loader.py"),
        )
        .expect("valid dynamic imports");
        let dynamics = parsed
            .imports
            .iter()
            .filter_map(|item| match &item.import {
                StaticImport::Dynamic { .. } => Some(&item.import),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(dynamics.len(), 3);
        assert!(matches!(
            dynamics[0],
            StaticImport::Dynamic {
                api: DynamicImportApi::ImportModule,
                module: Some(module),
                ..
            } if module == "pkg.mod"
        ));
        assert!(matches!(
            dynamics[1],
            StaticImport::Dynamic {
                module: Some(module),
                package: Some(package),
                ..
            } if module == ".service" && package == "pkg"
        ));
    }

    #[test]
    fn shadowed_dynamic_import_names_are_not_treated_as_import_apis() {
        let parsed = parse_python(
            "import importlib\nimportlib = object()\nimportlib.import_module(\"pkg.mod\")\ndef load(name):\n    return name\nload(\"pkg.other\")\n",
            Path::new("loader.py"),
        )
        .expect("valid Python");
        assert!(
            !parsed
                .imports
                .iter()
                .any(|item| matches!(item.import, StaticImport::Dynamic { .. }))
        );
    }

    #[test]
    fn dynamic_import_binding_respects_function_scope_boundaries() {
        let parsed = parse_python(
            "import importlib\ndef local(importlib):\n    importlib.import_module(\"wrong\")\nimportlib.import_module(\"pkg.mod\")\n",
            Path::new("loader.py"),
        )
        .expect("valid Python");
        let dynamics = parsed
            .imports
            .iter()
            .filter(|item| matches!(item.import, StaticImport::Dynamic { .. }))
            .collect::<Vec<_>>();
        assert_eq!(dynamics.len(), 1);
        assert!(matches!(
            &dynamics[0].import,
            StaticImport::Dynamic {
                module: Some(module),
                ..
            } if module == "pkg.mod"
        ));
    }

    #[test]
    fn recognizes_literal_fixture_lookup_and_marks_unsupported_forms() {
        let parsed = parse_python(
            "from pytest import fixture, mark\n@fixture(name=\"db\", autouse=True)\ndef database():\n    return 1\n@mark.usefixtures(\"db\", FIXTURE_NAME)\ndef test_value(request):\n    request.getfixturevalue(\"db\")\n    request.getfixturevalue(FIXTURE_NAME)\n",
            Path::new("tests/test_value.py"),
        )
        .expect("valid pytest source");
        assert_eq!(parsed.pytest.fixtures.len(), 1);
        assert_eq!(parsed.pytest.fixtures[0].name, "db");
        assert!(parsed.pytest.fixtures[0].autouse);
        assert!(
            parsed
                .pytest
                .usefixtures
                .iter()
                .any(|name| name.name == "db")
        );
        assert_eq!(parsed.pytest.fixture_requests[0].name, "request");
        assert!(
            parsed
                .pytest
                .fixture_requests
                .iter()
                .any(|name| name.name == "db")
        );
        assert_eq!(parsed.pytest.unresolved_fixture_lookups.len(), 1);
        assert!(!parsed.pytest.unsupported_fixtures.is_empty());
    }

    #[test]
    fn preserves_class_fixture_visibility_boundaries() {
        let parsed = parse_python(
            "import pytest\nclass Checkout:\n    @pytest.fixture\n    def token(self):\n        return \"token\"\n    @pytest.mark.usefixtures(\"token\")\n    def test_checkout(self, token):\n        pass\n",
            Path::new("tests/test_checkout.py"),
        )
        .expect("valid pytest source");
        assert_eq!(parsed.pytest.fixtures[0].scope.as_deref(), Some("Checkout"));
        assert!(
            parsed
                .pytest
                .fixture_requests
                .iter()
                .any(|request| request.name == "token"
                    && request.scope.as_deref() == Some("Checkout"))
        );
        assert!(
            parsed
                .pytest
                .usefixtures
                .iter()
                .any(|request| request.name == "token"
                    && request.scope.as_deref() == Some("Checkout"))
        );
    }
}
