//! `todo` — one set of verbs over a mount point.
//!
//! The model has **one** todo list. What backs it is a property of the session:
//! with a board mounted a row is filed on the fabric and every seat in the project
//! can see it; with none mounted the same verb records the same intent in this
//! session's own ledger, which needs no node, no token and no network. See
//! [`super::queue`] for the mount argument in full.
//!
//! # This is not a note the model writes to itself
//!
//! Four of the five surveyed harnesses ship a todo tool and all four treat it the
//! same way — the model writes a list, the list is re-emitted, and nothing ever
//! reads it back against what ran. opencode's `todowrite` literally returns *"the
//! list re-emitted as JSON"* (`todo.ts:38`).
//!
//! Ours is the **error signal** of `docs/closed-loop.md` §2, and the whole reason
//! the group is worth building. T21.3:
//!
//! > when model says ill start that and by end of the turn forgets and does not
//! > start anything
//!
//! So there are two rules here that none of the five have:
//!
//! 1. **A completion is checked, not accepted.** [`super::ledger::IntentLedger::complete`]
//!    asks the effect log whether any tool call succeeded while the item was in
//!    progress. If none did, the item is recorded as *claimed*, it is **not**
//!    counted as complete, and the same call says so. F5: a component's "I did not
//!    do this" is never reported upward as success — and an announced item is not a
//!    done one.
//! 2. **A count never travels without its denominator.** Every result ends with
//!    `3 of 7 complete — 2 in progress, 1 pending, 1 blocked`, including the
//!    zeroes.
//!
//! # Ownership is a query
//!
//! `claim` takes an id and nothing else. There is deliberately no `expect`
//! argument: `flowy todo claim --expect WHO` is a compare-and-swap, and the model
//! forming an opinion about who holds a row is the bug (`docs/closed-loop.md` §7).
//! The seam reads the holder and swaps against what it read.

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

use super::ledger::{Intent, IntentLedger, Source, Status, Verification};
use super::queue::{NewRow, NoMount, Queue, QueueError};

pub struct Todo {
    pub ledger: std::sync::Arc<IntentLedger>,
    pub queue: std::sync::Arc<dyn Queue>,
    /// Whether a board is mounted. Fixed at construction, because
    /// [`ToolSchema::access`] must be knowable before the arguments are.
    pub mounted: bool,
}

impl Todo {
    /// A session with no board. **The default, and not an error state.**
    pub fn local(ledger: std::sync::Arc<IntentLedger>) -> Self {
        Todo {
            ledger,
            queue: std::sync::Arc::new(NoMount),
            mounted: false,
        }
    }

    /// A session with the shared board mounted.
    ///
    /// This changes the tool's declared access from [`Access::Session`] to
    /// [`Access::Network`], because with a board mounted the widest thing `todo`
    /// can do is write to state other seats act on — and clause 4 says a tool
    /// declares the widest thing it can do. The consequence is deliberate and
    /// worth saying out loud: **a mounted `todo` is a gated tool**, so a session
    /// that mounts a board and attaches no adjudicator will find `todo` refusing
    /// with `not_run`, exactly as `write` does.
    pub fn mounted(ledger: std::sync::Arc<IntentLedger>, queue: std::sync::Arc<dyn Queue>) -> Self {
        Todo {
            ledger,
            queue,
            mounted: true,
        }
    }

    /// The line every result ends with. A mount that silently changed semantics
    /// would be the silent-degradation failure with better manners, so the backing
    /// is a **receipt on every call** rather than a rule in a description.
    fn backing(&self) -> String {
        if self.mounted {
            format!(
                "\nbacking: the shared board ({}), as {} — every seat in the project sees \
                 these rows",
                self.queue.describe(),
                if self.queue.identity().is_empty() {
                    "an unnamed seat".to_string()
                } else {
                    self.queue.identity()
                }
            )
        } else {
            "\nbacking: this session only — no board is mounted, so nothing here is \
             visible to another seat and none of it outlives the session"
                .to_string()
        }
    }

