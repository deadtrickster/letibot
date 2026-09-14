//! Onboarding: whose token this is, and where it came from.
//!
//! Two flows, and the second is the one that runs on a machine that has been
//! seated already:
//!
//! 1. **Manual.** Every field given: `--flowy-addr`, `--flowy AGENT`,
//!    `--flowy-token-file` (or `$FLOWY_TOKEN`). Nothing is looked up.
//! 2. **The usual path.** `flowy mint` leaves a seat's token at
//!    `~/.config/flowy/agents/<name>` and the seat brief leaves
//!    `~/.config/flowy/env-<name>` holding `FLOWY_ADDR`, `FLOWY_AGENT` and
//!    `FLOWY_TOKEN`. Given a name — or, when exactly one seat is on the machine,
//!    given nothing — both are read.
//!
//! # What this refuses, and why it is the interesting part
//!
//! **`~/.config/flowy/token` is never read.** That file is the OPERATOR'S own
//! credential; flowy's own CLI falls through to it with a warning, and the seat
//! brief says in bold that everything a seat sends goes out under the seat's
//! name. A harness that could speak as the operator by omission would be the
//! failure the brief calls a lie the whole fleet acts on. So there is no
//! fallback: no seat, no flowy.
//!
//! **Two seats and no name is a refusal, not a guess.** A seat is an identity.
//! Picking one because it sorted first would attach the wrong reader to the
//! wrong mind, and every message to the other name would go quietly unheard —
//! the roster showing a seat attached while nobody is behind it.
//!
//! Every resolved value carries its [`Source`], so the daemon's disclosure is a
//! reading of where the credential came from rather than a claim that it exists.

use std::path::{Path, PathBuf};

use letibot_http::Endpoint;

/// The default node when nothing says otherwise — flowy's own default.
pub const DEFAULT_ADDR: &str = "http://127.0.0.1:8787";

/// Where a value was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Given on the command line or by the caller.
    Explicit,
    /// `$FLOWY_ADDR`, `$FLOWY_AGENT`, `$FLOWY_TOKEN`.
    Env(&'static str),
    /// A line in `~/.config/flowy/env-<agent>`.
    EnvFile(PathBuf),
    /// The token file `~/.config/flowy/agents/<agent>`.
    TokenFile(PathBuf),
    /// The only seat file under `~/.config/flowy/agents/`.
    OnlySeat(PathBuf),
    /// Nothing said, so [`DEFAULT_ADDR`].
    Default,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Explicit => write!(f, "given explicitly"),
            Source::Env(v) => write!(f, "${v}"),
            Source::EnvFile(p) => write!(f, "{}", p.display()),
            Source::TokenFile(p) => write!(f, "{}", p.display()),
            Source::OnlySeat(p) => write!(f, "the only seat on this machine, {}", p.display()),
            Source::Default => write!(f, "the default"),
        }
    }
}

/// What the caller already knows. Every field optional; what is missing is looked
/// up along the usual path.
#[derive(Debug, Clone, Default)]
pub struct Onboarding {
    pub addr: Option<String>,
    pub agent: Option<String>,
    pub token: Option<String>,
    pub token_file: Option<PathBuf>,
    /// `~/.config/flowy` unless a test says otherwise.
    pub config_dir: Option<PathBuf>,
    /// Read the process environment. Off in tests, so a developer's own
    /// `$FLOWY_TOKEN` cannot leak into an assertion.
    pub read_env: bool,
}

impl Onboarding {
    /// The usual path: everything from the environment and the config directory.
    pub fn usual() -> Self {
        Onboarding {
            read_env: true,
            ..Default::default()
        }
    }

    fn config_dir(&self) -> Option<PathBuf> {
        if let Some(d) = &self.config_dir {
            return Some(d.clone());
        }
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("flowy"))
    }

    fn env(&self, key: &'static str) -> Option<String> {
        if !self.read_env {
            return None;
        }
        std::env::var(key)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }
}

/// A resolved seat credential, with provenance.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub addr: String,
    pub endpoint: Endpoint,
    pub agent: String,
    pub token: String,
    /// The file the token was read from, when it was one. The seat loop re-reads
    /// it to notice a re-mint — see [`crate::seat`].
    pub token_file: Option<PathBuf>,
    pub addr_from: Source,
    pub agent_from: Source,
    pub token_from: Source,
}

