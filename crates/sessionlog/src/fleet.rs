//! The packages running in other folders' daemons, as a head lists them. Each folder has its own
//! daemon, so a head sees only its own; the scan that fills this lives in `letibot-bridge`.

/// What a package session is doing, as far as its own daemon says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetState {
    Running,
    Idle,
    /// The session waits on the operator.
    Asking,
    /// The daemon is not running, so nothing about the session is live.
    DaemonDown,
    /// The daemon is there and would not answer; the reason is the daemon's or the socket's.
    Unreachable(String),
}

impl FleetState {
    pub fn word(&self) -> String {
        match self {
            FleetState::Running => "running".into(),
            FleetState::Idle => "idle".into(),
            FleetState::Asking => "asking".into(),
            FleetState::DaemonDown => "daemon not running".into(),
            FleetState::Unreachable(why) => format!("daemon not answering: {why}"),
        }
    }
}

/// One package session of another daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetRow {
    pub id: String,
    pub title: String,
    pub workspace: String,
    pub model: String,
    /// Where that daemon's tools run (`host:NAME`), when its record says.
    pub placement: Option<String>,
    pub state: FleetState,
}

impl FleetRow {
    /// The command that attaches to this session through its own daemon.
    pub fn attach_command(&self) -> String {
        format!(
            "cd {} && letibot --session {}",
            shell_word(&self.workspace),
            shell_word(&self.id)
        )
    }
}

fn shell_word(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-:@+=,~".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// What one reading of the other daemons found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fleet {
    pub rows: Vec<FleetRow>,
    /// Stopped packages older than the window, counted and not listed.
    pub unlisted: usize,
}

impl Fleet {
    /// The line that ends the group when something is not listed, or `None`.
    pub fn unlisted_line(&self) -> Option<String> {
        (self.unlisted > 0).then(|| unlisted_words(self.unlisted))
    }
}

pub fn unlisted_words(n: usize) -> String {
    format!(
        "{n} older stopped package{} not listed - `letibot --sessions` lists them",
        if n == 1 { "" } else { "s" }
    )
}

/// What reads the fleet: the head's binary supplies it, a test holds one.
pub type FleetReader = std::sync::Arc<dyn Fn() -> Fleet + Send + Sync>;
