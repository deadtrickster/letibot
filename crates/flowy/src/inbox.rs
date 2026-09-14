//! The [`Condition`] a session's `flowy` monitor watches.
//!
//! One per attached session. The seat hands every delivery to every attached
//! condition; each one runs it through **its own** attention table and
//! subscription set, keeps what passed, counts what did not, and pings the
//! monitor poller. `met` drains everything pending into **one** firing — a
//! burst of five messages is one wake, not five — and the firing carries the
//! counts of what went past, so a session can tell a busy room it asked to be
//! spared from a silent one.
//!
//! # Why the condition is where the table lives
//!
//! The table is the session's, not the seat's. Two sessions on one seat can want
//! different things — one working `#build` at `all`, another only its mentions —
//! and the seat polls at the loosest of them. Keeping the table here rather
//! than on the seat is what makes that a table rather than a flag.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use letibot_tools::exec::monitor::{Condition, Wait};

use crate::attention::{Attention, Identity};
use crate::client::Event;
use crate::render;
use crate::subs::{EntityChange, Subscription};

/// What the seat fans out. `Message` is much the largest and by far the most
/// common; boxing it would cost every delivery an allocation to save a notice
/// a few words.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Delivery {
    /// Something said: runs through the attention table.
    Message(Event),
    /// A watched entity moved: runs through the subscription set.
    Entity(EntityChange),
    /// The seat talking about itself — stalled, reattached, stopped, a backlog
    /// label. Always delivered.
    Notice(String),
    /// Addressed to THIS session by name — `@seat/session` in the body. Bypasses
    /// the table, the own-actor rule included: the tag is the recipient's name,
    /// and two sessions on one seat are the same actor on the node.
    Direct(Event),
}

pub struct InboxCondition {
    seat: String,
    /// Which session this is for. A label for listings, and the second half of
    /// this session's address on the fabric: `@seat/session`.
    pub session: String,
    /// A short name for the same address — the session's title, so a person can
    /// write `@seat/planner` rather than a timestamped id. Matched
    /// case-insensitively; empty is no alias.
    alias: Mutex<String>,
    me: Mutex<Identity>,
    attention: Mutex<Attention>,
    subs: Mutex<BTreeSet<Subscription>>,
    /// Rendered, in arrival order.
    pending: Mutex<Vec<String>>,
    local_skipped: AtomicUsize,
    server_skipped: AtomicI64,
    /// The node's clock at the last delivery.
    now: Mutex<String>,
    signal: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl std::fmt::Debug for InboxCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "InboxCondition({} for {})", self.seat, self.session)
    }
}

impl InboxCondition {
    pub fn new(seat: &str, session: &str, me: Identity, attention: Attention) -> Arc<Self> {
        Arc::new(InboxCondition {
            seat: seat.to_string(),
            session: session.to_string(),
            alias: Mutex::new(String::new()),
            me: Mutex::new(me),
            attention: Mutex::new(attention),
            subs: Mutex::new(BTreeSet::new()),
            pending: Mutex::new(Vec::new()),
            local_skipped: AtomicUsize::new(0),
            server_skipped: AtomicI64::new(0),
            now: Mutex::new(String::new()),
            signal: Mutex::new(None),
        })
    }

    pub fn set_alias(&self, alias: &str) {
        *self.alias.lock().unwrap_or_else(|e| e.into_inner()) = alias.trim().to_string();
    }

