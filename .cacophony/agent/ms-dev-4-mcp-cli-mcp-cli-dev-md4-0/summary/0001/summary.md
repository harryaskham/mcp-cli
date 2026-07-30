# Session summary — the MCP stdio read loop stops being a liability

## Goal

Second chunk of the same session. Having landed `bd-9171bc` (malformed JSON no
longer ends the session), the two remaining ways a peer could kill or starve an
mcp-cli stdio server both lived in the same eight lines of `read_protocol_message`.
Rather than rewrite that function twice, I triaged both to open and did them as
one batch: bound the frame size, and stop the framing layer from validating UTF-8.

## Bead(s)

- `bd-bb6800` — Harden `read_protocol_message` against unbounded line length
  (4-week-old draft; promoted and claimed this session).
- `bd-854d61` — MCP stdio: invalid UTF-8 on the transport ends the session
  (`Io`/`InvalidData`); filed earlier this session, promoted and claimed once it
  was clear the same rework resolves it.

There is no controller in this project, so the promotions were done by me
through `caco bd triage --promote` and announced on the project broadcast with
an explicit offer to defer them back.

## Before state

- `main` at `1988e43` (the `bd-9171bc` fix), 28 tests + 1 doctest, green.
- `read_protocol_message` did `reader.read_line(&mut String)` in a loop:
  - no upper bound, so a line with no `\n` was buffered in full;
  - `read_line` validates UTF-8, so one stray byte returned
    `Err(io::ErrorKind::InvalidData)`, which propagated out of `serve_transport`
    and ended the session — precisely the teardown shape the `-32600` / `-32602`
    (`bd-909a82`) and `-32700` (`bd-9171bc`) work had removed above it.
- `McpServer` had no notion of a frame size limit.

## After state

- Commit `9d2481c` on the agent branch.
- Framing reads raw bytes: `Read::take(&mut *reader, cap + 1)` + `read_until`.
  No single frame buffers past the cap.
- New public surface: `DEFAULT_MAX_FRAME_BYTES` (16 MiB) and
  `McpServer::with_max_frame_bytes` / `max_frame_bytes`. Additive only —
  `StdioServerConfig` is untouched, so existing struct-literal construction
  (including the README doctest) still compiles.
- An oversized frame is drained to the next newline in 64 KiB chunks and
  answered with `-32700` naming the cap and the discarded byte count; the
  session resynchronises on the following frame.
- Invalid UTF-8 reaches `serde_json` as bytes and comes back as `-32700`
  instead of killing the session.
- 35 tests + 1 doctest pass; fmt and clippy clean (run unpiped).

## Diff summary

Files touched: `src/lib.rs`, `README.md`.

- `read_protocol_message` now returns a `ProtocolFrame` enum
  (`Message` / `Oversized(bytes)` / `Eof`) instead of `Option<Vec<u8>>`, so the
  serve layer can distinguish "no more input" from "input too big to parse".
- New helpers: `drain_to_frame_boundary` (bounded resync),
  `trim_frame_terminator` (byte-level `\r\n` trim), `oversized_frame_response`.
- `serve_transport`'s `while let` became a `loop`/`match` over the three
  framing outcomes.
- Seven tests added or reworked: frame exactly at the cap, one byte over plus
  resync, a never-terminated stream, a final frame with no trailing newline,
  raw invalid UTF-8 at the framing layer, and both new legs end-to-end through
  `serve_transport`.
- README documents the `-32700` cases (JSON, UTF-8, oversize) and the cap
  override.

Landed squash SHA will come from the reintegration receipt.

## Operator-takeaway

All three "one bad frame kills the server" paths are now closed and each has a
test: bad JSON, bad UTF-8, and oversized. The interesting part is that they were
one bug wearing three hats — `bd-909a82` fixed the top hat a month ago, and the
two underneath survived because each layer's fix looked complete from inside
that layer. Also worth noting: the beads primary flapped mid-claim and returned
an indeterminate response, so the claim was confirmed by re-reading the bead
rather than re-issuing it. Filed `bd-22bd0d` (cacophony) about queued bead
writes returning an outbox id that resolves to nothing.
