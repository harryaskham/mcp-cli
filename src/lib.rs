use std::io::{self, BufRead, BufReader, Write};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

/// Stable schema version for JSON envelopes shared by CLI and MCP surfaces.
pub const JSON_SCHEMA_VERSION: u32 = 1;

/// Stable categories for structured JSON and MCP errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Validation,
    UnsupportedCapability,
    MissingPermission,
    TargetNotFound,
    PlatformAdapterFailure,
    ExecutionFailure,
    ConfigError,
    SerializationError,
    /// Operation exceeded a configured deadline (e.g. capture portal/grim hang).
    Timeout,
}

/// Stable metadata attached to every machine-readable response envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeMeta {
    pub schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl Default for EnvelopeMeta {
    fn default() -> Self {
        Self {
            schema_version: JSON_SCHEMA_VERSION,
            command: None,
        }
    }
}

impl EnvelopeMeta {
    #[must_use]
    pub fn for_command(command: impl Into<String>) -> Self {
        Self {
            schema_version: JSON_SCHEMA_VERSION,
            command: Some(command.into()),
        }
    }
}

/// Errors that can be projected into a stable JSON/MCP error payload.
pub trait StructuredError {
    fn category(&self) -> ErrorCategory;

    fn code(&self) -> String;

    fn message(&self) -> String;

    fn details(&self) -> Option<Value> {
        None
    }
}

/// Structured error payload shared by CLI and MCP surfaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonError {
    pub category: ErrorCategory,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl JsonError {
    #[must_use]
    pub fn new(
        category: ErrorCategory,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            category,
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    #[must_use]
    pub fn from_error<E>(error: &E) -> Self
    where
        E: StructuredError + ?Sized,
    {
        let mut value = Self::new(error.category(), error.code(), error.message());
        if let Some(details) = error.details() {
            value = value.with_details(details);
        }
        value
    }

    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl StructuredError for JsonError {
    fn category(&self) -> ErrorCategory {
        self.category
    }

    fn code(&self) -> String {
        self.code.clone()
    }

    fn message(&self) -> String {
        self.message.clone()
    }

    fn details(&self) -> Option<Value> {
        self.details.clone()
    }
}

/// Structured success/error envelope for machine-readable command responses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JsonEnvelope<T> {
    Success {
        meta: EnvelopeMeta,
        data: T,
    },
    Error {
        meta: EnvelopeMeta,
        error: JsonError,
    },
}

impl<T> JsonEnvelope<T> {
    #[must_use]
    pub fn success(data: T) -> Self {
        Self::Success {
            meta: EnvelopeMeta::default(),
            data,
        }
    }

    #[must_use]
    pub fn success_for(command: impl Into<String>, data: T) -> Self {
        Self::Success {
            meta: EnvelopeMeta::for_command(command),
            data,
        }
    }

    #[must_use]
    pub fn error(error: JsonError) -> Self {
        Self::Error {
            meta: EnvelopeMeta::default(),
            error,
        }
    }

    #[must_use]
    pub fn error_for(command: impl Into<String>, error: JsonError) -> Self {
        Self::Error {
            meta: EnvelopeMeta::for_command(command),
            error,
        }
    }

    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }
}

/// Convert a command result into a stable JSON envelope.
#[must_use]
pub fn envelope_from_result<T, E>(result: Result<T, E>) -> JsonEnvelope<T>
where
    E: StructuredError,
{
    match result {
        Ok(data) => JsonEnvelope::success(data),
        Err(error) => JsonEnvelope::error(JsonError::from_error(&error)),
    }
}

/// Convert a borrowed command result into a stable JSON envelope.
#[must_use]
pub fn envelope_from_result_ref<'a, T, E>(result: Result<&'a T, &'a E>) -> JsonEnvelope<&'a T>
where
    T: Serialize,
    E: StructuredError,
{
    match result {
        Ok(data) => JsonEnvelope::success(data),
        Err(error) => JsonEnvelope::error(JsonError::from_error(error)),
    }
}

/// Serialize a command result as a JSON envelope followed by a newline.
pub fn write_json_result<W, T, E>(mut writer: W, result: Result<T, E>) -> Result<(), McpCliError>
where
    W: Write,
    T: Serialize,
    E: StructuredError,
{
    serde_json::to_writer(&mut writer, &envelope_from_result(result))?;
    writer.write_all(b"\n")?;
    Ok(())
}

