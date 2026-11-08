//! Socket activation for letibot's long-lived helpers: a launchd agent on macOS, a systemd
//! `--user` socket and service on Linux. A helper describes itself once as a [`Spec`]; this crate
//! renders the agent or units, runs the control verbs, hands the helper the listeners the manager
//! holds, and decides with it when an idle helper may exit.
//!
//! ```text
//!   install ──▶ plist / units ──▶ launchd | systemd holds the sockets
//!   first connection ──▶ helper starts, takes the fds (intake) ──▶ serves ──▶ idle: exits 0
//! ```

pub mod control;
pub mod intake;
pub mod render;
pub mod serve;
pub mod state;

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

/// Set by the agent and the service unit, so a helper knows a manager started it.
pub const ENV: &str = "LETIBOT_ACTIVATION";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    Launchd,
    Systemd,
}

impl Manager {
    /// The manager of the machine this binary was built for.
    pub fn native() -> Manager {
        if cfg!(target_os = "macos") {
            Manager::Launchd
        } else {
            Manager::Systemd
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Manager::Launchd => "launchd",
            Manager::Systemd => "systemd",
        }
    }

    pub fn parse(s: &str) -> Option<Manager> {
        match s {
            "launchd" => Some(Manager::Launchd),
            "systemd" => Some(Manager::Systemd),
            _ => None,
        }
    }
}

/// Which manager started this process, from [`ENV`]. `None` is a foreground run.
pub fn activated() -> Option<Manager> {
    std::env::var(ENV).ok().and_then(|v| Manager::parse(&v))
}

/// One listening socket the manager holds for the helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Socket {
    Tcp {
        name: String,
        host: Ipv4Addr,
        port: u16,
    },
    /// Mode 0600, parent directory 0700.
    Unix { name: String, path: PathBuf },
}

impl Socket {
    pub fn name(&self) -> &str {
        match self {
            Socket::Tcp { name, .. } | Socket::Unix { name, .. } => name,
        }
    }
}

/// Everything a helper's agent or units say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// The launchd label, `io.letibot.<name>`.
    pub label: String,
    /// The systemd unit base name, `letibot-<name>`.
    pub unit: String,
    pub description: String,
    /// Absolute argv. Rebuilding the binary at `program[0]` is what `restart` picks up.
    pub program: Vec<String>,
    /// Extra environment, besides [`ENV`].
    pub env: Vec<(String, String)>,
    pub sockets: Vec<Socket>,
    /// launchd's stdout and stderr. Under systemd stderr goes to the journal.
    pub log: PathBuf,
}

impl Spec {
    /// The files `install` writes, each with its text.
    pub fn files(&self, m: Manager, home: &Path) -> Vec<(PathBuf, String)> {
        match m {
            Manager::Launchd => vec![(self.plist_path(home), render::plist(self))],
            Manager::Systemd => {
                let dir = unit_dir(home);
                vec![
                    (
                        dir.join(format!("{}.socket", self.unit)),
                        render::socket_unit(self),
                    ),
                    (
                        dir.join(format!("{}.service", self.unit)),
                        render::service_unit(self),
                    ),
                ]
            }
        }
    }

    pub fn plist_path(&self, home: &Path) -> PathBuf {
        home.join("Library/LaunchAgents")
            .join(format!("{}.plist", self.label))
    }
}

/// `$XDG_CONFIG_HOME/systemd/user`, else `~/.config/systemd/user`.
pub fn unit_dir(home: &Path) -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => home.join(".config"),
    }
    .join("systemd/user")
}
