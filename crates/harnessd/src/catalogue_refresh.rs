//! Keeping the model catalogue's cache fresh without a person running a script.
//!
//! ONE task per daemon, on a thread of its own: at start, and then once an hour, it asks whether
//! the cache is missing or older than [`letibot_provider::refresh::AGE`] and - only then - walks
//! to models.dev. Neither a start nor a turn ever waits for it, a failure is one line in the log
//! and at most one warning row a day, and it is never a card: nobody is at the screen a card would
//! need, and the catalogue that is there is the one the session goes on planning against.
//!
//! Why an hour rather than a day: the AGE check is what keeps the fetch to one a day, and an hour
//! is short enough that a start which found no network is retried, rather than a day of a stale
//! cache for a fault that lasted a minute.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use letibot_provider::refresh;
use letibot_sessionlog::registry::Registry;

/// How often the task looks at the file's age. See the module's note on why it is not a day.
pub const TICK: Duration = Duration::from_secs(60 * 60);

/// What one look came to, in the words the daemon's log says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Look {
    /// The operator's file, or the pointer in `LETIBOT_MODELS_JSON`, says not to fetch.
    Off,
    /// No cache directory on this box: there is nothing to write to.
    NoHome,
    /// The cache is younger than a day.
    Fresh,
    /// The catalogue changed and both files were written.
    Written { providers: usize, bytes: usize },
    /// The server said the copy here is current, so only its date moved.
    Unchanged,
    /// It failed, and this is the sentence for the log.
    Failed(String),
}

impl Look {
    /// The line the daemon's log says, or `None` when a look is not worth one: a fresh cache and
    /// a switch that is off are the two answers that happen every hour of every day.
    pub fn said(&self) -> Option<String> {
        match self {
            Look::Off | Look::Fresh => None,
            Look::NoHome => Some("no cache directory on this box, so nothing to refresh".into()),
            Look::Written { providers, bytes } => Some(format!(
                "refreshed the model catalogue: {providers} provider(s), {bytes} bytes"
            )),
            Look::Unchanged => Some("the model catalogue is unchanged upstream".into()),
            Look::Failed(why) => Some(format!("could not refresh the model catalogue: {why}")),
        }
    }

    /// Why it failed, when it did: the one answer that earns a warning row as well as a log line.
    pub fn failed(&self) -> Option<&str> {
        match self {
            Look::Failed(why) => Some(why),
            _ => None,
        }
    }
}

/// Whether this daemon fetches the catalogue itself: `[catalogue] refresh` at both levels, then
/// the provider's own rule that a file the operator pointed at is not one this box may replace.
pub fn wanted(workspace: &Path) -> bool {
    let user = crate::leticode_config::load_user().unwrap_or_default();
    // A project file that does not parse is reported where a session opens; here it is one
    // level that says nothing, which is the user file's answer standing.
    let project = crate::leticode_config::LeticodeConfig::discover(workspace)
        .and_then(|at| crate::leticode_config::LeticodeConfig::load(&at).ok())
        .unwrap_or_default();
    refresh::wanted(user.overridden_by(&project).catalogue_refresh())
}

/// One look, with the fetch handed in: the switch, the age rule and the three answers are a test
/// rather than an hour of waiting.
pub fn look(
    path: Option<&Path>,
    enabled: bool,
    now: SystemTime,
    fetch: impl FnOnce() -> Result<refresh::Outcome, String>,
) -> Look {
    if !enabled {
        return Look::Off;
    }
    let Some(path) = path else {
        return Look::NoHome;
    };
    if !refresh::stale(path, now, refresh::AGE) {
        return Look::Fresh;
    }
    match fetch() {
        Ok(refresh::Outcome::Written { providers, bytes }) => Look::Written { providers, bytes },
        Ok(refresh::Outcome::Unchanged) => Look::Unchanged,
        Err(why) => Look::Failed(why),
    }
}

/// The daemon's task. `enabled` is [`wanted`], read once by the caller: the file's answer at start
/// is the one this daemon runs with, and a change to it takes effect at the next start.
pub fn spawn(enabled: bool, registry: Arc<Registry>) {
    let _ = std::thread::Builder::new()
        .name("catalogue-refresh".into())
        .spawn(move || run(enabled, registry));
}

/// The loop, on its own thread: one look, then sleep in a second's slices so a stop is answered
/// at once rather than after the hour.
fn run(enabled: bool, registry: Arc<Registry>) {
    let mut warned: Option<u64> = None;
    loop {
        let path = letibot_provider::catalogue::cache_path();
        let look = look(
            path.as_deref(),
            enabled,
            SystemTime::now(),
            refresh::refresh,
        );
        if let Some(said) = look.said() {
            eprintln!("  catalogue: {said}");
        }
        if matches!(look, Look::Written { .. } | Look::Unchanged) {
            // The parse a running daemon swaps to. Every client's `Shelf::Current` asks this on
            // its next request, so the new windows and prices reach a session that outlived its
            // provider without a restart.
            let _ = letibot_provider::catalogue::Catalogue::load();
        }
        // What the refresh moved: a provider whose default is now a different model is said once,
        // on every live session's hub and in this log, because the next session to start is on a
        // model the operator did not choose. The record of what each provider started on is
        // written by the same call, so a restart says nothing twice.
        if let Some(path) = path.as_deref()
            && (matches!(look, Look::Written { .. })
                || !letibot_provider::catalogue::defaults_path(path).exists())
        {
            for line in letibot_provider::catalogue::note_defaults(path) {
                eprintln!("  catalogue: {line}");
                for session in registry.list() {
                    if let Some(hub) = registry.get(&session.session_id) {
                        hub.warn("catalogue_default_moved", line.clone());
                    }
                }
            }
        }
        if let Some(why) = look.failed() {
            let day = day_of(SystemTime::now());
            if warned != Some(day) {
                warned = Some(day);
                // The live sessions hear it once a day, on their own hub: a head that is open
                // shows it beside the conversation, and a daemon nobody is watching says it in
                // the log alone rather than waiting for somebody to attach.
                for session in registry.list() {
                    if let Some(hub) = registry.get(&session.session_id) {
                        hub.warn("catalogue_refresh_failed", why.to_string());
                    }
                }
            }
        }
        let began = std::time::Instant::now();
        while began.elapsed() < TICK {
            if registry.is_closing() || registry.is_closed() {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

/// The day a warning row is counted against: UTC days since 1970, so the row is at most one a day
/// however long the daemon runs and whatever the local timezone does.
fn day_of(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
