# Session summary — mcp-cli profile: maintenance-window + broadcast operational notes

## Goal

Capture operational lessons from a long maintenance-heavy idle stretch into the project profile, per operator guidance to self-improve while context is rich and then self-compact instead of recreating the agent.

## Bead(s)

- (no implementation bead) — profile self-improvement in response to operator guidance (helsinki:cacophony:harry) to prefer /compact and use heavy context for profile fixes/notes.

## Before state

- Failing tests: none (no code change this reintegration).
- The mcp-cli profile's operational notes lacked guidance for: expected transient daemon/beads-proxy flaps during fleet maintenance windows, how to treat recurring non-directed fleet broadcasts, and the operator's prefer-/self-compact-over-recreation rule.

## After state

- Failing tests: none (docs/profile-only change).
- `.cacophony/profiles/mcp-cli.md` operational-notes section now also covers: maintenance-window flaps are expected (bounded retry, no tight-loop, not bugs); recurring fleet broadcasts (Continue on current goals / continue if mid-work / infra status) are usually not mcp-cli-directed and should be disregarded quietly; and heavy-context guidance to self-improve then `/self-compact` rather than recreate (consistent with the endless mixin).

## Diff summary

- Code/content commits: pending final squash SHA from reintegration receipt
- Summary artefact commit: intentionally omitted; this file must not self-reference its own mutable SHA
- Files touched: `.cacophony/profiles/mcp-cli.md`, `.cacophony/agent/aurora-mcp-cli-mcp-cli-dev-aur-0/summary/pending/summary.md`
- Tests: none (no code change).
- Behavioural delta: none at runtime; guidance for future mcp-cli agents only.

## Operator-takeaway

The mcp-cli board has stayed fully drained through a multi-day node outage and Nix-update window; this session was idle monitoring plus security-sweep and closure-audit responses. The profile now tells future workers to treat maintenance-window daemon flaps as expected, ignore non-directed fleet broadcasts without spamming, and self-compact rather than recreate when context grows.
