//! `letibot browser` - the local bridge the Chrome extension asks sessions through
//! (`docs/browser.md`). The config is `[browser]` in the user's `leticode.toml` only.

use std::process::ExitCode;

use letibot_harnessd::leticode_config::{self, LeticodeConfig, Level};

const USAGE: &str = "\
letibot browser [--port N]   serve the Chrome extension on 127.0.0.1 (default port 8790)
  The token is ~/.config/letibot/browser.token, made 0600 and printed on the first run.
  Config: `[browser] port` and `extension_id` in ~/.config/letibot/leticode.toml.";

pub fn run(args: &[String]) -> ExitCode {
    let mut port: Option<u16> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--port" => match it.next().and_then(|p| p.parse::<u16>().ok()) {
                Some(p) if p > 0 => port = Some(p),
                _ => return usage_error("--port wants a number 1-65535"),
            },
            other => return usage_error(&format!("no such argument: {other}")),
        }
    }
    let user_file = leticode_config::user_path();
    let cfg = match LeticodeConfig::load_as(&user_file, Level::User) {
        Ok(c) => c,
        Err(why) => {
            eprintln!("letibot browser: {why}");
            return ExitCode::from(1);
        }
    };
    let port = port
        .or(cfg.browser.port)
        .unwrap_or(letibot_bridge::browser::DEFAULT_PORT);
    let id = cfg
        .browser
        .extension_id
        .unwrap_or_else(|| letibot_bridge::browser::EXTENSION_ID.to_string());
    let token = user_file
        .parent()
        .map(|d| d.join("browser.token"))
        .unwrap_or_else(|| "browser.token".into());
    match letibot_bridge::browser::run(port, &id, &token) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("letibot browser: {why}");
            ExitCode::from(1)
        }
    }
}

fn usage_error(why: &str) -> ExitCode {
    eprintln!("letibot browser: {why}\n{USAGE}");
    ExitCode::from(2)
}
