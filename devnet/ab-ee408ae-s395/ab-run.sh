#!/bin/bash
# Run one A/B leg: fresh 4-validator devnet -> settle -> measure -> teardown.
# Usage: ab-run.sh <override-yml> <label> [windows] [window_secs]
set -euo pipefail

AB_DIR="$(cd "$(dirname "$0")" && pwd)"
OVERRIDE="$1"
LABEL="$2"
WINDOWS="${3:-3}"
WINDOW_SECS="${4:-60}"

COMPOSE=(docker compose -p ab
  -f /home/crab/projects/Torus-hyperBFT/devnet/docker-compose.yml
  -f "$AB_DIR/$OVERRIDE")

cleanup() { "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true; }
trap cleanup EXIT

"${COMPOSE[@]}" up -d validator-0 validator-1 validator-2 validator-3
python3 "$AB_DIR/ab-cadence.py" "$LABEL" "$WINDOWS" "$WINDOW_SECS"
