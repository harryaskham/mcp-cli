# mcp-cli profile

Use this profile for work on the standalone `mcp-cli` crate, the generic Rust
framework for projecting CLI command implementations into structured JSON and MCP
stdio tools.

## Scope and design rules

- Keep the crate application-agnostic. Do not add Tendril-specific types,
  window/display vocabulary, platform adapters, daemon assumptions, or CLI names.
- Consumers should provide typed input structs (`Deserialize` + `JsonSchema`),
  serializable outputs, and domain errors implementing `StructuredError`.
- Prefer reusable framework primitives: `JsonEnvelope`, `JsonError`,
  `ToolRouter`, typed `Tool` registration, and `McpServer` framing.
- Preserve CLI/MCP parity: if a helper changes JSON output semantics, add or
  update tests that compare a sample CLI envelope with a routed MCP tool call.
- Keep MCP protocol support minimal and standards-shaped. New protocol methods
  should be generic and covered by framed stdio tests.

## Validation

Before reintegration, run targeted smoke checks from the crate checkout:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

If this crate is consumed as a submodule, also validate the consuming workspace
builds against the pinned submodule commit.

## Documentation expectations

- Update `README.md` when public APIs or integration patterns change.
- Show generic examples rather than examples tied to one consuming project.
- If a consuming project needs project-specific guidance, document that in the
  consuming project, not here.
