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

use std::io::{self, BufRead, BufReader, Read, Write};
use std::panic::{self, AssertUnwindSafe};
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

/// Default cap on the size of a single newline-delimited MCP stdio frame.
///
/// The stdio transport has no length prefix, so a peer that never emits a
/// newline would otherwise force the server to buffer without bound. Frames
/// larger than this are rejected with a JSON-RPC parse error instead of being
/// accumulated in memory. Override per server with
/// [`McpServer::with_max_frame_bytes`].
pub const DEFAULT_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Chunk size used to drain the remainder of an oversized frame.
///
/// Draining in bounded chunks lets the transport resynchronise on the next
/// frame boundary without buffering the discarded bytes.
const OVERSIZED_DRAIN_CHUNK_BYTES: usize = 64 * 1024;

/// Stable categories for structured JSON and MCP errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    /// Optional JSON Schema for the tool's structured output (`structuredContent`),
    /// advertised to MCP clients per the 2025-06-18 revision. `None` when the tool
    /// was registered without an output schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
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
            output_schema: None,
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

    /// Like [`Tool::new_typed`], but also advertises an `outputSchema` in the
    /// tool metadata so MCP clients can validate structured results (MCP
    /// 2025-06-18). Because `tools/call` returns `structuredContent` as the
    /// [`JsonEnvelope`] wrapping `Output`, the advertised schema describes that
    /// envelope (`status`/`meta`/`data`), so structuredContent conforms to it.
    /// Opt-in: this requires `Output: JsonSchema`, so existing `new_typed`
    /// callers are unaffected.
    #[must_use]
    pub fn new_typed_with_output_schema<Input, Output, Error, Handler>(
        name: impl Into<String>,
        description: impl Into<String>,
        handler: Handler,
    ) -> Self
    where
        Input: DeserializeOwned + JsonSchema + 'static,
        Output: Serialize + JsonSchema + 'static,
        Error: StructuredError + 'static,
        Handler: Fn(&Ctx, Input) -> Result<Output, Error> + Send + Sync + 'static,
    {
        let mut tool = Self::new_typed::<Input, Output, Error, Handler>(name, description, handler);
        // structuredContent is always the JsonEnvelope wrapping `Output`, so the
        // advertised outputSchema must describe that envelope (status/meta/data),
        // not the bare `Output` — otherwise a client validating structuredContent
        // against outputSchema would reject conformant responses (bd-870183).
        tool.metadata.output_schema = Some(envelope_output_schema::<Output>());
        tool
    }

    #[must_use]
    pub fn metadata(&self) -> &ToolMetadata {
        &self.metadata
    }

    /// Invoke the tool, converting a panic in the handler into a tool-level
    /// error rather than letting it end the process.
    ///
    /// The server is mid-obligation when a handler runs: a client is blocking on
    /// a JSON-RPC id, further requests are already buffered behind this one, and
    /// the session is expected to outlive the call. An unwinding panic breaks all
    /// three silently — the peer sees a closed pipe with no code, no message, and
    /// no indication of which tool failed. Catching here is not about owning the
    /// call site (`Iterator::map` owns one too and rightly does not catch); it is
    /// that this call site has an unfulfilled promise to a third party, which is
    /// the same reason `axum` and `actix` catch per request.
    ///
    /// Reporting the panic as `tool_panicked` makes the bug *more* visible than
    /// the status quo, not less: the client learns which tool failed and why, and
    /// the default panic hook still writes the message and location to stderr.
    ///
    /// Two limits, both real, neither of which this can fix:
    ///
    /// - It does nothing under `panic = "abort"`, where there is no unwind to
    ///   catch. This is a mitigation, not a guarantee.
    /// - It does not restore consistency. A caught panic leaves `Ctx` exactly as
    ///   the handler left it, so a half-updated cache or partly-applied batch
    ///   survives into subsequent calls. `Mutex` poisoning surfaces that; a
    ///   `RefCell` or atomics based `Ctx` will not. `tool_panicked` therefore
    ///   reports a bug to FIX, not a condition to handle.
    #[must_use]
    pub fn call(&self, ctx: &Ctx, arguments: Value) -> JsonEnvelope<Value> {
        // `AssertUnwindSafe` is forced here, not chosen for convenience. The
        // closure captures `&Ctx`, which is `UnwindSafe` only when
        // `Ctx: RefUnwindSafe`, and `&Tool<Ctx>`, which never is: the handler is
        // an `Arc<dyn Fn(..)>` and `dyn Fn` is not `RefUnwindSafe`. Adding a
        // `Ctx: RefUnwindSafe` bound therefore would not compile either, and
        // would exclude every `RefCell`-based single-threaded `Ctx` for nothing.
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| (self.handler)(ctx, arguments)));

        match outcome {
            Ok(envelope) => envelope,
            Err(payload) => JsonEnvelope::error_for(
                self.metadata.name.clone(),
                JsonError::new(
                    ErrorCategory::ExecutionFailure,
                    "tool_panicked",
                    format!(
                        "tool `{}` panicked: {}",
                        self.metadata.name,
                        panic_message(&payload)
                    ),
                ),
            ),
        }
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

    /// Register a tool, replacing nothing: tool names must be unique.
    ///
    /// Only the router's own invariants are enforced: a name must identify
    /// exactly one tool, and the input schema must be one MCP accepts. Charset
    /// and length limits imposed by a downstream host are that consumer's
    /// concern — layer them over [`ToolRouter::try_add_tool`].
    ///
    /// # Panics
    ///
    /// Panics if the name is empty or whitespace-only, if a tool with the same
    /// name is already registered, or if the tool's input schema declares a
    /// non-object root type. Registration is a startup-time concern, and each is
    /// a programming error that would otherwise be reported far from its cause:
    /// an unnameable tool surfaces as `TargetNotFound` at call time in the
    /// client, a duplicate name leaves the second registration unreachable, and
    /// a non-object input schema ships metadata MCP clients reject. Use
    /// [`ToolRouter::try_add_tool`] to handle any of them instead.
    pub fn add_tool(&mut self, tool: Tool<Ctx>) {
        if let Err(error) = self.try_add_tool(tool) {
            panic!("{}", error.message());
        }
    }

    /// Register a tool, returning a structured error if the name is already
    /// taken or the tool advertises an input schema MCP cannot accept.
    ///
    /// Use this when the tool set is assembled dynamically (from config, a
    /// plugin set, or user input) and a collision should be reported rather
    /// than abort the process.
    ///
    /// # Errors
    ///
    /// Returns a [`JsonError`] with code `invalid_tool_name` when the name is
    /// empty or whitespace-only, `invalid_input_schema` when the tool's input
    /// schema declares a non-object root type, or `duplicate_tool_name` when a
    /// tool with the same name is already registered. The router is left
    /// unchanged in every case.
    pub fn try_add_tool(&mut self, tool: Tool<Ctx>) -> Result<(), JsonError> {
        let name = &tool.metadata().name;
        if name.trim().is_empty() {
            return Err(JsonError::new(
                ErrorCategory::Validation,
                "invalid_tool_name",
                "a tool name must not be empty or whitespace-only".to_owned(),
            ));
        }

        if let Some(error) = non_object_input_schema_error(name, &tool.metadata().input_schema) {
            return Err(error);
        }

        if self
            .tools
            .iter()
            .any(|existing| &existing.metadata().name == name)
        {
            return Err(JsonError::new(
                ErrorCategory::Validation,
                "duplicate_tool_name",
                format!("tool `{name}` is already registered"),
            ));
        }

        self.tools.push(tool);
        Ok(())
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

    /// Like [`ToolRouter::add_typed_tool`], but advertises an `outputSchema` for
    /// the tool's `Output`. Opt-in: requires `Output: JsonSchema`.
    pub fn add_typed_tool_with_output_schema<Input, Output, Error, Handler>(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        handler: Handler,
    ) where
        Input: DeserializeOwned + JsonSchema + 'static,
        Output: Serialize + JsonSchema + 'static,
        Error: StructuredError + 'static,
        Handler: Fn(&Ctx, Input) -> Result<Output, Error> + Send + Sync + 'static,
    {
        self.add_tool(Tool::new_typed_with_output_schema::<
            Input,
            Output,
            Error,
            Handler,
        >(name, description, handler));
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
                    // A missing tool name is a not-found condition, not malformed
                    // input: classify it as TargetNotFound so consumers routing on
                    // ErrorCategory can distinguish it from validation errors.
                    ErrorCategory::TargetNotFound,
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
    max_frame_bytes: usize,
}

impl<Ctx> McpServer<Ctx> {
    #[must_use]
    pub fn new(config: StdioServerConfig, router: ToolRouter<Ctx>) -> Self {
        Self {
            config,
            router,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
        }
    }

    /// Override the largest single stdio frame this server will buffer.
    ///
    /// Defaults to [`DEFAULT_MAX_FRAME_BYTES`]. A frame larger than the cap is
    /// answered with a JSON-RPC parse error and skipped; the session keeps
    /// serving. A cap of `0` is raised to 1 byte so a frame boundary can still
    /// be found.
    #[must_use]
    pub const fn with_max_frame_bytes(mut self, max_frame_bytes: usize) -> Self {
        self.max_frame_bytes = if max_frame_bytes == 0 {
            1
        } else {
            max_frame_bytes
        };
        self
    }

    /// The largest single stdio frame this server will buffer, in bytes.
    #[must_use]
    pub const fn max_frame_bytes(&self) -> usize {
        self.max_frame_bytes
    }

    #[must_use]
    pub fn tool_metadata(&self) -> Vec<ToolMetadata> {
        self.router.tool_metadata()
    }

    /// Handle one parsed JSON-RPC value: a single request object, or a batch
    /// array of them.
    ///
    /// Batches are answered as an array of the member responses in request
    /// order, with notification members omitted; a notification-only batch
    /// produces no response at all (`Ok(None)`). An empty array is itself an
    /// Invalid Request and is answered with a single `-32600` object rather
    /// than an array, as required by JSON-RPC 2.0 section 6.
    pub fn handle_request_value(
        &self,
        ctx: &Ctx,
        request: Value,
    ) -> Result<Option<Value>, McpCliError> {
        // MCP 2025-03-26 requires implementations to be able to RECEIVE JSON-RPC
        // batches; 2025-06-18 removed batching. Accepting a batch on any
        // negotiated version is the tolerant, simpler choice: a client that
        // never batches is unaffected, and a 2025-03-26 client is not answered
        // with a single error for an entire warm-up batch.
        if let Value::Array(members) = request {
            return self.handle_batch(ctx, members);
        }

        self.handle_single_request_value(ctx, request)
    }

    fn handle_batch(&self, ctx: &Ctx, members: Vec<Value>) -> Result<Option<Value>, McpCliError> {
        if members.is_empty() {
            // JSON-RPC 2.0 section 6: an empty batch array is an Invalid
            // Request answered with a single response object, not an array.
            return Ok(Some(empty_batch_response()));
        }

        let mut responses = Vec::with_capacity(members.len());
        for member in members {
            if let Some(response) = self.handle_single_request_value(ctx, member)? {
                responses.push(response);
            }
        }

        // A batch of only notifications produces no response frame at all.
        if responses.is_empty() {
            return Ok(None);
        }

        Ok(Some(Value::Array(responses)))
    }

    fn handle_single_request_value(
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

    /// Serve the MCP protocol over this process's stdin/stdout.
    ///
    /// # stdout is the protocol channel
    ///
    /// Everything this process writes to stdout is framed as protocol output.
    /// That makes the hazard broader than a stray `println!` in a handler — it
    /// is ANYTHING the consumer installs that can reach file descriptor 1:
    ///
    /// - a `println!` or `dbg!` in a tool handler, or a dependency that logs to
    ///   stdout;
    /// - a `tracing`/`log` subscriber left on its default sink, which is
    ///   commonly stdout — the case most likely to bite, since nobody wires a
    ///   `println!` into a handler on purpose but plenty of people wire a logger
    ///   without thinking about where it lands;
    /// - a CUSTOM PANIC HOOK that writes to stdout. Rust's default hook prints
    ///   to stderr, which is correct and is part of why a caught handler panic
    ///   is more visible rather than less; a hook redirected to stdout instead
    ///   corrupts the stream at exactly the moment a tool is failing, and the
    ///   garbage frame arrives interleaved with the error response for that same
    ///   request.
    ///
    /// Any of them is emitted into the stream as its own frame:
    ///
    /// ```text
    /// DEBUG: about to do work                       <- a println! in a handler
    /// {"id":1,"jsonrpc":"2.0","result":{ ... }}     <- the actual response
    /// ```
    ///
    /// The observable symptom is a client reporting a non-JSON frame, or simply
    /// dropping the session: many MCP clients treat unparseable server output as
    /// fatal. Route ALL consumer output to stderr. This crate cannot enforce
    /// that: redirecting the file descriptor would need `unsafe`, which the
    /// crate forbids.
    pub fn serve_stdio(&self, ctx: &Ctx) -> Result<(), McpCliError> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let reader = BufReader::new(stdin.lock());
        let writer = stdout.lock();
        self.serve_transport(ctx, reader, writer)
    }

    /// Serve the MCP protocol over an arbitrary reader/writer pair.
    ///
    /// The writer receives protocol frames only. When that writer is this
    /// process's stdout — as with [`McpServer::serve_stdio`] — anything else the
    /// process prints to stdout is interleaved into the protocol stream and
    /// corrupts it; see that method's warning. Consumer logging belongs on
    /// stderr.
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
        loop {
            match read_protocol_message(&mut reader, self.max_frame_bytes)? {
                ProtocolFrame::Eof => break,
                // The frame exceeded the configured cap, so it was never
                // buffered in full and cannot be parsed. Report it like any
                // other unparseable frame and carry on with the next one.
                ProtocolFrame::Oversized(bytes) => {
                    let response = oversized_frame_response(bytes, self.max_frame_bytes);
                    write_protocol_message(&mut writer, &response)?;
                }
                ProtocolFrame::Message(message) => {
                    // A frame that is not valid JSON at all (including invalid
                    // UTF-8) is a JSON-RPC Parse error. Answer with `-32700` and
                    // keep serving: a single malformed line from a noisy or
                    // buggy peer must not tear down a long-lived stdio session,
                    // consistent with the `-32600` / `-32602` handling below.
                    let response = match serde_json::from_slice::<Value>(&message) {
                        Ok(value) => self.handle_request_value(ctx, value)?,
                        Err(error) => Some(parse_error_response(&error)),
                    };
                    if let Some(response) = response {
                        write_protocol_message(&mut writer, &response)?;
                    }
                }
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
            "initialize" => self.handle_initialize(request.id, request.params.as_ref()),
            // A notification carries no id and gets no response. If a client
            // does send an id, JSON-RPC 2.0 makes this a request that MUST be
            // answered, so reply with an empty result exactly as `ping` does
            // rather than leaving the peer waiting on a response that never
            // comes. `-32601` would be wrong here: the method is supported.
            "notifications/initialized" => request.id.map(|id| {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {}
                })
            }),
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
            "tools/call" => self.handle_tool_call(ctx, request.id, request.params)?,
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

    /// Answer `initialize`, negotiating the protocol version.
    ///
    /// The client's requested version is echoed when supported, otherwise the
    /// server advertises its latest.
    fn handle_initialize(&self, id: Option<Value>, params: Option<&Value>) -> Option<Value> {
        let protocol_version = negotiate_protocol_version(
            params
                .and_then(|params| params.get("protocolVersion"))
                .and_then(Value::as_str),
        );

        id.map(|id| {
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

    /// Answer `tools/call`, returning both `structuredContent` and a `text`
    /// content block, with `isError` reflecting tool failure.
    fn handle_tool_call(
        &self,
        ctx: &Ctx,
        id: Option<Value>,
        params: Option<Value>,
    ) -> Result<Option<Value>, McpCliError> {
        let params =
            match serde_json::from_value::<ToolCallParams>(params.unwrap_or_else(|| json!({}))) {
                Ok(params) => params,
                // A `tools/call` whose params do not match the expected shape (e.g.
                // missing `name`) is Invalid params. Respond with the JSON-RPC
                // `-32602` error and keep serving instead of propagating a transport
                // error that drops the session.
                Err(error) => {
                    return Ok(id.map(|id| {
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32602,
                                "message": format!("invalid params: {error}")
                            }
                        })
                    }));
                }
            };

        let envelope = self.router.call_tool(
            ctx,
            &params.name,
            params.arguments.unwrap_or_else(|| json!({})),
        );
        let structured_content = serde_json::to_value(&envelope)?;
        let text_content = serde_json::to_string(&envelope)?;

        Ok(id.map(|id| {
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
        }))
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

/// Recover a human-readable message from a caught panic payload.
///
/// `panic!` payloads are `&'static str` for a literal and `String` once
/// formatted; anything else is reported opaquely rather than guessed at.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload.downcast_ref::<&'static str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "panic payload of unknown type".to_owned())
        },
        |message| (*message).to_owned(),
    )
}

