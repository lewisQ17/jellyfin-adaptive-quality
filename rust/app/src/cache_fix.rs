//! Port of `scripts/cache-fix.sh`: houdt de N nieuwste transcode-bestanden, ruimt oude logs en verlopen cache op.
//!
//! De mappen zijn constanten in het script; hier zitten ze in `Config`, zodat tests op tijdelijke mappen kunnen
//! draaien. Ordening van `find` is bij het origineel de mapvolgorde van het bestandssysteem; hier alfabetisch.

use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
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

/// Veilige, fd-gebaseerde bestandsboom (zoals `find -delete`): symlinks (bestanden en mappen) worden nooit
/// gevolgd, elke map wordt geopend met `O_NOFOLLOW|O_DIRECTORY` relatief aan de ouder-fd en verwijderen gebeurt
/// met `unlinkat` relatief aan een open map-fd. Een tijdens de run verwisselde map-component (TOCTOU) wordt zo
/// nooit gevolgd; het script draait als root.
mod safefs {
    use super::*;
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat, unlinkat};

    fn flags() -> OFlags {
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::DIRECTORY | OFlags::CLOEXEC
    }

    fn cname(name: &OsStr) -> Option<CString> {
        CString::new(name.as_bytes()).ok()
    }

    /// Opent `path` zelf zonder een eindsymlink te volgen (zoals `find DIR` zonder -H/-L).
    pub fn open_root(path: &Path) -> Option<OwnedFd> {
        openat(rustix::fs::CWD, path, flags(), Mode::empty()).ok()
    }

    pub fn open_sub(dir: &OwnedFd, name: &OsStr) -> Option<OwnedFd> {
        openat(dir, cname(name)?.as_c_str(), flags(), Mode::empty()).ok()
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Kind {
        File,
        Dir,
        Other,
    }

    pub struct Child {
        pub name: OsString,
        pub kind: Kind,
        pub mtime: Entry,
    }

    /// Kinderen van een open map, alfabetisch, getypeerd via `fstatat(AT_SYMLINK_NOFOLLOW)`.
    pub fn children(dir: &OwnedFd) -> Vec<Child> {
        let Ok(rd) = Dir::read_from(dir) else {
            return Vec::new();
        };
        let mut v = Vec::new();
        for ent in rd.flatten() {
            let raw = ent.file_name().to_bytes();
            if raw == b"." || raw == b".." {
                continue;
            }
            let name = OsString::from_vec(raw.to_vec());
            let Ok(st) = statat(dir, ent.file_name(), AtFlags::SYMLINK_NOFOLLOW) else {
                continue;
            };
            let kind = match FileType::from_raw_mode(st.st_mode) {
                FileType::RegularFile => Kind::File,
                FileType::Directory => Kind::Dir,
                _ => Kind::Other, // symlinks, sockets, ...: overgeslagen
            };
            #[allow(
                clippy::unnecessary_cast,
                reason = "veldtypes verschillen per platform"
            )]
            let mtime = Entry {
                secs: st.st_mtime as i64,
                nanos: st.st_mtime_nsec as u32,
            };
            v.push(Child { name, kind, mtime });
        }
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// `find DIR -type f`: (relatief pad als componenten, mtime).
    pub fn find_files(dir: &OwnedFd, prefix: &[OsString], out: &mut Vec<(Vec<OsString>, Entry)>) {
        for c in children(dir) {
            match c.kind {
                Kind::File => {
                    let mut rel = prefix.to_vec();
                    rel.push(c.name);
                    out.push((rel, c.mtime));
                }
                Kind::Dir => {
                    if let Some(sub) = open_sub(dir, &c.name) {
                        let mut p = prefix.to_vec();
                        p.push(c.name);
                        find_files(&sub, &p, out);
                    }
                }
                Kind::Other => {}
            }
        }
    }

    /// Opent de map van `rel` stap voor stap met O_NOFOLLOW en verwijdert het bestand met `unlinkat`.
    /// Alleen als het (nog) een gewoon bestand is; een symlink of een verwisselde map-component wordt overgeslagen.
    pub fn unlink_file(root: &OwnedFd, rel: &[OsString]) -> bool {
        let Some((leaf, parents)) = rel.split_last() else {
            return false;
        };
        let mut cur: Option<OwnedFd> = None;
        for comp in parents {
            let next = open_sub(cur.as_ref().unwrap_or(root), comp);
            match next {
                Some(fd) => cur = Some(fd),
                None => return false,
            }
        }
        let dir = cur.as_ref().unwrap_or(root);
        let Some(c) = cname(leaf) else {
            return false;
        };
        match statat(dir, c.as_c_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) if FileType::from_raw_mode(st.st_mode) == FileType::RegularFile => {
                unlinkat(dir, c.as_c_str(), AtFlags::empty()).is_ok()
            }
            _ => false,
        }
    }

    /// `find DIR -type d -empty -print -delete` onder een open map (diepte-eerst). Geeft terug of `dir` leeg is.
    /// Symlinks tellen als inhoud (de map is dan niet leeg) en worden nooit gevolgd.
    pub fn delete_empty_subdirs(
        dir: &OwnedFd,
        path: &Path,
        out: &mut dyn Write,
        delete: bool,
    ) -> bool {
        let mut empty = true;
        for c in children(dir) {
            if c.kind == Kind::Dir {
                let child_path = path.join(&c.name);
                let sub_empty = match open_sub(dir, &c.name) {
                    Some(sub) => delete_empty_subdirs(&sub, &child_path, out, delete),
                    None => false,
                };
                if sub_empty {
                    let _ = writeln!(out, "{}", child_path.display());
                    if delete && let Some(n) = cname(&c.name) {
                        let _ = unlinkat(dir, n.as_c_str(), AtFlags::REMOVEDIR);
                    }
                } else {
                    empty = false;
                }
            } else {
                empty = false;
            }
        }
        empty
    }
}

