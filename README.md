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

| Crate         | Kind | Purpose                                     |
| ------------- | ---- | ------------------------------------------- |
| `adler-core` | lib  | Detection engine, site registry, executor. |
| `adler-cli`  | bin  | `adler` command-line interface.            |

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
adler --tui alice                 # interactive results browser

# deeper analysis (these fetch fresh data, bypassing the cache)
adler --enrich alice              # extract name/bio/avatar from profiles
adler --correlate alice           # group accounts that look like one person
adler --permute aggressive alice  # also search spelling variants

# throughput & network hygiene
adler --concurrency 64 alice      # more in-flight probes (default 32)
adler --proxy socks5://host:1080 alice
adler --tor alice                 # local Tor SOCKS proxy
adler --rotate-ua alice           # rotate User-Agent per request
adler --max-rps 5 alice           # cap total request rate

# shell completions
adler --completions zsh > _adler
```

By default the text output shows Found and Uncertain results and hides the
(usually many) NotFound rows — pass `--all` for the full list. On an
interactive terminal, results stream in as they resolve; piped output is
collected and ordered. `--tui` opens a live browser (results stream in as the
scan runs): `/` search, `f` filter by verdict, `g`/`G`/PageUp/PageDown to
navigate, `o` open the selected URL, `y`/`Y` copy one/all URLs, `Enter` for
details, `?` for the full key list. Wide terminals show a persistent
list+detail split.

Results are cached between runs (`~/.cache/adler/`, 1 h TTL); use
`--no-cache`, `--cache-ttl`, or `--cache-clear` to control it. Exit codes:
`0` something found, `1` nothing found, `2` error.

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

## Library

`adler-core` is the runtime-agnostic engine that powers the CLI;
it's published separately on
[crates.io](https://crates.io/crates/adler-core) so you can embed
username detection in your own Rust tools. Add to your `Cargo.toml`:

```toml
[dependencies]
adler-core = "0.4"
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

The default registry (`adler-core/data/sites.json`, ~450 sites) is generated
from the [Sherlock project](https://github.com/sherlock-project/sherlock)'s
MIT-licensed `data.json` via `scripts/import_sherlock.py`. Detections are
imported **unverified** — Sherlock's signatures rot over time. Validate them
with the built-in health check:

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

Licensed under the [MIT License](LICENSE).

The bundled site registry is derived from the Sherlock project (MIT). See
`adler-core/data/sites.json` for attribution.
