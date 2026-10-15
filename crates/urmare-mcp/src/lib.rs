//! Local, repository-bound stdio Model Context Protocol adapter for Urmare.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode,
    Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    Tool, ToolAnnotations,
};
use rmcp::service::{QuitReason, RequestContext, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Stdin, Stdout};
use tokio::sync::Mutex;
use urmare_app::{ChangeSelection, ExplanationContext, RepositoryApplication};
use urmare_contract::{DependencyPathOutput, PlanOutput};

const VALIDATION_PLAN_TOOL: &str = "validation_plan";
const EXPLAIN_IMPACT_TOOL: &str = "explain_impact";
const MAX_PROTOCOL_LINE_BYTES: usize = 16 * 1024 * 1024;

const VALIDATION_PLAN_DESCRIPTION: &str = "Return the deterministic Python impact and pytest validation plan for exactly one change source. This tool does not execute validation or modify the repository.";
const EXPLAIN_IMPACT_DESCRIPTION: &str = "Explain one deterministic dependency path from an affected dependent toward a changed dependency, including static import evidence and source locations.";

/// Instructions advertised during MCP initialization. The first paragraph is
/// deliberately self-contained for clients that truncate server instructions.
pub const SERVER_INSTRUCTIONS: &str = "Run `validation_plan` after Python edits. Execute every returned validation step using the repository's established environment. Recompute the plan after further edits. Use `explain_impact` when an affected relationship needs investigation. Urmare does not execute tests or modify repository files.\n\nThe server is permanently bound to the repository root supplied at startup. It may update Urmare's private operating-system cache, but it never writes inside the analyzed repository.";

#[derive(Debug, Error)]
pub enum McpServerError {
    #[error("MCP requires `--root` to be an absolute repository path; got `{0}`")]
    RootNotAbsolute(PathBuf),

    #[error(transparent)]
    Analysis(#[from] urmare_core::AnalysisError),

    #[error("unable to start the MCP runtime: {0}")]
    Runtime(#[from] std::io::Error),

    #[error("unable to initialize the MCP server: {0}")]
    Initialize(Box<rmcp::service::ServerInitializeError>),

    #[error("MCP server task failed: {0}")]
    ServerTask(#[from] tokio::task::JoinError),

    #[error("MCP server transport failed: {0}")]
    Transport(tokio::task::JoinError),
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidationPlanInput {
    pub source: ValidationSourceInput,
}

#[derive(Clone, Debug, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ValidationSourceInput {
    Files {
        #[schemars(length(min = 1))]
        files: Vec<String>,
    },
    Changed,
    GitDiff {
        #[schemars(length(min = 1))]
        base: String,
    },
}

impl<'de> Deserialize<'de> for ValidationSourceInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut object = serde_json::Map::<String, Value>::deserialize(deserializer)?;
        let kind = take_string::<D::Error>(&mut object, "kind")?;
        match kind.as_str() {
            "files" => {
                reject_unknown::<D::Error>(&object, &["files"])?;
                let files = object
                    .remove("files")
                    .ok_or_else(|| serde::de::Error::missing_field("files"))?;
                let files = serde_json::from_value(files).map_err(serde::de::Error::custom)?;
                Ok(Self::Files { files })
            }
            "changed" => {
                reject_unknown::<D::Error>(&object, &[])?;
                Ok(Self::Changed)
            }
            "git_diff" => {
                reject_unknown::<D::Error>(&object, &["base"])?;
                let base = take_string::<D::Error>(&mut object, "base")?;
                Ok(Self::GitDiff { base })
            }
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["files", "changed", "git_diff"],
            )),
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExplainImpactInput {
    pub changed_file: String,
    pub affected_file: String,
    pub context: ExplanationContextInput,
}

#[derive(Clone, Debug, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExplanationContextInput {
    Current,
    Changed,
    GitDiff {
        #[schemars(length(min = 1))]
        base: String,
    },
}

impl<'de> Deserialize<'de> for ExplanationContextInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut object = serde_json::Map::<String, Value>::deserialize(deserializer)?;
        let kind = take_string::<D::Error>(&mut object, "kind")?;
        match kind.as_str() {
            "current" => {
                reject_unknown::<D::Error>(&object, &[])?;
                Ok(Self::Current)
            }
            "changed" => {
                reject_unknown::<D::Error>(&object, &[])?;
                Ok(Self::Changed)
            }
            "git_diff" => {
                reject_unknown::<D::Error>(&object, &["base"])?;
                let base = take_string::<D::Error>(&mut object, "base")?;
                Ok(Self::GitDiff { base })
            }
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["current", "changed", "git_diff"],
            )),
        }
    }
}

