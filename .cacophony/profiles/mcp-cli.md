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
- **Worker scope is bound to mcp-cli, and cross-project bead CREATE is a
  ONE-WAY DOOR.** `caco bd list|search --project cacophony` errors with "worker
  scope cannot access project 'cacophony'", but the agent token DOES grant
  cacophony `create`/`update`. That asymmetry bites (observed 2026-07-31,
  md4-0's bd-22bd0d plus the compounding half):
  - a create during a beads outage returns only an outbox id, never a bead id;
  - without read scope you can then neither confirm it landed nor discover its
    id, so `update` is granted but unusable — you can file a bead you are
    permanently unable to revisit;
  - worse, the failure MASQUERADES AS NONEXISTENCE: `caco bd show --bead-id
    <a-real-cacophony-bead>` answers `bead not found`, not a scope error, which
    is indistinguishable from "the outbox dropped it" and invites the wrong
    inference that nothing was created.
  So: do not blind-overwrite a cross-project bead from a local copy of the text
  to add a paragraph — without read access you cannot tell whether someone has
  triaged it since, and clobbering a triage note is the worse trade. Put durable
  cross-project reasoning in THIS profile, which is readable and versioned, and
  route the cross-project ask to a controller/operator.
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
- **A lane split partitions the code and orphans everything that is not code.**
  The surface split (router/envelope/schema vs dependency-surface/JSON-RPC
  message layer) worked exactly as intended — eight beads, one file, zero
  collisions in a day — but its failure mode is structural: both workers audit
  inside `src/lib.rs` and nobody owns the release story, the packaging metadata,
  the changelog, or whether the crate is still describable from outside. Two
  real defects sat in that gap for a full day of active work (bd-57c17b:
  seventeen commits, six behavioural changes, no version bump and no changelog;
  bd-26b8a3: the crate name is taken on crates.io by an unrelated project, so
  the publication metadata implies a release that cannot happen). When running a
  lane split, periodically audit the SEAMS rather than the halves — it is not
  covered by either lane by construction, so it only happens if someone goes
  looking on purpose.
- **`cargo publish --dry-run` proves "packages", not "publishable".** It
  packages and compiles locally and never attempts the upload, so it cannot
  detect a name collision on the registry — it reported clean here while the name
  was already taken. Check the registry directly
  (`curl -s https://crates.io/api/v1/crates/<name>`) before treating a green
  dry-run as evidence about publication.
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
- **When the CI verdict looks UNREADABLE, try the other quota bucket before
  concluding anything — and if it really is dark, HOLD OPEN rather than close on
  local evidence.** `gh` returns HTTP 403 rate-limit against a token
  shared by the whole fleet, and it goes dark exactly when a worker most needs
  it: change finished, landed, locally verified, bead you are forbidden to
  close. The tempting resolution in that moment is to close anyway — which is
  precisely the behaviour the close-precondition exists to prevent — so
  exhaustion does not merely delay the discipline, it pressures workers off it.
  Do this instead:
  0. **First, try the GraphQL bucket — it is separate from REST core.** Observed
     2026-07-31: `gh run list` was hard 403 at `core 0/5000` while
     `graphql 4442/5000` was untouched. `gh api graphql` is still `gh`, so this
     is a first-party path and not a hand-rolled raw-API workaround. Batch
     several shas into one call with aliases:
     ```
     gh api graphql -f query='
     {
       repository(owner: "harryaskham", name: "mcp-cli") {
         a: object(oid: "<sha1>") { ... on Commit { statusCheckRollup { state } } }
         b: object(oid: "<sha2>") { ... on Commit { statusCheckRollup { state } } }
       }
     }'
     ```
     `state` is `SUCCESS` / `FAILURE` / `PENDING`; that IS the verdict, so a
     `SUCCESS` here licenses the close. GraphQL cost is measured, not inferred,
     on a bucket the fleet leaves untouched: one query aliasing FOUR shas costs
     exactly 1, so verification is priced per-QUERY not per-sha, and a query that
     fails validation costs 0 — so retry a rejected query freely, it is only your
     time at stake. Reproduced independently on both nodes (md3-0: graphql `used`
     42, 42 idle, 43 after one four-alias query, 43 after an invalid one).
     Pass FULL 40-character oids: a short sha
     fails with `argumentLiteralsIncompatible` / "Expected type 'GitObjectID'",
     which reads like a permissions or syntax fault rather than a truncation
     one, so expand first with `git rev-parse <short-sha>` and do not misdiagnose
     a five-second problem as an unreadable gate. `gh api rate_limit` tells you
     which buckets are live and is BOOTSTRAP-SAFE — it stays readable while core
     is 0/5000, which is how the exhaustion gets diagnosed at all. But do NOT
     check it before acting. ACT, and treat 403 as a first-class outcome; only
     then read `rate_limit` ONCE, diagnostically, to learn WHICH bucket died, and
     switch bucket or hold. A predictive check is worthless here: `used` is a
     whole-account counter the fleet drives at roughly 9 requests per SECOND from
     actors that are not you (measured 2026-07-31 by md4-0 at one-second
     intervals; ~1400-1600 per ten minutes seen independently here), so a reading
     of 4818 remaining is wrong seconds later and you cannot forecast it even one
     second out. Never poll it. That inverted sequence — act, hit the 403,
     diagnose the bucket, move to GraphQL — is exactly what actually worked; the
     exhaustion was never predicted by anyone.
     Its per-call cost is NOT measurable from a worker and should not be quoted:
     both workers initially misread fleet traffic as a per-call price and got 1,
     3, 2-3 and ~0 for the same command, including a six-reading run here with
     deltas 0, +1, -1, 0, 0 — a negative delta is the proof that consecutive
     readings sample the fleet, not you.
  Only if BOTH buckets are dark:
  1. Keep the bead open and claimed. An open bead costs a wait; a wrongly closed
     one silently deletes the work from the queue (see the BLOCKED != CLOSED
     section above). The costs are not symmetric, so hold.
  2. Record the landed sha in the bead plus the result of reproducing CI's exact
     command set locally on THAT sha — `cargo fmt --all -- --check`,
     `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
     `cargo test --workspace --all-features`. This is strong evidence but NOT
     the verdict: the hosted runner can carry a newer stable toolchain than the
     checkout, so a local pass does not license a close.
  3. Re-check when the limit resets (`gh` reports the window; core reset is
     typically under an hour) and close on the real verdict.
  4. If the limit persists long enough to strand finished work, escalate with
     the recorded evidence rather than choosing between closing blind and
     holding indefinitely. Do not hand-roll a raw API call to dodge `gh` — note
     that `gh api graphql` above is NOT that, it is a sanctioned `gh` path.
  Also avoid stacking further lands behind an unreadable gate: each one adds
  another bead you cannot close and spends more of the same budget.
- **Verification spends the budget it depends on.** Every land costs a compare
  call plus a run-list, and the (correct) guidance to verify on true GitHub
  rather than the lagging `origin` mirror multiplies that across every worker
  against one fixed fleet-wide ceiling, so the discipline scales its own cost
  linearly. That makes it a daemon-side problem rather than a per-project one:
  surfacing remaining budget in the reintegration receipt, and caching run
  conclusions by sha so N workers verifying one landed sha cost one call, both
  fix it at the right layer. Raised in the `cacophony` project by md4-0. The
  aliased-GraphQL form above also gives that cache a cheap implementation: one
  request can answer N shas at once.
- **Verification capability should be a fact a worker can READ, not something it
  probes for.** The close-precondition's enforceability turned out to depend on
  which of two quota buckets happened to be alive, and the second bucket was
  found by accident an hour after the rule that assumed the first was written.
  Generalised: nobody has enumerated which other budgets these controls silently
  depend on, so a worker cannot know in advance whether a control it is required
  to apply is currently applicable. Worse, CHECK-FIRST TELLS YOU ABOUT THE PAST:
  with the fleet spending 1400-1600 requests per ten minutes on the shared
  account, a healthy budget read minutes — or seconds — ago says nothing about
  the call you are about to make. A worker cannot fix that by measuring more
  carefully, because the quantity is not local to it. Until the daemon surfaces
  capability as a fact (remaining budget in the reintegration receipt being the
  obvious place, ideally with run conclusions cached by sha), read
  `gh api rate_limit` only AFTER a call fails, to diagnose which bucket died.
  The per-query pricing above removes the last objection to the cached form: one
  aliased query answers N shas for one request, so a daemon-side cache serving
  the whole fleet costs one request per interval REGARDLESS of how many shas or
  workers — it does not scale with fleet size at all.
- **Method: to measure a counter the fleet is moving, move to a bucket the fleet
  is not using, and start from a window boundary.** Core was climbing ~9/sec
  while GraphQL `used` sat dead flat, so the GraphQL bucket was the uncontended
  instrument hiding in the same token; taking the baseline just after its window
  reset gave a true zero to measure from. That is how the per-query and
  zero-cost-on-invalid facts above were established after four contaminated
  attempts on the contended counter. Generalises well past rate limits: find a
  quiet subsystem to benchmark in rather than trying to subtract the noise.
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
