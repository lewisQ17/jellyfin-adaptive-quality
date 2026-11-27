#!/usr/bin/env bash
# Herhaalt de opname van de originele bash-scripts (lokaal; NTFY_TOPIC wijst naar de effect-sink; df is een shim).
set -euo pipefail
rm -rf rust/app-cases
exec rust-anything app record . --inputs rust/jelly-inputs.jsonl --no-traps --timeout 60 \
 --effect-env NTFY_TOPIC=http-url \
 --db-reset 'python3 rust/tools/seed_db.py' \
 --normalize 'scriptpath=/[^ :]*/jellyfin-watch-notify\.sh' \
 -- bash rust/tools/orig.sh
