<p align="center">
  <img src="banner.png" alt="Adler" />
</p>

<p align="center">
  <a href="https://github.com/commit3296/adler/actions/workflows/ci.yml"><img src="https://github.com/commit3296/adler/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://crates.io/crates/adler-cli"><img src="https://img.shields.io/crates/v/adler-cli.svg" alt="crates.io"></a>
  <a href="https://docs.rs/adler-core"><img src="https://docs.rs/adler-core/badge.svg" alt="docs.rs"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
</p>

# Adler

> *Named for Irene Adler — "the Woman", the one who outwitted Sherlock Holmes.
> Where Sherlock searched, Adler outsmarts.*

OSINT username search across hundreds of sites. A modern successor to Sherlock —
multi-signal detection, self-healing site signatures, optional enrichment and
cross-site correlation, written in Rust.

> **Status:** in development. See [PLAN.md](PLAN.md) for the full roadmap.

## Crates

| Crate         | Kind | Purpose                                              |
| ------------- | ---- | ---------------------------------------------------- |
| `adler-core`  | lib  | Detection engine, site registry, executor.          |
| `adler-server`| lib  | HTTP API + SSE streaming + scan persistence; embeds the SolidJS web UI via `rust-embed`. |
| `adler-cli`   | bin  | `adler` command-line interface; `--web` launches the embedded server + UI in-process. |

## Install

From crates.io (compiles locally, ~1–2 min):

```bash
cargo install adler-cli
```

Pre-built binary from the GitHub release (instant, no compile):

```bash
cargo binstall adler-cli            # https://github.com/cargo-bins/cargo-binstall
```

From source:

```bash
git clone https://github.com/commit3296/adler.git
cd adler
cargo install --path adler-cli
```

