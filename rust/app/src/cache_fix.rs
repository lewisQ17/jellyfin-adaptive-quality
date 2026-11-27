//! Port of `scripts/cache-fix.sh`: houdt de N nieuwste transcode-bestanden, ruimt oude logs en verlopen cache op.
//!
//! De mappen zijn constanten in het script; hier zitten ze in `Config`, zodat tests op tijdelijke mappen kunnen
//! draaien. Ordening van `find` is bij het origineel de mapvolgorde van het bestandssysteem; hier alfabetisch.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

pub const USAGE: &str = "Gebruik:
  cache-fix.sh [--dry-run] [--keep-log-days N] [--keep-transcode-count N]

Wat doet dit script:
  - Houdt enkel de N nieuwste transcode-bestanden bij (standaard 2)
  - Verwijdert oude Jellyfin logs
  - Verwijdert tijdelijke cache-bestanden
  - Toont ruimtewinst
";

#[derive(Debug, Clone)]
pub struct Config {
    pub dry_run: bool,
    pub keep_log_days: String,
    pub keep_transcode_count: String,
    pub cache_dir: PathBuf,
    pub transcode_dir: PathBuf,
    pub log_dir: PathBuf,
    /// Het commando dat de ruimte toont (`df -h /`); `None` slaat het over (tests).
    pub show_space: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dry_run: false,
            keep_log_days: "7".into(),
            keep_transcode_count: "2".into(),
            cache_dir: "/srv/jellyfin/cache".into(),
            transcode_dir: "/mnt/jellyfin-transcodes".into(),
            log_dir: "/srv/jellyfin/config/log".into(),
            show_space: true,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Run(Box<ConfigArgs>),
    /// Uitvoer en afsluitcode (help of fout).
    Exit {
        stdout: String,
        code: i32,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct ConfigArgs {
    pub dry_run: bool,
    pub keep_log_days: String,
    pub keep_transcode_count: String,
}

/// De `while`/`case`-lus van het script, inclusief het gedrag bij een ontbrekende waarde (`${2:-}` en dubbele `shift`).
pub fn parse_args(args: &[String]) -> Parsed {
    let mut out = ConfigArgs {
        dry_run: false,
        keep_log_days: "7".into(),
        keep_transcode_count: "2".into(),
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => out.dry_run = true,
            opt @ ("--keep-log-days" | "--keep-transcode-count") => {
                let value = args.get(i + 1).cloned().unwrap_or_default();
                if opt == "--keep-log-days" {
                    out.keep_log_days = value;
                } else {
                    out.keep_transcode_count = value;
                }
                i += 1; // het script doet hier `shift` en daarna nog een `shift`
                if i >= args.len() {
                    // tweede `shift` zonder argumenten: `set -e` beeindigt het script stil met code 1
                    return Parsed::Exit {
                        stdout: String::new(),
                        code: 1,
                    };
                }
            }
            "-h" | "--help" => {
                return Parsed::Exit {
                    stdout: USAGE.into(),
                    code: 0,
                };
            }
            other => {
                return Parsed::Exit {
                    stdout: format!("Onbekende optie: {other}\n{USAGE}"),
                    code: 1,
                };
            }
        }
        i += 1;
    }
    Parsed::Run(Box::new(out))
}

/// `${EUID:-$(id -u)} -ne 0`.
pub fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .is_some_and(|s| s.trim() == "0")
}

fn bash_int(s: &str) -> i64 {
    // `(( count > VAR ))`: een niet-numerieke waarde telt als 0
    s.trim().parse().unwrap_or(0)
}

#[derive(Clone, Copy)]
struct Entry {
    secs: i64,
    nanos: u32,
}

fn mtime(path: &Path) -> Option<Entry> {
    let t = fs::symlink_metadata(path).ok()?.modified().ok()?;
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => Some(Entry {
            secs: d.as_secs() as i64,
            nanos: d.subsec_nanos(),
        }),
        Err(e) => Some(Entry {
            secs: -(e.duration().as_secs() as i64),
            nanos: 0,
        }),
    }
}

fn sorted_children(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// `find DIR -type f` (zonder symlinks te volgen).
fn find_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for p in sorted_children(dir) {
        let Ok(meta) = fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            find_files(&p, out);
        } else if meta.is_file() {
            out.push(p);
        }
    }
}

/// `find DIR -type d -empty -print -delete`: -delete werkt diepte-eerst, dus mappen die door het verwijderen van
/// lege submappen leeg worden, gaan mee (ook DIR zelf).
fn delete_empty_dirs(dir: &Path, out: &mut dyn Write, delete: bool) -> bool {
    let mut empty = true;
    for p in sorted_children(dir) {
        let is_dir = fs::symlink_metadata(&p)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if is_dir {
            if !delete_empty_dirs(&p, out, delete) {
                empty = false;
            }
        } else {
            empty = false;
        }
    }
    if empty {
        let _ = writeln!(out, "{}", dir.display());
        if delete {
            let _ = fs::remove_dir(dir);
        }
    }
    empty
}

