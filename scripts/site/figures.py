"""Figures and generated regions for the mcp-cli site, from the crates' code and data/*.json
(capture.py runs a demo host and every crate's tests)."""
import html
import json
from pathlib import Path

from diagrams import graph

DATA = Path(__file__).resolve().parent / "data"
METRICS = {"sans": 1.0, "mono": 0.6}


def load(name):
    return json.loads((DATA / name).read_text())


def surfaces():
    return graph(
        "surfaces", "One command, two surfaces",
        "How one typed command reaches both a person and an agent. The host registers a handler that takes a typed input "
        "and returns a typed output or a structured error. On the command line, write_json_result prints the result as a "
        "JSON envelope. Through the ToolRouter, the same handler becomes an MCP tool: McpServer answers tools/list with its "
        "name and a JSON Schema derived from the input type, and tools/call with the same envelope as structured content.",
        920, 300,
        nodes=[
            dict(id="h", x=150, y=150, label="your handler", sub=["Input → Result<Output, E>", "E: StructuredError"], kind="accent", w=210),
            dict(id="cli", x=440, y=70, label="CLI", sub="write_json_result", kind="b", w=170, mono=False),
            dict(id="router", x=440, y=230, label="ToolRouter", sub="schema from Input", kind="b", w=170),
            dict(id="env", x=760, y=70, label="JsonEnvelope on stdout", sub="status · meta · data | error", kind="ink", w=230),
            dict(id="mcp", x=760, y=230, label="McpServer (stdio)", sub=["tools/list · tools/call", "structuredContent = envelope"], kind="ink", w=230),
        ],
        edges=[
            {"from": "h", "to": "cli", "kind": "b", "out": "r", "in": "l", "label": "a person", "at": 0.62},
            {"from": "h", "to": "router", "kind": "b", "out": "r", "in": "l", "label": "an agent", "at": 0.62},
            {"from": "cli", "to": "env", "kind": "ink"},
            {"from": "router", "to": "mcp", "kind": "ink"},
        ],
    )


def stack():
    return graph(
        "stack", "The five crates",
        "How the crates fit together. A host CLI depends on mcp-cli-core, which provides the JSON envelope, structured "
        "errors, the tool router and the MCP stdio server. configurable-cli, updatable-cli and feedback-cli build on it: "
        "each adds one concern and registers matching MCP tools on the host's router (config_status and friends, "
        "self_update_status, check and run, feedback_status and feedback_report). remote-cli does not depend on "
        "mcp-cli-core; it is used alongside it, giving a host a daemon that owns a typed cache and serves it to the host's "
        "CLI and MCP processes.",
        920, 330,
        nodes=[
            dict(id="host", x=460, y=40, label="your CLI", sub="one binary: CLI + MCP", kind="accent", w=230),
            dict(id="cfg", x=120, y=150, label="configurable-cli", sub=["config show · init · import", "config_* tools"], kind="b", w=200, mono=True),
            dict(id="upd", x=350, y=150, label="updatable-cli", sub=["update · status", "self_update_* tools"], kind="b", w=200, mono=True),
            dict(id="fb", x=580, y=150, label="feedback-cli", sub=["errors and timings", "feedback_* tools"], kind="b", w=200, mono=True),
            dict(id="rem", x=810, y=150, label="remote-cli", sub=["daemon · cache · SSE", "alongside the core"], kind="c", w=200, mono=True),
            dict(id="core", x=400, y=280, label="mcp-cli-core", sub="envelope · StructuredError · ToolRouter · McpServer", kind="ink", w=420, mono=True),
        ],
        edges=[
            {"from": "host", "to": "cfg", "kind": "muted", "out": "b", "in": "t"},
            {"from": "host", "to": "upd", "kind": "muted", "out": "b", "in": "t"},
            {"from": "host", "to": "fb", "kind": "muted", "out": "b", "in": "t"},
            {"from": "host", "to": "rem", "kind": "muted", "out": "b", "in": "t"},
            {"from": "cfg", "to": "core", "kind": "ink", "out": "b", "in": "t", "ix": -130},
            {"from": "upd", "to": "core", "kind": "ink", "out": "b", "in": "t", "ix": -20},
            {"from": "fb", "to": "core", "kind": "ink", "out": "b", "in": "t", "ix": 90},

        ],
    )


def promotion():
    return graph(
        "promotion", "Replacing a running binary safely",
        "What updatable-cli's startup hook does with a staged update. It launches the candidate once with a validation "
        "argument and a timeout, and refuses it if it cannot run. It copies the current executable to tool_prev and records "
        "its sha256; if that copy fails, it refuses the update. It then swaps the candidate into place and re-executes. If "
        "the new binary fails to start, tool_prev is restored and re-hashed to prove the original bytes are back. A failure "
        "to restore is reported separately from a failure to start, with the preserved path for manual recovery.",
        920, 270,
        nodes=[
            dict(id="staged", x=80, y=70, label="tool_next", sub="staged, sha256 ok", kind="file", w=150, mono=True),
            dict(id="val", x=270, y=70, label="validate", sub="run once, timeout", kind="b", w=160),
            dict(id="prev", x=460, y=70, label="preserve", sub="copy to tool_prev", kind="b", w=160),
            dict(id="swap", x=650, y=70, label="promote", sub="swap, re-exec", kind="accent", w=150),
            dict(id="ok", x=840, y=70, label="running", sub="new version", kind="ok", w=130),
            dict(id="refuse", x=365, y=200, label="refused", sub="old binary untouched", kind="warn", w=200),
            dict(id="restore", x=745, y=200, label="restore tool_prev", sub="re-hash to prove it", kind="err", w=200),
        ],
        edges=[
            {"from": "staged", "to": "val", "kind": "ink"},
            {"from": "val", "to": "prev", "kind": "ink"},
            {"from": "prev", "to": "swap", "kind": "ink"},
            {"from": "swap", "to": "ok", "kind": "ok"},
            {"from": "val", "to": "refuse", "kind": "warn", "out": "b", "in": "t", "ox": 0, "ix": -40, "label": "won't run", "at": 0.45},
            {"from": "prev", "to": "refuse", "kind": "warn", "out": "b", "in": "t", "ix": 40, "label": "can't copy", "at": 0.45},
            {"from": "swap", "to": "restore", "kind": "err", "out": "b", "in": "t", "label": "fails to start", "at": 0.45},
        ],
    )


