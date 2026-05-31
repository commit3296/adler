"""Adapter for Snoop — TODO, see Implementation notes below.

Snoop (https://github.com/snooppr/snoop) is a Russian-origin Python OSINT tool
with the largest registry of any open-source competitor we measure (~5400
sites in the full edition). Its CLI is documented in Russian and the
package name on PyPI differs from the GitHub project name; the install path
is non-trivial — typically the operator runs the upstream installer rather
than `pip install`.

Implementation steps:
  1. Document the operator-side install in bench/README.md ("Optional:
     install Snoop per <upstream link>").
  2. Probe `snoop --help` for current flags. The relevant ones we want:
     - target/positional username
     - JSON or CSV structured output
     - site filtering (if supported; otherwise scan all and filter).
  3. Build SITE_MAP from Snoop's site list.
  4. Replace the `NotImplementedError` below with the real `run()`.

If install proves hostile in a given environment, run.sh should skip Snoop
with a clear "skipped — not installed" line in the bench summary rather than
fail the whole run.
"""

from __future__ import annotations

from pathlib import Path

# canonical_name -> Snoop site key. Verify after install.
SITE_MAP: dict[str, str] = {}


def run(username: str, output_dir: Path, timeout_s: int = 600) -> dict:
    return {
        "tool": "snoop",
        "username": username,
        "error": "snoop adapter not implemented yet — see bench/adapters/snoop.py docstring",
        "wall_clock_seconds": 0.0,
        "verdicts": {c: None for c in SITE_MAP},
    }
