//! What a session is waiting on: the daemon's answer to "will this act again by itself?"
//! [`Activity`] is computed by `letibot_harnessd` and published as
//! [`crate::SessionEvent::ActivityChanged`] on change, since per-round turn events cannot say.

use serde::{Deserialize, Serialize};

/// The kind and the id of one thing a busy session waits on - see the module header for which
/// of them mean *this session acts again on its own* and which one means *a person must*.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "on", content = "id", rename_all = "snake_case")]
pub enum Waiting {
    /// A prompt is in flight - the whole turn, however many rounds it takes, from
    /// the daemon taking it to its loop returning. Read from the daemon's per-prompt
    /// span, never from the round-level `TurnStarted`/`TurnFinished` pair.
    Turn,
    /// The round's generation ended and the calls it proposed are running; the next round reads
    /// their results. The finer detail inside [`Waiting::Turn`] - a head draws it, and nothing about
    /// *will this session act again* depends on it.
    Calls,
    /// The model is owed something and the turn that says it has not started yet - a
    /// settlement queued or held, a monitor that has fired, a message or a prompt waiting for the
    /// worker. This is the window every timer in `crates/bridge` was invented for.
    Delivery,
    /// A background job of this session's, running: its settlement is said to the model, and the
    /// turn that says it is the session acting again. Absent for the one the model parked with
    /// `wake: "later"`, whose ending it said it does not need waking for.
    Job(String),
    /// A monitor this session armed. A firing is immediate, so this always ends in a turn.
    Monitor(String),
    /// A subagent of this session's. It settles through the same queue a background job does.
    Child(String),
    /// A card is open and nobody has answered it. The one entry that does not end by itself.
    Ask(String),
}

impl Waiting {
    /// The word the wire and a head's line use for the kind - `turn`, `calls`, `delivery`, `job`,
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

    /// The id this entry names, empty for the three that name no thing.
    pub fn id(&self) -> &str {
        match self {
            Waiting::Turn | Waiting::Calls | Waiting::Delivery => "",
            Waiting::Job(id) | Waiting::Monitor(id) | Waiting::Child(id) | Waiting::Ask(id) => id,
        }
    }

    /// `job j8032`, `monitor ci`, `ask adj-s1-0001` - how a line that shows the list says one
    /// entry, and how `leticode_status` names it as data. One function rather than one per reader,
    /// because two spellings of the same fact is the drift this file exists to stop.
    pub fn label(&self) -> String {
        match self.id() {
            "" => self.kind().to_string(),
            id => format!("{} {id}", self.kind()),
        }
    }

    /// Does this session move only when a person moves it? True for [`Waiting::Ask`] and
    /// nothing else - the whole of why `Activity` is not a boolean.
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

/// What a session is waiting on, as the daemon computed it - [`Activity::Quiet`] or
/// the list. `Quiet` is *nothing running and nothing owed*, not *idle*, which
/// [`crate::hub::SessionStatus::running`] already means.
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
    /// A list, in the order [`Waiting::rank`] fixes, with nothing in it read as `Quiet` - so the
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

    /// The card this session is stopped on, when it is - the one entry a caller has to do
    /// something about. A session can in principle hold more than one; the oldest is the answer,
    /// because it is the one that has waited longest.
    pub fn ask(&self) -> Option<&str> {
        self.waiting_on().iter().find_map(|w| match w {
            Waiting::Ask(id) => Some(id.as_str()),
            _ => None,
        })
    }

    /// Whether the session will move without a person - true when the list holds anything but
    /// a card. A `Busy` made only of an ask is the *stopped* state, which is why this is not
    /// `!is_quiet()`.
    pub fn moves_by_itself(&self) -> bool {
        self.waiting_on().iter().any(|w| !w.needs_a_person())
    }

    /// `waiting on: job j8032 · monitor ci`, or empty for a quiet session - the one muted line
    /// a head draws (the TUI's status row) and a caller reads in `leticode_status`.
    pub fn line(&self) -> String {
        let labels: Vec<String> = self.waiting_on().iter().map(Waiting::label).collect();
        if labels.is_empty() {
            return String::new();
        }
        format!("waiting on: {}", labels.join(" · "))
    }

    /// The words a head shows on the status row's right, or empty: [`Activity::line`]
    /// without the `waiting on:` prefix, for callers that already say it.
    pub fn waiting_labels(&self) -> Vec<String> {
        self.waiting_on().iter().map(Waiting::label).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order is the line's order and the wire's order, and it is fixed here so that two
    /// computations of the same facts are the same value - which is what *published only when it
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

    /// An ask is the only entry that waits for a person, and a session stopped on one moves by
    /// itself for nothing - which is the difference `wait`'s exit code 10 is about.
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
    /// as some other kind - the same argument `PROTOCOL_VERSION` 36 makes about `TodoStatus`:
    /// `serde` has no catch-all on this enum on purpose.
    #[test]
    fn an_unknown_kind_is_refused_rather_than_guessed() {
        let e = serde_json::from_str::<Activity>(r#"{"busy":{"waiting_on":[{"on":"thinking"}]}}"#);
        assert!(e.is_err(), "{e:?}");
    }
}
