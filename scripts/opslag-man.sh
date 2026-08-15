#!/usr/bin/env bash
set -Eeuo pipefail

MEDIA_ROOT="${MEDIA_ROOT:-/Media}"
CONFIG_ROOT="${CONFIG_ROOT:-/srv/jellyfin/config}"
TRANSCODE_ROOT="${TRANSCODE_ROOT:-/mnt/jellyfin-transcodes}"
JELLYFIN_CONTAINER="${JELLYFIN_CONTAINER:-jellyfin}"
JELLYFIN_URL="${JELLYFIN_URL:-http://127.0.0.1:8096}"
JELLYFIN_API_KEY="${JELLYFIN_API_KEY:-}"
FORCE=false

BLUE='\033[0;34m'
RESET='\033[0m'

declare -A SIZE_CACHE=()

usage() {
  cat <<'EOF'
opslag-man: interactieve Jellyfin beheer + verwijdertool

Gebruik:
  opslag-man
  opslag-man --force
  opslag-man --media-root /Media --config-root /srv/jellyfin/config
  opslag-man --jellyfin-url http://127.0.0.1:8096 --jellyfin-api-key KEY

Flow:
  1) Toont live status (vaste snapshot, geen flikker)
  2) Kies Video of Serie
  3) Toont lijst
  4) Kies nummers (1 of 1,3,7)
  5) Verwijdert media + bijhorende Jellyfin metadata/cache

Tip:
  - Typ 'b' om terug te gaan naar vorige menu
  - Typ 'c' voor orphan clean mode (scan + verwijderen)
EOF
}

require_root() {
  if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
    echo "Draai als root/sudo: sudo opslag-man"
    exit 1
  fi
}

resolve_media_root() {
  local candidate
  local candidates=("$MEDIA_ROOT")
  local best_candidate=""
  local best_score=-1

  if docker ps --format '{{.Names}}' | grep -qx "$JELLYFIN_CONTAINER"; then
    while IFS= read -r src; do
      [[ -n "$src" ]] && candidates+=("$src")
    done < <(docker inspect "$JELLYFIN_CONTAINER" --format '{{range .Mounts}}{{if or (eq .Destination "/Media") (eq .Destination "/data")}}{{println .Source}}{{end}}{{end}}' 2>/dev/null || true)
  fi

  # Waar je media staat verschilt per opstelling. Docker-mounts worden hierboven
  # automatisch gelezen; deze lijst is de terugval. Eigen paden toevoegen kan met
  # MEDIA_ROOT_CANDIDATES="/pad/een:/pad/twee" (dubbelepunt-gescheiden).
  candidates+=(/Media /data)
  if [[ -n "${MEDIA_ROOT_CANDIDATES:-}" ]]; then
    IFS=':' read -r -a extra_roots <<< "$MEDIA_ROOT_CANDIDATES"
    candidates+=("${extra_roots[@]}")
  fi

  for candidate in "${candidates[@]}"; do
    [[ -n "$candidate" ]] || continue
    [[ -d "$candidate" ]] || continue

    local score=0
    local entries=0
    local films_present=0
    local series_present=0
    local df_total=""
    local fs_type=""

    [[ -d "$candidate/Films" || -d "$candidate/films" || -d "$candidate/Movies" || -d "$candidate/movies" ]] && films_present=1
    [[ -d "$candidate/Series" || -d "$candidate/series" || -d "$candidate/TV" || -d "$candidate/tv" || -d "$candidate/Shows" || -d "$candidate/shows" ]] && series_present=1

    (( films_present == 1 )) && score=$(( score + 60 ))
    (( series_present == 1 )) && score=$(( score + 60 ))

    entries=$(find "$candidate" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l | tr -d ' ')
    if [[ "${entries:-0}" =~ ^[0-9]+$ ]] && (( entries > 0 )); then
      score=$(( score + 20 ))
    fi

    df_total=$(df -B1 "$candidate" 2>/dev/null | awk 'NR==2{print $2}')
    if [[ -n "$df_total" && "$df_total" =~ ^[0-9]+$ ]]; then
      if (( df_total >= 500000000000 )); then
        score=$(( score + 40 ))
      elif (( df_total >= 100000000000 )); then
        score=$(( score + 20 ))
      fi
    fi

    fs_type=$(findmnt -T "$candidate" -n -o FSTYPE 2>/dev/null || true)
    [[ "$fs_type" == "nfs" || "$fs_type" == "nfs4" ]] && score=$(( score + 15 ))

    [[ "$candidate" == "$MEDIA_ROOT" ]] && score=$(( score + 3 ))

    if (( score > best_score )); then
      best_score=$score
      best_candidate="$candidate"
    fi
  done

  if [[ -n "$best_candidate" ]]; then
    echo "$best_candidate"
  else
    echo "$MEDIA_ROOT"
  fi
}

find_existing_subdir() {
  local base="$1"
  shift
  local name
  for name in "$@"; do
    [[ -d "$base/$name" ]] && { echo "$base/$name"; return 0; }
  done
  return 1
}

confirm() {
  local msg="$1"
  if $FORCE; then
    return 0
  fi
  read -r -p "$msg [y/N]: " ans
  [[ "$ans" == "y" || "$ans" == "Y" ]]
}

