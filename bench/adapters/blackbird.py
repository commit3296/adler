"""Adapter for Blackbird — TODO, see Implementation notes below.

Blackbird (https://github.com/p1ngul1n0/blackbird) is Python-based and ships a
CLI that scans WhatsMyName-derived sites for a username. It does not document
`--site`-style filtering, so we will:

  1. Run Blackbird once across all sites it knows about
     (`blackbird --username <user> --json --no-update`).
  2. Parse the resulting JSON for our canonical site IDs only.
  3. Map Blackbird's site names to canonical names via SITE_MAP.

Blackbird's verdict model is binary (found / not-found); there's no
Uncertain. Errors fold into notfound the same way Sherlock's do.

Implementation steps:
  1. `bench/run.sh --install blackbird` creates venvs/blackbird and
     `pip install blackbird-osint` (or the project's current package name —
     verify on first run).
  2. Probe `bench/venvs/blackbird/bin/blackbird --help` for the exact
     output-format flag (the project has changed `--json` <-> `--output`
     semantics across releases).
  3. Build SITE_MAP from Blackbird's site list (WhatsMyName-based, so site
     keys often match canonical names directly).
  4. Replace the `NotImplementedError` below with the real `run()`.
"""

from __future__ import annotations

from pathlib import Path

# canonical_name -> Blackbird site key (often WhatsMyName slug). Verify after install.
SITE_MAP: dict[str, str] = {}


def run(username: str, output_dir: Path, timeout_s: int = 600) -> dict:
    return {
        "tool": "blackbird",
        "username": username,
        "error": "blackbird adapter not implemented yet — see bench/adapters/blackbird.py docstring",
        "wall_clock_seconds": 0.0,
        "verdicts": {c: None for c in SITE_MAP},
    }