fn take_string<E: serde::de::Error>(
    object: &mut serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<String, E> {
    let value = object
        .remove(field)
        .ok_or_else(|| serde::de::Error::missing_field(field))?;
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}

fn reject_unknown<E: serde::de::Error>(
    object: &serde_json::Map<String, Value>,
    allowed: &'static [&'static str],
) -> Result<(), E> {
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        Err(serde::de::Error::unknown_field(field, allowed))
    } else {
        Ok(())
    }
}

/// Local MCP service bound to one canonical repository root.
#[derive(Clone, Debug)]
pub struct UrmareMcpServer {
    application: RepositoryApplication,
    analysis_gate: Arc<Mutex<()>>,
}

/// RMCP's protocol engine remains authoritative. This transport wrapper only
/// tightens stdio framing by returning JSON-RPC parse/invalid-request errors
/// instead of silently ignoring malformed input.
struct StrictStdioTransport {
    reader: BufReader<Stdin>,
    writer: Arc<Mutex<Option<Stdout>>>,
    line: Vec<u8>,
}

impl StrictStdioTransport {
    fn new() -> Self {
        Self {
            reader: BufReader::new(tokio::io::stdin()),
            writer: Arc::new(Mutex::new(Some(tokio::io::stdout()))),
            line: Vec::new(),
        }
    }
}

impl Transport<RoleServer> for StrictStdioTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        send_protocol_message(self.writer.clone(), item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            // The service loop may cancel this future when another branch
            // wins. Keep any partial line so the next poll resumes it.
            match self.reader.read_until(b'\n', &mut self.line).await {
                Ok(0) => return None,
                Ok(_) => {}
                Err(error) => {
                    eprintln!("MCP stdio read failed: {error}");
                    return None;
                }
            }

            if self.line.len() > MAX_PROTOCOL_LINE_BYTES {
                eprintln!("MCP protocol input exceeded the 16 MiB line limit");
                self.line.clear();
                let response = TxJsonRpcMessage::<RoleServer>::error(
                    McpError::parse_error("JSON-RPC message exceeds the 16 MiB limit", None),
                    None,
                );
                if send_protocol_message(self.writer.clone(), response)
                    .await
                    .is_err()
                {
                    return None;
                }
                continue;
            }

            let parsed = {
                let line = self
                    .line
                    .strip_suffix(b"\n")
                    .unwrap_or(self.line.as_slice());
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line.is_empty() {
                    self.line.clear();
                    continue;
                }
                serde_json::from_slice::<RxJsonRpcMessage<RoleServer>>(line)
            };
            self.line.clear();
            match parsed {
                Ok(message) => return Some(message),
                Err(error) => {
                    eprintln!("MCP protocol input error: {error}");
                    let protocol_error = match error.classify() {
                        serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
                            McpError::parse_error("Parse error", None)
                        }
                        serde_json::error::Category::Data | serde_json::error::Category::Io => {
                            McpError::invalid_request("Invalid request", None)
                        }
                    };
                    let response = TxJsonRpcMessage::<RoleServer>::error(protocol_error, None);
                    if send_protocol_message(self.writer.clone(), response)
                        .await
                        .is_err()
                    {
                        return None;
                    }
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        let mut writer = self.writer.lock().await;
        if let Some(mut stdout) = writer.take() {
            stdout.flush().await?;
            stdout.shutdown().await?;
        }
        Ok(())
    }
}

