//! **What a session is waiting on** — the daemon's own answer to *will this act again by itself?*
//!
//! # The question, and why a reader outside the daemon could not answer it
//!
//! The engine publishes `TurnStarted` and `TurnFinished` **once per ROUND**
//! ([`crate::hub::Hub::turn_in_flight`] says so at length, and the `TurnEnded` row in
//! `TODO.md` is the defect). So a reader watching the event stream cannot tell
//!
//! * *the model's turn is over and nobody owes it anything* — done, waiting for a person — from
//! * *the turn is between rounds, or a call is running, or a job the work waits on will settle*
//!   — the session will act again on its own, and a caller that reports *done* here hands back a
//!   package with hours of work left in it.
//!
//! `crates/bridge` had to guess that difference from outside, and every guess was a timer: a
//! ten-second settle window, a `wake_due` flag, an edge from the todo board. Each fixed one false
//! wake and left the next. The operator, on the third: *"it is easy to see if any jobs are
//! running, etc etc, why timeouts?"* — and they are right. **The daemon is the only process that
//! knows**: it holds the turn, the calls it proposed, the watchers on its jobs and children, the
//! monitors it armed, and the queue of settlements it has not delivered yet.
//!
//! So this is the published answer: [`Activity`], computed by `letibot_harnessd` (the module is
//! `activity` there too, and it is where the facts are gathered) and published as
//! [`crate::SessionEvent::ActivityChanged`] **only when the value changes** — never per round, never
//! per token. A late head finds the current value in [`crate::view::Snapshot::activity`], the way it
//! finds the turn in flight.
//!
//! # What counts as *acting again on its own*, and what does not
//!
//! The list is [`Waiting`]'s own, and the line it draws is *does the session move without a person
//! touching it?*
//!
//! | waiting on | the session acts again without outside input | why |
//! |---|---|---|
//! | [`Waiting::Turn`] | **yes** | a round is generating |
//! | [`Waiting::Calls`] | **yes** | the round ended and the calls it proposed are running; the next round reads their results |
//! | [`Waiting::Delivery`] | **yes** | a settlement, a monitor firing, or a queued prompt is owed to the model: it will be said, and the turn it starts is the session's own |
//! | [`Waiting::Job`] | **yes**, when its settlement is one the model will be told | see below |
//! | [`Waiting::Monitor`] | **yes** | a monitor is armed, and a firing is `Delivery::Immediate` (`crate::delivery`'s table) — it always wakes the model |
//! | [`Waiting::Child`] | **yes**, on the same rule as a job | a child is a background job (`jobwatch`'s own words) |
//! | [`Waiting::Ask`] | **no** | the session is stopped on a card. This is the ONE that waits for a person |
//!
//! **A job is counted when its settlement is one the session's own work will act on.** That is not
//! "every job": `crate::delivery`'s decision is the same one the delivery path makes, and it is
//! `Delivery::Immediate` — a row of the plan `needs` it or has a `when: job` condition on it, the
//! session asked for it (`job_wait`), or it was started with `wake: "now"` — against
//! `Delivery::Held`, which is a result the model is *shown* at its next break and is not waiting
//! for. A session with a dev server left running in the background is not *busy* for the life of
//! the daemon: the server's ending is not what the work is about, and a caller told to wait for it
//! would wait for ever. What keeps a settlement that is merely held from slipping through is
//! [`Waiting::Delivery`]: the moment it is queued the session is owed a turn, and the delivery
//! window — measured, and the bug `478f415a` was written for — is a `Delivery` and not a silence.
//!
//! A long-running job whose settlement the work waits on therefore counts for as long as it runs,
//! which is the point: that is what the model is blocked on.
//!
//! # Why a value and not a question
//!
//! A verb (`ClientFrame::…Activity`) would have to be asked, and a caller that asks has to decide
//! *how often* — which is a timer with extra steps, and the caller's timer is the thing being
//! deleted. A value that is published when it changes lets `wait` block on a condvar and wake on
//! the change, which is what the bridge does with it.

use serde::{Deserialize, Serialize};

/// **The kind and the id of one thing a busy session waits on** — see the module header for which
/// of them mean *this session acts again on its own* and which one means *a person must*.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "on", content = "id", rename_all = "snake_case")]
pub enum Waiting {
    /// A round is generating.
    Turn,
    /// The round's generation ended and the calls it proposed are running; the next round reads
    /// their results. The window `Hub::turn_in_flight` exists to name.
    Calls,
    /// **The model is owed something and the turn that says it has not started yet** — a
    /// settlement queued or held, a monitor that has fired, a message or a prompt waiting for the
    /// worker. This is the window every timer in `crates/bridge` was invented for.
    Delivery,
    /// A background job of this session's whose settlement the work waits on — see the module
    /// header for the rule (it is `delivery::decide`'s, not a second one).
    Job(String),
    /// A monitor this session armed. A firing is immediate, so this always ends in a turn.
    Monitor(String),
    /// A subagent of this session's. It settles through the same queue a background job does.
    Child(String),
    /// **A card is open and nobody has answered it.** The one entry that does not end by itself.
    Ask(String),
}

