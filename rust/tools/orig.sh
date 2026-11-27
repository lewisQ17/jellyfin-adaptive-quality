#!/usr/bin/env bash
# Opname-hulp: draait een van de originele scripts onder de subcommando-naam die de Rust-CLI ook gebruikt.
dir="$(cd "$(dirname "$0")/../.." && pwd)"
cmd="${1:-}"
shift || true
case "$cmd" in
  watch-notify) exec bash "$dir/scripts/jellyfin-watch-notify.sh" "$@" ;;
  cache-fix) exec bash "$dir/scripts/cache-fix.sh" "$@" ;;
  *) echo "gebruik: orig.sh watch-notify|cache-fix [opties]" >&2; exit 2 ;;
esac