    pub fn alias(&self) -> String {
        self.alias.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `@seat/session` — how somebody on the fabric names exactly this session.
    pub fn address(&self) -> String {
        format!("@{}/{}", self.seat, self.session)
    }

    /// Whether `fragment` — the part after `@seat/` — names this session: its
    /// id exactly, or its alias ignoring case.
    pub fn is_addressed_as(&self, fragment: &str) -> bool {
        if fragment == self.session {
            return true;
        }
        let alias = self.alias();
        !alias.is_empty() && alias.eq_ignore_ascii_case(fragment)
    }

    /// Count a message that went to another session on this seat by name.
    pub fn note_addressed_elsewhere(&self) {
        self.local_skipped.fetch_add(1, Ordering::SeqCst);
    }

    pub fn set_identity(&self, me: Identity) {
        *self.me.lock().unwrap_or_else(|e| e.into_inner()) = me;
    }

    pub fn identity(&self) -> Identity {
        self.me.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn attention(&self) -> Attention {
        self.attention
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn edit_attention(&self, f: impl FnOnce(&mut Attention)) -> Attention {
        let mut a = self.attention.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut a);
        a.clone()
    }

    pub fn subscriptions(&self) -> BTreeSet<Subscription> {
        self.subs.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Record a subscription. A thread goes into the attention table too — that
    /// is its transport.
    pub fn subscribe(&self, sub: Subscription) -> bool {
        if let Subscription::Thread(t) = &sub {
            self.edit_attention(|a| {
                a.threads.insert(t.clone());
            });
        }
        self.subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(sub)
    }

    pub fn unsubscribe(&self, sub: &Subscription) -> bool {
        if let Subscription::Thread(t) = sub {
            self.edit_attention(|a| {
                a.threads.remove(t);
            });
        }
        self.subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(sub)
    }

    fn ping(&self) {
        let s = self
            .signal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(f) = s {
            f();
        }
    }

    /// The seat's clock note, from the last page.
    pub fn set_now(&self, now: &str) {
        if !now.is_empty() {
            *self.now.lock().unwrap_or_else(|e| e.into_inner()) = now.to_string();
        }
    }

    pub fn add_server_skipped(&self, n: i64) {
        if n > 0 {
            self.server_skipped.fetch_add(n, Ordering::SeqCst);
        }
    }

    /// Offer one delivery. Returns whether it was kept.
    pub fn offer(&self, d: &Delivery) -> bool {
        let kept = self.keep(d);
        if kept {
            self.ping();
        }
        kept
    }

    /// Offer a page: everything is queued, then the poller is pinged **once**, so
    /// a page of five is one firing rather than a race between the poller's
    /// tick and the fan-out. Returns how many were kept.
    pub fn offer_all(&self, ds: &[Delivery]) -> usize {
        let kept = ds.iter().filter(|d| self.keep(d)).count();
        if kept > 0 {
            self.ping();
        }
        kept
    }

    fn keep(&self, d: &Delivery) -> bool {
        let kept = match d {
            Delivery::Message(e) => {
                let me = self.identity();
                let verdict = self.attention().wakes_for(&me, e);
                if verdict.wakes() {
                    Some(render::message(e, &me))
                } else {
                    self.local_skipped.fetch_add(1, Ordering::SeqCst);
                    None
                }
            }
            Delivery::Entity(c) => {
                if self
                    .subs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&c.sub)
                {
                    Some(format!("[{}] {}\n", c.sub, c.what))
                } else {
                    None
                }
            }
            Delivery::Notice(n) => Some(format!("[seat] {n}\n")),
            Delivery::Direct(e) => {
                let me = self.identity();
                let mut r = render::message(e, &me);
                // The header line ends with the newline the body follows; the
                // note goes on the header.
                if let Some(nl) = r.find('\n') {
                    r.insert_str(nl, &format!(" · to this session ({})", self.address()));
                }
                Some(r)
            }
        };
        let Some(text) = kept else {
            return false;
        };
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(text);
        true
    }

    /// How much is waiting, for a listing.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Condition for InboxCondition {
    fn met(&self) -> Option<String> {
        let drained: Vec<String> = {
            let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if p.is_empty() {
                return None;
            }
            std::mem::take(&mut *p)
        };
        let local = self.local_skipped.swap(0, Ordering::SeqCst);
        let server = self.server_skipped.swap(0, Ordering::SeqCst);
        let now = self.now.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Some(render::batch(&self.seat, &drained, local, server, &now))
    }

    fn wait(&self) -> Wait {
        // Push: the seat pings on every delivery. A long block is fine because
        // the poller wakes on the signal, not on the deadline.
        Wait::Block(std::time::Duration::from_secs(30))
    }

    fn install(self: Arc<Self>, signal: Arc<dyn Fn() + Send + Sync>) {
        *self.signal.lock().unwrap_or_else(|e| e.into_inner()) = Some(signal);
        // Something may already be waiting — a backlog offered at attach, before
        // the monitor was declared.
        if self.pending_count() > 0 {
            self.ping();
        }
    }

    fn describe(&self) -> String {
        let a = self.attention();
        format!(
            "flowy seat `{}`, this session is {}, {}",
            self.seat,
            self.address(),
            a.describe()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::Level;
    use serde_json::json;

    fn me() -> Identity {
        Identity {
            user_id: "U".into(),
            agent_id: "A".into(),
            name: "seat".into(),
        }
    }

    fn ev(room: &str, to: &str, body: &str) -> Event {
        serde_json::from_value(json!({
            "id": format!("e-{body}"), "type": "chat", "room": room, "project": "Lab",
            "actor": "X", "addressee": to, "body": body, "meta": {"actor_kind": "agent"}
        }))
        .unwrap()
    }

    #[test]
    fn a_burst_is_one_firing_and_the_skipped_are_counted() {
        let c = InboxCondition::new("seat", "s1", me(), Attention::default());
        let pings = Arc::new(AtomicUsize::new(0));
        let p = pings.clone();
        c.clone().install(Arc::new(move || {
            p.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(c.offer(&Delivery::Message(ev("general", "A", "one"))));
        assert!(c.offer(&Delivery::Message(ev("general", "A", "two"))));
        assert!(!c.offer(&Delivery::Message(ev("general", "", "not for me"))));
        c.add_server_skipped(4);
        c.set_now("2026-09-14T09:00:00Z");
        assert_eq!(pings.load(Ordering::SeqCst), 2);
        let why = c.met().unwrap();
        assert!(
            why.starts_with("[flowy] 2 message(s) for seat `seat`:\n"),
            "{why}"
        );
        assert!(why.contains("  one\n"));
        assert!(why.contains("  two\n"));
        assert!(
            why.contains(
                "1 went past that your attention table did not ask for; 4 the node filtered"
            ),
            "{why}"
        );
        assert!(why.contains("clock reads 2026-09-14T09:00:00Z"));
        // Drained: nothing more.
        assert!(c.met().is_none());
    }

    #[test]
    fn an_entity_change_reaches_only_the_session_that_subscribed() {
        let a = InboxCondition::new("seat", "a", me(), Attention::default());
        let b = InboxCondition::new("seat", "b", me(), Attention::default());
        a.subscribe(Subscription::Todo("T1".into()));
        let d = Delivery::Entity(EntityChange {
            sub: Subscription::Todo("T1".into()),
            what: "todo T1: todo.note by OP".into(),
        });
        assert!(a.offer(&d));
        assert!(!b.offer(&d));
        assert!(
            a.met()
                .unwrap()
                .contains("[todo T1] todo T1: todo.note by OP")
        );
    }

    #[test]
    fn a_thread_subscription_is_an_attention_override() {
        let c = InboxCondition::new("seat", "s", me(), Attention::default());
        c.edit_attention(|a| a.set_room("noisy", Level::Off));
        let mut e = ev("noisy", "", "reply");
        e.thread = "T".into();
        assert!(!c.offer(&Delivery::Message(e.clone())));
        c.subscribe(Subscription::Thread("T".into()));
        assert!(c.offer(&Delivery::Message(e)));
        assert!(c.unsubscribe(&Subscription::Thread("T".into())));
        assert!(c.attention().threads.is_empty());
    }

    #[test]
    fn a_notice_is_always_delivered() {
        let c = InboxCondition::new("seat", "s", me(), Attention::default());
        assert!(c.offer(&Delivery::Notice("stalled".into())));
        assert!(c.met().unwrap().contains("[seat] stalled"));
    }
}
