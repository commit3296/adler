# adler-mcp examples

End-to-end smoke probes that double as worked examples of how an MCP
client talks to `adler-mcp` over each of the two supported
transports. Hand-runnable against a built `adler` binary; not part of
the `cargo test` integration suite (that lives in
`adler-cli/tests/cli.rs`).

## What's here

| File | Transport | What it does |
| --- | --- | --- |
| `probe_stdio.py` | stdio | Spawns `adler --mcp`, drives the protocol over the child process's stdin/stdout. The shape Claude Desktop / Cursor see. |
| `probe_http.py` | HTTP+SSE | Spawns `adler --mcp-http 127.0.0.1:8766`, talks Streamable HTTP to `/mcp` (with `mcp-session-id` correlation and SSE-framed responses). |

Each probe walks the full handshake (`initialize` →
`notifications/initialized`) and then exercises every advertised
tool, resource, and prompt, printing one `[PASS] / [FAIL] <label>`
line per check and exiting non-zero if any fail.

## Running

```bash
# Build the binary once.
cargo build --release -p adler-cli

# Stdio probe (no extra deps).
python3 adler-mcp/examples/probe_stdio.py

# HTTP+SSE probe.
pip install requests
python3 adler-mcp/examples/probe_http.py
```

Override the binary path with `ADLER_BIN=…`; override the HTTP port
with `ADLER_MCP_PORT=…`.

## Expected output (abridged)

```
== handshake ==
  [PASS] initialize  server v0.11.7
  [PASS] capability advertised: tools
  [PASS] capability advertised: resources
  [PASS] capability advertised: prompts
…
== tools ==
  [PASS] tools/list returns all 5
  [PASS] tools/call list_sites(tag=coding)  32 sites, first 3: ['accounts.eclipse.org', 'BitBucket', 'codeberg.org']
  [PASS] tools/call scan_username(torvalds, top=2, tag=coding)  probed=2, found=1, sites=['GitHub', 'GitLab']
…
== summary == 22/22 PASS
```

The scan-related checks make real outbound HTTP requests against
the matching sites in the registry — the probes count as "live"
smoke tests in that sense.

## Reference notes for client authors

A couple of MCP shapes the probes intentionally exercise because
they're easy to miss writing your own client:

- **SSE response parsing (`probe_http.py`).** The Streamable HTTP
  transport frames every response as an SSE event. Progress
  notifications (`method=notifications/progress`, no `id`) interleave
  on the same stream when a tool emits them — `scan_username` does
  this. A client that returns on the first `data:` line will catch a
  progress notification instead of the final response. The canonical
  pattern is to walk the stream filtered by JSON-RPC `id`; see
  `parse_sse_response(..., want_id=N)`.
- **Session lifecycle (`probe_http.py`).** The `mcp-session-id`
  header is issued on the initialize response and must be echoed on
  every subsequent request. A request carrying a never-existed id is
  rejected with `404 Not Found` rather than silently spawning a new
  session — the probe verifies this on purpose.
- **Tool argument validation (both).** Missing required prompt
  arguments yield `invalid_params`; unknown tool / prompt / resource
  names yield `invalid_params` with a descriptive `message`.
  Path-traversal in `adler://scans/{id}` (anything containing `/` or
  `\`) is rejected before the file open.

## Why not Rust examples?

`cargo run --example name` is the canonical hook for crate-bundled
example *clients*, but the most realistic client surface here is "an
agent runtime in any language". Python is shorter and reads
top-to-bottom; the resulting probe is also a useful starting point
for someone porting MCP support into a different language.

The Rust-side integration tests in `adler-cli/tests/cli.rs`
(`mcp_stdio_serves_initialize_tools_resources_prompts`,
`mcp_stdio_tool_call_returns_structured_content`) cover the same
ground for CI.