fn join_rel(root: &Path, rel: &[OsString]) -> PathBuf {
    let mut p = root.to_path_buf();
    for c in rel {
        p.push(c);
    }
    p
}

/// `find DIR -type d -empty -print -delete`: -delete werkt diepte-eerst, dus mappen die door het verwijderen van
/// lege submappen leeg worden, gaan mee (ook DIR zelf).
fn delete_empty_dirs(dir: &Path, out: &mut dyn Write, delete: bool) {
    let Some(fd) = safefs::open_root(dir) else {
        return;
    };
    if safefs::delete_empty_subdirs(&fd, dir, out, delete) {
        let _ = writeln!(out, "{}", dir.display());
        if delete {
            drop(fd);
            let _ = fs::remove_dir(dir); // rmdir volgt geen symlink en verwijdert enkel een lege map
        }
    }
}

/// `find DIR -type f -name PATTERN <age test> -print -delete`.
fn delete_old_files(
    dir: &Path,
    name_ok: &dyn Fn(&str) -> bool,
    older_than: Duration,
    now: SystemTime,
    out: &mut dyn Write,
) {
    let Some(root) = safefs::open_root(dir) else {
        return;
    };
    let mut files = Vec::new();
    safefs::find_files(&root, &[], &mut files);
    for (rel, mt) in files {
        let name = rel.last().and_then(|n| n.to_str()).unwrap_or("");
        if !name_ok(name) {
            continue;
        }
        let modified = if mt.secs >= 0 {
            SystemTime::UNIX_EPOCH + Duration::new(mt.secs as u64, mt.nanos)
        } else {
            SystemTime::UNIX_EPOCH
        };
        if now.duration_since(modified).is_ok_and(|a| a > older_than) {
            let _ = writeln!(out, "{}", join_rel(dir, &rel).display());
            safefs::unlink_file(&root, &rel);
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
        let root = safefs::open_root(&cfg.transcode_dir);
        let mut files = Vec::new();
        if let Some(r) = &root {
            safefs::find_files(r, &[], &mut files);
        }
        let mut entries: Vec<(Entry, Vec<OsString>)> =
            files.into_iter().map(|(rel, e)| (e, rel)).collect();
        // sort -nr op "%T@|pad": numeriek op de tijd, aflopend; bij gelijke tijd de hele regel omgekeerd
        entries.sort_by(|(a, pa), (b, pb)| {
            (b.secs, b.nanos).cmp(&(a.secs, a.nanos)).then_with(|| {
                join_rel(&cfg.transcode_dir, pb).cmp(&join_rel(&cfg.transcode_dir, pa))
            })
        });
        let count = entries.len() as i64;
        let _ = writeln!(out, "Transcode files gevonden: {count}");
        let keep = bash_int(&cfg.keep_transcode_count);
        if count > keep {
            for (_, rel) in entries.iter().skip(keep.max(0) as usize) {
                let path = join_rel(&cfg.transcode_dir, rel).display().to_string();
                if cfg.dry_run {
                    let _ = writeln!(out, "[dry-run] delete {path}");
                } else {
                    let _ = writeln!(out, "{path}");
                    if let Some(r) = &root {
                        safefs::unlink_file(r, rel);
                    }
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

    #[test]
    fn never_follows_symlinks() {
        use std::os::unix::fs::symlink;
        let root = tmp("symlink");
        let outside = tmp("symlink-outside");
        let now = SystemTime::now();
        // doelen buiten de cache: een map met oude bestanden (mag NIET leeg gemaakt worden) en een los bestand
        touch(&outside.join("dir/precious.bin"), 9999 * 60, now);
        touch(&outside.join("dir/sub/precious2.log"), 99 * 86400, now);
        touch(&outside.join("file.log"), 99 * 86400, now);
        touch(&root.join("cache/old.bin"), 400 * 60, now);
        symlink(outside.join("dir"), root.join("cache/linkdir")).unwrap();
        symlink(outside.join("file.log"), root.join("cache/linkfile.bin")).unwrap();
        touch(&root.join("log/log_old.log"), 20 * 86400, now);
        symlink(outside.join("dir"), root.join("log/linkdir")).unwrap();
        symlink(outside.join("file.log"), root.join("log/log_link.log")).unwrap();
        touch(&root.join("transcodes/n1.ts"), 10, now);
        touch(&root.join("transcodes/n2.ts"), 20, now);
        touch(&root.join("transcodes/old.ts"), 3000, now);
        symlink(outside.join("dir"), root.join("transcodes/linkdir")).unwrap();
        symlink(
            outside.join("file.log"),
            root.join("transcodes/linkfile.ts"),
        )
        .unwrap();

        let text = output(&cfg(&root), now);
        assert!(text.contains("Transcode files gevonden: 3\n"), "{text}");
        assert!(!root.join("cache/old.bin").exists());
        assert!(!root.join("log/log_old.log").exists());
        assert!(!root.join("transcodes/old.ts").exists());
        // symlinks zelf blijven staan, en de doelen buiten de cache blijven intact
        for l in [
            "cache/linkdir",
            "cache/linkfile.bin",
            "log/linkdir",
            "log/log_link.log",
            "transcodes/linkdir",
            "transcodes/linkfile.ts",
        ] {
            assert!(
                fs::symlink_metadata(root.join(l)).is_ok(),
                "{l} is verwijderd"
            );
        }
        assert!(outside.join("dir/precious.bin").exists());
        assert!(outside.join("dir/sub/precious2.log").exists());
        assert!(outside.join("file.log").exists());
        assert!(
            !text.contains("precious") && !text.contains("file.log"),
            "{text}"
        );
    }

    #[test]
    fn symlinked_root_is_not_followed() {
        use std::os::unix::fs::symlink;
        let root = tmp("symroot");
        let outside = tmp("symroot-outside");
        let now = SystemTime::now();
        touch(&outside.join("a.bin"), 999 * 60, now);
        fs::create_dir_all(&root).unwrap();
        symlink(&outside, root.join("cache")).unwrap();
        let _ = output(&cfg(&root), now);
        assert!(outside.join("a.bin").exists());
    }

    #[test]
    fn empty_dir_removal_ignores_symlinked_dirs() {
        use std::os::unix::fs::symlink;
        let root = tmp("emptylink");
        let outside = tmp("emptylink-outside");
        let now = SystemTime::now();
        fs::create_dir_all(outside.join("empty")).unwrap();
        fs::create_dir_all(root.join("cache/realempty")).unwrap();
        fs::create_dir_all(root.join("cache/withlink")).unwrap();
        symlink(outside.join("empty"), root.join("cache/withlink/l")).unwrap();
        let _ = output(&cfg(&root), now);
        assert!(!root.join("cache/realempty").exists());
        // een map met alleen een symlink is niet leeg (find -empty), en het doel blijft bestaan
        assert!(root.join("cache/withlink/l").exists());
        assert!(outside.join("empty").exists());
    }
}
