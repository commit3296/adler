#!/usr/bin/env python3
"""End-to-end smoke probe of adler-mcp's HTTP+SSE transport.

Spawns `adler --mcp-http 127.0.0.1:<port>` as a subprocess, exercises
the Streamable HTTP variant of MCP (POST JSON-RPC, parse SSE-framed
responses, carry `mcp-session-id` across requests), and walks the
same surface that `probe_stdio.py` covers — proves transport parity
between the two.

Output is a `[PASS] / [FAIL] <label>` line per check; the script
exits non-zero if any check fails.

Usage:

    python3 adler-mcp/examples/probe_http.py

    # override binary path / port
    ADLER_BIN=/usr/local/bin/adler ADLER_MCP_PORT=8888 python3 .../probe_http.py

    # CI-safe: exercise HTTP transport without live network probes
    ADLER_MCP_SKIP_LIVE=1 python3 .../probe_http.py

Reference implementation note: a real MCP HTTP client must walk the
SSE stream and filter by JSON-RPC `id`, because progress
notifications (`method=notifications/progress`) interleave with
the final response. See `parse_sse_response(..., want_id=N)` for the
canonical pattern.

Dependencies:

    pip install requests
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time

import requests

BIN = os.environ.get("ADLER_BIN", "./target/release/adler")
PORT = int(os.environ.get("ADLER_MCP_PORT", "8766"))
URL = f"http://127.0.0.1:{PORT}/mcp"
SKIP_LIVE = os.environ.get("ADLER_MCP_SKIP_LIVE", "").lower() in ("1", "true", "yes")

_results: list[tuple[bool, str]] = []


def ok(label: str, passed: bool = True, extra: str = "") -> None:
    _results.append((passed, label))
    mark = "PASS" if passed else "FAIL"
    print(f"  [{mark}] {label}" + (f"  {extra}" if extra else ""))


def parse_sse_response(resp: requests.Response, *, want_id: int | None = None):
    """Read the SSE stream until we see the response payload matching
    `want_id`. Progress notifications (no `id`,
    `method=notifications/progress`) and the initial priming event
    (`data:` with empty payload) are skipped en route. The Streamable
    HTTP transport always frames response bodies as SSE events
    (`Content-Type: text/event-stream`), even for simple
    request-response tools.
    """
    if resp.headers.get("content-type", "").startswith("application/json"):
        return resp.json()
    for line in resp.iter_lines(decode_unicode=True):
        if not line or not line.startswith("data: "):
            continue
        payload = line[6:].strip()
        if not payload:
            continue  # priming event
        try:
            obj = json.loads(payload)
        except json.JSONDecodeError:
            continue
        if want_id is not None:
            if obj.get("id") == want_id:
                return obj
            continue  # progress notification — keep reading
        return obj
    return None


def expect_result(body, want_id: int | None, label: str):
    ok(
        f"{label} → JSON-RPC result envelope",
        isinstance(body, dict)
        and body.get("jsonrpc") == "2.0"
        and body.get("id") == want_id
        and "result" in body
        and "error" not in body,
    )
    return body.get("result", {}) if isinstance(body, dict) else {}


def expect_error(body, want_id: int | None, label: str, text: str):
    error = body.get("error", {}) if isinstance(body, dict) else {}
    message = str(error.get("message", "")).lower()
    ok(
        f"{label} → JSON-RPC error envelope",
        isinstance(body, dict)
        and body.get("jsonrpc") == "2.0"
        and body.get("id") == want_id
        and isinstance(error, dict)
        and text.lower() in message,
        f"message={message!r}",
    )
    return error


def wait_for_listener(timeout: float = 6.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            requests.post(URL, timeout=0.5)
            return
        except requests.RequestException:
            time.sleep(0.2)


def main() -> int:
    proc = subprocess.Popen(
        [BIN, "--mcp-http", f"127.0.0.1:{PORT}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    wait_for_listener()

    session_id: str | None = None
    next_id = [1]

    def call(method: str, params: dict | None = None, *, want_id: bool = True):
        msg: dict = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        msg_id = None
        if want_id:
            msg_id = next_id[0]
            msg["id"] = msg_id
            next_id[0] += 1
        headers = {
            "accept": "application/json, text/event-stream",
            "content-type": "application/json",
        }
        if session_id is not None:
            headers["mcp-session-id"] = session_id
        resp = requests.post(URL, json=msg, headers=headers, stream=True, timeout=20)
        resp.req_id = msg_id  # type: ignore[attr-defined]
        return resp

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
        session_id = resp.headers.get("mcp-session-id")
        ok("HTTP 200 on initialize", resp.status_code == 200, f"status={resp.status_code}")
        ok("mcp-session-id header present", session_id is not None, f"id={session_id}")
        body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
        result = expect_result(body, resp.req_id, "initialize")  # type: ignore[attr-defined]
        info = result.get("serverInfo", {})
        ok(
            "initialize.serverInfo.name == adler-mcp",
            info.get("name") == "adler-mcp",
            f"server v{info.get('version', '?')}",
        )
        caps = result.get("capabilities", {})
        for cap in ("tools", "resources", "prompts"):
            ok(f"capability advertised: {cap}", cap in caps)

        # Notifications: 202 Accepted, no SSE body to drain.
        resp = call("notifications/initialized", want_id=False)
        ok("notifications/initialized accepted", resp.status_code in (200, 202))

        # === tools ===
        print("\n== tools ==")
        resp = call("tools/list")
        body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
        result = expect_result(body, resp.req_id, "tools/list")  # type: ignore[attr-defined]
        names = sorted(t["name"] for t in result.get("tools", []))
        expected = sorted([
            "list_sites", "scan_username", "scan_batch",
            "doctor_check", "get_scan_history", "diff_scans",
        ])
        ok("tools/list returns all 6", names == expected, f"got {names}")

        resp = call("tools/call", {"name": "list_sites", "arguments": {"tag": ["coding"]}})
        body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
        result = expect_result(body, resp.req_id, "tools/call list_sites")  # type: ignore[attr-defined]
        sc = result.get("structuredContent", {})
        ok(
            "tools/call list_sites(tag=coding)",
            sc["total"] > 5,
            f"{sc['total']} sites, first 3: {[s['name'] for s in sc['sites'][:3]]}",
        )

        resp = call("tools/call", {"name": "doctor_check", "arguments": {"site": "Reddit"}})
        body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
        result = expect_result(body, resp.req_id, "tools/call doctor_check(Reddit)")  # type: ignore[attr-defined]
        sc = result.get("structuredContent", {})
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
            body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
            result = expect_result(body, resp.req_id, "tools/call doctor_check(GitHub)")  # type: ignore[attr-defined]
            sc = result.get("structuredContent", {})
            ok(
                "tools/call doctor_check(GitHub) → live verdict",
                sc.get("site") == "GitHub" and sc.get("verdict") in ("healthy", "unhealthy"),
                f"verdict={sc.get('verdict')}, issues={len(sc.get('issues', []))}",
            )

        resp = call("tools/call", {"name": "get_scan_history", "arguments": {"limit": 5}})
        body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
        result = expect_result(body, resp.req_id, "tools/call get_scan_history")  # type: ignore[attr-defined]
        sc = result.get("structuredContent", {})
        ok(
            "tools/call get_scan_history",
            "total" in sc and "scans" in sc,
            f"total={sc['total']}",
        )

        # Live scan via HTTP/SSE — progress notifications interleave
        # with the final response on the same SSE stream; the parser
        # walks past them by matching on `want_id`.
        if SKIP_LIVE:
            ok("tools/call scan_username skipped (ADLER_MCP_SKIP_LIVE)")
        else:
            resp = call(
                "tools/call",
                {
                    "name": "scan_username",
                    "arguments": {"username": "torvalds", "top": 2, "tag": ["coding"]},
                    "_meta": {"progressToken": "probe-http-1"},
                },
            )
            body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
            result = expect_result(body, resp.req_id, "tools/call scan_username")  # type: ignore[attr-defined]
            sc = result.get("structuredContent", {})
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
        body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
        result = expect_result(body, resp.req_id, "resources/list")  # type: ignore[attr-defined]
        names = sorted(r["name"] for r in result.get("resources", []))
        ok(
            "resources/list returns 4 static",
            names == sorted([
                "registry_sites", "registry_tags",
                "registry_disabled", "scans_recent",
            ]),
            f"got {names}",
        )

        resp = call("resources/templates/list")
        body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
        result = expect_result(body, resp.req_id, "resources/templates/list")  # type: ignore[attr-defined]
        templates = result.get("resourceTemplates", [])
        ok(
            "resources/templates/list returns scans/{id}",
            any(t["uriTemplate"] == "adler://scans/{id}" for t in templates),
        )
        ok(
            "resources/templates/list returns scan diff",
            any(t["uriTemplate"] == "adler://scans/{from}/diff/{to}" for t in templates),
        )

        for uri in [
            "adler://registry/sites",
            "adler://registry/tags",
            "adler://registry/disabled",
            "adler://scans/recent",
        ]:
            resp = call("resources/read", {"uri": uri})
            body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
            result = expect_result(body, resp.req_id, f"resources/read {uri}")  # type: ignore[attr-defined]
            contents = result.get("contents", [])
            payload = json.loads(contents[0]["text"])
            total = payload.get("total", payload.get("total_tags", "?"))
            ok(f"resources/read {uri}", bool(contents) and "text" in contents[0], f"total={total}")

        resp = call("resources/read", {"uri": "adler://nope/x"})
        body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
        expect_error(
            body,
            resp.req_id,  # type: ignore[attr-defined]
            "resources/read unknown URI",
            "unknown resource",
        )

        resp = call("resources/read", {"uri": "adler://scans/../etc/passwd"})
        body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
        expect_error(
            body,
            resp.req_id,  # type: ignore[attr-defined]
            "resources/read path-traversal id",
            "",
        )

        # === prompts ===
        print("\n== prompts ==")
        resp = call("prompts/list")
        body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
        result = expect_result(body, resp.req_id, "prompts/list")  # type: ignore[attr-defined]
        pnames = sorted(p["name"] for p in result.get("prompts", []))
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
            body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
            result = expect_result(body, resp.req_id, f"prompts/get {name}")  # type: ignore[attr-defined]
            msgs = result.get("messages", [])
            text = msgs[0]["content"]["text"]
            has_arg = any(v in text for v in args.values()) if args else True
            ok(
                f"prompts/get {name}",
                len(msgs) == 1 and msgs[0]["role"] == "user" and has_arg,
                f"body {len(text)} chars",
            )

        resp = call("prompts/get", {"name": "investigate_username", "arguments": {}})
        body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
        expect_error(
            body,
            resp.req_id,  # type: ignore[attr-defined]
            "prompts/get missing required arg",
            "requires argument",
        )

        resp = call("prompts/get", {"name": "nope"})
        body = parse_sse_response(resp, want_id=resp.req_id)  # type: ignore[attr-defined]
        expect_error(
            body,
            resp.req_id,  # type: ignore[attr-defined]
            "prompts/get unknown name",
            "unknown prompt",
        )

        # === transport-specific ===
        print("\n== transport-specific ==")
        # Stale (never-existed) session id must be rejected with 404,
        # not silently accepted by spawning a new session. This is the
        # rmcp `LocalSessionManager` contract — sessions are created
        # only on the initialize handshake.
        headers = {
            "accept": "application/json, text/event-stream",
            "content-type": "application/json",
            "mcp-session-id": "stale-session-deadbeef",
        }
        resp = requests.post(
            URL,
            json={"jsonrpc": "2.0", "method": "tools/list", "id": 9999},
            headers=headers,
            timeout=5,
        )
        ok("stale session-id → 404", resp.status_code == 404, f"status={resp.status_code}")

    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        banner = ""
        if proc.stderr is not None:
            banner = proc.stderr.read()
        print("\n== stderr (server banner) ==")
        for line in banner.strip().splitlines()[:3]:
            print(f"  {line}")

    passed = sum(1 for p, _ in _results if p)
    total = len(_results)
    print(f"\n== summary == {passed}/{total} PASS")
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
