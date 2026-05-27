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
        "Clozemaster", "Codolio", "DailyMotion",
        "Hashnode", "Hubski", "igromania",
        "interpals", "mercadolivre", "Needrom",
        "opennet", "RocketTube", "RoyalCams", "Shelf",
        "SlideShare", "Splice", "Spotify", "svidbook", "threads",
        "Velomania", "Weblate",
        # Too restrictive: marker is site-wide chrome, not a not-found
        # signal, so the site reports NotFound for everyone (verified the
        # marker appears on the homepage). 2026-05-20.
        "All Things Worn",
        # Too-permissive: a random nonsense username reported Found in the
        # residential validation pass (Replit.com triggered on both
        # baseline+residential, RedTube/YouPorn on residential). Signature
        # is non-discriminating; excluded until a working one is found.
        # 2026-05-24.
        "Replit.com", "RedTube", "YouPorn",
        # Same too-permissive class as the three above — the nightly
        # doctor run on 2026-05-26 (workflow 26438852090) reported
        # `nonsense user … reported Found` for all three NSFW sites
        # below. NSFW gate keeps them out of default scans, but
        # `--nsfw` users would get false positives, so honest move is
        # to drop them until a discriminating signature exists.
        "APClips", "Pornhub", "xHamster",
        # Cross-validated with the Sherlock community's
        # `false_positive_exclusions.txt` (refs/heads/exclusions)
        # against our own doctor on 2026-05-26: these six sites fail
        # on BOTH datacenter (Hetzner/Leaseweb) and US residential
        # (DECODO), AND our `--suggest-known-present` pool finds no
        # working account on residential. Triple agreement →
        # detection is genuinely broken, not just an IP issue.
        # Reinstate once someone authors a discriminating signature.
        "7Cups", "Cults3D", "Envato Forum",
        "YandexMusic", "dailykos", "phpRU",
        # Too restrictive: body-marker is site-wide chrome (forum nav for
        # forum_guns, generic "404" string for Pychess), so the signal
        # fires for *every* user → NotFound for everyone. 2026-05-24.
        "forum_guns", "Pychess",
        # 2026-05-26 nightly doctor on the merged v0.5 registry surfaced
        # 71 more Sherlock-side breakages — split between "too permissive"
        # (random nonsense user reports Found) and "no known-present user
        # yielded Found" (the verified account itself doctor-fails, so the
        # signature no longer discriminates). Run id 26477466422 covered
        # 1443/2558 sites before the 45-min CI timeout; the remaining
        # 1115 are still unprobed and may add more. Reinstate any of
        # these once someone authors a working signature.
        "1337x", "2Dimensions", "9GAG", "Academia.edu", "Airliners",
        "Aparat", "ArtStation", "Audiojungle", "Avizo", "BabyRu", "Bazar.cz",
        "BongaCams", "BreachSta.rs Forum", "CSSBattle", "ChaturBate",
        "Clapper", "CloudflareCommunity", "Code Snippet Wiki", "CodeSandbox",
        "Codechef", "Codepen", "Coinvote", "ColourLovers", "Cracked",
        "CryptoHack", "DMOJ", "DeviantArt", "DigitalSpy", "Discogs",
        "Exposure", "EyeEm", "F3.cool", "Fameswap", "Fandom", "Fanpop",
        "GNOME VCS", "GameFAQs", "Gamespot", "GeeksforGeeks",
        "Genius (Artists)", "Genius (Users)", "GetMyUni", "Giant Bomb",
        "Gitea", "HackenProof (Hackers)", "HackerEarth", "HackerNews",
        "Harvard Scholar", "IRC-Galleria", "Instagram", "Intigriti",
        "Itch.io", "Jimdo", "Kik", "LeetCode", "LemmyWorld", "LessWrong",
        "Letterboxd", "LibraryThing", "Lichess", "LinkedIn", "LottieFiles",
        "MMORPG Forum", "babyblogRU", "devRant", "drive2", "eGPU", "fixya",
        "freecodecamp", "kofi", "livelib",
        # 2026-05-27 doctor pass #2 (workflow ran post-v0.6 prune).
        # 8 more Sherlock-side sigs surfaced as structurally broken
        # (too-permissive: false positives across every probe; or
        # stale username_claimed where the upstream-pinned account
        # 404s with the registered signature). The companion 30
        # Sherlock sites that doctor-failed with Uncertain (likely
        # Cloudflare-blocked from CI) are NOT here — they got the
        # `bot-protected` tag instead, so `--exclude-tag
        # bot-protected` keeps them out of fast scans while
        # residential probes can still hit them.
        "Hackaday", "Ninja Kiwi", "NotABug.org", "PlayStore", "Polygon",
        "PyPi", "Rarible", "Scribd",
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
    # Stale known_present values (Sherlock's were 404) replaced with real
    # accounts that exist as of 2026-05-24, found via residential probe sweep.
    "Archive of Our Own": {"known_present": "torvalds"},
    "BitBucket": {"known_present": "torvalds"},
    "Duolingo": {"known_present": "torvalds"},
    "Gravatar": {"known_present": "jack"},
    "ImgUp.cz": {"known_present": "admin"},
    "Kick": {"known_present": "torvalds"},
    "Kongregate": {"known_present": "octocat"},
    "Opensource": {"known_present": "admin"},
    "Xbox Gamertag": {"known_present": "torvalds"},
    "moikrug": {"known_present": "microsoft"},
    "Ask Fedora": {"known_present": "mattdm"},
    "Bitwarden Forum": {"known_present": "kspearrin"},
    # Instagram's canonical /{username} page is a JS login wall identical
    # for every user, so we hit the `web_profile_info` JSON endpoint that
    # the web app itself queries. Requires the Instagram web app id and a
    # User-Agent the API accepts (the default Chrome UA gets rejected with
    # `{"message":"useragent mismatch","status":"fail"}`); both are sent
    # via the browser backend's `Network.setExtraHTTPHeaders` /
    # `setUserAgentOverride`. Found → 200 + profile JSON containing
    # `"is_verified"`; NotFound → 404.
    #
    # `known_present` is a list of well-known personal accounts — the
    # doctor passes if *any* of them detects as Found. Avoiding the
    # `"instagram"` brand account is deliberate: IG special-cases its
    # own account on `web_profile_info` and returns a degenerate JSON
    # that has no `"is_verified"` marker, so it would always
    # doctor-fail. Listing several guards against any of them being
    # deleted, renamed, or starting to behave oddly.
    "Instagram": {
        "url": "https://i.instagram.com/api/v1/users/web_profile_info/?username={username}",
        "signals": [
            {"kind": "status_not_found", "codes": [404]},
            {"kind": "body_present", "text": "\"is_verified\""},
        ],
        "request_headers": {
            "X-IG-App-ID": "936619743392459",
            "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/119.0.0.0 Safari/537.36",
        },
        "known_present": ["torvalds", "leomessi", "cristiano"],
    },
    # `twitter`/`x` are likewise IG-style brand accounts on x.com that
    # may behave oddly; pin to a few popular human accounts.
    "Twitter": {
        "url": "https://x.com/{username}",
        "signals": [
            {"kind": "body_present", "text": "data-testid=\"primaryColumn\""},
            {"kind": "body_absent", "text": "data-testid=\"mask\""},
        ],
        "known_present": ["jack", "elonmusk", "naval"],
    },
    "APClips": {"known_present": "apclips"},
    "Blitz Tactics": {"known_present": "test"},
    "Career.habr": {"known_present": "tj"},
    "Dribbble": {"known_present": "dribbble"},
    "Empretienda AR": {"known_present": "tj"},
    "fl": {"known_present": "fl"},
    "FortniteTracker": {"known_present": "torvalds"},
    "Image Fap": {"known_present": "admin"},
    "Itch.io": {"known_present": "itch"},
    "kaskus": {"known_present": "torvalds"},
    "livelib": {"known_present": "livelib"},
    "mstdn.io": {"known_present": "admin"},
    "Reddit": {"known_present": "reddit"},
    "Sbazar.cz": {"known_present": "dhh"},
    "SOOP": {"known_present": "support"},
    "Trakt": {"known_present": "admin"},
    "Typeracer": {"known_present": "typeracer"},
    "Untappd": {"known_present": "untappd"},
    "Xvideos": {"known_present": "xvideos"},
    # GeeksforGeeks: cherry-picked from Sherlock fix 2e2248a (Apr 2026).
    # Both existing and missing profiles return 200, but the
    # not-found page's title contains `"false   | GeeksforGeeks Profile"`.
    # We previously had this in KNOWN_BROKEN for false-positives on the
    # old status_code rule; the body-marker fix is clean. Username
    # `adam` is upstream's verified `username_claimed`.
    "GeeksforGeeks": {
        "url": "https://auth.geeksforgeeks.org/user/{username}",
        "signals": [
            {"kind": "status_found", "codes": [200]},
            {"kind": "body_absent", "text": "false   | GeeksforGeeks Profile"},
        ],
        "known_present": "adam",
    },
    # LushStories: cherry-picked from Sherlock fix 2e2248a (Apr 2026).
    # Missing profiles redirect 302 → /login; existing profiles return
    # 200. Our redirect-absent signal matches `/login` in the final
    # URL after following redirects. Marked NSFW; auto-excluded unless
    # `--nsfw` is passed.
    "LushStories": {
        "url": "https://www.lushstories.com/profile/{username}",
        "signals": [
            {"kind": "status_found", "codes": [200]},
            {"kind": "redirect_absent", "fragment": "/login"},
        ],
        "known_present": "chris_brown",
        "tags": ["nsfw"],
    },
}


