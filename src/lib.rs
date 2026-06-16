//! `mcp-cli` exposes one command implementation through both a traditional CLI
//! JSON surface and a Model Context Protocol (MCP) stdio server. Consumers
//! provide typed inputs, outputs, and [`StructuredError`] values; this crate
//! handles envelopes, JSON schema generation, MCP framing, tool listing, and
//! tool calls.
//!
//! # Minimal pattern
//!
//! ```
//! use mcp_cli::{ErrorCategory, McpServer, StdioServerConfig, StructuredError, ToolRouter};
//! use schemars::JsonSchema;
//! use serde::{Deserialize, Serialize};
//! use serde_json::json;
//!
//! #[derive(Debug, Deserialize, JsonSchema)]
//! struct AddInput {
//!     lhs: i64,
//!     rhs: i64,
//! }
//!
//! #[derive(Debug, Serialize)]
//! struct AddOutput {
//!     sum: i64,
//! }
//!
//! #[derive(Debug)]
//! struct AppError(String);
//!
//! impl StructuredError for AppError {
//!     fn category(&self) -> ErrorCategory { ErrorCategory::Validation }
//!     fn code(&self) -> String { "app_error".to_owned() }
//!     fn message(&self) -> String { self.0.clone() }
//! }
//!
//! let mut router = ToolRouter::new();
//! router.add_typed_tool("math_add", "Add two integers.", |(), input: AddInput| {
//!     Ok::<_, AppError>(AddOutput { sum: input.lhs + input.rhs })
//! });
//!
//! let server = McpServer::new(
//!     StdioServerConfig {
//!         server_name: "my-cli".to_owned(),
//!         server_version: env!("CARGO_PKG_VERSION").to_owned(),
//!     },
//!     router,
//! );
//!
//! let listing = json!({ "tools": server.tool_metadata() });
//! assert_eq!(listing["tools"][0]["name"], "math_add");
//! ```
//!
//! For CLI commands, use [`write_json_result`] or [`write_json_result_ref`] to
//! emit the same stable envelope shape that MCP `tools/call` returns as
//! structured content.

use std::io::{self, BufRead, BufReader, Write};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

/// Stable schema version for JSON envelopes shared by CLI and MCP surfaces.
pub const JSON_SCHEMA_VERSION: u32 = 1;

/// MCP protocol versions this server understands, oldest first. The last entry
/// is the server's preferred (latest) version, advertised when the client's
/// requested version is unsupported or omitted.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

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
        // Recover any `id` before the value is consumed by typed parsing so an
        // Invalid Request response can still reference it (null when absent).
        let recovered_id = request.get("id").cloned().unwrap_or(Value::Null);
        match serde_json::from_value::<JsonRpcRequest>(request) {
            Ok(request) => self.handle_request(ctx, request),
            // A value that parses as JSON but is not a valid JSON-RPC request
            // (e.g. missing `method`) is an Invalid Request. Respond with the
            // JSON-RPC `-32600` error and keep serving rather than tearing the
            // session down. The id is recovered from the raw value when present
            // (null otherwise), as required by JSON-RPC 2.0.
            Err(error) => Ok(Some(invalid_request_response(&recovered_id, &error))),
        }
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
            "initialize" => {
                // Negotiate the protocol version: echo the client's requested
                // version when supported, otherwise advertise our latest. The
                // borrow of `request.params` is released before `request.id` is
                // moved into the response below.
                let protocol_version = negotiate_protocol_version(
                    request
                        .params
                        .as_ref()
                        .and_then(|params| params.get("protocolVersion"))
                        .and_then(Value::as_str),
                );
                request.id.map(|id| {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": protocol_version,
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
                })
            }
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
                match serde_json::from_value::<ToolCallParams>(
                    request.params.unwrap_or_else(|| json!({})),
                ) {
                    Ok(params) => {
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
                    // A `tools/call` whose params do not match the expected shape
                    // (e.g. missing `name`) is Invalid params. Respond with the
                    // JSON-RPC `-32602` error and keep serving instead of
                    // propagating a transport error that drops the session.
                    Err(error) => request.id.map(|id| {
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32602,
                                "message": format!("invalid params: {error}")
                            }
                        })
                    }),
                }
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
        match self {
            Self::Io(_) => ErrorCategory::ExecutionFailure,
            Self::Json(_) => ErrorCategory::SerializationError,
            Self::Protocol(_) => ErrorCategory::Validation,
        }
    }
}

