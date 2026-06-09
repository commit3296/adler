#!/usr/bin/env python3
"""End-to-end smoke probe of adler-mcp's stdio surface.

Spawns `adler --mcp` as a subprocess, walks the full JSON-RPC
handshake (`initialize` → `notifications/initialized`), then
exercises every advertised tool, resource, and prompt with sensible
arguments. Doubles as a worked example of how an MCP host (Claude
Desktop, Cursor, any agent runtime) talks to the server.

Output is a `[PASS] / [FAIL] <label>` line per check; the script
exits non-zero if any check fails so it slots into CI smoke jobs.

Usage:

    # default: ./target/release/adler from the repo root
    python3 adler-mcp/examples/probe_stdio.py

    # override the binary location
    ADLER_BIN=/usr/local/bin/adler python3 .../probe_stdio.py

    # CI-safe: exercise stdio transport without live network probes
    ADLER_MCP_SKIP_LIVE=1 python3 .../probe_stdio.py

This probe is independent of the in-repo `cargo test` integration
suite (`adler-cli/tests/cli.rs::mcp_stdio_*`); it's intended for
hand-running against a built binary, and for use as a reference
implementation of a minimal MCP client over stdio.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time
from queue import Empty, Queue

BIN = os.environ.get("ADLER_BIN", "./target/release/adler")
SKIP_LIVE = os.environ.get("ADLER_MCP_SKIP_LIVE", "").lower() in ("1", "true", "yes")

# Test bookkeeping.
_results: list[tuple[bool, str]] = []


def ok(label: str, passed: bool = True, extra: str = "") -> None:
    """Record + print one check outcome."""
    _results.append((passed, label))
    mark = "PASS" if passed else "FAIL"
    print(f"  [{mark}] {label}" + (f"  {extra}" if extra else ""))


def _reader(stream, queue: Queue) -> None:
    for line in stream:
        line = line.strip()
        if line:
            queue.put(line)


def open_server() -> tuple[subprocess.Popen, Queue, Queue]:
    """Spawn `adler --mcp`, return process + (stdout queue, stderr queue)."""
    proc = subprocess.Popen(
        [BIN, "--mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    stdout_q: Queue[str] = Queue()
    stderr_q: Queue[str] = Queue()
    threading.Thread(target=_reader, args=(proc.stdout, stdout_q), daemon=True).start()
    threading.Thread(target=_reader, args=(proc.stderr, stderr_q), daemon=True).start()
    return proc, stdout_q, stderr_q


def recv_id(stdout_q: Queue, expect_id: int, timeout: float = 10.0):
    """Drain the stdout queue until we see a response with matching `id`.
    Progress notifications (no `id`, `method=notifications/progress`)
    are skipped — that's the normal MCP shape for streaming tools.
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            raw = stdout_q.get(timeout=max(0.05, deadline - time.time()))
        except Empty:
            break
        try:
            obj = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if obj.get("id") == expect_id:
            return obj
    return None