async fn send_protocol_message(
    writer: Arc<Mutex<Option<Stdout>>>,
    item: TxJsonRpcMessage<RoleServer>,
) -> Result<(), std::io::Error> {
    let mut encoded = serde_json::to_vec(&item)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    encoded.push(b'\n');
    let mut writer = writer.lock().await;
    let stdout = writer.as_mut().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "MCP stdout is closed")
    })?;
    stdout.write_all(&encoded).await?;
    stdout.flush().await
}

impl UrmareMcpServer {
    fn bind(root: &Path) -> Result<Self, McpServerError> {
        if !root.is_absolute() {
            return Err(McpServerError::RootNotAbsolute(root.to_path_buf()));
        }
        Ok(Self {
            application: RepositoryApplication::bind(root)?,
            analysis_gate: Arc::new(Mutex::new(())),
        })
    }

    fn tools() -> Vec<Tool> {
        let annotations = ToolAnnotations::new()
            .read_only(true)
            .destructive(false)
            .open_world(false);
        vec![
            Tool::new(
                VALIDATION_PLAN_TOOL,
                VALIDATION_PLAN_DESCRIPTION,
                serde_json::Map::<String, Value>::new(),
            )
            .with_input_schema::<ValidationPlanInput>()
            .with_output_schema::<PlanOutput>()
            .with_annotations(annotations.clone()),
            Tool::new(
                EXPLAIN_IMPACT_TOOL,
                EXPLAIN_IMPACT_DESCRIPTION,
                serde_json::Map::<String, Value>::new(),
            )
            .with_input_schema::<ExplainImpactInput>()
            .with_output_schema::<DependencyPathOutput>()
            .with_annotations(annotations),
        ]
    }

    async fn validation_plan(
        &self,
        input: ValidationPlanInput,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let selection = match input.source {
            ValidationSourceInput::Files { files } => {
                if files.is_empty() {
                    return Err(McpError::invalid_params(
                        "`source.files` must contain at least one path",
                        None,
                    ));
                }
                ChangeSelection::Files(files.into_iter().map(PathBuf::from).collect())
            }
            ValidationSourceInput::Changed => ChangeSelection::Changed,
            ValidationSourceInput::GitDiff { base } => {
                require_non_empty_base(&base)?;
                ChangeSelection::GitDiff { base }
            }
        };
        let application = self.application.clone();
        self.run_analysis(context, move || {
            application
                .validation_plan(&selection)
                .map(|plan| PlanOutput::from(&plan))
        })
        .await
    }

    async fn explain_impact(
        &self,
        input: ExplainImpactInput,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let explanation_context = match input.context {
            ExplanationContextInput::Current => ExplanationContext::Current,
            ExplanationContextInput::Changed => ExplanationContext::Changed,
            ExplanationContextInput::GitDiff { base } => {
                require_non_empty_base(&base)?;
                ExplanationContext::GitDiff { base }
            }
        };
        let application = self.application.clone();
        let changed_file = PathBuf::from(input.changed_file);
        let affected_file = PathBuf::from(input.affected_file);
        self.run_analysis(context, move || {
            application
                .explain_impact(&changed_file, &affected_file, &explanation_context)
                .map(|explanation| DependencyPathOutput::from(&explanation))
        })
        .await
    }

    async fn run_analysis<T, F>(
        &self,
        context: RequestContext<RoleServer>,
        operation: F,
    ) -> Result<CallToolResponse, McpError>
    where
        T: Serialize + Send + 'static,
        F: FnOnce() -> Result<T, urmare_core::AnalysisError> + Send + 'static,
    {
        let cancellation = context.ct.clone();
        let gate = self.analysis_gate.clone();
        let guard = tokio::select! {
            _ = cancellation.cancelled() => {
                return Ok(tool_error("request cancelled").into());
            }
            guard = gate.lock_owned() => guard,
        };

        // The owned guard moves into the blocking task. If a request is
        // cancelled, no later request can overlap an analysis still winding
        // down in the background.
        let task = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            operation()
        });
        let result = tokio::select! {
            _ = cancellation.cancelled() => {
                return Ok(tool_error("request cancelled").into());
            }
            result = task => result,
        };

        match result {
            Ok(Ok(output)) => structured_result(&output).map(Into::into),
            Ok(Err(error)) => Ok(tool_error(error.to_string()).into()),
            Err(error) => {
                eprintln!("MCP analysis task failed: {error}");
                Err(McpError::internal_error(
                    "Urmare's analysis task failed unexpectedly; retry the request",
                    None,
                ))
            }
        }
    }
}

