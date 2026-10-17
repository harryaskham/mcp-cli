#!/usr/bin/env python3
"""Record the real output the site quotes.

    python3 scripts/site/capture.py            # demo + tests (needs cargo and network for crates)
    python3 scripts/site/capture.py demo       # only the demo session

1. Builds scripts/site/demo, a tiny host with three tools (add, divide, crash), and records its CLI
   output and one MCP stdio session: each request frame and the server's reply.
2. Runs `cargo test` for this crate and for the four companion crates (cloned at their current
   main), recording passed/failed counts and the commit tested.

Writes scripts/site/data/{demo.json, tests.json}.
"""
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
DATA = HERE / "data"
COMPANIONS = ["configurable-cli", "updatable-cli", "feedback-cli", "remote-cli"]

SESSION = [
    {"jsonrpc": "2.0", "id": 1, "method": "initialize",
     "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "site", "version": "1"}}},
    {"jsonrpc": "2.0", "method": "notifications/initialized"},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "math_add", "arguments": {"lhs": 2, "rhs": 3}}},
    {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "math_divide", "arguments": {"lhs": 1, "rhs": 0}}},
    "{not json",
    {"jsonrpc": "2.0", "id": 5, "method": "resources/list"},
    {"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "math_add", "arguments": {"lhs": "two"}}},
    {"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "crash", "arguments": {"lhs": 0, "rhs": 0}}},
    {"jsonrpc": "2.0", "id": 8, "method": "ping"},
]


def frame(x):
    return x if isinstance(x, str) else json.dumps(x, separators=(",", ":"))


def demo(env):
    target = os.path.join(env["TMP"], "target")
    subprocess.run(["cargo", "build", "-q", "--manifest-path", str(HERE / "demo" / "Cargo.toml"), "--target-dir", target],
                   check=True, env=env)
    exe = os.path.join(target, "debug", "mcp-cli-site-demo")
    run = lambda args, stdin=None: subprocess.run([exe, *args], input=stdin, capture_output=True, text=True, env=env)
    cli = {name: {"args": args, "stdout": (r := run(args)).stdout.strip(), "exit": r.returncode}
           for name, args in (("add", ["add", "2", "3"]), ("divide", ["divide", "1", "0"]))}
    r = run(["mcp"], "\n".join(frame(f) for f in SESSION) + "\n")
    replies = [json.loads(l) for l in r.stdout.splitlines() if l.strip()]
    by_id, nulls = {}, []
    for rep in replies:
        (nulls.append(rep) if rep.get("id") is None else by_id.__setitem__(rep["id"], rep))
    turns = []
    for f in SESSION:
        if isinstance(f, str):
            turns.append({"request": f, "reply": nulls.pop(0)})
        elif "id" in f:
            turns.append({"request": frame(f), "reply": by_id[f["id"]]})
        else:
            turns.append({"request": frame(f), "reply": None})
    stderr = [l for l in r.stderr.splitlines() if "panicked" in l or "always panics" in l]
    return {"cli": cli, "session": turns, "stderr": stderr}


def tests(env):
    out = {}
    with tempfile.TemporaryDirectory() as tmp:
        dirs = {"mcp-cli": ROOT}
        for name in COMPANIONS:
            d = Path(tmp) / name
            subprocess.run(["git", "clone", "-q", "--depth", "1", f"https://github.com/harryaskham/{name}", str(d)],
                           check=True, env=env)
            dirs[name] = d
        for name, d in dirs.items():
            rev = subprocess.run(["git", "-C", str(d), "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
            r = subprocess.run(["cargo", "test", "--all-features", "--target-dir", os.path.join(tmp, "target")],
                               cwd=d, capture_output=True, text=True, env=env)
            text = r.stdout + r.stderr
            passed = sum(int(m) for m in re.findall(r"test result: \w+\. (\d+) passed", text))
            failed = sum(int(m) for m in re.findall(r"(\d+) failed", " ".join(re.findall(r"test result:[^\n]*", text))))
            out[name] = {"commit": rev, "passed": passed, "failed": failed, "ok": r.returncode == 0}
            print(name, out[name], flush=True)
    return out


def main():
    which = set(sys.argv[1:]) or {"demo", "tests"}
    DATA.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        env = {**os.environ, "TMP": tmp, "CARGO_TERM_COLOR": "never"}
        if "demo" in which:
            (DATA / "demo.json").write_text(json.dumps(demo(env), indent=1) + "\n")
        if "tests" in which:
            (DATA / "tests.json").write_text(json.dumps(tests(env), indent=1) + "\n")


if __name__ == "__main__":
    main()
