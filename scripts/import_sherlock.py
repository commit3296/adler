#!/usr/bin/env python3
"""Convert the Sherlock project's data.json into Adler's site registry.

Source data is MIT-licensed (sherlock-project/sherlock). This script only
transforms the schema; provenance and attribution live in
adler-core/data/sites.json's header comment and the project README.

Usage:
    # fetch the upstream registry, then convert it
    curl -fsSL \\
      https://raw.githubusercontent.com/sherlock-project/sherlock/master/sherlock_project/resources/data.json \\
      -o /tmp/sherlock.json
    python3 scripts/import_sherlock.py /tmp/sherlock.json adler-core/data/sites.json

Schema mapping:
    status_code  -> [StatusFound[200], StatusNotFound[errorCode or 404]]
    message      -> [StatusFound[200], BodyAbsent[errorMsg] for each msg]
    response_url -> SKIPPED (exact-URL-match semantics that Adler's
                    substring RedirectAbsent can't model without false
                    positives on homepage redirects)
    POST sites   -> SKIPPED (Adler only issues GET)
    urlProbe     -> used as the probe URL when present (the endpoint that
                    actually differentiates accounts)
    username_claimed -> known_present (used by `adler --doctor`)

Detections are imported unverified: Sherlock's signatures rot over time.
Run `adler --doctor` to find sites whose detection no longer holds.
"""

import json
import sys

# Sites whose Sherlock detection is "too permissive": they return 200 (or
# lack the error marker) for *any* username, so the imported signature
# reports Found for everyone — a false positive. Flagged by a full
# `adler --doctor` run on 2026-05-20 (random nonsense user reported Found).
# Excluded until someone contributes a working signature (e.g. a body marker
# instead of bare status). Keyed case-insensitively.
KNOWN_BROKEN = {
    name.lower()
    for name in (
        "Apple Discussions", "Archive.org", "authorSTREAM", "BoardGameGeek",
        "Chess", "Clozemaster", "Codolio", "CSSBattle", "DailyMotion",
        "Flightradar24", "GeeksforGeeks", "Hashnode", "Hubski", "igromania",
        "interpals", "Kaggle", "Kvinneguiden", "mercadolivre", "Needrom",
        "opennet", "Rarible", "RocketTube", "RoyalCams", "Scribd", "Shelf",
        "SlideShare", "Splice", "Spotify", "svidbook", "threads", "Trovo",
        "TryHackMe", "Velomania", "Weblate",
        # Too restrictive: marker is site-wide chrome, not a not-found
        # signal, so the site reports NotFound for everyone (verified the
        # marker appears on the homepage). 2026-05-20.
        "All Things Worn",
    )
}

# Per-site corrections applied after conversion. Sherlock's data is the
# source of truth, but some entries carry stale username_claimed values or
# placeholder probe URLs; these overrides survive a re-import. Each shallow-
# merges into the converted site dict.
OVERRIDES: dict[str, dict] = {
    # username_claimed "Lost_Arrow" 404s; "rocket" is a live account
    # (verified 200 vs 404 for a nonsense user on 2026-05-20).
    "Monkeytype": {"known_present": "rocket"},
    # Profile-field extractors for --enrich. Best-effort CSS selectors keyed
    # off stable itemprop / meta attributes; if GitHub's markup shifts they
    # simply yield no field (enrichment degrades gracefully).
    "GitHub": {
        "extract": [
            {"field": "name", "selector": "span.vcard-fullname"},
            {"field": "bio", "selector": "div.user-profile-bio"},
            {"field": "avatar", "selector": "img.avatar-user", "attr": "src"},
        ]
    },
}


# Starter tag taxonomy. Tags are advisory groupings for `adler --tag`; an
# untagged site is universal. This is intentionally a small curated seed —
# contributors extend it. Two automatic axes are derived below in addition to
# this map: region from a ccTLD, and region for a few platforms that are
# region-bound despite a .com domain.
CATEGORY_MAP: dict[str, str] = {
    # name.lower() -> category
    "github": "dev", "gitlab": "dev", "bitbucket": "dev", "codepen": "dev",
    "replit": "dev", "dev.to": "dev", "hackernews": "dev", "leetcode": "dev",
    "codewars": "dev", "exercism": "dev", "npm": "dev", "pypi": "dev",
    "docker hub": "dev", "hackerone": "dev", "keybase": "dev",
    "instagram": "social", "facebook": "social", "twitter": "social",
    "x": "social", "vk": "social", "tiktok": "social", "threads": "social",
    "mastodon": "social", "snapchat": "social", "tumblr": "social",
    "reddit": "social", "ok.ru": "social", "weibo": "social",
    "youtube": "video", "vimeo": "video", "dailymotion": "video",
    "twitch": "gaming", "steam community (user)": "gaming",
    "steam community (group)": "gaming", "speedrun.com": "gaming", "chess": "gaming",
    "soundcloud": "music", "spotify": "music", "last.fm": "music",
    "bandcamp": "music", "mixcloud": "music", "genius": "music",
    "patreon": "creator", "ko-fi": "creator", "buy me a coffee": "creator",
    "medium": "blog", "wordpress": "blog", "blogger": "blog",
    "pinterest": "photo", "flickr": "photo", "500px": "photo",
    "deviantart": "art", "behance": "art", "dribbble": "art", "artstation": "art",
}

