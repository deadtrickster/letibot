//! The coordinator under socket activation: launchd's `io.letibot.rooms` agent on macOS,
//! `letibot-rooms.socket` and `.service` under systemd --user. The manager holds the room socket
//! daemons dial, starts `harnessd --room` on the first connection and hands it the listener.

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use letibot_activation::control::{self, Status};
use letibot_activation::intake::{self, Listener};
use letibot_activation::state::State;
use letibot_activation::{Manager, Socket, Spec};

/// The launchd label and the systemd unit base name.
pub const LABEL: &str = "io.letibot.rooms";
pub const UNIT: &str = "letibot-rooms";
/// The `Sockets` key in the plist.
pub const SOCKET_NAME: &str = "room";

/// The agent or units for a coordinator on `socket`, run as `harnessd --room`. The path is
/// resolved at install, because launchd expands no variables.
pub fn spec(harnessd: &Path, socket: &Path, env: Vec<(String, String)>, home: &Path) -> Spec {
    Spec {
        label: LABEL.into(),
        unit: UNIT.into(),
        description: "letibot room coordinator".into(),
        program: vec![harnessd.to_string_lossy().into_owned(), "--room".into()],
        env,
        sockets: vec![Socket::Unix {
            name: SOCKET_NAME.into(),
            path: socket.to_path_buf(),
        }],
        log: log_path(home),
    }
}

/// launchd's log for the coordinator. Under systemd its stderr is in the journal.
pub fn log_path(home: &Path) -> PathBuf {
    home.join("Library/Logs/letibot/rooms.log")
}

/// Where a running coordinator notes its pid, binary and connected sessions, for `status`.
pub fn state_path(socket: &Path) -> PathBuf {
    socket.with_extension("state")
}

/// The activation note plus the sessions connected now, written whole.
pub fn write_state(path: &Path, state: &State, sessions: usize) -> std::io::Result<()> {
    let tmp = path.with_extension("state-tmp");
    std::fs::write(&tmp, format!("{}sessions={sessions}\n", state.to_text()))?;
    std::fs::rename(&tmp, path)
}

pub fn read_sessions(text: &str) -> Option<usize> {
    text.lines()
        .find_map(|l| l.strip_prefix("sessions="))
        .and_then(|n| n.trim().parse().ok())
}

/// Activation serves `spec`'s socket: the manager holds it, and the installed files name that
/// socket. Files for another socket (a test's `LETIBOT_ROOM_SOCKET`) are not this room's.
pub fn serves(st: &Status) -> bool {
    st.on && !st.stale.iter().any(|s| s.starts_with("the socket "))
}

/// What a `harnessd --room` start does about activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// Started by the manager: take its listener.
    Take(Manager),
    /// No activation for this socket: bind it, as without activation.
    Bind,
    /// Activation holds the socket, and this is a start by hand or by the launcher.
    Refuse(String),
}

pub fn gate(activated: Option<Manager>, serves: bool, socket: &Path, running: Option<u32>) -> Gate {
    match (activated, serves) {
        (Some(m), _) => Gate::Take(m),
        (None, true) => Gate::Refuse(refusal(socket, running)),
        (None, false) => Gate::Bind,
    }
}

/// Whether the launcher may lease and spawn a coordinator. Under activation it never does:
/// the first dial starts one, and a spawned one would be refused by [`gate`] anyway.
pub fn launcher_spawns(serves: bool) -> bool {
    !serves
}

pub fn refusal(socket: &Path, pid: Option<u32>) -> String {
    let running = pid.map_or(String::new(), |p| format!(", and it runs now as pid {p}"));
    format!(
        "activation holds the room socket {}{running}. `letibot rooms stop` stops the running \
         coordinator but activation keeps the socket; `letibot rooms off` frees it for a \
         foreground run, and `letibot rooms on` gives it back.",
        socket.display()
    )
}

/// The installed agent's status for this box, read from the files and the manager.
pub fn status_here(socket: &Path) -> Option<Status> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let m = Manager::native();
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let spec = spec(Path::new("harnessd"), socket, Vec::new(), &home);
    Some(control::status(m, &spec, &home, uid))
}

static TAKEN: Mutex<Option<UnixListener>> = Mutex::new(None);

/// Decide at configure time, before anything else opens an fd, and take the manager's
/// listener then. `Err` is the refusal or a failed intake.
pub fn admit(socket: &Path) -> Result<(), String> {
    let activated = letibot_activation::activated();
    let st = match activated {
        Some(_) => None,
        None => status_here(socket),
    };
    let serves = st.as_ref().is_some_and(serves);
    match gate(activated, serves, socket, st.and_then(|s| s.pid)) {
        Gate::Bind => Ok(()),
        Gate::Refuse(why) => Err(why),
        Gate::Take(m) => {
            let sockets = [Socket::Unix {
                name: SOCKET_NAME.into(),
                path: socket.to_path_buf(),
            }];
            match intake::take(m, &sockets)?.pop() {
                Some(Listener::Unix(l)) => {
                    *TAKEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(l);
                    Ok(())
                }
                _ => Err(format!("{} passed no unix socket", m.as_str())),
            }
        }
    }
}