/// Derive the advertised `outputSchema` for a tool returning `Output`.
///
/// `structuredContent` is always the [`JsonEnvelope`] wrapping `Output`, so the
/// schema describes that envelope. Because the envelope is an internally-tagged
/// enum, the derived document is rooted in `oneOf` with no declared `type`,
/// which MCP 2025-06-18 does not accept: `outputSchema` must declare a root
/// `"type": "object"`. Both branches of the `oneOf` are objects, so declaring it
/// at the root rejects nothing that was previously valid while making the
/// document conformant, and the success/error discrimination is preserved.
fn envelope_output_schema<Output>() -> Value
where
    Output: JsonSchema,
{
    let mut schema = serde_json::to_value(schemars::schema_for!(JsonEnvelope<Output>))
        .expect("tool output schema should serialize");

    if let Some(root) = schema.as_object_mut() {
        root.entry("type")
            .or_insert_with(|| Value::String("object".to_owned()));
    }

    schema
}

/// Reject an input schema whose root declares a type MCP cannot accept.
///
/// MCP constrains `Tool.inputSchema` to a JSON Schema object, but a derived
/// schema follows the input type: a scalar, sequence, or enum input yields a
/// root of `"string"`, `"array"`, and so on, which strict clients reject.
///
/// The check is deliberately conservative: only a root that *declares* a
/// non-object `type` is rejected. A schema that omits `type`, or expresses the
/// object through `$ref` or a combinator, is left alone rather than guessed at.
fn non_object_input_schema_error(name: &str, input_schema: &Value) -> Option<JsonError> {
    let declared = input_schema.get("type")?;
    let describes_object = match declared {
        Value::String(kind) => kind == "object",
        Value::Array(kinds) => kinds.iter().any(|kind| kind == "object"),
        _ => false,
    };

    if describes_object {
        return None;
    }

    Some(JsonError::new(
        ErrorCategory::Validation,
        "invalid_input_schema",
        format!(
            "tool `{name}` advertises an inputSchema with root type {declared}; \
             MCP requires inputSchema to be a JSON Schema object, so the tool's \
             input type should be a struct"
        ),
    ))
}

