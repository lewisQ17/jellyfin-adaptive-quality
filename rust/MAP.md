# Rust-port jellyfin-adaptive-quality (MAP)

Doel: de beheer-scripts in `scripts/` als een kleine Rust-CLI (`jelly-scripts`); de plugin-JS blijft.
Beslissingen: DECISIONS.md. Gemeten stand: PORT_MAP.md. Laatste update: 2026-11-27.

| Onderdeel | Status | Replay | Opmerking |
| --- | --- | --- | --- |
| `scripts/jellyfin-watch-notify.sh` | done | 22/22 (met cache-fix-cases) | synthetische DB's, ntfy-sink, `df`-shim; `rust/app/src/watch_notify.rs` (+4 unit-tests) |
| `scripts/cache-fix.sh` | done voor argumenten/help/niet-root; root-tak met de hand + snapshot-tests | idem | `rust/app/src/cache_fix.rs` (+7 tests); root-tak niet differentieel getest; sinds 2026-11-27 fd-relatief verwijderen zonder symlinks te volgen (ADR-006) |
| `scripts/opslag-man.sh` | keep / out of scope | - | destructief en interactief (sudo, mediaverwijdering, DB-bewerking) |
| `src/*.js`, `test/*.js` (plugin) | keep | - | ongewijzigd |

Front door: `rust/app` (`cargo build`; binary `rust/app/target/debug/jelly-scripts`).
`rust-anything app replay . --build rust/app/target/debug/jelly-scripts`: 22/22 cases, fallback rate 0% op de opgenomen cases (onbekende subcommando's vallen terug op `rust/tools/orig.sh`).
`app lift` niet gedraaid: `opslag-man.sh` en de wrapper blijven bash.

## Wat nog moet
1. De root-tak van `cache-fix` op een echte Jellyfin-host vergelijken met het bash-script (dry-run naast dry-run).
2. `watch-notify` op Linux opnieuw opnemen zodat `/proc/loadavg`, `/proc/meminfo` en echte `df`-uitvoer meetellen.
3. Niet-UTC `DateCreated`-waarden (offset) en bijzondere ISO-vormen hebben alleen unit-tests.
4. Beslissen of `opslag-man.sh` ooit mee moet (zou eerst een veilige fake-omgeving nodig hebben).

## Opnemen opnieuw
`rust/tools/record.sh` vanuit de repo-root (python3, bash, sqlite via Python; `PATH` in `rust/jelly-inputs.jsonl` bevat het absolute pad van deze werkmap).
