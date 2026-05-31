#!/usr/bin/env bash
# Thin wrapper around bench/_orchestrator.py. See `bench/README.md` and
# `./run.sh --help` for usage.

set -euo pipefail

cd "$(dirname "$0")"
exec python3 _orchestrator.py "$@"
