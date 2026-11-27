mod cache_fix;
mod original;
mod units;
mod watch_notify;

use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    original::report_fallbacks();
    let argv: Vec<OsString> = std::env::args_os().collect();
    let text: Option<Vec<String>> = argv
        .iter()
        .skip(1)
        .map(|a| a.to_str().map(str::to_string))
        .collect();
    match text.as_deref() {
        Some([cmd, rest @ ..]) if cmd == "watch-notify" && rest.is_empty() => units::watch_notify(),
        Some([cmd, rest @ ..]) if cmd == "cache-fix" => units::cache_fix(rest),
        _ => original::run(&argv[1..]),
    }
}
