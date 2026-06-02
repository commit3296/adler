"""Adapter for Maigret.

Invokes `maigret` from its dedicated venv, scoped to the canonical
site list via repeated `--site`, parses **stdout** for verdicts, and
emits normalized verdicts.

Why stdout: Maigret's `-J simple`/`ndjson` reports only include hits
— `Not found` sites are absent from the JSON entirely. To compute
real recall we need to distinguish "site probed, returned NotFound"
from "site wasn't probed at all", so we parse the live console lines
(`[+] Site: URL` / `[-] Site: Not found!`). Same approach Sherlock's
adapter uses.

Maigret has no `Uncertain` concept; we map any non-`[+]` line for a
known site to `notfound`. The `[?]` (error) cases are folded into
`notfound` too, matching Maigret's own behaviour of reporting
errors as "could not confirm" — but that's a known limitation of
binary-verdict tools that Adler's `Uncertain` model exists to fix.
"""

from __future__ import annotations

import re
import shutil
import subprocess
import time
from pathlib import Path

# canonical_name -> Maigret site name (exact key in ~/.maigret/data.json).
# All 30 canonical sites have one-to-one matches in Maigret's registry —
# Maigret inherited / extended the Sherlock taxonomy, so the names line up.
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
    "tumblr": "Tumblr",
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

# ANSI colour escape (Maigret paints lines green/red/yellow).
_ANSI = re.compile(r"\x1b\[[0-9;]*m")

# `[+] Site name: https://…` (found)  /  `[-] Site name: Not found!`
# Maigret sometimes prints recursive finds as `[+] ChildSite [ParentSite]:` —
# we strip the ` [Parent]` suffix to recover the canonical Maigret name.
_LINE = re.compile(r"^\[([+\-?])]\s+([^:]+?):\s+(.*)$")
_RECURSIVE_SUFFIX = re.compile(r"\s+\[[^\]]+\]\s*$")


def _venv_binary() -> str | None:
    """Return the Maigret binary in the bench venv, or fall back to PATH."""
    repo_root = Path(__file__).resolve().parents[2]
    candidate = repo_root / "bench" / "venvs" / "maigret" / "bin" / "maigret"
    if candidate.exists():
        return str(candidate)
    return shutil.which("maigret")


def run(username: str, output_dir: Path, timeout_s: int = 600) -> dict:
    """Run Maigret against `username` and return normalized verdicts."""
    binary = _venv_binary()
    if binary is None:
        return {
            "tool": "maigret",
            "username": username,
            "error": "maigret not installed (run `bench/run.sh --install maigret`)",
            "wall_clock_seconds": 0.0,
            "verdicts": {},
        }

    output_dir.mkdir(parents=True, exist_ok=True)
    raw_path = output_dir / f"{username}.maigret.txt"

    site_args: list[str] = []
    for site_name in SITE_MAP.values():
        site_args.extend(["--site", site_name])

    cmd = [
        binary,
        "--no-color",
        "--no-progressbar",
        # Skip recursive extraction — the bench tests binary recall, not
        # Maigret's profile-enrichment graph.
        "--no-recursion",
        "--no-extracting",
        # Print not-found lines so the parser sees `[-] Site: Not found!`.
        "--print-not-found",
        # Don't reach out to update the registry mid-run (each restart of
        # the orchestrator already paid the install cost; CI parity).
        "--no-autoupdate",
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
            "tool": "maigret",
            "username": username,
            "error": f"timeout after {timeout_s}s",
            "wall_clock_seconds": float(timeout_s),
            "verdicts": {},
        }
    elapsed = time.monotonic() - started
    raw_path.write_text(proc.stdout)

    maigret_verdicts: dict[str, str] = {}
    for raw_line in proc.stdout.splitlines():
        line = _ANSI.sub("", raw_line).strip()
        m = _LINE.match(line)
        if not m:
            continue
        kind, name, _rest = m.groups()
        # `ChildSite [ParentSite]` → `ChildSite`.
        name = _RECURSIVE_SUFFIX.sub("", name).strip()
        if kind == "+":
            maigret_verdicts[name] = "found"
        elif kind in ("-", "?"):
            # Maigret bundles network / captcha errors with not-found.
            # Adler's `Uncertain` model is what addresses this gap.
            maigret_verdicts.setdefault(name, "notfound")

    verdicts: dict[str, str | None] = {}
    for canonical, maigret_name in SITE_MAP.items():
        verdicts[canonical] = maigret_verdicts.get(maigret_name)

    return {
        "tool": "maigret",
        "username": username,
        "exit_code": proc.returncode,
        "wall_clock_seconds": elapsed,
        "verdicts": verdicts,
        "sites_total": len(maigret_verdicts),
    }
