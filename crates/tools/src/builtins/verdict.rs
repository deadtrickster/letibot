//! `review_verdict` — **the door a reviewer hands its verdict in through, and the desk behind
//! it.**
//!
//! # The seam this is
//!
//! The review queue's states do not move into the model, and this is the shape that keeps them
//! out. A gatekeeper child does not own a state: it *reports* one, through a tool it holds
//! ([`ReviewVerdictTool`]), and the daemon writes what it reported onto the entry's review row
//! through the same function it always used (`Harness::write_outcome`, reached by
//! `Harness::serve_reviews`). The queue's states, their storage and the per-workspace split stay
//! exactly where they were — the child has no store handle, no entry, and no way to name a SHA
//! ([`crate::gatekeeper::Submission`] has no field for a branch or a base).
//!
//! **What the parent used to do instead**: read the child's answer, parse a closing block out of
//! it, and write the label it found. That is prose being re-parsed for a label, and the label
//! here decides whether a branch lands. So the answer is now the child's own call, and a reply
//! in prose is not a verdict — see `Harness::review_outcome`, which counts such an attempt
//! rather than inventing one.
//!
//! # The two halves, and why the desk is shared rather than per-session
//!
//! * **[`ReviewDesk`]** — who is reviewing what, and what they have handed in. The review
//!   queue's session OPENS a review on it when it starts a gatekeeper child
//!   (`Harness::serve_reviews`), and the same desk is what the child's tool submits to. It is one
//!   `Arc` for the daemon (it rides `Parts`, like the todo boards do), because the child is a
//!   session of its own with a harness of its own: a per-harness desk would be a different desk
//!   on each side of the one act this file exists for.
//! * **[`ReviewVerdictTool`]** — the tool itself, seated in the `gatekeeper` role and registered
//!   for every session so that seat resolves anywhere. **The caller's own session id is bound at
//!   open and never read from the call**, so a model cannot hand a verdict in under another
//!   session's name — the same rule `todo_write`'s `target` follows, one door over.
//!
//! # Who may submit, and what happens to everyone else
//!
//! [`ReviewDesk::submit`] answers one question: **is this session a reviewer of an entry this
//! daemon is waiting for a verdict on?** It is open only to a child whose review the queue's
//! session opened, so:
//!
//! * **A child of some OTHER session cannot submit for this queue.** It is not in the desk, the
//!   call is refused by name, and nothing is recorded — a verdict that reached a queue from a
//!   session the queue never asked is a judgement nobody commissioned.
//! * **A session that is not reviewing anything** — the daemon's own session, an operator's
//!   session seated `gatekeeper` by hand, a reviewer whose verdict is already in and whose review
//!   is therefore closed — is refused by the same door, with the same sentence.
//!
//! # What the daemon does with what it was handed
//!
//! Nothing here writes a row. The desk holds the submission until the pass that owns the write
//! takes it ([`ReviewDesk::take_verdict`]), and the child keeps working in the meantime: the
//! verdict is written when the pass next looks, which the child's own settlement triggers (a
//! child's end wakes its parent) and the daemon's own clock covers otherwise.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::gatekeeper::{Decision, LookedAt, ReviewRequest, Submission, Verdict};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// **The name of the tool, in one place** — the seat lists it, the schema answers it, and the
/// tests spell it. A tool whose name is written twice is a tool one of the two can rename.
pub const TOOL: &str = "review_verdict";

/// **One review in flight**: the child that is making it, the entry it is about, and the request
/// it was asked with.
///
/// `req` is kept because it is what a `Verdict` is joined to — the branch and the base a person
/// reads on the row, and the sentence a refusal names the branch in. The child never sees it
/// again: what it hands in has no branch in it at all.
struct Open {
    /// **The session that OPENED this review** — the one that started the child, and the only one
    /// that may close it. The desk is one `Arc` for the whole daemon, so without this a pass on
    /// ANY session would prune reviews it never opened: measured, the daemon's own session's pass
    /// ended with `keep(&[])` and took the review queue's review with it, so the next pass started
    /// a second reviewer for one entry.
    host: String,
    /// The reviewer's session id — the handle the queue's session started it under.
    child: String,
    entry_id: String,
    req: ReviewRequest,
    /// **What the reviewer handed in**, once it has. `None` while it is still making up its
    /// mind; `Some` from the moment of the call, and the pass takes it to write the row.
    handed: Option<Submission>,
}