/// `find DIR -type f -name PATTERN <age test> -print -delete`.
fn delete_old_files(
    dir: &Path,
    name_ok: &dyn Fn(&str) -> bool,
    older_than: Duration,
    now: SystemTime,
    out: &mut dyn Write,
) {
    let mut files = Vec::new();
    find_files(dir, &mut files);
    for p in files {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name_ok(name) {
            continue;
        }
        let age = fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok());
        if age.is_some_and(|a| a > older_than) {
            let _ = writeln!(out, "{}", p.display());
            let _ = fs::remove_file(&p);
        }
    }
}

fn space(out: &mut dyn Write) {
    let _ = out.flush();
    let _ = Command::new("df").args(["-h", "/"]).status();
}

/// Draait het script; schrijft naar `out`. Geeft de afsluitcode terug.
pub fn run(cfg: &Config, now: SystemTime, out: &mut dyn Write) -> i32 {
    let _ = writeln!(out, "== Vooraf ruimte ==");
    if cfg.show_space {
        space(out);
    }
    let q = |p: &Path| p.display().to_string();
    if cfg.transcode_dir.is_dir() {
        let mut files = Vec::new();
        find_files(&cfg.transcode_dir, &mut files);
        let mut entries: Vec<(Entry, String)> = files
            .iter()
            .filter_map(|p| mtime(p).map(|e| (e, p.display().to_string())))
            .collect();
        // sort -nr op "%T@|pad": numeriek op de tijd, aflopend; bij gelijke tijd de hele regel omgekeerd
        entries.sort_by(|(a, pa), (b, pb)| {
            (b.secs, b.nanos)
                .cmp(&(a.secs, a.nanos))
                .then_with(|| pb.cmp(pa))
        });
        let count = entries.len() as i64;
        let _ = writeln!(out, "Transcode files gevonden: {count}");
        let keep = bash_int(&cfg.keep_transcode_count);
        if count > keep {
            for (_, path) in entries.iter().skip(keep.max(0) as usize) {
                if cfg.dry_run {
                    let _ = writeln!(out, "[dry-run] delete {path}");
                } else {
                    let _ = writeln!(out, "{path}");
                    let _ = fs::remove_file(path);
                }
            }
            if cfg.dry_run {
                let _ = writeln!(
                    out,
                    "[dry-run] find '{}' -type d -empty -print -delete",
                    q(&cfg.transcode_dir)
                );
            } else {
                delete_empty_dirs(&cfg.transcode_dir, out, true);
            }
        } else {
            let _ = writeln!(
                out,
                "Geen oude transcodes om te verwijderen (keep={}).",
                cfg.keep_transcode_count
            );
        }
    } else {
        let _ = writeln!(
            out,
            "Transcode map niet gevonden: {}",
            q(&cfg.transcode_dir)
        );
    }

    if cfg.log_dir.is_dir() {
        let days = cfg.keep_log_days.trim().parse::<u64>().ok();
        for (pattern, test) in [
            ("log_*.log", log_pattern as fn(&str) -> bool),
            ("FFmpeg*.log", ffmpeg_pattern as fn(&str) -> bool),
        ] {
            if cfg.dry_run {
                let _ = writeln!(
                    out,
                    "[dry-run] find '{}' -type f -name '{pattern}' -mtime +{} -print -delete",
                    q(&cfg.log_dir),
                    cfg.keep_log_days
                );
            } else if let Some(d) = days {
                // -mtime +N: leeftijd in hele dagen groter dan N
                delete_old_files(
                    &cfg.log_dir,
                    &test,
                    Duration::from_secs((d + 1) * 86400 - 1),
                    now,
                    out,
                );
            } else {
                let _ = out.flush();
                eprintln!(
                    "find: invalid argument `+{}' to `-mtime'",
                    cfg.keep_log_days
                );
                return 1;
            }
        }
    } else {
        let _ = writeln!(out, "Log map niet gevonden: {}", q(&cfg.log_dir));
    }

    if cfg.cache_dir.is_dir() {
        if cfg.dry_run {
            let _ = writeln!(
                out,
                "[dry-run] find '{}' -type f -mmin +360 -print -delete",
                q(&cfg.cache_dir)
            );
            let _ = writeln!(
                out,
                "[dry-run] find '{}' -type d -empty -print -delete",
                q(&cfg.cache_dir)
            );
        } else {
            delete_old_files(
                &cfg.cache_dir,
                &|_| true,
                Duration::from_secs(360 * 60),
                now,
                out,
            );
            delete_empty_dirs(&cfg.cache_dir, out, true);
        }
    } else {
        let _ = writeln!(out, "Cache map niet gevonden: {}", q(&cfg.cache_dir));
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "== Nadien ruimte ==");
    if cfg.show_space {
        space(out);
    }
    let _ = writeln!(out, "Klaar.");
    0
}

/// glob `log_*.log`
fn log_pattern(name: &str) -> bool {
    name.len() >= "log_.log".len() && name.starts_with("log_") && name.ends_with(".log")
}

