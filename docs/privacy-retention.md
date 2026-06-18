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
- Investigation reports: `adler --report-scan <ID>` reads a persisted
  scan and writes a Markdown or JSON report to stdout. If you redirect
  it to a file, that file contains the same case material in a more
  portable form.

Persisted scan files include the scanned username, request scope,
summary counts, per-site outcomes, URLs, evidence strings, normalized
profile evidence, confidence scores and reasons, identity cluster
candidates, elapsed timings, and disabled-site context when the scan was
launched through the Web API. Timelines, diffs, watch output, and
investigation reports are derived from these artifacts.

Avatar perceptual hashing is opt-in for CLI scans via `--avatar-hash`.
When enabled, Adler fetches avatar URLs already found by enrichment,
applies response size, content-type, timeout, and redirect limits, and
uses the scan's global `--proxy` or `--tor` setting for those image
requests. It stores only a normalized hash such as `ahash64_v1:...`.
Raw image bytes are not persisted in scan JSON, reports, MCP output, or
Web output. Avatar hashes are weak/supporting identity evidence: an
avatar-hash-only match is not treated as a confident hard merge.

Evidence access metadata is deliberately coarse. It can record the
transport tier, whether Adler escalated to a heavier route, whether an
authenticated access path was used, and when evidence was observed. It
does not store session names, cookie values, header values, proxy URLs,
or egress names. Persisted Web request context may still record the
operator-selected egress names for a scan so later reports can explain
scope.

## Retention

The web persistence layer keeps a bounded local history and prunes older
scan JSON files on save. Treat the scan directory as operator-owned
local data: move or delete it when a case ends, and use a temporary
`XDG_CACHE_HOME` for short-lived investigations. Markdown and JSON
reports are not pruned by Adler because they are ordinary files wherever
the operator redirects stdout.

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

Scan ids are not passwords, but they are capability-like local tokens:
any client that can reach the Web API or MCP resource surface and knows a
scan id can read that scan artifact. Do not share scan ids or report
files outside the authorization boundary for the investigation.

MCP over stdio is local to the launched process, but the client receives
the same history resources the server can read from disk.

## Responsible use

Adler aggregates public profile URLs and evidence, and aggregation can
make sensitive patterns easier to see. Use it only for your own accounts,
authorized security testing, bug-bounty work, defensive research, or
investigations with a lawful basis. Do not use persisted artifacts,
timelines, reports, or clusters to harass, dox, stalk, or surveil people
without authorization.

## Operator checklist

- Use `--no-cache` for one-shot scans that should not touch the result
  cache.
- Use `--avatar-hash` only when external avatar fetching is authorized
  for the investigation; leave it off for ordinary scans.
- Use a temporary `XDG_CACHE_HOME` for throwaway Web or MCP sessions.
- Delete `$XDG_CACHE_HOME/adler/scans/` or override it with
  `--scans-dir` when a case needs separate retention.
- Treat redirected Markdown/JSON reports as case files and delete or
  move them with the same retention policy as the underlying scan
  artifacts.
- Keep non-loopback `--web-bind` and `--mcp-http` deployments behind
  authentication and transport security that you control.
- Keep scan ids inside the same trust boundary as the scan files.
- Avoid storing scan artifacts longer than the authorization or
  investigation need requires.
