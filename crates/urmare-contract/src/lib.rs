//! Stable machine-readable contracts shared by the CLI and MCP adapters.
//!
//! Core domain values remain presentation-independent. These DTOs own field
//! names, schema versions, omission rules, portable paths, and generated JSON
//! schemas for public adapter boundaries.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use urmare_core::{
    DependencyPath, FullValidationReason, ImportProvenance, StaticImport, ValidationMode,
    ValidationPlan, ValidationStepKind, display_repository_path,
};

pub const PLAN_SCHEMA_VERSION: u32 = 1;
pub const EXPLANATION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanOutput {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u32,
    pub changed: Vec<String>,
    pub directly_affected: Vec<String>,
    pub transitively_affected: Vec<String>,
    pub affected_tests: Vec<String>,
    pub validation: ValidationOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_validation: Option<FullValidationOutput>,
    pub attributions: Vec<AttributionOutput>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationOutput {
    pub mode: ValidationModeOutput,
    pub steps: Vec<ValidationStepOutput>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationModeOutput {
    Selective,
    None,
    Full,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationStepOutput {
    pub kind: ValidationStepKindOutput,
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStepKindOutput {
    Pytest,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FullValidationOutput {
    pub required: bool,
    pub reason: FullValidationReasonOutput,
    pub configuration_paths: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FullValidationReasonOutput {
    ConfigurationChanged,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttributionOutput {
    pub affected: String,
    pub caused_by: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyPathOutput {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u32,
    pub changed: String,
    pub affected: String,
    pub path: Vec<String>,
    pub steps: Vec<DependencyStepOutput>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyStepOutput {
    pub dependent: String,
    pub dependency: String,
    pub imports: Vec<ImportProvenanceOutput>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportProvenanceOutput {
    #[schemars(range(min = 1))]
    pub line: usize,
    #[schemars(range(min = 1))]
    pub column: usize,
    pub import: StaticImportOutput,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StaticImportOutput {
    Import {
        module: String,
    },
    From {
        module: Option<String>,
        name: String,
        level: u32,
    },
}

impl From<&ValidationPlan> for PlanOutput {
    fn from(plan: &ValidationPlan) -> Self {
        let impact = &plan.impact;
        Self {
            schema_version: PLAN_SCHEMA_VERSION,
            changed: paths(&impact.changed),
            directly_affected: paths(&impact.directly_affected),
            transitively_affected: paths(&impact.transitively_affected),
            affected_tests: paths(&impact.affected_tests),
            validation: ValidationOutput {
                mode: match plan.mode {
                    ValidationMode::Selective => ValidationModeOutput::Selective,
                    ValidationMode::None => ValidationModeOutput::None,
                    ValidationMode::Full => ValidationModeOutput::Full,
                },
                steps: plan
                    .steps
                    .iter()
                    .map(|step| ValidationStepOutput {
                        kind: match step.kind {
                            ValidationStepKind::Pytest => ValidationStepKindOutput::Pytest,
                        },
                        program: step.program.clone(),
                        args: paths(&step.args),
                    })
                    .collect(),
            },
            full_validation: impact.full_validation.as_ref().map(|validation| {
                FullValidationOutput {
                    required: true,
                    reason: match validation.reason {
                        FullValidationReason::ConfigurationChanged => {
                            FullValidationReasonOutput::ConfigurationChanged
                        }
                    },
                    configuration_paths: paths(&validation.configuration_paths),
                }
            }),
            attributions: impact
                .attributions
                .iter()
                .map(|attribution| AttributionOutput {
                    affected: display_repository_path(&attribution.affected),
                    caused_by: paths(&attribution.caused_by),
                })
                .collect(),
        }
    }
}

impl From<&DependencyPath> for DependencyPathOutput {
    fn from(explanation: &DependencyPath) -> Self {
        Self {
            schema_version: EXPLANATION_SCHEMA_VERSION,
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
        }
    }
}

/// Pretty-prints one shared DTO exactly as the CLI JSON boundary requires.
pub fn to_pretty_json(value: &impl Serialize) -> Result<String, serde_json::Error> {
    let mut output = serde_json::to_string_pretty(value)?;
    output.push('\n');
    Ok(output)
}

fn provenance(imports: &[ImportProvenance]) -> Vec<ImportProvenanceOutput> {
    imports
        .iter()
        .map(|provenance| ImportProvenanceOutput {
            line: provenance.location.line,
            column: provenance.location.column,
            import: match &provenance.import {
                StaticImport::Import { module } => StaticImportOutput::Import {
                    module: module.clone(),
                },
                StaticImport::From {
                    module,
                    name,
                    level,
                } => StaticImportOutput::From {
                    module: module.clone(),
                    name: name.clone(),
                    level: *level,
                },
            },
        })
        .collect()
}

fn paths(paths: &[std::path::PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| display_repository_path(path))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use schemars::schema_for;
    use serde_json::json;
    use urmare_core::{
        DependencyPath, DependencyStep, ImpactResult, ImportProvenance, SourceLocation,
        StaticImport, ValidationPlan,
    };

    use super::{DependencyPathOutput, PlanOutput};

    #[test]
    fn plan_conversion_preserves_schema_one_and_omits_absent_full_validation() {
        let plan = ValidationPlan::from_impact(ImpactResult {
            changed: vec![PathBuf::from("src/pkg/core.py")],
            directly_affected: Vec::new(),
            transitively_affected: Vec::new(),
            affected_tests: vec![PathBuf::from("tests/test_core.py")],
            attributions: Vec::new(),
            full_validation: None,
        });
        let value = serde_json::to_value(PlanOutput::from(&plan)).expect("plan JSON");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["validation"]["mode"], "selective");
        assert!(value.get("full_validation").is_none());
        assert_eq!(
            value["validation"]["steps"][0]["args"],
            json!(["tests/test_core.py"])
        );
        let schema = serde_json::to_value(schema_for!(PlanOutput)).expect("plan schema");
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn explanation_conversion_preserves_structured_import_evidence() {
        let explanation = DependencyPath {
            changed: PathBuf::from("src/pkg/core.py"),
            affected: PathBuf::from("tests/test_core.py"),
            path: vec![
                PathBuf::from("tests/test_core.py"),
                PathBuf::from("src/pkg/core.py"),
            ],
            steps: vec![DependencyStep {
                dependent: PathBuf::from("tests/test_core.py"),
                dependency: PathBuf::from("src/pkg/core.py"),
                imports: vec![ImportProvenance {
                    location: SourceLocation {
                        line: 1,
                        column: 17,
                    },
                    import: StaticImport::From {
                        module: Some("pkg".to_owned()),
                        name: "core".to_owned(),
                        level: 0,
                    },
                }],
            }],
        };
        let value = serde_json::to_value(DependencyPathOutput::from(&explanation))
            .expect("explanation JSON");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["steps"][0]["imports"][0]["line"], 1);
        assert_eq!(value["steps"][0]["imports"][0]["import"]["kind"], "from");
        let schema =
            serde_json::to_value(schema_for!(DependencyPathOutput)).expect("explanation schema");
        assert_eq!(schema["type"], "object");
    }
}
