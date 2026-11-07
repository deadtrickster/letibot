//! The model's own verbs on its own session: `reseat_self`, `compact_self`, `nag_pause_self`.
//!
//! X24 of `docs/plugin-extension-points-wide.md`: three verbs, native, through the gate, and each one
//! session deep - the calling session and no other. The operator's own verbs (`/queue approve|veto`,
//! and the head's `/reseat`) stay theirs, and a model reaching for one is told what it has instead.

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

pub const RESEAT: &str = letibot_plugin::command::RESEAT_SELF;
pub const COMPACT: &str = letibot_plugin::command::COMPACT_SELF;
pub const NAG_PAUSE: &str = letibot_plugin::command::NAG_PAUSE_SELF;

/// What the model asked `/nag pause` for, in the operator's own vocabulary: until resumed, until a
/// duration the daemon's parser owns, or resumed now. A duration is passed through as typed, because
/// `90s`/`1h30m` is the daemon's grammar and a second copy here would drift from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NagAsk {
    Pause(Option<String>),
    Resume,
}

/// What a self verb needs from the daemon: the session it acts on, and the three acts themselves.
/// A trait rather than a handle because the tool must be seatable in a registry with no daemon behind
/// it - and then every verb says so by name rather than the tool being absent.
pub trait SessionSelf: Send + Sync {
    /// The id of the session these verbs act on. The only one they may reach.
    fn session(&self) -> String;

    /// Re-seat this session's conversation onto the prompt its seat holds now, carrying it across.
    fn reseat(&self, summarise: bool) -> Result<String, String>;

    /// Summarise this session and fork it onto the summary.
    fn compact(&self) -> Result<String, String>;

    /// Pause, or resume, this session's idle plan-check.
    fn nag_pause(&self, ask: NagAsk) -> Result<String, String>;
}

/// No session behind the tool - a registry a test built, or a daemon that opened none. Every verb says
/// so, which is what a seat naming one of these expects.
pub struct NoSession;

impl NoSession {
    const WHY: &str = "no session is behind this registry, so there is nothing to re-seat: these \
                       verbs act on the session that calls them, and this one has none";
}

impl SessionSelf for NoSession {
    fn session(&self) -> String {
        String::new()
    }

    fn reseat(&self, _summarise: bool) -> Result<String, String> {
        Err(Self::WHY.into())
    }

    fn compact(&self) -> Result<String, String> {
        Err(Self::WHY.into())
    }

    fn nag_pause(&self, _ask: NagAsk) -> Result<String, String> {
        Err(Self::WHY.into())
    }
}

/// The one refusal every self verb shares: a session named that is not the caller's. X24's whole
/// point is that a model's verb reaches its OWN session, so naming another is refused by name rather
/// than silently narrowed - a model that asked for another session's prompt to move would otherwise
/// believe it happened.
fn only_own(desk: &dyn SessionSelf, name: &str, args: &Value) -> Result<(), String> {
    let Some(named) = args.get("session").and_then(|s| s.as_str()) else {
        return Ok(());
    };
    let named = named.trim();
    if named.is_empty() || named == desk.session() {
        return Ok(());
    }
    Err(format!(
        "`{name}` acts on the session that calls it — `{}` — and cannot reach `{named}`. A \
         session's own verbs are its own; another session's prompt is moved from its own seat, or \
         by the person whose head is attached to it",
        desk.session()
    ))
}

fn schema(name: &'static str, description: &'static str, extra: Value) -> ToolSchema {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "session".into(),
        serde_json::json!({
            "type": "string",
            "description": "This session, and nothing else: naming another is refused.",
        }),
    );
    if let Value::Object(more) = extra {
        properties.extend(more);
    }
    ToolSchema::new(
        name,
        description,
        serde_json::json!({"type": "object", "properties": properties, "required": []}),
        Access::Session,
    )
}

/// `reseat_self`: rebuild this conversation's prompt from the tools seated now. What the operator's
/// `/reseat` does, asked by the session itself - the answer to *"there was a 'prompt was stale'
/// reseat offer, you should catch that and reseat yourself"*.
pub struct ReseatSelf {
    desk: std::sync::Arc<dyn SessionSelf>,
}