# ccTLD -> region code. Conservative subset of clearly-national TLDs.
CCTLD_REGION: dict[str, str] = {
    "ru": "ru", "cn": "cn", "jp": "jp", "kr": "kr", "br": "br", "de": "de",
    "fr": "fr", "it": "it", "es": "es", "pl": "pl", "nl": "nl", "ua": "ua",
    "in": "in", "tr": "tr", "cz": "cz", "fi": "fi", "no": "no", "se": "se",
    "vn": "vn", "id": "id", "ir": "ir", "gr": "gr", "hu": "hu", "ro": "ro",
}

# Sites that serve a login wall / JS app / Cloudflare challenge to a plain
# HTTP request, so raw-`reqwest` detection can't tell existing from missing
# accounts (verified via the residential-oracle validation). Tagged
# `bot-protected` so users can `--exclude bot-protected` for a fast clean run
# and know these need a residential IP / browser backend to detect reliably.
BOT_PROTECTED: set[str] = {
    "instagram", "twitter", "x", "tiktok", "facebook", "threads",
    "snapchat", "weibo",
}

# Platforms that are region-bound even on a .com/.net domain.
REGIONAL_PLATFORMS: dict[str, str] = {
    "vk": "ru", "ok.ru": "ru", "odnoklassniki": "ru", "livejournal": "ru",
    "yandex": "ru", "pikabu": "ru", "habr": "ru",
    "weibo": "cn", "bilibili": "cn", "douban": "cn", "zhihu": "cn",
    "naver": "kr",
}


def derive_tags(name: str, url: str) -> list[str]:
    """Compute the starter tag set for a site from its name and URL."""
    tags: set[str] = set()
    key = name.lower()
    if key in CATEGORY_MAP:
        tags.add(CATEGORY_MAP[key])

    host = url.split("://", 1)[-1].split("/", 1)[0].lower()
    tld = host.rsplit(".", 1)[-1] if "." in host else ""
    if tld in CCTLD_REGION:
        tags.add(f"region:{CCTLD_REGION[tld]}")
    for platform, region in REGIONAL_PLATFORMS.items():
        if platform in key:
            tags.add(f"region:{region}")

    if key in BOT_PROTECTED:
        tags.add("bot-protected")

    return sorted(tags)


def convert(entry: dict) -> dict | None:
    """Return an Adler site dict, or None if the entry can't be represented."""
    if entry.get("request_method", "GET").upper() != "GET":
        return None

    error_type = entry.get("errorType")
    raw_url = entry.get("urlProbe") or entry.get("url")
    if not raw_url or "{}" not in raw_url:
        return None
    if not (raw_url.startswith("http://") or raw_url.startswith("https://")):
        return None
    url = raw_url.replace("{}", "{username}")

    signals: list[dict] = []
    if error_type == "status_code":
        signals.append({"kind": "status_found", "codes": [200]})
        code = entry.get("errorCode")
        if isinstance(code, int):
            signals.append({"kind": "status_not_found", "codes": [code]})
        elif isinstance(code, list) and all(isinstance(c, int) for c in code) and code:
            signals.append({"kind": "status_not_found", "codes": code})
        else:
            signals.append({"kind": "status_not_found", "codes": [404]})
    elif error_type == "message":
        msgs = entry.get("errorMsg")
        if isinstance(msgs, str):
            msgs = [msgs]
        if not isinstance(msgs, list) or not all(isinstance(m, str) and m for m in msgs):
            return None
        signals.append({"kind": "status_found", "codes": [200]})
        for msg in msgs:
            signals.append({"kind": "body_absent", "text": msg})
    else:
        # response_url and anything unknown: skip.
        return None

    site: dict = {"url": url, "signals": signals}
    claimed = entry.get("username_claimed")
    if isinstance(claimed, str) and claimed:
        site["known_present"] = claimed
    return site


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    src, dst = sys.argv[1], sys.argv[2]

    with open(src, encoding="utf-8") as f:
        data = json.load(f)

    sites: list[dict] = []
    seen: set[str] = set()
    skipped = 0
    for name, entry in data.items():
        if name.startswith("$") or not isinstance(entry, dict):
            continue
        key = name.lower()
        if key in seen or key in KNOWN_BROKEN:
            skipped += 1
            continue
        converted = convert(entry)
        if converted is None:
            skipped += 1
            continue
        seen.add(key)
        site = {"name": name, **converted}
        if name in OVERRIDES:
            site.update(OVERRIDES[name])
        tags = derive_tags(name, site["url"])
        if tags:
            site["tags"] = tags
        sites.append(site)

    sites.sort(key=lambda s: s["name"].lower())

    header = (
        "Site registry for Adler.\n"
        "Generated from the Sherlock project's data.json (MIT-licensed,\n"
        "sherlock-project/sherlock) by scripts/import_sherlock.py.\n"
        "Detections are imported unverified — run `adler --doctor` to\n"
        "validate signatures. Hand-edit freely; re-running the importer\n"
        "overwrites this file."
    )
    out = {"_comment": header, "sites": sites}
    with open(dst, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=2, ensure_ascii=False)
        f.write("\n")

    print(f"wrote {len(sites)} sites to {dst} ({skipped} skipped)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
