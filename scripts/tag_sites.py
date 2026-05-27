#!/usr/bin/env python3
"""Apply the starter tag heuristic to an existing registry, in place.

Reuses `derive_tags` from import_sherlock so the tag logic lives in one place.
Use this to (re)tag the current data/sites.json without a full re-import from
Sherlock's source (which would also re-pull every site definition).

Usage:
    python3 scripts/tag_sites.py adler-core/data/sites.json
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from import_sherlock import derive_tags  # noqa: E402


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: tag_sites.py sites.json", file=sys.stderr)
        return 2
    path = sys.argv[1]
    with open(path, encoding="utf-8") as f:
        data = json.load(f)

    tagged = 0
    for site in data["sites"]:
        derived = derive_tags(site["name"], site["url"])
        if not derived:
            continue
        # Merge into existing tags rather than replace — preserves
        # hand-curated values (`source:maigret`, `bot-protected`,
        # `protection:cloudflare`, the doctor's classification tags)
        # that derive_tags has no way to recover from name+URL alone.
        existing = set(site.get("tags") or [])
        new = existing | set(derived)
        if new != existing:
            site["tags"] = sorted(new)
            tagged += 1

    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2, ensure_ascii=False)
        f.write("\n")

    print(f"tagged {tagged} of {len(data['sites'])} sites in {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
