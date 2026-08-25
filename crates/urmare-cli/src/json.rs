//! Versioned machine-readable output schemas.
//!
//! These DTOs intentionally live at the CLI boundary. The impact domain stays
//! independent of serialization, while field names and schema evolution remain
//! explicit and testable here.

use serde::Serialize;
use urmare_core::{
    DependencyEdge, DependencyPath, FullValidationReason, GraphInspection, GraphSummary,
    ImpactResult, ImportProvenance, ImportResolutionStatus, ImportResolutionTrace, StaticImport,
    UnresolvedImport, display_repository_path,
};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
struct GraphOutput {
    schema_version: u32,
    python_files: usize,
    modules: usize,
    import_edges: usize,
    tests: usize,
    unresolved_imports: usize,
    unresolved_import_details: Vec<UnresolvedImportOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inspection: Option<GraphInspectionOutput>,
}

#[derive(Debug, Serialize)]
struct UnresolvedImportOutput {
    importer: String,
    line: usize,
    column: usize,
    import: StaticImport,
}

#[derive(Debug, Serialize)]
struct GraphInspectionOutput {
    focus: Option<String>,
    source_roots: Vec<String>,
    modules: Vec<ModuleOutput>,
    edges: Vec<DependencyEdgeOutput>,
    resolution_traces: Vec<ImportResolutionTraceOutput>,
}

