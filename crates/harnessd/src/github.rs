//! The daemon's half of the GitHub reviewer seat (R118): the seat it holds for `letibot attach
//! github <scope>`, the review panel a pull request is judged by, and the feed `/pane github/github`
//! is drawn from. The seat itself - the poll, the rows, the drafts - is `letibot_github`'s.
//!
//! The seat lives HERE and not in the terminal that asked for it, because a pane is drawn from the
//! daemon's own snapshot: `letibot attach github <scope>` asks this daemon to hold a scope, and the
//! scope is polled for as long as the daemon runs.

pub mod panel;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use letibot_github::mirror::Mirrors;
use letibot_github::offers::{Candidate, Candidates, Kind, Scorer};
use letibot_github::seat::{Config, Reviewer, Seat, Snapshot};
use letibot_github::source::{Scope, Source};
use letibot_sessionlog::registry::{GithubSeat, Registry};
use serde_json::{Value, json};

use crate::mergequeue::review::claude;

/// What one poll pass asks GitHub through, and what judges a pull request. Built per pass: a
/// reviewer holds the state of one review, and a source is cheap to make.
pub struct Parts {
    pub source: Box<dyn Source + Send + Sync>,
    pub reviewer: Box<dyn Reviewer + Send>,
}

/// How a pass is made. A closure rather than two fixed objects so a test drives the seat with a
/// fake source and a stub panel, and the daemon with `gh` and its own mirror.
type Maker = Box<dyn FnMut() -> Result<Parts, String> + Send>;

/// The daemon's GitHub reviewer: every scope an operator attached, the poll that keeps them, and the
/// snapshot the pane is drawn from.
pub struct Github {
    /// This seat as the registry holds it, so a pass can run on a thread of its own. Set by
    /// [`Github::install`], after every builder has run and before anything can ask for a pass.
    me: OnceLock<Weak<Github>>,
    /// The seat, held for the length of a pass and a control that arrives mid-pass.
    seat: Mutex<Seat>,
    /// The last pass's rows in the pane's words, under its own lock so a head is drawn while the
    /// seat is mid-pass: a pane is a view of what was measured, and a stale age is honest.
    snapshot: Mutex<Snapshot>,
    /// How a pass is made. `None` is a daemon that cannot poll - it holds the scopes and the feed,
    /// and says on every scope that it has nothing to ask GitHub through.
    pass: Mutex<Option<Maker>>,
    /// One pass at a time: a pass that is still running means the next tick is skipped, not queued.
    polling: AtomicBool,
    at_ms: Mutex<u64>,
    /// Where the pull requests' mirrors go, once a caller named one.
    root: Option<PathBuf>,
}

/// How long a scope's poll waits between passes, in the daemon's own clock - the doc's minute.
pub const POLL_MS: u64 = 60_000;

impl Github {
    /// The daemon's seat over nothing: scopes are attached later, by `letibot attach github`.
    pub fn new(config: Config) -> Github {
        Github {
            me: OnceLock::new(),
            seat: Mutex::new(Seat::new(Vec::new(), config)),
            snapshot: Mutex::new(Snapshot::default()),
            pass: Mutex::new(None),
            polling: AtomicBool::new(false),
            at_ms: Mutex::new(0),
            root: None,
        }
    }

    /// The seat as the registry holds it, with its own name in hand so a due pass can run on a
    /// thread. Called once, after every builder: a `Weak` because the seat must not keep itself
    /// alive, and nothing asks for a pass before this.
    pub fn install(self) -> Arc<Github> {
        let me = Arc::new(self);
        let _ = me.me.set(Arc::downgrade(&me));
        me
    }

    /// Give the seat the way a pass is made, and the offers it makes: R114's two sources.
    pub fn with_pass(
        mut self,
        pass: Maker,
        candidates: Box<dyn Candidates>,
        scorer: Box<dyn Scorer>,
    ) -> Github {
        *self.pass.get_mut().unwrap_or_else(|e| e.into_inner()) = Some(pass);
        let seat = self.seat.get_mut().unwrap_or_else(|e| e.into_inner());
        let held = seat.held().into_iter().cloned().collect();
        let config = seat.config().clone();
        *seat = Seat::new(held, config).with_offers(candidates, scorer);
        self
    }

    /// The daemon's own way: `gh` for the source, the review panel for the reviewer, the tree of each
    /// pull request cut from its own repository's mirror, and the daemon's own open todos and running
    /// sessions as what a pull request's words are scored against.
    pub fn production(
        self,
        root: PathBuf,
        settings: Option<claude::Settings>,
        registry: Arc<Registry>,
    ) -> Github {
        let runs = Arc::new(claude::Runs::new());
        let config = self
            .seat
            .lock()
            .map(|s| s.config().clone())
            .unwrap_or_default();
        let token = config.token.clone();
        let mirrors = root.clone();
        let pass: Maker = Box::new(move || {
            let members: Vec<Box<dyn panel::Member>> = match &settings {
                Some(settings) => {
                    vec![Box::new(panel::Claude::new(runs.clone(), settings.clone()))]
                }
                None => Vec::new(),
            };
            Ok(Parts {
                source: Box::new(letibot_github::gh::Gh::new(token.clone())),
                reviewer: Box::new(panel::Panel::new(Mirrors::new(mirrors.clone()), members)),
            })
        });
        let mut github = self.with_pass(pass, Box::new(Around::of(registry)), Box::new(Lexical));
        // And the mirror root the panel cuts its trees under, so a caller can read it back.
        github.root = Some(root);
        github
    }

