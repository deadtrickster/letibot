//! Secrets the person gives, and the daemon's memory of them.
//!
//! The value never enters the log: it travels from the head's card to the helper that asked, and
//! what this module keeps is the *window* an ask may be answered from — in this process's memory,
//! for one session tree, never on disk and never in a row. A key is what makes an ask cacheable,
//! and what makes two asks at once one card.
//!
//! Three sentences are the whole design:
//!
//! - a key with a live value is answered without a card, and nothing is announced;
//! - a key whose card is already up JOINS it, so a second helper waits on the same person;
//! - an ask with no key is its own card, because there is no name to file it under later.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::hub::Hub;
use crate::registry::Registry;

/// The longest a value may be kept, whatever the asker asked: a password sitting in a daemon's
/// memory for longer than an hour is a password nobody is thinking about any more.
pub const KEEP_MOST: Duration = Duration::from_secs(60 * 60);

/// What one ask is for: every fact the card shows, and the name the value is filed under.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretWant {
    /// The prompt the program used (`sudo`'s own, or `ssh`'s).
    pub prompt: String,
    /// The command it came from — the session's `LETIBOT_COMMAND`, which is what a person
    /// recognises the ask by.
    pub command: String,
    /// `letibot-secret --for`: what will use the value, in the asker's own words.
    pub for_what: String,
    /// The name the value is filed under. `None` is a one-off.
    pub key: Option<String>,
    /// How long the asker allows the value to be kept.
    pub keep: Option<Duration>,
    /// The machine(s) the value is going to, as the asker spelled them.
    pub host: String,
}

impl SecretWant {
    /// The keep the card may offer: what was asked, clamped to [`KEEP_MOST`]. `None` when the ask
    /// has no key, because a value with no name to file it under cannot be found again.
    pub fn offered_keep(&self) -> Option<Duration> {
        self.key.as_ref()?;
        self.keep.map(|k| k.min(KEEP_MOST))
    }
}

/// What a helper is told when its ask settles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answered {
    /// The value, from a card or from a live cache entry.
    Value(String),
    /// A person was shown the card and declined it. A decision, not a silence.
    Refused,
    /// Nobody answered before the deadline — or the card's own asker went away.
    TimedOut,
}

/// What one card settled to, sent to every ask that was waiting on it.
#[derive(Clone)]
enum Settled {
    /// The value, for every ask that was waiting on this card. The window is not part of it: a
    /// joiner asked for a value, and the window is the card's own business.
    Value(String),
    Refused,
    GaveUp,
}

/// A value and the window it may be answered from.
struct Kept {
    value: String,
    until: Instant,
}

/// A card that is up, and every ask waiting on it.
struct Open {
    card: String,
    waiting: u32,
    waiters: Vec<mpsc::SyncSender<Settled>>,
}

/// The daemon's whole memory of given secrets. One per registry, so a session tree's subagents
/// share it and no other tree does.
#[derive(Default)]
pub struct SecretVault {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    kept: HashMap<(String, String), Kept>,
    open: HashMap<(String, String), Open>,
}

impl std::fmt::Debug for SecretVault {
    /// The values are the one thing this type exists to hold, and a `Debug` print is a log line
    /// waiting to happen: it names the windows and never a byte of what they hold.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let g = self.lock();
        let kept: Vec<&(String, String)> = g.kept.keys().collect();
        let open: Vec<&(String, String)> = g.open.keys().collect();
        f.debug_struct("SecretVault")
            .field("kept", &kept)
            .field("open", &open)
            .finish()
    }
}