/// Build a JSON-RPC `-32700 Parse error` response for a frame that exceeded the
/// configured size cap.
///
/// The frame was never buffered in full, so no id is recoverable and the id is
/// `null`, as for any other parse failure.
fn oversized_frame_response(discarded_bytes: usize, max_frame_bytes: usize) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": {
            "code": -32700,
            "message": format!(
                "parse error: frame exceeds the {max_frame_bytes}-byte limit \
                 ({discarded_bytes} bytes discarded)"
            )
        }
    })
}

/// Build a JSON-RPC `-32700 Parse error` response for a frame that is not
/// valid JSON.
///
/// The `id` is always `null`: the request could not be parsed, so no id is
/// recoverable from it, as required by JSON-RPC 2.0.
fn parse_error_response(error: &serde_json::Error) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": {
            "code": -32700,
            "message": format!("parse error: {error}")
        }
    })
}

/// Build the JSON-RPC `-32600 Invalid Request` response for an empty batch
/// array.
///
/// JSON-RPC 2.0 section 6 specifies a single response object here (not an
/// array) with a null id, because there is no member to answer.
fn empty_batch_response() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": {
            "code": -32600,
            "message": "invalid JSON-RPC request: batch array is empty"
        }
    })
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

/// One framing outcome from the MCP stdio transport.
#[derive(Debug)]
enum ProtocolFrame {
    /// A complete frame, with its newline terminator trimmed.
    Message(Vec<u8>),
    /// A frame that exceeded the size cap. The payload is discarded up to the
    /// next frame boundary; the value is the number of bytes dropped.
    Oversized(usize),
    /// Clean end of stream.
    Eof,
}

