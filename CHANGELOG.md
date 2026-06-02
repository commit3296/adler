# Changelog

All notable changes to Adler are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Adler follows [SemVer](https://semver.org); see the *Versioning &
releases* section of [CONTRIBUTING.md](CONTRIBUTING.md) for the
pre-1.0 bump rules.

## [Unreleased]

## [0.11.2](https://github.com/commit3296/adler/compare/v0.11.0...v0.11.2) - 2026-06-02

### Added
- *(cli)* doctor --fix --apply patches sites.json in place

## [0.11.1](https://github.com/commit3296/adler/compare/v0.11.0...v0.11.1) - 2026-06-02

### Added
- *(cli)* doctor --fix --apply patches sites.json in place

## [0.11.0](https://github.com/commit3296/adler/compare/v0.10.0...v0.11.0) - 2026-06-01

### Added
- *(server)* per-scan egress subset selection (phase 6b)
- *(server)* read-only access engine view + transport telemetry in SPA (phase 6a)
- *(core)* escalation router + transport telemetry (access engine phase 4)

## [0.10.0](https://github.com/commit3296/adler/compare/v0.9.0...v0.10.0) - 2026-05-31

### Added
- *(core)* impersonate transport behind the `impersonate` Cargo feature (phase 2)
- operator session injection for login-walled sites (access engine phase 5)

### Fixed
- *(server)* axum 0.8.9 compat — KeepAliveStream return type, {param} route syntax

## [0.9.0](https://github.com/commit3296/adler/compare/v0.8.3...v0.9.0) - 2026-05-29

### Added
- *(cli)* --proxy-pool egress config for geo routing (phase 3)
- *(core)* per-site access policy + egress pool with geo routing (phase 3 core)

## [0.8.3](https://github.com/commit3296/adler/compare/v0.8.2...v0.8.3) - 2026-05-29

### Added
- *(web)* multi-username batch scanning

## [0.8.2](https://github.com/commit3296/adler/compare/v0.8.1...v0.8.2) - 2026-05-28

### Added
- *(web)* not-found view, loading skeletons, and a boot preloader for smoother state transitions
- *(web)* persistent footer, About panel, dynamic page title, and meta/SEO tags

## [0.8.1](https://github.com/commit3296/adler/compare/v0.8.0...v0.8.1) - 2026-05-27

### Added
- *(cli)* friendly quickstart when run without arguments
- *(cli)* --web flag launches the embedded HTTP API + SPA

## [0.8.0](https://github.com/commit3296/adler/compare/v0.7.0...v0.8.0) - 2026-05-27

### Added
- *(browser)* FlareSolverr backend for free Cloudflare bypass (R4)
- *(cli)* --top N ranking with curated popularity seed (R11)
- *(site)* disabled flag + source field for mirrors (R8)

### Refactor
- *(cli)* [**breaking**] remove TUI in favour of upcoming web UI

## [0.7.0](https://github.com/commit3296/adler/compare/v0.6.0...v0.7.0) - 2026-05-27

### Added
- *(site)* structured protection field with kind enum (R3)
- *(site)* POST request support with templated body (R14)
- *(site)* strip_bad_char username normalisation (R9)

### Changed
- *(client)* HEAD-only probe when no signal needs the body (R13)

## [0.6.0](https://github.com/commit3296/adler/compare/v0.5.0...v0.6.0) - 2026-05-26

### Added
- *(cli)* [**breaking**] include WhatsMyName tranche by default, rename flag to --no-wmn

### Chore
- *(registry)* [**breaking**] prune 570 sites surfaced by 2026-05-26 doctor run

## [0.5.0](https://github.com/commit3296/adler/compare/v0.4.0...v0.5.0) - 2026-05-26

### Added
- *(registry)* opt-in WhatsMyName tranche as CC-BY-SA registry
- *(registry)* import Maigret sites and engines (439 -> 2558)
- *(site)* [**breaking**] engine inheritance system for shared signatures

## [0.4.0](https://github.com/commit3296/adler/compare/v0.3.2...v0.4.0) - 2026-05-26

### Added
- [**breaking**] hide NSFW sites behind opt-in --nsfw flag
- *(registry)* import 26 response_url sites from upstream Sherlock
- *(site)* per-site regex_check skips probes for invalid usernames
- *(ban)* detect WAF challenge pages via Sherlock-curated body fingerprints

### Fixed
- *(security)* defuse shell-interpolation attack in validate-sites.yml

## [0.3.2](https://github.com/commit3296/adler/compare/v0.3.1...v0.3.2) - 2026-05-26

### Added
- *(doctor)* --suggest-known-present probes a pool to heal stale entries

## [0.3.1](https://github.com/commit3296/adler/compare/v0.3.0...v0.3.1) - 2026-05-24

### Added
- *(doctor)* route --doctor --fix through browser backend for bot-protected sites

## [0.3.0](https://github.com/commit3296/adler/compare/v0.2.1...v0.3.0) - 2026-05-24

### Added
- [**breaking**] allow multiple known_present usernames per site for doctor

## [0.2.1](https://github.com/commit3296/adler/compare/v0.2.0...v0.2.1) - 2026-05-24

### Fixed
- *(registry)* use torvalds, not "instagram", as Instagram known_present

## [0.2.0] — 2026-05-24

### Added

- **Browser backend for bot-protected sites.** Sites tagged
  `bot-protected` (Instagram, X/Twitter, …) can now be routed through
  a real headless Chrome that runs JS, accepts cookies, and returns
  the final post-render DOM. Two transports:
  - `--browser-backend local` — launches Chrome on the host via
    `chromiumoxide`. Free; needs Chrome installed.
  - `--browser-backend browserbase` — opens a remote session on
    [Browserbase](https://browserbase.com) and drives it over CDP.
    Pay per session-minute; residential / mobile IPs and
    anti-fingerprint baked in. Reads `ADLER_BROWSERBASE_API_KEY` and
    `ADLER_BROWSERBASE_PROJECT_ID` from the environment.
  - `--browser-budget N` (default 50) caps how many fetches a single
    scan may route through the browser; remaining bot-protected
    sites fall back to `Uncertain(BrowserBudget)`.
- **Raw async CDP client** (`adler-core::browser::cdp::CdpClient`).
  Both maintained Rust CDP libraries deadlock against Browserbase's
  remote-attach semantics; the in-tree client is the only path that
  works. See `adler-core/src/browser/cdp.rs` and
  [issue #5](https://github.com/commit3296/adler/issues/5) for the
  diagnosis.
- **Per-site request headers** via a new `request_headers` field on
  `Site` (serde-default, so existing entries are unchanged). Browser
  backends apply them through `Network.setExtraHTTPHeaders` and
  `Network.setUserAgentOverride` before navigation. Required for
  JSON APIs that gate on caller identity rather than IP.
- **Twitter detection** (`x.com`) via the canonical profile page +
  react-testid signals (`data-testid="primaryColumn"` for found,
  `data-testid="mask"` for not-found). Browser-backend required.
- **Instagram detection** through the `web_profile_info` JSON
  endpoint with `X-IG-App-ID` + a Chrome `User-Agent`. Existing
  account → 200 + profile JSON containing `"is_verified"`; missing
  account → HTTP 404. Sherlock and Maigret both detect Instagram
  via broken third-party mirrors; this is the working path.
- **Detection-rate section in the README** with validated numbers
  for datacenter (65%, 272/416) and US residential
  (71%, 295/416) scans.
- **crates.io / docs.rs badges** in the README; refreshed `Install`
  section.

### Changed

- **BREAKING.** `BrowserBackend::fetch` signature changed from
  `fetch(url, timeout)` to
  `fetch(url, headers: &BTreeMap<String, String>, timeout)`. Custom
  backend impls (downstream of `adler-core`) need to take the extra
  parameter; pass `&BTreeMap::new()` to preserve the old behaviour.
- **Registry hygiene** — refreshed ~12 stale `known_present`
  usernames (Bitwarden Forum, Ask Fedora, Archive of Our Own,
  BitBucket, Duolingo, Gravatar, ImgUp.cz, Kick, Kongregate,
  Opensource, Xbox Gamertag, moikrug); dropped two non-discriminating
  signals.
- **Pruned `bot-protected` tag** — Snapchat and TikTok detect
  cleanly through a residential IP, so they no longer need a browser.
- **Dependency bumps** — `reqwest 0.12 → 0.13`; relaxed the
  `=`-version pins on `scraper` / `wiremock` now that CI is on a
  Clippy that accepts `let`-chains; `actions/checkout 4 → 6`,
  `actions/upload-artifact 4 → 7`, four other cargo-group bumps.

### Removed

- Three too-permissive site signatures
  (`Replit.com`, `RedTube`, `YouPorn`) that fired Found on a
  nonsense username during the residential validation pass.

### Fixed

- Clippy 1.95 lints across the new browser / CDP code (manual
  let-else, future-not-send, redundant pass-by-value, double
  `#[must_use]`, doc-markdown, etc.).

### Migration

If you build on top of `adler-core` and implement `BrowserBackend`
yourself, change:

```rust
async fn fetch(&self, url: &Url, timeout: Duration) -> Result<RenderedPage>
```

to:

```rust
async fn fetch(
    &self,
    url: &Url,
    headers: &std::collections::BTreeMap<String, String>,
    timeout: Duration,
) -> Result<RenderedPage>
```

Pass `&BTreeMap::new()` from callers that don't need custom headers.

## [0.1.0] — 2026-05-23

Initial public release.

[Unreleased]: https://github.com/commit3296/adler/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/commit3296/adler/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/commit3296/adler/releases/tag/v0.1.0