impl Waiting {
    /// The word the wire and a head's line use for the kind — `turn`, `calls`, `delivery`, `job`,
    /// `monitor`, `child`, `ask`.
    pub fn kind(&self) -> &'static str {
        match self {
            Waiting::Turn => "turn",
            Waiting::Calls => "calls",
            Waiting::Delivery => "delivery",
            Waiting::Job(_) => "job",
            Waiting::Monitor(_) => "monitor",
            Waiting::Child(_) => "child",
            Waiting::Ask(_) => "ask",
        }
    }

    /// **The id this entry names**, empty for the three that name no thing.
    pub fn id(&self) -> &str {
        match self {
            Waiting::Turn | Waiting::Calls | Waiting::Delivery => "",
            Waiting::Job(id) | Waiting::Monitor(id) | Waiting::Child(id) | Waiting::Ask(id) => id,
        }
    }

    /// **`job j8032`, `monitor ci`, `ask adj-s1-0001`** — how a line that shows the list says one
    /// entry, and how `leticode_status` names it as data. One function rather than one per reader,
    /// because two spellings of the same fact is the drift this file exists to stop.
    pub fn label(&self) -> String {
        match self.id() {
            "" => self.kind().to_string(),
            id => format!("{} {id}", self.kind()),
        }
    }

    /// **Does this session move only when a person moves it?** True for [`Waiting::Ask`] and
    /// nothing else — the whole of why `Activity` is not a boolean.
    pub fn needs_a_person(&self) -> bool {
        matches!(self, Waiting::Ask(_))
    }

    /// Where this sits in a line: turn, calls, delivery, then the things by name, then the card.
    /// The order is fixed so that the same set of facts always spells the same [`Activity`], which
    /// is what makes *published only when it changes* a comparison and not a coin toss.
    fn rank(&self) -> u8 {
        match self {
            Waiting::Turn => 0,
            Waiting::Calls => 1,
            Waiting::Delivery => 2,
            Waiting::Job(_) => 3,
            Waiting::Monitor(_) => 4,
            Waiting::Child(_) => 5,
            Waiting::Ask(_) => 6,
        }
    }
}

/// **What a session is waiting on, as the daemon computed it** — [`Activity::Quiet`] or the list.
///
/// `Quiet` is *nothing is running and nothing is owed*: the session is where a person left it. It
/// is deliberately not called *idle* — [`crate::hub::SessionStatus::running`] already uses that
/// sense and is false for most of a turn's wall clock.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    /// Nothing running, nothing owed, nobody asked.
    #[default]
    Quiet,
    /// Something is going to happen: by the session's own clock, or by a person's hand.
    Busy { waiting_on: Vec<Waiting> },
}

impl Activity {
    /// A list, in the order [`Waiting::rank`] fixes, with nothing in it read as `Quiet` — so the
    /// daemon cannot publish `Busy { waiting_on: [] }` and mean *quiet* in one place and *busy* in
    /// another.
    pub fn of(mut waiting_on: Vec<Waiting>) -> Activity {
        if waiting_on.is_empty() {
            return Activity::Quiet;
        }
        waiting_on.sort_by(|a, b| a.rank().cmp(&b.rank()).then_with(|| a.id().cmp(b.id())));
        Activity::Busy { waiting_on }
    }

    /// Whether nothing is going on at all.
    pub fn is_quiet(&self) -> bool {
        matches!(self, Activity::Quiet)
    }

    /// The list, empty for [`Activity::Quiet`].
    pub fn waiting_on(&self) -> &[Waiting] {
        match self {
            Activity::Quiet => &[],
            Activity::Busy { waiting_on } => waiting_on,
        }
    }

    /// **The card this session is stopped on**, when it is — the one entry a caller has to do
    /// something about. A session can in principle hold more than one; the oldest is the answer,
    /// because it is the one that has waited longest.
    pub fn ask(&self) -> Option<&str> {
        self.waiting_on().iter().find_map(|w| match w {
            Waiting::Ask(id) => Some(id.as_str()),
            _ => None,
        })
    }

    /// **Whether the session will move without a person** — true when the list holds anything but
    /// a card. A `Busy` made only of an ask is the *stopped* state, which is why this is not
    /// `!is_quiet()`.
    pub fn moves_by_itself(&self) -> bool {
        self.waiting_on().iter().any(|w| !w.needs_a_person())
    }