impl Credentials {
    /// One line for a disclosure.
    pub fn describe(&self) -> String {
        format!(
            "seat `{}` ({}), token from {}, node {} ({})",
            self.agent, self.agent_from, self.token_from, self.addr, self.addr_from
        )
    }
}

#[derive(Debug)]
pub enum CredError {
    /// No name, and the usual path did not settle it.
    NoAgent {
        seats_seen: Vec<String>,
        looked_in: PathBuf,
    },
    /// A name, but no token for it anywhere it could be.
    NoToken {
        agent: String,
        looked: Vec<PathBuf>,
    },
    /// The address is not `http://host:port`.
    BadAddr {
        addr: String,
        why: String,
    },
    Unreadable {
        path: PathBuf,
        err: std::io::Error,
    },
}

impl std::fmt::Display for CredError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredError::NoAgent {
                seats_seen,
                looked_in,
            } => {
                if seats_seen.is_empty() {
                    write!(
                        f,
                        "no flowy seat: nothing named one (--flowy NAME or $FLOWY_AGENT) \
                         and {} holds no seat token. Mint one on the node with \
                         `flowy mint`; ~/.config/flowy/token is the operator's own and is \
                         never used for a seat",
                        looked_in.display()
                    )
                } else {
                    write!(
                        f,
                        "which seat: {} holds {} and nothing named one. A seat is an \
                         identity, so it is not guessed — pass --flowy NAME or set \
                         $FLOWY_AGENT",
                        looked_in.display(),
                        seats_seen.join(", ")
                    )
                }
            }
            CredError::NoToken { agent, looked } => write!(
                f,
                "no token for seat `{agent}`: looked at {}. Mint one with `flowy mint \
                 --handle {agent}` and put it there, or pass --flowy-token-file",
                looked
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            CredError::BadAddr { addr, why } => write!(f, "flowy address `{addr}`: {why}"),
            CredError::Unreadable { path, err } => {
                write!(f, "could not read {}: {err}", path.display())
            }
        }
    }
}

impl std::error::Error for CredError {}

/// Resolve credentials. Explicit beats environment beats the env file beats the
/// token file beats the default, per field, and every choice is recorded.
pub fn discover(o: &Onboarding) -> Result<Credentials, CredError> {
    let config_dir = o.config_dir();
    let agents_dir = config_dir.as_ref().map(|d| d.join("agents"));

    // --- the agent -------------------------------------------------------
    let (agent, agent_from) = if let Some(a) = o.agent.clone().filter(|a| !a.trim().is_empty()) {
        (a.trim().to_string(), Source::Explicit)
    } else if let Some(a) = o.env("FLOWY_AGENT") {
        (a, Source::Env("FLOWY_AGENT"))
    } else {
        let looked_in = agents_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("~/.config/flowy/agents"));
        let seats = seats_in(&looked_in);
        match seats.as_slice() {
            [only] => (only.clone(), Source::OnlySeat(looked_in.join(only))),
            _ => {
                return Err(CredError::NoAgent {
                    seats_seen: seats,
                    looked_in,
                });
            }
        }
    };

    // --- the env file, read once, consulted for addr and token ------------
    let env_file = config_dir.as_ref().map(|d| d.join(format!("env-{agent}")));
    let env_lines = match &env_file {
        Some(p) if p.is_file() => Some(parse_env_file(p)?),
        _ => None,
    };
    let from_env_file = |key: &str| -> Option<(String, Source)> {
        let lines = env_lines.as_ref()?;
        let v = lines.iter().find(|(k, _)| k == key)?.1.clone();
        Some((v, Source::EnvFile(env_file.clone()?)))
    };

    // --- the address -----------------------------------------------------
    let (addr, addr_from) = if let Some(a) = o.addr.clone().filter(|a| !a.trim().is_empty()) {
        (a.trim().to_string(), Source::Explicit)
    } else if let Some(a) = o.env("FLOWY_ADDR") {
        (a, Source::Env("FLOWY_ADDR"))
    } else if let Some(pair) = from_env_file("FLOWY_ADDR") {
        pair
    } else {
        (DEFAULT_ADDR.to_string(), Source::Default)
    };
    let endpoint = parse_addr(&addr)?;

    // --- the token -------------------------------------------------------
    let mut looked = Vec::new();
    let (token, token_from, token_file) = if let Some(t) = o.token.clone().filter(|t| !t.is_empty())
    {
        (t, Source::Explicit, None)
    } else if let Some(p) = &o.token_file {
        looked.push(p.clone());
        (
            read_token(p, config_dir.as_deref())?,
            Source::Explicit,
            Some(p.clone()),
        )
    } else if let Some(t) = o.env("FLOWY_TOKEN") {
        (t, Source::Env("FLOWY_TOKEN"), None)
    } else {
        // The env file spells the token as `$(cat FILE)` — a shell line, not a
        // value — so the file it names is what is read, and the token file is
        // the source. A literal token in the env file is honoured too.
        let mut found = None;
        if let Some((v, src)) = from_env_file("FLOWY_TOKEN") {
            if let Some(p) = cat_target(&v) {
                looked.push(p.clone());
                if p.is_file() {
                    found = Some((
                        read_token(&p, config_dir.as_deref())?,
                        Source::TokenFile(p.clone()),
                        Some(p),
                    ));
                }
            } else {
                found = Some((v, src, None));
            }
        }
        match found {
            Some(f) => f,
            None => {
                let Some(dir) = &agents_dir else {
                    return Err(CredError::NoToken { agent, looked });
                };
                let p = dir.join(&agent);
                looked.push(p.clone());
                if !p.is_file() {
                    return Err(CredError::NoToken { agent, looked });
                }
                (
                    read_token(&p, config_dir.as_deref())?,
                    Source::TokenFile(p.clone()),
                    Some(p),
                )
            }
        }
    };

    Ok(Credentials {
        addr,
        endpoint,
        agent,
        token,
        token_file,
        addr_from,
        agent_from,
        token_from,
    })
}

