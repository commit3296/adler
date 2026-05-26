#!/usr/bin/env python3
"""Merge the Maigret project's data.json into Adler's site registry.

Source data is MIT-licensed (soxoj/maigret). This script transforms
Maigret's schema into Adler's and *additively* merges into an existing
adler-core/data/sites.json — Adler-side sites win on case-insensitive
name collision (the hand-curated overrides we've accumulated for
existing sites are preserved). The output is a candidate registry;
validate it with `adler --doctor` before swapping it in (R2.3).

Usage:
    python3 scripts/import_maigret.py \\
        research/competitor-study/maigret/maigret/resources/data.json \\
        adler-core/data/sites.json \\
        /tmp/sites-merged.json

Schema mapping:
    Maigret `engines.<Name>` (XenForo, vBulletin, Discourse, ...)
        -> Adler top-level `engines.<Name>` carrying signals only.
           Sites that reference it inherit those signals at load.
    Maigret site checkType:
        status_code  -> [StatusFound[200], StatusNotFound[errorCode or 404]]
        message      -> [StatusFound[200],
                         BodyAbsent[s] for s in absenceStrs,
                         BodyPresent[s] for s in presenseStrs]
        response_url -> [StatusFound[200], RedirectAbsent[errorUrl path]]
        (missing)    -> empty (only valid when site references an engine)
    Maigret site URL:
        own `url` containing `{username}` -> taken as-is
        engine reference -> expand engine.url template using site's
            `urlMain` + `urlSubpath` (Maigret-style templating).
            Engine-only sites with no `urlMain` are skipped.
    headers      -> request_headers (verbatim)
    regexCheck   -> regex_check
    tags         -> tags (lowercased, deduped)
    usernameClaimed -> known_present
    disabled: true -> SKIPPED
    POST request_method -> SKIPPED (Adler only issues GET)

Detections are imported unverified: Maigret's signatures rot over time,
and signal selectivity has not been measured against Adler's
negative-priority aggregation. Run `adler --doctor` to find sites
whose detection no longer holds before promoting the output to the
live registry.
"""

import json
import re
import sys
from urllib.parse import urlparse


# Mirrors the schema's site-name pattern in docs/sites.schema.json.
# Site names that don't match are skipped: Adler enforces this at load
# time to keep names safe for shell / CLI / CSV interpolation.
NAME_RE = re.compile(r"^[\w][\w .()!/+-]*$")

# Rust's `regex` crate does not support lookaround. Maigret carries
# ~60 patterns that use `(?=...)`, `(?!...)`, `(?<=...)`, `(?<!...)`.
# Drop the regex_check field for those sites so the registry loads
# clean — the site is still usable, the per-site username gate just
# isn't enforced.
UNSUPPORTED_REGEX_RE = re.compile(r"\(\?[=!<]")


def maigret_engine_to_adler(maigret_engine: dict) -> dict | None:
    """Translate a Maigret engine block into Adler engine fields.

    Returns None when the engine carries no inheritable signal — those
    are engines that only exist to tag a URL shape (engine404get etc.)
    and are not useful on their own without per-site signals.
    """
    site = maigret_engine.get("site", {})
    check_type = site.get("checkType")
    signals: list[dict] = []

    if check_type == "message":
        for s in site.get("absenceStrs") or []:
            if isinstance(s, str) and s:
                signals.append({"kind": "body_absent", "text": s})
        for s in site.get("presenseStrs") or []:
            if isinstance(s, str) and s:
                signals.append({"kind": "body_present", "text": s})
        if signals:
            # Most Maigret message engines also expect 200 on found
            signals.insert(0, {"kind": "status_found", "codes": [200]})
    elif check_type == "status_code":
        signals.append({"kind": "status_found", "codes": [200]})
        signals.append({"kind": "status_not_found", "codes": [404]})
    elif check_type == "response_url":
        # No errorUrl on the engine level — sites supply it. Don't emit
        # a half-baked signal; the engine here only carries a hint that
        # response_url is the check style.
        return None
    else:
        return None

    if not signals:
        return None

    out: dict = {"signals": signals}
    headers = site.get("headers")
    if isinstance(headers, dict) and headers:
        out["request_headers"] = {str(k): str(v) for k, v in headers.items()}
    regex_check = site.get("regexCheck")
    if (
        isinstance(regex_check, str)
        and regex_check
        and not UNSUPPORTED_REGEX_RE.search(regex_check)
    ):
        out["regex_check"] = regex_check
    return out


