//! Docker for a seat: no engine socket by default, a labelling proxy when one is granted
//! (`docs/lifecycle.md` d.5, d.7 and the G6 package).
//!
//! Three placements, three rules. A session in a VM gets docker inside the guest, where it dies
//! with the guest and nothing needs a label. A host seat gets no engine socket at all: what it can
//! reach is one socket under its own owner root, answered by [`proxy`], and that socket refuses
//! everything until the seat is granted docker (see [`WITHHELD`]). A granted seat's socket forwards
//! the Engine API and rewrites what it must, because the label is the only ownership docker offers
//! and nothing puts one on by default (`docs/lifecycle.md` b.4).
//!
//! The proxy targets **Engine API v1.44** for the calls it makes itself (label filters, the
//! `labels` parameter on a build, `/system/df` for volume sizes) and passes a client's own
//! `/v1.NN/` prefix through unchanged otherwise. What it does per endpoint:
//!
//! | endpoint | what the proxy does |
//! |---|---|
//! | `POST /containers/create` | injects the two labels, prefixes an explicit name, fills a missing `Memory`, sets `CgroupParent`; refuses `Privileged` and any bind or mount outside the owner root |
//! | `POST /networks/create`, `POST /volumes/create` | injects the two labels (and prefixes a network's name) |
//! | `POST /build` | injects the `labels` query parameter, merged with the client's own |
//! | `POST /containers/prune`, `/networks/prune`, `/volumes/prune`, `/images/prune` | rewrites the filters so a prune can only reach this owner's objects |
//! | `GET /containers/json`, `GET /networks`, `GET /volumes` | adds this owner's label filter and filters the answer again |
//! | any other request naming a container, network or volume | inspects it first and refuses (403) when it carries no `letibot.owner` |
//! | `GET /images/json`, `/info`, `/version`, `/_ping`, `/events`, `/system/df` | passed through: an image is a shared cache, is never owner-reaped, and its labels say nothing about who may pull one |
//! | a request that asks to upgrade the connection (`exec -it`, `attach`) | refused: a hijacked socket has no HTTP left in it to check |
//!
//! Every rule below is enforced by [`Proxy::handle`], which is pure enough to be tested with a
//! request and a stub engine - and [`Serve::start`] is the same code behind a real unix socket.

pub mod config;
mod http;
mod proxy;
/// What docker holds for an owner, and how it goes: the label filter, the removals, the report.
pub mod reap;
#[cfg(test)]
mod tests;

pub use config::Settings;
pub use http::{Request, Response, call};
pub use proxy::{Object, Proxy, Serve, Upstream};
pub use reap::{
    Discovered, Ended, Engine, Report, Row, Socket, discovered, end_compute, end_work, prune,
    remove, report,
};

use std::collections::BTreeMap;
use std::path::PathBuf;

/// The label that proves an object is an owner's. It is the whole of docker's ownership story: a
/// label is fixed for the life of an object, and no engine puts one on by default.
pub const LABEL_OWNER: &str = "letibot.owner";
/// And the box the owner lives on, so an object carried between boxes is not read as this one's.
pub const LABEL_BOX: &str = "letibot.box";
/// The Engine API version the proxy's own calls use (`docs/lifecycle.md` d.7).
pub const API_VERSION: &str = "1.44";
/// Where a stock engine listens, when neither `[docker] engine` nor `$DOCKER_HOST` says.
pub const ENGINE_DEFAULT: &str = "/run/docker.sock";
/// The socket a seat is given, inside its owner root.
pub const SOCKET_NAME: &str = "docker.sock";
/// Compose's own project label: what a report groups by and what an opt-in prune matches.
pub const COMPOSE_PROJECT: &str = "com.docker.compose.project";

/// The sentence a seat without the grant reads, from any client that speaks to the API - the CLI,
/// compose, an SDK, or `curl --unix-socket`. It names the grant and the VM route, because a
/// refusal that does not say how to proceed is a wall.
pub const WITHHELD: &str = "docker is not granted to this seat: its perimeter has no `docker = \
     true`, so the host's engine socket is not bound in and this socket refuses everything. Two \
     ways to get docker: run the package in a VM (`vm: true`), where docker runs inside the guest \
     and goes with it, or add `docker = true` under `[perimeter.<name>]` for this seat in \
     ~/.config/letibot/leticode.toml and reopen the session";

/// `s-1794555261791226969` as `17945552`: the short id every docker name carries
/// (`docs/lifecycle.md` d.4 naming conventions).
pub fn short_id(owner: &str) -> String {
    let body = owner.strip_prefix("s-").unwrap_or(owner);
    body.chars().take(8).collect()
}

