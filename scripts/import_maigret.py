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
NAME_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_ .()!/+-]*$")

# Rust's `regex` crate does not support lookaround. Maigret carries
# ~60 patterns that use `(?=...)`, `(?!...)`, `(?<=...)`, `(?<!...)`.
# Drop the regex_check field for those sites so the registry loads
# clean — the site is still usable, the per-site username gate just
# isn't enforced.
UNSUPPORTED_REGEX_RE = re.compile(r"\(\?[=!<]")


# Sites whose Maigret-imported signature is structurally broken
# (verified via the registry doctor). Two failure modes are folded
# into one set:
#   * "too permissive" — a random nonsense user reports Found
#     (the body marker or status code fires for every probe; users
#     would see false positives across every scan).
#   * "no known-present user yielded Found" — even the upstream's
#     verified `usernameClaimed` doctor-fails (signature no longer
#     discriminates).
# Both classes are deferred until someone authors a working
# signature; revisiting the entry is one PR away. Keyed
# case-insensitively. Sourced from doctor run 26477466422 on
# 2026-05-26 (covered 1443/2558 sites in 45 min before the CI
# timeout — the remaining unprobed tranche may add more).
KNOWN_BROKEN = {
    name.lower()
    for name in (
        "0-3.RU", "1001tracklists", "101xp.com", "11x2", "162nord.org",
        "21buttons", "23hq", "27r.ru", "2el5.ucoz.ua", "308-club.ru",
        "3DMir.ru", "3glaz.org", "500px", "50cc.com.ua", "8tracks.com",
        "Advego", "AirNFTs", "Alabay", "AllRecipes", "Allhockey", "Amperka",
        "Anarcho-punk", "Anibox", "Anime-planet", "AnimeNewsNetwork",
        "Anobii", "Anonup", "Antichat", "Antiwomen", "ApexLegends", "Appian",
        "Aptoide", "Archive.orgParlerPosts", "Archive.orgParlerProfiles",
        "Archive.orgTwitterProfiles", "Archlinux", "Arduino", "AreKamrbb",
        "Armchairgm", "Artistsnclients", "Astralinux", "Astro-talks",
        "Autolada", "Automania", "Avforums", "Avto-forum.name", "Avtomarket",
        "B17", "Baby.ru", "BabyBlog.ru", "Barnacl", "Basecamphq",
        "BeatStars", "Bentbox", "BitPapa", "Bitwarden", "BleachFandom",
        "Bobrdobr", "BodyBuilding", "Boosty", "Borsch.gallery",
        "Bratsk Forum", "Breach Sta.rs Forum", "Brusheezy", "CD-Action",
        "CapitalcityCombats", "Cash.app", "Casial", "Cent", "Change.org",
        "Chemport", "Chess-russia", "Clozemaster", "Codecanyon",
        "Codementor", "ContactInBio (URL)", "Coub", "Cqham",
        "CreativeMarket", "Cults3d", "Cydak", "DLive", "DailyMotion",
        "Dalnoboi", "DarkNet Trust", "DeepDreamGenerator",
        "DefenceForumIndia", "Demonscity", "Demotywatory", "Depop",
        "Designspiration", "Desu", "Detstrana", "Dissenter", "Division2",
        "Djangoproject.co", "Dojoverse", "Dreamstime", "Dumpor", "Ebay",
        "Elakiri", "Elftown", "Elwo", "Enot-poloskun", "Envato", "Expono",
        "F-droid", "F6S", "FCRubin", "FIFA FORUMS", "Fabswingers", "Faceit",
        "Faktopedia", "FandomCommunityCentral", "Fanlore", "Fansly",
        "Finforum", "Fiverr", "Fluther", "Flyertalk", "Fodors",
        "Forum.jambox.ru", "ForumJizni", "ForumKinopoisk", "ForumSmotri",
        "Forumsi", "Forumteam", "Fotka", "Fotki", "Foursquare",
        "Freelancebay", "Freelancehunt", "Freepik", "Friendfinder-x",
        "Fullhub", "G2g.com", "GBAtemp.net", "GGIZI", "GPS-Forum",
        "GamesRadar", "Gamesubject", "Gapyear", "Gardrops", "Geekdoing",
        "Geeksfor Geeks", "Genius", "Gnome-vcs", "Golangbridge",
        "Good-music", "Google Maps", "Google Plus (archived)",
        "Google Scholar", "Gribnikikybani", "Guns.ru", "GuruShots",
        "HabrCareer", "HackerNoon", "Hashnode", "Hctorpedo", "Hexrpg",
        "HiddenAnswers", "Holiday.ru", "Holopin", "Hyundaitruckclub",
        "ITVDN Forum", "Icheckmovies", "Icobench", "ImageShack", "ImgInn",
        "Ingvarr", "Insanejournal", "Interfaith", "Ipolska.pl", "Ispdn",
        "Itfy", "JSFiddle", "Jer.forum24.ru", "Justforfans", "Justlanded",
        "Karab.in", "Kickstarter", "KnigiOnline", "Kotburger", "Kriptom",
        "KubanForum24", "Ladies", "Lemmy World", "Liberapay", "Life-dom2",
        "Likee", "Linkkle", "LinuxMint", "LiveInternet", "LiveTrack24",
        "Liveexpert", "Livejasmin", "LiverpoolFC", "Livios", "Lkforum",
        "Lomography", "Love.Mail.ru", "Lovemakeup", "Lowcygier.pl",
        "Maccentre", "Maga-Chat", "Magiimir", "Mamochki", "Mapify.travel",
        "Mapillary Forum", "Marshmallow", "MassageAnywhere", "Matrix",
        "Medyczka.pl", "Meendo", "MeetMe", "Megamodels.pl", "Megane2",
        "MetaDiscourse", "Minecraftlist", "Mistrzowie", "Mobypicture",
        "ModDB", "abho.ru", "adblockplus.org", "all-gta.info",
        "allmobile.vo.uz", "amax-sb.ru", "animal-hope.ru", "antiscam.space",
        "aquamen.ru", "architizer.com", "arcolinuxforum.com",
        "artmilitaire.ru", "as8.ru", "asquero.com", "audi-belarus.by",
        "aussiehomebrewer.com", "australianfrequentflyer.com.au",
        "autotob.ru", "aviahistory.ucoz.ru", "avtoexamen.com", "awd.ru",
        "azovmore.ucoz.ru", "babyboom.pl", "baltnethub.3dn.ru",
        "barnaul-forum.ru", "baseball-reference.com", "bbs.boingboing.net",
        "beacons.ai", "bestclips.ws", "betawiki.net", "beyond3d",
        "big-game.ucoz.ru", "bitcoin.it", "bitpapa.com", "blogs.klerk.ru",
        "boards.insidethestar.com", "boards.straightdope.com",
        "bookafly.com", "bookz.su", "boominfo.org", "brute.pw",
        "bulbapedia.bulbagarden.net", "car72.ru", "cedia-club.ru",
        "cfd-online", "cheat-master.ru", "chelfishing.ru",
        "chelny-diplom.ru", "chevrolet-daewoo.ru", "chsnik-kz.ucoz.kz",
        "club-fiat.org.ua", "club-gas.ru", "club.passion.ru",
        "codeforces.com", "coder.social", "coffeeforum.ru",
        "community.adobe.com", "community.clearlinux.org",
        "community.endlessos.com", "community.getpostman.com",
        "community.gozenhost.com", "community.icons8.com",
        "community.letsencrypt.org", "community.mydevices.com",
        "community.p2pu.org", "community.simplilearn.com",
        "community.sphero.com", "community.startupnation.com",
        "community.sweatco.in", "cosmotarolog.ucoz.ru", "counter-art.ru",
        "crafta.ua", "creationwiki.org", "crown6.org", "cruiserswiki.org",
        "css-play4fun.ru", "cubecraft.net", "dapf.ru", "dariawiki.org",
        "darkart3d.ru", "deeptor.ws", "demon-art.ru", "dimitrov.ucoz.ua",
        "directx10.org", "discourse.haskell.org", "discourse.huel.com",
        "discourse.jupyter.org", "discourse.saylor.org",
        "discourse.snowplowanalytics.com", "discoursedb.org",
        "discuss.bootstrapped.fm", "discuss.codecademy.com",
        "discuss.inventables.com", "discuss.studiofow.com",
        "discussions.ubisoft.com", "dnbforum.com", "doccarb.ucoz.ru",
        "dolap", "domfrunze.kg", "donate.stream", "doublecmd.h1n.ru",
        "dreamteam43.ru", "dreddmc.ru", "dumskaya.net", "dvocu.ru", "dwg",
        "dzintarsmos09.ru", "e36club.com.ua", "edns.domains/iotex",
        "egiki.ru", "electronic-cigarette.ru", "electroprom.my1.ru",
        "elektrik-avto.ru", "elektron.ucoz.ua", "en.brickimedia.org",
        "en.wikifur.com", "endoctor.ru", "espero-club.ru",
        "exploretalent.com", "fanfiktion.de", "fanscout.com",
        "figarohair.ru", "fire-team.clan.su", "fkclub.ru", "followus.com",
        "ford-mondeoff.ru", "forum-ssc.ucoz.ru", "forum-ukraina.net",
        "forum.1796web.com", "forum.alconar.ru", "forum.audacityteam.org",
        "forum.balletfriends.ru", "forum.betsportslive.ru",
        "forum.blackmagicdesign.com", "forum.core-electronics.com.au",
        "forum.danetka.ru", "forum.eksmo.ru", "forum.exkavator.ru",
        "forum.finance.ua", "forum.foe-rechner.de", "forum.garudalinux.org",
        "forum.ghost.org", "forum.gong.bg", "forum.heroesleague.ru",
        "forum.hr", "forum.lancerx.ru",
        "forum.languagelearningwithnetflix.com", "forum.modding.ru",
        "forum.mxlinux.org", "forum.nameberry.com", "forum.newlcn.com",
        "forum.nvworld.ru", "forum.openoffice.org", "forum.palemoon.org",
        "forum.paradox.network", "forum.pavlovskyposad.ru",
        "forum.pkp.sfu.ca", "forum.postupim.ru", "forum.prihoz.ru",
        "forum.rarib.ag", "forum.rmnt.ru", "forum.rollerclub.ru",
        "forum.rosalinux.ru", "forum.rzn.info", "forum.scssoft.com",
        "forum.shopsmith.com", "forum.shotcut.org", "forum.sketchfab.com",
        "forum.spyderco.com", "forum.ss-iptv.com", "forum.sureai.net",
        "forum.ua-vet.com", "forum.ubuntu-it.org", "forum.uti-puti.com.ua",
        "forum.virtualsoccer.ru", "forum.wladimir.su", "forum.zorin.com",
        "forumbebas.com", "forums.docker.com", "forums.drom.ru",
        "forums.grandstream.com", "forums.linuxmint.com",
        "forums.mageia.org", "forums.mmorpg.com", "forums.scummvm.org",
        "forums.steinberg.net", "forums.theanimenetwork.com",
        "forums.wrestlezone.com", "forums.zooclub.ru", "fotostrana.ru",
        "foumds.universaldashboard.io", "frauflora.com", "free-otvet.ru",
        "free-pass.ru", "freedom.kiev.ua", "freelance.ua",
        "freelancehunt.ru", "freelansim.ru", "funcom", "garmin.ucoz.ru",
        "gearheadwiki.com", "gentoo", "getmakerlog.com", "ghisler.ch",
        "gifts.ucoz.ru", "git.tcp.direct", "goroskop.ucoz.ua",
        "grand-magic.ru", "guitar.by", "hcv.ru", "help-baby.org",
        "hevc-club.ucoz.net", "hikvision.msk.ru", "hiveblocks.com",
        "hiveos.farm", "homeofsky.ucoz.ru", "horek-samara.ru", "hyprr.com",
        "i2pforum", "iNaturalist", "ic.ucoz.ru", "imgsrc.ru",
        "inaturalist.nz", "inaturalist.org", "indiatv-forum.ru",
        "induste.com", "instaprofi.ru", "izobil.ru", "jeepspb.ru",
        "kadroviku.ru", "kali.org.ru", "kam-mamochka.ru", "kashanya.com",
        "kaz.ionyk.ru", "kazanlashkigalab.com", "khabmama.ru",
        "kiabongo.info", "kiev-live.com", "kinohouse.ucoz.ru",
        "kliki-doma.ru", "kpyto.pp.net.ua", "kredituemall.ru",
        "krskforum.com", "ksmsp.ru", "l2bz.ru", "labpentestit",
        "ladpremiya.ru", "lampoviedushi.hammarlund.ru", "lavkachudec.ru",
        "lemfo-russia.ru", "linuxpip.org", "liozno.info", "lithotherapy",
        "love2d.org", "lubuntu.ru", "lviv4x4.club", "mailpass.site",
        "make-ups.ru", "mama.tomsk.ru", "mansonwiki.com", "md", "mel.fm",
        "mfd", "microcap.forum24.ru", "mikrob.ru", "milliarderr.com",
        "mindmachine.ru", "mineplex.com", "minesuperior.com",
        "mir-stalkera.ru", "mir2007.ru", "mirmuzyki.ucoz.net",
        "mistoodesa.ucoz.ua", "mix-best.ucoz.ru", "mkr-rodniki.ru",
        "mkuniverse.ru", "mnogodetok.ru", "modnaya"
    )
}

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
    skipped_broken = 0
    for name, site in (maigret.get("sites") or {}).items():
        if not isinstance(site, dict):
            continue
        key = name.lower()
        if key in seen:
            skipped_dup += 1
            continue
        if key in KNOWN_BROKEN:
            skipped_broken += 1
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
        f"(skipped: {skipped_dup} dup, {skipped_broken} known-broken, {skipped_disabled} disabled, "
        f"{skipped_no_url} no-url, {skipped_no_signal} no-signal)"
    )
    print(f"output: {dst} ({len(sites)} sites total)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
