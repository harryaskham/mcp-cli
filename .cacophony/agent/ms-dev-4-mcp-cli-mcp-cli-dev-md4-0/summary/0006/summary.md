# Session summary — auditing the seam instead of the halves

## Goal

Board empty, my lane audited out, and `md3-0`'s too. Rather than manufacture
another defect hunt inside `src/lib.rs`, I looked at what the lane split had
left unowned. Seventeen commits had landed across two days, six of them
behavioural, with `version` unchanged at `0.0.1` and no changelog anywhere.

## Bead(s)

- `bd-57c17b` — 17 commits with breaking behavioural changes landed at 0.0.1
  with no changelog and no version bump (filed as a draft with the open question
  named, promoted once `md3-0` answered it, claimed, implemented).

## Before state

- `main` at `7c7dcf4`, green: 54 tests + 1 doctest.
- No `CHANGELOG.md`. `Cargo.toml` at `0.0.1`, unchanged through every landing.
- Six consumer-visible behaviour changes with no record: registration now panics
  at startup on three distinct conditions, `serve_transport` stopped returning
  `Err` on malformed JSON, handler panics stopped ending the process, a 16 MiB
  frame cap appeared, `outputSchema` gained a root type, and `tracing` left the
  dependency graph.

## After state

- Commit `11857c2`.
- `CHANGELOG.md` in Keep a Changelog form, Unreleased section, entries grouped
  Changed / Added / Fixed / Removed / Documentation, each carrying its landing
  sha and motivating bead. Breaking entries flagged inline.
- 54 tests + 1 doctest, clippy and fmt clean on the rebased tree.

## Diff summary

New file `CHANGELOG.md`; no source changes.

Keyed by **commit rather than version**, which was the substantive decision.
`md3-0` established that the `mcp-cli` name on crates.io belongs to an unrelated
project — "Interactive CLI debugger and TUI for MCP servers", `conikeec/mcp-probe`,
0.3.0, first published 2025-06-21 — and I verified it against the registry API
rather than taking it. So this crate has never been published and cannot be
under that name; consumption is by git rev or path, and `Cargo.toml`'s version
is not something anyone can pin to. A sha is.

No version bump for the same reason: at `0.0.x` an unpublishable version number
is a worse marker than an honest commit range.

Every sha cited was checked reachable with `git merge-base --is-ancestor`, and
every bead id checked present in a mainline commit.

## Operator-takeaway

The find worth escalating is not the changelog, it is *why nobody had noticed*.
The lane split with `md3-0` — I took router/envelope/schema, they took the
dependency surface and the JSON-RPC message layer — worked exactly as designed:
eight beads, one file, zero collisions, after we had already burned one complete
green implementation racing blind. But a split partitions the thing it names and
silently orphans everything it does not. Release story, packaging, the crate
name, whether the thing is still describable from outside: none of that lives in
`src/lib.rs`, so neither lane owned it. It took someone deliberately looking at
the seam rather than at either half.

Two things fell out of that one look: the missing changelog, and the fact that
the crate is dressed for publication — description, license, keywords,
categories, homepage, documentation — under a name it cannot have. Someone
intended to publish this and that intent has been silently blocked the whole
time. `md3-0` is tracking the rename-or-stop-pretending question separately; it
is a live decision for the owner, not a detail.

I also caught myself in the session's recurring failure a third time. The first
changelog draft cited `55a74a8` and `eafe034` for two beads — my own agent-branch
commits, which never existed on `main`, since both landed squashed at `92fe9ab`.
I wrote them from memory of what I had committed rather than from what had
landed. Same shape as the rate-limit arithmetic earlier: asserting from what I
remembered instead of measuring. A changelog pointing at unreachable commits
fails at the single job it has, so every sha and bead in the file is now checked
programmatically.

Landed squash SHA will come from the reintegration receipt.
