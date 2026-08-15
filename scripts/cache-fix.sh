#!/usr/bin/env bash
set -Eeuo pipefail

DRY_RUN=false
KEEP_LOG_DAYS=7
KEEP_TRANSCODE_COUNT=2
CACHE_DIR="/srv/jellyfin/cache"
TRANSCODE_DIR="/mnt/jellyfin-transcodes"
LOG_DIR="/srv/jellyfin/config/log"

usage() {
  cat <<'EOF'
Gebruik:
  cache-fix.sh [--dry-run] [--keep-log-days N] [--keep-transcode-count N]

Wat doet dit script:
  - Houdt enkel de N nieuwste transcode-bestanden bij (standaard 2)
  - Verwijdert oude Jellyfin logs
  - Verwijdert tijdelijke cache-bestanden
  - Toont ruimtewinst
EOF
}

run_cmd() {
  if $DRY_RUN; then
    echo "[dry-run] $*"
  else
    eval "$@"
  fi
}

require_root() {
  if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
    echo "Dit script moet als root draaien (sudo)."
    exit 1
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=true ;;
    --keep-log-days)
      KEEP_LOG_DAYS="${2:-}"; shift ;;
    --keep-transcode-count)
      KEEP_TRANSCODE_COUNT="${2:-}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Onbekende optie: $1"; usage; exit 1 ;;
  esac
  shift
done

require_root

echo "== Vooraf ruimte =="
df -h /

if [[ -d "$TRANSCODE_DIR" ]]; then
  mapfile -t transcodes < <(find "$TRANSCODE_DIR" -type f -printf '%T@|%p\n' | sort -nr)
  count=${#transcodes[@]}
  echo "Transcode files gevonden: $count"

  if (( count > KEEP_TRANSCODE_COUNT )); then
    for (( i=KEEP_TRANSCODE_COUNT; i<count; i++ )); do
      file_path="${transcodes[$i]#*|}"
      if $DRY_RUN; then
        echo "[dry-run] delete $file_path"
      else
        echo "$file_path"
        rm -f -- "$file_path"
      fi
    done
    run_cmd "find '$TRANSCODE_DIR' -type d -empty -print -delete"
  else
    echo "Geen oude transcodes om te verwijderen (keep=$KEEP_TRANSCODE_COUNT)."
  fi
else
  echo "Transcode map niet gevonden: $TRANSCODE_DIR"
fi

if [[ -d "$LOG_DIR" ]]; then
  run_cmd "find '$LOG_DIR' -type f -name 'log_*.log' -mtime +$KEEP_LOG_DAYS -print -delete"
  run_cmd "find '$LOG_DIR' -type f -name 'FFmpeg*.log' -mtime +$KEEP_LOG_DAYS -print -delete"
else
  echo "Log map niet gevonden: $LOG_DIR"
fi

if [[ -d "$CACHE_DIR" ]]; then
  run_cmd "find '$CACHE_DIR' -type f -mmin +360 -print -delete"
  run_cmd "find '$CACHE_DIR' -type d -empty -print -delete"
else
  echo "Cache map niet gevonden: $CACHE_DIR"
fi

echo
echo "== Nadien ruimte =="
df -h /

echo "Klaar."