    /// Where the pull requests' mirrors go, once a caller named one.
    pub fn root(&self) -> Option<&PathBuf> {
        self.root.as_ref()
    }

    /// What the seat may send: `post = false` is the shipped state and nothing leaves the box.
    pub fn config(&self) -> Config {
        self.seat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .config()
            .clone()
    }

    /// The scopes this daemon holds - a pane is only told about a seat somebody attached.
    pub fn scopes(&self) -> Vec<String> {
        self.seat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .held()
            .into_iter()
            .map(|s| s.as_str().to_string())
            .collect()
    }

    /// `letibot attach github <scope>`: this daemon holds the scope from the next pass on.
    pub fn attach(&self, scope: &str) -> Value {
        let scope = match Scope::parse(scope) {
            Ok(scope) => scope,
            Err(why) => return json!({"error": why}),
        };
        let mut seat = self.seat.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = seat.attach(scope.clone());
        let snapshot = seat.snapshot();
        drop(seat);
        self.publish(snapshot);
        json!({
            "ok": true,
            "scope": scope.as_str(),
            "fresh": fresh,
            "snapshot": self.document(),
        })
    }

    /// Stop asking GitHub for one scope. The rows stay as they are: a pause is not a reset.
    pub fn pause(&self, scope: &str) -> Value {
        let mut seat = self.seat.lock().unwrap_or_else(|e| e.into_inner());
        seat.pause(scope);
        let snapshot = seat.snapshot();
        drop(seat);
        self.publish(snapshot);
        self.document()
    }

    /// Ask again.
    pub fn resume(&self, scope: &str) -> Value {
        let mut seat = self.seat.lock().unwrap_or_else(|e| e.into_inner());
        seat.resume(scope);
        let snapshot = seat.snapshot();
        drop(seat);
        self.publish(snapshot);
        self.document()
    }

