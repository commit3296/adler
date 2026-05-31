#!/usr/bin/env python3
"""Compare per-tool results against ground-truth.tsv and write RESULTS.md.

Metrics (per tool, across all (user, site) labeled cases the tool produced a
verdict for):

  - TP  — ground truth "found",    tool said "found"
  - FN  — ground truth "found",    tool said "notfound"
  - FP  — ground truth "notfound", tool said "found"
  - TN  — ground truth "notfound", tool said "notfound"
  - Unc — tool said "uncertain" (Adler only); not counted in TP/FN/FP/TN
  - Miss — tool returned None (didn't know the site / wasn't run); not counted

  - Recall    = TP / (TP + FN)         "of known-positive cases, how many did
                                        the tool confirm?"
  - Precision = TP / (TP + FP)         "when the tool said Found, was it right?"

Adler-specific note: Uncertain cells aren't penalised as wrong — Adler
explicitly says "I don't know" rather than guessing. The Unc column makes
that abstention visible alongside Recall / Precision.
"""

from __future__ import annotations

import json
import statistics
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent
GROUND_TRUTH = BENCH_DIR / "ground-truth.tsv"
RESULTS_DIR = BENCH_DIR / "results"
REPORT_PATH = BENCH_DIR / "RESULTS.md"


def read_ground_truth() -> dict[tuple[str, str], str]:
    """{(username, canonical_site): expected_verdict}"""
    out: dict[tuple[str, str], str] = {}
    for line in GROUND_TRUTH.read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 3:
            continue
        user, site, expected = parts
        out[(user, site)] = expected
    return out


def gather_tool_results(tool: str) -> dict[str, dict]:
    """{username: result dict} for one tool, from bench/results/<tool>/."""
    tool_dir = RESULTS_DIR / tool
    if not tool_dir.exists():
        return {}
    out: dict[str, dict] = {}
    for path in sorted(tool_dir.glob("*.json")):
        try:
            data = json.loads(path.read_text())
        except (json.JSONDecodeError, OSError):
            continue
        # adapter saves both `<user>.json` and `<user>.raw.json`; ignore the
        # latter as they're the per-tool raw payloads, not normalized.
        if path.name.endswith(".raw.json"):
            continue
        # Either name should resolve to the same `username` field anyway.
        username = data.get("username")
        if username:
            out[username] = data
    return out


def score(truth: dict[tuple[str, str], str], results: dict[str, dict]) -> dict:
    tp = fn = fp = tn = unc = miss = 0
    wall_times: list[float] = []
    errored = 0

    for (user, site), expected in truth.items():
        if user not in results:
            miss += 1
            continue
        data = results[user]
        if data.get("error"):
            errored += 1
            miss += 1
            continue
        verdict = data.get("verdicts", {}).get(site)
        if verdict is None:
            miss += 1
            continue
        if verdict == "uncertain":
            unc += 1
            continue
        if expected == "found" and verdict == "found":
            tp += 1
        elif expected == "found" and verdict == "notfound":
            fn += 1
        elif expected == "notfound" and verdict == "found":
            fp += 1
        elif expected == "notfound" and verdict == "notfound":
            tn += 1

    for data in results.values():
        if not data.get("error") and data.get("wall_clock_seconds") is not None:
            wall_times.append(float(data["wall_clock_seconds"]))

    recall = tp / (tp + fn) if (tp + fn) > 0 else None
    precision = tp / (tp + fp) if (tp + fp) > 0 else None
    mean_wall = statistics.mean(wall_times) if wall_times else None

    return {
        "tp": tp, "fn": fn, "fp": fp, "tn": tn,
        "uncertain": unc, "missing": miss, "errored": errored,
        "recall": recall, "precision": precision,
        "mean_wall_seconds": mean_wall,
        "users_scored": len(wall_times),
    }


def fmt_pct(x: float | None) -> str:
    if x is None:
        return "—"
    return f"{x * 100:5.1f}%"