impl ReseatSelf {
    pub fn new(desk: std::sync::Arc<dyn SessionSelf>) -> ReseatSelf {
        ReseatSelf { desk }
    }
}

impl Tool for ReseatSelf {
    fn schema(&self) -> ToolSchema {
        schema(
            RESEAT,
            "Re-seat this conversation onto the prompt this session's seat holds now, carrying the \
             history across as it is: the tool list rebuilt, nothing dropped. Use it when you are \
             told the prompt you were seated with is not the one the daemon seats. The next turn \
             re-sends the whole conversation once.",
            serde_json::json!({
                "summarise": {
                    "type": "boolean",
                    "description": "Summarise the history instead of carrying it: cheaper, lossy.",
                }
            }),
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        self.answer(args)
    }
}

impl ReseatSelf {
    /// The verb's body, without a context: a self verb reads nothing but its arguments and the
    /// session behind the desk, so this is the whole of it and a test can ask it directly.
    pub fn answer(&self, args: &Value) -> Invocation {
        if let Err(why) = only_own(self.desk.as_ref(), RESEAT, args) {
            return Invocation::failed(why.clone(), why);
        }
        let summarise = args
            .get("summarise")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);
        match self.desk.reseat(summarise) {
            Ok(said) => Invocation::ok(said),
            Err(why) => Invocation::failed(why.clone(), why),
        }
    }
}

/// `compact_self`: this session's own compaction, on the same door the wall controller and `/compact`
/// use. A child that overran its window says so itself instead of waiting for a parent to notice.
pub struct CompactSelf {
    desk: std::sync::Arc<dyn SessionSelf>,
}

impl CompactSelf {
    pub fn new(desk: std::sync::Arc<dyn SessionSelf>) -> CompactSelf {
        CompactSelf { desk }
    }
}