/// The seat names under `~/.config/flowy/agents`: one file per seat, and nothing
/// else lives there.
fn seats_in(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    out.sort();
    out
}

/// `KEY=VALUE` and `export KEY=VALUE` lines; comments and blanks skipped. Values
/// keep their `$(...)` spelling so the caller can see a `cat`.
fn parse_env_file(p: &Path) -> Result<Vec<(String, String)>, CredError> {
    let raw = std::fs::read_to_string(p).map_err(|err| CredError::Unreadable {
        path: p.to_path_buf(),
        err,
    })?;
    Ok(raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let l = l.strip_prefix("export ").unwrap_or(l).trim();
            let (k, v) = l.split_once('=')?;
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            Some((k.trim().to_string(), v))
        })
        .collect())
}

/// `$(cat PATH)` → `PATH`, with a leading `~` expanded. Anything else is a
/// literal value.
fn cat_target(v: &str) -> Option<PathBuf> {
    let inner = v.strip_prefix("$(")?.strip_suffix(')')?.trim();
    let path = inner.strip_prefix("cat ")?.trim();
    Some(expand_home(path))
}

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(h) = std::env::var_os("HOME")
    {
        return PathBuf::from(h).join(rest);
    }
    PathBuf::from(p)
}

/// The token file: one line, trimmed. Refuses the operator's own file by name,
/// whatever path spelled it.
pub fn read_token(p: &Path, config_dir: Option<&Path>) -> Result<String, CredError> {
    let is_operators = p.file_name().is_some_and(|n| n == "token")
        && (p
            .parent()
            .is_some_and(|d| d.file_name().is_some_and(|n| n == "flowy"))
            || config_dir.is_some_and(|d| p.parent() == Some(d)));
    if is_operators {
        return Err(CredError::Unreadable {
            path: p.to_path_buf(),
            err: std::io::Error::other(
                "that is the operator's own credential, and a seat never speaks as the \
                 operator",
            ),
        });
    }
    let raw = std::fs::read_to_string(p).map_err(|err| CredError::Unreadable {
        path: p.to_path_buf(),
        err,
    })?;
    let t = raw.trim().to_string();
    if t.is_empty() {
        return Err(CredError::Unreadable {
            path: p.to_path_buf(),
            err: std::io::Error::other("the file is empty"),
        });
    }
    Ok(t)
}