/// glob `FFmpeg*.log`
fn ffmpeg_pattern(name: &str) -> bool {
    name.len() >= "FFmpeg.log".len() && name.starts_with("FFmpeg") && name.ends_with(".log")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jelly-cachefix-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(path: &Path, age_secs: u64, now: SystemTime) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let f = fs::File::create(path).unwrap();
        f.set_modified(now - Duration::from_secs(age_secs)).unwrap();
    }

    fn cfg(root: &Path) -> Config {
        Config {
            cache_dir: root.join("cache"),
            transcode_dir: root.join("transcodes"),
            log_dir: root.join("log"),
            show_space: false,
            ..Config::default()
        }
    }

    fn output(cfg: &Config, now: SystemTime) -> String {
        let mut buf = Vec::new();
        assert_eq!(run(cfg, now, &mut buf), 0);
        String::from_utf8(buf).unwrap().replace(
            &cfg.cache_dir.parent().unwrap().display().to_string(),
            "ROOT",
        )
    }

    #[test]
    fn parses_like_the_script() {
        let a = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert!(matches!(
            parse_args(&a(&["--help"])),
            Parsed::Exit { code: 0, .. }
        ));
        assert!(matches!(
            parse_args(&a(&["--bogus"])),
            Parsed::Exit { code: 1, .. }
        ));
        assert!(
            matches!(parse_args(&a(&["--keep-log-days"])), Parsed::Exit { code: 1, stdout } if stdout.is_empty())
        );
        let Parsed::Run(r) = parse_args(&a(&[
            "--dry-run",
            "--keep-log-days",
            "3",
            "--keep-transcode-count",
            "5",
        ])) else {
            panic!("run")
        };
        assert_eq!(
            (
                r.dry_run,
                r.keep_log_days.as_str(),
                r.keep_transcode_count.as_str()
            ),
            (true, "3", "5")
        );
    }

    #[test]
    fn dry_run_lists_without_deleting() {
        let root = tmp("dry");
        let now = SystemTime::now();
        for (i, age) in [100u64, 200, 300, 400].iter().enumerate() {
            touch(&root.join(format!("transcodes/t{i}.ts")), *age, now);
        }
        let mut c = cfg(&root);
        c.dry_run = true;
        fs::create_dir_all(&c.log_dir).unwrap();
        fs::create_dir_all(&c.cache_dir).unwrap();
        let text = output(&c, now);
        let expected = "== Vooraf ruimte ==
Transcode files gevonden: 4
[dry-run] delete ROOT/transcodes/t2.ts
[dry-run] delete ROOT/transcodes/t3.ts
[dry-run] find 'ROOT/transcodes' -type d -empty -print -delete
[dry-run] find 'ROOT/log' -type f -name 'log_*.log' -mtime +7 -print -delete
[dry-run] find 'ROOT/log' -type f -name 'FFmpeg*.log' -mtime +7 -print -delete
[dry-run] find 'ROOT/cache' -type f -mmin +360 -print -delete
[dry-run] find 'ROOT/cache' -type d -empty -print -delete

== Nadien ruimte ==
Klaar.
";
        assert_eq!(text, expected);
        assert!(root.join("transcodes/t3.ts").exists());
    }

    #[test]
    fn real_run_deletes_old_files_and_empty_dirs() {
        let root = tmp("real");
        let now = SystemTime::now();
        touch(&root.join("transcodes/new1.ts"), 10, now);
        touch(&root.join("transcodes/new2.ts"), 20, now);
        touch(&root.join("transcodes/sub/old.ts"), 5000, now);
        touch(&root.join("log/log_old.log"), 9 * 86400, now);
        touch(&root.join("log/log_new.log"), 86400, now);
        touch(&root.join("log/FFmpeg_old.log"), 8 * 86400 + 10, now);
        touch(&root.join("log/other_old.txt"), 20 * 86400, now);
        touch(&root.join("cache/a/stale.bin"), 361 * 60, now);
        touch(&root.join("cache/fresh.bin"), 60, now);
        let text = output(&cfg(&root), now);
        let expected = "== Vooraf ruimte ==
Transcode files gevonden: 3
ROOT/transcodes/sub/old.ts
ROOT/transcodes/sub
ROOT/log/log_old.log
ROOT/log/FFmpeg_old.log
ROOT/cache/a/stale.bin
ROOT/cache/a

== Nadien ruimte ==
Klaar.
";
        assert_eq!(text, expected);
        assert!(!root.join("transcodes/sub").exists());
        assert!(root.join("log/log_new.log").exists() && root.join("log/other_old.txt").exists());
        assert!(root.join("cache/fresh.bin").exists());
    }

    #[test]
    fn keeps_everything_when_few_files() {
        let root = tmp("few");
        let now = SystemTime::now();
        touch(&root.join("transcodes/a.ts"), 5, now);
        let text = output(&cfg(&root), now);
        assert!(text.contains(
            "Transcode files gevonden: 1\nGeen oude transcodes om te verwijderen (keep=2).\n"
        ));
        assert!(
            text.contains("Log map niet gevonden: ROOT/log\nCache map niet gevonden: ROOT/cache\n")
        );
    }
}