impl ServerHandler for UrmareMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("urmare", env!("CARGO_PKG_VERSION")))
            .with_instructions(SERVER_INSTRUCTIONS)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(Self::tools())))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        Self::tools().into_iter().find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        match request.name.as_ref() {
            VALIDATION_PLAN_TOOL => {
                let input = parse_arguments::<ValidationPlanInput>(request.arguments)?;
                self.validation_plan(input, context).await
            }
            EXPLAIN_IMPACT_TOOL => {
                let input = parse_arguments::<ExplainImpactInput>(request.arguments)?;
                self.explain_impact(input, context).await
            }
            name => Err(McpError::new(
                ErrorCode::METHOD_NOT_FOUND,
                format!("unknown Urmare tool `{name}`"),
                None,
            )),
        }
    }
}

fn parse_arguments<T: DeserializeOwned>(
    arguments: Option<serde_json::Map<String, Value>>,
) -> Result<T, McpError> {
    serde_json::from_value(Value::Object(arguments.unwrap_or_default()))
        .map_err(|error| McpError::invalid_params(format!("invalid tool arguments: {error}"), None))
}

fn require_non_empty_base(base: &str) -> Result<(), McpError> {
    if base.trim().is_empty() {
        Err(McpError::invalid_params(
            "`base` must be a non-empty locally available Git revision",
            None,
        ))
    } else {
        Ok(())
    }
}

fn structured_result(value: &impl Serialize) -> Result<CallToolResult, McpError> {
    let value = serde_json::to_value(value).map_err(|error| {
        eprintln!("MCP result serialization failed: {error}");
        McpError::internal_error("Urmare could not serialize its result", None)
    })?;
    Ok(CallToolResult::structured(value))
}

fn tool_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

