"""Adapter for Maigret — TODO, see Implementation notes below.

Maigret has a much larger registry (~3000 sites) and rich output (HTML / PDF /
XMind / JSON). For our bench we want the JSON output:

    maigret <username> --json simple --no-color --no-progressbar \\
        --site GitHub --site GitLab ... --folderoutput <results-dir>

The simple JSON form maps each site name to a dict with `status` (Status.CLAIMED
/ Status.AVAILABLE / Status.UNKNOWN / Status.ILLEGAL) and `url_user`. Map:

    Status.CLAIMED  -> found
    Status.AVAILABLE -> notfound
    Status.UNKNOWN   -> uncertain
    Status.ILLEGAL   -> uncertain   (username rejected by site)

Implementation steps:
  1. `bench/run.sh --install maigret` creates venvs/maigret and `pip install maigret`.
  2. Probe `bench/venvs/maigret/bin/maigret --help` for the exact flag spellings
     in the installed version (Maigret renames flags between releases).
  3. Build SITE_MAP — Maigret's site names differ from Sherlock's; the
     canonical-to-Maigret table lives below and must be checked against the
     installed registry (`venvs/maigret/lib/.../resources/data.json`).
  4. Parse `<folderoutput>/<username>.json` and normalize verdicts as above.
  5. Replace the `NotImplementedError` below with the real `run()`.
"""

from __future__ import annotations

from pathlib import Path

# canonical_name -> Maigret site name. Verify after install.
SITE_MAP: dict[str, str] = {}


def run(username: str, output_dir: Path, timeout_s: int = 600) -> dict:
    return {
        "tool": "maigret",
        "username": username,
        "error": "maigret adapter not implemented yet — see bench/adapters/maigret.py docstring",
        "wall_clock_seconds": 0.0,
        "verdicts": {c: None for c in SITE_MAP},
    }
