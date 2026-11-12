//! `letibot where` - which daemon serves a folder, in the bridge's own spelling.
//!
//! A program that drives letibot from outside (rano's editor panel) must not re-derive
//! `<rundir>/<sha256(folder)[..12]>.sock`: that rule is `bridge::daemon`'s, and a second
//! spelling of it names a socket no daemon ever listened on. `--ensure` starts the daemon when
//! nothing is listening, and never touches one that is.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use letibot_bridge::daemon;
use letibot_tools::builtins::task::Placement;

const USAGE: &str = "\
letibot where [FOLDER] [--ensure] [--json]
  The socket this folder's daemon listens on, as `bridge::daemon::resolve` computes it:
  `$LETIBOT_SOCKET` when set, else `$XDG_RUNTIME_DIR/letibot/<sha256(folder)[..12]>.sock`. And
  whether a daemon is live there. A program that drives letibot asks this instead of hashing
  the path itself.

  FOLDER     the folder (default: this one); its git toplevel, as the launcher resolves it
  --ensure   start the folder's daemon when none is running, then answer
  --json     one object: socket, live, project (the key), folder

  The plain form is two lines: the socket, then `up` or `down`.

  example:
    letibot --where --ensure --json      (the launcher's own spelling)
    letibot where --json                 (the binary's)";

/// What `--ensure` starts a daemon as. The launcher's own defaults, and the tools run here: a
/// daemon already up is neither asked for these nor refused over them.
const MODE: &str = "writes-allowed";

pub fn run(args: &[String]) -> ExitCode {
    let mut ensure = false;
    let mut json = false;
    let mut folder: Option<PathBuf> = None;
    for a in args {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--ensure" => ensure = true,
            "--json" => json = true,
            other if other.starts_with('-') => {
                return usage_error(&format!("no such flag: {other}"));
            }
            other if folder.is_some() => {
                return usage_error(&format!(
                    "where takes one folder, and it already has {other}"
                ));
            }
            other => folder = Some(PathBuf::from(other)),
        }
    }
    let folder = match folder {
        Some(f) => f,
        None => match std::env::current_dir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("letibot where: there is no current directory to ask about: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    match answer(&folder, ensure, json) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("letibot where: {why}");
            ExitCode::FAILURE
        }
    }
}

/// The answer itself: the resolved daemon, started first when asked and nothing is listening.
fn answer(folder: &Path, ensure: bool, json: bool) -> Result<(), String> {
    let mut d = daemon::resolve(folder)?;
    if ensure && !daemon::is_up(&d.socket) {
        // Only when nothing is listening: `daemon::ensure` refuses a live daemon placed
        // somewhere else, and a daemon somebody else started is not this call's to move.
        let started = daemon::ensure(folder, None, MODE, &Placement::Host)?;
        if started.started {
            eprintln!(
                "letibot where: started a daemon for {} ({})",
                started.workspace.display(),
                started.socket.display()
            );
        }
        d = started;
    }
    // Asked after the start, so a daemon that did not come up says `down` rather than lying.
    let live = daemon::is_up(&d.socket);
    if json {
        let object = serde_json::json!({
            "socket": d.socket.display().to_string(),
            "live": live,
            "project": d.key,
            "folder": d.workspace.display().to_string(),
        });
        println!("{object}");
    } else {
        println!("{}", d.socket.display());
        println!("{}", if live { "up" } else { "down" });
    }
    Ok(())
}

fn usage_error(why: &str) -> ExitCode {
    eprintln!("letibot where: {why}\n{USAGE}");
    ExitCode::from(2)
}
