# Jelly scripts

Deze map bevat beheer-scripts voor je Jellyfin media-opslag.

## Bestanden

- `opslag-man.sh` — interactieve cleanup/verwijdertool voor media + Jellyfin metadata
- `cache-fix.sh` — cache/transcode opruimscript

---

## 1) `opslag-man.sh` gebruiken

### Doel
`opslag-man.sh` helpt je:
- specifieke films/series verwijderen via Jellyfin DB
- orphan data opschonen
- hardlink-restanten opruimen zodat ruimte echt vrijkomt

### Starten

```bash
sudo /usr/local/bin/opslag-man
```

Of met API-key (aanbevolen voor betere orphan detectie):

```bash
sudo JELLYFIN_API_KEY='JOUW_API_KEY' /usr/local/bin/opslag-man
```

Je kan ook direct scriptbestand gebruiken:

```bash
sudo ./opslag-man.sh
```

---

## 2) Menu-opties in detail

Na starten krijg je:

- `1) Video`
- `2) Serie`
- `c) Clean mode (orphans)`
- `b) Terug/stop`

### `1) Video` / `2) Serie`
Gebruik dit om bewust items te verwijderen die nog in Jellyfin staan.

Wat gebeurt er:
1. item uit lijst kiezen
2. media pad verwijderen
3. hardlink siblings binnen media mount ook verwijderen
4. Jellyfin metadata/cache opschonen
5. stale DB-item opruimen

Dit is de beste optie voor "ik wil deze titel echt weg".

---

## 3) Clean mode (`c`) scopes

Na `c` kies je scope:

- `d` = alleen downloads orphan cleanup (**aanbevolen / veiligst**)
- `a` = veilige leftovers cleanup (films/series/downloads)
- `x` = agressieve full cleanup (films + series + downloads)
- `b` = terug

### `d` (downloads-only)
- Scant alleen `downloads`
- Verwijdert paden die niet meer door Jellyfin gerefereerd zijn
- Veiligste periodieke cleanup

### `a` (veilige leftovers)
- Scant films/series/downloads
- Probeert normale film/seriecontent te sparen
- Gericht op leftovers/orphans

### `x` (agressief)
- Verwijdert alle orphan paden in films/series/downloads
- Kan dus ook gewone film/serie mappen pakken als die niet meer in Jellyfin referenties staan
- Alleen gebruiken als je zeker weet dat je full orphan cleanup wil

---

## 4) Waarom `d` soms weinig ruimte geeft en `x` veel

Je setup gebruikt hardlinks.

Dat betekent:
- hetzelfde fysieke datablock kan tegelijk zichtbaar zijn in `downloads` én `Films/Series`
- ruimte komt pas vrij wanneer de **laatste link** weg is

Dus:
- `d` kan files in `downloads` wegdoen zonder veel reclaim als link in `Films/Series` nog bestaat
- `x` verwijdert vaak ook die laatste links -> dan komt veel ruimte vrij

---

## 5) qBittorrent / Radarr / Sonarr flow

Typische flow:
1. qBittorrent downloadt naar `downloads`
2. Radarr/Sonarr importeert naar `Films/Series` (vaak via hardlink)
3. tijdens seeden blijft `downloads` vaak nodig

Gevolg:
- vroeg opruimen in `downloads` kan seeding breken
- daarom: eerst seeding klaar laten worden, dan cleanup

---

## 6) Aanbevolen veilige workflow

1. verwijder titels bewust via `1` of `2`
2. draai daarna `c -> d` voor download-restanten
3. gebruik `a` of `x` alleen als je gericht orphan/full cleanup wil
4. lees altijd de lijst vóór bevestigen (`y`)

---

## 7) `cache-fix.sh` (kort)

Draai met sudo:

```bash
sudo ./cache-fix.sh
```

Gebruik dit wanneer cache/transcode mappen oplopen of inconsistent zijn.

---

## 8) Handige checks

### Vrije ruimte

```bash
df -h /Media
```

### Hardlinks van een bestand zien

```bash
stat -c '%h %n' /Media/Films/Naam/Bestand.mkv
```

Als het eerste getal > 1 is, zijn er meerdere links naar dezelfde data.
