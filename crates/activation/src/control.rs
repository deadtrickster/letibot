//! The control verbs as plans: each verb is a list of [`Step`]s, built without touching the
//! machine so the exact launchctl and systemctl lines are testable, then run by [`execute`].

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Manager, Socket, Spec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
    Off,
    On,
    Status,
    Logs,
}

impl Verb {
    pub const ALL: &[(&str, Verb)] = &[
        ("install", Verb::Install),
        ("uninstall", Verb::Uninstall),
        ("start", Verb::Start),
        ("stop", Verb::Stop),
        ("restart", Verb::Restart),
        ("off", Verb::Off),
        ("on", Verb::On),
        ("status", Verb::Status),
        ("logs", Verb::Logs),
    ];

    pub fn parse(s: &str) -> Option<Verb> {
        Verb::ALL.iter().find(|(n, _)| *n == s).map(|(_, v)| *v)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `create_dir_all`, then mode 0700 on the last component.
    Mkdir(PathBuf),
    Write(PathBuf, String),
    Remove(PathBuf),
    /// A command; `must` makes its failure the verb's.
    Run {
        argv: Vec<String>,
        must: bool,
    },
    /// Replace this process with the command (`logs`).
    Exec(Vec<String>),
}

fn run(must: bool, argv: &[&str]) -> Step {
    Step::Run {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        must,
    }
}

/// What `verb` does for `spec`. `Status` is not a plan: see [`status`].
pub fn plan(verb: Verb, m: Manager, spec: &Spec, home: &Path, uid: u32) -> Vec<Step> {
    match m {
        Manager::Launchd => launchd_plan(verb, spec, home, uid),
        Manager::Systemd => systemd_plan(verb, spec, home),
    }
}

fn launchd_plan(verb: Verb, spec: &Spec, home: &Path, uid: u32) -> Vec<Step> {
    let domain = format!("gui/{uid}");
    let target = format!("{domain}/{}", spec.label);
    let plist = spec.plist_path(home);
    let plist_s = plist.to_string_lossy().into_owned();
    match verb {
        Verb::Install => {
            let mut out = mkdirs(spec, &plist);
            if let Some(d) = spec.log.parent() {
                out.push(Step::Mkdir(d.to_path_buf()));
            }
            out.extend(
                spec.files(Manager::Launchd, home)
                    .into_iter()
                    .map(|(p, t)| Step::Write(p, t)),
            );
            out.push(run(false, &["launchctl", "bootout", &target]));
            out.push(run(true, &["launchctl", "bootstrap", &domain, &plist_s]));
            out
        }
        Verb::Uninstall => vec![
            run(false, &["launchctl", "bootout", &target]),
            Step::Remove(plist),
        ],
        Verb::Start => vec![run(true, &["launchctl", "kickstart", &target])],
        Verb::Stop => vec![run(false, &["launchctl", "kill", "SIGTERM", &target])],
        Verb::Restart => vec![run(true, &["launchctl", "kickstart", "-k", &target])],
        Verb::Off => vec![run(true, &["launchctl", "bootout", &target])],
        Verb::On => vec![run(true, &["launchctl", "bootstrap", &domain, &plist_s])],
        Verb::Logs => vec![Step::Exec(vec![
            "tail".into(),
            "-n".into(),
            "50".into(),
            "-F".into(),
            spec.log.to_string_lossy().into_owned(),
        ])],
        Verb::Status => Vec::new(),
    }
}

fn systemd_plan(verb: Verb, spec: &Spec, home: &Path) -> Vec<Step> {
    let socket = format!("{}.socket", spec.unit);
    let service = format!("{}.service", spec.unit);
    let ctl = |must: bool, args: &[&str]| {
        let mut argv = vec!["systemctl", "--user"];
        argv.extend_from_slice(args);
        run(must, &argv)
    };
    match verb {
        Verb::Install => {
            let files = spec.files(Manager::Systemd, home);
            let mut out = mkdirs(spec, &files[0].0);
            out.extend(files.into_iter().map(|(p, t)| Step::Write(p, t)));
            out.push(ctl(true, &["daemon-reload"]));
            out.push(ctl(false, &["stop", &service]));
            out.push(ctl(true, &["enable", &socket]));
            out.push(ctl(true, &["restart", &socket]));
            out
        }
        Verb::Uninstall => {
            let mut out = vec![
                ctl(false, &["disable", "--now", &socket]),
                ctl(false, &["stop", &service]),
            ];
            out.extend(
                spec.files(Manager::Systemd, home)
                    .into_iter()
                    .map(|(p, _)| Step::Remove(p)),
            );
            out.push(ctl(false, &["daemon-reload"]));
            out
        }
        Verb::Start => vec![ctl(true, &["start", &service])],
        Verb::Stop => vec![ctl(true, &["stop", &service])],
        Verb::Restart => vec![ctl(true, &["restart", &service])],
        Verb::Off => vec![
            ctl(true, &["disable", "--now", &socket]),
            ctl(false, &["stop", &service]),
        ],
        Verb::On => vec![ctl(true, &["enable", "--now", &socket])],
        Verb::Logs => vec![Step::Exec(
            ["journalctl", "--user", "-u", &spec.unit, "-n", "50", "-f"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )],
        Verb::Status => Vec::new(),
    }
}

/// The directory of the agent or units, then each unix socket's directory (0700).
fn mkdirs(spec: &Spec, file: &Path) -> Vec<Step> {
    let mut out = Vec::new();
    if let Some(d) = file.parent() {
        out.push(Step::Mkdir(d.to_path_buf()));
    }
    for s in &spec.sockets {
        if let Socket::Unix { path, .. } = s
            && let Some(d) = path.parent()
        {
            out.push(Step::Mkdir(d.to_path_buf()));
        }
    }
    out
}

/// Run a plan, saying each command. launchd may still be tearing down a booted-out job when
/// `bootstrap` comes, so a failed `must` command is tried twice more.
pub fn execute(steps: &[Step]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    for step in steps {
        match step {
            Step::Mkdir(d) => {
                std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
                let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
            }
            Step::Write(p, t) => {
                std::fs::write(p, t).map_err(|e| format!("{}: {e}", p.display()))?;
                eprintln!("  wrote {}", p.display());
            }
            Step::Remove(p) => match std::fs::remove_file(p) {
                Ok(()) => eprintln!("  removed {}", p.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", p.display())),
            },
            Step::Run { argv, must } => {
                eprintln!("  {}", argv.join(" "));
                let tries = if *must { 3 } else { 1 };
                let mut last = String::new();
                let ok = (0..tries).any(|i| {
                    if i > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(700));
                    }
                    match Command::new(&argv[0]).args(&argv[1..]).output() {
                        Ok(o) if o.status.success() => true,
                        Ok(o) => {
                            last = String::from_utf8_lossy(&o.stderr).trim().to_string();
                            if last.is_empty() {
                                last = format!("exit {}", o.status);
                            }
                            false
                        }
                        Err(e) => {
                            last = e.to_string();
                            false
                        }
                    }
                });
                if !ok && *must {
                    return Err(format!("`{}` failed: {last}", argv.join(" ")));
                }
            }
            Step::Exec(argv) => {
                use std::os::unix::process::CommandExt;
                let e = Command::new(&argv[0]).args(&argv[1..]).exec();
                return Err(format!("{}: {e}", argv[0]));
            }
        }
    }
    Ok(())
}

/// What the manager says about a helper.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// The agent or units are on disk.
    pub installed: bool,
    /// Items in the installed files that differ from what `install` would write now.
    pub stale: Vec<String>,
    /// The manager holds the sockets.
    pub on: bool,
    pub pid: Option<u32>,
}

