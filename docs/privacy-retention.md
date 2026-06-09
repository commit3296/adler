# Privacy and retention

Adler can run without durable history, but some surfaces persist local
artifacts so operators can compare scans, build timelines, and let MCP
clients inspect previous work.

## What is stored

- CLI result cache: ordinary one-shot scans may read/write a cache file
  unless `--no-cache` is used. Clear it with `adler --cache-clear` or
  override its location with `--cache-path`.
- Web scan history: `adler --web` writes finished scan JSON files under
  `$XDG_CACHE_HOME/adler/scans/`, falling back to
  `$HOME/.cache/adler/scans/`.
- Watch snapshots: `adler --watch` stores the previous found-account
  snapshot under the cache directory so it can report added/removed
  accounts on the next run.
- MCP history resources: MCP does not write scan history by itself, but
  it reads the same persisted web scan directory for
  `get_scan_history`, `diff_scans`, `adler://scans/{id}`,
  `adler://scans/{from}/diff/{to}`, and
  `adler://timelines/{username}`.

Persisted scan files include the scanned username, request scope,
summary counts, per-site outcomes, URLs, evidence strings, normalized
profile evidence, elapsed timings, and disabled-site context when the
scan was launched through the Web API.

## Retention

The web persistence layer keeps a bounded local history and prunes older
scan JSON files on save. Treat the scan directory as operator-owned
local data: move or delete it when a case ends, and use a temporary
`XDG_CACHE_HOME` for short-lived investigations.

Examples:

```bash
XDG_CACHE_HOME="$(mktemp -d)" adler --web --only GitHub
rm -rf "$XDG_CACHE_HOME/adler/scans"
```

## Exposure boundaries

By default, Web and MCP HTTP servers bind to loopback. Binding either
surface to `0.0.0.0` exposes scan APIs, history, diffs, timelines, and
resource metadata to the network without Adler-provided authentication.
Only do this behind your own access control on a trusted network.

MCP over stdio is local to the launched process, but the client receives
the same history resources the server can read from disk.

## Operator checklist

- Use `--no-cache` for one-shot scans that should not touch the result
  cache.
- Use a temporary `XDG_CACHE_HOME` for throwaway Web or MCP sessions.
- Delete `$XDG_CACHE_HOME/adler/scans/` or override it with
  `--scans-dir` when a case needs separate retention.
- Keep non-loopback `--web-bind` and `--mcp-http` deployments behind
  authentication and transport security that you control.
- Avoid storing scan artifacts longer than the authorization or
  investigation need requires.