def resolve_url(site: dict, engines: dict) -> str | None:
    """Return a usable URL template for an Adler site or None to skip.

    Maigret engines hold a `url` template like
    `{urlMain}{urlSubpath}/members/?username={username}` and rely on
    each site to supply `urlMain` (mandatory) and optionally
    `urlSubpath`. Expand it here so the imported Adler site has a
    self-contained URL — Adler engines carry signature, not URL shape.
    """
    own = site.get("url")
    if isinstance(own, str) and "{username}" in own and own.startswith(
        ("http://", "https://")
    ):
        return own

    engine_name = site.get("engine")
    if not engine_name:
        return None
    engine = engines.get(engine_name)
    if not isinstance(engine, dict):
        return None
    template = (engine.get("site") or {}).get("url")
    if not isinstance(template, str) or "{username}" not in template:
        return None

    url_main = site.get("urlMain") or (engine.get("site") or {}).get("urlMain")
    if not isinstance(url_main, str) or not url_main.startswith(
        ("http://", "https://")
    ):
        return None
    # Maigret's urlMain often has a trailing slash; the template usually
    # contains its own separators. Strip a single trailing slash so we
    # don't double up on `https://x.com//path`.
    url_main = url_main.rstrip("/")

    url_subpath = site.get("urlSubpath") or ""
    if not isinstance(url_subpath, str):
        url_subpath = ""

    expanded = template.replace("{urlMain}", url_main).replace(
        "{urlSubpath}", url_subpath
    )
    if "{username}" not in expanded or "{" in expanded.replace("{username}", ""):
        return None
    if not expanded.startswith(("http://", "https://")):
        return None
    return expanded