    fn board_summary(&self) -> String {
        format!("\n{}", self.ledger.board())
    }

    fn refuse_unmounted(&self, op: &str) -> Invocation {
        let e = QueueError::NotMounted(format!(
            "`{op}` is a fact about a SHARED board, and no board is mounted on this \
             session — so it did not run, and nothing was filed, claimed or closed \
             anywhere. No other seat saw anything from this call. That is a \
             configuration and not a fault. What works without a board: `add`, `start`, \
             `note`, `complete`, `block`, `list`."
        ));
        Invocation {
            outcome: e.outcome(),
            payload: format!("{e}{}{}", self.board_summary(), self.backing()),
            notes: vec![],
            edit: None,
            needs_in_view: Vec::new(),
        }
    }

    fn queue_failed(&self, e: QueueError) -> Invocation {
        Invocation {
            outcome: e.outcome(),
            payload: format!("{e}{}{}", self.board_summary(), self.backing()),
            notes: vec![],
            edit: None,
            needs_in_view: Vec::new(),
        }
    }

    fn no_such_item(&self, arg: &str) -> Invocation {
        let known = self.ledger.known();
        let body = if known.is_empty() {
            format!(
                "there is no item `{arg}`, and this session's list is empty — nothing has \
                 been added yet. Call `todo` with `op: \"add\"` and `text` first."
            )
        } else {
            format!(
                "there is no item `{arg}`; this list has {}: {}",
                known.len(),
                known.join(", ")
            )
        };
        Invocation::failed(
            format!("no item `{arg}` on this list"),
            format!("{body}{}{}", self.board_summary(), self.backing()),
        )
    }

    fn render(&self, i: &Intent) -> String {
        let mut s = format!("#{} {} — {}", i.id, i.text, i.status.word());
        if let Some(r) = &i.row {
            s.push_str(&format!(" [row {r}]"));
        }
        if let Status::Settled(Verification::ByEffect { calls }) = &i.status {
            s.push_str(&format!(
                " (checked against {} tool call(s): {})",
                calls.len(),
                calls.join(", ")
            ));
        }
        s
    }
}

/// The most items one `list` prints before it stops and says so.
const MAX_LIST: usize = 50;