def remote():
    return graph(
        "remote", "One owner of the data, many readers",
        "remote-cli's shape. In the daemon, the host's collector fetches from the upstream API and commits each result to "
        "a shared snapshot with a monotonic revision, persisted by atomic, owner-only replacement of a cache file. The "
        "daemon serves the snapshot over HTTP or a Unix socket, always with a bearer token: GET /snapshot, a Server-Sent "
        "Events stream of new revisions, GET /health, POST /refresh and an optional POST /command for confirmed mutations. "
        "CLI and MCP processes subscribe as clients and write the received revisions through to their own cache. If the "
        "daemon is gone for longer than a timeout, one client takes a lease file and collects in its place until the daemon "
        "returns with a newer revision.",
        920, 330,
        nodes=[
            dict(id="api", x=90, y=70, label="upstream API", kind="muted", w=150),
            dict(id="col", x=280, y=70, label="collector", sub="host code", kind="accent", w=150),
            dict(id="snap", x=470, y=70, label="SharedSnapshot", sub="revision n", kind="b", w=170, mono=True),
            dict(id="cache", x=470, y=180, label="cache file", sub="atomic, 0600", kind="file", w=170),
            dict(id="srv", x=700, y=70, label="server", sub=["/snapshot · /events (SSE)", "/health · /refresh · /command"], kind="ink", w=230),
            dict(id="cli", x=700, y=262, label="CLI and MCP clients", sub=["subscribe, write through", "bearer token, any transport"], kind="c", w=230),
            dict(id="lease", x=250, y=262, label="fallback lease", sub="one client collects", kind="warn", w=180),
        ],
        edges=[
            {"from": "api", "to": "col", "kind": "muted"},
            {"from": "col", "to": "snap", "kind": "ink"},
            {"from": "snap", "to": "cache", "kind": "muted", "out": "b", "in": "t"},
            {"from": "snap", "to": "srv", "kind": "ink"},
            {"from": "srv", "to": "cli", "kind": "c", "out": "b", "in": "t", "label": "SSE revisions", "at": 0.5},
            {"from": "cli", "to": "lease", "kind": "warn", "out": "l", "in": "r", "style": "dash", "label": "daemon gone", "at": 0.45},
        ],
    )


FIGS = {"surfaces": surfaces, "stack": stack, "promotion": promotion, "remote": remote}


# ── generated regions ───────────────────────────────────────────────────────

def _compact(reply):
    """A reply as printed, with tools/list shortened to tool names and the duplicate text block elided."""
    if reply is None:
        return "(a notification: no reply)"
    r = json.loads(json.dumps(reply))
    res = r.get("result")
    if isinstance(res, dict) and "tools" in res:
        res["tools"] = [f'{t["name"]} …' for t in res["tools"]]
    if isinstance(res, dict) and "content" in res:
        res["content"] = ["… the same envelope as text …"]
    return json.dumps(r, separators=(",", ":"), ensure_ascii=False)


def session():
    d = load("demo.json")
    lines = []
    for t in d["session"]:
        lines.append(f'<span class="req">→ {html.escape(t["request"])}</span>')
        lines.append(f'<span class="rep">← {html.escape(_compact(t["reply"]))}</span>')
    return ('<pre class="session" tabindex="0" aria-label="MCP session"><code>' + "\n".join(lines) + "</code></pre>")


def cli():
    d = load("demo.json")["cli"]
    rows = []
    for k in ("add", "divide"):
        rows.append(f'<span class="p">demo {" ".join(d[k]["args"])}</span>')
        rows.append(f'<span class="o">{html.escape(d[k]["stdout"])}</span>')
    rows.append(f'<span class="c"># exit status {d["divide"]["exit"]}: the host decides what an error envelope exits with</span>')
    return '<pre tabindex="0"><code>' + "\n".join(rows) + "</code></pre>"


def tests():
    t = load("tests.json")
    rows = "".join(f'<tr><td><code>{html.escape(name)}</code></td><td class="num">{v["passed"]}</td>'
                   f'<td class="num">{v["failed"]}</td><td><code>{html.escape(v["commit"])}</code></td></tr>'
                   for name, v in t.items())
    return ('<table class="tests"><caption>cargo test --all-features, run by scripts/site/capture.py</caption>'
            '<thead><tr><th scope="col">Crate</th><th scope="col" class="num">Passed</th><th scope="col" class="num">Failed</th>'
            '<th scope="col">Commit</th></tr></thead><tbody>' + rows + "</tbody></table>")


GEN = {"session": session, "cli": cli, "tests": tests}


def demo_source():
    """The demo's tool registration and main, exactly as in scripts/site/demo/src/main.rs."""
    src = (Path(__file__).resolve().parent / "demo" / "src" / "main.rs").read_text()
    start = src.index("fn router()")
    return '<pre tabindex="0"><code>' + html.escape(src[start:].rstrip()) + "</code></pre>"


GEN["demo-source"] = demo_source
