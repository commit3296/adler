# Adler — roadmap

OSINT username search across many sites. Successor to Sherlock, written in
Rust. Goals: higher recall and lower false-positive rate than a single-shot
status check, data-driven and self-healing site signatures, modern terminal
UX. This file is intentionally short — the README is the primary docs entry
point, this is the engineering roadmap.

---

## Phase 0 — Bootstrap ✓

Workspace (`adler-core` lib + `adler-cli` bin), workspace-level lints
(clippy pedantic + nursery, `-D warnings`), CI (fmt / clippy / test /
doctest), `tracing` (`ADLER_LOG`), error model (`thiserror`, crate-level
`Error` enum), MIT license.

## Phase 1 — MVP ✓

Multi-signal HTTP detection (status / body marker / redirect marker),
concurrent executor over `tokio::JoinSet`, per-host minimum-interval
throttle, retry with exponential backoff + jitter (transient bans only),
global deadline (sites still in flight produce `Uncertain`), embedded
registry of ~417 sites generated from Sherlock by
`scripts/import_sherlock.py`. CLI: `--format text|json|ndjson`,
`--only`/`--exclude`, `--timeout`/`--concurrency`/`--deadline`/`--sites`,
progress bar, exit codes. Unit + integration tests (`wiremock`,
`assert_cmd`, `insta` snapshots) plus a criterion microbench.

## Phase 2 — Reliability ✓

- **Ensemble:** `Vec<Signal>` per site, negative-priority aggregation —
  any `NotFound` vote wins over `Found`; no votes → `Uncertain`. Symmetric
  body / status / redirect signals.
- **Self-healing:** `adler --doctor` probes `known_present` + a random
  nonsense username and reports broken signatures. `--doctor --fix` diffs
  the responses and prints a ready-to-paste signature. Nightly CI job.
- **Ban awareness:** 429 / `Retry-After` / Cloudflare detection;
  bot-protected sites tagged so users can `--exclude-tag bot-protected`
  for a fast clean run.
- **Cache:** keyed by site × username under `$XDG_CACHE_HOME/adler/`.

## Phase 3 — Profile enrichment & correlation ✓ (partial)

- `--enrich`: optional per-site CSS-selector extractors (name / bio /
  avatar).
- `--correlate`: cross-site profile field correlation across found
  accounts.
- `--format html`: self-contained HTML report including avatars.

## Phase 4 — UX ✓

- **Live TUI** (`--tui`): scan streams in over a channel while you browse;
  rounded panels, master-detail split on wide terminals (≥90 cols),
  default `found + uncertain` filter (`f` cycles through the others),
  `/` incremental search, `o` open in browser, `y`/`Y` copy via OSC 52,
  `n`/`N` jump to next/prev found account, `Enter` toggle detail,
  `?` help overlay. Theme-adaptive contrast.
- **Batch mode:** `--input users.txt` (one username per line).
- **Watch mode:** `adler --watch [--interval N]` — fresh scan, diff the
  found set against the last snapshot, report new / removed accounts.
- **Explainability:** `--explain` prints which signal(s) produced each
  verdict.
- **Output formats:** text / json / ndjson / html / csv.

## Phase 5 — Validation & honest limits

- 232 tests; `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --doc`, and `cargo fmt --all --check` all
  clean.
- `--doctor` ran end-to-end across the bundled registry; the importer's
  `KNOWN_BROKEN` set excludes sites whose Sherlock signature was found to
  be too-permissive (200 for any username) or too-restrictive (homepage
  chrome treated as a `NotFound` marker).
- **Not yet validated:** detection accuracy at scale from a residential
  IP. Empirically, a datacenter SOCKS proxy does **not** reduce bans on
  bot-protected sites (Instagram, X / Twitter, TikTok, Facebook, Threads,
  Snapchat, Weibo are tagged `bot-protected`); reliable detection there
  needs a residential IP or a browser backend, both deferred.

## Pending / future

- Browser backend (Playwright via Browserbase) for `bot-protected` sites.
- Real benchmark vs Sherlock on 50+ live sites with a clean network.
- First GitHub Release with pre-built binaries (the `release.yml`
  workflow is wired up; tag `v0.1.0` to trigger it).
- crates.io publish (`adler-core` then `adler-cli`).
- More enrichment extractors as the registry curation matures.

## Quality bar (current)

`cargo build --workspace --all-targets`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --all-targets`,
`cargo test --workspace --doc`,
`cargo fmt --all --check` — all must pass; CI enforces.