Requires Rust ≥ 1.85. The installed binary is `adler`. The library
([`adler-core`](https://crates.io/crates/adler-core)) is published separately
for embedding the engine in your own tools — see the
[*Library*](#library) section below.

## Build & run

```bash
cargo build --workspace
cargo run -p adler-cli -- alice
```

Logging is controlled by the `ADLER_LOG` env var (defaults to `adler=info`):

```bash
ADLER_LOG=adler=debug cargo run -p adler-cli -- alice
```

## Detection rate

Recall depends on where you scan from. A `--doctor` pass on 2026-05-26
against the bundled registry (411 sites):

| Scan source | Sites where a known-existing account is found | Recall |
| --- | ---: | ---: |
| Datacenter IP (Hetzner / Leaseweb DE) | 282 / 411 | 68.6% |
| US residential proxy pool (DECODO) | **305 / 411** | **74.2%** |

The residential lift is real: ~40 sites swap their verdict between
`Uncertain` (datacenter) and `Found` (residential) — most are
Cloudflare-walled or geo-restricted (RU-segment, plus platforms like
Reddit, Imgur, Patreon). The remaining ~26% breaks down roughly as:

- **Bot-protected sites** tagged `bot-protected` (Instagram and
  X/Twitter today) — these serve a JS login wall to a plain HTTP
  request; a clean IP doesn't help, you need a browser backend.
  Exclude them with `--exclude-tag bot-protected`.
- **Stale Sherlock-imported `known_present` accounts** that no
  longer exist on the live site. The `--doctor --suggest-known-present`
  tool (new in v0.4.0) probes a small candidate pool (the site's
  brand name, plus `torvalds` / `octocat` / `admin` / …) and prints
  a paste-ready snippet for any site where it finds a live account.
  Discovery surfaced 19 healable entries on the most recent sweep;
  the remaining placeholders need either a contributor-found
  candidate or a deeper repair via `--doctor --fix`.
- **Sites whose detection rule fires for *every* username** —
  signal repair territory, not username repair. `--doctor --fix`
  diffs the responses and proposes a tighter signal.
- **Sites that don't reliably distinguish found from not-found** for
  unauthenticated requests at all — investigated and not added
  rather than ship false-positive entries: Reddit, TikTok,
  Pinterest, and Threads. See issues
  [#11–#14](https://github.com/commit3296/adler/issues?q=is%3Aissue+label%3A%22help+wanted%22)
  for the specific failure modes and what would unblock each.

Run the same check yourself: `adler --doctor` (uses your current IP)
or `adler --doctor --proxy <url>` (via your own proxy). With
`--browser-backend browserbase` the doctor's `--fix` mode routes
bot-protected sites through a real Chrome session, so the diff sees
real profile pages rather than two identical login walls. With
`--suggest-known-present` you get an OVERRIDES block per healable
site.

## Browser backend (optional)

A small subset of sites — currently **Instagram and Twitter**
(`adler --list-tags` shows the live count; the tag is kept narrow
because every additional candidate we investigated either detects
fine without a browser or is structurally unscrapable even *with*
one — see *Detection rate* above) — serve a JavaScript login wall
or a Cloudflare challenge to a plain HTTP request. They're tagged
`bot-protected` and, on the raw HTTP path, will *always* return
`Uncertain` because the response looks identical for an existing
account and a missing one.

With `--browser-backend` Adler routes those sites (and *only* those —
everything else stays on the fast HTTP path) through a real headless
Chrome that runs JS, accepts cookies, and returns the final post-render
DOM. The same detection signals then apply, and a verdict becomes
possible.

Two backends are supported, picked at the CLI:

| Flag | What it does | Cost | Requirements |
|---|---|---|---|
| `--browser-backend local` | Launches headless Chrome on your machine via [`chromiumoxide`](https://crates.io/crates/chromiumoxide) | Free | Chrome / Chromium installed locally |
| `--browser-backend browserbase` | Opens a remote session on [Browserbase](https://browserbase.com) and connects over the CDP WebSocket | Pay per session-minute (≈ $0.05/min) | `ADLER_BROWSERBASE_API_KEY` and `ADLER_BROWSERBASE_PROJECT_ID` env vars. Drives CDP through a small in-tree async client (`adler-core/src/browser/cdp.rs`) — neither `chromiumoxide` nor `headless_chrome` could attach to Browserbase's remote browser cleanly (issue #5), so we wrote our own. |

Both reuse a single browser instance across all routed fetches for the
scan, so cost / setup overhead is one-time.

### Examples

```bash
# Use local Chrome — pairs cleanly with --proxy (passed through as
# --proxy-server to the child process).
adler --browser-backend local --proxy socks5h://USER:PASS@HOST:PORT alice

# Cloud session with residential / mobile IP and anti-fingerprint baked in.
export ADLER_BROWSERBASE_API_KEY=bb_live_...
export ADLER_BROWSERBASE_PROJECT_ID=...
adler --browser-backend browserbase alice

# Cap the number of browser-routed probes (default 50). Once exceeded,
# remaining bot-protected sites return Uncertain(browser_budget_exceeded).
adler --browser-backend browserbase --browser-budget 10 alice

# Disable for one run even if the env / a shell alias has it on.
adler --no-browser alice
```

### Guardrails

- **Per-scan budget** — `--browser-budget N` caps how many browser
  fetches a single scan may consume. Default is 50, ≈ 5× the
  `bot-protected` subset of the registry, so the cap only ever fires if
  a flag is misconfigured.
- **No surprise routing** — only sites tagged `bot-protected` are sent
  through the browser. Everything else is unaffected. Use
  `adler --list-tags` to see what's tagged.
- **Privacy** — the `browserbase` backend sends the URLs you scan to a
  third-party US-based service. The `local` backend doesn't leave your
  machine (modulo whatever proxy you've configured Chrome to use).

### Trade-offs vs. raw HTTP

Browser fetches are inherently 5–10× slower than raw HTTP and (for
`browserbase`) cost real money. They're the only way to detect
accounts on the bot-protected subset, but on the rest of the registry
they would add latency for no recall gain — which is why routing is
opt-in and tag-driven, not blanket.

## Usage

```bash
adler alice                       # scan the embedded registry
adler --only github,gitlab alice  # restrict to matching sites
adler --exclude reddit alice      # drop matching sites
adler --list-sites --only git     # discover filter terms (no scan)
adler --tag social,dev alice      # scan only sites tagged social or dev
adler --tag region:ru alice       # scan only Russia-region sites
adler --exclude-tag bot-protected alice  # skip login-walled sites (fast clean run)
adler --list-tags                 # show all tags + site counts (no scan)
adler --explain alice             # show which signal produced each verdict
adler --input users.txt           # batch: scan many usernames, grouped output
adler --watch alice               # diff against the last run; new/removed accounts
adler --watch --interval 3600 alice  # keep watching every hour
adler --all alice                 # also show NotFound rows (hidden by default)
adler -q alice                    # quiet: print only found URLs
adler --color never alice         # never colorize (also honors NO_COLOR)

# output formats
adler --format json alice         # JSON array
adler --format ndjson alice       # one JSON object per line (jq-friendly)
adler --format csv alice > out.csv  # spreadsheet-friendly table
adler --format html alice > out.html   # self-contained HTML report

# interactive web UI (see § Web UI below)
adler --web                       # launch http://127.0.0.1:8080 with the bundled SPA
adler --web --web-bind 0.0.0.0:9000  # custom address

# deeper analysis (these fetch fresh data, bypassing the cache)
adler --enrich alice              # extract name/bio/avatar from profiles
adler --correlate alice           # group accounts that look like one person
adler --permute aggressive alice  # also search spelling variants

# throughput & network hygiene
adler --concurrency 64 alice      # more in-flight probes (default 32)
adler --proxy socks5://host:1080 alice
adler --proxy-pool pool.toml alice  # geo/IP-type egress pool (see § Egress pool)
adler --tor alice                 # local Tor SOCKS proxy
adler --rotate-ua alice           # rotate User-Agent per request
adler --max-rps 5 alice           # cap total request rate

# shell completions
adler --completions zsh > _adler
```

By default the text output shows Found and Uncertain results and hides the
(usually many) NotFound rows — pass `--all` for the full list. On an
interactive terminal, results stream in as they resolve; piped output is
collected and ordered. For an interactive browser-based view of a running
scan — search, filter, evidence drawers, side-by-side diff against an older
scan — pass `--web` (see [*Web UI*](#web-ui) below).

Results are cached between runs (`~/.cache/adler/`, 1 h TTL); use
`--no-cache`, `--cache-ttl`, or `--cache-clear` to control it. Exit codes:
`0` something found, `1` nothing found, `2` error.

## Web UI

`adler --web` boots a small in-process HTTP server and serves a SolidJS
SPA from the same binary — no separate frontend deployment, no extra
process to manage. Once the server is up, kick off scans, watch outcomes
stream in over SSE, persist them to disk, and diff them against earlier
runs.

```bash
adler --web                          # http://127.0.0.1:8080
adler --web --web-bind 0.0.0.0:9000  # listen on all interfaces, custom port
```

What you get in the browser:

- **Live scan view** — outcomes stream in as they resolve (SSE), grouped
  by category, with per-row evidence (verdict reason, response snippet,
  URL) and a one-click retry.
- **History modal** — every finished scan is persisted to
  `~/.cache/adler/scans/` (oldest 200, atomic writes). Reopen any past
  scan via `#/scan/<id>` deep-links.
- **Compare with previous** — pick any two persisted scans and diff
  them side-by-side (`#/diff/<a>/<b>`); shows accounts gained / lost /
  flipped between the two runs. Esc / back-button exits.
- **Filters & sort** — by verdict, category, presence of evidence,
  hidden NotFound rows. Preferences persist to localStorage.
- **NSFW gate** — off by default; the toggle is hidden behind a
  confirmation, matching the CLI's `--nsfw` opt-in.

The server exposes a small JSON API at `/api/*` (`/health`, `/sites`,
`/scans`, `POST /scan`, `GET /scan/:id`, `GET /scan/:id/stream`,
`POST /scan/:id/retry`) — useful if you want to drive Adler from a
different frontend or a script. SSE consumers should subscribe to the
`/stream` endpoint and treat each event as one outcome.

The bundled SPA is baked into the binary at compile time
(`rust-embed`), so the deployed unit is just the `adler` executable
plus whatever scan-cache directory you point it at. The SolidJS
project lives at `adler-server/web/`; if you build from source, run
`npm ci && npm run build` there before `cargo build` — Vite emits
`web/dist/`, which `rust-embed` reads directly.

## Performance

A scan is network-bound: the engine itself is negligible. The `executor::run`
benchmark (`cargo bench -p adler-core`) fans out 50 probes against a local
mock server in **~1.6 ms total — roughly 32 µs per site** of framework
overhead (~30K sites/s), while a real HTTP request takes 100–1000 ms. So
wall-clock time is set almost entirely by how many requests are in flight.

The lever that matters is therefore concurrency, not micro-optimisation:

- `--concurrency` (default **32**) bounds in-flight probes. Most sites are
  distinct hosts, so the per-host throttle rarely serialises; raising it
  (e.g. `--concurrency 64`) shortens large scans, with diminishing returns
  past your network's limits.
- The result cache (`~/.cache/adler/`) skips re-probing unchanged sites
  between runs entirely.
- `--max-rps` trades throughput for politeness when you need a global cap.

## Egress pool (geo routing)

Some sites only answer from a particular country, or block datacenter
IP ranges. A site can declare what egress it needs via its `access`
policy in the registry (a country and/or an IP type); `--proxy-pool`
supplies the proxies that satisfy those requirements.

`--proxy` still routes *everything* through one proxy (the default
egress). `--proxy-pool` is additive and **only** kicks in for sites
whose `access` policy requires a specific egress — everything else
keeps using the default. If a site needs an egress the pool can't
provide, it's reported `Uncertain(geo_unavailable)` rather than fetched
from the wrong place — a location you can't reach is not evidence the
account is absent.

The pool is a TOML file of `[[egress]]` entries:

```toml
# pool.toml
[[egress]]
url = "socks5://user:pass@pl.example.com:1080"
country = "pl"          # ISO-3166-1 alpha-2 (lowercased)
kind = "residential"    # datacenter (default) | residential | mobile | tor

[[egress]]
url = "http://de.example.com:8080"
country = "de"
# kind omitted → datacenter
```

```bash
adler --proxy-pool pool.toml alice
```

Bring your own proxies — Adler ships the routing, not the egress. The
browser backend keeps its own egress (e.g. Browserbase's residential
IPs); `--proxy-pool` routes the raw-HTTP path.

## Sessions (reach login-walled sites)

Some sites only show a profile to a logged-in user (Instagram, Threads,
Reddit's JSON). A site can declare `access.session = "<name>"` in the
registry; `--sessions <file>` supplies that named session's headers —
your own (or a sock-puppet) account's — applied to the site's probe so
it sees a real session instead of a login wall.

This is "use a real account", not evasion: Adler doesn't solve
challenges or forge anything; you bring a session you're entitled to.
If a site names a session you didn't supply, it's reported
`Uncertain(session_required)` rather than a login-wall false negative.

The file is TOML; each `[name]` table is a set of HTTP headers (copy
them from your browser's devtools):

```toml
# sessions.toml
[ig]
Cookie = "sessionid=...; csrftoken=..."
X-IG-App-ID = "936619743392459"

[reddit]
Cookie = "reddit_session=..."
```

```bash
adler --sessions sessions.toml alice
```

Header values are secrets — redacted from logs, never written to scan
output. Using a sock-puppet account may breach a site's ToS; that's an
operator decision within your engagement's scope.

## TLS-fingerprint impersonation (optional build feature)

Some sites read the TLS handshake's JA3 / JA4 fingerprint and serve a
block page to anything that doesn't look like a real browser — `rustls`
or `reqwest`'s default fingerprints are well-known and easy to
filter. Sites tagged `protection: tls-fingerprint` in the registry
declare this.

Build Adler with the `impersonate` feature to enable an in-process
`wreq` HTTP client emulating Chrome 134 (BoringSSL handshake matches
real Chrome's JA3 / JA4 / HTTP-2 fingerprint). Sites whose protection
is *only* TLS fingerprint then route through it — much cheaper than
spinning up a real browser:

```bash
cargo install adler-cli --features impersonate
```

The feature pulls in BoringSSL and needs `cmake`, a C++ compiler, and
`libclang` at build time (on Fedora: `dnf install cmake gcc-c++
clang`; on Debian/Ubuntu: `apt install cmake clang libclang-dev`).
`cargo binstall adler-cli` ships impersonate-enabled binaries for
x86_64-linux, both macOS targets, and Windows; the
`aarch64-unknown-linux-gnu` binary is built without the feature (cross-
compiled BoringSSL toolchain isn't wired up), so on aarch64 Linux use
`cargo install adler-cli --features impersonate` instead. Sites with
mixed protections (e.g. `tls-fingerprint` + `cloudflare`) stay on the
browser-backend path.

## Library

`adler-core` is the runtime-agnostic engine that powers the CLI;
it's published separately on
[crates.io](https://crates.io/crates/adler-core) so you can embed
username detection in your own Rust tools. Add to your `Cargo.toml`:

```toml
[dependencies]
adler-core = "0.8"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Minimal worked example — load the embedded registry, scan one
username, print the hits:

```rust
use adler_core::{Client, ExecutorOptions, MatchKind, Registry, Username, executor};

#[tokio::main]
async fn main() -> adler_core::Result<()> {
    let registry = Registry::default_embedded()?;
    // filter(include, exclude, tags, exclude_tags, include_nsfw)
    // — empty slices = no name/tag filter; `false` keeps the
    // default NSFW auto-exclusion.
    let sites = registry.filter(&[], &[], &[], &[], false);
    let username = Username::new("torvalds")?;
    let client = Client::builder().build()?;

    let outcomes = executor::run(
        &client, &sites, &username, ExecutorOptions::default(),
    ).await;

    for outcome in outcomes.iter().filter(|o| o.kind == MatchKind::Found) {
        println!("found: {} → {}", outcome.site, outcome.url);
    }
    Ok(())
}
```

See [`docs.rs/adler-core`](https://docs.rs/adler-core) for the
full API. Notable knobs:

| | |
|---|---|
| `Client::builder()` | timeout, redirect policy, user-agent rotation, proxy, retry, rotate-UA, throttle, cache, browser backend, NSFW gate. |
| `Registry::filter` | include/exclude by name substring, tag, `nsfw` opt-in (the 5th `include_nsfw: bool` parameter — pass `true` to scan adult sites). |
| `Site::request_headers` | per-site HTTP headers (e.g. Instagram's `X-IG-App-ID`); browser backends apply via `Network.setExtraHTTPHeaders`. |
| `Site::regex_check` | per-site username-validity regex. Mismatched usernames short-circuit to `Uncertain(UsernameNotAllowed)` without a network request. |
| `Site::known_present` | `KnownPresent::Single(String)` or `KnownPresent::Multiple(Vec<String>)`; `--doctor` passes if **any** declared username resolves to `Found`. |
| `BrowserBackend` trait | route bot-protected sites through real Chrome. Built-in: `LocalBackend` (chromiumoxide) and `BrowserbaseBackend` (cloud CDP). |

**Breaking changes since 0.1:** the `Registry::filter` signature
grew an `include_nsfw: bool` (v0.4.0), `Site::known_present` now
accepts a `KnownPresent` enum instead of `Option<String>` (v0.3.0),
`Site::request_headers` and `Site::regex_check` are new fields
(v0.2.0 / v0.4.0 respectively). The
[CHANGELOG](CHANGELOG.md) has the migration notes for each.

## Site registry

The default registry (`adler-core/data/sites.json`, ~2.5k sites) is generated
from MIT-licensed upstream data — the
[Sherlock project](https://github.com/sherlock-project/sherlock) (base) plus
the [Maigret project](https://github.com/soxoj/maigret) (engine-inherited
forum platforms and additional sites) — via `scripts/import_sherlock.py`
and `scripts/import_maigret.py`. Detections are imported **unverified** —
upstream signatures rot over time. Validate them with the built-in health
check:

```bash
adler --doctor                 # check every site's signature
adler --doctor --only github   # check a subset
```

`--doctor` probes each site's known-present user (must be Found) and a random
nonsense user (must not be Found), reporting any site whose detection no
longer holds. `--doctor --fix` additionally suggests a corrected signature
for failing sites by diffing the present/absent responses. A nightly GitHub
Actions workflow (`.github/workflows/doctor.yml`) runs the check across the
whole registry and flags structural rot.

A supplementary registry derived from
[WhatsMyName](https://github.com/WebBreacher/WhatsMyName) is shipped in
`adler-core/data/sites_wmn.json` and is **included by default** for
maximum coverage — it adds ~675 sites with two-sided body+status
detection signatures. The file is licensed CC BY-SA 4.0; if you
redistribute Adler scan output and need an MIT-only data lineage,
pass `--no-wmn` to drop the tranche.

## Quality bar

CI must pass on every push:

```bash
cargo fmt --all --check
cargo clippy --all-targets --workspace -- -D warnings
cargo test --workspace
```

## Ethics & responsible use

Adler aggregates publicly reachable profile URLs, but aggregation makes
intrusion easy — please use it responsibly.

**Intended uses:** checking your own accounts; authorized penetration tests
and bug-bounty engagements; security research; and OSINT investigations with
a lawful basis. **Do not** use Adler to stalk, harass, dox, or surveil
people without authorization, or to mass-target individuals.

**Detect, never circumvent.** Adler reports anti-bot responses (rate limits,
Cloudflare challenges, captchas) as `Uncertain` — it does not solve captchas
or bypass access controls. It rate-limits per host, supports `--max-rps` and
`--respect-robots`, and writes an optional `--audit-log` of every request.
See [SECURITY.md](SECURITY.md) and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## License

The Adler **code** is licensed under the [MIT License](LICENSE).

The default site registry (`adler-core/data/sites.json`) is also under MIT
— it is derived from the Sherlock project (MIT) and the Maigret project
(MIT). See the file's `_comment` header and the corresponding importer
scripts in `scripts/` for attribution.

The supplementary registry (`adler-core/data/sites_wmn.json`, included
by default; opt-out with `adler --no-wmn`) is derived from WhatsMyName
and licensed [CC BY-SA 4.0](LICENSE-CC-BY-SA-4.0). Adler's MIT licence
does not cover this file; downstream redistribution must preserve
attribution and the `ShareAlike` obligation on derivative data.
