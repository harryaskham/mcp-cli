# mcp-cli

`mcp-cli` is a small Rust framework for exposing the same command implementation
through a traditional CLI JSON surface and a Model Context Protocol (MCP) stdio
server. It is intentionally application-agnostic: consumers provide typed input
structures, output values, and structured errors; the crate handles envelopes,
JSON schema generation, MCP framing, tool listing, and tool calls.

## What it provides

- Stable `JsonEnvelope<T>` success/error responses for `--json` CLI output.
- `StructuredError` and `JsonError` for projecting domain errors into a shared
  machine-readable shape.
- `ToolRouter` and typed `Tool` registration backed by `schemars` input schemas.
- A minimal `McpServer` that speaks MCP over stdio using JSON-RPC framing.
- Generic tests that prove a CLI command surface and MCP tool surface can share
  the same command contracts without hard-coding any one application.

## Minimal pattern

```rust
use mcp_cli::{ErrorCategory, McpServer, StdioServerConfig, StructuredError, ToolRouter};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Deserialize, JsonSchema)]
struct AddInput {
    lhs: i64,
    rhs: i64,
}

#[derive(Debug, Serialize)]
struct AddOutput {
    sum: i64,
}

#[derive(Debug)]
struct AppError(String);

impl StructuredError for AppError {
    fn category(&self) -> ErrorCategory { ErrorCategory::Validation }
    fn code(&self) -> String { "app_error".to_owned() }
    fn message(&self) -> String { self.0.clone() }
}

let mut router = ToolRouter::new();
router.add_typed_tool("math_add", "Add two integers.", |(), input: AddInput| {
    Ok::<_, AppError>(AddOutput { sum: input.lhs + input.rhs })
});

let server = McpServer::new(
    StdioServerConfig {
        server_name: "my-cli".to_owned(),
        server_version: env!("CARGO_PKG_VERSION").to_owned(),
    },
    router,
);
# let _ = json!({ "tools": server.tool_metadata() });
```

For CLI commands, use `write_json_result` or `write_json_result_ref` to emit the
same stable envelope shape that MCP `tools/call` returns as structured content.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Keep this crate generic. Application-specific concepts (for example window IDs,
platform adapters, or project-specific error codes) belong in the consuming CLI,
not in `mcp-cli`.