pub fn status(m: Manager, spec: &Spec, home: &Path, uid: u32) -> Status {
    let files = spec.files(m, home);
    let installed = files.iter().all(|(p, _)| p.exists());
    let mut out = Status {
        installed,
        ..Status::default()
    };
    if !installed {
        return out;
    }
    for (p, want) in &files {
        let have = std::fs::read_to_string(p).unwrap_or_default();
        if &have != want {
            out.stale.extend(differs(spec, &have, p));
        }
    }
    let text = |argv: &[&str]| {
        Command::new(argv[0])
            .args(&argv[1..])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    match m {
        Manager::Launchd => {
            let target = format!("gui/{uid}/{}", spec.label);
            if let Some(t) = text(&["launchctl", "print", &target]) {
                out.on = true;
                out.pid = launchctl_pid(&t);
            }
        }
        Manager::Systemd => {
            let socket = format!("{}.socket", spec.unit);
            let service = format!("{}.service", spec.unit);
            out.on = text(&["systemctl", "--user", "is-active", &socket])
                .is_some_and(|t| t.trim() == "active");
            out.pid = text(&[
                "systemctl",
                "--user",
                "show",
                &service,
                "-p",
                "MainPID",
                "--value",
            ])
            .and_then(|t| main_pid(&t));
        }
    }
    out
}

/// Which of the program, the sockets and the log an installed file does not name.
pub fn differs(spec: &Spec, have: &str, file: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let check = |what: String, needle: String, out: &mut Vec<String>| {
        if !have.contains(&needle) {
            out.push(what);
        }
    };
    let is_plist = file.extension().is_some_and(|e| e == "plist");
    let is_socket = file.extension().is_some_and(|e| e == "socket");
    if !is_socket && let Some(p) = spec.program.first() {
        check(format!("the binary {p}"), p.clone(), &mut out);
    }
    if is_plist || is_socket {
        for s in &spec.sockets {
            match s {
                Socket::Tcp { host, port, .. } => {
                    let needle = if is_plist {
                        format!("<string>{port}</string>")
                    } else {
                        format!("ListenStream={host}:{port}")
                    };
                    check(format!("the port {port}"), needle, &mut out);
                }
                Socket::Unix { path, .. } => {
                    let p = path.to_string_lossy().into_owned();
                    check(format!("the socket {p}"), p, &mut out);
                }
            }
        }
    }
    if out.is_empty() {
        out.push(format!(
            "{} differs from what install writes",
            file.display()
        ));
    }
    out
}

/// `pid = N` from `launchctl print`, present only while the job runs.
pub fn launchctl_pid(text: &str) -> Option<u32> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("pid = "))
        .and_then(|p| p.trim().parse().ok())
}