/// **The desk: who is reviewing what, and what they have handed in.**
///
/// One per daemon, held by the harness and shared with every session's tool (see the module
/// doc). Every method is a short critical section over one `Mutex`, because the two sides run on
/// two threads: the review queue's session opens reviews and takes verdicts on its own thread,
/// and each reviewer submits from its own.
#[derive(Default)]
pub struct ReviewDesk {
    inner: Mutex<Vec<Open>>,
}

impl ReviewDesk {
    pub fn new() -> Self {
        Self::default()
    }

    /// **The queue's act: this child is reviewing this entry.** Called once, by the session that
    /// started the child, with the handle it started it under. `host` is that session, and it is
    /// what makes the review the opener's to close ([`ReviewDesk::keep`]).
    pub fn opened(&self, host: &str, child: &str, entry_id: &str, req: ReviewRequest) {
        let mut open = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        open.push(Open {
            host: host.to_string(),
            child: child.to_string(),
            entry_id: entry_id.to_string(),
            req,
            handed: None,
        });
    }

    /// **The reviewer's act: here is my verdict.** The one door a reviewer holds, and the whole
    /// of what this file exists for.
    ///
    /// `Ok` is the verdict rendered back, so the child can see what it just handed in — the same
    /// rendering a person reads, from [`Verdict::render`]. `Err` is a refusal written for a
    /// model, and **nothing is recorded** on any of its arms.
    pub fn submit(&self, from: &str, said: Submission) -> Result<Verdict, String> {
        let mut open = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(row) = open.iter_mut().find(|o| o.child == from) else {
            return Err(format!(
                "nothing was handed in: `{from}` is not reviewing an entry of the merge queue. \
                 `{TOOL}` is the reviewer's own door and it is open only to a gatekeeper child \
                 that the review queue's session started for an entry waiting for a verdict — a \
                 session that was never asked has nothing to hand in, and a review whose verdict \
                 is already in is closed. No verdict was recorded and no row was written."
            ));
        };
        if row.handed.is_some() {
            return Err(format!(
                "nothing was handed in: `{from}` has already handed a verdict in for `{}` (entry \
                 `{}`). A review is judged ONCE — a second verdict is not a second judgement, and \
                 the queue has the first one. No verdict was recorded and no row was written.",
                row.req.branch, row.entry_id
            ));
        }
        let verdict = Verdict::for_request(
            &row.req,
            said.decision,
            said.reasons.clone(),
            said.looked_at.clone(),
        );
        row.handed = Some(said);
        Ok(verdict)
    }

    /// **What the daemon writes next, taken so it is written exactly once.** `None` for a review
    /// nobody has judged yet, and for one whose verdict a previous pass already took.
    ///
    /// The submission stays *taken* rather than being removed with the review: the review is
    /// closed by the pass that wrote the row, and until then a second call is refused with *you
    /// have already handed a verdict in* rather than with *you are not reviewing anything*.
    pub fn take_verdict(&self, child: &str) -> Option<Submission> {
        let mut open = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        open.iter_mut()
            .find(|o| o.child == child)
            .and_then(|o| o.handed.take())
    }

    /// **Every review still in flight that THIS session opened**, oldest first — a snapshot, for
    /// the pass that walks them.
    ///
    /// Scoped to the host for the reason [`Open::host`] exists: the desk is the daemon's, and a
    /// session that hosts nothing must not walk (or close) another session's reviews. It is the
    /// same scope the session's own in-memory list had before the desk was one `Arc` for the
    /// daemon, one level down.
    pub fn open(&self, host: &str) -> Vec<Review> {
        let open = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        open.iter()
            .filter(|o| o.host == host)
            .map(|o| Review {
                child: o.child.clone(),
                entry_id: o.entry_id.clone(),
                req: o.req.clone(),
            })
            .collect()
    }