impl Tool for Todo {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "todo",
            "Keep the working list for what you are doing, and check it against what \
             actually ran. `op` is one of add, start, note, complete, block, list, claim, \
             hand_back, waiting_on. `add` takes `text`; the others take `id`. Completion \
             is CHECKED: an item marked complete with no successful tool call behind it \
             is recorded as claimed, not complete, and the call tells you so. Every \
             result carries the whole list's counts and says where the list is stored.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "op": {
                        "type": "string",
                        "enum": ["add", "start", "note", "complete", "block", "list",
                                 "claim", "hand_back", "waiting_on"],
                        "description": "add: put an item on the list. start: begin one. note: add detail. complete: mark done — checked against what ran. block: stop with a reason. list: show everything. claim/hand_back/waiting_on: only mean something when a shared board is mounted."
                    },
                    "text": {"type": "string", "description": "For add: what you are going to do, in one line. For note: the detail. For complete: what was measured."},
                    "id": {"type": "string", "description": "Which item, as printed by `list`."},
                    "why": {"type": "string", "description": "For block: what is stopping it."},
                    "of": {"type": "string", "description": "For waiting_on: who owes the next move. This does not change who is carrying the item."},
                    "category": {"type": "string", "enum": ["bug", "feature", "chore", "question"], "description": "For add, when a shared board is mounted."}
                },
                "required": ["op"]
            }),
            // Clause 4, decided at construction rather than per call: with a board
            // mounted the widest thing this tool can do is write state other seats
            // act on. Without one it touches nothing outside the session.
            if self.mounted {
                Access::Network
            } else {
                Access::Session
            },
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let turn = ctx.turn_id().to_string();
        let Some(op) = args.get("op").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "todo needs an op",
                format!(
                    "call `todo` again with `op` set to one of: add, start, note, \
                     complete, block, list, claim, hand_back, waiting_on.{}{}",
                    self.board_summary(),
                    self.backing()
                ),
            );
        };
        let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let id_arg = args.get("id").and_then(|v| v.as_str()).unwrap_or("");

        // Every op but `add` and `list` names an item, and resolving it is the same
        // question every time.
        // Boxed because `Invocation` is a large error variant and this closure is
        // called on every op that names an item.
        let resolved = |s: &Self| -> Result<u32, Box<Invocation>> {
            if id_arg.is_empty() {
                return Err(Box::new(Invocation::failed(
                    format!("todo `{op}` needs an id"),
                    format!(
                        "call `todo` again with `id` set to one of: {}.{}{}",
                        if s.ledger.known().is_empty() {
                            "(the list is empty — `op: \"add\"` first)".to_string()
                        } else {
                            s.ledger.known().join(", ")
                        },
                        s.board_summary(),
                        s.backing()
                    ),
                )));
            }
            s.ledger
                .resolve(id_arg)
                .ok_or_else(|| Box::new(s.no_such_item(id_arg)))
        };

        match op {
            "add" => {
                if text.trim().is_empty() {
                    return Invocation::failed(
                        "todo add needs text",
                        format!(
                            "call `todo` again with `op: \"add\"` and `text` set to what \
                             you are going to do, in one line.{}{}",
                            self.board_summary(),
                            self.backing()
                        ),
                    );
                }
                let row = if self.mounted {
                    match self.queue.file(&NewRow {
                        title: text.to_string(),
                        body: text.to_string(),
                        category: args
                            .get("category")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        room: None,
                    }) {
                        Ok(r) => Some(r.id),
                        // Nothing was filed. It does NOT quietly become a local
                        // note: the operator would believe a row exists.
                        Err(e) => return self.queue_failed(e),
                    }
                } else {
                    None
                };
                let id = self
                    .ledger
                    .declare_row(&turn, text, Source::Todo, row.clone());
                let i = self.ledger.get(id).expect("just declared");
                Invocation::ok(format!(
                    "{}{}{}",
                    self.render(&i),
                    self.board_summary(),
                    self.backing()
                ))
            }
            "start" => {
                let id = match resolved(self) {
                    Ok(i) => i,
                    Err(inv) => return *inv,
                };
                match self.ledger.start(&turn, id) {
                    Ok(i) => Invocation::ok(format!(
                        "{}{}{}",
                        self.render(&i),
                        self.board_summary(),
                        self.backing()
                    ))
                    .with_note(
                        "started. If this turn ends with no successful tool call, the \
                         intent check will say so before the next one begins."
                            .to_string(),
                    ),
                    Err(_) => self.no_such_item(id_arg),
                }
            }
            "note" => {
                let id = match resolved(self) {
                    Ok(i) => i,
                    Err(inv) => return *inv,
                };
                let i = self.ledger.get(id).expect("resolved");
                if self.mounted
                    && let Some(r) = &i.row
                    && let Err(e) = self.queue.note(r, text)
                {
                    return self.queue_failed(e);
                }
                Invocation::ok(format!(
                    "noted on {}{}{}",
                    self.render(&i),
                    self.board_summary(),
                    self.backing()
                ))
            }
            "complete" => {
                let id = match resolved(self) {
                    Ok(i) => i,
                    Err(inv) => return *inv,
                };
                let i = match self.ledger.complete(&turn, id) {
                    Ok(i) => i,
                    Err(_) => return self.no_such_item(id_arg),
                };
                let verified = matches!(&i.status, Status::Settled(v) if v.is_complete());
                // The board is only told about a completion that was CHECKED. A
                // row closed on the fleet queue with nothing measured behind it is
                // the same lie one blast radius wider.
                if self.mounted
                    && verified
                    && let Some(r) = &i.row
                    && let Err(e) = self.queue.done(r, if text.is_empty() { "" } else { text })
                {
                    return self.queue_failed(e);
                }
                let mut inv = Invocation::ok(format!(
                    "{}{}{}",
                    self.render(&i),
                    self.board_summary(),
                    self.backing()
                ));
                if !verified {
                    inv = inv.with_note(match &i.status {
                        Status::Settled(Verification::NoEncoder) => format!(
                            "#{} is NOT counted as complete: this session has no effect \
                             log attached, so nothing could check it either way. That is \
                             a harness defect, not yours — say so rather than working \
                             around it.",
                            i.id
                        ),
                        _ => format!(
                            "#{} is NOT counted as complete: no tool call succeeded while \
                             it was in progress, so there is nothing that shows the work \
                             happened. It is recorded as claimed. Either do it and mark \
                             it again, or `block` it with the reason.{}",
                            i.id,
                            if self.mounted {
                                " The shared row was left open."
                            } else {
                                ""
                            }
                        ),
                    });
                }
                inv
            }
            "block" => {
                let id = match resolved(self) {
                    Ok(i) => i,
                    Err(inv) => return *inv,
                };
                let why = args.get("why").and_then(|v| v.as_str()).unwrap_or(text);
                if why.trim().is_empty() {
                    return Invocation::failed(
                        "todo block needs a reason",
                        format!(
                            "call `todo` again with `op: \"block\"`, `id` and `why` set to \
                             what is stopping it. A blocked item with no reason cannot be \
                             unblocked by anybody but you.{}{}",
                            self.board_summary(),
                            self.backing()
                        ),
                    );
                }
                match self.ledger.block(&turn, id, why) {
                    Ok(i) => Invocation::ok(format!(
                        "{}{}{}",
                        self.render(&i),
                        self.board_summary(),
                        self.backing()
                    )),
                    Err(_) => self.no_such_item(id_arg),
                }
            }
            "list" => {
                // With a board mounted the board is the record, so rows this
                // session has never seen are adopted here — which is what puts
                // another seat's row under the same intent/effect check as ours.
                if self.mounted {
                    match self.queue.list() {
                        Ok(rows) => {
                            for r in &rows {
                                self.ledger.adopt(&turn, &r.id, &r.title);
                            }
                        }
                        Err(e) => return self.queue_failed(e),
                    }
                }
                let items = self.ledger.items();
                let mut body = String::new();
                for i in items.iter().take(MAX_LIST) {
                    body.push_str(&self.render(i));
                    body.push('\n');
                }
                if items.len() > MAX_LIST {
                    body.push_str(&format!(
                        "… stopped after {MAX_LIST} of {}; complete or block some before \
                         adding more\n",
                        items.len()
                    ));
                }
                if items.is_empty() {
                    body.push_str("the list is empty\n");
                }
                Invocation::ok(format!("{body}{}{}", self.board_summary(), self.backing()))
            }
            "claim" => {
                if !self.mounted {
                    return self.refuse_unmounted("claim");
                }
                if id_arg.is_empty() {
                    return Invocation::failed(
                        "todo claim needs an id",
                        format!(
                            "call `todo` again with `op: \"claim\"` and `id` set to the row \
                             you want to take. `list` shows what is on the board.{}{}",
                            self.board_summary(),
                            self.backing()
                        ),
                    );
                }
                // No `expect` argument anywhere in this call, by construction: the
                // seam reads the holder and swaps against what it read.
                match self.queue.claim(id_arg) {
                    Ok(r) => {
                        let id = self.ledger.adopt(&turn, &r.id, &r.title);
                        let _ = self.ledger.start(&turn, id);
                        let i = self.ledger.get(id).expect("adopted");
                        Invocation::ok(format!(
                            "{}\n{}{}{}",
                            r.line(),
                            self.render(&i),
                            self.board_summary(),
                            self.backing()
                        ))
                        .with_note(
                            "you are carrying this row now, and the whole fleet can see \
                             that. A row claimed and not worked is visible to everybody."
                                .to_string(),
                        )
                    }
                    Err(e) => self.queue_failed(e),
                }
            }
            "hand_back" => {
                if !self.mounted {
                    return self.refuse_unmounted("hand_back");
                }
                match self.queue.hand_back(id_arg) {
                    Ok(r) => Invocation::ok(format!(
                        "{}{}{}",
                        r.line(),
                        self.board_summary(),
                        self.backing()
                    )),
                    Err(e) => self.queue_failed(e),
                }
            }
            "waiting_on" => {
                if !self.mounted {
                    return self.refuse_unmounted("waiting_on");
                }
                let of = args.get("of").and_then(|v| v.as_str()).unwrap_or("");
                if of.trim().is_empty() {
                    return Invocation::failed(
                        "todo waiting_on needs `of`",
                        format!(
                            "call `todo` again with `op: \"waiting_on\"`, `id`, and `of` \
                             set to who owes the next move. This does not hand the row \
                             over — `claim` is the verb that does that.{}{}",
                            self.board_summary(),
                            self.backing()
                        ),
                    );
                }
                match self.queue.waiting_on(id_arg, of, text) {
                    Ok(r) => Invocation::ok(format!(
                        "{}{}{}",
                        r.line(),
                        self.board_summary(),
                        self.backing()
                    ))
                    .with_note(
                        "this records who owes the next move; it did NOT change who is \
                         carrying the row."
                            .to_string(),
                    ),
                    Err(e) => self.queue_failed(e),
                }
            }
            other => {
                let ops = [
                    "add",
                    "start",
                    "note",
                    "complete",
                    "block",
                    "list",
                    "claim",
                    "hand_back",
                    "waiting_on",
                ];
                let near = ops
                    .iter()
                    .map(|o| (super::super::edit_distance(other, o), o))
                    .min_by_key(|(d, _)| *d)
                    .filter(|(d, _)| *d <= 3)
                    .map(|(_, o)| *o);
                let mut body = format!("`todo` takes one of: {}.", ops.join(", "));
                if let Some(n) = near {
                    body.push_str(&format!("\nthe nearest to `{other}` is `{n}`."));
                }
                Invocation::failed(
                    format!("`{other}` is not a todo op"),
                    format!("{body}{}{}", self.board_summary(), self.backing()),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins::intent::ledger::IntentSink;
    use crate::builtins::intent::queue::FakeQueue;
    use crate::events::{NullToolSink, ToolEvent, ToolEventSink};
    use crate::result::ToolResult;
    use crate::testing::harness;
    use letibot_transcript::ToolOutcome;
    use std::sync::Arc;

    struct Fix {
        h: crate::testing::Harness,
        ledger: Arc<IntentLedger>,
        sink: IntentSink<NullToolSink>,
    }

    impl Fix {
        fn local() -> Self {
            let ledger = Arc::new(IntentLedger::new());
            let sink = IntentSink::new(ledger.clone(), NullToolSink);
            let mut h = harness();
            h.rt.registry
                .register(Box::new(Todo::local(ledger.clone())))
                .unwrap();
            Fix { h, ledger, sink }
        }

        fn call(&mut self, args: &str) -> ToolResult {
            self.h.call("todo", args)
        }

        /// A tool call that really ran, as the encoder sees it.
        fn effect(&mut self, call_id: &str, name: &str, ok: bool) {
            self.sink.emit(ToolEvent::Started {
                turn_id: "turn_1".into(),
                call_id: call_id.into(),
                name: name.into(),
                access: Access::Read,
            });
            self.sink.emit(ToolEvent::Finished {
                turn_id: "turn_1".into(),
                call_id: call_id.into(),
                outcome: if ok {
                    ToolOutcome::Ok
                } else {
                    ToolOutcome::Failed {
                        reason: "no".into(),
                    }
                },
                payload_digest: "d".into(),
                inline_bytes: 1,
                full_bytes: 1,
                spill: None,
                repairs: 0,
            });
        }
    }

    /// A mounted `todo` declares `Network`, so the fixture needs a gate that
    /// admits — which is itself the point being made in `Todo::mounted`'s docs.
    fn mounted_fix(q: Arc<dyn Queue>) -> Fix {
        let ledger = Arc::new(IntentLedger::new());
        let sink = IntentSink::new(ledger.clone(), NullToolSink);
        let mut h = harness();
        h.rt.registry
            .register(Box::new(Todo::mounted(ledger.clone(), q)))
            .unwrap();
        h.rt = h.rt.with_gate(crate::testing::allow_all());
        Fix { h, ledger, sink }
    }

    #[test]
    fn a_completion_with_nothing_behind_it_is_reported_as_claimed() {
        let mut f = Fix::local();
        f.call(r#"{"op":"add","text":"rewrite the parser"}"#);
        f.call(r#"{"op":"start","id":"1"}"#);
        let r = f.call(r#"{"op":"complete","id":"1"}"#);
        let out = r.render();
        assert!(out.contains("NOT counted as complete"), "{out}");
        assert!(out.contains("0 of 1 complete"), "{out}");
        assert_eq!(f.ledger.board().done, 0);
        assert_eq!(f.ledger.board().claimed, 1);
    }

    #[test]
    fn a_completion_with_a_successful_call_behind_it_is_complete() {
        let mut f = Fix::local();
        f.call(r#"{"op":"add","text":"rewrite the parser"}"#);
        f.call(r#"{"op":"start","id":"1"}"#);
        f.effect("c9", "edit", true);
        let r = f.call(r#"{"op":"complete","id":"1"}"#);
        let out = r.render();
        assert!(out.contains("1 of 1 complete"), "{out}");
        assert!(out.contains("checked against 1 tool call"), "{out}");
        assert!(!out.contains("NOT counted"), "{out}");
    }

    #[test]
    fn every_result_says_where_the_list_is_stored() {
        let mut f = Fix::local();
        let out = f.call(r#"{"op":"add","text":"a thing"}"#).render();
        assert!(out.contains("no board is mounted"), "{out}");

        let mut m = mounted_fix(Arc::new(FakeQueue::new("claude-lab2x1")));
        let out = m.call(r#"{"op":"add","text":"a thing"}"#).render();
        assert!(out.contains("the shared board"), "{out}");
        assert!(out.contains("row r1"), "{out}");
    }

    #[test]
    fn a_mount_only_verb_refuses_rather_than_becoming_a_local_no_op() {
        let mut f = Fix::local();
        let r = f.call(r#"{"op":"claim","id":"r7"}"#);
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }), "{r:?}");
        let out = r.render();
        assert!(out.contains("no board is mounted"), "{out}");
        assert!(out.contains("configuration and not a fault"), "{out}");
        assert_eq!(f.ledger.items().len(), 0, "nothing was quietly recorded");
    }

    #[test]
    fn a_row_somebody_else_took_refuses_and_names_them() {
        let q = Arc::new(FakeQueue::new("claude-lab2x1"));
        q.seed("r7", "rewrite the parser", None);
        q.set_holder("r7", Some("claude-host"));
        let mut f = mounted_fix(q);
        let r = f.call(r#"{"op":"claim","id":"r7"}"#);
        assert!(matches!(r.outcome, ToolOutcome::Failed { .. }), "{r:?}");
        let out = r.render();
        assert!(out.contains("claude-host"), "{out}");
        assert!(out.contains("do not start the work anyway"), "{out}");
    }

    #[test]
    fn an_unreachable_board_never_becomes_a_local_note() {
        let mut f = mounted_fix(Arc::new(FakeQueue::offline("claude-lab2x1")));
        let r = f.call(r#"{"op":"add","text":"a thing"}"#);
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }), "{r:?}");
        assert!(r.render().contains("NOTHING was written"), "{}", r.render());
        assert_eq!(
            f.ledger.items().len(),
            0,
            "a failed file must not leave a local item the operator thinks is a row"
        );
    }

    #[test]
    fn a_claimed_row_from_another_seat_comes_under_the_same_check() {
        let q = Arc::new(FakeQueue::new("claude-lab2x1"));
        q.seed("r7", "somebody else's row", None);
        let mut f = mounted_fix(q);
        f.call(r#"{"op":"claim","id":"r7"}"#);
        // Claimed and not worked: the diff sees it.
        let rec = f.ledger.reconcile("turn_1", "");
        assert!(
            rec.findings
                .iter()
                .any(|x| matches!(x, super::super::ledger::Finding::AnnouncedNotStarted { .. })),
            "{:?}",
            rec.findings
        );
    }

    #[test]
    fn an_unknown_op_comes_back_with_the_list_and_the_nearest() {
        let mut f = Fix::local();
        let out = f.call(r#"{"op":"complte","id":"1"}"#).render();
        assert!(out.contains("nearest to `complte` is `complete`"), "{out}");
    }

    #[test]
    fn an_unknown_id_reports_the_ids_there_are() {
        let mut f = Fix::local();
        f.call(r#"{"op":"add","text":"a"}"#);
        let out = f.call(r#"{"op":"start","id":"99"}"#).render();
        assert!(out.contains("#1"), "{out}");
    }

    #[test]
    fn blocking_without_a_reason_is_refused_with_the_fix() {
        let mut f = Fix::local();
        f.call(r#"{"op":"add","text":"a"}"#);
        let r = f.call(r#"{"op":"block","id":"1"}"#);
        assert!(matches!(r.outcome, ToolOutcome::Failed { .. }));
        assert!(r.render().contains("`why`"), "{}", r.render());
    }

    #[test]
    fn the_local_path_needs_no_flowy_environment() {
        // The whole local path, with FLOWY_ADDR / FLOWY_TOKEN / FLOWY_AGENT
        // removed from the process. Nothing here reads the environment and
        // nothing here opens a socket — this crate has no HTTP client at all —
        // but the property is worth a test rather than an assertion in a comment.
        let _guard = env_lock();
        let saved: Vec<(String, Option<String>)> = ["FLOWY_ADDR", "FLOWY_TOKEN", "FLOWY_AGENT"]
            .iter()
            .map(|k| (k.to_string(), std::env::var(k).ok()))
            .collect();
        for (k, _) in &saved {
            // SAFETY: single-threaded within the env lock, and nothing in this
            // crate reads these names.
            unsafe { std::env::remove_var(k) };
        }

        let mut f = Fix::local();
        f.call(r#"{"op":"add","text":"works with no fabric"}"#);
        f.call(r#"{"op":"start","id":"1"}"#);
        f.effect("c1", "read", true);
        let out = f.call(r#"{"op":"complete","id":"1"}"#).render();
        assert!(out.contains("1 of 1 complete"), "{out}");
        assert!(out.contains("no board is mounted"), "{out}");
        let listed = f.call(r#"{"op":"list"}"#);
        assert_eq!(listed.outcome, ToolOutcome::Ok);

        for (k, v) in saved {
            if let Some(v) = v {
                // SAFETY: as above.
                unsafe { std::env::set_var(&k, v) };
            }
        }
    }

    /// Serialises the two tests that touch process environment.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
        L.lock().unwrap_or_else(|e| e.into_inner())
    }
}
