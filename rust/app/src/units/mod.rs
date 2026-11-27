//! De twee gepoorte scripts als subcommando's. Alles anders valt terug op het bash-origineel.

use std::io::Write;
use std::process::ExitCode;
use std::time::SystemTime;

use crate::{cache_fix, watch_notify};

/// `cache-fix.sh [--dry-run] [--keep-log-days N] [--keep-transcode-count N]`.
pub fn cache_fix(args: &[String]) -> ExitCode {
    let parsed = match cache_fix::parse_args(args) {
        cache_fix::Parsed::Exit { stdout, code } => {
            print!("{stdout}");
            return ExitCode::from(code as u8);
        }
        cache_fix::Parsed::Run(a) => a,
    };
    if !cache_fix::is_root() {
        println!("Dit script moet als root draaien (sudo).");
        return ExitCode::from(1);
    }
    let cfg = cache_fix::Config {
        dry_run: parsed.dry_run,
        keep_log_days: parsed.keep_log_days,
        keep_transcode_count: parsed.keep_transcode_count,
        ..cache_fix::Config::default()
    };
    let mut out = std::io::stdout().lock();
    let code = cache_fix::run(&cfg, SystemTime::now(), &mut out);
    let _ = out.flush();
    ExitCode::from(code as u8)
}

/// `jellyfin-watch-notify.sh` (configured through environment variables, like the original).
pub fn watch_notify() -> ExitCode {
    ExitCode::from(watch_notify::run(&watch_notify::Env::from_process()) as u8)
}
