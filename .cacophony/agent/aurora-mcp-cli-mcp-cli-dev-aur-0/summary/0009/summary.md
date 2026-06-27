# Session summary — outputSchema must describe the structuredContent envelope

## Goal

Fix a spec-correctness bug in the opt-in outputSchema feature (bd-5221f5): the advertised `outputSchema` did not match the `structuredContent` that `tools/call` actually returns, so a spec-compliant MCP client validating one against the other would reject conformant responses.

## Bead(s)

- `bd-870183` — tools/call structuredContent must conform to advertised outputSchema (envelope mismatch)

## Before state

- `tools/call` returns `structuredContent` = the full `JsonEnvelope` (`{status, meta, data}` / `{status, meta, error}`).
- `new_typed_with_output_schema` advertised `outputSchema = schema_for!(Output)` — the bare `data` payload schema only. Mismatch: MCP 2025-06-18 requires structuredContent to conform to the declared outputSchema.

## After state

- `outputSchema` is now `schema_for!(JsonEnvelope<Output>)` — it describes the real envelope wrapping `Output`, so structuredContent conforms.
- `JsonSchema` derived for `ErrorCategory`, `EnvelopeMeta`, `JsonError`, and `JsonEnvelope<T>` (additive trait impls; non-breaking). structuredContent shape is unchanged, so consumers (e.g. tendril) are unaffected.
- Tests: updated `typed_tool_can_advertise_output_schema` to assert the envelope-shaped schema (tolerant of schemars $ref/oneOf layout); added `advertised_output_schema_describes_tools_call_structured_content` proving every real structuredContent key is named in the advertised schema. README MCP section documents the envelope/outputSchema conformance.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features` all green (27 unit tests + 1 doctest).

## Diff summary

- Code/content commits: pending final squash SHA from reintegration receipt
- Summary artefact commit: intentionally omitted; must not self-reference its own mutable SHA
- Files touched: `src/lib.rs`, `README.md`, `.cacophony/agent/aurora-mcp-cli-mcp-cli-dev-aur-0/summary/pending/summary.md`
- Tests: +1 unit test (26 to 27); one existing test's assertions tightened to the envelope schema.
- Behavioural delta: advertised `outputSchema` content changes for tools using the opt-in helper; runtime structuredContent unchanged.

## Operator-takeaway

The opt-in outputSchema feature now actually round-trips: the schema a tool advertises matches the structuredContent mcp-cli returns, so MCP clients that validate structured results against the schema will accept conformant envelopes instead of rejecting them. No change to the wire shape of structuredContent.
