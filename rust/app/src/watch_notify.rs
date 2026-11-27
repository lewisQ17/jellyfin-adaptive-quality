//! Port of `scripts/jellyfin-watch-notify.sh`: meldt via ntfy wie er op Jellyfin kijkt (en of dat transcodeert),
//! maar alleen als het bericht veranderd is sinds de vorige keer.
//!
//! Gedrag volgt het script, ook zijn eigenaardigheden: een `df` die faalt beeindigt het script stil met code 1,
//! en een ongeldig `LOOKBACK_MINUTES` geeft dezelfde Python-traceback.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, types::Value};
use sha2::{Digest, Sha256};

pub struct Env {
    pub ntfy_topic: Option<String>,
    pub exclude_users: String,
    pub lookback: String,
    pub state_file: String,
    pub playback_db: String,
    pub jelly_db: String,
}

impl Env {
    pub fn from_process() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        let or = |k: &str, d: &str| {
            var(k)
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| d.to_string())
        };
        Self {
            ntfy_topic: var("NTFY_TOPIC").filter(|v| !v.is_empty()),
            exclude_users: var("EXCLUDE_USERS").unwrap_or_default(),
            lookback: or("LOOKBACK_MINUTES", "15"),
            state_file: or("STATE_FILE", "/tmp/jellyfin-watch-notify.state"),
            playback_db: or(
                "PLAYBACK_DB",
                "/srv/jellyfin/config/data/playback_reporting.db",
            ),
            jelly_db: or("JELLY_DB", "/srv/jellyfin/config/data/jellyfin.db"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Play {
    pub username: String,
    pub item: String,
    pub kind: String,
    pub method: String,
    pub transcode: bool,
    pub client: String,
    pub device: String,
}

/// Where the message goes and what happened, so tests can check it without a network.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to report (databases missing, nobody active, or message unchanged).
    Quiet,
    Send {
        message: String,
        hash: String,
    },
    /// Exit with this code and this text on stderr.
    Fail {
        code: i32,
        stderr: String,
    },
}

fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Integer(i) => *i != 0,
        Value::Real(f) => *f != 0.0,
        Value::Text(t) => !t.is_empty(),
        Value::Blob(b) => !b.is_empty(),
    }
}

/// Python's `str(value)`.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => float_repr(*f),
        Value::Text(t) => t.clone(),
        Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

/// `str(x or default)`.
fn str_or(v: &Value, default: &str) -> String {
    if py_truthy(v) {
        py_str(v)
    } else {
        default.to_string()
    }
}

fn float_repr(x: f64) -> String {
    if x == 0.0 {
        return "0.0".into();
    }
    let sci = format!("{:e}", x.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0")); // INFALLIBLE: `{:e}` always has an exponent
    let exp: i32 = exp.parse().unwrap_or(0); // INFALLIBLE: decimal exponent
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let sign = if x < 0.0 { "-" } else { "" };
    if !(-5 < exp && exp < 16) {
        let frac = &digits[1..];
        let dot = if frac.is_empty() {
            String::new()
        } else {
            format!(".{frac}")
        };
        return format!(
            "{sign}{}{dot}e{}{:02}",
            &digits[..1],
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        );
    }
    if exp >= 0 {
        let n = exp as usize + 1;
        if digits.len() <= n {
            format!("{sign}{digits}{}.0", "0".repeat(n - digits.len()))
        } else {
            format!("{sign}{}.{}", &digits[..n], &digits[n..])
        }
    } else {
        format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
    }
}

/// `int(x or 0)`; `Err` is the Python exception line.
fn int_or_zero(v: &Value) -> Result<i64, String> {
    if !py_truthy(v) {
        return Ok(0);
    }
    match v {
        Value::Integer(i) => Ok(*i),
        Value::Real(f) if f.is_finite() => Ok(f.trunc() as i64),
        Value::Text(t) => t.trim().parse::<i64>().map_err(|_| {
            format!(
                "ValueError: invalid literal for int() with base 10: {}",
                py_repr_str(t)
            )
        }),
        _ => Err(
            "TypeError: int() argument must be a string, a bytes-like object or a real number"
                .into(),
        ),
    }
}

fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Microseconds since the epoch of a naive timestamp, parsed like `datetime.fromisoformat` (the common ISO shapes).
/// `None` = Python would raise ValueError (the row is skipped). `Some(Err)` = an offset is left, which makes
/// Python raise TypeError when comparing with the naive cutoff.
fn parse_iso(raw: &str) -> Option<Result<i64, ()>> {
    let s = raw.replace('Z', "+00:00").replace("+00:00", "");
    let (date, rest) = match s.find(['T', ' ']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s.as_str(), None),
    };
    let mut dp = date.split('-');
    let (y, m, d) = (dp.next()?, dp.next()?, dp.next()?);
    if dp.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if y < 1 || !(1..=12).contains(&m) || d < 1 || d > dim[(m - 1) as usize] {
        return None;
    }
    let (mut hh, mut mm, mut ss, mut micro) = (0i64, 0i64, 0i64, 0i64);
    if let Some(t) = rest {
        let (clock, offset) = match t.find(['+', '-']) {
            Some(i) => (&t[..i], Some(&t[i..])),
            None => (t, None),
        };
        let mut parts = clock.splitn(3, ':');
        hh = parts.next()?.parse().ok()?;
        if let Some(p) = parts.next() {
            mm = p.parse().ok()?;
        }
        if let Some(p) = parts.next() {
            let (sec, frac) = match p.split_once(['.', ',']) {
                Some((a, b)) => (a, Some(b)),
                None => (p, None),
            };
            ss = sec.parse().ok()?;
            if let Some(f) = frac {
                if f.is_empty() || !f.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                let padded = format!("{:0<6}", &f[..f.len().min(6)]);
                micro = padded.parse().ok()?;
            }
        }
        if !(0..24).contains(&hh) || !(0..60).contains(&mm) || !(0..60).contains(&ss) {
            return None;
        }
        if offset.is_some() {
            return Some(Err(()));
        }
    }
    // days from civil
    let (yy, mm_) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm_ + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(Ok(
        ((days * 86400 + hh * 3600 + mm * 60 + ss) * 1_000_000) + micro
    ))
}

