//! **Asking the room's supervisor to restart it** — the door the pane's restart control goes
//! through, and why it is not the coordinator.
//!
//! `docs/room.md` § *Still owed* 1 is the operator's own words: *"and since we are going full blown
//! agents orchestration again — that panel must explicitly let me to restart the server"*. The same
//! list draws the line this module is built on: the restart is **asked of the daemon that
//! supervises the coordinator and not performed by the coordinator on itself**.
//!
//! # What the supervisor IS on this box, measured
//!
//! A coordinator is a whole `harnessd --room`, started **detached** by the launcher
//! (`scripts/letibot`'s `room_ensure`, which runs before any run path opens a session) with its
//! socket, its lease and its log named from the HOST. Nothing else on this box starts one, and
//! nothing resident watches it: the launcher is a program, and its two verbs are `--room` (make sure
//! one is serving) and `--room --stop`. So the supervisor is **the launcher**, and asking it is
//! running it — `--room --stop`, then `--room`. That is also what keeps the room VISIBLE: the
//! launcher writes the record `letibot --ls` and `letibot --stop` read, so a room the pane restarted
//! is still a room the operator can see and stop. A daemon that spawned `harnessd --room` itself
//! would leave a coordinator with no record, which is a second way for a room to become invisible.
//!
//! # Why the coordinator cannot do it
//!
//! Two reasons, and the first is the one the operator would feel. **The pane is fed by the instance
//! it would kill**: the coordinator holds the connection that draws the queues, so a coordinator
//! that stopped itself mid-act could not say what happened — the person would be left looking at a
//! frozen pane, unable to tell a restart from a hang. The second is the room's own design: it keeps
//! no store, so a coordinator that restarted itself would also have to be the thing that decided it
//! should exist, which is the lease's job ([`letibot_room::CoordinatorLease`]) and not a room verb's.
//!
//! For the same reason the **coordinator's own daemon is never given a supervisor**
//! (`coordinator::configure` clears the flag): the arbiter's pane is drawn by the process a restart
//! would kill.
//!
//! # The frame
//!
//! [`RestartFrame`] is the act: when it was asked, by which session, and whether anything was
//! running to stop. It is handed to [`RoomSupervisor::restart`], and the production implementation
//! writes it as one JSON line on the launcher's stdin — a launcher ignores it, and a supervisor
//! that wants to record *why* it was asked has it without a second channel. The same frame is what
//! a test asserts on: a control whose act could not be observed is a control nobody can review.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// **How long one launcher verb may take.** The launcher's own stop waits ten seconds for the
/// coordinator to go and its start waits ten for the socket to bind, so a minute is generous by a
/// factor of three and still bounds the act — a person pressing a key must not be left guessing.
pub const RESTART_WITHIN: Duration = Duration::from_secs(60);

/// **Where the launcher is, when it is not on `PATH`** — a test's door, and the reason no fixture
/// has to run the box's real launcher (which would stop the operator's room).
pub const SUPERVISOR_ENV: &str = "LETIBOT_ROOM_SUPERVISOR";

/// **The act, as the supervisor is handed it** — and as a test asserts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartFrame {
    /// When it was asked. The room's one clock, milliseconds since the epoch.
    pub at_ms: u64,
    /// The session whose pane it was. Empty when the daemon asked for itself.
    pub by: String,
    /// **Was there a room to stop?** The difference between a restart and a start, and the reason
    /// the marker's cause can say *the key did not empty the queue — the room had already gone*.
    pub was_running: bool,
    /// The words the person's key stands for.
    pub why: String,
}

impl RestartFrame {
    /// **The frame on the wire** — one line of JSON, which is what the supervisor reads on stdin.
    ///
    /// A line rather than fields on an argv: an argv is a list of words with no names, and the
    /// supervisor's first two words are already spoken for (`--room --stop`).
    pub fn line(&self) -> String {
        let value = serde_json::json!({
            "verb": "restart",
            "at_ms": self.at_ms,
            "by": self.by,
            "was_running": self.was_running,
            "why": self.why,
        });
        format!("{value}\n")
    }
}