impl Tool for CompactSelf {
    fn schema(&self) -> ToolSchema {
        schema(
            COMPACT,
            "Compact this conversation: one summary turn, then the history is replaced by that \
             summary. What `/compact` does, asked by the session itself - reach for it before the \
             window does.",
            serde_json::json!({}),
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        self.answer(args)
    }
}

impl CompactSelf {
    /// The verb's body, without a context - see [`ReseatSelf::answer`].
    pub fn answer(&self, args: &Value) -> Invocation {
        if let Err(why) = only_own(self.desk.as_ref(), COMPACT, args) {
            return Invocation::failed(why.clone(), why);
        }
        match self.desk.compact() {
            Ok(said) => Invocation::ok(said),
            Err(why) => Invocation::failed(why.clone(), why),
        }
    }
}

/// `nag_pause_self`: hold this session's idle plan-check for a while, or resume it. The operator's
/// `/nag pause` and `/nag resume`, asked by the session - the model that is being nagged between its
/// own steps.
pub struct NagPauseSelf {
    desk: std::sync::Arc<dyn SessionSelf>,
}

impl NagPauseSelf {
    pub fn new(desk: std::sync::Arc<dyn SessionSelf>) -> NagPauseSelf {
        NagPauseSelf { desk }
    }
}

impl Tool for NagPauseSelf {
    fn schema(&self) -> ToolSchema {
        schema(
            NAG_PAUSE,
            "Pause or resume this session's idle plan-check - the nag that asks a session with an \
             open row to carry on. `/nag pause` holds it until `resume`; `/nag pause 30m` holds it \
             for a duration in the same spelling (`90s`, `30m`, `2h`, `1d`, `1h30m`).",
            serde_json::json!({
                "pause": {
                    "type": "boolean",
                    "description": "true pauses, false resumes. Absent reports nothing and is refused.",
                },
                "for": {
                    "type": "string",
                    "description": "How long, as `30m` or `2h`; without it the pause holds until \
                                    `pause: false`.",
                }
            }),
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        self.answer(args)
    }
}

impl NagPauseSelf {
    /// The verb's body, without a context - see [`ReseatSelf::answer`].
    pub fn answer(&self, args: &Value) -> Invocation {
        if let Err(why) = only_own(self.desk.as_ref(), NAG_PAUSE, args) {
            return Invocation::failed(why.clone(), why);
        }
        let Some(pause) = args.get("pause").and_then(|p| p.as_bool()) else {
            return Invocation::failed(
                "nag_pause_self needs `pause`: true to hold the plan-check, false to resume it",
                "Nothing was asked for: say `pause: true` (and `for: \"30m\"` if the hold has an end) \
                 or `pause: false` to resume."
                    .to_string(),
            );
        };
        let ask = if pause {
            NagAsk::Pause(
                args.get("for")
                    .and_then(|f| f.as_str())
                    .map(str::trim)
                    .filter(|f| !f.is_empty())
                    .map(str::to_string),
            )
        } else {
            NagAsk::Resume
        };
        match self.desk.nag_pause(ask) {
            Ok(said) => Invocation::ok(said),
            Err(why) => Invocation::failed(why.clone(), why),
        }
    }
}

/// The operator's verb a model may reach for, and the self verb that stands where it would - or why
/// there is none. The runtime reads this for an unknown tool, so a model that typed `/reseat`'s
/// spelling as a tool gets the door it has instead of a list of its tools (X24).
pub fn operator_verb_note(name: &str) -> Option<String> {
    let name = name.trim().trim_start_matches('/');
    let verb = verb_of(name)?;
    if !letibot_plugin::command::is_operator_verb(verb) {
        return None;
    }
    Some(match letibot_plugin::command::self_verb_for(verb) {
        Some(self_) => format!(
            "`{name}` is the operator's own verb, and a person's verb is not a model's to cause. \
             The session's own door is the tool `{self_}`"
        ),
        None => format!(
            "`{name}` is the operator's own verb, and there is no self-scoped form of it: a \
             session that could decide its own queue's verdicts would be the reviewer of every \
             branch it wrote. Ask the person who is attached to this session"
        ),
    })
}

/// The operator verb a tool call names, when it names one: `reseat`, `queue_approve`, `queue/reset`
/// and the `/`-prefixed spellings all land on a word in [`letibot_plugin::command::OPERATOR_VERBS`].
fn verb_of(name: &str) -> Option<&str> {
    let folded = name.replace(['-', '/', '.'], "_");
    let verb = folded.as_str();
    if letibot_plugin::command::OPERATOR_VERBS
        .iter()
        .any(|(v, _)| *v == verb)
    {
        // Leaked only through the borrow: `OPERATOR_VERBS` is `'static` and `folded` is not, so the
        // match is looked up again here rather than returned from the folded string.
        return letibot_plugin::command::OPERATOR_VERBS
            .iter()
            .find(|(v, _)| *v == verb)
            .map(|(v, _)| *v);
    }
    // `queue_reset` and `queue_approve`: the merge queue's own sub-verbs, which X24 names as the
    // operator's (`/queue approve|veto|reset`).
    let sub = verb.strip_prefix("queue_")?;
    ["queue_approve", "queue_veto", "queue_reset", "queue_clean"]
        .into_iter()
        .find(|v| v.strip_prefix("queue_") == Some(sub))
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_transcript::ToolOutcome;
    use std::sync::{Arc, Mutex};

    /// A desk that records what was asked and answers as the daemon would.
    #[derive(Default)]
    struct Recorded(Mutex<Vec<String>>);

    struct Scripted {
        session: String,
        seen: Arc<Recorded>,
    }

    impl SessionSelf for Scripted {
        fn session(&self) -> String {
            self.session.clone()
        }
        fn reseat(&self, summarise: bool) -> Result<String, String> {
            self.seen
                .0
                .lock()
                .unwrap()
                .push(format!("reseat summarise={summarise}"));
            Ok("this conversation now speaks the prompt this daemon seats".into())
        }
        fn compact(&self) -> Result<String, String> {
            self.seen.0.lock().unwrap().push("compact".into());
            Ok("compacted".into())
        }
        fn nag_pause(&self, ask: NagAsk) -> Result<String, String> {
            self.seen.0.lock().unwrap().push(format!("nag {ask:?}"));
            Ok("the plan-check is paused".into())
        }
    }

    fn desk() -> (Arc<Recorded>, Arc<dyn SessionSelf>) {
        let seen = Arc::new(Recorded::default());
        let desk: Arc<dyn SessionSelf> = Arc::new(Scripted {
            session: "s-mine".into(),
            seen: seen.clone(),
        });
        (seen, desk)
    }

    /// The three verbs are seated under the names the doc gives them, and each one says when to
    /// reach for it - the sentence a model reads in its tool list.
    #[test]
    fn the_three_verbs_are_the_names_the_doc_gives_and_say_when_to_reach_for_them() {
        let (_, desk) = desk();
        assert_eq!(ReseatSelf::new(desk.clone()).schema().name, "reseat_self");
        assert_eq!(CompactSelf::new(desk.clone()).schema().name, "compact_self");
        assert_eq!(NagPauseSelf::new(desk).schema().name, "nag_pause_self");
        let said = ReseatSelf::new(Arc::new(NoSession))
            .schema()
            .description
            .to_ascii_lowercase();
        assert!(said.contains("re-seat"), "{said}");
        let nag = NagPauseSelf::new(Arc::new(NoSession)).schema().description;
        assert!(
            nag.contains("90s"),
            "the duration grammar is on the schema: {nag}"
        );
        // The access class is the session's own, which no downgrade can take away: a read-only
        // child still says when its own prompt moved.
        for access in [
            ReseatSelf::new(Arc::new(NoSession)).schema().access,
            CompactSelf::new(Arc::new(NoSession)).schema().access,
            NagPauseSelf::new(Arc::new(NoSession)).schema().access,
        ] {
            assert_eq!(access, Access::Session);
        }
    }

    /// A self verb aimed at another session is refused BY NAME, and nothing reaches the desk: the
    /// scope is the whole point of the verb, so a wrong session is a refusal and not a silent
    /// narrowing to the one it may reach.
    #[test]
    fn a_self_verb_aimed_at_another_session_is_refused_by_name() {
        let (seen, desk) = desk();
        let other = serde_json::json!({"session": "s-somebody-else"});
        for (name, invocation) in [
            ("reseat_self", ReseatSelf::new(desk.clone()).answer(&other)),
            (
                "compact_self",
                CompactSelf::new(desk.clone()).answer(&other),
            ),
            (
                "nag_pause_self",
                NagPauseSelf::new(desk.clone()).answer(&serde_json::json!({
                    "session": "s-somebody-else", "pause": true
                })),
            ),
        ] {
            match invocation.outcome {
                ToolOutcome::Failed { reason } => {
                    assert!(reason.contains(name), "the verb is named: {reason}");
                    assert!(
                        reason.contains("s-somebody-else"),
                        "and the target: {reason}"
                    );
                    assert!(reason.contains("s-mine"), "and the caller: {reason}");
                }
                other => panic!("`{name}` reached a session it does not own: {other:?}"),
            }
        }
        assert!(
            seen.0.lock().unwrap().is_empty(),
            "nothing was asked of the desk: {:?}",
            seen.0.lock().unwrap()
        );

        // Naming ITSELF - the one session a self verb may reach - is allowed: an argument a model
        // fills in from what it was told should not be a refusal.
        let mine = serde_json::json!({"session": "s-mine"});
        assert_eq!(
            ReseatSelf::new(desk.clone()).answer(&mine).outcome,
            ToolOutcome::Ok
        );
        assert_eq!(seen.0.lock().unwrap().len(), 1);
    }

    /// An operator verb a model reaches for is refused, and the refusal names the self verb that does
    /// exist - the second half of X24.
    #[test]
    fn an_operator_verb_from_a_model_is_refused_and_names_the_self_verb() {
        for (reached, exists) in [
            ("reseat", "reseat_self"),
            ("/reseat", "reseat_self"),
            ("compact", "compact_self"),
            ("nag", "nag_pause_self"),
            ("nag_pause", "nag_pause_self"),
        ] {
            let note = operator_verb_note(reached).expect(reached);
            assert!(note.contains(exists), "`{reached}`: {note}");
            assert!(note.contains("operator"), "`{reached}`: {note}");
        }
        // The queue's verdicts have no self form, and the sentence says why rather than pointing at
        // a verb that does not exist.
        for reached in ["queue_approve", "queue/veto", "queue_reset", "queue_clean"] {
            let note = operator_verb_note(reached).expect(reached);
            assert!(note.contains("no self-scoped form"), "`{reached}`: {note}");
        }
        // A tool name that is nobody's operator verb is not this table's business.
        for name in ["reseat_self", "read", "todo_write", "queue_status"] {
            assert_eq!(operator_verb_note(name), None, "`{name}`");
        }
    }

    /// The nag verb's own argument: `pause` is required, and the duration is passed through as typed
    /// for the daemon's parser rather than re-read here.
    #[test]
    fn the_nag_verb_passes_the_duration_through_and_refuses_an_empty_ask() {
        let (seen, desk) = desk();
        let tool = NagPauseSelf::new(desk);
        assert!(matches!(
            tool.answer(&serde_json::json!({})).outcome,
            ToolOutcome::Failed { .. }
        ));
        assert!(seen.0.lock().unwrap().is_empty());
        let paused = tool.answer(&serde_json::json!({"pause": true, "for": "2h"}));
        assert_eq!(paused.outcome, ToolOutcome::Ok);
        let resumed = tool.answer(&serde_json::json!({"pause": false}));
        assert_eq!(resumed.outcome, ToolOutcome::Ok);
        assert_eq!(
            seen.0.lock().unwrap().clone(),
            vec![
                "nag Pause(Some(\"2h\"))".to_string(),
                "nag Resume".to_string()
            ]
        );
    }

    /// A registry with no daemon behind it says so by name, rather than the verb being absent from a
    /// seat that names it.
    #[test]
    fn with_no_session_behind_it_every_verb_says_so() {
        for invocation in [
            ReseatSelf::new(Arc::new(NoSession)).answer(&serde_json::json!({})),
            CompactSelf::new(Arc::new(NoSession)).answer(&serde_json::json!({})),
            NagPauseSelf::new(Arc::new(NoSession)).answer(&serde_json::json!({"pause": true})),
        ] {
            match invocation.outcome {
                ToolOutcome::Failed { reason } => {
                    assert!(reason.contains("nothing to re-seat"), "{reason}")
                }
                other => panic!("{other:?}"),
            }
        }
    }

    /// A model that reaches for an operator's verb is refused THROUGH THE RUNTIME, not only by this
    /// module's table: an unknown tool's reason names the self verb that exists, and an ordinary
    /// misspelling is still the miss it always was.
    #[test]
    fn the_runtime_refuses_an_operator_verb_and_names_the_self_verb() {
        let mut h = crate::testing::harness();
        for (called, exists) in [
            ("reseat", "reseat_self"),
            ("compact", "compact_self"),
            ("nag_pause", "nag_pause_self"),
        ] {
            let r = h.call(called, "{}");
            match &r.outcome {
                ToolOutcome::Failed { reason } => {
                    assert!(
                        reason.contains(exists) && reason.contains("operator's own verb"),
                        "`{called}`: {reason}"
                    );
                }
                other => panic!("`{called}` ran: {other:?}"),
            }
            // The body says it too, because a model reads the payload and an outcome is a word.
            assert!(r.payload.contains(exists), "{called}: {}", r.payload);
        }
        // A queue verdict has no self form, and the refusal says so rather than pointing at a verb
        // that does not exist.
        let queue = h.call("queue_approve", "{}");
        match &queue.outcome {
            ToolOutcome::Failed { reason } => {
                assert!(reason.contains("no self-scoped form"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
        // And a plain misspelling is still *this session has these tools*, with the nearest named.
        let miss = h.call("reed", "{}");
        match &miss.outcome {
            ToolOutcome::Failed { reason } => {
                assert!(reason.contains("no tool called `reed`"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
        assert!(miss.payload.contains("the nearest"), "{}", miss.payload);
    }
}