/// `int(LOOKBACK_MINUTES)` the Python way.
fn py_int_arg(s: &str) -> Result<i64, String> {
    s.trim().replace('_', "").parse::<i64>().map_err(|_| {
        format!(
            "ValueError: invalid literal for int() with base 10: {}",
            py_repr_str(s)
        )
    })
}

fn traceback(line: u32, error: &str) -> String {
    format!(
        "Traceback (most recent call last):\n  File \"<stdin>\", line {line}, in <module>\n{error}\n"
    )
}

/// The python block of the script: who is active in the last `lookback` minutes.
pub fn collect(env: &Env, now_micros: i64) -> Result<(usize, usize, Vec<Play>), String> {
    let lookback = py_int_arg(&env.lookback).map_err(|e| traceback(9, &e))?;
    let exclude: Vec<String> = env
        .exclude_users
        .split(',')
        .map(|x| x.trim().to_lowercase())
        .filter(|x| !x.is_empty())
        .collect();
    let cutoff = now_micros - lookback * 60 * 1_000_000;

    let mut users: HashMap<String, String> = HashMap::new();
    let jelly = Connection::open_with_flags(&env.jelly_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("sqlite3.OperationalError: {e}\n"))?;
    let mut stmt = jelly
        .prepare("SELECT Id, Username FROM Users")
        .map_err(|e| format!("sqlite3.OperationalError: {e}\n"))?;
    let pairs = stmt
        .query_map([], |r| Ok((r.get::<_, Value>(0)?, r.get::<_, Value>(1)?)))
        .map_err(|e| e.to_string())?;
    for pair in pairs.flatten() {
        if py_truthy(&pair.0) && py_truthy(&pair.1) {
            users.insert(py_str(&pair.0).to_lowercase(), py_str(&pair.1));
        }
    }

    let playback = Connection::open_with_flags(&env.playback_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("sqlite3.OperationalError: {e}\n"))?;
    let mut stmt = playback
        .prepare(
            "SELECT rowid, DateCreated, UserId, ItemName, ItemType, PlaybackMethod, ClientName, DeviceName, PlayDuration \
             FROM PlaybackActivity WHERE DateCreated IS NOT NULL ORDER BY rowid DESC LIMIT 600",
        )
        .map_err(|e| format!("sqlite3.OperationalError: {e}\n"))?;
    let rows = stmt
        .query_map([], |r| {
            (1..9)
                .map(|i| r.get::<_, Value>(i))
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| e.to_string())?;
    let mut all: Vec<Play> = Vec::new();
    for row in rows.flatten() {
        let Some(parsed) = parse_iso(&py_str(&row[0])) else {
            continue;
        };
        let Ok(ts) = parsed else {
            return Err(traceback(
                41,
                "TypeError: can't compare offset-naive and offset-aware datetimes",
            ));
        };
        if ts < cutoff {
            continue;
        }
        let uid = if py_truthy(&row[1]) {
            py_str(&row[1]).to_lowercase()
        } else {
            String::new()
        };
        let username = users.get(&uid).cloned().unwrap_or_else(|| {
            if uid.is_empty() {
                "unknown".to_string()
            } else {
                uid.chars().take(8).collect()
            }
        });
        if exclude.contains(&username.to_lowercase()) {
            continue;
        }
        let method = if py_truthy(&row[4]) {
            py_str(&row[4])
        } else {
            String::new()
        };
        int_or_zero(&row[7]).map_err(|e| traceback(45, &e))?;
        all.push(Play {
            username,
            item: str_or(&row[2], "unknown"),
            kind: str_or(&row[3], "unknown"),
            transcode: method.to_lowercase().contains("transcode"),
            method,
            client: str_or(&row[5], "unknown"),
            device: str_or(&row[6], "unknown"),
        });
    }
    let transcodes = all.iter().filter(|p| p.transcode).count();
    let mut active: Vec<Play> = Vec::new();
    for p in &all {
        if !active.iter().any(|a| a.username == p.username) {
            active.push(p.clone());
        }
    }
    let active_count = active.len();
    active.truncate(10);
    Ok((active_count, transcodes, active))
}

/// `resource_block` of the script: load, memory and filesystem lines (empty lines dropped).
pub fn resource_block() -> Result<String, ()> {
    let mut lines: Vec<String> = Vec::new();
    let load = fs::read_to_string("/proc/loadavg").ok().map(|t| {
        t.split(' ')
            .take(3)
            .collect::<Vec<_>>()
            .join(" ")
            .trim_end_matches('\n')
            .to_string()
    });
    lines.push(format!(
        "Host load: {}",
        load.unwrap_or_else(|| "n/a".into())
    ));
    let (mut total, mut avail) = (0f64, 0f64);
    if let Ok(meminfo) = fs::read_to_string("/proc/meminfo") {
        for line in meminfo.lines() {
            let mut f = line.split_whitespace();
            match (f.next(), f.next().and_then(|v| v.parse::<f64>().ok())) {
                (Some("MemTotal:"), Some(v)) => total = v,
                (Some("MemAvailable:"), Some(v)) => avail = v,
                _ => {}
            }
        }
    }
    if total > 0.0 {
        let used = total - avail;
        lines.push(format!(
            "Host mem: {:.1}/{:.1} GiB ({:.0}%)",
            used / 1_048_576.0,
            total / 1_048_576.0,
            (used * 100.0) / total
        ));
    } else {
        lines.push("Host mem: n/a".into());
    }
    for (dir, label) in [
        ("/srv/jellyfin", "Config fs"),
        ("/mnt/jellyfin-transcodes", "Transcode fs"),
    ] {
        let out = Command::new("df")
            .args(["-h", dir])
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|_| ())?;
        if !out.status.success() && label == "Transcode fs" {
            return Err(()); // set -o pipefail: de laatste `df | awk` faalt, het script stopt stil
        }
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(second) = text.lines().nth(1) {
            let f: Vec<&str> = second.split_whitespace().collect();
            let g = |i: usize| f.get(i).copied().unwrap_or("");
            lines.push(format!("{label}: {}/{} ({})", g(2), g(1), g(4)));
        }
    }
    Ok(lines
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn build_message(
    lookback: &str,
    active_count: usize,
    transcodes: usize,
    active: &[Play],
    resource: &str,
) -> String {
    let mut lines = vec![
        format!("Jellyfin activity ({lookback}m window)"),
        format!("Active users: {active_count} | Transcodes: {transcodes}"),
        String::new(),
    ];
    for r in active {
        let marker = if r.transcode { "TRANSCODE" } else { "DIRECT" };
        lines.push(format!(
            "- {} -> {} [{}] ({marker}; {}; {}/{})",
            r.username, r.item, r.kind, r.method, r.client, r.device
        ));
    }
    if !resource.is_empty() {
        lines.push(String::new());
        lines.push(resource.to_string());
    }
    lines.join("\n")
}

pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Everything up to (not including) the HTTP request.
pub fn plan(env: &Env, now_micros: i64, resource: &dyn Fn() -> Result<String, ()>) -> Outcome {
    if env.ntfy_topic.is_none() {
        return Outcome::Fail { code: 1, stderr: "jellyfin-watch-notify.sh: line 5: NTFY_TOPIC: zet NTFY_TOPIC, bv. https://ntfy.sh/<jouw-topic>\n".into() };
    }
    if !Path::new(&env.playback_db).is_file() || !Path::new(&env.jelly_db).is_file() {
        return Outcome::Quiet;
    }
    if let Some(parent) = Path::new(&env.state_file)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        let _ = fs::create_dir_all(parent);
    }
    let last_hash = fs::read_to_string(&env.state_file)
        .map(|t| t.trim_end_matches('\n').to_string())
        .unwrap_or_default();
    let (active_count, transcodes, active) = match collect(env, now_micros) {
        Ok(r) => r,
        Err(stderr) => return Outcome::Fail { code: 1, stderr },
    };
    if active_count == 0 {
        return Outcome::Quiet;
    }
    let Ok(resource) = resource() else {
        return Outcome::Fail {
            code: 1,
            stderr: String::new(),
        };
    };
    let message = build_message(
        &env.lookback.trim().replace('_', ""),
        active_count,
        transcodes,
        &active,
        &resource,
    );
    let hash = sha256_hex(&message);
    if hash == last_hash {
        Outcome::Quiet
    } else {
        Outcome::Send { message, hash }
    }
}

pub fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}

