#!/usr/bin/env bash
# Compare Adler and Sherlock against the same username/site sample.
#
# This is a real-network benchmark harness, not a CI test. Run it from a
# stable network, preferably with a freshly built release binary:
#
#   scripts/bench-vs-sherlock.sh --install-sherlock
#   ADLER_BIN=target/release/adler scripts/bench-vs-sherlock.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEFAULT_USERS="$ROOT/scripts/bench-vs-sherlock-usernames.txt"
DEFAULT_SITES="$ROOT/scripts/bench-vs-sherlock-sites.txt"

USERS_FILE="$DEFAULT_USERS"
SITES_FILE="$DEFAULT_SITES"
OUT_DIR="$ROOT/bench-results/sherlock"
ADLER_BIN="${ADLER_BIN:-}"
SHERLOCK_BIN="${SHERLOCK_BIN:-sherlock}"
INSTALL_SHERLOCK=0
LIMIT=0
TIMEOUT_SECS=30
DRY_RUN=0

usage() {
    cat <<'EOF'
Usage: scripts/bench-vs-sherlock.sh [options]

Options:
  --usernames PATH       newline-delimited usernames (default: scripts/bench-vs-sherlock-usernames.txt)
  --sites PATH           newline-delimited shared site names (default: scripts/bench-vs-sherlock-sites.txt)
  --out-dir PATH         result directory (default: bench-results/sherlock)
  --adler-bin PATH       Adler binary (default: build target/release/adler)
  --sherlock-bin PATH    Sherlock executable (default: sherlock)
  --install-sherlock     install sherlock-project with pipx when missing
  --limit N              run only the first N usernames
  --timeout SECS         per-site Sherlock timeout (default: 30)
  --dry-run              print planned commands and exit
  -h, --help             show this help

Outputs:
  <out-dir>/results.csv              append-only per-tool timings/counts
  <out-dir>/<timestamp>/summary.md   markdown summary for this run
  <out-dir>/<timestamp>/raw/*        raw Adler/Sherlock outputs
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --usernames) USERS_FILE="$2"; shift 2 ;;
        --sites) SITES_FILE="$2"; shift 2 ;;
        --out-dir) OUT_DIR="$2"; shift 2 ;;
        --adler-bin) ADLER_BIN="$2"; shift 2 ;;
        --sherlock-bin) SHERLOCK_BIN="$2"; shift 2 ;;
        --install-sherlock) INSTALL_SHERLOCK=1; shift ;;
        --limit) LIMIT="$2"; shift 2 ;;
        --timeout) TIMEOUT_SECS="$2"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "missing required command: $1" >&2
        exit 2
    }
}

read_list() {
    local file="$1"
    python3 - "$file" <<'PY'
import sys
for line in open(sys.argv[1], encoding="utf-8"):
    line = line.strip()
    if line and not line.startswith("#"):
        print(line)
PY
}

now_ms() {
    python3 - <<'PY'
import time
print(time.time_ns() // 1_000_000)
PY
}

json_counts() {
    local tool="$1"
    local file="$2"
    python3 - "$tool" "$file" <<'PY'
import json, sys
tool, path = sys.argv[1], sys.argv[2]
try:
    data = json.load(open(path, encoding="utf-8"))
except Exception:
    print("0,0,0,0")
    raise SystemExit

def walk(obj):
    if isinstance(obj, dict):
        yield obj
        for value in obj.values():
            yield from walk(value)
    elif isinstance(obj, list):
        for value in obj:
            yield from walk(value)

found = not_found = uncertain = 0
if tool == "adler":
    rows = data if isinstance(data, list) else []
    for row in rows:
        kind = str(row.get("kind", "")).lower()
        if kind == "found":
            found += 1
        elif kind == "not_found":
            not_found += 1
        else:
            uncertain += 1
else:
    seen = 0
    for row in walk(data):
        status = str(row.get("status") or row.get("status_message") or "").lower()
        if not status:
            continue
        seen += 1
        if any(token in status for token in ("claimed", "found", "exists")):
            found += 1
        elif any(token in status for token in ("available", "not found", "not_found")):
            not_found += 1
        else:
            uncertain += 1
    if seen == 0 and isinstance(data, dict):
        for value in data.values():
            if isinstance(value, dict) and value.get("url_user"):
                found += 1
total = found + not_found + uncertain
print(f"{total},{found},{not_found},{uncertain}")
PY
}

summary_md() {
    local csv="$1"
    local run_id="$2"
    local out="$3"
    python3 - "$csv" "$run_id" "$out" <<'PY'
import csv, statistics, sys
csv_path, run_id, out_path = sys.argv[1:]
rows = [r for r in csv.DictReader(open(csv_path, encoding="utf-8")) if r["run_id"] == run_id]
by_tool = {}
for row in rows:
    by_tool.setdefault(row["tool"], []).append(row)

def pct(values, p):
    if not values:
        return 0
    values = sorted(values)
    idx = min(len(values) - 1, round((p / 100) * (len(values) - 1)))
    return values[idx]

with open(out_path, "w", encoding="utf-8") as f:
    f.write(f"# Adler vs Sherlock benchmark ({run_id})\n\n")
    f.write("| Tool | Runs | Wall total | Mean/run | p50 | p95 | Found | NotFound | Uncertain |\n")
    f.write("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n")
    for tool in sorted(by_tool):
        vals = [int(r["elapsed_ms"]) for r in by_tool[tool]]
        found = sum(int(r["found"]) for r in by_tool[tool])
        not_found = sum(int(r["not_found"]) for r in by_tool[tool])
        uncertain = sum(int(r["uncertain"]) for r in by_tool[tool])
        f.write(
            f"| {tool} | {len(vals)} | {sum(vals)} ms | {statistics.mean(vals):.0f} ms | "
            f"{pct(vals, 50)} ms | {pct(vals, 95)} ms | {found} | {not_found} | {uncertain} |\n"
        )
    f.write("\nRaw outputs are in this run's `raw/` directory. Counts are verdict breakdowns, not ground-truth accuracy labels.\n")
PY
}

need python3

mapfile -t USERS < <(read_list "$USERS_FILE")
mapfile -t SITES < <(read_list "$SITES_FILE")
if [[ ${#USERS[@]} -eq 0 || ${#SITES[@]} -eq 0 ]]; then
    echo "username and site lists must both be non-empty" >&2
    exit 2
fi
if [[ "$LIMIT" -gt 0 && "$LIMIT" -lt "${#USERS[@]}" ]]; then
    USERS=("${USERS[@]:0:$LIMIT}")
fi

if [[ -z "$ADLER_BIN" ]]; then
    ADLER_BIN="$ROOT/target/release/adler"
    [[ -x "$ADLER_BIN" ]] || cargo build --release -p adler-cli
fi
if ! command -v "$SHERLOCK_BIN" >/dev/null 2>&1; then
    if [[ "$INSTALL_SHERLOCK" -eq 1 ]]; then
        need pipx
        pipx install sherlock-project
    else
        echo "missing Sherlock executable: $SHERLOCK_BIN (try --install-sherlock)" >&2
        exit 2
    fi
fi

RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="$OUT_DIR/$RUN_ID"
RAW_DIR="$RUN_DIR/raw"
CSV="$OUT_DIR/results.csv"
mkdir -p "$RAW_DIR" "$OUT_DIR"
if [[ ! -f "$CSV" ]]; then
    echo "run_id,tool,username,sites,total,found,not_found,uncertain,exit_code,elapsed_ms,raw_path" > "$CSV"
fi

SITE_CSV="$(IFS=,; echo "${SITES[*]}")"
SHERLOCK_SITE_ARGS=()
for site in "${SITES[@]}"; do
    SHERLOCK_SITE_ARGS+=(--site "$site")
done

echo "run: $RUN_ID"
echo "usernames: ${#USERS[@]} · sites: ${#SITES[@]} ($SITE_CSV)"
echo "results: $CSV"

if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "Adler: $ADLER_BIN --format json --all --only $SITE_CSV <username>"
    echo "Sherlock: $SHERLOCK_BIN ${SHERLOCK_SITE_ARGS[*]} --timeout $TIMEOUT_SECS --json <username>"
    exit 0
fi

for username in "${USERS[@]}"; do
    safe_user="${username//[^A-Za-z0-9_.-]/_}"

    adler_raw="$RAW_DIR/adler-$safe_user.json"
    start="$(now_ms)"
    set +e
    "$ADLER_BIN" --format json --all --only "$SITE_CSV" "$username" >"$adler_raw" 2>"$RAW_DIR/adler-$safe_user.stderr"
    code=$?
    set -e
    elapsed=$(( "$(now_ms)" - start ))
    IFS=, read -r total found not_found uncertain < <(json_counts adler "$adler_raw")
    printf '%s\n' "$RUN_ID,adler,$username,${#SITES[@]},$total,$found,$not_found,$uncertain,$code,$elapsed,$adler_raw" >> "$CSV"
    echo "adler   $username ${elapsed}ms f=$found nf=$not_found u=$uncertain"

    sherlock_work="$RAW_DIR/sherlock-$safe_user"
    mkdir -p "$sherlock_work"
    start="$(now_ms)"
    set +e
    (cd "$sherlock_work" && "$SHERLOCK_BIN" "${SHERLOCK_SITE_ARGS[@]}" --timeout "$TIMEOUT_SECS" --json "$username" >stdout.txt 2>stderr.txt)
    code=$?
    set -e
    elapsed=$(( "$(now_ms)" - start ))
    sherlock_json="$(find "$sherlock_work" -maxdepth 1 -name '*.json' -print -quit)"
    if [[ -z "$sherlock_json" ]]; then
        sherlock_json="$sherlock_work/stdout.txt"
    fi
    IFS=, read -r total found not_found uncertain < <(json_counts sherlock "$sherlock_json")
    printf '%s\n' "$RUN_ID,sherlock,$username,${#SITES[@]},$total,$found,$not_found,$uncertain,$code,$elapsed,$sherlock_json" >> "$CSV"
    echo "sherlock $username ${elapsed}ms f=$found nf=$not_found u=$uncertain"
done

summary_md "$CSV" "$RUN_ID" "$RUN_DIR/summary.md"
echo "summary: $RUN_DIR/summary.md"
