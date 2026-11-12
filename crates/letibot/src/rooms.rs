//! `letibot rooms` - the box's room coordinator (`harnessd --room`, docs/room.md). Bare it runs
//! the coordinator in the foreground; the verbs install and drive its socket activation, the
//! same ones `letibot browser` has.

use std::path::PathBuf;
use std::process::ExitCode;

use letibot_activation::control::{self, Verb};
use letibot_activation::state::{self, State};
use letibot_activation::{Manager, Spec};
use letibot_harnessd::room::{self as room, activation};

const USAGE: &str = "\
letibot rooms                  run the coordinator in the foreground (`harnessd --room`)
letibot rooms install          install socket activation: the coordinator starts on the first
                               connection to the room socket and stays up while it serves
letibot rooms uninstall        unload and remove the agent or units
letibot rooms start            run it now
letibot rooms stop             stop the running coordinator; activation keeps listening
letibot rooms restart          stop and start, running a rebuilt binary; sessions reconnect
letibot rooms off | on         disable or enable activation entirely
letibot rooms status           installed, activation, running (pid, binary, staleness), sessions
letibot rooms logs             follow the log
  Without activation, `letibot --room` starts a detached coordinator under the lease, as before.";

/// Exit 0 when activation serves this box's room: the launcher then dials instead of spawning.
const IS_ON: &str = "is-on";

#[derive(Debug, PartialEq, Eq)]
enum Cmd {
    Help,
    Foreground,
    IsOn,
    Verb(Verb),
}

fn parse(args: &[String]) -> Result<Cmd, String> {
    match args {
        [] => Ok(Cmd::Foreground),
        [a] if a == "--help" || a == "-h" => Ok(Cmd::Help),
        [a] if a == IS_ON => Ok(Cmd::IsOn),
        [a] => Verb::parse(a)
            .map(Cmd::Verb)
            .ok_or_else(|| format!("no such argument: {a}")),
        [_, b, ..] => Err(format!("one verb at a time: {b}")),
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
            eprintln!("letibot rooms: {why}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match go(cmd) {
        Ok(code) => code,
        Err(why) => {
            eprintln!("letibot rooms: {why}");
            ExitCode::from(1)
        }
    }
}

struct Here {
    m: Manager,
    spec: Spec,
    home: PathBuf,
    uid: u32,
    socket: PathBuf,
}

fn here() -> Result<Here, String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("no $HOME")?);
    let socket = room::socket_path().ok_or("this box has no data directory for the room socket")?;
    let m = Manager::native();
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let spec = activation::spec(&harnessd(), &socket, env(m), &home);
    Ok(Here {
        m,
        spec,
        home,
        uid,
        socket,
    })
}

fn go(cmd: Cmd) -> Result<ExitCode, String> {
    let h = here()?;
    match cmd {
        Cmd::Help => Ok(ExitCode::SUCCESS),
        Cmd::IsOn => {
            let st = control::status(h.m, &h.spec, &h.home, h.uid);
            Ok(if activation::launcher_spawns(activation::serves(&st)) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Cmd::Foreground => {
            let st = control::status(h.m, &h.spec, &h.home, h.uid);
            if activation::serves(&st) {
                return Err(activation::refusal(&h.socket, st.pid));
            }
            use std::os::unix::process::CommandExt;
            let e = std::process::Command::new(harnessd()).arg("--room").exec();
            Err(format!("{}: {e}", harnessd().display()))
        }
        Cmd::Verb(v) => verb_run(v, &h).map(|()| ExitCode::SUCCESS),
    }
}

/// The coordinator binary: `harnessd` beside this one, resolved, so a rebuild in place is what
/// `restart` runs.
fn harnessd() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| "letibot".into());
    let exe = exe.canonicalize().unwrap_or(exe);
    exe.with_file_name("harnessd")
}

/// What the agent's environment carries over from this shell, so the coordinator derives the
/// same socket, data and config paths. No API keys: those stay in `providers.toml`.
fn env(m: Manager) -> Vec<(String, String)> {
    let mut out = vec![("CUDA_VISIBLE_DEVICES".to_string(), String::new())];
    let mut keys = vec![room::SOCKET_ENV, "XDG_DATA_HOME", "XDG_CONFIG_HOME"];
    if m == Manager::Launchd {
        keys.extend(["XDG_RUNTIME_DIR", "PATH"]);
    }
    out.extend(keys.into_iter().filter_map(|k| {
        std::env::var(k)
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| (k.to_string(), v))
    }));
    out
}

/// A live coordinator that activation did not start, named by the lease. Installing over it
/// would leave two rooms on one name until it stopped.
fn stray(activated: Option<u32>) -> Option<letibot_room::Holder> {
    let lease = letibot_room::CoordinatorLease::new(room::lease_path()?);
    lease
        .read()
        .filter(|l| letibot_room::lease::pid_alive(l.pid) && Some(l.pid) != activated)
}