/// The listener [`admit`] took, once.
pub fn taken() -> Option<UnixListener> {
    TAKEN.lock().unwrap_or_else(|e| e.into_inner()).take()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> Spec {
        spec(
            Path::new("/opt/lb/harnessd"),
            Path::new("/h/.local/share/letibot/room-box-0a1b2c3d.sock"),
            vec![("CUDA_VISIBLE_DEVICES".into(), String::new())],
            Path::new("/h"),
        )
    }

    #[test]
    fn the_plist_is_exact() {
        let want = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>io.letibot.rooms</string>
	<key>ProgramArguments</key>
	<array>
		<string>/opt/lb/harnessd</string>
		<string>--room</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>LETIBOT_ACTIVATION</key>
		<string>launchd</string>
		<key>CUDA_VISIBLE_DEVICES</key>
		<string></string>
	</dict>
	<key>Sockets</key>
	<dict>
		<key>room</key>
		<dict>
			<key>SockPathName</key>
			<string>/h/.local/share/letibot/room-box-0a1b2c3d.sock</string>
			<key>SockPathMode</key>
			<integer>384</integer>
		</dict>
	</dict>
	<key>StandardOutPath</key>
	<string>/h/Library/Logs/letibot/rooms.log</string>
	<key>StandardErrorPath</key>
	<string>/h/Library/Logs/letibot/rooms.log</string>
</dict>
</plist>
"#;
        assert_eq!(letibot_activation::render::plist(&s()), want);
    }

    #[test]
    fn the_units_are_exact() {
        assert_eq!(
            letibot_activation::render::socket_unit(&s()),
            "[Unit]\nDescription=letibot room coordinator sockets\n\n[Socket]\n\
             ListenStream=/h/.local/share/letibot/room-box-0a1b2c3d.sock\n\
             SocketMode=0600\nDirectoryMode=0700\nService=letibot-rooms.service\n\n\
             [Install]\nWantedBy=sockets.target\n"
        );
        assert_eq!(
            letibot_activation::render::service_unit(&s()),
            "[Unit]\nDescription=letibot room coordinator\nRequires=letibot-rooms.socket\n\
             After=letibot-rooms.socket\n\n[Service]\nExecStart=/opt/lb/harnessd --room\n\
             Environment=LETIBOT_ACTIVATION=systemd\nEnvironment=CUDA_VISIBLE_DEVICES=\n"
        );
    }

    #[test]
    fn the_verbs_drive_the_rooms_label() {
        let runs = |m, v| -> Vec<String> {
            control::plan(v, m, &s(), Path::new("/h"), 501)
                .into_iter()
                .filter_map(|st| match st {
                    control::Step::Run { argv, .. } => Some(argv.join(" ")),
                    control::Step::Exec(argv) => Some(argv.join(" ")),
                    _ => None,
                })
                .collect()
        };
        use control::Verb;
        assert_eq!(
            runs(Manager::Launchd, Verb::Restart),
            vec!["launchctl kickstart -k gui/501/io.letibot.rooms"]
        );
        assert_eq!(
            runs(Manager::Launchd, Verb::On),
            vec!["launchctl bootstrap gui/501 /h/Library/LaunchAgents/io.letibot.rooms.plist"]
        );
        assert_eq!(
            runs(Manager::Launchd, Verb::Logs),
            vec!["tail -n 50 -F /h/Library/Logs/letibot/rooms.log"]
        );
        assert_eq!(
            runs(Manager::Systemd, Verb::Restart),
            vec!["systemctl --user restart letibot-rooms.service"]
        );
        assert_eq!(
            runs(Manager::Systemd, Verb::Off),
            vec![
                "systemctl --user disable --now letibot-rooms.socket",
                "systemctl --user stop letibot-rooms.service",
            ]
        );
        assert_eq!(
            runs(Manager::Systemd, Verb::Logs),
            vec!["journalctl --user -u letibot-rooms -n 50 -f"]
        );
    }

    #[test]
    fn activation_on_means_no_spawn_and_a_refusal_by_hand() {
        let sock = Path::new("/d/room-x.sock");
        assert_eq!(
            gate(Some(Manager::Launchd), true, sock, None),
            Gate::Take(Manager::Launchd)
        );
        assert_eq!(gate(None, false, sock, None), Gate::Bind);
        assert!(matches!(gate(None, true, sock, Some(9)), Gate::Refuse(_)));
        assert!(!launcher_spawns(true));
        assert!(launcher_spawns(false));
    }

    #[test]
    fn only_activation_for_this_socket_serves_it() {
        let on = Status {
            installed: true,
            on: true,
            ..Status::default()
        };
        assert!(serves(&on));
        let other = Status {
            stale: vec!["the socket /tmp/test/room.sock".into()],
            ..on.clone()
        };
        assert!(
            !serves(&other),
            "files naming another socket are not this room's"
        );
        let rebuilt = Status {
            stale: vec!["the binary /new/harnessd".into()],
            ..on.clone()
        };
        assert!(serves(&rebuilt));
        assert!(!serves(&Status::default()));
    }

    #[test]
    fn the_refusal_names_stop_off_and_on() {
        assert_eq!(
            refusal(Path::new("/d/room-x.sock"), Some(12)),
            "activation holds the room socket /d/room-x.sock, and it runs now as pid 12. \
             `letibot rooms stop` stops the running coordinator but activation keeps the \
             socket; `letibot rooms off` frees it for a foreground run, and `letibot rooms on` \
             gives it back."
        );
    }

    #[test]
    fn the_state_carries_the_session_count() {
        let dir = std::env::temp_dir().join(format!("lb-rooms-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("room.state");
        let st = State {
            pid: 7,
            exe: "/opt/lb/harnessd".into(),
            exe_mtime: 5,
            started: 9,
        };
        write_state(&p, &st, 3).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(State::parse(&text), Some(st));
        assert_eq!(read_sessions(&text), Some(3));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