/// The whole script, with a real HTTP POST (`curl -fsS -X POST`).
pub fn run(env: &Env) -> i32 {
    match plan(env, now_micros(), &resource_block) {
        Outcome::Quiet => 0,
        Outcome::Fail { code, stderr } => {
            let _ = std::io::stderr().write_all(stderr.as_bytes());
            code
        }
        Outcome::Send { message, hash } => {
            let Some(topic) = env.ntfy_topic.as_deref() else {
                return 1;
            };
            match post(topic, &message) {
                Ok(()) => match fs::write(&env.state_file, hash) {
                    Ok(()) => 0,
                    Err(_) => 1,
                },
                Err(code) => code,
            }
        }
    }
}

fn post(topic: &str, message: &str) -> Result<(), i32> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .proxy(None)
        .build()
        .into();
    let resp = agent
        .post(topic)
        .header("Title", "Jellyfin watcher")
        .header("Priority", "default")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(message.as_bytes());
    match resp {
        Ok(r) if r.status().as_u16() < 400 => Ok(()),
        Ok(r) => {
            eprintln!(
                "curl: (22) The requested URL returned error: {}",
                r.status().as_u16()
            );
            Err(22)
        }
        Err(e) => {
            eprintln!("curl: (7) {e}");
            Err(7)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn micros(ts: &str) -> i64 {
        parse_iso(ts).unwrap().unwrap()
    }

    #[test]
    fn iso_parsing_matches_python() {
        assert_eq!(micros("1970-01-01T00:00:01"), 1_000_000);
        assert_eq!(micros("1970-01-01 00:00:01.5"), 1_500_000);
        assert_eq!(
            micros("2024-02-29T12:00:00Z"),
            micros("2024-02-29T12:00:00")
        );
        assert_eq!(micros("1970-01-02"), 86_400_000_000);
        assert!(parse_iso("2023-02-29T00:00:00").is_none());
        assert!(parse_iso("niet-een-datum").is_none());
        assert!(parse_iso("2024-01-01T25:00:00").is_none());
        assert_eq!(parse_iso("2024-01-01T10:00:00+02:00"), Some(Err(())));
    }

    #[test]
    fn message_layout() {
        let play = Play {
            username: "alice".into(),
            item: "Film".into(),
            kind: "Movie".into(),
            method: "Transcode".into(),
            transcode: true,
            client: "Web".into(),
            device: "Fox".into(),
        };
        let m = build_message("15", 1, 1, &[play], "Host load: n/a");
        assert_eq!(
            m,
            "Jellyfin activity (15m window)\nActive users: 1 | Transcodes: 1\n\n- alice -> Film [Movie] (TRANSCODE; Transcode; Web/Fox)\n\nHost load: n/a"
        );
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn python_float_repr_for_text_columns() {
        assert_eq!(py_str(&Value::Real(1.5)), "1.5");
        assert_eq!(py_str(&Value::Real(1e21)), "1e+21");
        assert_eq!(str_or(&Value::Integer(0), "unknown"), "unknown");
        assert_eq!(str_or(&Value::Integer(7), "unknown"), "7");
    }

    #[test]
    fn bad_lookback_gives_the_python_traceback() {
        let env = Env {
            ntfy_topic: Some("x".into()),
            exclude_users: String::new(),
            lookback: "abc".into(),
            state_file: "s".into(),
            playback_db: "p".into(),
            jelly_db: "j".into(),
        };
        let err = collect(&env, 0).unwrap_err();
        assert!(err.ends_with("ValueError: invalid literal for int() with base 10: 'abc'\n"));
    }
}
