<p align="center">
  <img src="banner.png" alt="Adler" />
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

## Build & run

```bash
cargo build --workspace
cargo run -p adler-cli -- alice
```

Logging is controlled by the `ADLER_LOG` env var (defaults to `adler=info`):

```bash
ADLER_LOG=adler=debug cargo run -p adler-cli -- alice
```

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

`adler-core` is usable as a crate; see the [crate docs](https://docs.rs/adler-core)
(`cargo doc -p adler-core --open`) for a worked example.

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