/// **The door a pane's restart control goes through.** One implementation in production (the
/// launcher); a test stands in its own, which is what makes *the act is a frame* assertable with no
/// process and no socket.
pub trait RoomSupervisor: Send + Sync {
    /// Restart the box's room. `Ok` is a sentence saying what happened; `Err` is the refusal, in
    /// words a person can act on — and it is PUBLISHED (`room_restart_refused`), never silence.
    fn restart(&self, frame: &RestartFrame) -> Result<String, String>;
}

/// **The box's launcher, asked with the two verbs a person would type.**
pub struct Launcher {
    program: PathBuf,
    within: Duration,
}

impl std::fmt::Debug for Launcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launcher")
            .field("program", &self.program)
            .finish()
    }
}

impl Launcher {
    pub fn new(program: impl Into<PathBuf>) -> Launcher {
        Launcher {
            program: program.into(),
            within: RESTART_WITHIN,
        }
    }

    /// **`$LETIBOT_ROOM_SUPERVISOR`, else `letibot` on `PATH`** — `None` on a box with neither,
    /// which is a box where the control cannot be offered (and the pane says so rather than
    /// drawing a key that would refuse).
    pub fn found() -> Option<Launcher> {
        if let Some(named) = std::env::var_os(SUPERVISOR_ENV).filter(|v| !v.is_empty()) {
            let path = PathBuf::from(named);
            return path.is_file().then(|| Launcher::new(path));
        }
        on_path("letibot").map(Launcher::new)
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    fn say(&self, verb: &str, ended: letibot_tools::rpc::Ended) -> Result<(), String> {
        use letibot_tools::rpc::Ended;
        match ended {
            Ended::Answered(out) if out.status.success() => Ok(()),
            Ended::Answered(out) => Err(format!(
                "`{} {verb}` answered {}: {}",
                self.program.display(),
                out.status,
                tail(&out)
            )),
            Ended::TimedOut => Err(format!(
                "`{} {verb}` did not finish within {}s, so what the room is doing now is unknown",
                self.program.display(),
                self.within.as_secs()
            )),
            Ended::Unstartable(why) => Err(format!(
                "the room's supervisor could not be run: {why} — the launcher is what starts a \
                 coordinator (`letibot --room`)"
            )),
        }
    }
}

impl RoomSupervisor for Launcher {
    fn restart(&self, frame: &RestartFrame) -> Result<String, String> {
        // **The frame goes to the STOP**, which is where the act begins: a supervisor that wants
        // the note reads it there, and the launcher ignores its stdin entirely.
        let stop = letibot_tools::rpc::run_bounded_fed(
            &self.program,
            &["--room".to_string(), "--stop".to_string()],
            self.within,
            frame.line().as_bytes(),
        );
        self.say("--room --stop", stop)?;
        let start = letibot_tools::rpc::run_bounded(
            &self.program,
            &["--room".to_string()],
            self.within,
        );
        self.say("--room", start)?;
        Ok(format!(
            "the room was stopped and started again by `{}`",
            self.program.display()
        ))
    }
}

/// **The last word a program left**, on one line and bounded — a launcher's own sentence is the
/// refusal, and a refusal nobody can read is the silence this module exists to remove.
fn tail(out: &std::process::Output) -> String {
    let text = if out.stderr.iter().any(|b| !b.is_ascii_whitespace()) {
        String::from_utf8_lossy(&out.stderr).to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let line = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("and it said nothing");
    if line.chars().count() > 240 {
        format!("{}…", line.chars().take(239).collect::<String>())
    } else {
        line.to_string()
    }
}

/// **Is `name` a program on `PATH`?** A path walk rather than a shell, because the answer is asked
/// inside `Harness::open` and a `sh -c` per session open is a fork nobody needs.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// **The frame is one line, and it carries every field** — the act is observable, which is what
    /// makes *the control was pressed* a fact rather than a guess.
    #[test]
    fn the_frame_is_one_line_of_json() {
        let frame = RestartFrame {
            at_ms: 1_700_000_000_000,
            by: "s-a".to_string(),
            was_running: true,
            why: "pressed restart in the room pane".to_string(),
        };
        let line = frame.line();
        assert_eq!(line.lines().count(), 1, "one line: {line:?}");
        let value: serde_json::Value = serde_json::from_str(&line).expect("it is JSON");
        assert_eq!(value["verb"], serde_json::json!("restart"));
        assert_eq!(value["at_ms"], serde_json::json!(1_700_000_000_000u64));
        assert_eq!(value["by"], serde_json::json!("s-a"));
        assert_eq!(value["was_running"], serde_json::json!(true));
        assert_eq!(
            value["why"],
            serde_json::json!("pressed restart in the room pane")
        );
    }

    /// **The launcher is asked with the two verbs a person would type, and the frame goes to the
    /// stop.** A stand-in script records both the argv and its stdin, so what is asserted is the
    /// frame the supervisor RECEIVED rather than the fact that something was run.
    #[test]
    fn the_launcher_is_asked_with_both_verbs_and_handed_the_frame() {
        let dir = std::env::temp_dir().join(format!("letibot-room-sup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("calls");
        let script = dir.join("letibot");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 echo \"$*\" >> '{log}'\n\
                 cat >> '{log}'\n\
                 exit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let frame = RestartFrame {
            at_ms: 42,
            by: "s-a".to_string(),
            was_running: false,
            why: "pressed restart in the room pane".to_string(),
        };
        let said = Launcher::new(&script)
            .restart(&frame)
            .expect("the stand-in launcher answers");
        assert!(said.contains("stopped and started again"), "{said}");

        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "--room --stop", "{lines:?}");
        // **The frame is the STOP's stdin**, which is where the act begins: a supervisor that
        // wants the note reads it there, and the launcher ignores its stdin entirely.
        assert_eq!(
            lines[1],
            frame.line().trim_end(),
            "the frame is the supervisor's stdin: {lines:?}"
        );
        assert_eq!(lines[2], "--room", "{lines:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A refusal is the program's own words**, said rather than swallowed — and it stops before
    /// the start, because a stop that did not stop is not a restart.
    #[test]
    fn a_launcher_that_refuses_is_a_sentence_and_not_a_silence() {
        let dir = std::env::temp_dir().join(format!("letibot-room-sup-no-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("letibot");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'no room on this box' >&2\nexit 1\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let frame = RestartFrame {
            at_ms: 1,
            by: "s-a".to_string(),
            was_running: true,
            why: "pressed".to_string(),
        };
        let why = Launcher::new(&script)
            .restart(&frame)
            .expect_err("a failed stop is not a restart");
        assert!(why.contains("--room --stop"), "{why}");
        assert!(why.contains("no room on this box"), "{why}");

        // And a program that is not there is named, with the door in the sentence.
        let why = Launcher::new(dir.join("nothing-here"))
            .restart(&frame)
            .expect_err("no program, no restart");
        assert!(why.contains("could not be run"), "{why}");
        assert!(why.contains("letibot --room"), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A supervisor is a door a test can stand in** — the trait's own reason for existing.
    #[test]
    fn a_fixture_supervisor_records_the_frame_it_was_handed() {
        struct Recorder(Mutex<Vec<RestartFrame>>);
        impl RoomSupervisor for Recorder {
            fn restart(&self, frame: &RestartFrame) -> Result<String, String> {
                self.0.lock().unwrap().push(frame.clone());
                Err("not this one".to_string())
            }
        }
        let rec = Recorder(Mutex::new(Vec::new()));
        let frame = RestartFrame {
            at_ms: 7,
            by: "s-b".to_string(),
            was_running: true,
            why: "pressed".to_string(),
        };
        assert!(rec.restart(&frame).is_err());
        assert_eq!(rec.0.lock().unwrap().as_slice(), &[frame]);
    }
}
