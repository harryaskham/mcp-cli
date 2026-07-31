# Changelog

Notable changes to `mcp-cli-core`, in the spirit of
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

**Entries are keyed by commit, not by released version.** This crate is consumed
by git rev or path/submodule rather than from a registry — the `mcp-cli` name on
crates.io belongs to an unrelated project, which is why the package was renamed
to `mcp-cli-core` — so `version` in `Cargo.toml` is not
something a consumer can pin to. A commit sha is. Each entry therefore carries
the landing sha and the bead that motivated it, which is also what you want when
bisecting.

Changes marked **BREAKING** alter existing behaviour. At `0.0.x` that is
permitted without a version bump, so this file is the only place a consumer can
learn about them.

## Unreleased

### Changed

- **BREAKING** — Tool registration now rejects, rather than silently accepting,
  registrations that cannot work. `add_tool` and the typed helpers **panic**;
  `try_add_tool` returns a structured error and leaves the router unchanged:
  - duplicate tool name (`duplicate_tool_name`) — previously the second
    registration was unreachable, because `tools/list` advertised the name twice
    and `call_tool` dispatched to the first match forever
    (`bd-daeb00`, `92fe9ab`).
  - input type whose derived schema has a non-object root
    (`invalid_input_schema`) — MCP constrains `inputSchema` to an object, so a
    scalar, sequence, or enum input advertised metadata strict clients reject
    (`bd-2308cb`, `92fe9ab`).
  - empty or whitespace-only name (`invalid_tool_name`) — previously surfaced
    only at call time as `TargetNotFound`, in the client, pointing at the caller
    (`bd-e6ed5d`, `5c155df`).

  Charset and length are deliberately **not** enforced; they are a downstream
  host's constraint. See the README portability note.

- **BREAKING** — `serve_transport` no longer returns `Err(McpCliError::Json)` on
  a malformed frame. It answers JSON-RPC `-32700` with a null `id` and keeps
  serving. Code matching that `Err` arm is now unreachable
  (`bd-9171bc`, `1988e43`).

- **BREAKING** — A panicking tool handler no longer unwinds out of `Tool::call`
  and ends the process. It is caught and returned as an `isError` envelope with
  code `tool_panicked`, and requests queued behind it are still served. Does
  nothing under `panic = "abort"`, and does not restore `Ctx` consistency
  (`bd-c18590`, `b8f782a`).

- **BREAKING** — Frames larger than `DEFAULT_MAX_FRAME_BYTES` (16 MiB) are
  rejected with `-32700` and drained to the next frame boundary. Previously any
  size was accepted given enough memory (`bd-bb6800`, `fd419d4`).

- Advertised `outputSchema` now declares a root `"type": "object"`, which MCP
  2025-06-18 requires. The `oneOf` over the success and error envelope variants
  is preserved. Wire-visible: a strict client that was rejecting the metadata
  will now accept it (`bd-cc4429`, `95d95af`).

### Added

- `try_add_tool` — fallible registration returning `JsonError`, for tool sets
  assembled from config, plugins, or user input (`bd-daeb00`, `92fe9ab`).
- `DEFAULT_MAX_FRAME_BYTES`, `McpServer::with_max_frame_bytes`, and
  `McpServer::max_frame_bytes` (`bd-bb6800`, `fd419d4`).
- JSON-RPC batch receive: an array frame executes every member and returns one
  array frame in request order. Notification members are omitted, an
  all-notification batch produces no frame, and an empty array is answered with
  a single `-32600` (`bd-55de1d`, `a4a4f49`).

### Fixed

- Invalid UTF-8 on the transport no longer ends the session. Framing reads raw
  bytes, so a bad byte becomes an ordinary `-32700` rather than
  `Io(InvalidData)` (`bd-854d61`, `fd419d4`).
- `notifications/initialized` carrying an `id` is answered instead of leaving
  the client blocked forever on a response that never comes. JSON-RPC 2.0 makes
  a request with an `id` one that must be answered (`bd-a9b422`, `970206a`).

### Removed

- The unused `tracing` dependency, never referenced since the crate extraction.
  Removes it from the transitive graph for anyone who was receiving it via this
  crate (`bd-0f31f3`, `73621eb`).

### Documentation

- README documents the registration invariants, the frame cap, the `-32700`
  cases, batch semantics, and the `outputSchema` shape.
- README and the `serve_stdio` / `serve_transport` docs warn that **stdout is
  the protocol channel**: anything a consumer installs that can reach file
  descriptor 1 — a logger or `tracing` subscriber left on its default sink, a
  custom panic hook, a stray `println!` — corrupts the stream with its own frame
  (`bd-ef614e`, `b2d875e`).
