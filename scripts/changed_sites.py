#!/usr/bin/env python3
"""Print the names of sites added or modified between two registry files.

Usage: changed_sites.py BASE.json HEAD.json

BASE may be missing or unparseable (e.g. the site list didn't exist on the
base branch) — it is then treated as empty, so every site in HEAD counts as
added. Output is one site name per line, suitable for feeding to
`adler --doctor --only`.
"""

import json
import sys


def load(path: str) -> dict:
    try:
        with open(path, encoding="utf-8") as f:
            return {s["name"]: s for s in json.load(f)["sites"]}
    except (FileNotFoundError, json.JSONDecodeError, KeyError, TypeError):
        return {}


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: changed_sites.py BASE.json HEAD.json", file=sys.stderr)
        return 2
    base = load(sys.argv[1])
    head = load(sys.argv[2])
    for name, site in head.items():
        if base.get(name) != site:
            print(name)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