#[derive(Debug, Serialize)]
struct ModuleOutput {
    path: String,
    module: String,
    is_package: bool,
    is_test: bool,
    dependencies: Vec<String>,
    dependents: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DependencyEdgeOutput {
    dependent: String,
    dependency: String,
    imports: Vec<ImportProvenanceOutput>,
}

#[derive(Debug, Serialize)]
struct ImportProvenanceOutput {
    line: usize,
    column: usize,
    import: StaticImport,
}

#[derive(Debug, Serialize)]
struct ImportResolutionTraceOutput {
    importer: String,
    line: usize,
    column: usize,
    import: StaticImport,
    status: &'static str,
    candidate_modules: Vec<String>,
    resolved_modules: Vec<ResolvedModuleOutput>,
}

#[derive(Debug, Serialize)]
struct ResolvedModuleOutput {
    module: String,
    path: String,
}

#[derive(Debug, Serialize)]
struct ImpactOutput {
    schema_version: u32,
    changed: Vec<String>,
    directly_affected: Vec<String>,
    transitively_affected: Vec<String>,
    affected_tests: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    full_validation: Option<FullValidationOutput>,
    attributions: Vec<AttributionOutput>,
}

#[derive(Debug, Serialize)]
struct TestSelectionOutput {
    schema_version: u32,
    changed: Vec<String>,
    affected_tests: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    full_validation: Option<FullValidationOutput>,
    attributions: Vec<AttributionOutput>,
}

#[derive(Debug, Serialize)]
struct FullValidationOutput {
    required: bool,
    reason: &'static str,
    configuration_paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AttributionOutput {
    affected: String,
    caused_by: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DependencyPathOutput {
    schema_version: u32,
    changed: String,
    affected: String,
    path: Vec<String>,
    steps: Vec<DependencyStepOutput>,
}

#[derive(Debug, Serialize)]
struct DependencyStepOutput {
    dependent: String,
    dependency: String,
    imports: Vec<ImportProvenanceOutput>,
}

/// Serializes repository graph statistics using schema version 1.
pub fn graph(
    summary: &GraphSummary,
    unresolved_imports: &[UnresolvedImport],
    inspection: Option<&GraphInspection>,
) -> Result<String, serde_json::Error> {
    pretty_json(&GraphOutput {
        schema_version: SCHEMA_VERSION,
        python_files: summary.python_files,
        modules: summary.modules,
        import_edges: summary.import_edges,
        tests: summary.tests,
        unresolved_imports: summary.unresolved_imports,
        unresolved_import_details: unresolved_imports
            .iter()
            .map(|unresolved| UnresolvedImportOutput {
                importer: display_repository_path(&unresolved.importer),
                line: unresolved.location.line,
                column: unresolved.location.column,
                import: unresolved.import.clone(),
            })
            .collect(),
        inspection: inspection.map(graph_inspection),
    })
}

/// Serializes a complete impact result using schema version 1.
pub fn impact(impact: &ImpactResult) -> Result<String, serde_json::Error> {
    pretty_json(&ImpactOutput {
        schema_version: SCHEMA_VERSION,
        changed: paths(&impact.changed),
        directly_affected: paths(&impact.directly_affected),
        transitively_affected: paths(&impact.transitively_affected),
        affected_tests: paths(&impact.affected_tests),
        full_validation: full_validation(impact),
        attributions: impact
            .attributions
            .iter()
            .map(|attribution| AttributionOutput {
                affected: display_repository_path(&attribution.affected),
                caused_by: paths(&attribution.caused_by),
            })
            .collect(),
    })
}

/// Serializes affected-test selection using schema version 1.
pub fn tests(impact: &ImpactResult) -> Result<String, serde_json::Error> {
    pretty_json(&TestSelectionOutput {
        schema_version: SCHEMA_VERSION,
        changed: paths(&impact.changed),
        affected_tests: paths(&impact.affected_tests),
        full_validation: full_validation(impact),
        attributions: if impact.full_validation.is_some() {
            Vec::new()
        } else {
            impact
                .affected_tests
                .iter()
                .map(|affected| AttributionOutput {
                    affected: display_repository_path(affected),
                    caused_by: paths(impact.causes_for(affected)),
                })
                .collect()
        },
    })
}

/// Serializes one dependency explanation using schema version 1.
pub fn why(explanation: &DependencyPath) -> Result<String, serde_json::Error> {
    pretty_json(&DependencyPathOutput {
        schema_version: SCHEMA_VERSION,
        changed: display_repository_path(&explanation.changed),
        affected: display_repository_path(&explanation.affected),
        path: paths(&explanation.path),
        steps: explanation
            .steps
            .iter()
            .map(|step| DependencyStepOutput {
                dependent: display_repository_path(&step.dependent),
                dependency: display_repository_path(&step.dependency),
                imports: provenance(&step.imports),
            })
            .collect(),
    })
}

fn full_validation(impact: &ImpactResult) -> Option<FullValidationOutput> {
    impact
        .full_validation
        .as_ref()
        .map(|validation| FullValidationOutput {
            required: true,
            reason: match validation.reason {
                FullValidationReason::ConfigurationChanged => "configuration_changed",
            },
            configuration_paths: paths(&validation.configuration_paths),
        })
}

fn graph_inspection(inspection: &GraphInspection) -> GraphInspectionOutput {
    GraphInspectionOutput {
        focus: inspection.focus.as_deref().map(display_repository_path),
        source_roots: inspection
            .source_roots
            .iter()
            .map(|root| {
                if root.as_os_str().is_empty() {
                    ".".to_owned()
                } else {
                    display_repository_path(root)
                }
            })
            .collect(),
        modules: inspection
            .modules
            .iter()
            .map(|module| ModuleOutput {
                path: display_repository_path(&module.path),
                module: module.module.clone(),
                is_package: module.is_package,
                is_test: module.is_test,
                dependencies: paths(&module.dependencies),
                dependents: paths(&module.dependents),
            })
            .collect(),
        edges: inspection.edges.iter().map(dependency_edge).collect(),
        resolution_traces: inspection
            .resolution_traces
            .iter()
            .map(resolution_trace)
            .collect(),
    }
}

fn dependency_edge(edge: &DependencyEdge) -> DependencyEdgeOutput {
    DependencyEdgeOutput {
        dependent: display_repository_path(&edge.dependent),
        dependency: display_repository_path(&edge.dependency),
        imports: provenance(&edge.imports),
    }
}

fn provenance(imports: &[ImportProvenance]) -> Vec<ImportProvenanceOutput> {
    imports
        .iter()
        .map(|provenance| ImportProvenanceOutput {
            line: provenance.location.line,
            column: provenance.location.column,
            import: provenance.import.clone(),
        })
        .collect()
}

fn resolution_trace(trace: &ImportResolutionTrace) -> ImportResolutionTraceOutput {
    ImportResolutionTraceOutput {
        importer: display_repository_path(&trace.importer),
        line: trace.location.line,
        column: trace.location.column,
        import: trace.import.clone(),
        status: match trace.status {
            ImportResolutionStatus::Resolved => "resolved",
            ImportResolutionStatus::Unresolved => "unresolved",
            ImportResolutionStatus::InvalidRelativeImport => "invalid_relative_import",
        },
        candidate_modules: trace.candidate_modules.clone(),
        resolved_modules: trace
            .resolved_modules
            .iter()
            .map(|resolved| ResolvedModuleOutput {
                module: resolved.module.clone(),
                path: display_repository_path(&resolved.path),
            })
            .collect(),
    }
}

fn paths(paths: &[std::path::PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| display_repository_path(path))
        .collect()
}

fn pretty_json(value: &impl Serialize) -> Result<String, serde_json::Error> {
    let mut output = serde_json::to_string_pretty(value)?;
    output.push('\n');
    Ok(output)
}