/// `http://host:port` (a trailing slash tolerated) → an [`Endpoint`]. `https` is
/// refused by name: the client has no TLS, and a silent downgrade would send a
/// bearer token in the clear to a node that expected otherwise.
pub fn parse_addr(addr: &str) -> Result<Endpoint, CredError> {
    let bad = |why: &str| CredError::BadAddr {
        addr: addr.to_string(),
        why: why.to_string(),
    };
    let rest = if let Some(r) = addr.strip_prefix("http://") {
        r
    } else if addr.starts_with("https://") {
        return Err(bad(
            "https is not spoken here (no TLS in the client); the node is plain http on \
             the LAN",
        ));
    } else {
        return Err(bad("expected http://HOST:PORT"));
    };
    let rest = rest.trim_end_matches('/');
    if rest.contains('/') {
        return Err(bad("a path after the authority is not supported"));
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|_| bad("the port is not a number"))?,
        ),
        None => (rest, 8787),
    };
    if host.is_empty() {
        return Err(bad("empty host"));
    }
    let mut ep = Endpoint::new(host, port);
    // The node's long poll blocks for at most 25 s and heartbeats a stream every
    // 5 s; a gap longer than a minute is a dead node, not a slow one.
    ep.read_timeout = std::time::Duration::from_secs(60);
    ep.connect_timeout = std::time::Duration::from_secs(5);
    Ok(ep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "letibot-creds-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(d.join("agents")).unwrap();
        d
    }

    fn usual_with(dir: &Path) -> Onboarding {
        Onboarding {
            config_dir: Some(dir.to_path_buf()),
            read_env: false,
            ..Default::default()
        }
    }

    #[test]
    fn the_usual_path_reads_the_only_seat_and_its_env_file() {
        let d = tmp();
        std::fs::write(d.join("agents/lubuntu3-glm"), "tok-1\n").unwrap();
        std::fs::write(
            d.join("env-lubuntu3-glm"),
            "export FLOWY_ADDR=http://192.168.1.55:8787\nexport FLOWY_AGENT=lubuntu3-glm\n\
             export FLOWY_TOKEN=$(cat ~/.config/flowy/agents/lubuntu3-glm)\n",
        )
        .unwrap();
        let c = discover(&usual_with(&d)).unwrap();
        assert_eq!(c.agent, "lubuntu3-glm");
        assert!(matches!(c.agent_from, Source::OnlySeat(_)));
        assert_eq!(c.addr, "http://192.168.1.55:8787");
        assert!(matches!(c.addr_from, Source::EnvFile(_)));
        assert_eq!(c.endpoint.host, "192.168.1.55");
        // `$(cat ~/...)` names a path that does not exist in this sandbox, so the
        // seat file under the config dir is what answers.
        assert_eq!(c.token, "tok-1");
        assert!(matches!(c.token_from, Source::TokenFile(_)));
    }

    #[test]
    fn two_seats_and_no_name_is_a_refusal_that_names_both() {
        let d = tmp();
        std::fs::write(d.join("agents/a-seat"), "x").unwrap();
        std::fs::write(d.join("agents/b-seat"), "y").unwrap();
        let err = discover(&usual_with(&d)).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("a-seat, b-seat"), "{s}");
        assert!(s.contains("not guessed"), "{s}");
    }

    #[test]
    fn no_seat_at_all_says_so_and_names_the_operator_file_as_off_limits() {
        let d = tmp();
        let s = discover(&usual_with(&d)).unwrap_err().to_string();
        assert!(s.contains("operator's own"), "{s}");
    }

    #[test]
    fn the_operators_own_token_is_refused_even_when_named() {
        let d = tmp();
        std::fs::write(d.join("token"), "operator-secret").unwrap();
        let mut o = usual_with(&d);
        o.agent = Some("me".into());
        o.token_file = Some(d.join("token"));
        let s = discover(&o).unwrap_err().to_string();
        assert!(s.contains("never speaks as the operator"), "{s}");
    }

    #[test]
    fn explicit_beats_everything_and_records_that() {
        let d = tmp();
        std::fs::write(d.join("agents/seat"), "file-token").unwrap();
        let o = Onboarding {
            addr: Some("http://10.0.0.1:9000/".into()),
            agent: Some("seat".into()),
            token: Some("given".into()),
            config_dir: Some(d.clone()),
            read_env: false,
            token_file: None,
        };
        let c = discover(&o).unwrap();
        assert_eq!(c.token, "given");
        assert_eq!(c.token_from, Source::Explicit);
        assert_eq!(c.endpoint.port, 9000);
        assert!(c.token_file.is_none());
    }

    #[test]
    fn https_is_refused_by_name() {
        let s = parse_addr("https://node:8787").unwrap_err().to_string();
        assert!(s.contains("https"), "{s}");
    }
}
