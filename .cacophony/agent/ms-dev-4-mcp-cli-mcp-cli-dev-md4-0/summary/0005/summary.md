# Session summary — the largest blast radius left in the crate

## Goal

Board was empty and the process thread with `md3-0` had converged, so I went
looking for product work in my lane rather than polishing the account of how we
found the last thing. Probed one behaviour I had never measured: what a panicking
tool handler does to the server. It kills the process.

## Bead(s)

- `bd-c18590` — Decide whether a panicking tool handler should kill the server
  process (filed as a draft with three options, triaged to option 2 by `md3-0`,
  then promoted, claimed and implemented).

## Before state

- `main` at `617c2b7`, green: 52 tests + 1 doctest.
- Measured with a throwaway example: two tools registered, two framed
  `tools/call` in one stream, first handler panics. `serve_transport` never
  returns, no response frame is written for the panicking call *or* for the
  request already buffered behind it, and the client's only signal is a closed
  pipe — no code, no message, no indication of which tool failed.
- `md3-0` independently reproduced it in a separate crate before agreeing.

## After state

- Commit `33bcc27` on top of `617c2b7`.
- `Tool::call` catches an unwinding handler and returns
  `isError` / `tool_panicked` / `execution_failure` naming the tool and carrying
  the panic message. The session continues and queued requests are served.
- 54 tests + 1 doctest pass; clippy clean at `-D warnings`; fmt clean — all three
  run against the rebased tree, which is the specific thing I got wrong in
  `bd-46f7d2`.

## Diff summary

Files touched: `src/lib.rs`, `README.md`.

- `Tool::call` wraps the handler in `catch_unwind(AssertUnwindSafe(..))`; new
  private `panic_message` helper recovers `&'static str` and `String` payloads
  and reports anything else opaquely rather than guessing.
- Two tests: the envelope shape, and end-to-end proof that a request queued
  behind a panicking one is still answered.
- Doc comment and README carry both limits explicitly.

## Operator-takeaway

I filed this as a draft with a preference rather than a bead, and it was the
right call twice over. `md3-0` agreed with my conclusion and rejected my
argument for it, which improved the change: I had justified catching by saying
the router owns the call site, and `Iterator::map` owns the call site of a user
closure without catching, so the principle would have proved far too much. The
distinction that actually holds is an **outstanding obligation to a third
party** — a client blocking on an id, requests queued behind, a session expected
to outlive the call. That is why `axum` and `actix` catch and `map` does not, and
it scopes the catch to one boundary instead of licensing it anywhere the crate
touches consumer code.

The second reframing was better still: the Rust convention against catching
panics is really "never make a bug invisible", and the status quo — dead process,
closed pipe, no diagnostics — hides the failure far more thoroughly than an
`isError` envelope naming the tool does. The convention argues *for* the change,
not against it.

Both caveats are documented rather than left to be discovered, because catching
is a mitigation and not a guarantee: nothing is caught under `panic = "abort"`,
and a caught panic leaves `Ctx` exactly as the handler left it, so the session
continues against possibly-inconsistent consumer state. That is still the right
trade — a stranded client is certain, a corrupt `Ctx` is contingent — but it is
why the code reports a bug to fix rather than a condition to handle.

Eight defects now, and the shape has held from the first: the failure is
invisible at the place where it is caused. This was the largest instance and the
last one I can find in my lane.

Landed squash SHA will come from the reintegration receipt.