fn verb_run(verb: Verb, h: &Here) -> Result<(), String> {
    let (m, spec, home, uid) = (h.m, &h.spec, &h.home, h.uid);
    if verb == Verb::Status {
        print_status(h);
        return Ok(());
    }
    if verb == Verb::Install {
        let st = control::status(m, spec, home, uid);
        if let Some(l) = stray(st.pid) {
            return Err(format!(
                "a coordinator not under activation runs as pid {} ({}); `letibot --room --stop` \
                 it first, then install",
                l.pid, l.owner
            ));
        }
        if !harnessd().is_file() {
            return Err(format!("no harnessd at {}", harnessd().display()));
        }
        control::execute(&control::plan(verb, m, spec, home, uid))?;
        eprintln!(
            "letibot rooms: installed; the coordinator starts on the first connection to {}",
            h.socket.display()
        );
        eprintln!(
            "  the agent does not inherit this shell's keys: the tie-breaker's key must be in \
             ~/.config/letibot/providers.toml"
        );
        return Ok(());
    }
    if !control::status(m, spec, home, uid).installed {
        return Err("not installed: `letibot rooms install` first".into());
    }
    let before = control::status(m, spec, home, uid).pid;
    control::execute(&control::plan(verb, m, spec, home, uid))?;
    if verb == Verb::Stop
        && let Some(p) = before
    {
        // `launchctl kill` returns at once; a `start` right after must find it gone.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while state::alive(p) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    if verb != Verb::Logs {
        let st = control::status(m, spec, home, uid);
        eprintln!(
            "letibot rooms: activation {}, {}",
            if st.on { "on" } else { "off" },
            st.pid
                .map_or("not running".to_string(), |p| format!("running as pid {p}"))
        );
    }
    Ok(())
}

fn print_status(h: &Here) {
    let (m, spec, home, uid) = (h.m, &h.spec, &h.home, h.uid);
    let st = control::status(m, spec, home, uid);
    let now = state::now();
    let ago = |t: u64| crate::browser::ago(now.saturating_sub(t));
    let mut out = Vec::new();
    let mut row = |k: &str, v: String| out.push(format!("{k:<11} {v}"));
    if !st.installed {
        row("installed", "no - `letibot rooms install`".into());
    } else {
        let names: Vec<String> = spec
            .files(m, home)
            .iter()
            .map(|(p, _)| p.display().to_string())
            .collect();
        row("installed", format!("yes, {}", names.join(", ")));
        for s in &st.stale {
            row(
                "",
                format!("stale: {s} - `letibot rooms install` rewrites it"),
            );
        }
        row(
            "activation",
            if st.on {
                "on".into()
            } else {
                "off - `letibot rooms on`".into()
            },
        );
    }
    let note_path = activation::state_path(&h.socket);
    let note_text = std::fs::read_to_string(&note_path).unwrap_or_default();
    let note = State::parse(&note_text).filter(|s| state::alive(s.pid));
    // The lease names a coordinator activation did not start, too.
    let pid = st.pid.or(note.as_ref().map(|s| s.pid));
    match pid {
        None if st.on => row("running", "no; the next connection starts it".into()),
        None => row("running", "no".into()),
        Some(pid) => match note.filter(|s| s.pid == pid) {
            None => row("running", format!("pid {pid}")),
            Some(s) => {
                row(
                    "running",
                    format!("pid {pid}, {}, started {}", s.exe.display(), ago(s.started)),
                );
                if s.stale() {
                    let built = state::mtime(&s.exe).unwrap_or(now);
                    row(
                        "",
                        format!(
                            "STALE: the binary was rebuilt {} - `letibot rooms restart` runs it",
                            ago(built)
                        ),
                    );
                } else {
                    row(
                        "",
                        format!("binary built {}, the one running", ago(s.exe_mtime)),
                    );
                }
                if let Some(n) = activation::read_sessions(&note_text) {
                    row("sessions", format!("{n} connected"));
                }
                if st.pid.is_none() && st.on {
                    row("", "not started by activation".into());
                }
            }
        },
    }
    let me = harnessd();
    let built = state::mtime(&me).map_or("?".into(), ago);
    row("this", format!("{} (built {built})", me.display()));
    row("socket", h.socket.display().to_string());
    row(
        "log",
        match m {
            Manager::Launchd if st.installed => spec.log.display().to_string(),
            Manager::Systemd if st.installed => format!("journalctl --user -u {}", spec.unit),
            _ => "`letibot --room` logs to ~/logs/<room>.log".into(),
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
        assert_eq!(parse(&a(&[])), Ok(Cmd::Foreground));
        assert_eq!(parse(&a(&["-h"])), Ok(Cmd::Help));
        assert_eq!(parse(&a(&["is-on"])), Ok(Cmd::IsOn));
        for (w, v) in Verb::ALL {
            assert_eq!(parse(&a(&[w])), Ok(Cmd::Verb(*v)));
        }
        assert!(parse(&a(&["start", "stop"])).is_err());
        assert!(parse(&a(&["launch"])).unwrap_err().contains("no such"));
    }

    #[test]
    fn the_usage_names_every_verb() {
        for (w, _) in Verb::ALL {
            assert!(
                USAGE.contains(&format!("rooms {w}")) || USAGE.contains(&format!("| {w}")),
                "{w}"
            );
        }
    }

    #[test]
    fn the_agent_gets_no_api_key() {
        for (k, _) in env(Manager::Launchd) {
            assert!(!k.ends_with("_API_KEY"), "{k}");
        }
    }
}