/// `MainPID` as `systemctl show --value` prints it: 0 is not running.
pub fn main_pid(text: &str) -> Option<u32> {
    text.trim().parse().ok().filter(|p| *p > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            label: "io.letibot.browser".into(),
            unit: "letibot-browser".into(),
            description: "letibot browser bridge".into(),
            program: vec!["/opt/lb/letibot".into(), "browser".into()],
            env: Vec::new(),
            sockets: vec![
                Socket::Tcp {
                    name: "http".into(),
                    host: [127, 0, 0, 1].into(),
                    port: 8790,
                },
                Socket::Unix {
                    name: "sock".into(),
                    path: PathBuf::from("/tmp/r/letibot/browser.sock"),
                },
            ],
            log: PathBuf::from("/h/Library/Logs/letibot/browser.log"),
        }
    }

    fn runs(steps: &[Step]) -> Vec<String> {
        steps
            .iter()
            .filter_map(|s| match s {
                Step::Run { argv, must } => Some(format!(
                    "{}{}",
                    if *must { "" } else { "?" },
                    argv.join(" ")
                )),
                Step::Exec(argv) => Some(format!("exec {}", argv.join(" "))),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_verb_parses_and_nothing_else_does() {
        for (n, v) in Verb::ALL {
            assert_eq!(Verb::parse(n), Some(*v));
        }
        assert_eq!(Verb::parse("--port"), None);
        assert_eq!(Verb::parse("Start"), None);
    }

    #[test]
    fn launchd_verbs_are_these_commands() {
        let h = Path::new("/h");
        let p = |v| runs(&plan(v, Manager::Launchd, &spec(), h, 501));
        assert_eq!(
            p(Verb::Install),
            vec![
                "?launchctl bootout gui/501/io.letibot.browser",
                "launchctl bootstrap gui/501 /h/Library/LaunchAgents/io.letibot.browser.plist",
            ]
        );
        assert_eq!(
            p(Verb::Start),
            vec!["launchctl kickstart gui/501/io.letibot.browser"]
        );
        assert_eq!(
            p(Verb::Stop),
            vec!["?launchctl kill SIGTERM gui/501/io.letibot.browser"]
        );
        assert_eq!(
            p(Verb::Restart),
            vec!["launchctl kickstart -k gui/501/io.letibot.browser"]
        );
        assert_eq!(
            p(Verb::Off),
            vec!["launchctl bootout gui/501/io.letibot.browser"]
        );
        assert_eq!(
            p(Verb::On),
            vec!["launchctl bootstrap gui/501 /h/Library/LaunchAgents/io.letibot.browser.plist"]
        );
        assert_eq!(
            p(Verb::Logs),
            vec!["exec tail -n 50 -F /h/Library/Logs/letibot/browser.log"]
        );
        let install = plan(Verb::Install, Manager::Launchd, &spec(), h, 501);
        for d in [
            "/h/Library/LaunchAgents",
            "/tmp/r/letibot",
            "/h/Library/Logs/letibot",
        ] {
            assert!(install.contains(&Step::Mkdir(d.into())), "{d}: {install:?}");
        }
        let uninstall = plan(Verb::Uninstall, Manager::Launchd, &spec(), h, 501);
        assert!(uninstall.contains(&Step::Remove(
            "/h/Library/LaunchAgents/io.letibot.browser.plist".into()
        )));
    }

    #[test]
    fn systemd_verbs_are_these_commands() {
        let h = Path::new("/h");
        let p = |v| runs(&plan(v, Manager::Systemd, &spec(), h, 1000));
        assert_eq!(
            p(Verb::Install),
            vec![
                "systemctl --user daemon-reload",
                "?systemctl --user stop letibot-browser.service",
                "systemctl --user enable letibot-browser.socket",
                "systemctl --user restart letibot-browser.socket",
            ]
        );
        assert_eq!(
            p(Verb::Start),
            vec!["systemctl --user start letibot-browser.service"]
        );
        assert_eq!(
            p(Verb::Stop),
            vec!["systemctl --user stop letibot-browser.service"]
        );
        assert_eq!(
            p(Verb::Restart),
            vec!["systemctl --user restart letibot-browser.service"]
        );
        assert_eq!(
            p(Verb::Off),
            vec![
                "systemctl --user disable --now letibot-browser.socket",
                "?systemctl --user stop letibot-browser.service",
            ]
        );
        assert_eq!(
            p(Verb::On),
            vec!["systemctl --user enable --now letibot-browser.socket"]
        );
        assert_eq!(
            p(Verb::Logs),
            vec!["exec journalctl --user -u letibot-browser -n 50 -f"]
        );
    }

    #[test]
    fn manager_output_is_read_for_the_pid() {
        let print = "gui/501/io.letibot.browser = {\n\tactive count = 1\n\tstate = running\n\tpid = 4242\n}\n";
        assert_eq!(launchctl_pid(print), Some(4242));
        assert_eq!(
            launchctl_pid("gui/501/x = {\n\tstate = not running\n}\n"),
            None
        );
        assert_eq!(main_pid("0\n"), None);
        assert_eq!(main_pid("17\n"), Some(17));
    }

    #[test]
    fn a_moved_socket_or_port_is_named() {
        let s = spec();
        let plist = crate::render::plist(&s);
        let mut moved = s.clone();
        moved.sockets[1] = Socket::Unix {
            name: "sock".into(),
            path: "/elsewhere/browser.sock".into(),
        };
        let d = differs(&moved, &plist, Path::new("x.plist"));
        assert_eq!(d, vec!["the socket /elsewhere/browser.sock".to_string()]);
        let unit = crate::render::socket_unit(&s);
        let mut port = s.clone();
        port.sockets[0] = Socket::Tcp {
            name: "http".into(),
            host: [127, 0, 0, 1].into(),
            port: 9000,
        };
        assert_eq!(
            differs(&port, &unit, Path::new("x.socket")),
            vec!["the port 9000".to_string()]
        );
    }
}