/// Read one newline-delimited JSON message from an MCP stdio transport.
///
/// The MCP stdio transport frames each JSON-RPC message as a single line of
/// UTF-8 JSON terminated by `\n` (no `Content-Length` headers, no embedded
/// newlines). Blank lines between messages are skipped.
///
/// The frame is read as raw bytes and never buffers more than
/// `max_frame_bytes` for one message: the transport has no length prefix, so a
/// peer that never emits a newline would otherwise force an unbounded
/// allocation. Bytes are returned unvalidated — UTF-8 and JSON validity are the
/// serve layer's concern, so a non-UTF-8 frame becomes an ordinary parse error
/// rather than an I/O failure that ends the session.
fn read_protocol_message<R>(
    reader: &mut R,
    max_frame_bytes: usize,
) -> Result<ProtocolFrame, McpCliError>
where
    R: BufRead,
{
    // One extra byte of headroom so a frame of exactly `max_frame_bytes` can
    // still be read together with its terminator.
    let limit = (max_frame_bytes as u64).saturating_add(1);
    let mut frame = Vec::new();

    loop {
        frame.clear();
        let bytes_read = Read::take(&mut *reader, limit).read_until(b'\n', &mut frame)?;
        if bytes_read == 0 {
            return Ok(ProtocolFrame::Eof);
        }

        // A frame with no terminator is oversized only when the cap stopped the
        // read; otherwise the stream simply ended without a trailing newline and
        // the remainder is a complete final frame.
        if frame.last() != Some(&b'\n') && bytes_read as u64 == limit {
            // The cap was reached before any terminator: drop the rest of the
            // line so the next frame boundary is still recoverable.
            let discarded = bytes_read.saturating_add(drain_to_frame_boundary(reader)?);
            return Ok(ProtocolFrame::Oversized(discarded));
        }

        let trimmed = trim_frame_terminator(&frame);
        if trimmed.is_empty() {
            // Tolerate blank separator lines between messages.
            continue;
        }

        return Ok(ProtocolFrame::Message(trimmed.to_vec()));
    }
}

/// Discard bytes up to and including the next newline, in bounded chunks.
///
/// Returns the number of bytes discarded. A clean end of stream ends the drain.
fn drain_to_frame_boundary<R>(reader: &mut R) -> Result<usize, McpCliError>
where
    R: BufRead,
{
    let mut discarded = 0usize;
    let mut chunk = Vec::new();

    loop {
        chunk.clear();
        let bytes_read = Read::take(&mut *reader, OVERSIZED_DRAIN_CHUNK_BYTES as u64)
            .read_until(b'\n', &mut chunk)?;
        if bytes_read == 0 {
            return Ok(discarded);
        }

        discarded = discarded.saturating_add(bytes_read);
        if chunk.last() == Some(&b'\n') {
            return Ok(discarded);
        }
    }
}

