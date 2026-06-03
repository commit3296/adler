#!/usr/bin/env python3
"""Aggregate nightly `adler --doctor` runs and open a PR for sites that
keep failing.

Pipeline
--------

  1. Parse one or more doctor reports (`report.txt` produced by
     `.github/workflows/doctor.yml`), extracting *structural* failure
     site names (known_present reported NotFound, or absent user
     reported Found — "signal too permissive"). Transient failures
     (Uncertain / network blip) are ignored — they're noisy by
     design.
  2. Read prior per-site failure counts from `doctor-state.json`.
     Increment the count for every site that failed today; reset the
     count to zero for every site we know about that DIDN'T fail
     today. Sites we've never seen before are added with count 1.
  3. Threshold: any site whose count reaches `--threshold` (default
     3) is a candidate to mark `disabled: true` in `sites.json` with
     a one-line reason.
  4. Emit a patched `sites.json` (`--patch-sites`), an updated
     `doctor-state.json` (`--out-state`), and a markdown PR body
     summarising what we propose (`--pr-body`).

Outputs
-------

The script does **not** open a PR itself — that's the workflow's job.
It only produces the files the workflow needs. The exit code is
`0` whether or not there are candidates; the workflow checks the
candidates count via `--gh-output candidates=N` to decide whether to
open a PR.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

# A "[FAIL] <site>" header line in the doctor output, followed by one
# or more "       · <issue>" continuation lines (4 leading spaces + ·).
RE_FAIL = re.compile(r"^\[FAIL\]\s+(.+?)\s*$")
RE_ISSUE = re.compile(r"^\s+·\s+(.+?)\s*$")
RE_OK = re.compile(r"^\[OK\]\s+")

# Substrings that classify a per-site issue as *structural* rather
# than transient. Mirrors the regex doctor.yml uses to gate the build
# fail.
STRUCTURAL_HINTS = ("reported NotFound", "too permissive")


def parse_structural_failures(report_path: Path) -> set[str]:
    """Return the set of site names that hit at least one structural
    failure in `report_path`."""
    failures: set[str] = set()
    current_site: str | None = None
    for raw in report_path.read_text(encoding="utf-8", errors="replace").splitlines():
        if (m := RE_FAIL.match(raw)) is not None:
            current_site = m.group(1)
            continue
        if RE_OK.match(raw) is not None:
            current_site = None
            continue
        if current_site is None:
            continue
        if (m := RE_ISSUE.match(raw)) is not None:
            issue = m.group(1)
            if any(h in issue for h in STRUCTURAL_HINTS):
                failures.add(current_site)
                # one issue is enough — keep iterating in case more
                # `[FAIL]` blocks follow, but no need to re-add.
    return failures


def parse_all_sites(report_path: Path) -> set[str]:
    """Set of every site mentioned in the doctor output (OK + FAIL).

    Used to decide which sites to *reset* in state: a site that
    appeared in the report and didn't fail had a clean run and its
    consecutive counter should drop back to zero.
    """
    seen: set[str] = set()
    for raw in report_path.read_text(encoding="utf-8", errors="replace").splitlines():
        if (m := RE_FAIL.match(raw)) is not None:
            seen.add(m.group(1))
            continue
        if (m := RE_OK.match(raw)) is not None:
            # `[OK]   <site name>` — strip prefix.
            seen.add(raw[len("[OK]") :].strip())
    return seen


def load_state(path: Path) -> dict:
    if not path.exists():
        return {"sites": {}, "version": 1}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {"sites": {}, "version": 1}
    if not isinstance(data.get("sites"), dict):
        return {"sites": {}, "version": 1}
    return data


def apply_sites_patch(
    sites_json_path: Path,
    flagged: list[str],
    reason: str,
) -> int:
    """Patch entries named in `flagged` to `disabled: true` with a
    short top-level reason comment field (`disabled_reason`). Returns
    the number of entries actually changed.
    """
    content = sites_json_path.read_text(encoding="utf-8")
    root = json.loads(content)
    arr = root.get("sites")
    if not isinstance(arr, list):
        raise SystemExit(
            f"{sites_json_path} has no top-level 'sites' array — is it a registry file?"
        )
    flagged_set = set(flagged)
    patched = 0
    for entry in arr:
        if not isinstance(entry, dict):
            continue
        name = entry.get("name")
        if name in flagged_set and not entry.get("disabled"):
            entry["disabled"] = True
            entry["disabled_reason"] = reason
            patched += 1
    if patched > 0:
        out = json.dumps(root, indent=2, ensure_ascii=False)
        sites_json_path.write_text(out + "\n", encoding="utf-8")
    return patched


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--report",
        action="append",
        default=[],
        type=Path,
        help="Path to a doctor report.txt (repeatable for matrix runs).",
    )
    parser.add_argument(
        "--state",
        type=Path,
        required=True,
        help="Path to read prior doctor-state.json (may not exist on first run).",
    )
    parser.add_argument(
        "--out-state",
        type=Path,
        required=True,
        help="Path to write the updated doctor-state.json.",
    )
    parser.add_argument(
        "--sites-json",
        type=Path,
        required=True,
        help="Path to sites.json that gets disabled: true patches.",
    )
    parser.add_argument(
        "--pr-body",
        type=Path,
        required=True,
        help="Path to write the proposed PR body in markdown.",
    )
    parser.add_argument(
        "--threshold",
        type=int,
        default=3,
        help="Consecutive nightly failures before a site is flagged.",
    )
    parser.add_argument(
        "--gh-output",
        type=Path,
        default=None,
        help="GitHub Actions GITHUB_OUTPUT path — write `candidates=N` / `patched=N`.",
    )
    args = parser.parse_args()

    if not args.report:
        print("no --report given; nothing to aggregate", file=sys.stderr)
        return 0

    structural_today: set[str] = set()
    seen_today: set[str] = set()
    for r in args.report:
        if not r.exists():
            print(f"warning: report {r} missing — skipping", file=sys.stderr)
            continue
        structural_today.update(parse_structural_failures(r))
        seen_today.update(parse_all_sites(r))

    state = load_state(args.state)
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")

    # Reset counters for sites that ran today and didn't structurally
    # fail. Sites that didn't run today (e.g. doctor didn't reach
    # them) keep their previous counter — we shouldn't reward
    # "happened to be missed" as recovery.
    recovered: list[str] = []
    for name in list(state["sites"].keys()):
        if name in seen_today and name not in structural_today:
            if state["sites"][name].get("consecutive", 0) > 0:
                recovered.append(name)
            state["sites"][name]["consecutive"] = 0
            state["sites"][name]["last_clean"] = today

    # Bump counters for today's structural failures.
    for name in sorted(structural_today):
        entry = state["sites"].setdefault(
            name,
            {"consecutive": 0, "first_seen": today, "last_clean": None},
        )
        entry["consecutive"] = int(entry.get("consecutive", 0)) + 1
        entry["last_failed"] = today

    # Identify candidates that crossed the threshold.
    candidates = sorted(
        name
        for name, e in state["sites"].items()
        if int(e.get("consecutive", 0)) >= args.threshold
    )

    args.out_state.parent.mkdir(parents=True, exist_ok=True)
    args.out_state.write_text(
        json.dumps(state, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )

    patched = 0
    if candidates:
        patched = apply_sites_patch(
            args.sites_json,
            candidates,
            f"doctor: {args.threshold}+ consecutive structural failures",
        )

    args.pr_body.parent.mkdir(parents=True, exist_ok=True)
    args.pr_body.write_text(render_pr_body(candidates, recovered, state, args.threshold))

    if args.gh_output is not None:
        with args.gh_output.open("a", encoding="utf-8") as f:
            f.write(f"candidates={len(candidates)}\n")
            f.write(f"patched={patched}\n")

    print(
        f"structural today: {len(structural_today)}; "
        f"candidates over threshold: {len(candidates)}; "
        f"patched: {patched}; recovered (counter reset): {len(recovered)}"
    )
    return 0


def render_pr_body(
    candidates: list[str],
    recovered: list[str],
    state: dict,
    threshold: int,
) -> str:
    lines: list[str] = []
    lines.append(
        f"Automated proposal from the nightly registry doctor. "
        f"Each site below has structurally failed `adler --doctor` for "
        f"**{threshold} or more consecutive nights** — known-present user "
        f"reported NotFound, or absent user reported Found (signal too "
        f"permissive). Both shapes mean the site's detection rule no "
        f"longer matches reality and the verdict can't be trusted."
    )
    lines.append("")
    lines.append(
        "Setting `disabled: true` parks the site without deleting its "
        "entry — easy to revive once someone authors a working signal."
    )
    lines.append("")
    lines.append("## Proposed `disabled: true`")
    lines.append("")
    if not candidates:
        lines.append("_None — no site crossed the threshold this run._")
    else:
        lines.append("| Site | Consecutive failures | First seen | Last failed |")
        lines.append("| --- | ---: | --- | --- |")
        for name in candidates:
            e = state["sites"].get(name, {})
            lines.append(
                f"| `{name}` | {e.get('consecutive', '?')} | "
                f"{e.get('first_seen', '?')} | {e.get('last_failed', '?')} |"
            )
    lines.append("")
    if recovered:
        lines.append("## Recovered (counter reset to 0)")
        lines.append("")
        lines.append(
            "These sites previously had a consecutive-failure streak; "
            "tonight's run was clean and their counter has been reset."
        )
        lines.append("")
        for name in sorted(recovered):
            lines.append(f"- `{name}`")
        lines.append("")
    lines.append("---")
    lines.append("")
    lines.append(
        "If a flagged site looks like it should stay enabled — typically because "
        "the structural failure is actually a `known_present` user that just got "
        "deleted upstream — close this PR and run "
        "`adler --doctor --suggest-known-present --only <site>` locally to "
        "discover a fresh candidate user, then paste the resulting `OVERRIDES` "
        "entry."
    )
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    sys.exit(main())
