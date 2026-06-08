# Adler — roadmap

OSINT username search across many sites. Successor to Sherlock, written
in Rust. Goals: higher recall and lower false-positive rate than a
single-shot status check, data-driven and self-healing site signatures,
modern terminal UX, AI-agent-ready surface.

> **Where to read what.** The [README](README.md) is the user-facing
> entry point. The [CHANGELOG](CHANGELOG.md) is the source of truth for
> what shipped, by version — every entry below labelled "shipped"
> resolves to a CHANGELOG section. This file is the forward-looking
> *roadmap*: where we are now, what's open next, and what's been
> investigated and parked. Detail belongs in CHANGELOG; intent belongs
> here.

---

## Now

Adler is a 4-crate Rust workspace on `main` at v0.12.1:

- **`adler-core`** — the OSINT engine. 1834 enabled sites + 66
  disabled-with-reason in the main tranche, plus 674 enabled sites + 1
  disabled-with-reason in the default-on WhatsMyName supplement
  (`--no-wmn` to drop it); 11 inheritable
  engine signatures
  (uCoz / vBulletin / phpBB / phpBB-Search / XenForo / Flarum /
  Discourse / op.gg / Wordpress-Author / engine404 / engine404get).
  Multi-signal detection (status / body / redirect). Two browser
  backends for bot-protected sites (`LocalBackend` via `chromiumoxide`;
  `BrowserbaseBackend` via cloud CDP) and FlareSolverr.
