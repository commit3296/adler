#!/usr/bin/env python3
"""Bench orchestrator: run each tool against each username in ground-truth.tsv.

Writes one JSON per `(tool, username)` pair to `bench/results/<tool>/<user>.json`,
re-using cached results unless `--clean` is passed. After every pair completes,
invokes `bench/analyze.py` to regenerate `bench/RESULTS.md`.

Invoked via `bench/run.sh`; see `--help` for flag reference.
"""

from __future__ import annotations

import argparse
import importlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent
RESULTS_DIR = BENCH_DIR / "results"
VENVS_DIR = BENCH_DIR / "venvs"
GROUND_TRUTH = BENCH_DIR / "ground-truth.tsv"
ADAPTERS_DIR = BENCH_DIR / "adapters"

ALL_TOOLS = ("adler", "sherlock", "maigret", "blackbird", "snoop")


def read_ground_truth() -> list[tuple[str, str, str]]:
    rows: list[tuple[str, str, str]] = []
    if not GROUND_TRUTH.exists():
        print(
            f"error: {GROUND_TRUTH.relative_to(BENCH_DIR.parent)} missing; "
            f"run `python3 bench/derive-ground-truth.py` first",
            file=sys.stderr,
        )
        sys.exit(1)
    for line in GROUND_TRUTH.read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 3:
            continue
        rows.append((parts[0], parts[1], parts[2]))
    return rows


def install_tool(tool: str) -> int:
    """Set up the venv + pip install for `tool`. Returns process exit code."""
    if tool == "adler":
        print("adler: built directly via `cargo install` or `target/release/`; no venv needed.")
        return 0
    if tool == "sherlock":
        venv = VENVS_DIR / "sherlock"
        if not (venv / "bin" / "sherlock").exists():
            VENVS_DIR.mkdir(parents=True, exist_ok=True)
            subprocess.run(["python3", "-m", "venv", str(venv)], check=True)
            pip = str(venv / "bin" / "pip")
            # `--resume-retries` because the Sherlock dep tree pulls in
            # ~50 MB of wheels (pandas/numpy/openpyxl) and a transient
            # download stall otherwise aborts the whole install.
            r = subprocess.run(
                [pip, "install", "--resume-retries", "5", "sherlock-project"],
                check=False,
            )
            return r.returncode
        return 0
    if tool in ("maigret", "blackbird", "snoop"):
        print(
            f"{tool}: adapter is a stub — see bench/adapters/{tool}.py for the "
            f"install path. Skipping.",
            file=sys.stderr,
        )
        return 0
    print(f"unknown tool: {tool}", file=sys.stderr)
    return 1


def load_adapter(tool: str):
    sys.path.insert(0, str(BENCH_DIR))
    return importlib.import_module(f"adapters.{tool}")


def run_pair(tool: str, username: str, force: bool) -> dict:
    out_path = RESULTS_DIR / tool / f"{username}.json"
    if out_path.exists() and not force:
        try:
            return json.loads(out_path.read_text())
        except json.JSONDecodeError:
            pass  # fall through and re-run

    adapter = load_adapter(tool)
    result = adapter.run(username, RESULTS_DIR / tool)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(result, indent=2, default=str))
    return result


def parse_csv(arg: str) -> list[str]:
    return [s.strip() for s in arg.split(",") if s.strip()]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tool", help="comma-separated tool subset; default all")
    parser.add_argument("--user", help="comma-separated username subset; default all from ground-truth")
    parser.add_argument("--clean", action="store_true", help="wipe bench/results/ before running")
    parser.add_argument("--install", help="install named tool(s) and exit; e.g. --install sherlock")
    parser.add_argument("--no-analyze", action="store_true", help="skip the analyzer step at end")
    parser.add_argument("--list", action="store_true", help="list ground-truth users + tools and exit")
    parser.add_argument("--force", action="store_true", help="re-run pairs even if results JSON exists")
    args = parser.parse_args()

    if args.install:
        tools = parse_csv(args.install)
        rc = 0
        for t in tools:
            rc = max(rc, install_tool(t))
        return rc

    rows = read_ground_truth()
    all_users = sorted({u for u, _, _ in rows})

    if args.list:
        print(f"Ground-truth: {len(rows)} cases over {len(all_users)} users")
        for u in all_users:
            n_pos = sum(1 for r in rows if r[0] == u and r[2] == "found")
            n_neg = sum(1 for r in rows if r[0] == u and r[2] == "notfound")
            print(f"  {u:30}  found={n_pos:<3}  notfound={n_neg}")
        print(f"\nTools: {', '.join(ALL_TOOLS)}")
        return 0

    if args.clean and RESULTS_DIR.exists():
        shutil.rmtree(RESULTS_DIR)

    tools = parse_csv(args.tool) if args.tool else list(ALL_TOOLS)
    users = parse_csv(args.user) if args.user else all_users

    # Validate.
    for t in tools:
        if t not in ALL_TOOLS:
            print(f"unknown tool: {t}", file=sys.stderr)
            return 1
    unknown_users = [u for u in users if u not in all_users]
    if unknown_users:
        print(f"unknown users: {unknown_users}", file=sys.stderr)
        return 1

    # Ensure each tool has a working install before invoking it.
    for t in tools:
        rc = install_tool(t)
        if rc != 0:
            print(f"install failed for {t}; skipping the tool", file=sys.stderr)
            tools = [x for x in tools if x != t]

    print(f"Running {len(tools)} tools × {len(users)} users = {len(tools) * len(users)} invocations")
    for tool in tools:
        for user in users:
            print(f"  [{tool}] {user} ... ", end="", flush=True)
            result = run_pair(tool, user, force=args.force)
            if result.get("error"):
                print(f"ERROR ({result['error'][:60]})")
            else:
                wall = result.get("wall_clock_seconds", 0.0)
                found = sum(1 for v in result["verdicts"].values() if v == "found")
                notfound = sum(1 for v in result["verdicts"].values() if v == "notfound")
                uncertain = sum(1 for v in result["verdicts"].values() if v == "uncertain")
                print(f"{wall:5.1f}s  found={found:<2} notfound={notfound:<2} uncertain={uncertain}")

    if not args.no_analyze:
        analyze_script = BENCH_DIR / "analyze.py"
        if analyze_script.exists():
            print("\nRunning analyzer ...")
            subprocess.run([sys.executable, str(analyze_script)], check=False)
        else:
            print("\n(analyze.py missing — skipping report)")

    return 0


if __name__ == "__main__":
    sys.exit(main())
