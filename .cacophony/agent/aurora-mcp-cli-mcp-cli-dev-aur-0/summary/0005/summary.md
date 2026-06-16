# Session summary — Document the MCP protocol surface in README

## Goal

Document the MCP stdio protocol surface the crate actually implements, following the project profile's rule to update README.md when public APIs or behavior change. Recent work added protocol-version negotiation, a public supported-versions constant, and JSON-RPC error handling that were undocumented for consumers.

## Bead(s)

- `bd-b0314b` — Document the MCP protocol surface (methods, version negotiation, error codes) in README

## Before state

- Failing tests: none (24 unit + 1 doctest green).
- README covered JSON envelopes and tool registration but never described the MCP methods, NDJSON framing, protocol-version negotiation, or JSON-RPC error behavior.

## After state

- Failing tests: none. `cargo fmt --all -- --check` and `cargo test --workspace --all-features` green (24 unit + 1 doctest); README-only change.
- README now has an "MCP protocol support" section documenting NDJSON stdio framing; the supported methods (initialize with version negotiation, notifications/initialized, ping, tools/list, tools/call); the `SUPPORTED_PROTOCOL_VERSIONS` constant; and the session-preserving JSON-RPC error codes (-32600/-32601/-32602).

## Diff summary

- Code/content commits: pending final squash SHA from reintegration receipt
- Summary artefact commit: intentionally omitted; this file must not self-reference its own mutable SHA
- Files touched: `README.md`, `.cacophony/agent/aurora-mcp-cli-mcp-cli-dev-aur-0/summary/pending/summary.md`
- Tests: none (documentation-only change).
- Behavioural delta: none at runtime; documents the existing public MCP surface.

## Operator-takeaway

The README now reflects the MCP protocol surface that bd-909a82, bd-1bd8b6, and bd-c7aeca established (NDJSON framing, version negotiation, session-preserving JSON-RPC errors), so consumers of the published crate can see the supported methods and error semantics without reading the source.