- **`adler-cli`** — the `adler` binary on
  [crates.io](https://crates.io/crates/adler-cli); five-platform prebuilts
  attached to every GitHub Release; `cargo binstall adler-cli` works.
  Doctor flow shipped end-to-end: `--fix` / `--suggest-known-present` /
  `--suggest-extract` / `--suggest-protection`, each pairable with
  `--apply --sites <path>` for atomic JSON rewrite. Structured
  `--format json|ndjson` doctor output.
- **`adler-server`** — `--web` HTTP API + SolidJS SPA embedded via
  `rust-embed`. Server-Sent Events for live scan streaming;
  multi-username batch; mid-scan refilter (cancel-and-restart with
  overlap); compare-with-previous picker; per-scan egress subset
  selection; read-only access engine view + transport telemetry.
- **`adler-mcp`** — Model Context Protocol server on `rmcp 1.7`.
  Two transports (stdio for Claude Desktop / Cursor / local agents;
  Streamable HTTP+SSE for remote agents). Five tools (`list_sites`,
  `scan_username` with streaming progress, `scan_batch`,
  `doctor_check`, `get_scan_history`), five resources (`adler://
  registry/{sites,tags,disabled}`, `adler://scans/recent`,
  `adler://scans/{id}` template), three prompts (`investigate_username`,
  `audit_registry_health`, `correlate_accounts`).

The **Access engine initiative** (6 phases: `Fetcher` seam, impersonate
transport, egress pool + geo routing, escalation router + telemetry,
session injection, web UI surface) is fully shipped. It unified what
used to be ad-hoc transports — single global proxy, bot-protected tag,
FlareSolverr, `--tor` — into one routing engine where each site
declares what it needs.

**Release pipeline.** `release-plz` reads Conventional Commits on
`main`, opens a Release PR with version bumps + CHANGELOG section; on
merge, the matching GitHub App publishes both crates to crates.io and
creates a tagged Release; `release.yml` builds the platform binaries
on the release event. Nightly `doctor.yml` runs the full doctor across
both tranches, classifies failures, force-pushes per-site
consecutive-failure counters to a `_doctor-state` branch, and opens
a PR proposing `disabled: true` patches when a site fails ≥3
consecutive nights.

**Honest about measurement.** The last apples-to-apples detection-rate
number is from the v0.3.x 411-site registry (67.9% datacenter,
72.3% US residential). The current ~2600-entry mix has not been
re-benchmarked end-to-end; the `bench/` harness exists for it but
results are gitignored on purpose (per-operator variance is too high
for a single "official" number).

## Next

Three open items. All three need a clean residential IP to make
material progress — the tooling for each is shipped, the work that
remains is content the doctor can only discover from a non-datacenter
network.

- **Refresh stale `known_present` entries.** 185 entries still carry
  Sherlock's `"blue"` placeholder. Tooling shipped:
  `adler --doctor --suggest-known-present --apply --sites <path>
  --yes`. First batch refreshed 10 from a datacenter IP — recall capped
  at ~5% from there because most remaining entries are Cloudflare-
  walled. Contributor passes from cleaner IPs can chew through the
  rest one batch at a time. The `discover_known_present` guard added
  in v0.11.7 (probe a nonsense user first; abort if the site responds
  Found to it) means brand-name catch-alls can't yield false-positive
  discoveries.
- **Hand out `extract` rules across the long tail.** The main registry
  ships with extraction rules on a single site (GitHub) out of 1900
  entries, and the WhatsMyName supplement currently has none.
  Tooling shipped: `adler --doctor --suggest-extract --apply --sites
  <path> --yes` mines OpenGraph + Twitter Card meta tags from each
  healthy site's `known_present` profile page. Same network constraint
  — most sites whose profile pages are reachable from a clean IP will
  yield, JS-only SPAs won't.
- **Site-specific signal authoring.** A full-registry doctor pass from
  a datacenter IP turns up ~91 sites that fail
  `"detection rule too permissive"` while carrying a real-looking
  `known_present`. Two categories already handled this round:
  importer-duplicate dedup (59 entries) and structurally-unscrapable
  Honest-Limits disables (see below). Remaining work:
  *engine-block tightening* (~32 entries inheriting from a single
  engine's too-generous signal block — fixing one engine fixes many
  sites at once) and *custom-page body markers* on ~25 sites where
  `--doctor --fix` can usually derive a tighter signal when present /
  absent pages actually differ from the operator's IP. Both
  clean-IP-bound.

## Honest limits (investigated, deferred)

Structurally unscrapable for anonymous OSINT until something changes
upstream. All entries below carry `disabled: true` + per-site
`disabled_reason` in `sites.json` so the doctor stops flagging them
every night.

- **Reddit** — 403s anonymous requests since the 2023 API restriction.
  Only path forward is OAuth, which defeats anonymous use.
- **TikTok**, **Pinterest** — JS-rendered SPAs; user data never
  hydrates into the headless DOM (verified with 15 s post-load wait
  through Browserbase). Needs full fingerprint spoofing + realistic
  interaction.
- **Threads**, **Facebook** — public profile pages exist for a handful
  of Meta-special-cased accounts (`@zuck`); every normal username
  redirects to a login wall. Indistinguishable from a missing user.
- **Spotify**, **Steam (by id)** — datacenter-IP-too-permissive: the
  URL returns 200 for arbitrary strings. Re-enableable with a tighter
  body marker authored from a clean IP, but the current signal can't
  be salvaged from datacenter network conditions.

## Quality bar (current, enforced by CI)

`cargo build --workspace --all-targets`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --all-targets`,
`cargo test --workspace --doc`,
`cargo fmt --all --check` — all must pass; release-plz blocks the
Release PR on the same gates.

## Trajectory

One line per release showing direction-of-travel. CHANGELOG.md has
the per-commit detail.

- **v0.1** (2026-05-23) — initial public release. Phases 0–4
  (workspace, MVP detection, reliability, enrichment + correlation,
  UX). 411 sites.
- **v0.2** — browser backends (`chromiumoxide` local + Browserbase
  cloud) for bot-protected sites; in-tree async CDP client.
- **v0.3** — multi-`known_present` defensive doctor;
  `--suggest-known-present`; browser-aware `--fix`.
- **v0.4** — NSFW opt-in; `regex_check` username gate; WAF-challenge
  ban detection.
- **v0.5** — engine inheritance for shared forum signatures; Maigret
  import (439 → 2558 sites); opt-in WhatsMyName tranche behind a
  separate registry file.
- **v0.6** — WhatsMyName tranche default-on (`--no-wmn` to opt out);
  doctor-driven prune of 570 sites.
- **v0.7** — structured `protection` field with kind enum; POST body
  templates; `strip_bad_char` username normalisation; HEAD-only probe
  when the signal doesn't need a body.
- **v0.8** — `--web` flag launches embedded HTTP API + SolidJS SPA;
  TUI retired; FlareSolverr backend; `--top N` ranking with curated
  popularity seed; multi-username batch in the SPA.
- **v0.9** — Access engine phase 3: per-site access policy + egress
  pool with geo routing; `--proxy-pool` config.
- **v0.10** — Access engine phase 2 (impersonate transport via
  `wreq`) + phase 5 (operator session injection for login-walled
  sites).
- **v0.11** — Access engine phase 4 (escalation router + transport
  telemetry) + phase 6 (read-only access view + per-scan egress
  subset in the SPA); `--doctor --fix --apply` atomic rewrite;
  `--suggest-extract` + `--suggest-known-present --apply`; URL+signals
  uniqueness check at load time; mid-scan refilter; structured
  `--format json|ndjson` doctor output; nightly doctor → auto-PR
  workflow; registry hygiene rounds (66 entries disabled with reasons
  across dedup + Honest Limits).
- **next release (pending)** — `adler-mcp` crate: Model Context
  Protocol server with 5 tools + 5 resources + 3 prompts over stdio
  + Streamable HTTP+SSE transports.
