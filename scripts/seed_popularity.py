#!/usr/bin/env python3
"""Populate `Site.popularity` ranks on the registry from a curated list.

The seed list covers ~50 well-known OSINT-relevant sites where most
users have accounts — useful for fast `adler --top 50 <name>` scans
that don't burn time on long-tail forum instances. Rank 1 = most
popular. Ranks are hand-curated, not derived from traffic data; the
goal is "if someone has an online identity, these are the sites
worth checking first."

Run after editing the list. Idempotent — re-running just
re-applies the ranks.

Usage:
    python3 scripts/seed_popularity.py adler-core/data/sites.json
"""

import json
import sys


# Lower = more popular. Aliases handle case/punctuation variants
# (Sherlock has "X", "x.com", "Twitter" all for the same site; we
# match the first that exists on a registry entry by case-insensitive
# name and assign rank).
POPULAR: list[tuple[int, list[str]]] = [
    (1,  ["youtube"]),
    (2,  ["facebook"]),
    (3,  ["instagram"]),
    (4,  ["twitter", "x"]),
    (5,  ["wikipedia"]),
    (6,  ["reddit"]),
    (7,  ["tiktok"]),
    (8,  ["linkedin"]),
    (9,  ["github"]),
    (10, ["pinterest"]),
    (11, ["snapchat"]),
    (12, ["twitch"]),
    (13, ["discord"]),
    (14, ["telegram"]),
    (15, ["whatsapp"]),
    (16, ["spotify"]),
    (17, ["medium"]),
    (18, ["tumblr"]),
    (19, ["mastodon"]),
    (20, ["threads"]),
    (21, ["vk", "vkontakte"]),
    (22, ["ok.ru", "odnoklassniki"]),
    (23, ["weibo"]),
    (24, ["soundcloud"]),
    (25, ["vimeo"]),
    (26, ["flickr"]),
    (27, ["dailymotion"]),
    (28, ["deviantart"]),
    (29, ["behance"]),
    (30, ["dribbble"]),
    (31, ["wordpress"]),
    (32, ["blogger"]),
    (33, ["bandcamp"]),
    (34, ["mixcloud"]),
    (35, ["last.fm"]),
    (36, ["patreon"]),
    (37, ["ko-fi"]),
    (38, ["gitlab"]),
    (39, ["bitbucket"]),
    (40, ["stackoverflow"]),
    (41, ["npm"]),
    (42, ["pypi"]),
    (43, ["docker hub"]),
    (44, ["keybase"]),
    (45, ["hackerone"]),
    (46, ["replit"]),
    (47, ["dev.to"]),
    (48, ["codepen"]),
    (49, ["leetcode"]),
    (50, ["pinterest"]),
]


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: seed_popularity.py adler-core/data/sites.json", file=sys.stderr)
        return 2
    path = sys.argv[1]
    with open(path, encoding="utf-8") as f:
        data = json.load(f)

    by_name = {s["name"].lower(): s for s in data["sites"]}
    populated = 0
    missed: list[tuple[int, list[str]]] = []
    for rank, aliases in POPULAR:
        for alias in aliases:
            entry = by_name.get(alias.lower())
            if entry is not None:
                # Don't overwrite a manually-set popularity that
                # might be more accurate than our seed.
                if "popularity" not in entry:
                    entry["popularity"] = rank
                    populated += 1
                break
        else:
            missed.append((rank, aliases))

    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2, ensure_ascii=False)
        f.write("\n")

    print(f"populated {populated} of {len(POPULAR)} ranks in {path}")
    if missed:
        print(f"missing ({len(missed)}): " + ", ".join(
            f"#{r}={'/'.join(a)}" for r, a in missed
        ))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