/// Serialize a borrowed command result as a JSON envelope followed by a newline.
pub fn write_json_result_ref<W, T, E>(
    mut writer: W,
    result: &Result<T, E>,
) -> Result<(), McpCliError>
where
    W: Write,
    T: Serialize,
    E: StructuredError,
{
    let envelope = match result {
        Ok(data) => JsonEnvelope::success(data),
        Err(error) => JsonEnvelope::error(JsonError::from_error(error)),
    };
    serde_json::to_writer(&mut writer, &envelope)?;
    writer.write_all(b"\n")?;
    Ok(())
}

/// Metadata describing the MCP stdio server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioServerConfig {
    pub server_name: String,
    pub server_version: String,
}

/// Public MCP tool metadata surfaced to clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

type ToolHandler<Ctx> = dyn Fn(&Ctx, Value) -> JsonEnvelope<Value> + Send + Sync;

/// A typed tool binding that can be exposed over MCP.
pub struct Tool<Ctx> {
    metadata: ToolMetadata,
    handler: Arc<ToolHandler<Ctx>>,
}

impl<Ctx> Tool<Ctx> {
    #[must_use]
    pub fn new_typed<Input, Output, Error, Handler>(
        name: impl Into<String>,
        description: impl Into<String>,
        handler: Handler,
    ) -> Self
    where
        Input: DeserializeOwned + JsonSchema + 'static,
        Output: Serialize + 'static,
        Error: StructuredError + 'static,
        Handler: Fn(&Ctx, Input) -> Result<Output, Error> + Send + Sync + 'static,
    {
        let tool_name = name.into();
        let metadata = ToolMetadata {
            name: tool_name.clone(),
            description: description.into(),
            input_schema: serde_json::to_value(schemars::schema_for!(Input))
                .expect("tool schema should serialize"),
        };

        let erased_handler =
            move |ctx: &Ctx, arguments: Value| match serde_json::from_value(arguments) {
                Ok(input) => match handler(ctx, input) {
                    Ok(output) => match serde_json::to_value(output) {
                        Ok(data) => JsonEnvelope::success_for(tool_name.clone(), data),
                        Err(error) => JsonEnvelope::error_for(
                            tool_name.clone(),
                            JsonError::new(
                                ErrorCategory::SerializationError,
                                "serialization_error",
                                format!("failed to serialize tool result: {error}"),
                            ),
                        ),
                    },
                    Err(error) => {
                        JsonEnvelope::error_for(tool_name.clone(), JsonError::from_error(&error))
                    }
                },
                Err(error) => JsonEnvelope::error_for(
                    tool_name.clone(),
                    JsonError::new(
                        ErrorCategory::Validation,
                        "invalid_tool_arguments",
                        format!("invalid tool arguments: {error}"),
                    ),
                ),
            };

        Self {
            metadata,
            handler: Arc::new(erased_handler),
        }
    }

    #[must_use]
    pub fn metadata(&self) -> &ToolMetadata {
        &self.metadata
    }

    #[must_use]
    pub fn call(&self, ctx: &Ctx, arguments: Value) -> JsonEnvelope<Value> {
        (self.handler)(ctx, arguments)
    }
}

/// A reusable typed tool router that can back both CLI and MCP surfaces.
pub struct ToolRouter<Ctx> {
    tools: Vec<Tool<Ctx>>,
}

impl<Ctx> Default for ToolRouter<Ctx> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Ctx> ToolRouter<Ctx> {
    #[must_use]
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    pub fn add_tool(&mut self, tool: Tool<Ctx>) {
        self.tools.push(tool);
    }

    pub fn add_typed_tool<Input, Output, Error, Handler>(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        handler: Handler,
    ) where
        Input: DeserializeOwned + JsonSchema + 'static,
        Output: Serialize + 'static,
        Error: StructuredError + 'static,
        Handler: Fn(&Ctx, Input) -> Result<Output, Error> + Send + Sync + 'static,
    {
        self.add_tool(Tool::new_typed::<Input, Output, Error, Handler>(
            name,
            description,
            handler,
        ));
    }

    #[must_use]
    pub fn tool_metadata(&self) -> Vec<ToolMetadata> {
        self.tools
            .iter()
            .map(|tool| tool.metadata().clone())
            .collect()
    }

    #[must_use]
    pub fn call_tool(&self, ctx: &Ctx, name: &str, arguments: Value) -> JsonEnvelope<Value> {
        match self.tools.iter().find(|tool| tool.metadata().name == name) {
            Some(tool) => tool.call(ctx, arguments),
            None => JsonEnvelope::error_for(
                name,
                JsonError::new(
                    ErrorCategory::Validation,
                    "unknown_tool",
                    format!("unknown tool `{name}`"),
                ),
            ),
        }
    }
}