    /// **`waiting on: job j8032 · monitor ci`**, or empty for a quiet session — the one muted line
    /// a head draws (the TUI's status row) and a caller reads in `leticode_status`.
    pub fn line(&self) -> String {
        let labels: Vec<String> = self.waiting_on().iter().map(Waiting::label).collect();
        if labels.is_empty() {
            return String::new();
        }
        format!("waiting on: {}", labels.join(" · "))
    }

    /// The words a head shows on the status row's right, or empty.
    ///
    /// The same as [`Activity::line`] without the `waiting on:` prefix for the callers that already
    /// say it — `leticode_status`' own key is `waiting_on`, so it hands back the bare list.
    pub fn waiting_labels(&self) -> Vec<String> {
        self.waiting_on().iter().map(Waiting::label).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order is the line's order and the wire's order, and it is fixed here so that two
    /// computations of the same facts are the same value — which is what *published only when it
    /// changes* rests on.
    #[test]
    fn the_list_is_ordered_by_kind_then_name() {
        let a = Activity::of(vec![
            Waiting::Ask("adj-2".into()),
            Waiting::Monitor("ci".into()),
            Waiting::Job("j9".into()),
            Waiting::Job("j10".into()),
            Waiting::Turn,
            Waiting::Delivery,
        ]);
        assert_eq!(
            a.waiting_on(),
            &[
                Waiting::Turn,
                Waiting::Delivery,
                Waiting::Job("j10".into()),
                Waiting::Job("j9".into()),
                Waiting::Monitor("ci".into()),
                Waiting::Ask("adj-2".into()),
            ],
            "kind first, then the id as text"
        );
        assert_eq!(
            a.line(),
            "waiting on: turn · delivery · job j10 · job j9 · monitor ci · ask adj-2"
        );
    }

    /// An empty list is quiet, not a busy session waiting on nothing: the daemon cannot publish
    /// the two as different values, so no reader has to decide what an empty `Busy` means.
    #[test]
    fn nothing_to_wait_on_is_quiet() {
        assert_eq!(Activity::of(Vec::new()), Activity::Quiet);
        assert!(Activity::Quiet.is_quiet());
        assert_eq!(Activity::Quiet.line(), "");
        assert!(!Activity::Quiet.moves_by_itself());
        assert_eq!(Activity::Quiet.ask(), None);
    }

    /// **An ask is the only entry that waits for a person**, and a session stopped on one moves by
    /// itself for nothing — which is the difference `wait`'s exit code 10 is about.
    #[test]
    fn only_a_card_waits_for_a_person() {
        let busy = Activity::of(vec![Waiting::Turn, Waiting::Job("j1".into())]);
        assert!(busy.moves_by_itself());
        assert_eq!(busy.ask(), None);
        let stopped = Activity::of(vec![
            Waiting::Job("j1".into()),
            Waiting::Ask("adj-1".into()),
        ]);
        assert!(stopped.moves_by_itself(), "the job is still running");
        assert_eq!(stopped.ask(), Some("adj-1"));
        let card_only = Activity::of(vec![Waiting::Ask("adj-1".into())]);
        assert!(!card_only.moves_by_itself());
        assert_eq!(card_only.line(), "waiting on: ask adj-1");
    }

    /// The wire shape: a tagged entry, so an id travels beside its kind and never inside it.
    #[test]
    fn an_activity_round_trips_through_the_wire() {
        let a = Activity::of(vec![
            Waiting::Calls,
            Waiting::Job("j8032".into()),
            Waiting::Monitor("ci".into()),
            Waiting::Child("s-sub-1".into()),
            Waiting::Ask("adj-7".into()),
        ]);
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.contains(r#""on":"job","id":"j8032""#), "{s}");
        assert!(
            s.contains(r#""on":"turn""#) || s.contains(r#""on":"calls""#),
            "{s}"
        );
        assert_eq!(serde_json::from_str::<Activity>(&s).unwrap(), a, "{s}");
        assert_eq!(
            serde_json::to_string(&Activity::Quiet).unwrap(),
            r#""quiet""#
        );
    }

    /// A wire value from a build that knows a kind this one does not is refused, rather than read
    /// as some other kind — the same argument `PROTOCOL_VERSION` 36 makes about `TodoStatus`:
    /// `serde` has no catch-all on this enum on purpose.
    #[test]
    fn an_unknown_kind_is_refused_rather_than_guessed() {
        let e = serde_json::from_str::<Activity>(r#"{"busy":{"waiting_on":[{"on":"thinking"}]}}"#);
        assert!(e.is_err(), "{e:?}");
    }
}