impl SecretVault {
    pub fn new() -> SecretVault {
        SecretVault::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock means a thread panicked while holding it, and every mutation under it is
        // infallible: recovering is strictly better than refusing every later ask for a password.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Is `key` answerable without a card, in `tree`, right now?
    pub fn held(&self, tree: &str, key: &str) -> Option<String> {
        let mut g = self.lock();
        let slot = (tree.to_string(), key.to_string());
        match g.kept.get(&slot) {
            Some(k) if k.until > Instant::now() => Some(k.value.clone()),
            // Expired: forgotten here, so the next ask costs a card and not a stale answer.
            Some(_) => {
                g.kept.remove(&slot);
                None
            }
            None => None,
        }
    }

    fn keep(&self, slot: (String, String), value: String, window: Duration) {
        let mut g = self.lock();
        g.kept.insert(
            slot,
            Kept {
                value,
                until: Instant::now() + window,
            },
        );
    }

    /// The card is over: take its waiters, if it is still the card this key is filed under. A
    /// later ask's card is left alone, which is what keeps one key's answer off another's.
    fn close(&self, slot: &(String, String), card: &str) -> Vec<mpsc::SyncSender<Settled>> {
        let mut g = self.lock();
        match g.open.get(slot) {
            Some(o) if o.card == card => g.open.remove(slot).map(|o| o.waiters).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Forget everything `tree` kept. The session tree is over; a value it was given is not a
    /// value the next tree inherits.
    pub fn forget_tree(&self, tree: &str) {
        let mut g = self.lock();
        g.kept.retain(|(t, _), _| t != tree);
    }

    /// Every value this daemon holds for `tree`, and how long is left of its window. For a test
    /// and a diagnostic line, never for a reader: the keys are the names, never the values.
    pub fn windows(&self, tree: &str) -> Vec<(String, Duration)> {
        let now = Instant::now();
        let g = self.lock();
        g.kept
            .iter()
            .filter(|((t, _), k)| t == tree && k.until > now)
            .map(|((_, key), k)| (key.clone(), k.until - now))
            .collect()
    }
}

/// Ask for a secret: the whole of the daemon's side, from one helper's frame to its answer.
///
/// `hub` is the session the helper belongs to (`tree_root()` is the scope), and `patience` is how
/// long a helper waits before it gives up. A refusal's SENTENCE belongs to the caller, which is
/// what knows how many heads were attached.
pub fn obtain(
    registry: &Arc<Registry>,
    hub: &Arc<Hub>,
    want: SecretWant,
    patience: Duration,
) -> Answered {
    let tree = hub.tree_root();
    let slot = want.key.as_ref().map(|k| (tree.clone(), k.clone()));
    let deadline = crate::event::now_ms() + patience.as_millis() as u64;
    let (tx, rx) = mpsc::sync_channel(1);

    {
        let mut g = registry.vault().lock();
        // The cache first: a live value is not a question.
        let mut hit = None;
        if let Some(slot) = &slot {
            match g.kept.get(slot) {
                Some(k) if k.until > Instant::now() => hit = Some(k.value.clone()),
                Some(_) => {
                    g.kept.remove(slot);
                }
                None => {}
            }
        }
        if let Some(v) = hit {
            return Answered::Value(v);
        }
        let joined = match &slot {
            Some(slot) => g.open.get_mut(slot).map(|open| {
                open.waiting += 1;
                open.waiters.push(tx.clone());
                (open.card.clone(), open.waiting)
            }),
            None => None,
        };
        match joined {
            // Somebody else's card is up: this ask waits on it and never raises a second one.
            // A card that now answers more than one ask says so, so the person is not asked
            // twice for one password and the card can count the waits it settles.
            Some((card, waiting)) => {
                drop(g);
                hub.announce_secret_waiting(&card, waiting);
            }
            None => {
                let (card, hub_rx) = hub.request_secret(&want, deadline);
                if let Some(slot) = &slot {
                    g.open.insert(
                        slot.clone(),
                        Open {
                            card: card.clone(),
                            waiting: 1,
                            waiters: vec![tx],
                        },
                    );
                }
                drop(g);
                // This call raised the card, so this call waits on the head and answers the
                // others: no thread outlives the helper that asked.
                let settled = match hub_rx.recv_timeout(patience) {
                    Ok(Some((value, keep))) => {
                        // Given: masked in this tree's output from now on, and kept only when the
                        // person said so and there is a name to file it under.
                        registry.secret_given(&tree, &value);
                        if let (Some(slot), Some(keep)) = (slot.clone(), keep) {
                            registry.vault().keep(slot, value.clone(), keep);
                        }
                        Settled::Value(value)
                    }
                    Ok(None) => Settled::Refused,
                    Err(_) => {
                        hub.abandon_secret(&card, "nobody, before the deadline");
                        Settled::GaveUp
                    }
                };
                if let Some(slot) = &slot {
                    for w in registry.vault().close(slot, &card) {
                        let _ = w.try_send(settled.clone());
                    }
                }
                return match settled {
                    Settled::Value(v) => Answered::Value(v),
                    Settled::Refused => Answered::Refused,
                    Settled::GaveUp => Answered::TimedOut,
                };
            }
        }
    }
    // Only a joining ask reaches this line: the ask that raised the card returned above.
    match rx.recv_timeout(patience + Duration::from_secs(5)) {
        Ok(Settled::Value(v)) => Answered::Value(v),
        Ok(Settled::Refused) => Answered::Refused,
        Ok(Settled::GaveUp) | Err(_) => Answered::TimedOut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window is a clock, not a promise: the value is answered inside it and forgotten at its
    /// end. Measured with a short window rather than by moving a clock, because the clock here is
    /// `Instant` and the point is that the expiry is honoured at all.
    #[test]
    fn a_kept_value_is_answered_inside_its_window_and_not_after() {
        let v = SecretVault::new();
        let slot = ("s-1".to_string(), "fleet-sudo".to_string());
        v.keep(
            slot.clone(),
            "hunter2hunter2".into(),
            Duration::from_millis(120),
        );
        assert_eq!(
            v.held("s-1", "fleet-sudo").as_deref(),
            Some("hunter2hunter2")
        );
        std::thread::sleep(Duration::from_millis(160));
        assert_eq!(v.held("s-1", "fleet-sudo"), None, "the window is over");
        // And the expired row is gone rather than dormant: a second look finds nothing either.
        assert!(v.windows("s-1").is_empty());
    }

    /// The tree is the scope. One tree's value is not another's, and a tree that ends takes its
    /// values with it.
    #[test]
    fn a_value_belongs_to_its_tree_and_goes_with_it() {
        let v = SecretVault::new();
        v.keep(
            ("root".into(), "k".into()),
            "aaaaaaaaaaaa".into(),
            Duration::from_secs(60),
        );
        assert_eq!(v.held("child", "k"), None);
        assert_eq!(v.windows("root").len(), 1);
        v.forget_tree("root");
        assert_eq!(v.held("root", "k"), None);
    }

    /// The keep a card may offer is the asker's, clamped, and a keyless ask offers none: the
    /// clamp is what makes `--keep 1440m` an hour rather than a day.
    #[test]
    fn the_offered_keep_is_clamped_and_needs_a_key() {
        let want = SecretWant {
            key: Some("k".into()),
            keep: Some(Duration::from_secs(24 * 60 * 60)),
            ..SecretWant::default()
        };
        assert_eq!(want.offered_keep(), Some(KEEP_MOST));
        let keyless = SecretWant {
            keep: Some(Duration::from_secs(600)),
            ..SecretWant::default()
        };
        assert_eq!(keyless.offered_keep(), None);
    }
}
