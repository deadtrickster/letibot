//! `letibot browser` - the local bridge the Chrome extension asks sessions through
//! (`docs/browser.md`). The config is `[browser]` in the user's `leticode.toml` only. The verbs
//! install and drive its socket activation (launchd on macOS, systemd --user on Linux).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use letibot_activation::control::{self, Verb};
use letibot_activation::state::{self, State};
use letibot_activation::{Manager, Spec};
use letibot_bridge::browser as bridge;
use letibot_harnessd::leticode_config::{self, LeticodeConfig, Level};

const USAGE: &str = "\
letibot browser [--port N]     run the bridge in the foreground (debugging; default port 8790)
letibot browser install [--port N]
                               install socket activation: the bridge starts on the first
                               connection and exits after 10 idle minutes. Makes the token
                               if missing and prints it once, with the setup steps.
letibot browser uninstall      unload and remove the agent or units
letibot browser start          run it now
letibot browser stop           stop the running bridge; activation keeps listening
letibot browser restart        stop and start, running a rebuilt binary
letibot browser off | on       disable or enable activation entirely
letibot browser status         installed, activation, running (pid, binary, staleness), grants
letibot browser logs           follow the log
  The token is ~/.config/letibot/browser.token, mode 0600.
  Config: `[browser] port` and `extension_id` in ~/.config/letibot/leticode.toml.";

#[derive(Debug, PartialEq, Eq)]
enum Cmd {
    Help,
    Foreground { port: Option<u16> },
    Verb { verb: Verb, port: Option<u16> },
}

fn parse(args: &[String]) -> Result<Cmd, String> {
    let mut port = None;
    let mut verb = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--help" | "-h" => return Ok(Cmd::Help),
            "--port" => match it.next().and_then(|p| p.parse::<u16>().ok()) {
                Some(p) if p > 0 => port = Some(p),
                _ => return Err("--port wants a number 1-65535".into()),
            },
            w => match Verb::parse(w) {
                Some(v) if verb.is_none() => verb = Some(v),
                Some(_) => return Err(format!("one verb at a time: {w}")),
                None => return Err(format!("no such argument: {w}")),
            },
        }
    }
    match verb {
        None => Ok(Cmd::Foreground { port }),
        Some(v) if port.is_some() && v != Verb::Install => {
            Err("--port goes with `install` or a foreground run".into())
        }
        Some(verb) => Ok(Cmd::Verb { verb, port }),
    }
}

pub fn run(args: &[String]) -> ExitCode {
    let cmd = match parse(args) {
        Ok(Cmd::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(c) => c,
        Err(why) => {
            eprintln!("letibot browser: {why}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match go(cmd) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("letibot browser: {why}");
            ExitCode::from(1)
        }
    }
}

fn go(cmd: Cmd) -> Result<(), String> {
    let user_file = leticode_config::user_path();
    let cfg = LeticodeConfig::load_as(&user_file, Level::User)?;
    let flag = match &cmd {
        Cmd::Foreground { port } | Cmd::Verb { port, .. } => *port,
        Cmd::Help => None,
    };
    let port = flag.or(cfg.browser.port).unwrap_or(bridge::DEFAULT_PORT);
    let id = cfg
        .browser
        .extension_id
        .unwrap_or_else(|| bridge::EXTENSION_ID.to_string());
    let token = user_file
        .parent()
        .map(|d| d.join("browser.token"))
        .unwrap_or_else(|| "browser.token".into());
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("no $HOME")?);
    let m = Manager::native();
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let spec = bridge::spec(port, &program(), env(m), &home);
    match cmd {
        Cmd::Help => Ok(()),
        Cmd::Foreground { .. } => {
            if letibot_activation::activated().is_none() {
                let st = control::status(m, &spec, &home, uid);
                if st.on {
                    return Err(refusal(port, st.pid));
                }
            }
            bridge::run(port, &id, &token)
        }
        Cmd::Verb { verb, .. } => verb_run(verb, m, &spec, &home, uid, &token),
    }
}

/// The binary the agent runs: this one, resolved, so a rebuild in place is what `restart` runs.
fn program() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| "letibot".into());
    exe.canonicalize().unwrap_or(exe)
}

/// What the agent's environment must carry over from this shell for the bridge to find the
/// same store and daemons. systemd sets its own `XDG_RUNTIME_DIR`.
fn env(m: Manager) -> Vec<(String, String)> {
    let mut keys = vec!["LETIBOT_STORE"];
    if m == Manager::Launchd {
        keys.push("XDG_RUNTIME_DIR");
    }
    keys.into_iter()
        .filter_map(|k| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| (k.to_string(), v))
        })
        .collect()
}

fn refusal(port: u16, pid: Option<u32>) -> String {
    let running = pid.map_or(String::new(), |p| format!(", and it runs now as pid {p}"));
    format!(
        "activation holds 127.0.0.1:{port} and the session socket{running}. `letibot browser \
         stop` stops the running bridge but activation keeps the sockets; `letibot browser off` \
         frees them for a foreground run, and `letibot browser on` gives them back."
    )
}