/// Negotiate the MCP protocol version to advertise in the `initialize` result.
///
/// Per the MCP spec the server echoes the client's requested version when it
/// supports it, and otherwise responds with its own latest supported version
/// (also used when the client omits `protocolVersion`), letting the client
/// decide whether to proceed or disconnect.
fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    let latest = SUPPORTED_PROTOCOL_VERSIONS[SUPPORTED_PROTOCOL_VERSIONS.len() - 1];
    match requested {
        Some(requested) => SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .copied()
            .find(|version| *version == requested)
            .unwrap_or(latest),
        None => latest,
    }
}

/// Build a JSON-RPC `-32600 Invalid Request` response for a value that parsed
/// as JSON but is not a valid JSON-RPC request object.
///
/// The `id` should be recovered from the raw request value when present and is
/// `null` otherwise, as required by JSON-RPC 2.0 for Invalid Request errors.
fn invalid_request_response(id: &Value, error: &serde_json::Error) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32600,
            "message": format!("invalid JSON-RPC request: {error}")
        }
    })
}

/// Read one newline-delimited JSON message from an MCP stdio transport.
///
/// The MCP stdio transport frames each JSON-RPC message as a single line of
/// UTF-8 JSON terminated by `\n` (no `Content-Length` headers, no embedded
/// newlines). Blank lines between messages are skipped. Returns `Ok(None)` on a
/// clean end of stream.
fn read_protocol_message<R>(reader: &mut R) -> Result<Option<Vec<u8>>, McpCliError>
where
    R: BufRead,
{
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            return Ok(None);
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            // Tolerate blank separator lines between messages.
            continue;
        }

        return Ok(Some(trimmed.as_bytes().to_vec()));
    }
}

