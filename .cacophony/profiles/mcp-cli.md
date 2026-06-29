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
- **`origin` is the daemon-local mirror, not GitHub — verify load-sensitive
  landings on TRUE GitHub main.** This checkout's `origin` is
  `~/.cacophony/daemon/checkouts/mcp-cli` (a local mirror), so
  `git log origin/main --grep` can confirm against the mirror yet still miss a
  push to true GitHub. For a land that timed out / happened under load, also
  verify the canonical upstream: `git ls-remote
  ssh://git@github.com/harryaskham/mcp-cli.git refs/heads/main` and/or `gh api
  repos/harryaskham/mcp-cli/compare/main...<sha>` (compare status `identical` or
  `behind` = landed; `ahead`/`diverged` = NOT landed).
- **Always reintegrate SYNCHRONOUSLY; never `--async`.** Under host load
  `caco agent reintegrate --async` can SILENTLY FALSE-LAND — the detached process
  goes defunct, never registers in the merge-queue, and the agent falsely reports
  "landed" while the commit is never pushed (cluster-ctrl P1; fix bd-9a0041). Plain
  synchronous `caco agent reintegrate` lands cleanly even when the MCP response
  times out. Do not pass `--async` for reintegration.
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
- **Maintenance-window flaps are expected, not product bugs.** During fleet-wide
  node outages / Nix-update / TLS-restart windows you will see transient
  `beads proxy to the active primary is temporarily unavailable`,
  `endpoint failed before any semantic response`, and `msg send` backpressure /
  `accept_timeout`. Treat these as expected: do one bounded retry (or just wait
  for the next idle tick); do not tight-loop and do not file them as bugs.
- **Recurring fleet broadcasts are usually not your work.** Global broadcasts like
  `Continue on current goals, or disregard if not relevant`, `continue if you are
  mid-work`, and infra/PR-mode/Nix-cache status notes are not mcp-cli-directed.
  When you have no in-flight goal and an empty queue, disregard them quietly
  without spamming speak; only act on messages that name mcp-cli or a directed
  task.
- **PR-mode reintegration is permitted; CI is the gate (operator, 2026-06-29).** Harry: dev re-ints CAN use PR mode and the local cargo gate is not the right place — a GitHub Action keeps main green. For mcp-cli, prefer `caco agent reintegrate --mode pr_auto_merge` once the repo has CI runners. Until then mcp-cli has ZERO self-hosted runners (`gh api repos/harryaskham/mcp-cli/actions/runners` => total 0) and ci.yml stays `workflow_dispatch`-only; enabling push/pull_request triggers without runners would leave every check pending forever. Flip triggers + switch to PR mode together when mcp-cli joins the azure-ephemeral pool. One agent owns this; do not swarm.
- **Heavy context: self-improve then /self-compact, don't recreate.** Operator
  guidance (helsinki:cacophony:harry): when context is heavy, prefer
  `/self-compact` over agent recreation. Use the rich context first to capture
  profile fixes / notes / draft beads, then `/self-compact` to continue. This is
  consistent with the endless mixin's "do not self-initiate a handoff/recreation"
  rule — `/self-compact` is runtime compaction, not a fresh-agent handoff.