    /// **Whether a child of ANY session is already on this entry** — the guard that makes *one
    /// review per entry* true, which the store cannot say (a request row and an attempt in flight
    /// are the same row).
    ///
    /// Deliberately NOT scoped to a host: the queue writes one request row per entry, so one
    /// entry has at most one review, whichever session is hosting it.
    pub fn is_open_for(&self, entry_id: &str) -> bool {
        let open = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        open.iter().any(|o| o.entry_id == entry_id)
    }

    /// **Keep exactly these reviews and close the rest — but only THIS session's.** The pass's own
    /// end, spelled the way `Harness::serve_reviews` leaves its list: what it walked and did not
    /// finish stays, and a review whose child is gone, whose entry left the queue, or whose verdict
    /// was written is over.
    ///
    /// `host` is the session that is closing, and a review another session opened is left exactly
    /// as it is — the daemon runs this pass for EVERY session it holds, so an unscoped close would
    /// let the first pass of the daemon's own session end the review queue's review.
    pub fn keep(&self, host: &str, children: &[String]) {
        let mut open = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        open.retain(|o| o.host != host || children.iter().any(|c| c == &o.child));
    }

    /// How many reviews are in flight — for a log line, and for a test that wants the count
    /// rather than the rows.
    pub fn len(&self) -> usize {
        let open = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// **One review as it stands**, for the pass that owns it — see [`ReviewDesk::open`].
pub struct Review {
    /// The reviewer's session id, which is the handle `task_result` collects by.
    pub child: String,
    /// The entry it is reviewing.
    pub entry_id: String,
    /// What it was asked with: the branch and base the row names, and the framing it was given.
    pub req: ReviewRequest,
}

/// **The reviewer's own tool: hand your verdict in.**
///
/// Seated in the `gatekeeper` role and registered for every session (so that seat resolves in a
/// daemon that is not running the review queue, and so a session seated `gatekeeper` by hand gets
/// the honest refusal rather than an absent tool). What makes it usable is not the seat: it is
/// whether the session is a reviewer of an entry this daemon is waiting on, and that is
/// [`ReviewDesk::submit`]'s question and nobody else's.
pub struct ReviewVerdictTool {
    desk: Arc<ReviewDesk>,
    /// **This session's own id, bound at open.** The author of a submission is a fact about the
    /// harness the tool was seated in, never a field of the call — so a model cannot hand a
    /// verdict in as somebody else.
    who: String,
}

impl ReviewVerdictTool {
    pub fn new(desk: Arc<ReviewDesk>, who: impl Into<String>) -> Self {
        ReviewVerdictTool {
            desk,
            who: who.into(),
        }
    }
}

/// One field's worth of a list argument: a list of strings, or one string for the case a model
/// writes a single item without wrapping it. Anything else is refused by name, because a field
/// silently dropped is how a verdict comes to claim evidence it never had.
fn list(args: &Value, field: &str) -> Result<Vec<String>, Invocation> {
    let Some(v) = args.get(field) else {
        return Ok(Vec::new());
    };
    let one = |s: &str| -> Vec<String> {
        let t = s.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("none") {
            // **`none` is not a file called `none`**, on the rule the old closing block kept: it
            // is how a reviewer says *I looked at nothing*, which is recorded as nothing.
            Vec::new()
        } else {
            vec![t.to_string()]
        }
    };
    match v {
        Value::Null => Ok(Vec::new()),
        Value::String(s) => Ok(one(s)),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Some(s) = item.as_str() else {
                    return Err(Invocation::failed(
                        format!("`{field}` is a list of strings, and one item is not a string"),
                        format!(
                            "**nothing was handed in.** Send `{field}` as a list of strings — \
                             each item one reason, one path or one command — or leave it out."
                        ),
                    ));
                };
                out.extend(one(s));
            }
            Ok(out)
        }
        _ => Err(Invocation::failed(
            format!("`{field}` is neither a list of strings nor a string"),
            format!(
                "**nothing was handed in.** Send `{field}` as a list of strings, or as a single \
                 string when there is one — or leave it out."
            ),
        )),
    }
}

