# Beslissingen (genomen door agent, aanbevolen antwoord)

De gebruiker gaf de port vooraf goed; de "grill"-ronde is overgeslagen en de aanbevolen antwoorden zijn gekozen.

## ADR-001: Scope = `jellyfin-watch-notify.sh` en `cache-fix.sh` als een kleine Rust-CLI
Status: accepted (2026-11-27) - genomen door agent, aanbevolen antwoord
Context: `scripts/` bevat drie bash-scripts. De plugin-JS (`src/`, `test/`) blijft ongemoeid.
Decision: `rust/app` (binary `jelly-scripts`) met subcommando's `watch-notify` en `cache-fix`. `opslag-man.sh` (1278 regels, interactief, verwijdert media en bewerkt de Jellyfin-database met `sudo`) is bewust NIET geport: het is destructief en kan niet veilig worden opgenomen; het blijft bash (`keep / out of scope`).
Consequences: wie `opslag-man` gebruikt, merkt niets.

## ADR-002: Opname van het origineel via een wrapper en synthetische databases
Status: accepted (2026-11-27)
Decision: `rust/tools/orig.sh` draait het script onder dezelfde subcommando-naam als de Rust-CLI. `watch-notify` is opgenomen met fictieve SQLite-databases (`rust/tools/seed_db.py`, tijdstippen relatief aan nu), `NTFY_TOPIC` wijst naar de lokale effect-sink en `df` is een shim (`rust/tools/shims/df`) omdat de scripts op Linux zijn geschreven. Geen echte Jellyfin-server, geen echte ntfy.
Consequences: opgenomen op macOS (bash 3.2, geen `/proc`): de resource-regels geven daar `n/a`.

## ADR-003: `cache-fix` kan alleen in de niet-root takken worden opgenomen
Status: accepted (2026-11-27)
Context: het script eist root, gebruikt vaste paden (`/srv/jellyfin/...`), GNU `find -printf` en `mapfile` (bash 4); op deze Mac niet uitvoerbaar en nooit als root te draaien.
Decision: replay-cases dekken `--help`, `-h`, onbekende optie, ontbrekende waarde en de niet-root-melding. De echte logica (dry-run, verwijderen, lege mappen, log-/cache-leeftijd) is met de hand geport (`cache_fix.rs`) en bewezen met snapshot-tests op tijdelijke mappen (4 tests).
Consequences: geen differentiele test tegen de bash-versie voor de root-tak; tijdgrenzen (`-mtime`, `-mmin`) zijn op de seconde nagebouwd, de volgorde van `find` is alfabetisch (origineel: mapvolgorde).

## ADR-004: Eigenaardigheden van het origineel blijven
Status: accepted (2026-11-27)
Decision: (a) als `df -h /mnt/jellyfin-transcodes` faalt, stopt `watch-notify` stil met code 1 (set -o pipefail); (b) een ongeldig `LOOKBACK_MINUTES` geeft dezelfde Python-traceback; (c) de ntfy-POST gaat als `application/x-www-form-urlencoded` zoals `curl --data`; (d) een tweede `shift` zonder argumenten in `cache-fix` beeindigt het script met code 1.
Consequences: dit zijn kandidaat-bugs om later bewust te herzien (nieuw ADR).

## ADR-005: Niets deployen
Status: accepted (2026-11-27)
Decision: geen installatie op de Jellyfin-host, geen cron/systemd-wijziging, geen `rust-anything enable/setup`.
