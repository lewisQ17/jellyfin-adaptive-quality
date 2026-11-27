"""Maakt synthetische Jellyfin-databases in de huidige map voor het opnemen/testen van jellyfin-watch-notify.

Schrijft `playback.db` (PlaybackActivity) en `jellyfin.db` (Users) met fictieve gebruikers; tijdstippen zijn relatief
aan nu, ruim binnen of buiten het 15-minutenvenster, zodat de uitkomst niet van de kloktijd afhangt."""
import sqlite3
import sys
from datetime import datetime, timedelta, timezone

now = datetime.now(timezone.utc).replace(tzinfo=None)


def ts(minutes_ago: float, fmt: str = "iso") -> str:
    t = now - timedelta(minutes=minutes_ago)
    if fmt == "z":
        return t.strftime("%Y-%m-%dT%H:%M:%S.%fZ")
    if fmt == "space":
        return t.strftime("%Y-%m-%d %H:%M:%S")
    if fmt == "plus":
        return t.strftime("%Y-%m-%dT%H:%M:%S.%f") + "+00:00"
    return t.strftime("%Y-%m-%dT%H:%M:%S.%f")


users = [
    ("11111111-aaaa-bbbb-cccc-000000000001", "alice"),
    ("11111111-AAAA-BBBB-CCCC-000000000002", "Bob"),
    ("11111111-aaaa-bbbb-cccc-000000000003", "carol"),
    ("11111111-aaaa-bbbb-cccc-000000000004", "admin"),
]
rows = [
    # (DateCreated, UserId, ItemName, ItemType, PlaybackMethod, ClientName, DeviceName, PlayDuration)
    (ts(1, "z"), users[0][0], "Voorbeeldfilm", "Movie", "DirectPlay", "Jellyfin Web", "Firefox", 1200),
    (ts(2), users[1][0].lower(), "Voorbeeldserie S01E02", "Episode", "Transcode (v:h264 a:aac)", "Jellyfin Android", "Pixel", 300),
    (ts(3, "space"), users[2][0], "Een Liedje", "Audio", "DirectStream", "Finamp", "iPhone", 0),
    (ts(4, "plus"), users[0][0], "Tweede Film", "Movie", "Transcode", "Jellyfin Web", "Firefox", 60),
    (ts(5), users[3][0], "Admin Test", "Movie", "DirectPlay", "Jellyfin Web", "Chrome", 10),
    (ts(6), "99999999-0000-0000-0000-000000000009", None, None, None, None, None, None),
    (ts(7), None, "Zonder Gebruiker", "Movie", "DirectPlay", "Kodi", "Shield", "42"),
    ("niet-een-datum", users[0][0], "Kapotte Datum", "Movie", "DirectPlay", "X", "Y", 1),
    (ts(25), users[2][0], "Te Oud", "Movie", "Transcode", "X", "Y", 5),
    (ts(60, "z"), users[1][0], "Veel Te Oud", "Movie", "DirectPlay", "X", "Y", 5),
]
with sqlite3.connect("playback.db") as con:
    con.execute("DROP TABLE IF EXISTS PlaybackActivity")
    con.execute(
        "CREATE TABLE PlaybackActivity (rowid_alias INTEGER PRIMARY KEY, DateCreated TEXT, UserId TEXT, ItemName TEXT, ItemType TEXT,"
        " PlaybackMethod TEXT, ClientName TEXT, DeviceName TEXT, PlayDuration)"
    )
    con.executemany(
        "INSERT INTO PlaybackActivity (DateCreated, UserId, ItemName, ItemType, PlaybackMethod, ClientName, DeviceName, PlayDuration)"
        " VALUES (?,?,?,?,?,?,?,?)",
        rows,
    )
with sqlite3.connect("jellyfin.db") as con:
    con.execute("DROP TABLE IF EXISTS Users")
    con.execute("CREATE TABLE Users (Id TEXT, Username TEXT)")
    con.executemany("INSERT INTO Users VALUES (?,?)", users + [(None, "geen-id"), ("11111111-aaaa-bbbb-cccc-000000000005", None)])
if len(sys.argv) > 1 and sys.argv[1] == "empty":
    with sqlite3.connect("playback.db") as con:
        con.execute("DELETE FROM PlaybackActivity")