/// A minimal reusable MCP stdio server for exposing typed tools.
pub struct McpServer<Ctx> {
    config: StdioServerConfig,
    router: ToolRouter<Ctx>,
}

impl<Ctx> McpServer<Ctx> {
    #[must_use]
    pub fn new(config: StdioServerConfig, router: ToolRouter<Ctx>) -> Self {
        Self { config, router }
    }

    #[must_use]
    pub fn tool_metadata(&self) -> Vec<ToolMetadata> {
        self.router.tool_metadata()
    }

    pub fn handle_request_value(
        &self,
        ctx: &Ctx,
        request: Value,
    ) -> Result<Option<Value>, McpCliError> {
        let request: JsonRpcRequest = serde_json::from_value(request)?;
        self.handle_request(ctx, request)
    }

    pub fn serve_stdio(&self, ctx: &Ctx) -> Result<(), McpCliError> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let reader = BufReader::new(stdin.lock());
        let writer = stdout.lock();
        self.serve_transport(ctx, reader, writer)
    }

    pub fn serve_transport<R, W>(
        &self,
        ctx: &Ctx,
        mut reader: R,
        mut writer: W,
    ) -> Result<(), McpCliError>
    where
        R: BufRead,
        W: Write,
    {
        while let Some(message) = read_protocol_message(&mut reader)? {
            let response = self.handle_request_value(ctx, serde_json::from_slice(&message)?)?;
            if let Some(response) = response {
                write_protocol_message(&mut writer, &response)?;
            }
        }

        Ok(())
    }

    fn handle_request(
        &self,
        ctx: &Ctx,
        request: JsonRpcRequest,
    ) -> Result<Option<Value>, McpCliError> {
        let response = match request.method.as_str() {
            "initialize" => request.id.map(|id| {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {
                            "tools": {
                                "listChanged": false
                            }
                        },
                        "serverInfo": {
                            "name": self.config.server_name,
                            "version": self.config.server_version
                        }
                    }
                })
            }),
            "notifications/initialized" => None,
            "ping" => request.id.map(|id| {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {}
                })
            }),
            "tools/list" => request.id.map(|id| {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": self.router.tool_metadata()
                    }
                })
            }),
            "tools/call" => {
                let params: ToolCallParams =
                    serde_json::from_value(request.params.unwrap_or_else(|| json!({})))?;
                let envelope = self.router.call_tool(
                    ctx,
                    &params.name,
                    params.arguments.unwrap_or_else(|| json!({})),
                );
                let structured_content = serde_json::to_value(&envelope)?;
                let text_content = serde_json::to_string(&envelope)?;

                request.id.map(|id| {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": text_content
                                }
                            ],
                            "structuredContent": structured_content,
                            "isError": envelope.is_error()
                        }
                    })
                })
            }
            method => request.id.map(|id| {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("unsupported MCP method `{method}`")
                    }
                })
            }),
        };

        Ok(response)
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Option<Value>,
}

/// Errors surfaced by the reusable CLI/MCP façade.
#[derive(Debug, Error)]
pub enum McpCliError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl McpCliError {
    #[must_use]
    pub const fn category(&self) -> ErrorCategory {
        ErrorCategory::SerializationError
    }
}

fn read_protocol_message<R>(reader: &mut R) -> Result<Option<Vec<u8>>, McpCliError>
where
    R: BufRead,
{
    let mut content_length = None;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            return if content_length.is_none() {
                Ok(None)
            } else {
                Err(McpCliError::Protocol(
                    "unexpected EOF while reading MCP headers".to_string(),
                ))
            };
        }

        if line == "\r\n" || line == "\n" {
            break;
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some((name, value)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            let parsed_length = value.trim().parse::<usize>().map_err(|error| {
                McpCliError::Protocol(format!("invalid Content-Length header: {error}"))
            })?;
            content_length = Some(parsed_length);
        }
    }

    let length = content_length.ok_or_else(|| {
        McpCliError::Protocol("missing Content-Length header in MCP message".to_string())
    })?;
    let mut body = vec![0; length];
    std::io::Read::read_exact(reader, &mut body)?;
    Ok(Some(body))
}

