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

## Operational notes (session-learned lessons)

Concrete lifecycle gotchas observed while operating as the persistent mcp-cli
worker. Additive; extend over time rather than pruning.

- **Direct cargo is allowed here.** This project does not intercept heavyweight
  commands, so run `cargo fmt`/`clippy`/`test` directly in the checkout for smoke
  validation; no `caco test run` queue is required.
- **Reintegrate timeout != failure.** `caco agent reintegrate` (MCP) can return a
  request timeout while the merge actually lands. Do NOT blindly retry: first run
  `git fetch origin main -q && git log origin/main --oneline --grep=<bead-id>`. If
  the squash commit is present, the reintegration succeeded — proceed to close.
- **Close-validator can 412 even when the work landed.** `caco bd close` may fail
  with `mainline_validation_failed` / upstream 412 "project checkout not available
  for mcp-cli" (authority-node infra), not a missing commit. After verifying the
  bead id is on `origin/main` via `git log --grep`, close with
  `--validate-on-main false`.
- **Remove autoinjected docs before reintegrating.** Startup injects untracked
  `AGENTS.md` / `CLAUDE.md`; the reintegrate uncommitted-changes guard refuses
  while they exist. They are regenerated, so `rm -f AGENTS.md CLAUDE.md` to clean
  the worktree before reintegrating (do not commit them).
- **`summary/pending` is cleaned by each direct reintegration.** Recreate it with
  `mkdir -p .cacophony/agent/$CACO_AGENT_ID/summary/pending` before writing the
  next session summary.
- **Worker scope is bound to mcp-cli.** This agent cannot read/write the
  `cacophony` project (e.g. `bd search --project cacophony` errors with
  "worker scope cannot access project 'cacophony'"); route cross-project asks to a
  controller/operator instead.
- **Do not pipe `cargo fmt --all -- --check` through `tail`/`head`.** A pipeline's
  exit status is the last command's, so `cargo fmt --check | tail` reports success
  even when rustfmt found a diff (exit 1). Run the check unpiped and inspect `$?`,
  or just run `cargo fmt --all` and `git diff` before committing.
