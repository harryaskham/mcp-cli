# Session summary — MCP stdio NDJSON framing fix

## Goal

Fix the operator-reported connection hang where apps built on `mcp-cli` got stuck connecting under `tendril mcp` / `tendril mcp stdio`, and make typed garbage on stdin fail visibly instead of being silently consumed.

## Bead(s)

- `bd-c7aeca` — stdio transport must use newline-delimited JSON (MCP), not Content-Length framing

## Before state

- Failing tests: none in the self-consistent test suite.
- Relevant metrics: 19 unit tests plus 1 doctest passed, but all stdio transport tests used LSP-style `Content-Length` framing.
- Context: Real MCP clients use newline-delimited JSON on stdio. `mcp-cli` instead expected `Content-Length` headers, so a client sending a normal JSON-RPC line could be treated as a header line and leave both sides waiting. Manually typed garbage also did not error because it was swallowed as header text.

## After state

- Failing tests: none.
- Relevant metrics: 20 unit tests plus 1 doctest pass. `cargo fmt --all -- --check`, `cargo test --workspace --all-features`, `cargo build --workspace --all-features`, and `cargo clippy --workspace --all-targets --all-features -- -D warnings` are all green.
- Context: The stdio transport now reads and writes MCP newline-delimited JSON. Blank separator lines are tolerated, clean EOF still returns `None`, non-JSON stdin now surfaces a JSON parse error, and all stdio helper tests speak the same NDJSON dialect as the MCP SDK.

## Diff summary

- Code/content commits: pending final squash SHA from reintegration receipt
- Summary artefact commit: intentionally omitted; this file must not self-reference its own mutable SHA
- Files touched: `src/lib.rs`, `.cacophony/agent/aurora-mcp-cli-mcp-cli-dev-aur-0/summary/pending/summary.md`
- Tests: +1 net unit test (19 to 20), with the old Content-Length framing tests replaced by NDJSON framing coverage and a regression test for garbage stdin.
- Behavioural delta: `serve_stdio` / `serve_transport` now implement MCP's newline-delimited JSON stdio framing instead of LSP `Content-Length` framing, which should unblock `tendril mcp` and standard MCP clients.

## Operator-takeaway

The hang was caused by a transport-level protocol mismatch: the crate's tests and server both used `Content-Length`, but real MCP stdio clients send one JSON-RPC message per newline. The implementation and tests now match the official MCP SDK's stdio framing, so connection handshakes should no longer stall before `initialize` is parsed.
