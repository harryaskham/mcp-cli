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
- **CI gate is LIVE; PR backend is intended-default but BLOCKED on operator config (2026-06-29).** Harry directed: switch to PR mode, prefer PR auto-merge when available, gate may be temporarily disabled if it causes problems. State: CI (`.github/workflows/ci.yml`, bd-bcb4b8) runs cargo fmt/clippy/test on GitHub-hosted `ubuntu-latest` (public personal-account repo — no self-hosted pool needed), on `pull_request`+`push`+`merge_group`, green. Branch protection on `main` requires the `check` context (strict, `enforce_admins=false` escape hatch); `allow_auto_merge`+`allow_update_branch` enabled. PR landing uses `caco agent reintegrate --backend pull_request` (bd-c949e1) — dry-run is CLEAN — BUT real runs error `bd-1d514b: project 'mcp-cli' uses pull_request backend ... but projects[].integration is missing`. That `projects[].integration` config lives in the cacophony project; this worker is scope-bound to mcp-cli and CANNOT add it — an operator/controller must. DO NOT use `--mode pr_auto_merge` (daemon dead-code, bd-d58da8). UNTIL integration config is added, reintegrate via `caco agent reintegrate --backend local_merge` (direct merge; works). GitHub merge queue needs an org (unavailable); `merge_group` is pre-wired. One agent owns this; do not swarm.
- **Two workers share this project — on a drained board, self-filed work
  DUPLICATES.** Observed 2026-07-30: with an empty ready board, `ms-dev-3` and
  `ms-dev-4` independently found the same defect and both implemented it; the
  `-32700` parse-error fix was filed twice (bd-8afbc4 / bd-9171bc) and the loser
  discarded a complete, green implementation at the rebase. Self-improvement on a
  drained board is correct, but it is NOT solo work. Before implementing:
  `caco msg broadcast` the exact area you are taking, check
  `caco bd list --status in_progress` for the other worker's claim, and re-run
  `git fetch origin main && git log origin/main --oneline -5` immediately before
  deep implementation. Yield and close as concurrently-resolved (admin override,
  no commits) rather than re-landing a sibling fix.
- **`assignment source was first_class, not bead_store_reconciled` is a
  maintenance-window symptom, not a product bug.** `caco agent reintegrate` fails
  with `reintegration_assignment_binding_unavailable` (no publication attempted)
  while the beads primary is unreachable, because the local claim has not been
  reconciled into the authoritative bead store. The signal that it will bind
  again is a successful `caco bd sync`. Wait for that, then retry; do not file it.
- **`caco bd create` queues to the daemon outbox when the primary is down.** It
  returns `queued: ... outbox entry outbox-...` with NO bead id. The work can
  still proceed: commit with a bead-id-pending message and `git commit --amend`
  the id in once the outbox drains (poll with
  `caco bd list --assignee $CACO_AGENT_ID --status in_progress`). Reintegration
  cannot bind an assignment until the bead exists, so the amend must happen first.
- **`caco scratch append --text` rejects a value starting with `-`.** The leading
  dash is parsed as a flag (`error: unsupported flag: - start ...`). Start health
  note entries with a bracketed timestamp instead of a Markdown bullet dash.
- **PR-backend blocker is UNVERIFIED as of 2026-07-30.** A `--backend
  pull_request` attempt this session failed earlier in the pipeline (assignment
  binding, during a beads outage), so the previously documented
  `projects[].integration is missing` error was never reached and is neither
  confirmed nor refuted. `--backend local_merge` lands cleanly and CI stays green
  on the resulting push. Re-probe the PR backend when the fleet is healthy before
  assuming either state.
- **`incident-hold admission unavailable (HTTP 503, fail closed)` is the other
  half of the same outage.** When the authority node is down, `caco agent
  reintegrate` alternates between
  `reintegration_assignment_binding_unavailable` (assignment source was
  first_class) and `incident-hold admission unavailable ... fail closed`
  depending on how far the call gets. Both are `helsinki` being unreachable, not
  two separate problems, and neither is a product bug. A `caco bd sync` that
  returns cleanly is necessary but NOT sufficient — it can succeed while
  incident-hold admission is still 503. Retry on a slow cadence, keep working,
  and let local commits hold the work: nothing is at risk while it is committed
  on the agent branch. Do not hammer the endpoint and do not file it.
