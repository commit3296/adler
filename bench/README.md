# bench/ — Adler measured against the field

A reproducible benchmark of **Adler vs Sherlock, Maigret, Blackbird, and
Snoop** on the same fixed dataset of usernames and sites. The output is a
local `bench/RESULTS.md` — a markdown table with per-tool recall, precision,
and wall-clock numbers from your network.

`RESULTS.md` is **not** checked in: a single committed file gives a misleading
"official" verdict, and per-operator results vary too much (residential vs
datacenter IP, country, network conditions) for one number to mean much.

## What this measures

For each tool we run the same `(username, site-list)` job and compare the
tool's verdicts against a small **ground-truth** dataset of
`(username, site, expected)` triples. The dataset is derived from Adler's
registry `known_present` field — authoritative "this username exists on this
site" pairs we keep validated with `adler --doctor` — plus a handful of
**synthetic-nonsense** usernames known not to exist anywhere.

Metrics:

- **Recall** — true positives / (true positives + false negatives).
  *Did the tool find the accounts we know exist?*
- **Precision** — true positives / (true positives + false positives).
  *When the tool said Found, was it right?*
- **Wall-clock** — end-to-end seconds per `(tool, username)` invocation.
- **Uncertain rate** *(Adler only)* — fraction of cases where Adler reports
  `Uncertain(reason)` rather than a binary verdict. The other tools don't
  have this concept; for them every case lands as found or notfound.

## What this is NOT

- A raw-HTTP-throughput shootout. Tools differ in concurrency models and
  per-host throttling; wall-clock alone is only meaningful within the same
  network conditions.
- A replacement for the Criterion microbenchmarks under
  `adler-core/benches/`. Those measure CPU hot paths inside Adler itself;
  this harness measures live OSINT outcomes across tools and networks.
- A claim of "N× faster" or "X % more accurate" without measurement — both
  numbers come from running this harness on your network.
- A drop-in replacement for `--doctor`. This harness compares *tools*;
  `--doctor` validates *signatures*.

## How to run

```bash
cd bench/
./run.sh                  # install each tool in its own venv if missing,
                          # run over ground-truth.tsv, write RESULTS.md
./run.sh --tool adler     # one tool only
./run.sh --user torvalds  # one username only
./run.sh --clean          # wipe results/ and re-run
```

Re-running is idempotent: each `(tool, user)` pair is only re-run if its
`results/<tool>/<user>.json` is missing.

## Adler microbenchmarks

The Rust crate also carries Criterion benches for regressions that are too
small to show up in the field harness but run on every scan:

```bash
cargo bench -p adler-core --benches
cargo bench -p adler-core --bench registry
cargo bench -p adler-core --bench permute
cargo bench -p adler-core --bench correlate
```

Use them before and after a change to registry loading/filtering,
username permutation, correlation, or executor throughput. Criterion writes
HTML reports under `target/criterion/`; the manual GitHub Actions
`bench` workflow runs the same suite and uploads those reports as an
artifact. Treat hosted-runner numbers as advisory: compare runs from the
same runner class or, for serious tuning, from the same local machine.

Dependencies:

- **Python ≥ 3.11** plus `pip` and `venv`.
- **`adler`** on `PATH`. Build with the feature you want measured:
  `cargo install --path adler-cli --features impersonate` to include the
  TLS-fingerprint transport, or plain `--path adler-cli` to compare the
  baseline.
- **Network from a real residential IP** if you want representative numbers.
  Datacenter IPs see much higher Uncertain / NotFound rates across all
  tools — see Adler's *Detection rate* section in the root README.

## Files

```
bench/
├── README.md                  # this file
├── ground-truth.tsv           # (username, site, expected_verdict) — the dataset
├── sites.tsv                  # canonical site → per-tool name + URL pattern
├── synthetic-nonsense.txt     # nonsense usernames + how we generate them
├── derive-ground-truth.py     # regenerates ground-truth.tsv from adler-core/data
├── run.sh                     # orchestrator
├── adapters/                  # per-tool: invoke + parse → normalized verdicts
│   ├── adler.py
│   ├── sherlock.py
│   ├── maigret.py
│   ├── blackbird.py
│   └── snoop.py
├── analyze.py                 # ground-truth × per-tool → RESULTS.md
├── venvs/                     # gitignored: per-tool Python venvs
└── results/                   # gitignored: raw per-run JSON + wall-clock
```

## Honest limits

- The dataset is small by ML standards — roughly 20 usernames × 30 sites =
  600 labeled cases. Confidence intervals are wide; the report prints `n =`
  per row so this is obvious.
- Ground truth is derived from Adler's own `known_present`. That's a bias
  toward Adler in the sense that we picked sites where we already verified
  the username exists. We mitigate by running the **same** `(user, site)`
  pairs through every tool: if a tool fails to find a known-present account,
  that's a real recall miss, not measurement bias.
- All tools share the network conditions of the machine that runs the
  harness. A US residential IP gives different numbers than a Frankfurt
  datacenter IP. We do not normalize for this — record your scan source in
  the `RESULTS.md` header.
- Tools have overlapping but non-identical site registries. The harness
  scopes each tool to its supported subset of the canonical site list and
  treats "this tool doesn't know about this site" as missing data, not as a
  recall miss.

## When you publish numbers

Run the harness on your machine, attach the resulting `RESULTS.md` plus a
note of your scan source (residential / datacenter / country / approximate
RTT) when discussing numbers in issues or pull requests. The harness prints
all four into `RESULTS.md`'s header so it travels with the data.