def main() -> int:
    proc, stdout_q, stderr_q = open_server()
    next_id = [1]

    def send(msg: dict) -> None:
        assert proc.stdin is not None
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()

    def call(method: str, params: dict | None = None, *, want_id: bool = True):
        msg: dict = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        if want_id:
            mid = next_id[0]
            next_id[0] += 1
            msg["id"] = mid
            send(msg)
            return recv_id(stdout_q, mid)
        send(msg)
        return None

    try:
        # === handshake ===
        print("\n== handshake ==")
        resp = call(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "probe", "version": "0.1"},
            },
        )
        info = resp["result"]["serverInfo"] if resp else {}
        ok(
            "initialize",
            info.get("name") == "adler-mcp",
            f"server v{info.get('version', '?')}" if info else "no response",
        )
        caps = resp["result"]["capabilities"] if resp else {}
        for cap in ("tools", "resources", "prompts"):
            ok(f"capability advertised: {cap}", cap in caps)
        call("notifications/initialized", want_id=False)

        # === tools ===
        print("\n== tools ==")
        resp = call("tools/list")
        tool_names = sorted(t["name"] for t in resp["result"]["tools"])
        expected = sorted([
            "list_sites", "scan_username", "scan_batch",
            "doctor_check", "get_scan_history", "diff_scans",
        ])
        ok("tools/list returns all 6", tool_names == expected, f"got {tool_names}")

        resp = call("tools/call", {"name": "list_sites", "arguments": {"tag": ["coding"]}})
        sc = resp["result"]["structuredContent"]
        ok(
            "tools/call list_sites(tag=coding)",
            sc["total"] > 5,
            f"{sc['total']} sites, first 3: {[s['name'] for s in sc['sites'][:3]]}",
        )

        resp = call("tools/call", {"name": "doctor_check", "arguments": {"site": "Reddit"}})
        sc = resp.get("result", {}).get("structuredContent", {})
        issues = [str(issue).lower() for issue in sc.get("issues", [])]
        ok(
            "tools/call doctor_check(Reddit) → session-required verdict",
            sc.get("site") == "Reddit"
            and sc.get("verdict") == "unhealthy"
            and any("session_required" in issue for issue in issues),
            f"verdict={sc.get('verdict')}, issues={len(issues)}",
        )

        if SKIP_LIVE:
            ok("tools/call doctor_check(GitHub) skipped (ADLER_MCP_SKIP_LIVE)")
        else:
            resp = call("tools/call", {"name": "doctor_check", "arguments": {"site": "GitHub"}})
            sc = resp.get("result", {}).get("structuredContent", {})
            ok(
                "tools/call doctor_check(GitHub) → live verdict",
                sc.get("site") == "GitHub" and sc.get("verdict") in ("healthy", "unhealthy"),
                f"verdict={sc.get('verdict')}, issues={len(sc.get('issues', []))}",
            )

        resp = call("tools/call", {"name": "get_scan_history", "arguments": {"limit": 5}})
        sc = resp["result"]["structuredContent"]
        ok(
            "tools/call get_scan_history",
            "total" in sc and "scans" in sc,
            f"total={sc['total']}",
        )

        if SKIP_LIVE:
            ok("tools/call scan_username skipped (ADLER_MCP_SKIP_LIVE)")
        else:
            # Live network scan via MCP — top=2 keeps it fast.
            resp = call(
                "tools/call",
                {
                    "name": "scan_username",
                    "arguments": {"username": "torvalds", "top": 2, "tag": ["coding"]},
                    "_meta": {"progressToken": "probe-stdio-1"},
                },
            )
            sc = resp.get("result", {}).get("structuredContent", {})
            ok(
                "tools/call scan_username(torvalds, top=2, tag=coding)",
                sc.get("total_probed", 0) >= 1,
                f"probed={sc.get('total_probed')}, "
                f"found={sc.get('summary', {}).get('found')}, "
                f"sites={[o['site'] for o in sc.get('outcomes', [])]}",
            )

        # === resources ===
        print("\n== resources ==")
        resp = call("resources/list")
        names = sorted(r["name"] for r in resp["result"]["resources"])
        ok(
            "resources/list returns 5 static",
            names == sorted([
                "registry_sites", "registry_tags",
                "registry_disabled", "scans_recent", "watchlist_default",
            ]),
            f"got {names}",
        )

        resp = call("resources/templates/list")
        templates = resp["result"]["resourceTemplates"]
        ok(
            "resources/templates/list returns scans/{id}",
            any(t["uriTemplate"] == "adler://scans/{id}" for t in templates),
        )
        ok(
            "resources/templates/list returns scan diff",
            any(t["uriTemplate"] == "adler://scans/{from}/diff/{to}" for t in templates),
        )
        ok(
            "resources/templates/list returns timeline",
            any(t["uriTemplate"] == "adler://timelines/{username}" for t in templates),
        )

        for uri in [
            "adler://registry/sites",
            "adler://registry/tags",
            "adler://registry/disabled",
            "adler://scans/recent",
            "adler://watchlists/default",
        ]:
            resp = call("resources/read", {"uri": uri})
            contents = resp["result"]["contents"]
            payload = json.loads(contents[0]["text"])
            total = payload.get(
                "total",
                payload.get("total_tags", payload.get("target_count", "?")),
            )
            ok(
                f"resources/read {uri}",
                bool(contents) and "text" in contents[0],
                f"total={total}",
            )

        resp = call("resources/read", {"uri": "adler://nope/x"})
        ok(
            "resources/read unknown URI → invalid_params",
            "error" in resp and "unknown resource" in resp["error"]["message"].lower(),
        )

        resp = call("resources/read", {"uri": "adler://scans/../etc/passwd"})
        ok(
            "resources/read path-traversal id → rejected",
            "error" in resp,
        )

        # === prompts ===
        print("\n== prompts ==")
        resp = call("prompts/list")
        pnames = sorted(p["name"] for p in resp["result"]["prompts"])
        ok(
            "prompts/list returns 3",
            pnames == sorted([
                "investigate_username",
                "audit_registry_health",
                "correlate_accounts",
            ]),
            f"got {pnames}",
        )

        for name, args in [
            ("investigate_username", {"username": "alice", "regions": "ru"}),
            ("audit_registry_health", {}),
            ("correlate_accounts", {"usernames": "alice,bob"}),
        ]:
            resp = call("prompts/get", {"name": name, "arguments": args})
            msgs = resp["result"]["messages"]
            text = msgs[0]["content"]["text"]
            has_arg = any(v in text for v in args.values()) if args else True
            ok(
                f"prompts/get {name}",
                len(msgs) == 1 and msgs[0]["role"] == "user" and has_arg,
                f"body {len(text)} chars",
            )

        resp = call("prompts/get", {"name": "investigate_username", "arguments": {}})
        ok(
            "prompts/get missing required arg → invalid_params",
            "error" in resp and "requires argument" in resp["error"]["message"].lower(),
        )

        resp = call("prompts/get", {"name": "nope"})
        ok(
            "prompts/get unknown name → invalid_params",
            "error" in resp and "unknown prompt" in resp["error"]["message"].lower(),
        )

    finally:
        assert proc.stdin is not None
        proc.stdin.close()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        banner: list[str] = []
        while True:
            try:
                banner.append(stderr_q.get_nowait())
            except Empty:
                break
        print("\n== stderr (server banner) ==")
        for line in banner[:3]:
            print(f"  {line}")

    # Final tally.
    passed = sum(1 for p, _ in _results if p)
    total = len(_results)
    print(f"\n== summary == {passed}/{total} PASS")
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