- **Lane-split with the other worker instead of racing on a drained board.**
  After the duplicated `-32700` work, `md3-0` and `md4-0` agreed a split by
  surface: router/envelope/schema vs dependency-surface/JSON-RPC-message-layer.
  Both then landed into `src/lib.rs` within the hour with zero conflicts and
  zero duplicated implementation. If you are one of two workers here and the
  board is empty, propose a split by surface in a directed message before you
  start auditing — it is cheaper than the broadcast-then-discover-collision
  path, and much cheaper than discarding a finished implementation at rebase.
- **A bead gated on a required check is NOT closeable until that check is
  green, whoever landed it.** Confirming the sha reached `main` confirms the
  land, not the gate. Observed 2026-07-31: bd-a9b422 landed at `970206a`,
  `git ls-remote` confirmed the sha on true GitHub, and the bead was closed —
  while the required CI check was FAILING on that exact commit. The next worker
  rebased onto the red tip, re-ran tests but not clippy, and landed bd-cc4429 on
  top; two failing runs and a cross-worker handoff (bd-46f7d2) followed. After
  reintegration run `gh run list --repo harryaskham/mcp-cli --limit 1` and
  confirm `success` for YOUR commit before `caco bd close`. If `gh` is
  rate-limited (HTTP 403), wait and re-check — do not close blind. Letting
  hosted CI own validation is only sound if you actually read the verdict.
- **Rebasing invalidates the earlier clippy run exactly as much as the earlier
  test run.** Re-running only `cargo test` after a rebase is how the second
  worker shipped over an already-broken lint.
- **Under `local_merge` / direct landing the CI gate is POST-HOC.** Branch
  protection requires the `check` context on pull requests, but a direct land
  never opens a PR, so nothing blocks a red commit from reaching `main` — CI only
  reports after the fact. Escalated as `choice-019fb686` and RESOLVED
  2026-07-31 by operator-proxy: unblock the PR backend, because it is the only
  option that is actually a gate and the pre-merge machinery is already built
  (CI runs on `pull_request`, branch protection already requires the check).
  Note bd-1d514b is CLOSED — the direct-intent-over-PR-backend machinery exists;
  the only missing piece is the `projects[].integration` entry, which is
  operator-routed and not a worker's to make. Do not wait on it to keep working;
  if it has not landed within a few hours, say so rather than quietly absorbing
  the risk.
- **MANDATORY INTERIM until that config is live (operator-proxy, not
  optional):** run `cargo clippy --workspace --all-targets --all-features --
  -D warnings` IMMEDIATELY before reintegrating, and read the CI verdict after
  landing before closing the bead. This knowingly re-adds part of the local
  preflight the hosted-CI lane asks us to skip; that is the correct trade while
  the gate is absent, and it stops the moment the gate exists.
- **`clippy::pedantic` is on, so `too_many_lines` (100) bites long match-based
  dispatch.** `handle_request` crossed it at 104 lines by gaining one small arm.
  Extract an arm into its own method rather than reaching for an `allow`.
- **A green suite is not evidence for an invariant that has no test.** When
  refactoring, enumerate the behavioural invariants FIRST and check that each one
  actually has coverage on the path being moved. Doing this on bd-46f7d2 found
  that `tools/call` sent as a NOTIFICATION (no id, runs the tool, returns no
  response) had no test at all — the exact arm being extracted, so a green run
  was not evidence for the thing it most needed to prove.
- **Heavy context: self-improve then /self-compact, don't recreate.** Operator
  guidance (helsinki:cacophony:harry): when context is heavy, prefer
  `/self-compact` over agent recreation. Use the rich context first to capture
  profile fixes / notes / draft beads, then `/self-compact` to continue. This is
  consistent with the endless mixin's "do not self-initiate a handoff/recreation"
  rule — `/self-compact` is runtime compaction, not a fresh-agent handoff.
