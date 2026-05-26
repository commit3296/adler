#!/usr/bin/env python3
"""Convert WhatsMyName's wmn-data.json into Adler's site registry shape.

Output goes to adler-core/data/sites_wmn.json — a *separate* file
from sites.json. WhatsMyName data is licensed under CC BY-SA 4.0,
which is incompatible with Adler's single MIT LICENSE; shipping it
in a separate, clearly-attributed file keeps the licensing posture
honest. The file is dual-licensed alongside LICENSE-CC-BY-SA-4.0 at
the repo root.

The Rust loader exposes the file via Registry::wmn_embedded(); the
CLI exposes it via `adler --with-wmn`. Default scans don't include
WMN sites — this is opt-in to keep the MIT-only path the default
for downstream redistributors.

Usage:
    python3 scripts/import_whatsmyname.py \\
        research/competitor-study/whatsmyname/wmn-data.json \\
        adler-core/data/sites_wmn.json

Schema mapping:
    e_code + e_string + m_code + m_string -> [StatusFound[e_code],
        BodyPresent[e_string], StatusNotFound[m_code],
        BodyAbsent[m_string]]
    known                  -> known_present (single or array)
    cat                    -> tags (with `xx NSFW xx` -> `nsfw`)
    protection             -> additional tag(s) (cloudflare, captcha, ...)
    headers                -> request_headers (verbatim)
    strip_bad_char         -> SKIPPED (no Adler equivalent today)
    post_body              -> SKIPPED (POST sites; Adler issues GET)
    valid: false           -> SKIPPED

Two-sided detection is the WMN signature: both presence and absence
markers are mandatory, which makes their signals discriminating
against false positives. We carry both directly into Adler's
negative-priority aggregator without simplification.
"""

import json
import re
import sys


NAME_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_ .()!/+-]*$")

CATEGORY_TAG_MAP = {
    "xx nsfw xx": "nsfw",
    "archived": "archive",
    "search": "people-search",
}


def convert(entry: dict) -> dict | None:
    if entry.get("valid") is False:
        return None
    if entry.get("post_body"):
        return None

    name = entry.get("name")
    if not isinstance(name, str) or not name:
        return None
    if not NAME_RE.match(name) or len(name) > 80:
        return None

    uri = entry.get("uri_check")
    if not isinstance(uri, str) or "{account}" not in uri:
        return None
    if not (uri.startswith("http://") or uri.startswith("https://")):
        return None
    url = uri.replace("{account}", "{username}")
    if "{" in url.replace("{username}", ""):
        return None

    e_code = entry.get("e_code")
    e_string = entry.get("e_string")
    m_code = entry.get("m_code")
    m_string = entry.get("m_string")
    if not (
        isinstance(e_code, int)
        and isinstance(m_code, int)
        and isinstance(e_string, str)
        and e_string
        and isinstance(m_string, str)
        and m_string
    ):
        # WMN's contract is two-sided detection; entries missing it are
        # incomplete by their own standard — skip.
        return None

    signals = [
        {"kind": "status_found", "codes": [e_code]},
        {"kind": "body_present", "text": e_string},
        {"kind": "status_not_found", "codes": [m_code]},
        {"kind": "body_absent", "text": m_string},
    ]

    out: dict = {"name": name, "url": url, "signals": signals}

    known = entry.get("known")
    if isinstance(known, list):
        cleaned = [k for k in known if isinstance(k, str) and k]
        if len(cleaned) == 1:
            out["known_present"] = cleaned[0]
        elif cleaned:
            out["known_present"] = cleaned

    headers = entry.get("headers")
    if isinstance(headers, dict) and headers:
        out["request_headers"] = {str(k): str(v) for k, v in headers.items()}

    tags: set[str] = {"source:wmn"}
    cat = entry.get("cat")
    if isinstance(cat, str) and cat:
        ck = cat.lower()
        tags.add(CATEGORY_TAG_MAP.get(ck, ck))
    protection = entry.get("protection")
    if isinstance(protection, list):
        for p in protection:
            if isinstance(p, str) and p:
                low = p.lower()
                if low in {"cloudflare", "cloudfront", "ddos-guard", "captcha", "anubis"}:
                    tags.add("bot-protected")
                tags.add(f"protection:{low}")
    out["tags"] = sorted(tags)

    return out


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
    for entry in data.get("sites", []):
        if not isinstance(entry, dict):
            continue
        converted = convert(entry)
        if converted is None:
            skipped += 1
            continue
        key = converted["name"].lower()
        if key in seen:
            skipped += 1
            continue
        seen.add(key)
        sites.append(converted)

    sites.sort(key=lambda s: s["name"].lower())

    header = (
        "WhatsMyName-derived sites for Adler, licensed CC BY-SA 4.0 "
        "(see LICENSE-CC-BY-SA-4.0 at the repo root).\n"
        "Source: WebBreacher/WhatsMyName (https://whatsmyname.app).\n"
        "Generated by scripts/import_whatsmyname.py. Detections imported "
        "unverified — run `adler --doctor --with-wmn` before relying on "
        "any signature.\n"
        "Adler's MIT licence does NOT cover this file. Downstream "
        "redistribution must preserve attribution and the ShareAlike "
        "obligation on derivative data."
    )
    out = {"_comment": header, "sites": sites}
    with open(dst, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=2, ensure_ascii=False)
        f.write("\n")

    print(f"wrote {len(sites)} sites to {dst} ({skipped} skipped)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