# Starter tag taxonomy. Tags are advisory groupings for `adler --tag`; an
# untagged site is universal. This is intentionally a small curated seed —
# contributors extend it. Two automatic axes are derived below in addition to
# this map: region from a ccTLD, and region for a few platforms that are
# region-bound despite a .com domain.
# Canonical category axis matches WhatsMyName's 21-category enum so cross-
# source filters work uniformly: `--tag dating` picks up Sherlock-derived
# OkCupid + WMN-derived AdultFriendFinder + Maigret-derived Mamba alike.
# The full canonical list (`archived`, `art`, `blog`, `business`, `coding`,
# `dating`, `finance`, `gaming`, `health`, `hobby`, `images`, `misc`,
# `music`, `news`, `political`, `search`, `shopping`, `social`, `tech`,
# `video`, `nsfw`) is documented in PLAN.md. Sherlock importer uses the
# below name→category map; extending the map adds tags to future imports.
CATEGORY_MAP: dict[str, str] = {
    # dev / coding
    "github": "coding", "gitlab": "coding", "bitbucket": "coding",
    "codepen": "coding", "replit": "coding", "dev.to": "coding",
    "hackernews": "coding", "leetcode": "coding", "codewars": "coding",
    "exercism": "coding", "npm": "coding", "pypi": "coding",
    "docker hub": "coding", "hackerone": "coding", "keybase": "coding",
    # social
    "instagram": "social", "facebook": "social", "twitter": "social",
    "x": "social", "vk": "social", "tiktok": "social", "threads": "social",
    "mastodon": "social", "snapchat": "social", "tumblr": "social",
    "reddit": "social", "ok.ru": "social", "weibo": "social",
    # video
    "youtube": "video", "vimeo": "video", "dailymotion": "video",
    # gaming
    "twitch": "gaming", "steam community (user)": "gaming",
    "steam community (group)": "gaming", "speedrun.com": "gaming",
    "chess": "gaming",
    # music
    "soundcloud": "music", "spotify": "music", "last.fm": "music",
    "bandcamp": "music", "mixcloud": "music", "genius": "music",
    # blog (covers WMN `blog` category)
    "medium": "blog", "wordpress": "blog", "blogger": "blog",
    "patreon": "blog", "ko-fi": "blog", "buy me a coffee": "blog",
    # images (WMN axis replaces our older `photo`)
    "pinterest": "images", "flickr": "images", "500px": "images",
    "imgur": "images",
    # art
    "deviantart": "art", "behance": "art", "dribbble": "art",
    "artstation": "art",
    # news (WMN axis)
    "hackernoon": "news", "lobsters": "news", "slashdot": "news",
    "digg": "news",
    # archived — sites for finding deleted/cached profiles
    "archive.org": "archived", "wayback machine": "archived",
    "archive of our own": "archived", "internet archive": "archived",
    # dating — WMN axis we previously had no coverage for
    "okcupid": "dating", "bumble": "dating", "tinder": "dating",
    "match.com": "dating", "pof": "dating", "hinge": "dating",
    "adultfriendfinder": "dating",
    # shopping
    "etsy": "shopping", "ebay": "shopping", "amazon": "shopping",
    "depop": "shopping", "poshmark": "shopping",
    # finance
    "tradingview": "finance", "stocktwits": "finance", "coinbase": "finance",
    "robinhood": "finance",
    # health
    "myfitnesspal": "health", "strava": "health", "fitocracy": "health",
    # tech
    "stackoverflow": "tech", "stackexchange": "tech", "tech.io": "tech",
    "producthunt": "tech",
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
    # Verified via `--doctor` through a US-residential pool on 2026-05-24:
    # these still serve a JS / login wall to a plain HTTP request even with
    # a clean IP, so they need a browser backend to detect reliably.
    "instagram", "twitter", "x", "facebook", "threads", "weibo",
    # Previously listed: "snapchat", "tiktok". The same 2026-05-24
    # validation showed both detect cleanly on raw HTTP through residential —
    # they're no longer "bot-protected" in our sense.
}

