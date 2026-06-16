# Session summary — Capture operational lessons in the mcp-cli profile

## Goal

Fold durable, session-learned lifecycle lessons into the project-local
`mcp-cli` profile so future persistent workers avoid the operational gotchas I
hit this session, per the operator's self-improvement request.

## Bead(s)

- (no implementation bead) — profile self-improvement in response to a broadcast
  asking agents to reintegrate important lessons into their profiles over time.

## Before state

- Failing tests: none (code unchanged this reintegration).
- `.cacophony/profiles/mcp-cli.md` had scope/design, validation, and docs
  sections but no operational-lifecycle notes, so each new worker rediscovered
  reintegrate/close/cleanup gotchas from scratch.

## After state

- Failing tests: none (docs/profile-only change).
- The profile now has an "Operational notes (session-learned lessons)" section
  covering: direct cargo is allowed here; reintegrate MCP timeout != failure
  (verify on origin/main before retrying); close-validator 412 fallback via
  --validate-on-main false after verifying the commit landed; removing
  autoinjected AGENTS.md/CLAUDE.md before reintegrating; recreating summary/pending
  each reintegration; and worker scope being bound to mcp-cli.

## Diff summary

- Code/content commits: pending final squash SHA from reintegration receipt
- Summary artefact commit: intentionally omitted; this file must not self-reference its own mutable SHA
- Files touched: `.cacophony/profiles/mcp-cli.md`, `.cacophony/agent/aurora-mcp-cli-mcp-cli-dev-aur-0/summary/pending/summary.md`
- Tests: none (no code change).
- Behavioural delta: none at runtime; this is guidance for future agents only.

## Operator-takeaway

The mcp-cli profile now records the real lifecycle friction I hit (reintegrate
timeouts that still land, close-validator 412s, autoinjection-file cleanup,
summary/pending recreation, and worker-scope boundaries), so the next worker
spends its time on the crate, not on rediscovering harness quirks.
