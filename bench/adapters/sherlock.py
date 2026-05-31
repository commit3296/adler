"""Adapter for Sherlock.

Invokes `sherlock` from its dedicated venv, filtered to our canonical
sites via `--site`, parses the per-username output file (one URL per
found site, plus the `--print-all` line discipline for not-found),
and emits normalized verdicts.

Sherlock has no `Uncertain` concept — every site lands as found or
notfound. Errors (timeout, connection refused) are folded into
`notfound` because Sherlock itself reports them that way.
"""

from __future__ import annotations

import re
import shutil
import subprocess
import time
from pathlib import Path

# canonical_name -> Sherlock site name (used with `--site`)
# Sherlock's site registry is `sherlock_project/resources/data.json`; names
# match the dictionary keys there. We use the case Sherlock expects.
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
    "steam": "Steam Community (User)",
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

# ANSI colour escape (Sherlock paints output green/red).
_ANSI = re.compile(r"\x1b\[[0-9;]*m")

# `[+] SiteName: https://…` or `[-] SiteName: Not Found!`
_LINE = re.compile(r"^\[([+\-])]\s+(.+?):\s+(.*)$")


def _venv_binary() -> str | None:
    """Return the path to the Sherlock binary in the bench venv, or None."""
    repo_root = Path(__file__).resolve().parents[2]
    candidate = repo_root / "bench" / "venvs" / "sherlock" / "bin" / "sherlock"
    if candidate.exists():
        return str(candidate)
    return shutil.which("sherlock")


def run(username: str, output_dir: Path, timeout_s: int = 300) -> dict:
    """Run Sherlock against `username` and return normalized verdicts."""
    binary = _venv_binary()
    if binary is None:
        return {
            "tool": "sherlock",
            "username": username,
            "error": "sherlock not installed (run `bench/run.sh --install sherlock`)",
            "wall_clock_seconds": 0.0,
            "verdicts": {},
        }

    output_dir.mkdir(parents=True, exist_ok=True)
    output_file = output_dir / f"{username}.sherlock.txt"
    if output_file.exists():
        output_file.unlink()

    # `--site` is repeatable; pass each Sherlock site name once.
    site_args: list[str] = []
    for site_name in SITE_MAP.values():
        site_args.extend(["--site", site_name])

    cmd = [
        binary,
        "--print-all",
        "--no-color",
        "--output", str(output_file),
        *site_args,
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
        )
    except subprocess.TimeoutExpired:
        return {
            "tool": "sherlock",
            "username": username,
            "error": f"timeout after {timeout_s}s",
            "wall_clock_seconds": float(timeout_s),
            "verdicts": {},
        }
    elapsed = time.monotonic() - started

    # Stdout carries the live per-site lines; the --output file has just the
    # found URLs (one per line). The live lines are what we want.
    sherlock_verdicts: dict[str, str] = {}
    for raw_line in proc.stdout.splitlines():
        line = _ANSI.sub("", raw_line).strip()
        m = _LINE.match(line)
        if not m:
            continue
        kind, name, _rest = m.groups()
        if kind == "+":
            sherlock_verdicts[name.strip()] = "found"
        elif kind == "-":
            sherlock_verdicts[name.strip()] = "notfound"

    # Map back to canonical names.
    verdicts: dict[str, str | None] = {}
    for canonical, sherlock_name in SITE_MAP.items():
        verdicts[canonical] = sherlock_verdicts.get(sherlock_name)

    return {
        "tool": "sherlock",
        "username": username,
        "exit_code": proc.returncode,
        "wall_clock_seconds": elapsed,
        "verdicts": verdicts,
        "sites_total": len(sherlock_verdicts),
    }