# Platforms that are region-bound even on a .com/.net domain.
REGIONAL_PLATFORMS: dict[str, str] = {
    "vk": "ru", "ok.ru": "ru", "odnoklassniki": "ru", "livejournal": "ru",
    "yandex": "ru", "pikabu": "ru", "habr": "ru",
    "weibo": "cn", "bilibili": "cn", "douban": "cn", "zhihu": "cn",
    "naver": "kr",
}


def derive_tags(name: str, url: str, entry: dict | None = None) -> list[str]:
    """Compute the starter tag set for a site from its name and URL.

    `entry` is the raw Sherlock entry; we read its `isNSFW` flag and
    map it onto our `nsfw` tag, which the Rust loader auto-excludes
    from scans unless `adler --nsfw` is passed. Keeping the mapping
    here means a re-import preserves the gate for any new sites the
    upstream marks isNSFW.
    """
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

    if entry is not None and entry.get("isNSFW") is True:
        tags.add("nsfw")

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
    elif error_type == "response_url":
        # Sherlock's `response_url` model: missing users 302-redirect
        # to a fixed "you must log in" / "no such user" URL; existing
        # ones return 200. Adler's equivalent is `redirect_absent`
        # matching a discriminating substring of the error URL's path
        # in the final URL after following redirects. We use the
        # path-component of the upstream `errorUrl` as that substring
        # (the host alone is often shared with profile URLs and would
        # false-positive).
        err_url = entry.get("errorUrl")
        if not isinstance(err_url, str) or not err_url:
            return None
        from urllib.parse import urlparse
        parsed = urlparse(err_url)
        # Prefer the path; fall back to the full URL if the path is
        # too generic (`/`) or empty.
        fragment = parsed.path if parsed.path and parsed.path != "/" else err_url
        if not fragment:
            return None
        signals.append({"kind": "status_found", "codes": [200]})
        signals.append({"kind": "redirect_absent", "fragment": fragment})
    else:
        # Unknown errorType: skip.
        return None

    site: dict = {"url": url, "signals": signals}
    claimed = entry.get("username_claimed")
    if isinstance(claimed, str) and claimed:
        site["known_present"] = claimed
    # Carry Sherlock's `regexCheck` through as our `regex_check` —
    # 95+ upstream sites declare username constraints (length bounds,
    # allowed character classes). The Rust loader will reject probes
    # for usernames that don't match before issuing any HTTP request.
    regex_check = entry.get("regexCheck")
    if isinstance(regex_check, str) and regex_check:
        site["regex_check"] = regex_check
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
        tags = derive_tags(name, site["url"], entry)
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
