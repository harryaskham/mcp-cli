# Session summary — MCP stdio server resilience for invalid request / invalid params

## Goal

Make the reusable `mcp-cli` stdio MCP server resilient to malformed-but-parseable JSON-RPC traffic. A long-running stdio server should answer a bad message with the correct JSON-RPC error and keep serving the connection, rather than tearing the whole session down and dropping every subsequent valid request.

## Bead(s)

- `bd-909a82` — MCP stdio server should return JSON-RPC errors for invalid request / invalid params, not tear down the session

## Before state

- Failing tests: none. Baseline `cargo test --workspace --all-features` was green (20 unit tests + 1 doctest).
- Only `-32601 method not found` was handled gracefully in `handle_request`.
- Two malformed-but-parseable cases instead propagated a Rust error out of `serve_transport`, terminating the session:
  - a JSON value that parses but is not a valid JSON-RPC request (e.g. missing `method`) via `serde_json::from_value::<JsonRpcRequest>(request)?` in `handle_request_value`;
  - a `tools/call` whose `params` failed to deserialize into `ToolCallParams` (e.g. missing `name`) via `serde_json::from_value(...)?` in the `tools/call` arm.

## After state

- Failing tests: none. `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features` are all green (22 unit tests + 1 doctest).
- `handle_request_value` now recovers any `id` first, then returns a `-32600 Invalid Request` response (id recovered when present, otherwise `null`) and keeps serving.
- The `tools/call` arm now returns a `-32602 Invalid params` response with the request id on bad params and keeps serving.
- The deliberate bd-c7aeca behavior for non-JSON transport lines (fail visibly rather than hang) is unchanged; its regression test still passes.

## Diff summary

- Code/content commits: pending final squash SHA from reintegration receipt
- Summary artefact commit: intentionally omitted; this file must not self-reference its own mutable SHA
- Files touched: `src/lib.rs`, `.cacophony/agent/aurora-mcp-cli-mcp-cli-dev-aur-0/summary/pending/summary.md`
- Tests: +2 unit tests (20 to 22): one proves an invalid request object (with id and with no id) yields `-32600` and the session still answers a following `ping`; one proves invalid `tools/call` params yields `-32602` and the session still answers a following `ping`.
- Behavioural delta: invalid-but-parseable JSON-RPC input now produces spec-correct JSON-RPC error responses and the stdio session survives, instead of the connection being dropped on the first bad message.

## Operator-takeaway

The stdio server only degraded gracefully for unknown methods; any other malformed-but-parseable message killed the whole session. It now answers `-32600`/`-32602` and keeps serving, which matters because a single buggy client message previously dropped all subsequent valid requests. Non-JSON garbage lines are still intentionally fatal (bd-c7aeca); whether those should also become `-32700` + continue is left as a separate operator decision.
