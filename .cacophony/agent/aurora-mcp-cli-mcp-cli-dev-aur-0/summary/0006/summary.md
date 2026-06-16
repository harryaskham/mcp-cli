# Session summary — Opt-in tool outputSchema (MCP 2025-06-18)

## Goal

Let typed tools advertise an `outputSchema` (JSON Schema for their structured output) so MCP clients can validate `structuredContent`, per the 2025-06-18 revision the server now negotiates — without breaking existing consumers. Implemented after Harry gave blanket approval, which unblocked the API decision the feature was deferred on.

## Bead(s)

- `bd-5221f5` — Advertise tool outputSchema and optionally validate structuredContent (MCP 2025-06-18)

## Before state

- Failing tests: none (24 unit + 1 doctest green after the crash-recovery audit; HEAD c5833a0).
- `ToolMetadata` carried only `input_schema`; `tools/list` never advertised an output schema, so clients could not validate tool results.

## After state

- Failing tests: none. `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features` are all green (25 unit + 1 doctest).
- `ToolMetadata` has a new optional `output_schema: Option<Value>` (serialized camelCase `outputSchema`, omitted when `None`).
- New opt-in constructors `Tool::new_typed_with_output_schema` and `ToolRouter::add_typed_tool_with_output_schema` require `Output: JsonSchema` and advertise the generated schema. Existing `new_typed` / `add_typed_tool` are unchanged (no `Output: JsonSchema` bound), so no consumer breaks.

## Diff summary

- Code/content commits: pending final squash SHA from reintegration receipt
- Summary artefact commit: intentionally omitted; this file must not self-reference its own mutable SHA
- Files touched: `src/lib.rs`, `README.md`, `.cacophony/profiles/mcp-cli.md`, `.cacophony/agent/aurora-mcp-cli-mcp-cli-dev-aur-0/summary/pending/summary.md`
- Tests: +1 unit test (24 to 25): `typed_tool_can_advertise_output_schema` asserts a tool registered with the new constructor advertises an object output schema (camelCase `outputSchema`) and that a plain tool omits the field.
- Behavioural delta: tools registered via the opt-in path now advertise an `outputSchema` in `tools/list`; all existing registration paths are byte-for-byte unchanged.
- Also: README documents the optional output schema; profile gains a note that piping `cargo fmt --check` through `tail` masks its exit code.

## Operator-takeaway

This adds 2025-06-18 `outputSchema` support as a strictly additive opt-in: consumers that want client-side result validation call the `*_with_output_schema` variants (and derive `JsonSchema` on their outputs); everyone else is unaffected. Chosen over forcing a `JsonSchema` bound on all tool outputs specifically to avoid breaking existing consumers like tendril.
