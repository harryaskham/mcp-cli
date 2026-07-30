# Session summary — MCP stdio no longer dies on one bad line

## Goal

First session of this persistent worker. No prior summaries existed and the
mcp-cli board was fully drained, so instead of idling I audited the crate's
stdio transport against the JSON-RPC 2.0 / MCP contract. That audit turned up a
live conformance bug rather than polish: a single malformed frame terminated the
whole MCP session. The session's goal became closing that gap and pinning the
correct behaviour with tests.

## Bead(s)

- `bd-9171bc` — MCP stdio: malformed JSON line kills the session instead of
  returning `-32700` Parse error (P2 bug, filed and claimed this session).

Related history: `bd-909a82` made invalid *request objects* (`-32600`) and
invalid *params* (`-32602`) non-fatal, but never covered the parse layer
underneath them.

## Before state

- `main` at `2e4efc1`, green: 27 unit tests + 1 doctest, fmt and clippy clean.
- `McpServer::serve_transport` parsed each framed line with
  `serde_json::from_slice(&message)?`, so a non-JSON line propagated
  `McpCliError::Json` out of `serve_stdio` and ended the session.
- That behaviour was *asserted* by
  `stdio_server_rejects_non_json_input_instead_of_hanging`, which expected
  `Err(McpCliError::Json(_))`. It was written to fix an older hang; erroring out
  beat hanging but is not the protocol-conformant outcome.
- `README.md` hedged this with "Malformed-**but-parseable** input is answered
  with a JSON-RPC error while the session keeps serving" — the qualifier was
  covering for the missing `-32700` leg.

## After state

- Commit `b2cb038` on the agent branch.
- A frame that is not valid JSON now produces
  `{"jsonrpc":"2.0","id":null,"error":{"code":-32700,...}}` and the read loop
  continues; only I/O and write failures still end the session.
- 28 unit tests + 1 doctest pass; `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean
  (run unpiped).

## Diff summary

Files touched: `src/lib.rs`, `README.md` (+84/-11 in the code commit).

- `serve_transport` no longer uses `?` on the frame parse; it matches and, on
  failure, writes a `-32700` response and keeps reading.
- New private helper `parse_error_response(&serde_json::Error) -> Value`,
  documented alongside the existing `invalid_request_response`, always emitting
  a null `id` (nothing is recoverable from an unparseable frame).
- Test flipped, not deleted: `stdio_server_rejects_non_json_input_instead_of_hanging`
  became `stdio_server_answers_non_json_input_with_parse_error_and_keeps_serving`,
  and now also asserts that a `ping` sent after the garbage line is answered.
- Test added: `stdio_server_parse_error_does_not_consume_following_frames_silently`
  proves a truncated object is classified `-32700` (not `-32600`) and that the
  following `tools/call` still returns `sum: 5`.
- README error table gains the `-32700` row and drops the "but-parseable" hedge.

Landed squash SHA will come from the reintegration receipt.

## Operator-takeaway

The `-32600` / `-32602` fixes from `bd-909a82` were sitting on top of a parse
layer that still killed the session, so the "server survives bad input" property
they advertised was only true for input that already parsed as JSON. It is true
now, and a test pins each of the three legs. Worth noting the board being empty
is not the same as the crate being done — this bug was one `grep` deep in the
happy path.
