"""Adapter for Blackbird.

Blackbird (https://github.com/p1ngul1n0/blackbird) scans a username
across the WhatsMyName-derived registry. There's no `--site` filter
and no machine-readable not-found output: we run it across the full
registry, parse its JSON (a list of FOUND entries only), and derive
not-found verdicts by intersecting with the WMN registry it ships in
`data/wmn-data.json`.

Install path: not PyPI (the `blackbird-osint` / `blackbird-pw` /
plain `blackbird` packages on PyPI are either dead or build-broken).
Upstream's documented install is `git clone` + `pip install -r
requirements.txt`, which is what `_orchestrator.py::install_tool`
does on first run. The pinned requirements file ships with versions
that don't have prebuilt wheels for current Python (3.14 here), so
we install the packages *without* the file's pins and let pip pick
versions that build — most of the deps are stable utilities (aiohttp,
rich, pillow) whose APIs blackbird leans on at a coarse level.

Verdicts: blackbird has no Uncertain concept. Sites it probes but
doesn't find resolve to `notfound`; sites it doesn't know about
resolve to `None` (missing — the same convention every adapter uses).
"""

from __future__ import annotations

import json
import shutil
import subprocess
import time
from pathlib import Path

# canonical_name -> Blackbird/WhatsMyName site name. Names taken from the
# bundled `data/wmn-data.json` (732 sites). `twitter` and `behance` have no
# entry in blackbird's registry — they fall through to `missing` for fair
# bench attribution rather than fake recall.
SITE_MAP: dict[str, str | None] = {
    "github": "GitHub (User)",
    "gitlab": "GitLab",
    "bitbucket": "Bitbucket",
    "hackerone": "HackerOne",
    "patreon": "Patreon",
    "pinterest": "Pinterest",
    "reddit": "Reddit",
    "twitch": "Twitch",
    "twitter": None,
    "medium": "Medium",
    "behance": None,
    "dribbble": "Dribbble",
    "imgur": "Imgur",
    "producthunt": "Producthunt",
    "steam": "Steam",
    "soundcloud": "SoundCloud",
    "aboutme": "about.me",
    "keybase": "Keybase",
    "vimeo": "Vimeo",
    "lastfm": "Last.fm",
    "tumblr": "tumblr",
    "wattpad": "Wattpad",
    "trello": "Trello",
    "roblox": "Roblox",
    "bandcamp": "Bandcamp",
    "codewars": "Codewars",
    "telegram": "Telegram",
    "tiktok": "TikTok",
    "youtube": "YouTube User",
    "spotify": "Spotify",
}


def _venv_paths() -> tuple[Path | None, Path | None]:
    """Return (python, blackbird.py) inside the bench venv, or (None, None)."""
    repo_root = Path(__file__).resolve().parents[2]
    venv = repo_root / "bench" / "venvs" / "blackbird"
    py = venv / "bin" / "python"
    script = venv / "src" / "blackbird.py"
    if py.exists() and script.exists():
        return py, script
    # Fall back to a system install.
    sys_path = shutil.which("blackbird")
    if sys_path:
        return Path(sys_path), Path("")
    return None, None


def _load_registry_names(blackbird_src: Path) -> set[str]:
    """Names of every site in the bundled WMN registry — what blackbird probed."""
    wmn = blackbird_src.parent / "data" / "wmn-data.json"
    try:
        d = json.loads(wmn.read_text())
    except (OSError, json.JSONDecodeError):
        return set()
    return {s["name"] for s in d.get("sites", []) if isinstance(s, dict) and "name" in s}


def run(username: str, output_dir: Path, timeout_s: int = 600) -> dict:
    """Run Blackbird against `username` and return normalized verdicts."""
    py, script = _venv_paths()
    if py is None or script is None:
        return {
            "tool": "blackbird",
            "username": username,
            "error": "blackbird not installed (run `bench/run.sh --install blackbird`)",
            "wall_clock_seconds": 0.0,
            "verdicts": {},
        }

    output_dir.mkdir(parents=True, exist_ok=True)
    # Blackbird writes results into `<cwd>/results/<user>_<date>_blackbird/`.
    # We run with cwd = blackbird's src dir (it expects `./data/wmn-data.json`
    # relative to cwd) and read the most recent results subfolder afterwards.
    src_dir = script.parent

    cmd = [
        str(py),
        str(script),
        "-u", username,
        "--json",
        "--no-update",
        "--no-nsfw",
    ]

    started = time.monotonic()
    try:
        proc = subprocess.run(
            cmd,
            cwd=str(src_dir),
            capture_output=True,
            text=True,
            timeout=timeout_s,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return {
            "tool": "blackbird",
            "username": username,
            "error": f"timeout after {timeout_s}s",
            "wall_clock_seconds": float(timeout_s),
            "verdicts": {},
        }
    elapsed = time.monotonic() - started

    # Find the JSON blackbird wrote: results/<user>_<date>_blackbird/*.json
    results_root = src_dir / "results"
    found_names: set[str] = set()
    if results_root.exists():
        latest = max(
            (p for p in results_root.glob(f"{username}_*_blackbird") if p.is_dir()),
            default=None,
            key=lambda p: p.stat().st_mtime,
        )
        if latest is not None:
            for j in latest.glob("*.json"):
                try:
                    entries = json.loads(j.read_text())
                except (OSError, json.JSONDecodeError):
                    continue
                if not isinstance(entries, list):
                    continue
                for e in entries:
                    if isinstance(e, dict) and e.get("status") == "FOUND":
                        found_names.add(e.get("name", ""))
                break  # one JSON per run

    registry = _load_registry_names(script)

    verdicts: dict[str, str | None] = {}
    for canonical, bb_name in SITE_MAP.items():
        if bb_name is None or bb_name not in registry:
            verdicts[canonical] = None  # site not in tool's registry
            continue
        verdicts[canonical] = "found" if bb_name in found_names else "notfound"

    return {
        "tool": "blackbird",
        "username": username,
        "exit_code": proc.returncode,
        "wall_clock_seconds": elapsed,
        "verdicts": verdicts,
        "sites_total": len(found_names),
    }