/// Write one newline-delimited JSON message to an MCP stdio transport.
///
/// Emits compact JSON (no embedded newlines) followed by a single `\n`, then
/// flushes so the peer sees the complete message immediately.
fn write_protocol_message<W>(writer: &mut W, value: &Value) -> Result<(), McpCliError>
where
    W: Write,
{
    let body = serde_json::to_vec(value)?;
    writer.write_all(&body)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        EnvelopeMeta, ErrorCategory, JSON_SCHEMA_VERSION, JsonEnvelope, JsonError, McpCliError,
        McpServer, SUPPORTED_PROTOCOL_VERSIONS, StdioServerConfig, StructuredError, ToolRouter,
        read_protocol_message, write_json_result, write_json_result_ref,
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
                "params": { "protocolVersion": "2024-11-05" }
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
        assert_eq!(responses[0]["result"]["serverInfo"]["version"], "0.0.1");
        assert_eq!(responses[0]["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(
            responses[0]["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
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

    #[test]
    fn stdio_server_answers_ping_with_empty_result() {
        let server = McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            build_math_router(),
        );

        let input = frame_request(&json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "ping",
            "params": {}
        }));

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("stdio server should handle a ping request");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["jsonrpc"], "2.0");
        assert_eq!(responses[0]["id"], 42);
        assert_eq!(responses[0]["result"], json!({}));
    }

    #[test]
    fn stdio_server_does_not_respond_to_initialized_notification() {
        let server = McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            build_math_router(),
        );

        let input = frame_request(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("stdio server should accept the initialized notification");

        assert!(
            output.is_empty(),
            "initialized notification must not produce a response"
        );
    }

    #[test]
    fn stdio_server_reports_unknown_method_as_method_not_found() {
        let server = McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            build_math_router(),
        );

        let input = frame_request(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "does/not/exist",
            "params": {}
        }));

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("stdio server should handle an unknown method");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["id"], 7);
        assert_eq!(responses[0]["error"]["code"], -32601);
        assert!(
            responses[0]["error"]["message"]
                .as_str()
                .expect("error message should be a string")
                .contains("does/not/exist")
        );
    }

    #[test]
    fn read_protocol_message_reads_one_newline_delimited_line() {
        let mut input = std::io::Cursor::new(b"{\"jsonrpc\":\"2.0\"}\n".to_vec());

        let message = read_protocol_message(&mut input)
            .expect("a newline-delimited line should read cleanly")
            .expect("a message should be present");

        assert_eq!(message, b"{\"jsonrpc\":\"2.0\"}");
    }

    #[test]
    fn read_protocol_message_skips_blank_separator_lines() {
        let mut input = std::io::Cursor::new(b"\n\r\n{\"id\":1}\n".to_vec());

        let message = read_protocol_message(&mut input)
            .expect("blank lines should be skipped")
            .expect("a message should follow the blank lines");

        assert_eq!(message, b"{\"id\":1}");
    }

    #[test]
    fn read_protocol_message_returns_raw_line_for_non_json_text() {
        // The reader is framing-only: it returns the raw line and lets the serve
        // layer reject non-JSON, matching the MCP stdio NDJSON transport.
        let mut input = std::io::Cursor::new(b"this is not json\n".to_vec());

        let message = read_protocol_message(&mut input)
            .expect("reading a line should not itself fail")
            .expect("the raw line should be returned");

        assert_eq!(message, b"this is not json");
    }

    #[test]
    fn read_protocol_message_returns_none_on_clean_eof() {
        let mut input = std::io::Cursor::new(Vec::new());

        let message = read_protocol_message(&mut input).expect("clean EOF should not be an error");

        assert!(message.is_none());
    }

    #[test]
    fn write_json_result_emits_success_envelope_with_trailing_newline() {
        let mut output = Vec::new();
        let result: Result<Value, SampleError> = Ok(json!({ "sum": 12 }));

        write_json_result(&mut output, result).expect("json result should serialize");

        let rendered = String::from_utf8(output).expect("json output should be utf-8");
        assert!(rendered.ends_with('\n'), "output should end with a newline");
        let value: Value =
            serde_json::from_str(rendered.trim()).expect("output should be valid json");
        assert_eq!(value["status"], "success");
        assert_eq!(value["data"]["sum"], 12);
    }

    #[test]
    fn write_json_result_emits_error_envelope_for_structured_error() {
        let mut output = Vec::new();
        let result: Result<Value, SampleError> = Err(SampleError::validation("bad input"));

        write_json_result(&mut output, result).expect("json error result should serialize");

        let rendered = String::from_utf8(output).expect("json output should be utf-8");
        let value: Value =
            serde_json::from_str(rendered.trim()).expect("output should be valid json");
        assert_eq!(value["status"], "error");
        assert_eq!(value["error"]["category"], "validation");
        assert_eq!(value["error"]["message"], "bad input");
    }

    #[test]
    fn success_envelope_round_trips_through_serde() {
        let original = JsonEnvelope::success_for("list", json!({ "crate": "mcp-cli" }));

        let encoded = serde_json::to_string(&original).expect("success envelope serializes");
        let decoded: JsonEnvelope<Value> =
            serde_json::from_str(&encoded).expect("success envelope deserializes");

        assert_eq!(decoded, original);
        match decoded {
            JsonEnvelope::Success { meta, data } => {
                assert_eq!(meta.command.as_deref(), Some("list"));
                assert_eq!(meta.schema_version, JSON_SCHEMA_VERSION);
                assert_eq!(data["crate"], "mcp-cli");
            }
            JsonEnvelope::Error { .. } => panic!("expected success variant after round-trip"),
        }
    }

    #[test]
    fn error_envelope_round_trips_through_serde() {
        let original: JsonEnvelope<Value> = JsonEnvelope::error_for(
            "capture",
            JsonError::new(
                ErrorCategory::Validation,
                "invalid_target",
                "placeholder validation failure",
            )
            .with_details(json!({ "field": "window" })),
        );

        let encoded = serde_json::to_string(&original).expect("error envelope serializes");
        let decoded: JsonEnvelope<Value> =
            serde_json::from_str(&encoded).expect("error envelope deserializes");

        assert_eq!(decoded, original);
        match decoded {
            JsonEnvelope::Error { meta, error } => {
                assert_eq!(meta.command.as_deref(), Some("capture"));
                assert_eq!(error.category, ErrorCategory::Validation);
                assert_eq!(error.code, "invalid_target");
                assert_eq!(error.message, "placeholder validation failure");
                assert_eq!(
                    error.details.expect("details should survive round-trip")["field"],
                    "window"
                );
            }
            JsonEnvelope::Success { .. } => panic!("expected error variant after round-trip"),
        }
    }

    #[test]
    fn stdio_server_rejects_non_json_input_instead_of_hanging() {
        // Regression: typing arbitrary text into the stdio transport must surface
        // a JSON error rather than silently consuming it (which previously hung).
        let server = McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            build_math_router(),
        );

        let mut output = Vec::new();
        let result = server.serve_transport(
            &(),
            std::io::Cursor::new(b"hello there\n".to_vec()),
            &mut output,
        );

        match result {
            Err(McpCliError::Json(_)) => {}
            other => panic!("expected a JSON parse error on garbage input, got {other:?}"),
        }
    }

    #[test]
    fn stdio_server_surfaces_tool_call_errors_as_is_error() {
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
                "method": "tools/call",
                "params": {
                    "name": "does_not_exist",
                    "arguments": {}
                }
            })),
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "math_add",
                    "arguments": { "lhs": 3 }
                }
            })),
        ]
        .concat();

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("stdio server should handle failing tool calls");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 2);

        // Unknown tool name surfaces as a structured error, not a transport failure.
        assert_eq!(responses[0]["result"]["isError"], true);
        assert_eq!(
            responses[0]["result"]["structuredContent"]["status"],
            "error"
        );
        assert_eq!(
            responses[0]["result"]["structuredContent"]["error"]["code"],
            "unknown_tool"
        );

        // Arguments that fail typed validation also surface as isError with a
        // validation error envelope embedded in structuredContent.
        assert_eq!(responses[1]["result"]["isError"], true);
        assert_eq!(
            responses[1]["result"]["structuredContent"]["error"]["code"],
            "invalid_tool_arguments"
        );
        assert_eq!(
            responses[1]["result"]["structuredContent"]["error"]["category"],
            "validation"
        );
    }

    #[test]
    fn stdio_server_invalid_request_object_returns_invalid_request_and_keeps_serving() {
        // A value that parses as JSON but is not a valid JSON-RPC request (no
        // `method`) must produce a `-32600` error and the session must keep
        // serving subsequent valid requests rather than tearing down.
        let server = McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            build_math_router(),
        );

        let input = [
            // Invalid request with a recoverable id.
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 9,
                "foo": "bar"
            })),
            // Invalid request with no id at all: response id must be null.
            frame_request(&json!({
                "jsonrpc": "2.0",
                "foo": "bar"
            })),
            // A normal request that must still be answered after the bad ones.
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "ping",
                "params": {}
            })),
        ]
        .concat();

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("invalid request objects must not tear down the session");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 3);

        assert_eq!(responses[0]["id"], 9);
        assert_eq!(responses[0]["error"]["code"], -32600);

        assert_eq!(responses[1]["id"], Value::Null);
        assert_eq!(responses[1]["error"]["code"], -32600);

        // The session survived and answered the following valid request.
        assert_eq!(responses[2]["id"], 5);
        assert_eq!(responses[2]["result"], json!({}));
    }

    #[test]
    fn stdio_server_invalid_tool_call_params_returns_invalid_params_and_keeps_serving() {
        // A `tools/call` whose params do not match the expected shape (missing
        // `name`) must produce a `-32602` error and the session must keep
        // serving rather than propagating a transport error.
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
                "method": "tools/call",
                "params": {
                    "arguments": { "lhs": 1, "rhs": 2 }
                }
            })),
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "ping",
                "params": {}
            })),
        ]
        .concat();

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("invalid tool-call params must not tear down the session");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 2);

        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["error"]["code"], -32602);
        assert!(
            responses[0]["error"]["message"]
                .as_str()
                .expect("error message should be a string")
                .contains("invalid params")
        );

        // The session survived and answered the following valid request.
        assert_eq!(responses[1]["id"], 2);
        assert_eq!(responses[1]["result"], json!({}));
    }

    #[test]
    fn stdio_server_initialize_negotiates_protocol_version() {
        // The server should echo a supported requested version, fall back to its
        // latest supported version for an unsupported request, and use the
        // latest when the client omits `protocolVersion`.
        let server = McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            build_math_router(),
        );

        let latest = SUPPORTED_PROTOCOL_VERSIONS[SUPPORTED_PROTOCOL_VERSIONS.len() - 1];
        let supported = SUPPORTED_PROTOCOL_VERSIONS[0];

        let input = [
            // Supported requested version is echoed back verbatim.
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": supported }
            })),
            // Unsupported requested version falls back to the latest supported.
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "initialize",
                "params": { "protocolVersion": "1999-01-01" }
            })),
            // Omitted version defaults to the latest supported.
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "initialize",
                "params": {}
            })),
        ]
        .concat();

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("stdio server should handle initialize negotiation");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0]["result"]["protocolVersion"], supported);
        assert_eq!(responses[1]["result"]["protocolVersion"], latest);
        assert_eq!(responses[2]["result"]["protocolVersion"], latest);
    }

    #[test]
    fn mcp_cli_error_category_reflects_each_variant() {
        let io_error = McpCliError::Io(std::io::Error::other("boom"));
        assert_eq!(io_error.category(), ErrorCategory::ExecutionFailure);

        let json_error = McpCliError::Json(
            serde_json::from_str::<Value>("{").expect_err("malformed json should fail to parse"),
        );
        assert_eq!(json_error.category(), ErrorCategory::SerializationError);

        let protocol_error = McpCliError::Protocol("bad frame".to_string());
        assert_eq!(protocol_error.category(), ErrorCategory::Validation);
    }

    fn frame_request(value: &Value) -> Vec<u8> {
        let mut message = serde_json::to_vec(value).expect("request should serialize");
        message.push(b'\n');
        message
    }

    fn parse_framed_responses(bytes: &[u8]) -> Vec<Value> {
        let text = std::str::from_utf8(bytes).expect("responses should be valid utf-8");
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("response line should be json"))
            .collect()
    }
}