/// Trim a single `\n` terminator and any preceding `\r` from a raw frame.
fn trim_frame_terminator(frame: &[u8]) -> &[u8] {
    let frame = frame.strip_suffix(b"\n").unwrap_or(frame);
    frame.strip_suffix(b"\r").unwrap_or(frame)
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
        DEFAULT_MAX_FRAME_BYTES, EnvelopeMeta, ErrorCategory, JSON_SCHEMA_VERSION, JsonEnvelope,
        JsonError, McpCliError, McpServer, ProtocolFrame, SUPPORTED_PROTOCOL_VERSIONS,
        StdioServerConfig, StructuredError, Tool, ToolRouter, read_protocol_message,
        write_json_result, write_json_result_ref,
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
    fn router_rejects_a_tool_whose_input_schema_is_not_an_object() {
        // MCP constrains inputSchema to a JSON Schema object. A scalar, sequence
        // or enum input derives a root of "string" / "array" / etc, which strict
        // clients reject — and mcp-cli previously advertised it without comment.
        for (name, error) in [
            (
                "scalar",
                ToolRouter::<()>::new()
                    .try_add_tool_for_test::<String>("scalar")
                    .expect_err("a string input schema should be rejected"),
            ),
            (
                "sequence",
                ToolRouter::<()>::new()
                    .try_add_tool_for_test::<Vec<i64>>("sequence")
                    .expect_err("an array input schema should be rejected"),
            ),
            (
                "choice",
                ToolRouter::<()>::new()
                    .try_add_tool_for_test::<SampleChoice>("choice")
                    .expect_err("an enum input schema should be rejected"),
            ),
        ] {
            assert_eq!(error.code(), "invalid_input_schema", "for {name}");
            assert_eq!(error.category(), ErrorCategory::Validation, "for {name}");
            assert!(error.message().contains(name), "for {name}");
        }
    }

    #[test]
    fn router_accepts_a_struct_input_schema_including_nested_refs() {
        // A nested struct derives `$defs` + `$ref` but keeps a `type: object`
        // root, which is conformant and must not be rejected.
        let mut router = ToolRouter::<()>::new();
        router
            .try_add_tool_for_test::<SampleNested>("nested")
            .expect("a struct input schema should register");

        let metadata = router.tool_metadata();
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].input_schema["type"], "object");
        assert!(
            metadata[0].input_schema["properties"]["inner"]["$ref"].is_string(),
            "the nested property should still be a $ref"
        );
    }

    #[test]
    #[should_panic(expected = "MCP requires inputSchema to be a JSON Schema object")]
    fn add_typed_tool_panics_on_a_non_object_input_schema() {
        let mut router = ToolRouter::<()>::new();
        router.add_typed_tool("scalar", "A scalar input.", |(), _input: String| {
            Ok::<_, SampleError>(json!({}))
        });
    }

    #[test]
    fn router_rejects_a_name_that_cannot_identify_a_tool() {
        // A name exists to identify a tool, so a name that identifies nothing is
        // a broken registration on this crate's own terms. Left unchecked it is
        // also reported in the wrong place: the failure would not surface until a
        // `tools/call` came back TargetNotFound, at call time, in the client,
        // pointing at the caller — when the defect was in the registration line.
        for name in ["", " ", "\t", "\n", "   \t "] {
            let error = ToolRouter::<()>::new()
                .try_add_tool_for_test::<AddArgs>(name)
                .expect_err("an unnameable tool should be rejected");

            assert_eq!(error.code(), "invalid_tool_name", "for {name:?}");
            assert_eq!(error.category(), ErrorCategory::Validation, "for {name:?}");
        }
    }

    #[test]
    fn router_does_not_import_a_downstream_host_charset_rule() {
        // Charset and length are somebody else's constraint. A consumer talking
        // to a host with a stricter function-name rule layers its own check over
        // try_add_tool; a consumer that is not must not be forced to.
        for name in ["my tool", "tool.with.dots", "tool/with/slashes", "工具"] {
            ToolRouter::<()>::new()
                .try_add_tool_for_test::<AddArgs>(name)
                .unwrap_or_else(|error| panic!("{name:?} should register: {}", error.message()));
        }
    }

    #[test]
    #[should_panic(expected = "must not be empty or whitespace-only")]
    fn add_typed_tool_panics_on_an_unnameable_tool() {
        let mut router = ToolRouter::<()>::new();
        router.add_typed_tool("  ", "An unnameable tool.", |(), args: AddArgs| {
            Ok::<_, SampleError>(json!({ "sum": args.lhs + args.rhs }))
        });
    }

    #[test]
    fn router_rejects_a_duplicate_tool_name_with_a_structured_error() {
        // A duplicate registration would otherwise be silent: `tools/list` would
        // advertise the name twice and `call_tool` would dispatch to the first
        // registration forever, leaving the second unreachable.
        let mut router = build_math_router();

        let error = router
            .try_add_tool(Tool::new_typed::<AddArgs, Value, SampleError, _>(
                "math_add",
                "A second, colliding registration.",
                |(), _args: AddArgs| Ok::<_, SampleError>(json!({ "sum": 0 })),
            ))
            .expect_err("a duplicate tool name should be rejected");

        assert_eq!(error.code(), "duplicate_tool_name");
        assert_eq!(error.category(), ErrorCategory::Validation);
        assert!(error.message().contains("math_add"));

        // The router is unchanged: still one `math_add`, still the original.
        let names: Vec<String> = router
            .tool_metadata()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(names.iter().filter(|name| *name == "math_add").count(), 1);
        assert_eq!(
            router.call_tool(&(), "math_add", json!({ "lhs": 2, "rhs": 3 })),
            JsonEnvelope::success_for("math_add", json!({ "sum": 5 }))
        );
    }

    #[test]
    fn router_accepts_a_distinct_tool_name() {
        let mut router = build_math_router();

        router
            .try_add_tool(Tool::new_typed::<AddArgs, Value, SampleError, _>(
                "math_add_v2",
                "A distinct registration.",
                |(), args: AddArgs| Ok::<_, SampleError>(json!({ "sum": args.lhs + args.rhs })),
            ))
            .expect("a distinct tool name should register");

        assert!(
            router
                .tool_metadata()
                .iter()
                .any(|tool| tool.name == "math_add_v2")
        );
    }

    #[test]
    #[should_panic(expected = "tool `math_add` is already registered")]
    fn add_tool_panics_on_a_duplicate_tool_name() {
        // The ergonomic registration path fails loudly at startup rather than
        // building a router with an unreachable tool.
        let mut router = build_math_router();
        router.add_typed_tool(
            "math_add",
            "A colliding registration.",
            |(), args: AddArgs| Ok::<_, SampleError>(json!({ "sum": args.lhs + args.rhs })),
        );
    }

    #[test]
    fn a_panicking_handler_becomes_a_tool_error_not_a_dead_process() {
        // Before this, an unwinding handler took the whole process with it: no
        // response for the panicking call, no response for anything queued behind
        // it, and a closed pipe as the client's only signal.
        let mut router: ToolRouter<()> = ToolRouter::new();
        router.add_typed_tool("risky", "Panics on demand.", |(), args: EchoArgs| {
            assert!(!args.uppercase, "tool handler blew up");
            Ok::<_, SampleError>(json!({ "text": args.text }))
        });

        let envelope = router.call_tool(&(), "risky", json!({ "text": "hi", "uppercase": true }));

        assert!(envelope.is_error());
        let rendered = serde_json::to_value(&envelope).expect("envelope serializes");
        assert_eq!(rendered["error"]["code"], "tool_panicked");
        assert_eq!(rendered["error"]["category"], "execution_failure");
        // The panic's own message is surfaced, so the client learns which tool
        // failed and why rather than inferring it from a closed pipe.
        let message = rendered["error"]["message"]
            .as_str()
            .expect("message is a string");
        assert!(message.contains("risky"), "{message}");
        assert!(message.contains("tool handler blew up"), "{message}");
    }

    #[test]
    fn a_panicking_handler_does_not_strand_the_requests_behind_it() {
        // The obligation that makes catching correct here: a client is blocking
        // on an id, and further requests are already buffered behind this one.
        let mut router: ToolRouter<()> = ToolRouter::new();
        router.add_typed_tool(
            "risky",
            "Panics on demand.",
            |(), _args: EchoArgs| -> Result<Value, SampleError> { panic!("handler exploded") },
        );
        router.add_typed_tool("math_add", "Add two integers.", |(), args: AddArgs| {
            Ok::<_, SampleError>(json!({ "sum": args.lhs + args.rhs }))
        });

        let server = McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            router,
        );

        let input = [
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "risky", "arguments": { "text": "x", "uppercase": false } }
            })),
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "math_add", "arguments": { "lhs": 2, "rhs": 3 } }
            })),
        ]
        .concat();

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("a panicking handler must not end the session");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 2);
        // The panicking call is answered as a tool-level failure, not a
        // JSON-RPC error and not silence.
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["result"]["isError"], true);
        assert_eq!(
            responses[0]["result"]["structuredContent"]["error"]["code"],
            "tool_panicked"
        );
        assert!(responses[0]["error"].is_null());
        // The request queued behind it still runs.
        assert_eq!(responses[1]["id"], 2);
        assert_eq!(
            responses[1]["result"]["structuredContent"]["data"]["sum"],
            5
        );
        assert_eq!(responses[1]["result"]["isError"], false);
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
    fn stdio_server_treats_every_arm_without_an_id_as_a_notification() {
        // The id-presence rule is a property of every arm, not just the
        // notification method: a request object with no id gets no response
        // frame, whatever it asked for. This is the invariant most easily lost
        // when an arm is lifted into its own method, because the natural
        // refactor returns a Value and reattaches the id at the end — so it is
        // pinned here for `initialize` and for both `tools/call` paths, the
        // arms extracted in bd-46f7d2.
        let server = sample_server();

        let input = [
            // initialize, no id
            frame_request(&json!({
                "jsonrpc": "2.0",
                "method": "initialize",
                "params": { "protocolVersion": "2024-11-05" }
            })),
            // tools/call with valid params, no id
            frame_request(&json!({
                "jsonrpc": "2.0",
                "method": "tools/call",
                "params": {
                    "name": "math_add",
                    "arguments": { "lhs": 2, "rhs": 3 }
                }
            })),
            // tools/call whose params do not deserialize, no id: the -32602
            // path must respect id-absence too.
            frame_request(&json!({
                "jsonrpc": "2.0",
                "method": "tools/call",
                "params": { "not_a_name": true }
            })),
            // tools/list, no id
            frame_request(&json!({
                "jsonrpc": "2.0",
                "method": "tools/list"
            })),
        ]
        .concat();

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("notifications should be accepted");

        assert!(
            output.is_empty(),
            "no arm may answer a request that carries no id: {}",
            String::from_utf8_lossy(&output)
        );
    }

    #[test]
    fn stdio_server_answers_initialized_when_the_client_sends_it_with_an_id() {
        // JSON-RPC 2.0: a request object carrying an id is not a notification
        // and MUST be answered. A client that assigns an id to every outgoing
        // message would otherwise wait forever on this one, stalling the
        // session with no error surfaced.
        let server = sample_server();

        let mut input = frame_request(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "notifications/initialized"
        }));
        input.extend_from_slice(&frame_request(&json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "ping"
        })));

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("stdio server should accept initialized sent as a request");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 2, "responses: {responses:?}");

        assert_eq!(responses[0]["jsonrpc"], "2.0");
        assert_eq!(responses[0]["id"], 4);
        assert_eq!(responses[0]["result"], json!({}));
        assert!(
            responses[0].get("error").is_none(),
            "a supported method must not answer with an error: {:?}",
            responses[0]
        );

        // The session keeps serving afterwards.
        assert_eq!(responses[1]["id"], 5);
        assert_eq!(responses[1]["result"], json!({}));
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

        let frame = read_protocol_message(&mut input, DEFAULT_MAX_FRAME_BYTES)
            .expect("a newline-delimited line should read cleanly");

        assert_eq!(frame_bytes(frame), b"{\"jsonrpc\":\"2.0\"}");
    }

    #[test]
    fn read_protocol_message_skips_blank_separator_lines() {
        let mut input = std::io::Cursor::new(b"\n\r\n{\"id\":1}\n".to_vec());

        let frame = read_protocol_message(&mut input, DEFAULT_MAX_FRAME_BYTES)
            .expect("blank lines should be skipped");

        assert_eq!(frame_bytes(frame), b"{\"id\":1}");
    }

    #[test]
    fn read_protocol_message_returns_raw_line_for_non_json_text() {
        // The reader is framing-only: it returns the raw line and lets the serve
        // layer reject non-JSON, matching the MCP stdio NDJSON transport.
        let mut input = std::io::Cursor::new(b"this is not json\n".to_vec());

        let frame = read_protocol_message(&mut input, DEFAULT_MAX_FRAME_BYTES)
            .expect("reading a line should not itself fail");

        assert_eq!(frame_bytes(frame), b"this is not json");
    }

    #[test]
    fn read_protocol_message_returns_raw_bytes_for_invalid_utf8() {
        // Framing must not validate UTF-8: `read_line` would fail the whole
        // session with Io(InvalidData), whereas a bad byte should degrade to an
        // ordinary parse error at the serve layer.
        let mut input = std::io::Cursor::new(b"{\"a\":\"\xff\xfe\"}\n".to_vec());

        let frame = read_protocol_message(&mut input, DEFAULT_MAX_FRAME_BYTES)
            .expect("invalid UTF-8 must not be an I/O error");

        assert_eq!(frame_bytes(frame), b"{\"a\":\"\xff\xfe\"}");
    }

    #[test]
    fn read_protocol_message_accepts_a_frame_exactly_at_the_cap() {
        let payload = vec![b'x'; 8];
        let mut input = std::io::Cursor::new([payload.clone(), b"\n".to_vec()].concat());

        let frame =
            read_protocol_message(&mut input, payload.len()).expect("a frame at the cap is legal");

        assert_eq!(frame_bytes(frame), payload.as_slice());
    }

    #[test]
    fn read_protocol_message_reports_an_oversized_frame_and_resyncs() {
        // One byte over the cap: the frame is discarded up to the next newline
        // and the following frame is still readable.
        let mut input =
            std::io::Cursor::new([b"xxxxxxxxx\n".to_vec(), b"{\"id\":1}\n".to_vec()].concat());

        match read_protocol_message(&mut input, 8).expect("an oversized frame is not an error") {
            ProtocolFrame::Oversized(discarded) => assert_eq!(discarded, 10),
            other => panic!("expected an oversized frame, got {other:?}"),
        }

        let frame = read_protocol_message(&mut input, 8).expect("the transport should resync");
        assert_eq!(frame_bytes(frame), b"{\"id\":1}");
    }

    #[test]
    fn read_protocol_message_bounds_a_frame_that_never_terminates() {
        // A peer that never emits a newline must not be able to force an
        // unbounded allocation: the read stops at the cap.
        let mut input = std::io::Cursor::new(vec![b'x'; 1024]);

        match read_protocol_message(&mut input, 16).expect("an unterminated frame is not an error")
        {
            ProtocolFrame::Oversized(discarded) => assert_eq!(discarded, 1024),
            other => panic!("expected an oversized frame, got {other:?}"),
        }

        assert!(matches!(
            read_protocol_message(&mut input, 16).expect("clean EOF should follow"),
            ProtocolFrame::Eof
        ));
    }

    #[test]
    fn read_protocol_message_returns_a_final_frame_without_a_trailing_newline() {
        let mut input = std::io::Cursor::new(b"{\"id\":1}".to_vec());

        let frame = read_protocol_message(&mut input, DEFAULT_MAX_FRAME_BYTES)
            .expect("a missing trailing newline is tolerated at EOF");

        assert_eq!(frame_bytes(frame), b"{\"id\":1}");
    }

    #[test]
    fn read_protocol_message_returns_none_on_clean_eof() {
        let mut input = std::io::Cursor::new(Vec::new());

        let frame = read_protocol_message(&mut input, DEFAULT_MAX_FRAME_BYTES)
            .expect("clean EOF should not be an error");

        assert!(matches!(frame, ProtocolFrame::Eof));
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
    fn stdio_server_answers_non_json_input_with_parse_error_and_keeps_serving() {
        // Regression: typing arbitrary text into the stdio transport must surface
        // a JSON-RPC `-32700` parse error rather than silently consuming it
        // (which previously hung) or tearing down the session (which previously
        // returned `Err(McpCliError::Json)` and dropped every later message).
        let server = McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            build_math_router(),
        );

        let mut input = b"hello there\n".to_vec();
        input.extend_from_slice(&frame_request(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "ping"
        })));

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("a malformed frame must not end the session");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["jsonrpc"], "2.0");
        assert_eq!(responses[0]["error"]["code"], -32700);
        assert_eq!(responses[0]["id"], Value::Null);
        // The session keeps serving: the request after the garbage line is answered.
        assert_eq!(responses[1]["id"], 7);
        assert_eq!(responses[1]["result"], json!({}));
    }

    #[test]
    fn stdio_server_answers_an_oversized_frame_with_parse_error_and_keeps_serving() {
        // A frame beyond the cap is never buffered in full; the server reports it
        // and resynchronises on the next frame instead of allocating without
        // bound or dropping the session.
        let server = McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            build_math_router(),
        )
        .with_max_frame_bytes(64);
        assert_eq!(server.max_frame_bytes(), 64);

        let mut input = vec![b'x'; 4096];
        input.push(b'\n');
        input.extend_from_slice(&frame_request(&json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "ping"
        })));

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("an oversized frame must not end the session");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["error"]["code"], -32700);
        assert_eq!(responses[0]["id"], Value::Null);
        assert!(
            responses[0]["error"]["message"]
                .as_str()
                .expect("error message should be a string")
                .contains("64-byte limit")
        );
        assert_eq!(responses[1]["id"], 9);
        assert_eq!(responses[1]["result"], json!({}));
    }

    #[test]
    fn stdio_server_answers_invalid_utf8_with_parse_error_and_keeps_serving() {
        // Framing reads raw bytes, so a non-UTF-8 frame degrades to a -32700
        // parse error rather than an Io(InvalidData) that ends the session.
        let server = McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            build_math_router(),
        );

        let mut input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"\xff\xfe\"}\n".to_vec();
        input.extend_from_slice(&frame_request(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "ping"
        })));

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("a non-UTF-8 frame must not end the session");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["error"]["code"], -32700);
        assert_eq!(responses[0]["id"], Value::Null);
        assert_eq!(responses[1]["id"], 2);
        assert_eq!(responses[1]["result"], json!({}));
    }

    #[test]
    fn stdio_server_parse_error_does_not_consume_following_frames_silently() {
        // A truncated JSON object is a parse error, not an invalid request:
        // it must not be reported as `-32600`, and the frame after it still runs.
        let server = McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            build_math_router(),
        );

        let mut input = b"{\"jsonrpc\": \"2.0\", \"id\": 1, \"method\":\n".to_vec();
        input.extend_from_slice(&frame_request(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "math_add",
                "arguments": { "lhs": 2, "rhs": 3 }
            }
        })));

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("a truncated frame must not end the session");

        let responses = parse_framed_responses(&output);
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["error"]["code"], -32700);
        assert_eq!(responses[0]["id"], Value::Null);
        assert_eq!(
            responses[1]["result"]["structuredContent"]["data"]["sum"],
            5
        );
        assert_eq!(responses[1]["result"]["isError"], false);
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
        // A missing tool name is a not-found condition, not a validation failure.
        assert_eq!(
            responses[0]["result"]["structuredContent"]["error"]["category"],
            "target_not_found"
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

    #[derive(Debug, Serialize, JsonSchema)]
    struct SumOutput {
        sum: i64,
    }

    #[test]
    fn typed_tool_can_advertise_output_schema() {
        let mut router: ToolRouter<()> = ToolRouter::new();
        router.add_typed_tool_with_output_schema(
            "sum",
            "Add two integers and report the sum.",
            |(), args: AddArgs| {
                Ok::<_, SampleError>(SumOutput {
                    sum: args.lhs + args.rhs,
                })
            },
        );
        router.add_typed_tool(
            "echo_plain",
            "Echo text without an output schema.",
            |(), args: EchoArgs| Ok::<_, SampleError>(json!({ "text": args.text })),
        );

        let tools = router.tool_metadata();

        let sum_tool = tools
            .iter()
            .find(|tool| tool.name == "sum")
            .expect("sum tool is registered");
        let output_schema = sum_tool
            .output_schema
            .as_ref()
            .expect("sum tool advertises an output schema");
        // structuredContent is the JsonEnvelope wrapping Output, so the advertised
        // schema must describe that envelope rather than the bare Output. Assert
        // the envelope discriminator/fields and the Output field all appear,
        // tolerant of schemars' exact $ref/oneOf layout.
        let schema_text = serde_json::to_string(output_schema).expect("schema serializes");
        assert!(
            schema_text.contains("status"),
            "envelope status discriminator present: {schema_text}"
        );
        assert!(
            schema_text.contains("\"data\""),
            "envelope data field present: {schema_text}"
        );
        assert!(
            schema_text.contains("sum"),
            "Output `sum` surfaced under the envelope data: {schema_text}"
        );

        // Serialized metadata uses camelCase `outputSchema` when present.
        let sum_json = serde_json::to_value(sum_tool).expect("tool metadata serializes");
        assert!(sum_json.get("outputSchema").is_some());

        // A tool registered without an output schema omits the field entirely.
        let echo_tool = tools
            .iter()
            .find(|tool| tool.name == "echo_plain")
            .expect("echo tool is registered");
        assert!(echo_tool.output_schema.is_none());
        let echo_json = serde_json::to_value(echo_tool).expect("tool metadata serializes");
        assert!(echo_json.get("outputSchema").is_none());
    }

    #[test]
    fn advertised_output_schema_declares_an_object_root_and_keeps_the_oneof() {
        // MCP 2025-06-18 requires outputSchema to declare a root `type: object`,
        // but JsonEnvelope is an internally-tagged enum, so the derived document
        // is rooted in `oneOf` with no `type` at all. Declaring the root type
        // makes the document conformant without collapsing the success/error
        // discrimination that makes it worth advertising.
        let mut router: ToolRouter<()> = ToolRouter::new();
        router.add_typed_tool_with_output_schema(
            "sum",
            "Add two integers and report the sum.",
            |(), args: AddArgs| {
                Ok::<_, SampleError>(SumOutput {
                    sum: args.lhs + args.rhs,
                })
            },
        );

        let tools = router.tool_metadata();
        let output_schema = tools[0]
            .output_schema
            .as_ref()
            .expect("sum tool advertises an output schema");

        assert_eq!(output_schema["type"], "object");

        // Both envelope variants survive, and both are objects, so the declared
        // root type rejects nothing the schema previously accepted.
        let variants = output_schema["oneOf"]
            .as_array()
            .expect("the envelope schema keeps its oneOf branches");
        assert_eq!(variants.len(), 2);
        for variant in variants {
            assert_eq!(variant["type"], "object");
        }
        let discriminators: Vec<&Value> = variants
            .iter()
            .map(|variant| &variant["properties"]["status"]["const"])
            .collect();
        assert!(discriminators.contains(&&json!("success")));
        assert!(discriminators.contains(&&json!("error")));
    }

    #[test]
    fn advertised_output_schema_describes_tools_call_structured_content() {
        // Every top-level key a real tools/call emits in structuredContent must be
        // named in the advertised outputSchema, proving the two describe the same
        // shape (the JsonEnvelope wrapping Output) rather than diverging (bd-870183).
        let mut router: ToolRouter<()> = ToolRouter::new();
        router.add_typed_tool_with_output_schema(
            "sum",
            "Add two integers and report the sum.",
            |(), args: AddArgs| {
                Ok::<_, SampleError>(SumOutput {
                    sum: args.lhs + args.rhs,
                })
            },
        );

        let schema_text = {
            let tools = router.tool_metadata();
            let sum_tool = tools
                .iter()
                .find(|tool| tool.name == "sum")
                .expect("sum tool is registered");
            serde_json::to_string(
                sum_tool
                    .output_schema
                    .as_ref()
                    .expect("sum tool advertises an output schema"),
            )
            .expect("schema serializes")
        };

        let envelope = router.call_tool(&(), "sum", json!({ "lhs": 2, "rhs": 3 }));
        let structured = serde_json::to_value(&envelope).expect("envelope serializes");
        let object = structured
            .as_object()
            .expect("structuredContent is an object");
        for key in object.keys() {
            assert!(
                schema_text.contains(key.as_str()),
                "structuredContent key `{key}` is described by the advertised schema: {schema_text}"
            );
        }
        // And the success payload really is the envelope shape we advertise.
        assert_eq!(structured["status"], "success");
        assert_eq!(structured["data"]["sum"], 5);
    }

    #[test]
    fn stdio_server_reinitialize_is_idempotent_and_keeps_serving() {
        // Reconnect-safe contract: the stateless server answers a repeated
        // `initialize` (including a re-initialize after notifications/initialized)
        // without rejecting the same session, and keeps serving afterwards. This
        // is the reference behavior a same-agent MCP reconnect should rely on.
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
                "method": "notifications/initialized"
            })),
            // Re-initialize on the same session must still be acknowledged.
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "initialize",
                "params": { "protocolVersion": "2024-11-05" }
            })),
            // The session must keep serving subsequent requests after re-init.
            frame_request(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "ping",
                "params": {}
            })),
        ]
        .concat();

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("re-initialize must not tear down the session");

        let responses = parse_framed_responses(&output);
        // initialize(id=1), initialize(id=2), ping(id=3); the notification yields none.
        assert_eq!(responses.len(), 3);

        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["result"]["serverInfo"]["name"], "sample-mcp");
        assert_eq!(responses[0]["result"]["protocolVersion"], "2024-11-05");

        // The re-initialize is acknowledged idempotently (no 'already initialized' error).
        assert_eq!(responses[1]["id"], 2);
        assert!(responses[1].get("error").is_none());
        assert_eq!(responses[1]["result"]["serverInfo"]["name"], "sample-mcp");
        assert_eq!(responses[1]["result"]["protocolVersion"], "2024-11-05");

        // The session survived the reconnect handshake and answered the next request.
        assert_eq!(responses[2]["id"], 3);
        assert_eq!(responses[2]["result"], json!({}));
    }

    fn sample_server() -> McpServer<()> {
        McpServer::new(
            StdioServerConfig {
                server_name: "sample-mcp".to_string(),
                server_version: "0.0.1".to_string(),
            },
            build_math_router(),
        )
    }

    #[test]
    fn stdio_server_answers_a_batch_with_an_ordered_array_of_responses() {
        // MCP 2025-03-26 requires implementations to be able to RECEIVE JSON-RPC
        // batches. Every member runs and the responses come back in request
        // order as a single array frame (JSON-RPC 2.0 section 6).
        let server = sample_server();

        let input = frame_request(&json!([
            { "jsonrpc": "2.0", "id": 1, "method": "ping" },
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "math_add", "arguments": { "lhs": 2, "rhs": 3 } }
            },
            { "jsonrpc": "2.0", "id": 3, "method": "tools/list" }
        ]));

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("a batch must be served");

        let frames = parse_framed_responses(&output);
        assert_eq!(frames.len(), 1, "a batch is answered in one frame");

        let responses = frames[0].as_array().expect("batch response is an array");
        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["result"], json!({}));
        assert_eq!(responses[1]["id"], 2);
        assert_eq!(
            responses[1]["result"]["structuredContent"]["data"]["sum"],
            5
        );
        assert_eq!(responses[2]["id"], 3);
        assert_eq!(responses[2]["result"]["tools"][0]["name"], "math_add");
    }

    #[test]
    fn stdio_server_batch_omits_notification_members_and_keeps_serving() {
        // Notifications carry no id and get no response, so they are omitted
        // from the batch array while their siblings are still answered. The
        // session then serves the next frame normally.
        let server = sample_server();

        let mut input = frame_request(&json!([
            { "jsonrpc": "2.0", "method": "notifications/initialized" },
            { "jsonrpc": "2.0", "id": 9, "method": "ping" }
        ]));
        input.extend_from_slice(&frame_request(&json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "ping"
        })));

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("a batch with notifications must be served");

        let frames = parse_framed_responses(&output);
        assert_eq!(frames.len(), 2, "frames: {frames:?}");

        let responses = frames[0].as_array().expect("batch response is an array");
        assert_eq!(responses.len(), 1, "the notification is omitted");
        assert_eq!(responses[0]["id"], 9);

        assert_eq!(frames[1]["id"], 10);
        assert_eq!(frames[1]["result"], json!({}));
    }

    #[test]
    fn stdio_server_notification_only_batch_produces_no_response_frame() {
        let server = sample_server();

        let input = frame_request(&json!([
            { "jsonrpc": "2.0", "method": "notifications/initialized" },
            { "jsonrpc": "2.0", "method": "notifications/initialized" }
        ]));

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("a notification-only batch must be served");

        assert!(
            output.is_empty(),
            "a notification-only batch gets no response: {:?}",
            String::from_utf8_lossy(&output)
        );
    }

    #[test]
    fn stdio_server_empty_batch_returns_a_single_invalid_request_object() {
        // JSON-RPC 2.0 section 6: an empty batch array is itself an Invalid
        // Request, answered with one response object rather than an array.
        let server = sample_server();

        let mut output = Vec::new();
        server
            .serve_transport(
                &(),
                std::io::Cursor::new(frame_request(&json!([]))),
                &mut output,
            )
            .expect("an empty batch must not end the session");

        let frames = parse_framed_responses(&output);
        assert_eq!(frames.len(), 1);
        assert!(
            !frames[0].is_array(),
            "empty batch is answered with a single object: {:?}",
            frames[0]
        );
        assert_eq!(frames[0]["id"], Value::Null);
        assert_eq!(frames[0]["error"]["code"], -32600);
    }

    #[test]
    fn stdio_server_batch_reports_invalid_members_without_dropping_valid_ones() {
        // A malformed member gets its own -32600 entry; its siblings still run.
        let server = sample_server();

        let input = frame_request(&json!([
            { "jsonrpc": "2.0", "id": 1, "missing": "method" },
            { "jsonrpc": "2.0", "id": 2, "method": "ping" },
            "not even an object"
        ]));

        let mut output = Vec::new();
        server
            .serve_transport(&(), std::io::Cursor::new(input), &mut output)
            .expect("a partially invalid batch must be served");

        let frames = parse_framed_responses(&output);
        assert_eq!(frames.len(), 1);

        let responses = frames[0].as_array().expect("batch response is an array");
        assert_eq!(responses.len(), 3, "responses: {responses:?}");

        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["error"]["code"], -32600);

        assert_eq!(responses[1]["id"], 2);
        assert_eq!(responses[1]["result"], json!({}));

        // A non-object member has no recoverable id, so it answers with null.
        assert_eq!(responses[2]["id"], Value::Null);
        assert_eq!(responses[2]["error"]["code"], -32600);
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    struct SampleInner {
        a: i64,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    struct SampleNested {
        inner: SampleInner,
        b: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    enum SampleChoice {
        Left,
        Right,
    }

    /// Register a no-op tool with the given `Input` type, for schema-shape tests.
    trait TryAddToolForTest {
        fn try_add_tool_for_test<Input>(&mut self, name: &str) -> Result<(), JsonError>
        where
            Input: serde::de::DeserializeOwned + JsonSchema + 'static;
    }

    impl TryAddToolForTest for ToolRouter<()> {
        fn try_add_tool_for_test<Input>(&mut self, name: &str) -> Result<(), JsonError>
        where
            Input: serde::de::DeserializeOwned + JsonSchema + 'static,
        {
            self.try_add_tool(Tool::new_typed::<Input, Value, SampleError, _>(
                name,
                "A tool registered for its input schema shape.",
                |(), _input: Input| Ok::<_, SampleError>(json!({})),
            ))
        }
    }

    fn frame_request(value: &Value) -> Vec<u8> {
        let mut message = serde_json::to_vec(value).expect("request should serialize");
        message.push(b'\n');
        message
    }

    fn frame_bytes(frame: ProtocolFrame) -> Vec<u8> {
        match frame {
            ProtocolFrame::Message(message) => message,
            other => panic!("expected a complete frame, got {other:?}"),
        }
    }

    fn parse_framed_responses(bytes: &[u8]) -> Vec<Value> {
        let text = std::str::from_utf8(bytes).expect("responses should be valid utf-8");
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("response line should be json"))
            .collect()
    }
}
