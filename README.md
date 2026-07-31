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

## Tool registration

The router enforces its own invariants and nothing else. A tool name must be
non-empty and unique, because a name exists to identify exactly one tool, and a
tool's input type must derive a JSON Schema **object**, because MCP constrains
`inputSchema` to an object. All three are caught at registration rather than
surfacing later somewhere that misreports where the bug is — an unnameable tool
would otherwise come back as "no such tool" at call time, in the client.

- `add_tool` / `add_typed_tool` / `add_typed_tool_with_output_schema` **panic**
  on an empty or whitespace-only name, a duplicate name, or a non-object input
  schema. Registration is a startup-time concern, so it fails immediately and
  names the offending tool.
- `try_add_tool` returns `Err(JsonError)` with code `invalid_tool_name`,
  `duplicate_tool_name`, or `invalid_input_schema` and leaves the router
  unchanged. Use it when the tool set is assembled dynamically from config, a
  plugin set, or user input.

Charset and length are deliberately **not** enforced: they are a downstream
host's constraint, not this crate's. As a portability note, some hosts feed tool
names into function-calling APIs with stricter rules (a common one is
`^[a-zA-Z0-9_-]{1,64}$`), so names that stay inside that set travel furthest. A
consumer targeting such a host can layer its own check over `try_add_tool`.

The schema check is conservative: only a root that declares a non-object `type`
is rejected, so a nested struct using `$defs` / `$ref` registers normally.

## MCP protocol support

> **stdout is the protocol channel.** `serve_stdio` frames everything this
> process writes to stdout as protocol output. The hazard is broader than a
> stray `println!` in a handler — it is **anything the consumer installs that
> can reach file descriptor 1**:
>
> - a `println!` or `dbg!` in a tool handler, or a dependency that logs to
>   stdout;
> - a `tracing`/`log` subscriber left on its default sink, which is commonly
>   stdout — the case most likely to bite, since nobody wires a `println!` into
>   a handler on purpose but plenty of people wire a logger without thinking
>   about where it lands;
> - a **custom panic hook** that writes to stdout. Rust's default hook prints to
>   stderr, which is correct; a hook redirected to stdout corrupts the stream at
>   exactly the moment a tool is failing, interleaved with the error response
>   for that same request.
>
> Any of them is emitted as its own frame:
>
> ```text
> DEBUG: about to do work                       <- a println! in a handler
> {"id":1,"jsonrpc":"2.0","result":{ ... }}     <- the actual response
> ```
>
> The symptom a client reports is a non-JSON frame, or the session simply
> dropping — many MCP clients treat unparseable server output as fatal. Route
> **all** consumer output to stderr. This crate cannot enforce it: redirecting
> the file descriptor would require `unsafe`, which the crate forbids.

`McpServer::serve_stdio` (and `serve_transport`) speak MCP over stdio using
newline-delimited JSON (NDJSON): one JSON-RPC message per line terminated by
`\n`, with blank separator lines tolerated. Supported methods:

- `initialize` — negotiates the protocol version: the client's requested
  `protocolVersion` is echoed when supported, otherwise the server advertises its
  latest supported version. The supported set is the `SUPPORTED_PROTOCOL_VERSIONS`
  constant.
- `notifications/initialized` — accepted; produces no response. If a client
  sends it with an `id` it is a request under JSON-RPC 2.0, so it is answered
  with an empty result rather than leaving the client waiting.
- `ping` — replies with an empty result.
- `tools/list` — returns the router's tool metadata (name, description, input
  schema, and an optional `outputSchema` for tools registered with
  `add_typed_tool_with_output_schema`). Because `tools/call` returns the stable
  `JsonEnvelope` as `structuredContent`, the advertised `outputSchema` describes
  that envelope (`status`/`meta`/`data`) wrapping the tool's `Output`, so the
  returned `structuredContent` conforms to it (MCP 2025-06-18). The envelope is
  an internally-tagged enum, so the derived document keeps a `oneOf` over the
  success and error variants and declares a root `"type": "object"`, which that
  revision requires of `outputSchema`.
- `tools/call` — runs a typed tool and returns both `structuredContent` (the
  stable `JsonEnvelope`) and a `text` content block, with `isError` reflecting
  tool failures.

Malformed input is answered with a JSON-RPC error while the session keeps
serving, rather than tearing the connection down:

- `-32700` Parse error — a frame that is not valid JSON, is not valid UTF-8, or
  exceeds the frame size cap (`id` is `null`).
- `-32600` Invalid Request — JSON that is not a valid JSON-RPC request object.
- `-32601` Method not found — an unknown MCP method.
- `-32602` Invalid params — a `tools/call` whose params do not deserialize.

A tool handler that **panics** is caught and reported as a tool-level failure —
`isError` with code `tool_panicked` — so the client learns which tool failed and
why, and requests queued behind it are still served. Two limits worth knowing:
it cannot help under `panic = "abort"`, where there is no unwind to catch, and it
does not restore consistency, since `Ctx` is left exactly as the handler left it.
`tool_panicked` reports a bug to fix, not a condition to handle.

The stdio transport is newline-delimited with no length prefix, so a single
frame is capped at `DEFAULT_MAX_FRAME_BYTES` (16 MiB) to keep a peer that never
emits a newline from forcing an unbounded allocation. An oversized frame is
discarded up to the next frame boundary and reported as a parse error. Override
the cap per server:

```rust,ignore
let server = McpServer::new(config, router).with_max_frame_bytes(1024 * 1024);
```

### Batch requests

A frame containing a JSON array is handled as a JSON-RPC batch: every member is
executed and the responses come back as one array frame in request order.
Members that are notifications (no `id`) are omitted from the array, a batch of
only notifications produces no response frame at all, and an empty array is
answered with a single `-32600` object rather than an array (JSON-RPC 2.0
section 6). A malformed member gets its own `-32600` entry without affecting its
siblings.

MCP `2025-03-26` requires implementations to be able to *receive* batches;
`2025-06-18` removed batching. Batches are accepted on any negotiated version:
that is simpler than gating the transport on the handshake, and a client that
never batches is unaffected.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Keep this crate generic. Application-specific concepts (for example window IDs,
platform adapters, or project-specific error codes) belong in the consuming CLI,
not in `mcp-cli`.
