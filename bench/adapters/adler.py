"""Adapter for Adler.

Invokes the `adler` binary with JSON output, parses the response, maps
Adler's site names back to canonical bench IDs, and emits normalized
verdicts. We pass `--no-wmn` so Adler's site set matches the main registry
(`adler-core/data/sites.json`) one-to-one, which is what `ground-truth.tsv`
is derived from.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import time
from pathlib import Path

# canonical_name -> exact Adler site name (`name` field in sites.json)
SITE_MAP: dict[str, str] = {
    "github": "GitHub",
    "gitlab": "GitLab",
    "bitbucket": "BitBucket",
    "hackerone": "HackerOne",
    "patreon": "Patreon",
    "pinterest": "Pinterest",
    "reddit": "Reddit",
    "twitch": "Twitch",
    "twitter": "Twitter",
    "medium": "Medium",
    "behance": "Behance",
    "dribbble": "Dribbble",
    "imgur": "Imgur",
    "producthunt": "ProductHunt",
    "steam": "Steam",
    "soundcloud": "SoundCloud",
    "aboutme": "About.me",
    "keybase": "Keybase",
    "vimeo": "Vimeo",
    "lastfm": "last.fm",
    "tumblr": "tumblr",
    "wattpad": "Wattpad",
    "trello": "Trello",
    "roblox": "Roblox",
    "bandcamp": "Bandcamp",
    "codewars": "Codewars",
    "telegram": "Telegram",
    "tiktok": "TikTok",
    "youtube": "YouTube",
    "spotify": "Spotify",
}

_KIND_MAP = {
    "found": "found",
    "not_found": "notfound",
    "uncertain": "uncertain",
}


def ensure_installed() -> str | None:
    """Return the path to the adler binary or None if missing."""
    path = shutil.which("adler")
    if path:
        return path
    # Fall back to the release binary we may have built locally.
    repo_root = Path(__file__).resolve().parents[2]
    local = repo_root / "target" / "release" / "adler"
    if local.exists():
        return str(local)
    return None


def run(username: str, output_dir: Path, timeout_s: int = 180) -> dict:
    """Run Adler against `username` and return normalized verdicts."""
    binary = ensure_installed()
    if binary is None:
        return {
            "tool": "adler",
            "username": username,
            "error": "adler binary not on PATH and target/release/adler missing",
            "wall_clock_seconds": 0.0,
            "verdicts": {},
        }

    output_dir.mkdir(parents=True, exist_ok=True)
    raw_path = output_dir / f"{username}.raw.json"

    # `--only` is substring matching. We pass the comma-joined Adler names
    # for our canonical 30 sites; substring matches are filtered down to
    # exact matches when we parse the JSON below.
    only_filter = ",".join(SITE_MAP.values())
    cmd = [
        binary,
        "--format", "json",
        "--all",
        "--no-wmn",
        "--no-cache",
        "--only", only_filter,
        username,
    ]

    started = time.monotonic()
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout_s,
            check=False,
            env={**os.environ, "ADLER_LOG": "error"},
        )
    except subprocess.TimeoutExpired:
        return {
            "tool": "adler",
            "username": username,
            "error": f"timeout after {timeout_s}s",
            "wall_clock_seconds": float(timeout_s),
            "verdicts": {},
        }
    elapsed = time.monotonic() - started

    raw_path.write_text(proc.stdout)

    try:
        outcomes = json.loads(proc.stdout)
    except json.JSONDecodeError as e:
        return {
            "tool": "adler",
            "username": username,
            "error": f"JSON parse failed: {e}; stderr={proc.stderr[:400]}",
            "wall_clock_seconds": elapsed,
            "exit_code": proc.returncode,
            "verdicts": {},
        }

    by_adler_name: dict[str, dict] = {}
    for o in outcomes:
        by_adler_name[o.get("site", "")] = o

    verdicts: dict[str, str | None] = {}
    for canonical, adler_name in SITE_MAP.items():
        o = by_adler_name.get(adler_name)
        if o is None:
            verdicts[canonical] = None  # site not in tool's registry / not scanned
            continue
        verdicts[canonical] = _KIND_MAP.get(o.get("kind", ""), "uncertain")

    return {
        "tool": "adler",
        "username": username,
        "exit_code": proc.returncode,
        "wall_clock_seconds": elapsed,
        "verdicts": verdicts,
        "sites_total": len(outcomes),
    }