    /// The snapshot, as the pane's feed is spelled: the same document the terminal's seat printed,
    /// now the daemon's.
    pub fn document(&self) -> Value {
        serde_json::to_value(self.snapshot()).unwrap_or_else(|e| json!({"error": e.to_string()}))
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn publish(&self, snapshot: Snapshot) {
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = snapshot;
    }

    /// One pass over every held scope. `None` when a pass is already running - the second tick is
    /// skipped and not queued, so a slow pass cannot build a backlog of them.
    pub fn poll(&self, now_ms: u64) -> Option<Vec<String>> {
        if self.polling.swap(true, Ordering::SeqCst) {
            return None;
        }
        Some(self.pass(now_ms))
    }

    /// One pass, with the flag already taken by the caller.
    fn pass(&self, now_ms: u64) -> Vec<String> {
        let made = {
            let mut pass = self.pass.lock().unwrap_or_else(|e| e.into_inner());
            match pass.as_mut() {
                Some(make) => (make)(),
                None => Err("this daemon has no way to ask GitHub: no `gh` and no panel".into()),
            }
        };
        let said = match made {
            Ok(Parts {
                source,
                mut reviewer,
            }) => {
                let mut seat = self.seat.lock().unwrap_or_else(|e| e.into_inner());
                let said = seat.poll(&*source, &mut *reviewer, now_ms);
                let snapshot = seat.snapshot();
                drop(seat);
                self.publish(snapshot);
                said
            }
            Err(why) => {
                // Said on the scope and not on every row: a seat with no way to ask is one fact.
                let mut seat = self.seat.lock().unwrap_or_else(|e| e.into_inner());
                let held: Vec<String> = seat
                    .held()
                    .into_iter()
                    .map(|s| s.as_str().to_string())
                    .collect();
                for scope in held {
                    seat.fail_scope(&scope, why.clone());
                }
                let snapshot = seat.snapshot();
                drop(seat);
                self.publish(snapshot);
                vec![why]
            }
        };
        *self.at_ms.lock().unwrap_or_else(|e| e.into_inner()) = now_ms;
        self.polling.store(false, Ordering::SeqCst);
        said
    }

    /// When the last pass was made, so the daemon's own clock can tell a due poll from an idle one.
    pub fn polled_at(&self) -> u64 {
        *self.at_ms.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether a pass is due: a scope is attached, none is running, and the minute since the last
    /// one is up - or nothing has been polled at all.
    pub fn due(&self, now_ms: u64) -> bool {
        if self.polling.load(Ordering::SeqCst) || self.scopes().is_empty() {
            return false;
        }
        let at = self.polled_at();
        at == 0 || now_ms.saturating_sub(at) >= POLL_MS
    }

    /// What a `letibot attach github` request means, as the daemon answers it: one verb and its
    /// scope, answered with the snapshot the pane would be drawn from.
    pub fn reply(&self, op: &Value) -> Value {
        let verb = op.get("verb").and_then(Value::as_str).unwrap_or("list");
        let scope = op.get("scope").and_then(Value::as_str).unwrap_or_default();
        match verb {
            "attach" if scope.is_empty() => json!({
                "error": "attach takes a scope: `letibot attach github <org|owner/repo>`"
            }),
            "attach" => {
                let out = self.attach(scope);
                // An attach polls once, now, on its own thread: the answer to the operator is not
                // kept waiting for a review that takes minutes.
                self.poll_due(now_ms());
                out
            }
            "pause" => self.pause(scope),
            "resume" => self.resume(scope),
            "poll" => match self.poll(now_ms()) {
                Some(said) => json!({"ok": true, "said": said, "snapshot": self.document()}),
                None => json!({"ok": false, "said": ["a pass is already running"]}),
            },
            "list" => self.document(),
            other => json!({
                "error": format!(
                    "no verb `{other}`: the verbs are attach, pause, resume, poll, list"
                )
            }),
        }
    }
}

impl GithubSeat for Github {
    fn request(&self, op: &Value) -> Value {
        self.reply(op)
    }

    /// Nobody has attached a scope yet is no key at all: the pane says *no scope is attached*
    /// rather than drawing an empty seat.
    fn feed(&self) -> Option<Value> {
        (!self.scopes().is_empty()).then(|| self.document())
    }

    /// One pass, on a thread of its own: a review of a pull request is minutes of a member's work,
    /// and the daemon's own idle pass is not where that waits.
    fn poll_due(&self, at: u64) -> bool {
        if !self.due(at) || self.polling.swap(true, Ordering::SeqCst) {
            return false;
        }
        let Some(me) = self.me.get().and_then(|me| me.upgrade()) else {
            self.polling.store(false, Ordering::SeqCst);
            return false;
        };
        let started = std::thread::Builder::new()
            .name("github-seat".into())
            .spawn(move || me.pass(at));
        if started.is_err() {
            self.polling.store(false, Ordering::SeqCst);
        }
        started.is_ok()
    }
}

/// The tree's clock, for the daemon's door: one clock for every row's age and the poll's window.
fn now_ms() -> u64 {
    crate::mergequeue::gate::now_ms()
}

/// The open todos and the running sessions' titles, read off the registry at each poll: what a new
/// pull request's words are scored against, so the row that is the same work is pointed at.
pub struct Around {
    registry: Option<Arc<Registry>>,
}

impl Around {
    /// A source with nothing in it: a daemon with no registry (a test) and a seat that makes no
    /// offers rather than an offer against an empty list.
    pub fn nothing() -> Around {
        Around { registry: None }
    }

    /// The daemon's own lists.
    pub fn of(registry: Arc<Registry>) -> Around {
        Around {
            registry: Some(registry),
        }
    }
}

impl Candidates for Around {
    fn candidates(&self) -> Vec<Candidate> {
        let Some(registry) = &self.registry else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for brief in registry.list() {
            if !brief.live {
                continue;
            }
            let title = brief.title.trim();
            if title.is_empty() {
                continue;
            }
            out.push(Candidate {
                kind: Kind::Watch,
                label: format!("the running session `{title}`"),
                text: title.to_string(),
            });
            for todo in registry.todos(&brief.session_id) {
                if !matches!(
                    todo.status,
                    letibot_sessionlog::event::TodoStatus::Pending
                        | letibot_sessionlog::event::TodoStatus::InProgress
                ) {
                    continue;
                }
                let text = todo.content.trim();
                if text.is_empty() {
                    continue;
                }
                out.push(Candidate {
                    kind: Kind::Watch,
                    label: text.to_string(),
                    text: text.to_string(),
                });
            }
        }
        out
    }
}

/// The tree's lexical score, which is the one R114 kept: a corpus of the candidates, and the subject
/// weighed against the words that corpus makes cheap.
pub struct Lexical;

impl Scorer for Lexical {
    fn scores(&self, subject: &str, texts: &[&str]) -> Vec<f64> {
        if texts.is_empty() {
            return Vec::new();
        }
        // The candidate's INDEX is its corpus path, so a score comes back to the right candidate
        // without a second list to keep in step.
        let files: Vec<(PathBuf, String)> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| (PathBuf::from(i.to_string()), (*text).to_string()))
            .collect();
        let corpus = letibot_tools::similarity::Corpus::of_texts(&files);
        let mut out = vec![0.0; texts.len()];
        for (path, score) in corpus.ranked_with(subject, texts.len(), None) {
            if let Some(i) = path
                .to_str()
                .and_then(|name| name.parse::<usize>().ok())
                .filter(|i| *i < out.len())
            {
                out[i] = score;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
