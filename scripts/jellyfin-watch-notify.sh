#!/usr/bin/env bash
set -euo pipefail

# Verplicht: zet je eigen ntfy-topic, bv. NTFY_TOPIC=https://ntfy.sh/mijn-topic
NTFY_TOPIC="${NTFY_TOPIC:?zet NTFY_TOPIC, bv. https://ntfy.sh/<jouw-topic>}"
# Komma-gescheiden Jellyfin-gebruikers die geen melding opleveren (bv. je eigen account).
EXCLUDE_USERS_RAW="${EXCLUDE_USERS:-}"
LOOKBACK_MINUTES="${LOOKBACK_MINUTES:-15}"
STATE_FILE="${STATE_FILE:-/tmp/jellyfin-watch-notify.state}"
PLAYBACK_DB="${PLAYBACK_DB:-/srv/jellyfin/config/data/playback_reporting.db}"
JELLY_DB="${JELLY_DB:-/srv/jellyfin/config/data/jellyfin.db}"

if [[ ! -f "$PLAYBACK_DB" || ! -f "$JELLY_DB" ]]; then
  exit 0
fi

mkdir -p "$(dirname "$STATE_FILE")"
last_hash=""
if [[ -f "$STATE_FILE" ]]; then
  last_hash="$(cat "$STATE_FILE" 2>/dev/null || true)"
fi

tmp_json="$(mktemp)"
python3 - <<'PY' "$PLAYBACK_DB" "$JELLY_DB" "$EXCLUDE_USERS_RAW" "$LOOKBACK_MINUTES" > "$tmp_json"
import json
import sqlite3
import sys
from datetime import datetime, timedelta, timezone

playback_db = sys.argv[1]
jelly_db = sys.argv[2]
exclude_raw = sys.argv[3]
lookback_minutes = int(sys.argv[4])

exclude = {x.strip().lower() for x in exclude_raw.split(',') if x.strip()}
now = datetime.now(timezone.utc).replace(tzinfo=None)
cutoff = now - timedelta(minutes=lookback_minutes)

users = {}
with sqlite3.connect(jelly_db) as con:
    cur = con.cursor()
    for uid, username in cur.execute("SELECT Id, Username FROM Users"):
        if uid and username:
            users[str(uid).lower()] = str(username)

rows = []
with sqlite3.connect(playback_db) as con:
    con.row_factory = sqlite3.Row
    cur = con.cursor()
    query = """
        SELECT rowid, DateCreated, UserId, ItemName, ItemType, PlaybackMethod, ClientName, DeviceName, PlayDuration
        FROM PlaybackActivity
        WHERE DateCreated IS NOT NULL
        ORDER BY rowid DESC
        LIMIT 600
    """
    for r in cur.execute(query):
        try:
            ts = datetime.fromisoformat(str(r["DateCreated"]).replace('Z', '+00:00').replace('+00:00', ''))
        except Exception:
            continue
        if ts < cutoff:
            continue
        uid = str(r["UserId"] or "").lower()
        username = users.get(uid, uid[:8] if uid else "unknown")
        if username.lower() in exclude:
            continue
        method = str(r["PlaybackMethod"] or "")
        rows.append({
            "ts": str(r["DateCreated"]),
            "username": username,
            "item": str(r["ItemName"] or "unknown"),
            "type": str(r["ItemType"] or "unknown"),
            "method": method,
            "transcode": "transcode" in method.lower(),
            "client": str(r["ClientName"] or "unknown"),
            "device": str(r["DeviceName"] or "unknown"),
            "duration": int(r["PlayDuration"] or 0),
        })

active_by_user = {}
for row in rows:
    key = row["username"]
    if key not in active_by_user:
        active_by_user[key] = row

transcode_count = sum(1 for x in rows if x["transcode"])

payload = {
    "active_count": len(active_by_user),
    "transcode_count": transcode_count,
    "lookback_minutes": lookback_minutes,
    "active": list(active_by_user.values())[:10],
}
print(json.dumps(payload, ensure_ascii=True))
PY

active_count="$(python3 - <<'PY' "$tmp_json"
import json,sys
with open(sys.argv[1], 'r', encoding='utf-8') as f:
    d=json.load(f)
print(d.get('active_count',0))
PY
)"

if [[ "$active_count" == "0" ]]; then
  rm -f "$tmp_json"
  exit 0
fi

resource_block="$(
  {
    echo "Host load: $(cut -d' ' -f1-3 /proc/loadavg 2>/dev/null || echo n/a)"
    awk '/MemTotal|MemAvailable/ {print $1" "$2}' /proc/meminfo 2>/dev/null | xargs -n2 | awk 'BEGIN{t=0;a=0} /MemTotal:/{t=$2} /MemAvailable:/{a=$2} END{if(t>0){u=t-a; printf("Host mem: %.1f/%.1f GiB (%.0f%%)\n",u/1048576,t/1048576,(u*100)/t)} else print "Host mem: n/a"}'
    df -h /srv/jellyfin 2>/dev/null | awk 'NR==2{print "Config fs: "$3"/"$2" ("$5")"}'
    df -h /mnt/jellyfin-transcodes 2>/dev/null | awk 'NR==2{print "Transcode fs: "$3"/"$2" ("$5")"}'
  } | sed '/^$/d'
)"

message="$(python3 - <<'PY' "$tmp_json" "$resource_block"
import json,sys
with open(sys.argv[1], 'r', encoding='utf-8') as f:
    d=json.load(f)
resource=sys.argv[2]
lines=[]
lines.append(f"Jellyfin activity ({d.get('lookback_minutes',15)}m window)")
lines.append(f"Active users: {d.get('active_count',0)} | Transcodes: {d.get('transcode_count',0)}")
lines.append("")
for row in d.get('active',[]):
    method=row.get('method','')
    marker='TRANSCODE' if row.get('transcode') else 'DIRECT'
    lines.append(f"- {row.get('username')} -> {row.get('item')} [{row.get('type')}] ({marker}; {method}; {row.get('client')}/{row.get('device')})")
if resource:
    lines.append("")
    lines.append(resource)
print("\n".join(lines))
PY
)"

new_hash="$(printf "%s" "$message" | shasum -a 256 | awk '{print $1}')"
if [[ "$new_hash" == "$last_hash" ]]; then
  rm -f "$tmp_json"
  exit 0
fi

curl -fsS -X POST "$NTFY_TOPIC" \
  -H "Title: Jellyfin watcher" \
  -H "Priority: default" \
  --data "$message" >/dev/null

printf "%s" "$new_hash" > "$STATE_FILE"
rm -f "$tmp_json"