def maigret_site_to_adler(
    name: str, site: dict, engines: dict, importable_engines: set[str]
) -> dict | None:
    """Translate a Maigret site into an Adler site, or None to skip.

    `importable_engines` is the set of engine names we successfully
    translated into Adler engines. A site that references an
    out-of-set engine and has no own signals can't be represented and
    is skipped.
    """
    if site.get("disabled") is True:
        return None
    if (site.get("request_method") or "GET").upper() != "GET":
        return None
    if not NAME_RE.match(name) or len(name) > 80:
        return None

    url = resolve_url(site, engines)
    if url is None:
        return None

    out: dict = {"name": name, "url": url}

    check_type = site.get("checkType")
    signals: list[dict] = []
    if check_type == "status_code":
        signals.append({"kind": "status_found", "codes": [200]})
        code = site.get("errorCode")
        if isinstance(code, int):
            signals.append({"kind": "status_not_found", "codes": [code]})
        elif isinstance(code, list) and all(isinstance(c, int) for c in code) and code:
            signals.append({"kind": "status_not_found", "codes": code})
        else:
            signals.append({"kind": "status_not_found", "codes": [404]})
    elif check_type == "message":
        for s in site.get("absenceStrs") or []:
            if isinstance(s, str) and s:
                signals.append({"kind": "body_absent", "text": s})
        for s in site.get("presenseStrs") or []:
            if isinstance(s, str) and s:
                signals.append({"kind": "body_present", "text": s})
        if signals:
            signals.insert(0, {"kind": "status_found", "codes": [200]})
    elif check_type == "response_url":
        err_url = site.get("errorUrl")
        if isinstance(err_url, str) and err_url:
            parsed = urlparse(err_url)
            fragment = (
                parsed.path if parsed.path and parsed.path != "/" else err_url
            )
            if fragment:
                signals.append({"kind": "status_found", "codes": [200]})
                signals.append({"kind": "redirect_absent", "fragment": fragment})
    # else: no own checkType -> rely on engine inheritance

    engine_name = site.get("engine")
    engine_usable = bool(engine_name) and engine_name in importable_engines
    if signals:
        out["signals"] = signals
    elif engine_usable:
        # Will be filled at registry load via engine inheritance
        pass
    else:
        # No signals and no usable engine -> can't represent in Adler
        return None

    if engine_usable:
        out["engine"] = engine_name

    headers = site.get("headers")
    if isinstance(headers, dict) and headers:
        out["request_headers"] = {str(k): str(v) for k, v in headers.items()}

    regex_check = site.get("regexCheck")
    if (
        isinstance(regex_check, str)
        and regex_check
        and not UNSUPPORTED_REGEX_RE.search(regex_check)
    ):
        out["regex_check"] = regex_check

    tags = site.get("tags")
    cleaned: set[str] = set()
    if isinstance(tags, list):
        cleaned = {t.lower() for t in tags if isinstance(t, str) and t}
    # Provenance tag — the nightly doctor uses it to scope its
    # structural-failure classification (a Maigret-imported entry that
    # rots on day 1 is different from a Sherlock-imported one we've
    # been shipping for months).
    cleaned.add("source:maigret")
    out["tags"] = sorted(cleaned)

    claimed = site.get("usernameClaimed")
    if isinstance(claimed, str) and claimed:
        out["known_present"] = claimed

    return out


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__)
        return 2
    maigret_src, adler_src, dst = sys.argv[1], sys.argv[2], sys.argv[3]

    with open(maigret_src, encoding="utf-8") as f:
        maigret = json.load(f)
    with open(adler_src, encoding="utf-8") as f:
        adler = json.load(f)

    existing_names = {s["name"].lower() for s in adler.get("sites", [])}
    existing_engines = adler.get("engines") or {}

    # Engines
    out_engines: dict = dict(existing_engines)
    referenced_engines: set[str] = set()
    for name, eng in (maigret.get("engines") or {}).items():
        if name in out_engines:
            continue
        adler_eng = maigret_engine_to_adler(eng)
        if adler_eng is not None:
            out_engines[name] = adler_eng

    # Sites
    added: list[dict] = []
    seen: set[str] = set(existing_names)
    skipped_disabled = 0
    skipped_no_url = 0
    skipped_no_signal = 0
    skipped_dup = 0
    for name, site in (maigret.get("sites") or {}).items():
        if not isinstance(site, dict):
            continue
        key = name.lower()
        if key in seen:
            skipped_dup += 1
            continue
        if site.get("disabled") is True:
            skipped_disabled += 1
            continue
        converted = maigret_site_to_adler(
            name, site, maigret.get("engines") or {}, set(out_engines.keys())
        )
        if converted is None:
            # Distinguish reasons for stats
            if resolve_url(site, maigret.get("engines") or {}) is None:
                skipped_no_url += 1
            else:
                skipped_no_signal += 1
            continue
        if "engine" in converted:
            referenced_engines.add(converted["engine"])
        seen.add(key)
        added.append(converted)

    # Drop engines that nothing references (engine404get etc. that we
    # didn't emit signals for, and engineRedirect which we skipped).
    out_engines = {
        k: v
        for k, v in out_engines.items()
        if k in referenced_engines
        or any(s.get("engine") == k for s in adler.get("sites", []))
    }

    sites = sorted(
        list(adler.get("sites", [])) + added, key=lambda s: s["name"].lower()
    )

    header = adler.get("_comment") or (
        "Site registry for Adler. Detections imported unverified — run "
        "`adler --doctor` before promoting."
    )
    if "Maigret" not in header:
        header = header.rstrip() + (
            "\nMerged with sites/engines from the Maigret project "
            "(MIT-licensed, soxoj/maigret) via scripts/import_maigret.py."
        )

    out = {"_comment": header}
    if out_engines:
        out["engines"] = dict(sorted(out_engines.items()))
    out["sites"] = sites

    with open(dst, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=2, ensure_ascii=False)
        f.write("\n")

    print(
        f"merged: +{len(added)} sites, +{len(out_engines) - len(existing_engines)} engines "
        f"(skipped: {skipped_dup} dup, {skipped_disabled} disabled, "
        f"{skipped_no_url} no-url, {skipped_no_signal} no-signal)"
    )
    print(f"output: {dst} ({len(sites)} sites total)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