fn write_protocol_message<W>(writer: &mut W, value: &Value) -> Result<(), McpCliError>
where
    W: Write,
{
    let body = serde_json::to_vec(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        EnvelopeMeta, ErrorCategory, JSON_SCHEMA_VERSION, JsonEnvelope, JsonError, McpServer,
        StdioServerConfig, StructuredError, ToolRouter, write_json_result_ref,
    };
    use clap::{Args, Parser, Subcommand};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};
    use thiserror::Error;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Args)]
    struct AddArgs {
        #[arg(long)]
        lhs: i64,
        #[arg(long)]
        rhs: i64,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Args)]
    struct EchoArgs {
        #[arg(long)]
        text: String,

        #[arg(long)]
        uppercase: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Args)]
    struct ReverseArgs {
        #[arg(long)]
        value: String,
    }

    #[derive(Debug, Error)]
    #[error("{message}")]
    struct SampleError {
        category: ErrorCategory,
        message: String,
    }

    impl SampleError {
        fn validation(message: impl Into<String>) -> Self {
            Self {
                category: ErrorCategory::Validation,
                message: message.into(),
            }
        }
    }

    impl StructuredError for SampleError {
        fn category(&self) -> ErrorCategory {
            self.category
        }

        fn code(&self) -> String {
            "sample_validation".to_owned()
        }

        fn message(&self) -> String {
            self.message.clone()
        }
    }

    fn build_math_router() -> ToolRouter<()> {
        let mut router = ToolRouter::new();
        router.add_typed_tool("math_add", "Add two integers.", |(), args: AddArgs| {
            Ok::<_, SampleError>(json!({ "sum": args.lhs + args.rhs }))
        });
        router.add_typed_tool(
            "text_echo",
            "Echo text with optional uppercasing.",
            |(), args: EchoArgs| {
                let rendered = if args.uppercase {
                    args.text.to_uppercase()
                } else {
                    args.text
                };
                Ok::<_, SampleError>(json!({ "text": rendered }))
            },
        );
        router
    }

    fn build_reverse_router() -> ToolRouter<()> {
        let mut router = ToolRouter::new();
        router.add_typed_tool(
            "text_reverse",
            "Reverse a string.",
            |(), args: ReverseArgs| {
                Ok::<_, SampleError>(json!({
                    "reversed": args.value.chars().rev().collect::<String>()
                }))
            },
        );
        router
    }

    #[derive(Debug, Parser)]
    struct MathCli {
        #[arg(long, global = true)]
        json: bool,

        #[command(subcommand)]
        command: MathCommand,
    }

    #[derive(Debug, Subcommand)]
    enum MathCommand {
        Add(AddArgs),
        Echo(EchoArgs),
    }

    #[derive(Debug, Parser)]
    struct ReverseCli {
        #[arg(long, global = true)]
        json: bool,

        #[command(subcommand)]
        command: ReverseCommand,
    }

    #[derive(Debug, Subcommand)]
    enum ReverseCommand {
        Reverse(ReverseArgs),
    }

    fn run_math_cli(args: &[&str]) -> (Result<Value, SampleError>, String) {
        let cli = MathCli::parse_from(args);
        let result = match cli.command {
            MathCommand::Add(input) => {
                if input.lhs < 0 || input.rhs < 0 {
                    Err(SampleError::validation("operands must be non-negative"))
                } else {
                    Ok(json!({ "sum": input.lhs + input.rhs }))
                }
            }
            MathCommand::Echo(input) => Ok(json!({
                "text": if input.uppercase {
                    input.text.to_uppercase()
                } else {
                    input.text
                }
            })),
        };

        let mut output = Vec::new();
        if cli.json {
            write_json_result_ref(&mut output, &result).expect("json output should serialize");
        }

        (
            result,
            String::from_utf8(output).expect("json output should be utf-8"),
        )
    }

    fn run_reverse_cli(args: &[&str]) -> (Result<Value, SampleError>, String) {
        let cli = ReverseCli::parse_from(args);
        let result = match cli.command {
            ReverseCommand::Reverse(input) => Ok(json!({
                "reversed": input.value.chars().rev().collect::<String>()
            })),
        };

        let mut output = Vec::new();
        if cli.json {
            write_json_result_ref(&mut output, &result).expect("json output should serialize");
        }

        (
            result,
            String::from_utf8(output).expect("json output should be utf-8"),
        )
    }

    #[test]
    fn success_envelope_serializes_with_status_tag_and_meta() {
        let envelope = JsonEnvelope::success_for("list", json!({ "crate": "mcp-cli" }));

        let value = serde_json::to_value(envelope).expect("success envelope serializes");

        assert_eq!(value["status"], "success");
        assert_eq!(value["meta"]["schema_version"], JSON_SCHEMA_VERSION);
        assert_eq!(value["meta"]["command"], "list");
        assert_eq!(value["data"]["crate"], "mcp-cli");
    }

    #[test]
    fn error_envelope_serializes_with_structured_category_and_code() {
        let envelope: JsonEnvelope<()> = JsonEnvelope::error_for(
            "capture",
            JsonError::new(
                ErrorCategory::Validation,
                "invalid_target",
                "placeholder validation failure",
            )
            .with_details(json!({ "field": "window" })),
        );

        let value = serde_json::to_value(envelope).expect("error envelope serializes");

        assert_eq!(value["status"], "error");
        assert_eq!(value["meta"]["command"], "capture");
        assert_eq!(value["error"]["category"], "validation");
        assert_eq!(value["error"]["code"], "invalid_target");
        assert_eq!(value["error"]["details"]["field"], "window");
    }

    #[test]
    fn envelope_meta_defaults_are_stable() {
        let meta = EnvelopeMeta::default();

        assert_eq!(meta.schema_version, JSON_SCHEMA_VERSION);
        assert!(meta.command.is_none());
    }

    #[test]
    fn typed_tool_schema_comes_from_the_input_type() {
        let router = build_math_router();
        let tools = router.tool_metadata();
        let add_tool = tools
            .iter()
            .find(|tool| tool.name == "math_add")
            .expect("add tool is registered");

        assert_eq!(add_tool.input_schema["type"], "object");
        assert_eq!(
            add_tool.input_schema["properties"]["lhs"]["type"],
            "integer"
        );
        assert_eq!(
            add_tool.input_schema["properties"]["rhs"]["type"],
            "integer"
        );
    }

    #[test]
    fn router_returns_structured_validation_errors() {
        let router = build_math_router();

        let envelope = router.call_tool(&(), "math_add", json!({ "lhs": 3 }));

        assert!(envelope.is_error());
        let value = serde_json::to_value(envelope).expect("error envelope serializes");
        assert_eq!(value["error"]["code"], "invalid_tool_arguments");
    }

    #[test]
    fn cli_and_router_match_for_primary_and_secondary_command_surfaces() {
        let (_, math_cli_json) =
            run_math_cli(&["math-cli", "--json", "add", "--lhs", "7", "--rhs", "5"]);
        let math_cli_envelope: Value =
            serde_json::from_str(math_cli_json.trim()).expect("math cli emits valid json");
        let math_router_envelope = serde_json::to_value(build_math_router().call_tool(
            &(),
            "math_add",
            json!({ "lhs": 7, "rhs": 5 }),
        ))
        .expect("math router envelope serializes");

        assert_eq!(math_cli_envelope["status"], math_router_envelope["status"]);
        assert_eq!(math_cli_envelope["data"], math_router_envelope["data"]);

        let (_, reverse_cli_json) =
            run_reverse_cli(&["reverse-cli", "--json", "reverse", "--value", "straw"]);
        let reverse_cli_envelope: Value =
            serde_json::from_str(reverse_cli_json.trim()).expect("reverse cli emits valid json");
        let reverse_router_envelope = serde_json::to_value(build_reverse_router().call_tool(
            &(),
            "text_reverse",
            json!({ "value": "straw" }),
        ))
        .expect("reverse router envelope serializes");

        assert_eq!(
            reverse_cli_envelope["data"],
            reverse_router_envelope["data"]
        );
    }

    #[test]
    fn stdio_server_handles_initialize_list_and_call() {
        let server = McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            build_math_router(),
        );

        let input = [
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {}
            })),
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            })),
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "text_echo",
                    "arguments": {
                        "text": "hello",
                        "uppercase": true
                    }
                }
            })),
        ]
        .concat();

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("stdio server should handle framed messages");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0]["result"]["serverInfo"]["name"], "sample-mcp");
        assert!(
            responses[1]["result"]["tools"]
                .as_array()
                .expect("tools list should be an array")
                .iter()
                .any(|tool| tool["name"] == "math_add")
        );
        assert_eq!(
            responses[2]["result"]["structuredContent"]["data"]["text"],
            "HELLO"
        );
        assert_eq!(responses[2]["result"]["isError"], false);
    }

    fn frame_request(value: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(value).expect("request should serialize");
        let mut message = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        message.extend(body);
        message
    }

    fn parse_framed_responses(mut bytes: &[u8]) -> Vec<Value> {
        let mut responses = Vec::new();

        while !bytes.is_empty() {
            let header_end = bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("framed response should contain a header terminator");
            let (header, remainder) = bytes.split_at(header_end + 4);
            let header_str = std::str::from_utf8(header).expect("header should be valid utf-8");
            let length = header_str
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .expect("response should include Content-Length")
                .trim()
                .parse::<usize>()
                .expect("content length should parse");
            let (body, rest) = remainder.split_at(length);
            responses.push(serde_json::from_slice(body).expect("response body should be json"));
            bytes = rest;
        }

        responses
    }
}