impl Tool for ReviewVerdictTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            TOOL,
            "Hand your verdict in: the decision you reached on the branch you were asked to \
             review, with your reasons and what you looked at. This call IS your answer — the \
             queue takes the entry off its board because the verdict reached it, and a reply in \
             prose without this call has judged nothing. Call it once, when you have judged; \
             after it you may keep talking, and the session that started you reads that in your \
             own transcript.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "verdict": {
                        "type": "string",
                        "enum": ["accept", "reject", "needs_human"],
                        "description": "accept: the change does what the brief asked without breaking what was there. reject: it does not, and the reasons say why. needs_human: you cannot decide from what you can see, and a person must."
                    },
                    "reasons": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Why, one reason per item, in your own words. A rejection without reasons is a refusal nobody can act on."
                    },
                    "files": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "The files you actually read, as paths. An empty list is recorded as nothing looked at."
                    },
                    "commands": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "The commands you actually ran, one simple command per item, written the way you ran it."
                    }
                },
                "required": ["verdict"]
            }),
            // **Session state, and not a write.** It changes the review queue's own row and
            // nothing the operator owns: no file, no process, no network. `Access::Session` is
            // the class for exactly this, and it is the reason a reviewer's own verdict does not
            // raise a permission card at the head of the tree it runs in — a verdict nobody can
            // answer unattended is a review that can never finish.
            Access::Session,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        // **An unknown top-level field is refused by name.** The same rule `todo_write` keeps,
        // for the same reason: a field that is silently ignored is how a wrong call becomes a
        // quiet one — here, a verdict that records no evidence because the model spelled the
        // field `reason` or `file`.
        if let Some(obj) = args.as_object() {
            for key in obj.keys() {
                if !matches!(key.as_str(), "verdict" | "reasons" | "files" | "commands") {
                    return Invocation::failed(
                        format!("`{key}` is not an argument of `{TOOL}`"),
                        format!(
                            "**nothing was handed in.** The arguments are `verdict` (one of \
                             {}), `reasons`, `files` and `commands`.",
                            Decision::ALL
                                .iter()
                                .map(|d| d.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    );
                }
            }
        }
        let Some(word) = args.get("verdict").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "this call names no verdict",
                format!(
                    "**nothing was handed in.** `verdict` is the one required argument, and it \
                     is one of: {}.",
                    Decision::ALL
                        .iter()
                        .map(|d| d.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        };
        // **The closed set, checked here and refused by name.** `Decision::parse` is the queue's
        // own reader of the word, so the door and the row cannot disagree about what `accept`
        // means; a word outside the set is never defaulted, because guessing `accept` lands a
        // branch nobody accepted and guessing `reject` parks work nobody rejected.
        let Some(decision) = Decision::parse(word.trim()) else {
            return Invocation::failed(
                format!("`{word}` is not a verdict"),
                format!(
                    "**nothing was handed in.** A verdict is one of: {} — the words are exact.",
                    Decision::ALL
                        .iter()
                        .map(|d| d.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        };
        let reasons = match list(args, "reasons") {
            Ok(v) => v,
            Err(refusal) => return refusal,
        };
        let files = match list(args, "files") {
            Ok(v) => v,
            Err(refusal) => return refusal,
        };
        let commands = match list(args, "commands") {
            Ok(v) => v,
            Err(refusal) => return refusal,
        };
        let said = Submission {
            decision,
            reasons,
            looked_at: LookedAt { files, commands },
        };
        match self.desk.submit(&self.who, said) {
            Ok(verdict) => Invocation::ok(format!(
                "{}\nHanded in. The queue's own pass writes this onto the entry's review row, and \
                 the row comes off the review queue's board when it does — nothing else is needed \
                 from you. Finish your reply with what you found; the session that started you \
                 reads it there.",
                verdict.render()
            )),
            Err(why) => Invocation::failed(
                why,
                format!(
                    "**nothing was handed in**, and no entry's row was touched. `{TOOL}` is the \
                     reviewer's own door: it is open to a gatekeeper child that the review \
                     queue's session started for an entry waiting for a verdict, and to nobody \
                     else."
                ),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::HostBackend;
    use crate::backend::tempdir::TempDir;
    use crate::events::RecordingToolSink;
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::{ToolCall, ToolOutcome};

    fn request() -> ReviewRequest {
        ReviewRequest {
            brief: "make the reader keep the last record".into(),
            branch: "agent/reader".into(),
            base_sha: "abc123".into(),
            head_sha: String::new(),
            cwd: String::new(),
        }
    }

    /// A desk with one review open on `sub-1`, and the tool seated in the session `who`.
    fn desk_with_a_review(who: &str) -> (Arc<ReviewDesk>, ToolRuntime) {
        let desk = Arc::new(ReviewDesk::new());
        desk.opened("s-host", "sub-1", "m-1", request());
        let mut reg = Registry::new();
        reg.register(Box::new(ReviewVerdictTool::new(desk.clone(), who)))
            .expect("the tool registers");
        let dir = TempDir::new();
        let backend = HostBackend::new(dir.path()).expect("a backend");
        // The temp dir outlives the backend only within one test; leak it there rather than
        // complicate every caller — `todo`'s own tests do the same.
        std::mem::forget(dir);
        (desk, ToolRuntime::new(reg, Box::new(backend)))
    }

    fn call(args: &str) -> ToolCall {
        ToolCall {
            id: "c0".into(),
            name: TOOL.into(),
            arguments: args.into(),
        }
    }

    /// **A refusal read as a whole**: the sentence the tool named as the reason, then the body it
    /// handed back. A refusal is two halves by construction — `Invocation::failed(what, advice)` —
    /// and a test that read only one of them would miss half of what the model is told.
    fn refusal(r: &crate::result::ToolResult) -> String {
        match &r.outcome {
            ToolOutcome::Failed { reason } => format!("{reason}\n{}", r.payload),
            other => panic!("not a refusal: {other:?}"),
        }
    }

    /// **The child's verdict is the child's act, and it is recorded as one.** The call is made
    /// by the session the review was opened for, and what comes back is the verdict it just
    /// handed in — the same rendering a person reads — so the reviewer can see the queue has it.
    #[test]
    fn a_reviewers_own_call_hands_its_verdict_in() {
        let (desk, rt) = desk_with_a_review("sub-1");
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &call(
                r#"{"verdict": "reject",
                    "reasons": ["the brief asked for X and the change does Y"],
                    "files": ["src/reader.rs"],
                    "commands": ["git diff abc123...agent/reader"]}"#,
            ),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Ok),
            "the reviewer's own verdict is accepted: {r:?}"
        );
        assert!(r.payload.contains("agent/reader"), "{}", r.payload);
        assert!(r.payload.contains("reject"), "{}", r.payload);
        assert!(
            r.payload.contains("Handed in"),
            "the reply says what happens next: {}",
            r.payload
        );

        // **And it is on the desk, waiting for the daemon's own write** — not written by the
        // tool, and not written by the child either: the queue's pass takes it.
        let taken = desk
            .take_verdict("sub-1")
            .expect("the submission is waiting for the pass");
        assert_eq!(taken.decision, Decision::Reject);
        assert_eq!(
            taken.reasons,
            vec!["the brief asked for X and the change does Y"]
        );
        assert_eq!(taken.looked_at.files, vec!["src/reader.rs"]);
        assert_eq!(
            taken.looked_at.commands,
            vec!["git diff abc123...agent/reader"]
        );
        assert!(
            desk.take_verdict("sub-1").is_none(),
            "and it is taken ONCE, so no pass can write it twice"
        );
    }

    /// **A child of some OTHER session cannot submit for this queue.** The desk has one review
    /// open, for `sub-1`; a session that was never asked — a child of another tree, a session
    /// seated `gatekeeper` by hand, the daemon's own session — is refused by name, and nothing is
    /// recorded. This is the whole of the parentage rule, and it is the desk's because the desk
    /// is the only thing that knows which children the queue asked.
    #[test]
    fn a_session_that_was_never_asked_cannot_submit() {
        for who in ["s-other-child", "s-host", "sub-2"] {
            let (desk, rt) = desk_with_a_review(who);
            let mut sink = RecordingToolSink::new();
            let r = rt.invoke(
                "t1",
                &call(r#"{"verdict": "accept", "reasons": ["looks fine"]}"#),
                &mut sink,
            );
            let ToolOutcome::Failed { .. } = r.outcome else {
                panic!("`{who}` is not reviewing anything and must be refused: {r:?}");
            };
            let said = refusal(&r);
            assert!(
                said.contains("is not reviewing an entry of the merge queue"),
                "the refusal says what is wrong: {said}"
            );
            assert!(
                said.contains("No verdict was recorded"),
                "and that nothing happened: {said}"
            );
            assert!(
                desk.take_verdict("sub-1").is_none(),
                "the review is untouched: a verdict from `{who}` was recorded anyway"
            );
        }
    }

    /// **A review is judged once.** A second call from the same reviewer is refused by name —
    /// with the branch and the entry it is about — and the first verdict is what the daemon
    /// takes. A model that calls twice (a retry, a change of mind) must not be able to hand the
    /// queue a second judgement for one entry.
    #[test]
    fn a_second_verdict_from_one_reviewer_is_refused() {
        let (desk, rt) = desk_with_a_review("sub-1");
        let mut sink = RecordingToolSink::new();
        let first = rt.invoke(
            "t1",
            &call(r#"{"verdict": "accept", "reasons": ["the test covers it"]}"#),
            &mut sink,
        );
        assert!(matches!(first.outcome, ToolOutcome::Ok), "{first:?}");
        let second = rt.invoke(
            "t2",
            &call(r#"{"verdict": "reject", "reasons": ["on reflection"]}"#),
            &mut sink,
        );
        let ToolOutcome::Failed { .. } = second.outcome else {
            panic!("one review is judged once: {second:?}");
        };
        let said = refusal(&second);
        assert!(
            said.contains("has already handed a verdict in")
                && said.contains("agent/reader")
                && said.contains("m-1"),
            "the refusal names the review it is about: {said}"
        );
        assert_eq!(
            desk.take_verdict("sub-1").map(|s| s.decision),
            Some(Decision::Accept),
            "and the FIRST verdict is the one the daemon writes"
        );
    }

    /// **A word outside the closed set is refused, and never defaulted.** Guessing `accept` would
    /// land a branch nobody accepted; guessing `reject` would park work nobody rejected. The
    /// refusal names every word there is, and an unknown field — the way a model writes `reason`
    /// or `file` — is refused too, because a field silently ignored is a verdict that records no
    /// evidence.
    #[test]
    fn a_verdict_outside_the_closed_set_or_with_a_stray_field_is_refused() {
        let (desk, rt) = desk_with_a_review("sub-1");
        let mut sink = RecordingToolSink::new();
        for (args, want) in [
            (
                r#"{"verdict": "probably", "reasons": ["hmm"]}"#,
                "is not a verdict",
            ),
            (r#"{}"#, "names no verdict"),
            (
                r#"{"verdict": "accept", "reason": "looks fine"}"#,
                "is not an argument",
            ),
            (
                r#"{"verdict": "accept", "files": "src/x.rs", "reasons": 7}"#,
                "one item is not a string",
            ),
        ] {
            let r = rt.invoke("t1", &call(args), &mut sink);
            let ToolOutcome::Failed { .. } = r.outcome else {
                panic!("`{args}` must be refused: {r:?}");
            };
            let said = refusal(&r);
            assert!(
                said.contains(want),
                "want {want:?} in the refusal for `{args}`: {said}"
            );
            assert!(
                said.contains("nothing was handed in"),
                "and the refusal says nothing happened: {said}"
            );
        }
        assert!(desk.is_empty() || desk.take_verdict("sub-1").is_none());
        assert_eq!(desk.len(), 1, "no refused call closed or added a review");
    }

    /// **A single string where a list belongs is read as one item**, and `none` is recorded as
    /// nothing looked at rather than as a file called `none` — the rule the old closing block
    /// kept, kept here because the words mean the same thing to a reviewer.
    #[test]
    fn a_lone_string_is_one_item_and_none_is_nothing() {
        let (desk, rt) = desk_with_a_review("sub-1");
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &call(
                r#"{"verdict": "needs_human", "reasons": "the spec is ambiguous about the retry",
                    "files": "none", "commands": []}"#,
            ),
            &mut sink,
        );
        assert!(matches!(r.outcome, ToolOutcome::Ok), "{r:?}");
        let taken = desk.take_verdict("sub-1").expect("handed in");
        assert_eq!(taken.decision, Decision::NeedsHuman);
        assert_eq!(taken.reasons, vec!["the spec is ambiguous about the retry"]);
        assert!(taken.looked_at.files.is_empty(), "`none` is not a file");
        assert!(taken.looked_at.commands.is_empty());
    }

    /// **The tool is session state and never a card.** A verdict that needed a person to approve
    /// it would be a review that can never finish unattended, and the child that hands one in is
    /// a child with no head of its own.
    #[test]
    fn the_verdict_is_session_state_and_raises_no_card() {
        let tool = ReviewVerdictTool::new(Arc::new(ReviewDesk::new()), "sub-1");
        let access = tool.schema().access;
        assert_eq!(access, Access::Session);
        assert!(access.is_unattended(), "nobody is asked about a verdict");
    }

    /// **The desk is the one thing both sides hold**, so the daemon's half is measured on the
    /// same object the child's half writes to: open a review, hand one in, take it — and a
    /// review closed by the pass is closed for the reviewer too.
    ///
    /// **And a pass closes only its OWN reviews**, which is the half that had to be measured to
    /// be believed: the daemon runs the pass for every session it holds, so a close that was not
    /// scoped to the opener would let the daemon's own session end the review queue's review —
    /// and the next pass would start a second reviewer for one entry.
    #[test]
    fn the_desk_is_what_both_halves_share_and_only_the_opener_closes() {
        let desk = ReviewDesk::new();
        assert!(desk.is_empty());
        desk.opened("s-gatekeeper", "sub-1", "m-1", request());
        desk.opened("s-gatekeeper", "sub-2", "m-2", request());
        desk.opened("s-other", "sub-3", "m-3", request());
        assert_eq!(desk.len(), 3);
        assert!(desk.is_open_for("m-1") && desk.is_open_for("m-2"));
        assert!(!desk.is_open_for("m-4"));

        // **The pass sees its own and nobody else's.**
        let open = desk.open("s-gatekeeper");
        assert_eq!(open.len(), 2, "oldest first, and only this host's");
        assert_eq!(open[0].child, "sub-1");
        assert_eq!(open[0].entry_id, "m-1");
        assert_eq!(open[0].req.branch, "agent/reader");
        assert_eq!(desk.open("s-nobody").len(), 0);

        // **And a pass that hosts nothing closes nothing.** The daemon's own session is this
        // shape exactly, and an unscoped `keep` here is the defect that was measured.
        desk.keep("s-other", &[]);
        assert_eq!(
            desk.len(),
            2,
            "`s-other` closed the one it opened and left the queue's alone"
        );
        assert!(
            !desk.is_open_for("m-3"),
            "the review `s-other` opened is the one it closed"
        );
        assert!(
            desk.is_open_for("m-1") && desk.is_open_for("m-2"),
            "and the two it never opened are untouched"
        );

        // The host keeps one and closes the other: the closed one takes no verdict any more.
        desk.keep("s-gatekeeper", &["sub-2".to_string()]);
        assert_eq!(desk.len(), 1);
        let refused = desk
            .submit(
                "sub-1",
                Submission {
                    decision: Decision::Accept,
                    reasons: Vec::new(),
                    looked_at: LookedAt::default(),
                },
            )
            .unwrap_err();
        assert!(refused.contains("is not reviewing an entry"), "{refused}");
        assert!(
            desk.submit(
                "sub-2",
                Submission {
                    decision: Decision::Accept,
                    reasons: Vec::new(),
                    looked_at: LookedAt::default(),
                }
            )
            .is_ok(),
            "the review that is still open still takes a verdict"
        );
    }
}
