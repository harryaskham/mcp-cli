# Session summary — Lock MCP stdio reconnect-safe contract

## Goal

Respond to Harry's reconnect design point (a same-agent MCP reconnect failing to be re-acknowledged) by confirming and locking mcp-cli's reconnect-safe behavior as the reference contract the cacophony precursor binary should match.

## Bead(s)

- `bd-b22230` — Lock MCP stdio reconnect-safe contract: idempotent repeated initialize keeps serving

## Before state

- Failing tests: none (25 unit + 1 doctest before this change).
- Investigation: `McpServer` is stateless per-message (only config+router; no per-connection / 'already-initialized' state). A repeated `initialize` is already answered idempotently and the session keeps serving, but there was no regression test locking that reconnect contract.

## After state

- Failing tests: none. `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features` are all green (26 unit tests + 1 doctest).
- New test `stdio_server_reinitialize_is_idempotent_and_keeps_serving` sends initialize -> notifications/initialized -> re-initialize -> ping on one serve_transport session and asserts both initialize calls return well-formed results (serverInfo + protocolVersion, no 'already initialized' error) and the session keeps serving the following ping.

## Diff summary

- Code/content commits: pending final squash SHA from reintegration receipt
- Summary artefact commit: intentionally omitted; this file must not self-reference its own mutable SHA
- Files touched: `src/lib.rs`, `.cacophony/agent/aurora-mcp-cli-mcp-cli-dev-aur-0/summary/pending/summary.md`
- Tests: +1 unit test (25 to 26). No production code change — documents/locks existing stateless behavior.
- Behavioural delta: none at runtime; the reconnect-safe contract is now regression-tested.

## Operator-takeaway

mcp-cli's stdio server is stateless per message, so it already accepts a same-agent reconnect / repeated initialize idempotently and never tears the session down for it. The "fails to reacknowledge" behavior Harry saw is in the cacophony precursor binary or the spawn/transport layer, not mcp-cli; this test pins mcp-cli's behavior as the reference the precursor should mirror (or solve harness-side via auto caco agent reconnect-mcp).