/// The engine socket this box has, if any: `[docker] engine` first, then `$DOCKER_HOST`, and only
/// when something is really listening there. A path that is not a socket is a box with no docker,
/// which is the answer `Places` wants - a reaper that connects to nothing every pass is a reaper
/// that says "docker did not answer" for ever.
pub fn engine_socket() -> Option<PathBuf> {
    let named = Settings::load_or_default()
        .engine
        .or_else(|| {
            std::env::var_os("DOCKER_HOST").and_then(|v| {
                let v = v.to_string_lossy().into_owned();
                v.strip_prefix("unix://")
                    .map(PathBuf::from)
                    .or_else(|| v.starts_with('/').then(|| PathBuf::from(v)))
            })
        })
        .unwrap_or_else(|| PathBuf::from(ENGINE_DEFAULT));
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(&named)
        .map(|m| m.file_type().is_socket())
        .unwrap_or(false)
        .then_some(named)
}

/// What the socket does with a request: forward it, or answer every one with [`WITHHELD`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Forward,
    Refuse(String),
}

/// One seat's docker: the owner it acts for, where its socket lives, and the engine behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub owner: String,
    pub box_id: String,
    /// The owner root. The proxy's socket lives here and no bind may point outside it.
    pub root: PathBuf,
    /// The real engine socket. `None` on a box with no docker: a granted seat then says so rather
    /// than failing with a bare ENOENT.
    pub engine: Option<PathBuf>,
    /// The limit a create that names none is given, so an unbounded container is not the default.
    pub memory: Option<u64>,
    /// The cgroup parent a container is placed under, when the engine's driver allows it.
    pub cgroup_parent: Option<String>,
}

impl Policy {
    /// A policy for `owner` under this box's settings.
    pub fn of(owner: &str, places: &crate::lifecycle::Places) -> Policy {
        let settings = Settings::load_or_default();
        Policy {
            owner: owner.to_string(),
            box_id: places.box_id.clone(),
            root: places.owner_root(owner),
            engine: settings.engine,
            memory: settings.memory,
            cgroup_parent: settings.cgroup_parent,
        }
    }

    /// `<owner root>/docker.sock`: the one socket a seat may reach.
    pub fn socket(&self) -> PathBuf {
        self.root.join(SOCKET_NAME)
    }

    /// `lb-17945552`: compose's project name, and the prefix of every name letibot's docker makes.
    pub fn project(&self) -> String {
        format!("lb-{}", short_id(&self.owner))
    }

    /// `lb-17945552-`: a container or network name this owner made.
    pub fn name_prefix(&self) -> String {
        format!("{}-", self.project())
    }

    /// The two labels every object this owner makes carries.
    pub fn labels(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (LABEL_OWNER.to_string(), self.owner.clone()),
            (LABEL_BOX.to_string(), self.box_id.clone()),
        ])
    }

    /// Whether a name is letibot's spelling for this owner, with the prefix put on once.
    pub fn prefixed(&self, name: &str) -> String {
        let prefix = self.name_prefix();
        if name.starts_with(&prefix) {
            name.to_string()
        } else {
            format!("{prefix}{name}")
        }
    }

    pub fn route(&self, granted: bool) -> Route {
        if granted {
            Route::Forward
        } else {
            Route::Refuse(WITHHELD.to_string())
        }
    }

    /// The pairs a seat's commands need so their `docker` reaches the proxy, never the engine.
    ///
    /// `DOCKER_HOST` is set even when docker is withheld: the socket is served either way, and a
    /// client that finds a refusal in our words is worth more than one that says "no such file".
    pub fn seat_env(&self, granted: bool) -> Vec<(String, String)> {
        let mut out = vec![
            (
                "DOCKER_HOST".to_string(),
                format!("unix://{}", self.socket().display()),
            ),
            ("LETIBOT_OWNER".to_string(), self.owner.clone()),
        ];
        if granted {
            // Compose labels every container, network and volume it makes with its project name,
            // so a compose file that never heard of letibot is still labelled for us.
            out.push(("COMPOSE_PROJECT_NAME".to_string(), self.project()));
        }
        out
    }

    /// The catalogued APIs this policy gives one line each, for `/boundary` and a report.
    pub fn line(&self, granted: bool) -> String {
        format!(
            "docker: {} · socket {} · engine {}",
            if granted { "granted" } else { "withheld" },
            self.socket().display(),
            self.engine
                .as_deref()
                .map(|e| e.display().to_string())
                .unwrap_or_else(|| "none on this box".into())
        )
    }
}
