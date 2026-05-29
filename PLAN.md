# Adler — roadmap

OSINT username search across many sites. Successor to Sherlock, written
in Rust. Goals: higher recall and lower false-positive rate than a
single-shot status check, data-driven and self-healing site signatures,
modern terminal UX.

> **Where to read what.** The
> [README](README.md) is the user-facing entry point. The
> [CHANGELOG](CHANGELOG.md) is the source of truth for what shipped, by
> version. This file is the forward-looking *roadmap* — what we're
> aiming at next, and a brief retrospective of the major shipped
> milestones so readers can see the trajectory.

---

## Now (v0.5.x line)

- Workspace: `adler-core` (library) + `adler-cli` (binary `adler`),
  both published on [crates.io](https://crates.io/crates/adler-cli);
  prebuilt binaries for five platforms attached to every GitHub
  Release; `cargo binstall adler-cli` works.
- 2558-site embedded registry with shared engine inheritance for
  forum-software families (XenForo, vBulletin, Discourse, phpBB,
  uCoz, Flarum, op.gg — 12 engines in total). Multi-signal
  detection (status / body / redirect), `--doctor` health check
  with `--fix` signature derivation (now browser-aware for
  bot-protected sites), per-site `request_headers`, and a
  defensive multi-`known_present` doctor.
- Two browser backends for `bot-protected` sites
  (Instagram, X/Twitter): `LocalBackend` (`chromiumoxide`,
  free) and `BrowserbaseBackend` (cloud, residential IPs). The
  Browserbase path drives CDP through an in-tree async client
  (`adler-core/src/browser/cdp.rs`) because both maintained Rust
  CDP libraries deadlock against remote-attach sessions.
- Fully automated release pipeline: `release-plz` reads
  Conventional Commits on `main`, opens a Release PR with bumps
  and CHANGELOG entry; on merge, the matching GitHub App publishes
  both crates to crates.io and creates a `v<X.Y.Z>` GitHub Release;
  the binary upload workflow auto-triggers on the release event.
- Current detection rate measured pre-merge on the 411-site
  v0.3.x registry: **67.9% datacenter**, **72.3% US residential**.
  The Maigret-imported tranche (2119 sites, tagged
  `source:maigret`) is unvalidated at the time of import; the
  nightly doctor workflow will gradually classify its structural
  failures. See README for the breakdown of what doesn't detect
  and why.

## Access engine (next major initiative)

**Strategic framing.** Adler's positioning is a tool real red-team /
OSINT operators adopt over Sherlock/Maigret. The switch trigger is
reaching *the sites that matter* (geo-restricted, fingerprinted,
login-walled) with verdicts you can trust — access is the bottleneck,
not signature parsing. The pieces exist but are ad-hoc (single global
proxy, browser backends gated by the `bot-protected` tag, FlareSolverr,
`--tor`). This initiative unifies them into one engine where **each
site declares what it needs and a router supplies it**.

**Ethical line (decided).** We build "legitimate access as a real
user": geo/residential proxy routing, realistic TLS/HTTP fingerprints,
JS rendering, and injection of a real (operator-supplied) session. We
do **not** ship CAPTCHA solving or anti-detect defeat of
human-verification. The `Fetcher` trait leaves an extension point but
ships nothing there. The README/ethics text moves from "never bypass
access controls" to: *access public content as a legitimate user
would; we do not solve human-verification challenges or defeat
controls designed to stop automation.*

**Architecture.**

- `Fetcher` trait normalises the request path into `FetchResponse` /
  `FetchError`; `Client` becomes a router over fetchers. Transports:
  `HttpFetcher` (reqwest), `ImpersonateFetcher` (`rquest`, behind an
  `impersonate` Cargo feature so the base build stays lean),
  `BrowserFetcher` (adapts the existing `BrowserBackend`),
  `FlareSolverrFetcher`.
- **Egress** layer orthogonal to transport: a pool of proxies tagged
  `{country, kind: direct|datacenter|residential|mobile|tor}`. reqwest
  bakes the proxy at build, so this is a lazy per-egress client cache.
  Throttle stays per-host.
- **Per-site `access` policy** (new `Site::access`): `geo`, `ip_type`,
  `transport` pref, `session` name. The existing `protection` vec
  *infers* a default transport (`tls-fingerprint`→impersonate,
  `cloudflare`→browser/flaresolverr, `user-auth`→needs session); the
  `region:*` tags auto-imply `access.geo` (overridable).
- **Router with escalation**: cheap→expensive ladder
  (http → impersonate → browser/flaresolverr), egress chosen from
  policy, bounded by an access budget, recording what worked to feed
  the doctor. A required geo with no matching egress, or a block after
  escalation, yields `Uncertain(GeoUnavailable | AccessDenied |
  ChallengeUnsolved | SessionRequired | …)` — **never** `NotFound`
  (truthful verdicts are the whole point).
- **Session injection** (the login-wall key): an operator-supplied
  `Session { cookies, headers }` store (TOML / env), referenced by
  `access.session`. Secrets are `SecretString`, never logged, scrubbed
  from `CheckOutcome` / persisted scans. This is "use a real account",
  not evasion — and it's what cracks the Reddit / Threads / login-wall
  entries in *Honest limits* below.
- **Config**: `access.toml` (egress pools + sessions) as source of
  truth; `--proxy` stays for the trivial single-proxy case; the web UI
  later manages the same file + per-scan egress override.

**Phases (each shippable; phase 3 is what unblocks benchmarking the
hard sites, tying back to the accuracy thesis):**

- [x] **1 — `Fetcher` seam**: hoist the HTTP + browser paths in
  `client.rs::probe_once` behind the trait with zero behaviour change;
  parity proven by the existing test suite. (`adler-core/src/transport.rs`:
  `Fetcher` trait + `HttpFetcher` / `BrowserFetcher`; `Client` is now the
  router. Found that `request_headers` apply only on the browser path
  today — candidate fix for Phase 2.)
- [ ] **2 — Impersonate transport** (`rquest`, `impersonate` feature):
  biggest cheap coverage win against fingerprint blocks.
- [ ] **3 — Egress pool + geo routing**: pool, per-egress client cache,
  `access.geo`/`ip_type`, migrate `region:*`→`access.geo`,
  `Uncertain(GeoUnavailable)`.
- [ ] **4 — Router + escalation + telemetry** feeding the doctor.
- [ ] **5 — Session injection**: defeat login walls via real sessions.
- [ ] **6 — Web UI**: manage pools / sessions / per-scan egress in the
  SPA.

## Next

In rough priority order. None of these are blocking anything that
shipped — they're the candidates we'd pick from when allocating the
next chunk of work.

### Registry hygiene

- **Refresh the ~80 stale `known_present` entries** still using
  Sherlock's placeholder `"blue"` or other long-dead usernames.
  [Issue #4](https://github.com/commit3296/adler/issues/4) tracks
  the original 17-site batch; the latest doctor pass found 66 more
  on `"blue"` alone. Pure contributor task: one site = one OVERRIDE
  entry in `scripts/import_sherlock.py` + a `sites.json` edit.
- **Periodic registry validation in CI**: nightly `--doctor`
  workflow already exists; we should make it produce a PR with
  a `KNOWN_BROKEN` suggestion for sites that fail repeatedly.

### Detection coverage

- **More enrichment extractors** (CSS-selector rules under
  `extract:`) for the long tail of sites where `--enrich` currently
  returns an empty profile.
- **Site-specific signal authoring** for the ~30 sites that fail
  doctor with non-`"blue"` known_present — typically a real account
  whose detection rule needs tweaking, not a missing user.

### Honest limits (investigated, deferred)

These were tested during the v0.3.x development; the conclusion is
that they're structurally unscrapable for our anonymous-OSINT use
case until something changes upstream. Documented so a future
contributor doesn't re-tread the same ground:

- **Reddit** — 403s any unauthenticated request to its JSON or
  canonical user endpoints from datacenter, Browserbase, and most
  residential IPs since the 2023 API restriction. Only path forward
  is OAuth, which defeats the anonymous use case.
- **TikTok**, **Pinterest** — JS-rendered SPAs whose initial
  document is a 400–1700 KB script bootstrap; user data never
  hydrates into the DOM for headless browsers (verified with up to
  15 s post-load wait through Browserbase). Probably needs full
  browser fingerprint spoofing plus realistic user interaction.
- **Threads** — public profile pages exist for a handful of
  Meta-special-cased accounts (e.g. `@zuck`); every normal username
  redirects to a login wall. Indistinguishable from a missing user.

### Infra polish

- **`scripts/bench-vs-sherlock.sh`** — real-network benchmark
  comparing Adler against Sherlock on a fixed username set; the
  in-process `cargo bench` only measures executor overhead. See
  [issue #8](https://github.com/commit3296/adler/issues/8).
- **`adler --doctor --fix --apply`** — currently `--fix` only
  prints a suggested signature; an `--apply` flag could patch
  `sites.json` directly (still gated on a contributor review of
  the resulting PR).
### Web UI — shipped

The `--tui` flag was retired in favour of a browser-based UI that
covers the same interactive-browse use case with richer rendering
and real-time streaming of results.

- [x] `adler-server` crate — axum + Server-Sent Events. Endpoints:
  `/api/health`, `/api/sites`, `/api/scans`, `POST /api/scan`,
  `GET /api/scan/:id`, `GET /api/scan/:id/stream`,
  `POST /api/scan/:id/retry`. Per-scan filter body mirrors the CLI
  flags one-for-one. Persists finished scans under
  `$XDG_CACHE_HOME/adler/scans/` so history survives restarts.
- [x] `adler --web` flag launches the server on
  `127.0.0.1:8765` (override with `--web-bind`), respecting the same
  `--only` / `--exclude` / `--tag` / `--top` filters as one-shot
  scans. Banner + structured boot log; TTY-aware colours.
- [x] `adler-server/web/` — SolidJS + Vite + TypeScript SPA. Hero /
  scan view / diff view, hash-routed (`#/scan/:id`, `#/diff/:a/:b`),
  rAF-batched SSE ingestion, per-bucket reactive store, live
  category groupings, per-row retry, datacenter-IP hint, localStorage
  prefs, full keyboard shortcuts.
- [x] `adler-server/web/src/ui/` — portable component library (tokens +
  Button / IconButton / Input / SearchInput / Chip / Tabs / Modal /
  Toast / Kbd / Icon). Used by all app components; documented in
  `adler-server/web/src/ui/README.md`.
- [x] `rust-embed` snapshots `web/dist/` into the final binary so
  `adler --web` ships a self-contained UI. Living inside the
  `adler-server` package means SPA-only edits are visible to
  release-plz and still cut a release. CI / release builds pre-run
  `npm ci && npm run build` to populate `web/dist/`.

### Web UI — next

- [ ] Picker for "Compare with previous" — currently auto-picks the
  most recent finished scan of the same username; a dropdown would
  let the user choose which historical scan to diff against.
- [ ] Server-side filter changes during running scan (e.g. "narrow
  scope to dev sites only"). Today the catalogue is frozen at scan
  start; a cancel-and-restart-with-overlap would be friendlier.
- [ ] Multi-username batch (analogous to `--input file.txt` in the
  CLI). The store can already model multiple scans in history; the
  UI just needs a textarea-input mode.

---

## Quality bar (current, enforced by CI)

`cargo build --workspace --all-targets`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --all-targets`,
`cargo test --workspace --doc`,
`cargo fmt --all --check` — all must pass; release-plz blocks the
Release PR on the same gates.

## History (one line each)

- **v0.5.0** (pending) — engine inheritance for shared forum
  signatures (`Site.engine` + top-level `engines` block); Maigret
  importer brings the registry from 439 to 2558 sites (+12
  engines); WhatsMyName import deferred on CC-BY-SA / MIT
  incompatibility.
- **v0.4.0** (2026-05-24) — default-exclude NSFW with `--nsfw`
  opt-in mirroring Sherlock; per-site `regex_check` username
  gate; new `UncertainReason::UsernameNotAllowed`.
- **v0.3.0** (2026-05-24) — multi-`known_present` defensive
  doctor; mock-CDP test harness; closed Phase 5 *honest limits*
  follow-ups.
- **v0.2.1** (2026-05-24) — Instagram `known_present` fix
  surfaced by smoke-testing v0.2.0.
- **v0.2.0** (2026-05-24) — browser backend (local +
  Browserbase) for `bot-protected` sites; in-tree raw async CDP
  client; Twitter via `x.com` canonical + react-testid; Instagram
  via `web_profile_info` JSON; per-site `request_headers`.
- **v0.1.0** (2026-05-23) — initial public release. Phases 0–4
  complete (workspace, MVP detection, reliability,
  enrichment + correlation, UX). 411-site registry.