human_bytes() {
  local bytes="${1:-0}"
  if [[ -z "$bytes" || ! "$bytes" =~ ^[0-9]+$ ]]; then
    echo "onbekend"
    return 0
  fi

  local -a units=(B KB MB GB TB PB)
  local unit=0
  local value="$bytes"

  while (( value >= 1024 && unit < ${#units[@]} - 1 )); do
    value=$(( value / 1024 ))
    ((unit++))
  done

  echo "${value}${units[$unit]}"
}

path_size_human() {
  local target="$1"
  local bytes=""

  [[ -n "$target" && -e "$target" ]] || { echo "onbekend"; return 0; }

  if [[ -f "$target" ]]; then
    bytes=$(stat -c '%s' "$target" 2>/dev/null || true)
  elif [[ -d "$target" ]]; then
    bytes=$(du -sb "$target" 2>/dev/null | awk '{print $1}' || true)
  fi

  if [[ -z "$bytes" ]]; then
    du -sh "$target" 2>/dev/null | awk '{print $1}'
    return 0
  fi

  human_bytes "$bytes"
}

display_size_for_item() {
  local item_name="$1"
  local item_path="$2"
  local cache_key="${item_name}|${item_path}"

  if [[ -n "${SIZE_CACHE[$cache_key]:-}" ]]; then
    echo "${SIZE_CACHE[$cache_key]}"
    return 0
  fi

  local target=""
  local size="onbekend"

  if [[ -n "$item_path" ]]; then
    target="$(resolve_delete_target "$item_path")"
  fi

  if [[ -z "$target" ]]; then
    local fallback
    fallback=$(find_media_targets_by_name "$item_name" | head -n1 || true)
    target="$fallback"
  fi

  if [[ -n "$target" ]]; then
    size="$(path_size_human "$target")"
  fi

  SIZE_CACHE[$cache_key]="$size"
  echo "$size"
}

show_dashboard_core() {
  local media_root
  media_root="$(resolve_media_root)"

  echo
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  JELLYFIN LIVE STATUS"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  Tijd            : $(date '+%Y-%m-%d %H:%M:%S')"

  echo
  echo "[Container]"
  if docker ps --format '{{.Names}}' | grep -qx "$JELLYFIN_CONTAINER"; then
    docker ps --filter "name=^/${JELLYFIN_CONTAINER}$" --format '  Naam            : {{.Names}}\n  Status          : {{.Status}}\n  Ports           : {{.Ports}}'
    docker stats --no-stream --format '  Docker Stats    : CPU={{.CPUPerc}} | MEM={{.MemUsage}} | NET={{.NetIO}} | BLOCK={{.BlockIO}}' "$JELLYFIN_CONTAINER" 2>/dev/null || true
  else
    echo "  Container $JELLYFIN_CONTAINER draait niet."
  fi

  echo
  echo "[Host Resources]"
  echo "  Uptime          : $(uptime | sed 's/.*up //')"
  free -h 2>/dev/null || vm_stat 2>/dev/null || true

  echo
  echo "[Transcode]"
  if [[ -d "$TRANSCODE_ROOT" ]]; then
    echo "  Map             : $TRANSCODE_ROOT"
    echo "  Bestanden       : $(find "$TRANSCODE_ROOT" -type f 2>/dev/null | wc -l | tr -d ' ')"
    echo "  Grootte         : $(du -sh "$TRANSCODE_ROOT" 2>/dev/null | awk '{print $1}')"
    trans_df=$(df -h "$TRANSCODE_ROOT" 2>/dev/null | awk 'NR==2{print "tot=" $2 " used=" $3 " vrij=" $4 " use=" $5}')
    [[ -n "${trans_df:-}" ]] && echo "  Vrije opslag    : $trans_df"
  else
    echo "  Transcode map niet gevonden: $TRANSCODE_ROOT"
  fi

  if docker ps --format '{{.Names}}' | grep -qx "$JELLYFIN_CONTAINER"; then
    ffmpeg_count=$(docker exec "$JELLYFIN_CONTAINER" sh -lc "ps aux | grep -i '[f]fmpeg' | wc -l" 2>/dev/null | tr -d ' ' || echo 0)
    echo "  Actieve ffmpeg  : ${ffmpeg_count:-0}"
  fi

  echo
  echo "[Wie kijkt / connecties]"
  ss -tn state established '( sport = :8096 or dport = :8096 )' 2>/dev/null | awk 'NR>1 {print $5}' | cut -d: -f1 | sort -u | sed 's/^/  Client IP       : /' || true
  conn_count=$(ss -tn state established '( sport = :8096 or dport = :8096 )' 2>/dev/null | awk 'NR>1 {print $0}' | wc -l | tr -d ' ')
  echo "  Verbindingen    : ${conn_count:-0}"

  echo
  if [[ -d "$media_root" ]]; then
    media_entries=$(find "$media_root" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l | tr -d ' ')
    echo "[Media Mount]"
    echo "  Pad             : $media_root"
    echo "  Entries         : ${media_entries:-0}"
    media_df=$(df -h "$media_root" 2>/dev/null | awk 'NR==2{print "tot=" $2 " used=" $3 " vrij=" $4 " use=" $5}')
    [[ -n "${media_df:-}" ]] && echo "  Vrije opslag    : $media_df"

    media_df_bytes=$(df -B1 "$media_root" 2>/dev/null | awk 'NR==2{print "used_bytes=" $3 " vrij_bytes=" $4}')
    [[ -n "${media_df_bytes:-}" ]] && echo "  Echt gebruikt   : $media_df_bytes"

    films_dir="$(find_existing_subdir "$media_root" Films films Movies movies 2>/dev/null || true)"
    series_dir="$(find_existing_subdir "$media_root" Series series TV tv Shows shows 2>/dev/null || true)"

    if [[ -n "${films_dir:-}" ]]; then
      films_size=$(du -sh "$films_dir" 2>/dev/null | awk '{print $1}')
      [[ -n "${films_size:-}" ]] && echo "  Films grootte   : $films_size"
    fi
    if [[ -n "${series_dir:-}" ]]; then
      series_size=$(du -sh "$series_dir" 2>/dev/null | awk '{print $1}')
      [[ -n "${series_size:-}" ]] && echo "  Series grootte  : $series_size"
    fi
  fi
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
}

show_dashboard() {
  show_dashboard_core
}

list_items_from_db() {
  local media_kind="$1"   # Movie | Series
  local db_path="$CONFIG_ROOT/data/jellyfin.db"

  if [[ ! -f "$db_path" ]]; then
    return 0
  fi

  python3 - <<'PY' "$db_path" "$media_kind"
import sqlite3,sys

db=sys.argv[1]
media_kind=sys.argv[2]

wanted = {
    'Movie': 'MediaBrowser.Controller.Entities.Movies.Movie',
    'Series': 'MediaBrowser.Controller.Entities.TV.Series'
}[media_kind]

conn=sqlite3.connect(db)
cur=conn.cursor()
q='''
SELECT Id, Name, Path, Type
FROM BaseItems
WHERE Path IS NOT NULL
  AND Path <> ''
  AND Type = ?
GROUP BY Name, Path, Type
ORDER BY Name COLLATE NOCASE
'''

idx=1
for item_id,name,path,typ in cur.execute(q, (wanted,)):
    simple='Movie' if 'Movies.Movie' in typ else 'Series'
    print(f"{idx}|{item_id}|{simple}|{name}|{path}")
    idx+=1

conn.close()
PY
}

resolve_delete_target() {
  local item_path="$1"
  local candidate="$item_path"
  local media_root
  media_root="$(resolve_media_root)"

  if [[ -f "$candidate" ]]; then
    dirname "$candidate"
    return 0
  fi

  if [[ -d "$candidate" ]]; then
    echo "$candidate"
    return 0
  fi

  if [[ "$item_path" == /Media/* && "$media_root" != "/Media" ]]; then
    local mapped="${media_root}${item_path#/Media}"
    if [[ -f "$mapped" ]]; then
      dirname "$mapped"
      return 0
    fi
    if [[ -d "$mapped" ]]; then
      echo "$mapped"
      return 0
    fi
  fi

  if [[ "$item_path" == /data/* && "$media_root" != "/data" ]]; then
    local mapped_data="${media_root}${item_path#/data}"
    if [[ -f "$mapped_data" ]]; then
      dirname "$mapped_data"
      return 0
    fi
    if [[ -d "$mapped_data" ]]; then
      echo "$mapped_data"
      return 0
    fi
  fi

  if [[ "$item_path" == /mnt/nfs/jelli/* && "$media_root" != "/mnt/nfs/jelli" ]]; then
    local mapped_nfs="${media_root}${item_path#/mnt/nfs/jelli}"
    if [[ -f "$mapped_nfs" ]]; then
      dirname "$mapped_nfs"
      return 0
    fi
    if [[ -d "$mapped_nfs" ]]; then
      echo "$mapped_nfs"
      return 0
    fi
  fi

  if [[ "$item_path" == /mnt/JELLI/jelli/* && "$media_root" != "/mnt/JELLI/jelli" ]]; then
    local mapped_jelli="${media_root}${item_path#/mnt/JELLI/jelli}"
    if [[ -f "$mapped_jelli" ]]; then
      dirname "$mapped_jelli"
      return 0
    fi
    if [[ -d "$mapped_jelli" ]]; then
      echo "$mapped_jelli"
      return 0
    fi
  fi

  if [[ -n "$media_root" ]]; then
    local by_basename="$media_root/$(basename "$item_path")"
    if [[ -d "$by_basename" ]]; then
      echo "$by_basename"
      return 0
    fi
    if [[ -f "$by_basename" ]]; then
      dirname "$by_basename"
      return 0
    fi
  fi

  if [[ "$item_path" == /Media/* && "$MEDIA_ROOT" != "/Media" ]]; then
    local mapped="${MEDIA_ROOT}${item_path#/Media}"
    if [[ -f "$mapped" ]]; then
      dirname "$mapped"
      return 0
    fi
    if [[ -d "$mapped" ]]; then
      echo "$mapped"
      return 0
    fi
  fi

  if docker ps --format '{{.Names}}' | grep -qx "$JELLYFIN_CONTAINER"; then
    while IFS='|' read -r src dst; do
      [[ -z "$src" || -z "$dst" ]] && continue
      if [[ "$item_path" == "$dst"* ]]; then
        local maybe="${src}${item_path#${dst}}"
        if [[ -f "$maybe" ]]; then
          dirname "$maybe"
          return 0
        fi
        if [[ -d "$maybe" ]]; then
          echo "$maybe"
          return 0
        fi
      fi
    done < <(docker inspect "$JELLYFIN_CONTAINER" --format '{{range .Mounts}}{{printf "%s|%s\n" .Source .Destination}}{{end}}' 2>/dev/null || true)
  fi

  echo ""
}

delete_related_data() {
  local typed_name="$1"
  local item_id="$2"
  local cleaned
  cleaned=$(echo "$typed_name" | tr '[:upper:]' '[:lower:]')

  echo
  echo "Zoek en verwijder bijhorende metadata/cache voor: $typed_name"

  if [[ -d "$CONFIG_ROOT/metadata" ]]; then
    find "$CONFIG_ROOT/metadata" -iname "*${cleaned}*" -print -exec rm -rf {} + 2>/dev/null || true
    if [[ -n "$item_id" ]]; then
      find "$CONFIG_ROOT/metadata" -iname "*${item_id}*" -print -exec rm -rf {} + 2>/dev/null || true
    fi
  fi

  if [[ -d "$CONFIG_ROOT/cache" ]]; then
    find "$CONFIG_ROOT/cache" -iname "*${cleaned}*" -print -exec rm -rf {} + 2>/dev/null || true
    if [[ -n "$item_id" ]]; then
      find "$CONFIG_ROOT/cache" -iname "*${item_id}*" -print -exec rm -rf {} + 2>/dev/null || true
    fi
  fi

  if [[ -d "$CONFIG_ROOT/log" ]]; then
    find "$CONFIG_ROOT/log" -type f -iname "*${cleaned}*" -print -delete 2>/dev/null || true
    if [[ -n "$item_id" ]]; then
      find "$CONFIG_ROOT/log" -type f -iname "*${item_id}*" -print -delete 2>/dev/null || true
    fi
  fi
}

purge_stale_item_from_db() {
  local item_id="$1"
  local db_path="$CONFIG_ROOT/data/jellyfin.db"

  if [[ ! -f "$db_path" || -z "$item_id" ]]; then
    return 0
  fi

  python3 - <<'PY' "$db_path" "$item_id"
import sqlite3,sys

db_path=sys.argv[1]
item_id=sys.argv[2]

conn=sqlite3.connect(db_path)
cur=conn.cursor()

tables=[r[0] for r in cur.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall()]

for table in tables:
    cols=[c[1] for c in cur.execute(f"PRAGMA table_info({table})").fetchall()]
    try:
        if 'ItemId' in cols:
            cur.execute(f"DELETE FROM {table} WHERE ItemId = ?", (item_id,))
        if 'AncestorId' in cols:
            cur.execute(f"DELETE FROM {table} WHERE AncestorId = ?", (item_id,))
    except Exception:
        pass

cur.execute("DELETE FROM BaseItems WHERE Id = ?", (item_id,))
try:
  cur.execute("VACUUM")
except Exception:
  pass
conn.commit()
conn.close()
print("DB cleanup done for item:", item_id)
PY
}

count_hardlinked_files() {
  local target="$1"
  if [[ -f "$target" ]]; then
    local links
    links=$(stat -c '%h' "$target" 2>/dev/null || echo 1)
    if [[ "$links" =~ ^[0-9]+$ ]] && (( links > 1 )); then
      echo 1
    else
      echo 0
    fi
    return 0
  fi
  [[ -d "$target" ]] || { echo 0; return 0; }
  find "$target" -type f -links +1 2>/dev/null | wc -l | tr -d ' '
}

find_hardlink_siblings() {
  local target="$1"
  local media_root="$2"
  local inode
  local -A seen=()

  [[ -e "$target" ]] || return 0
  [[ -d "$media_root" ]] || return 0

  local inode_source_cmd
  if [[ -f "$target" ]]; then
    inode_source_cmd="stat -c '%i' '$target' 2>/dev/null"
  else
    inode_source_cmd="find '$target' -xdev -type f -links +1 -printf '%i\\n' 2>/dev/null | sort -u"
  fi

  while IFS= read -r inode; do
    [[ -n "$inode" ]] || continue
    while IFS= read -r sibling; do
      [[ -n "$sibling" ]] || continue
      if [[ -d "$target" && "$sibling" == "$target"/* ]]; then
        continue
      fi
      if [[ -f "$target" && "$sibling" == "$target" ]]; then
        continue
      fi
      if [[ -z "${seen[$sibling]:-}" ]]; then
        seen[$sibling]=1
        echo "$sibling"
      fi
    done < <(find "$media_root" -xdev -type f -inum "$inode" 2>/dev/null || true)
  done < <(eval "$inode_source_cmd")
}

cleanup_empty_parent_dirs() {
  local root="$1"
  local path="$2"
  local current

  [[ -n "$root" && -d "$root" ]] || return 0
  [[ -n "$path" ]] || return 0

  current="$(dirname "$path")"
  while [[ -n "$current" && "$current" != "/" ]]; do
    [[ "$current" == "$root" ]] && break
    if [[ "$current" == "$root"/* ]]; then
      rmdir "$current" 2>/dev/null || break
      current="$(dirname "$current")"
    else
      break
    fi
  done
}

delete_target_and_hardlinks() {
  local target="$1"
  local media_root="$2"
  local siblings
  local sibling_count=0
  local deleted_siblings=0
  local target_kind="bestand"

  [[ -e "$target" ]] || { echo "Pad bestaat niet meer: $target"; return 0; }
  [[ -d "$target" ]] && target_kind="map"

  siblings="$(find_hardlink_siblings "$target" "$media_root" | awk '!seen[$0]++')"
  if [[ -n "$siblings" ]]; then
    sibling_count=$(echo "$siblings" | sed '/^$/d' | wc -l | tr -d ' ')
    echo "Hardlink siblings gevonden buiten doelmap: ${sibling_count:-0}"
  fi

  if [[ -d "$target" ]]; then
    rm -rf -- "$target"
  else
    rm -f -- "$target"
  fi
  echo "Verwijderd ${target_kind}: $target"

  if [[ -n "$siblings" ]]; then
    while IFS= read -r sibling; do
      [[ -z "$sibling" ]] && continue
      if [[ -f "$sibling" || -L "$sibling" ]]; then
        rm -f -- "$sibling"
        deleted_siblings=$(( deleted_siblings + 1 ))
        cleanup_empty_parent_dirs "$media_root" "$sibling"
      fi
    done <<< "$siblings"
    echo "Extra hardlink-bestanden verwijderd: $deleted_siblings"
  fi
}

build_referenced_paths() {
  local out_file="$1"
  local db_path="$CONFIG_ROOT/data/jellyfin.db"

  : > "$out_file"

  if [[ -n "$JELLYFIN_API_KEY" ]]; then
  python3 - <<'PY' "$JELLYFIN_URL" "$JELLYFIN_API_KEY" "$out_file"
import json
import sys
import urllib.parse
import urllib.request

base_url=sys.argv[1].rstrip('/')
api_key=sys.argv[2]
out_file=sys.argv[3]

params={
  'Recursive': 'true',
  'Fields': 'Path',
  'IncludeItemTypes': 'Movie,Series,Season,Episode',
  'api_key': api_key,
  'Limit': '200000'
}
url=f"{base_url}/Items?{urllib.parse.urlencode(params)}"

paths=[]
try:
  with urllib.request.urlopen(url, timeout=30) as resp:
    payload=json.load(resp)
  for item in payload.get('Items', []):
    p=item.get('Path')
    if p:
      paths.append(p)
except Exception:
  paths=[]

with open(out_file, 'a', encoding='utf-8') as fh:
  for p in sorted(set(paths)):
    fh.write(p + '\n')
PY
  fi

  if [[ ! -s "$out_file" && -f "$db_path" ]]; then
  python3 - <<'PY' "$db_path" "$out_file"
import sqlite3
import sys

db=sys.argv[1]
out=sys.argv[2]

conn=sqlite3.connect(db)
cur=conn.cursor()
query='''
SELECT Path
FROM BaseItems
WHERE Path IS NOT NULL
  AND Path <> ''
  AND (
  Type='MediaBrowser.Controller.Entities.Movies.Movie'
  OR Type='MediaBrowser.Controller.Entities.TV.Series'
  OR Type='MediaBrowser.Controller.Entities.TV.Season'
  OR Type='MediaBrowser.Controller.Entities.TV.Episode'
  )
'''

paths=sorted({r[0] for r in cur.execute(query).fetchall() if r[0]})
with open(out, 'a', encoding='utf-8') as fh:
  for p in paths:
    fh.write(p + '\n')

conn.close()
PY
  fi

  sort -u "$out_file" -o "$out_file"
}

run_orphan_clean_mode() {
  local media_root
  media_root="$(resolve_media_root)"

  local films_dir
  local series_dir
  local downloads_dir
  films_dir="$(find_existing_subdir "$media_root" Films films Movies movies 2>/dev/null || true)"
  series_dir="$(find_existing_subdir "$media_root" Series series TV tv Shows shows 2>/dev/null || true)"
  downloads_dir="$(find_existing_subdir "$media_root" downloads Downloads 2>/dev/null || true)"

  if [[ -z "$films_dir" && -z "$series_dir" && -z "$downloads_dir" ]]; then
  echo "Geen media mappen gevonden onder $media_root"
  return 0
  fi

  local clean_scope=""
  echo
  echo "Clean scope kiezen:"
  echo "  d) Alleen downloads orphan cleanup (aanbevolen)"
  echo "  a) Veilige leftovers cleanup (films/series/downloads)"
  echo "     - verwijdert GEEN normale films/series met videobestand"
  echo "  x) Agressieve full cleanup (films + series + downloads)"
  echo "  b) Terug"
  if $FORCE; then
    clean_scope="d"
  else
    read -r -p "Keuze [d/a/x/b]: " clean_scope
  fi

  case "$clean_scope" in
    d|D|"")
      ;;
    a|A)
      ;;
    x|X)
      ;;
    b|B)
      echo "Clean mode geannuleerd."
      return 0
      ;;
    *)
      echo "Ongeldige keuze, standaard naar downloads-only."
      clean_scope="d"
      ;;
  esac

  local refs_file candidates_file verify_file
  refs_file="$(mktemp /tmp/opslag-man-refs.XXXXXX)"
  candidates_file="$(mktemp /tmp/opslag-man-orphans.XXXXXX)"
  verify_file="$(mktemp /tmp/opslag-man-orphans-after.XXXXXX)"

  build_referenced_paths "$refs_file"

  if [[ ! -s "$refs_file" ]]; then
  echo "Kon geen Jellyfin referenties ophalen (API/DB)."
  rm -f "$refs_file" "$candidates_file" "$verify_file"
  return 1
  fi

  python3 - <<'PY' "$refs_file" "$films_dir" "$series_dir" "$downloads_dir" "$candidates_file" "$media_root" "$clean_scope"
import os
import sys

refs_file, films_dir, series_dir, downloads_dir, out_file, media_root, clean_scope = sys.argv[1:8]

alias_prefixes=[]
for p in [media_root, '/Media', '/data', '/mnt/nfs/jelli', '/mnt/JELLI/jelli']:
  if p and p not in alias_prefixes:
    alias_prefixes.append(p)

def expand_aliases(path):
  path=os.path.normpath(path)
  out={path}
  for src in alias_prefixes:
    srcn=os.path.normpath(src)
    if path == srcn:
      for dst in alias_prefixes:
        out.add(os.path.normpath(dst))
    elif path.startswith(srcn + os.sep):
      suffix=path[len(srcn):]
      for dst in alias_prefixes:
        out.add(os.path.normpath(dst + suffix))
  return out

refs=[]
with open(refs_file, 'r', encoding='utf-8', errors='ignore') as fh:
  for line in fh:
    p=line.strip()
    if p:
      refs.extend(list(expand_aliases(p)))

refs=sorted({os.path.normpath(r).lower() for r in refs if r})

def is_referenced(path):
  p=os.path.normpath(path).lower()
  for r in refs:
    if r == p or r.startswith(p + os.sep) or p.startswith(r + os.sep):
      return True
  return False

def size_bytes(path):
  total=0
  try:
    if os.path.isfile(path):
      return os.path.getsize(path)
    for root, dirs, files in os.walk(path, followlinks=False):
      for f in files:
        fp=os.path.join(root, f)
        try:
          total += os.path.getsize(fp)
        except Exception:
          pass
  except Exception:
    return 0
  return total

VIDEO_EXTS={'.mkv','.mp4','.avi','.m4v','.mov','.wmv','.ts','.m2ts','.iso','.webm'}

def has_video_content(path):
  try:
    if os.path.isfile(path):
      return os.path.splitext(path)[1].lower() in VIDEO_EXTS
    for root, dirs, files in os.walk(path, followlinks=False):
      for f in files:
        if os.path.splitext(f)[1].lower() in VIDEO_EXTS:
          return True
  except Exception:
    return False
  return False

def has_hardlinks_outside(entry, downloads_dir):
  if not downloads_dir or not os.path.isdir(downloads_dir):
    return False
  inodes=set()
  try:
    if os.path.isfile(entry):
      st=os.stat(entry)
      if st.st_nlink > 1:
        inodes.add(st.st_ino)
    else:
      for root, dirs, files in os.walk(entry, followlinks=False):
        for f in files:
          fp=os.path.join(root, f)
          try:
            st=os.stat(fp)
          except Exception:
            continue
          if st.st_nlink > 1:
            inodes.add(st.st_ino)
  except Exception:
    return False

  if not inodes:
    return False

  for root, dirs, files in os.walk(downloads_dir, followlinks=False):
    for f in files:
      fp=os.path.join(root, f)
      try:
        st=os.stat(fp)
      except Exception:
        continue
      if st.st_ino in inodes:
        return True
  return False

candidates=[]

scan_targets=[]
if clean_scope.lower() in ('a','x'):
  scan_targets=[(films_dir, 'films'), (series_dir, 'series'), (downloads_dir, 'downloads')]
else:
  scan_targets=[(downloads_dir, 'downloads')]

for base, kind in scan_targets:
  if not base or not os.path.isdir(base):
    continue
  try:
    entries=[os.path.join(base, e) for e in os.listdir(base)]
  except Exception:
    continue
  for entry in sorted(entries):
    if not os.path.exists(entry):
      continue
    if is_referenced(entry):
      continue

    if clean_scope.lower() == 'a' and kind in ('films','series'):
      if has_video_content(entry) and not has_hardlinks_outside(entry, downloads_dir):
        continue

    candidates.append((kind, entry, size_bytes(entry), 'not_referenced_by_jellyfin'))

with open(out_file, 'w', encoding='utf-8') as out:
  for kind, path, sz, reason in sorted(candidates, key=lambda x: x[2], reverse=True):
    out.write(f"{kind}|{path}|{sz}|{reason}\n")
PY

  local count
  count=$(wc -l < "$candidates_file" | tr -d ' ')

  if [[ "$count" == "0" ]]; then
  echo "Geen orphan media gevonden."
  rm -f "$refs_file" "$candidates_file" "$verify_file"
  return 0
  fi

  echo
  echo "Orphan media (niet in Jellyfin referenties):"
  awk -F'|' '{printf "  [%d] \033[0;34m%s\033[0m | %s | %s\n", NR, $2, $1, $3}' "$candidates_file"

  local before_bytes after_bytes reclaimed
  before_bytes=$(df -B1 "$media_root" 2>/dev/null | awk 'NR==2{print $4}')

  if ! confirm "Orphan clean uitvoeren en ALLE bovenstaande paden verwijderen?"; then
  echo "Clean mode geannuleerd."
  rm -f "$refs_file" "$candidates_file" "$verify_file"
  return 0
  fi

  while IFS='|' read -r kind path sz reason; do
  [[ -z "$path" ]] && continue
  echo "Verwijder: $path"
  if [[ -e "$path" ]]; then
    delete_target_and_hardlinks "$path" "$media_root"
  else
    rm -rf -- "$path"
  fi
  done < "$candidates_file"

  sync

  python3 - <<'PY' "$refs_file" "$films_dir" "$series_dir" "$downloads_dir" "$verify_file" "$media_root" "$clean_scope"
import os
import sys

refs_file, films_dir, series_dir, downloads_dir, out_file, media_root, clean_scope = sys.argv[1:8]

alias_prefixes=[]
for p in [media_root, '/Media', '/data', '/mnt/nfs/jelli', '/mnt/JELLI/jelli']:
  if p and p not in alias_prefixes:
    alias_prefixes.append(p)

def expand_aliases(path):
  path=os.path.normpath(path)
  out={path}
  for src in alias_prefixes:
    srcn=os.path.normpath(src)
    if path == srcn:
      for dst in alias_prefixes:
        out.add(os.path.normpath(dst))
    elif path.startswith(srcn + os.sep):
      suffix=path[len(srcn):]
      for dst in alias_prefixes:
        out.add(os.path.normpath(dst + suffix))
  return out

refs=[]
with open(refs_file, 'r', encoding='utf-8', errors='ignore') as fh:
  for line in fh:
    p=line.strip()
    if p:
      refs.extend(list(expand_aliases(p)))

refs=sorted({os.path.normpath(r).lower() for r in refs if r})

def is_referenced(path):
  p=os.path.normpath(path).lower()
  for r in refs:
    if r == p or r.startswith(p + os.sep) or p.startswith(r + os.sep):
      return True
  return False

def size_bytes(path):
  total=0
  try:
    if os.path.isfile(path):
      return os.path.getsize(path)
    for root, dirs, files in os.walk(path, followlinks=False):
      for f in files:
        fp=os.path.join(root, f)
        try:
          total += os.path.getsize(fp)
        except Exception:
          pass
  except Exception:
    return 0
  return total

VIDEO_EXTS={'.mkv','.mp4','.avi','.m4v','.mov','.wmv','.ts','.m2ts','.iso','.webm'}

def has_video_content(path):
  try:
    if os.path.isfile(path):
      return os.path.splitext(path)[1].lower() in VIDEO_EXTS
    for root, dirs, files in os.walk(path, followlinks=False):
      for f in files:
        if os.path.splitext(f)[1].lower() in VIDEO_EXTS:
          return True
  except Exception:
    return False
  return False

def has_hardlinks_outside(entry, downloads_dir):
  if not downloads_dir or not os.path.isdir(downloads_dir):
    return False
  inodes=set()
  try:
    if os.path.isfile(entry):
      st=os.stat(entry)
      if st.st_nlink > 1:
        inodes.add(st.st_ino)
    else:
      for root, dirs, files in os.walk(entry, followlinks=False):
        for f in files:
          fp=os.path.join(root, f)
          try:
            st=os.stat(fp)
          except Exception:
            continue
          if st.st_nlink > 1:
            inodes.add(st.st_ino)
  except Exception:
    return False

  if not inodes:
    return False

  for root, dirs, files in os.walk(downloads_dir, followlinks=False):
    for f in files:
      fp=os.path.join(root, f)
      try:
        st=os.stat(fp)
      except Exception:
        continue
      if st.st_ino in inodes:
        return True
  return False

candidates=[]

scan_targets=[]
if clean_scope.lower() in ('a','x'):
  scan_targets=[(films_dir, 'films'), (series_dir, 'series'), (downloads_dir, 'downloads')]
else:
  scan_targets=[(downloads_dir, 'downloads')]

for base, kind in scan_targets:
  if not base or not os.path.isdir(base):
    continue
  try:
    entries=[os.path.join(base, e) for e in os.listdir(base)]
  except Exception:
    continue
  for entry in sorted(entries):
    if not os.path.exists(entry):
      continue
    if is_referenced(entry):
      continue

    if clean_scope.lower() == 'a' and kind in ('films','series'):
      if has_video_content(entry) and not has_hardlinks_outside(entry, downloads_dir):
        continue

    candidates.append((kind, entry, size_bytes(entry), 'still_not_referenced'))

with open(out_file, 'w', encoding='utf-8') as out:
  for kind, path, sz, reason in sorted(candidates, key=lambda x: x[2], reverse=True):
    out.write(f"{kind}|{path}|{sz}|{reason}\n")
PY

  after_bytes=$(df -B1 "$media_root" 2>/dev/null | awk 'NR==2{print $4}')
  reclaimed=$(( after_bytes - before_bytes ))

  echo
  echo "Clean mode klaar."
  echo "  Vrije bytes voor : $before_bytes"
  echo "  Vrije bytes na   : $after_bytes"
  echo "  Teruggewonnen    : $reclaimed"

  local remain
  remain=$(wc -l < "$verify_file" | tr -d ' ')
  if [[ "$remain" != "0" ]]; then
  echo "  Let op: er zijn nog $remain orphan entries over:"
  awk -F'|' '{printf "    - %s (%s bytes)\n", $2, $3}' "$verify_file"
  else
  echo "  Verificatie: geen orphan entries over."
  fi

  rm -f "$refs_file" "$candidates_file" "$verify_file"
}

get_media_roots() {
  local media_root
  media_root="$(resolve_media_root)"
  echo "$media_root"

  if docker ps --format '{{.Names}}' | grep -qx "$JELLYFIN_CONTAINER"; then
    docker inspect "$JELLYFIN_CONTAINER" --format '{{range .Mounts}}{{if or (eq .Destination "/Media") (eq .Destination "/data")}}{{println .Source}}{{end}}{{end}}' 2>/dev/null || true
  fi
}

find_media_targets_by_name() {
  local item_name="$1"
  while IFS= read -r root; do
    [[ -z "$root" ]] && continue
    [[ -d "$root" ]] || continue
    find "$root" -maxdepth 5 \( -type d -o -type f \) -iname "*${item_name}*" 2>/dev/null || true
  done < <(get_media_roots | awk '!seen[$0]++')
}

choose_type() {
  echo
  echo "Kies type:"
  echo "  1) Video"
  echo "  2) Serie"
  echo "  c) Clean mode (orphans)"
  echo "  b) Terug/stop"
  read -r -p "Keuze [1/2/c/b]: " type_choice

  case "$type_choice" in
    1) media_kind="Movie" ;;
    2) media_kind="Series" ;;
    c|C) media_kind="__CLEAN__" ;;
    b|B) media_kind="__BACK__" ;;
    *) media_kind="" ;;
  esac
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --media-root) MEDIA_ROOT="${2:-}"; shift ;;
    --config-root) CONFIG_ROOT="${2:-}"; shift ;;
    --transcode-root) TRANSCODE_ROOT="${2:-}"; shift ;;
    --container) JELLYFIN_CONTAINER="${2:-}"; shift ;;
    --jellyfin-url) JELLYFIN_URL="${2:-}"; shift ;;
    --jellyfin-api-key) JELLYFIN_API_KEY="${2:-}"; shift ;;
    --force) FORCE=true ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Onbekende optie: $1"; usage; exit 1 ;;
  esac
  shift
done

require_root
show_dashboard

while true; do
  media_kind=""
  choose_type

  if [[ "$media_kind" == "__BACK__" ]]; then
    echo "Afsluiten."
    exit 0
  fi

  if [[ "$media_kind" == "__CLEAN__" ]]; then
    run_orphan_clean_mode
    continue
  fi

  if [[ -z "$media_kind" ]]; then
    echo "Ongeldige keuze."
    continue
  fi

  all_items=$(list_items_from_db "$media_kind")
  if [[ -z "$all_items" ]]; then
    echo "Geen $media_kind items gevonden in Jellyfin database."
    continue
  fi

  echo
  printf 'Beschikbare %s items:\n' "$media_kind"
  while IFS='|' read -r idx item_id item_type item_name item_path; do
    [[ -z "$idx" ]] && continue
    item_size="$(display_size_for_item "$item_name" "$item_path")"
    printf "  [%s] ${BLUE}%s${RESET}  (%s)\n" "$idx" "$item_name" "$item_size"
  done <<< "$all_items"

  echo
  read -r -p "Kies nummer(s) om te verwijderen (bv: 1 of 1,3,7 | b=terug): " picks_raw
  if [[ "$picks_raw" =~ ^[bB]$ ]]; then
    continue
  fi
  if [[ -z "$picks_raw" ]]; then
    echo "Geen nummers ingevuld."
    continue
  fi

  pick_list=$(echo "$picks_raw" | tr ',' ' ' | xargs)
  if [[ -z "$pick_list" ]]; then
    echo "Geen geldige nummers ingevuld."
    continue
  fi

  selected_lines=""
  valid=true
  for pick in $pick_list; do
    if ! [[ "$pick" =~ ^[0-9]+$ ]]; then
      echo "Ongeldig nummer: $pick"
      valid=false
      break
    fi

    line=$(echo "$all_items" | awk -F'|' -v p="$pick" '$1==p{print $0}')
    if [[ -z "$line" ]]; then
      echo "Nummer niet gevonden: $pick"
      valid=false
      break
    fi
    selected_lines+="$line"$'\n'
  done

  if ! $valid; then
    continue
  fi

  echo
  echo "Te verwijderen items:"
  echo "$selected_lines" | awk -F'|' 'NF>=5 {printf "  [%s] %s | %s\n", $1, $3, $4}'

  if ! confirm "Definitief verwijderen (media + bijhorende data) voor alle geselecteerde items?"; then
    echo "Geannuleerd."
    continue
  fi

  while IFS= read -r line; do
    [[ -z "$line" ]] && continue

    item_id=$(echo "$line" | awk -F'|' '{print $2}')
    item_type=$(echo "$line" | awk -F'|' '{print $3}')
    item_name=$(echo "$line" | awk -F'|' '{print $4}')
    item_path=$(echo "$line" | awk -F'|' '{print $5}')
    delete_target=$(resolve_delete_target "$item_path")

    echo
    echo "--- Verwerken: $item_type | $item_name ---"
    echo "DB pad: $item_path"
    if [[ -n "$delete_target" ]]; then
      echo "Doelmap op host: $delete_target"
    else
      echo "Doelmap op host: (niet gevonden)"
    fi

    if [[ -n "$delete_target" && -d "$delete_target" ]]; then
      hardlinked_count="$(count_hardlinked_files "$delete_target")"
      if [[ "${hardlinked_count:-0}" =~ ^[0-9]+$ ]] && (( hardlinked_count > 0 )); then
        echo "Waarschuwing: $hardlinked_count bestand(en) hebben hardlinks."
        echo "Ik verwijder ook de andere hardlink(s) binnen de media mount zodat ruimte echt vrijkomt."
      fi
      delete_target_and_hardlinks "$delete_target" "$(resolve_media_root)"
    else
      echo "Kon media pad niet verwijderen: pad niet bereikbaar op host."
      echo "Zoek fallback targets op naam in mediabronnen..."
      fallback_targets="$(find_media_targets_by_name "$item_name" | awk '!seen[$0]++')"
      if [[ -n "$fallback_targets" ]]; then
        echo "Fallback targets gevonden:"
        echo "$fallback_targets" | sed 's/^/  - /'
        if confirm "Deze fallback targets ook verwijderen?"; then
          while IFS= read -r target; do
            [[ -z "$target" ]] && continue
            rm -rf -- "$target"
            echo "Verwijderd (fallback): $target"
          done <<< "$fallback_targets"
        fi
      else
        echo "Geen fallback targets gevonden op host."
      fi
      echo "Ik verwijder wel bijhorende Jellyfin metadata/cache + stale DB item."
    fi

    purge_stale_item_from_db "$item_id"
    delete_related_data "$item_name" "$item_id"
  done <<< "$selected_lines"

  echo
  read -r -p "Nog items verwijderen? [Y/n]: " again
  if [[ "$again" =~ ^[Nn]$ ]]; then
    break
  fi
done

echo
if docker ps --format '{{.Names}}' | grep -qx "$JELLYFIN_CONTAINER"; then
  echo "Tip: start eventueel handmatig een library scan in Jellyfin voor directe sync."
fi

echo "Klaar."