/// Runs Urmare's local MCP server, bound permanently to one canonical root.
pub fn run(root: &Path) -> Result<(), McpServerError> {
    let server = UrmareMcpServer::bind(root)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let service = match server.serve(StrictStdioTransport::new()).await {
            Ok(service) => service,
            Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
            Err(error) => return Err(McpServerError::Initialize(Box::new(error))),
        };
        match service.waiting().await? {
            QuitReason::Cancelled | QuitReason::Closed => Ok(()),
            QuitReason::JoinError(error) => Err(McpServerError::Transport(error)),
            _ => Ok(()),
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        ExplainImpactInput, ExplanationContextInput, UrmareMcpServer, ValidationPlanInput,
        ValidationSourceInput, parse_arguments, structured_result,
    };

    #[test]
    fn discriminated_inputs_reject_conflicts_and_unknown_fields() {
        let files: ValidationPlanInput = parse_arguments(Some(
            json!({"source":{"kind":"files","files":["src/pkg/core.py"]}})
                .as_object()
                .expect("object")
                .clone(),
        ))
        .expect("valid files input");
        assert!(matches!(files.source, ValidationSourceInput::Files { .. }));

        assert!(
            parse_arguments::<ValidationPlanInput>(Some(
                json!({"source":{"kind":"unsupported"}})
                    .as_object()
                    .expect("object")
                    .clone(),
            ))
            .is_err()
        );
        assert!(
            parse_arguments::<ExplainImpactInput>(Some(
                json!({
                    "changed_file":"src/pkg/core.py",
                    "affected_file":"tests/test_core.py",
                    "context":{"kind":"current"},
                    "root":"/tmp/not-allowed"
                })
                .as_object()
                .expect("object")
                .clone(),
            ))
            .is_err()
        );
    }

    #[test]
    fn advertised_tools_have_strict_schemas_and_read_only_annotations() {
        let tools = UrmareMcpServer::tools();
        assert_eq!(tools.len(), 2);
        for tool in tools {
            assert!(tool.input_schema.contains_key("additionalProperties"));
            assert!(tool.output_schema.is_some());
            let annotations = tool.annotations.expect("tool annotations");
            assert_eq!(annotations.read_only_hint, Some(true));
            assert_eq!(annotations.destructive_hint, Some(false));
            assert_eq!(annotations.open_world_hint, Some(false));
        }
    }

    #[test]
    fn explanation_context_is_required() {
        assert!(
            parse_arguments::<ExplainImpactInput>(Some(
                json!({
                    "changed_file":"src/pkg/core.py",
                    "affected_file":"tests/test_core.py"
                })
                .as_object()
                .expect("object")
                .clone(),
            ))
            .is_err()
        );
        let input: ExplainImpactInput = parse_arguments(Some(
            json!({
                "changed_file":"src/pkg/core.py",
                "affected_file":"tests/test_core.py",
                "context":{"kind":"changed"}
            })
            .as_object()
            .expect("object")
            .clone(),
        ))
        .expect("changed context");
        assert!(matches!(input.context, ExplanationContextInput::Changed));
    }

    #[test]
    fn representative_results_validate_against_advertised_output_schemas() {
        let tools = UrmareMcpServer::tools();
        let cases = [
            (
                "validation_plan",
                json!({
                    "schema_version": 1,
                    "changed": ["src/pkg/core.py"],
                    "directly_affected": ["tests/test_core.py"],
                    "transitively_affected": [],
                    "affected_tests": ["tests/test_core.py"],
                    "validation": {
                        "mode": "selective",
                        "steps": [{
                            "kind": "pytest",
                            "program": "pytest",
                            "args": ["tests/test_core.py"]
                        }]
                    },
                    "attributions": [{
                        "affected": "tests/test_core.py",
                        "caused_by": ["src/pkg/core.py"]
                    }]
                }),
            ),
            (
                "validation_plan",
                json!({
                    "schema_version": 1,
                    "changed": ["src/pkg/standalone.py"],
                    "directly_affected": [],
                    "transitively_affected": [],
                    "affected_tests": [],
                    "validation": {"mode": "none", "steps": []},
                    "attributions": []
                }),
            ),
            (
                "validation_plan",
                json!({
                    "schema_version": 1,
                    "changed": [],
                    "directly_affected": [],
                    "transitively_affected": [],
                    "affected_tests": ["tests/test_core.py"],
                    "validation": {
                        "mode": "full",
                        "steps": [{"kind":"pytest","program":"pytest","args":[]}]
                    },
                    "full_validation": {
                        "required": true,
                        "reason": "configuration_changed",
                        "configuration_paths": ["pyproject.toml"]
                    },
                    "attributions": []
                }),
            ),
            (
                "explain_impact",
                json!({
                    "schema_version": 1,
                    "changed": "src/pkg/core.py",
                    "affected": "tests/test_core.py",
                    "path": ["tests/test_core.py", "src/pkg/core.py"],
                    "steps": [{
                        "dependent": "tests/test_core.py",
                        "dependency": "src/pkg/core.py",
                        "imports": [{
                            "line": 1,
                            "column": 1,
                            "import": {"kind": "import", "module": "pkg.core"}
                        }]
                    }]
                }),
            ),
        ];

        for (name, value) in cases {
            let tool = tools
                .iter()
                .find(|tool| tool.name == name)
                .expect("advertised tool");
            let schema = serde_json::Value::Object(
                tool.output_schema
                    .as_ref()
                    .expect("output schema")
                    .as_ref()
                    .clone(),
            );
            let validator = jsonschema::validator_for(&schema).expect("valid advertised schema");
            validator
                .validate(&value)
                .unwrap_or_else(|error| panic!("{name} output does not match schema: {error}"));

            let result = structured_result(&value).expect("structured result");
            assert_eq!(result.structured_content, Some(value.clone()));
            let text = result.content[0]
                .as_text()
                .expect("text fallback")
                .text
                .as_str();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(text).unwrap(),
                value
            );
        }
    }
}