def fmt_seconds(x: float | None) -> str:
    if x is None:
        return "—"
    return f"{x:5.1f}s"


def main() -> int:
    truth = read_ground_truth()
    tools = ("adler", "sherlock", "maigret", "blackbird", "snoop")

    table_rows: list[str] = []
    table_rows.append(
        "| Tool | n | TP | FN | FP | TN | Uncertain | Missing | Recall | Precision | Mean wall |"
    )
    table_rows.append(
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    )

    have_data: list[str] = []
    skipped: list[str] = []
    for tool in tools:
        results = gather_tool_results(tool)
        if not results:
            skipped.append(tool)
            continue
        have_data.append(tool)
        s = score(truth, results)
        n = s["tp"] + s["fn"] + s["fp"] + s["tn"]
        table_rows.append(
            f"| **{tool}** | {n} | {s['tp']} | {s['fn']} | {s['fp']} | {s['tn']} | "
            f"{s['uncertain']} | {s['missing']} | "
            f"{fmt_pct(s['recall'])} | {fmt_pct(s['precision'])} | "
            f"{fmt_seconds(s['mean_wall_seconds'])} |"
        )

    if not have_data:
        REPORT_PATH.write_text(
            "# bench results\n\nNo tool results found — run `bench/run.sh` first.\n"
        )
        print(f"wrote (empty) {REPORT_PATH}", flush=True)
        return 0

    body: list[str] = []
    body.append("# bench results")
    body.append("")
    body.append(
        "Per-tool comparison against the `ground-truth.tsv` dataset, regenerated by "
        "`bench/analyze.py`. Re-run with `bench/run.sh` to refresh."
    )
    body.append("")
    body.append("## Scoreboard")
    body.append("")
    body.extend(table_rows)
    body.append("")
    body.append("Notation:")
    body.append("")
    body.append(
        "- **n** — number of `(user, site)` cases the tool produced a confident "
        "verdict on (TP + FN + FP + TN); `Uncertain` and `Missing` are excluded "
        "from n so Recall / Precision aren't penalised for honest abstention "
        "(Adler) or registry gaps."
    )
    body.append(
        "- **Uncertain** — tool reported `Uncertain(reason)` rather than a "
        "binary verdict. Only Adler has this; the others always commit."
    )
    body.append(
        "- **Missing** — tool didn't return a verdict for this `(user, site)` "
        "pair, usually because the site isn't in the tool's registry."
    )
    body.append(
        "- **Mean wall** — average wall-clock seconds per "
        "`(tool, user)` invocation across all users the tool was run on."
    )
    if skipped:
        body.append("")
        body.append(
            f"_Tools with no recorded results (skipped): {', '.join(skipped)}._"
        )
    body.append("")
    body.append("## Reading the numbers")
    body.append("")
    body.append(
        "- A tool that **never says Uncertain** but has low precision is "
        "guessing — its `Found` answers are unreliable."
    )
    body.append(
        "- A tool with **high Uncertain rate but high precision** is being "
        "honest about the limits of its visibility — its `Found` answers are "
        "trustworthy and its `Uncertain` cells are areas the operator can fix "
        "with better access (residential IP, sessions, browser backend)."
    )
    body.append(
        "- A tool with **high Missing rate** has a narrower registry; that's "
        "not a recall failure on the sites it does cover."
    )
    body.append("")
    body.append("## Reproduce")
    body.append("")
    body.append("```bash")
    body.append("cd bench/")
    body.append("./run.sh                  # run all tools × all users")
    body.append("./run.sh --tool adler     # one tool only")
    body.append("./run.sh --user blue      # one user only")
    body.append("./run.sh --clean          # discard cached results")
    body.append("```")
    body.append("")
    body.append(
        "See `bench/README.md` for the methodology, ground-truth derivation, "
        "and honest limits."
    )

    REPORT_PATH.write_text("\n".join(body) + "\n")
    print(f"wrote {REPORT_PATH.relative_to(BENCH_DIR.parent)} ({len(have_data)} tool(s) scored)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