fn verb_run(
    verb: Verb,
    m: Manager,
    spec: &Spec,
    home: &Path,
    uid: u32,
    token: &Path,
) -> Result<(), String> {
    if verb == Verb::Status {
        print_status(m, spec, home, uid);
        return Ok(());
    }
    if verb == Verb::Install {
        let (t, created) = bridge::token(token)?;
        control::execute(&control::plan(verb, m, spec, home, uid))?;
        eprintln!("{}", bridge::setup_text(token, created.then_some(&*t)));
        eprintln!(
            "letibot browser: installed; the bridge starts on the first connection to {}",
            listen_line(spec)
        );
        return Ok(());
    }
    if !control::status(m, spec, home, uid).installed {
        return Err("not installed: `letibot browser install` first".into());
    }
    control::execute(&control::plan(verb, m, spec, home, uid))?;
    if verb != Verb::Logs {
        let st = control::status(m, spec, home, uid);
        eprintln!(
            "letibot browser: activation {}, {}",
            if st.on { "on" } else { "off" },
            st.pid
                .map_or("not running".to_string(), |p| format!("running as pid {p}"))
        );
    }
    Ok(())
}

fn listen_line(spec: &Spec) -> String {
    spec.sockets
        .iter()
        .map(|s| match s {
            letibot_activation::Socket::Tcp { host, port, .. } => format!("{host}:{port}"),
            letibot_activation::Socket::Unix { path, .. } => path.display().to_string(),
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

fn ago(secs: u64) -> String {
    match secs {
        0..60 => "under a minute ago".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..172_800 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

fn print_status(m: Manager, spec: &Spec, home: &Path, uid: u32) {
    let st = control::status(m, spec, home, uid);
    let files = spec.files(m, home);
    let now = state::now();
    let mut out = Vec::new();
    let mut row = |k: &str, v: String| out.push(format!("{k:<11} {v}"));
    if !st.installed {
        row("installed", "no - `letibot browser install`".into());
    } else {
        let names: Vec<String> = files.iter().map(|(p, _)| p.display().to_string()).collect();
        row("installed", format!("yes, {}", names.join(", ")));
        for s in &st.stale {
            row(
                "",
                format!("stale: {s} - `letibot browser install` rewrites it"),
            );
        }
        row(
            "activation",
            if st.on {
                "on".into()
            } else {
                "off - `letibot browser on`".into()
            },
        );
        match st.pid {
            None => row("running", "no; the next connection starts it".into()),
            Some(pid) => {
                let note = State::read(&bridge::state_path())
                    .filter(|s| s.pid == pid && state::alive(pid));
                match note {
                    None => row("running", format!("pid {pid}")),
                    Some(s) => {
                        row(
                            "running",
                            format!(
                                "pid {pid}, {}, started {}",
                                s.exe.display(),
                                ago(now.saturating_sub(s.started))
                            ),
                        );
                        if s.stale() {
                            let built = state::mtime(&s.exe).unwrap_or(now);
                            row(
                                "",
                                format!(
                                    "STALE: the binary was rebuilt {} - `letibot browser \
                                     restart` runs it",
                                    ago(now.saturating_sub(built))
                                ),
                            );
                        } else {
                            row(
                                "",
                                format!(
                                    "binary built {}, the one running",
                                    ago(now.saturating_sub(s.exe_mtime))
                                ),
                            );
                        }
                    }
                }
            }
        }
    }
    let me = program();
    let built = state::mtime(&me).map_or("?".into(), |t| ago(now.saturating_sub(t)));
    row("this", format!("{} (built {built})", me.display()));
    for s in &spec.sockets {
        match s {
            letibot_activation::Socket::Tcp { port, .. } => row("port", port.to_string()),
            letibot_activation::Socket::Unix { path, .. } => {
                row("socket", path.display().to_string());
                if st.pid.is_some()
                    && let Some(n) = bridge::tab::ask_status(path)
                {
                    row("grants", format!("{n} open"));
                }
            }
        }
    }
    row(
        "log",
        match m {
            Manager::Launchd => spec.log.display().to_string(),
            Manager::Systemd => format!("journalctl --user -u {}", spec.unit),
        },
    );
    println!("{}", out.join("\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn arguments_parse_to_one_command() {
        assert_eq!(parse(&a(&[])), Ok(Cmd::Foreground { port: None }));
        assert_eq!(
            parse(&a(&["--port", "9000"])),
            Ok(Cmd::Foreground { port: Some(9000) })
        );
        assert_eq!(parse(&a(&["-h"])), Ok(Cmd::Help));
        assert_eq!(
            parse(&a(&["install", "--port", "9001"])),
            Ok(Cmd::Verb {
                verb: Verb::Install,
                port: Some(9001)
            })
        );
        for (w, v) in Verb::ALL {
            assert_eq!(
                parse(&a(&[w])),
                Ok(Cmd::Verb {
                    verb: *v,
                    port: None
                })
            );
        }
        assert!(parse(&a(&["restart", "--port", "1"])).is_err());
        assert!(parse(&a(&["start", "stop"])).is_err());
        assert!(parse(&a(&["--port", "0"])).is_err());
        assert!(parse(&a(&["launch"])).unwrap_err().contains("no such"));
    }

    #[test]
    fn the_double_bind_refusal_names_stop_and_off() {
        let r = refusal(8790, Some(12));
        assert!(r.contains("`letibot browser stop`"), "{r}");
        assert!(r.contains("`letibot browser off`"), "{r}");
        assert!(r.contains("pid 12"), "{r}");
    }

    #[test]
    fn the_usage_names_every_verb() {
        for (w, _) in Verb::ALL {
            assert!(
                USAGE.contains(&format!("browser {w}")) || USAGE.contains(&format!("| {w}")),
                "{w}"
            );
        }
    }
}
