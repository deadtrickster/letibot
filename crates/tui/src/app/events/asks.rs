//! Events that ask the person something, and their answers: a decision, a secret, a prompt,
//! an explanation, a denial.

use super::*;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::view::{OpenDecision, SettledDecision, Warned};

impl App {
    /// A question the operator answered as the daemon stopped holding it. The words typed on a card that went are an
    /// answer, not a prompt: left in the composer, the next Enter would send them to the model. Said, not silent,
    /// and undoable; a permission's words are the operator's own and are never touched.
    pub(crate) fn drop_a_gone_questions_draft(
        &mut self,
        was_asking: Option<OpenDecision>,
        same_session: bool,
    ) {
        let Some(d) = was_asking else {
            return;
        };
        // A switch is not a card going away: the question is the outgoing session's, and a line
        // typed there is the operator's own, so leaving takes nothing with it.
        if !same_session
            || d.kind != "question"
            || d.req_id.starts_with(crate::app::todos::TODO_ASK)
        {
            return;
        }
        // A command is never an answer: `App::submit` takes `/` and `!` before it reads the card.
        let typed = self.editor.text();
        let command = typed.starts_with('/') || typed.starts_with('!');
        let empty = typed.is_empty();
        if command
            || empty
            || self.open.iter().any(|o| o.req_id == d.req_id)
            || self.away.iter().any(|a| a.decision.req_id == d.req_id)
            // A card this head answered is not one that closed under the operator: the words
            // were the answer, and what is in the composer now came back with the card's going.
            || self.answering.iter().any(|a| a.decision.req_id == d.req_id)
        {
            return;
        }
        self.clear_composer();
        self.say(
            "that question is no longer open — its card closed, and what you typed on it was \
             cleared (ctrl-z brings it back)",
        );
        self.redraw = true;
    }

    /// Asks and their answers: decisions, secrets, prompts, explanations, denials. One family of
    /// [`App::event`]'s arms, moved here verbatim; `event` hands it only these variants.
    pub(crate) fn on_ask_event(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        match e {
            SessionEvent::DecisionRequested {
                req_id,
                kind,
                call_id,
                access,
                summary,
                target,
                write_targets,
                detail,
                options,
                options_note,
                choices,
                because,
                advice,
                deadline,
                on_timeout,
                subagent,
                ..
            } => {
                // An answer is already on its way, and this say is a replay or a resync: putting
                // the card back would let the next Enter send the same answer a second time.
                if self.answering.iter().any(|a| a.decision.req_id == req_id) {
                    return Disposition::Rendered;
                }
                // A card the operator put away stays away when its question is said again
                // (a replay, a second head's resync): refreshed in place, still not on screen.
                if let Some(a) = self.away.iter_mut().find(|a| a.decision.req_id == req_id) {
                    a.decision = OpenDecision {
                        req_id,
                        write_targets,
                        kind,
                        call_id,
                        access,
                        summary,
                        target,
                        detail,
                        options,
                        options_note,
                        choices,
                        because,
                        advice,
                        deadline,
                        on_timeout,
                        subagent,
                        asked_ts: a.decision.asked_ts,
                    };
                    self.redraw = true;
                    return Disposition::Rendered;
                }
                self.open.retain(|d| d.req_id != req_id);
                // The highlight is NOT reset here: the card goes to the BACK, and the one on
                // screen keeps the row the operator moved to. `App::sync_front` resets it when
                // the front card changes, so a fresh question still starts at the top.
                self.open.push(OpenDecision {
                    req_id,
                    write_targets,
                    kind,
                    call_id,
                    access,
                    summary,
                    target,
                    detail,
                    options,
                    options_note,
                    choices,
                    because,
                    advice,
                    deadline,
                    on_timeout,
                    subagent,
                    // Not `ts`. A head renders how long a decision has been waiting
                    // from the view's own stamp, and this arm is the live one - the
                    // snapshot path at `apply` carries the real `asked_ts`.
                    asked_ts: 0,
                });
                Disposition::Rendered
            }
            SessionEvent::DecisionAnswered {
                req_id,
                outcome,
                by,
                basis,
                late,
            } => {
                // A question put away and settled while away - its deadline lapsed, another
                // head answered it - settles exactly as an open one does, and its kept draft
                // goes with it: there is nothing left to answer.
                if let Some(i) = self.away.iter().position(|a| a.decision.req_id == req_id) {
                    let a = self.away.remove(i);
                    self.open.push(a.decision);
                    self.redraw = true;
                }
                // …and one this head answered, closed when the answer left: it settles
                // here like an open one, read off the same card.
                let mut answered_here = false;
                if let Some(i) = self
                    .answering
                    .iter()
                    .position(|a| a.decision.req_id == req_id)
                {
                    let a = self.answering.remove(i);
                    self.open.push(a.decision);
                    answered_here = true;
                }
                // Read off the open decision BEFORE it is removed: the answer event carries
                // only the `req_id`, and the oracle's `advice` can never ride it - its `basis`
                // is the DECIDER's. `kind` too: a question's ending travels as `Cancelled`.
                let (summary, call_id, kind, advice) = self
                    .open
                    .iter()
                    .find(|d| d.req_id == req_id)
                    .map(|d| {
                        (
                            d.summary.clone(),
                            d.call_id.clone(),
                            d.kind.clone(),
                            d.advice.clone(),
                        )
                    })
                    .unwrap_or_default();
                // The front card, before the settlement takes it: the composer's words are that
                // card's answer, and only a question on screen owns them.
                let was_asking = self.open.first().cloned();
                self.open.retain(|d| d.req_id != req_id);
                // A card this head answered has already used the composer's words, and what is
                // there now came back with the card's going. Only a question that closed under
                // the operator's fingers leaves a draft that the next Enter would send.
                if !answered_here {
                    self.drop_a_gone_questions_draft(was_asking, true);
                }
                let d = SettledDecision {
                    req_id,
                    kind: kind.clone(),
                    call_id: call_id.clone(),
                    summary,
                    outcome,
                    by,
                    basis,
                    advice,
                    late,
                };
                // A permission settles on the call it gated, so it rides that call's card in
                // the dim register. A question has no call to ride and stays a note - as does
                // a log recorded before the field existed.
                if let Some(call_id) = &call_id
                    && let Some(t) = self.turn.as_mut()
                    && let Some(c) = t.calls.iter_mut().find(|c| &c.call_id == call_id)
                {
                    c.decision = Some(d);
                } else {
                    self.note(Note::settled(d));
                }
                Disposition::Rendered
            }
            SessionEvent::SecretRequested {
                req_id,
                prompt,
                command,
                deadline,
            } => {
                if command.is_empty() {
                    self.key_secrets.push(req_id.clone());
                }
                self.secret = Some(SecretAsk {
                    req_id,
                    prompt,
                    command,
                    deadline,
                });
                self.secret_buf.clear();
                self.redraw = true;
                Disposition::Rendered
            }
            SessionEvent::SecretSettled { req_id, given, by } => {
                // A key card is a secret request with no command (`Harness::obtain_key`),
                // remembered by id because the card itself may already be gone.
                let was_key = match self.key_secrets.iter().position(|r| *r == req_id) {
                    Some(i) => {
                        self.key_secrets.remove(i);
                        true
                    }
                    None => false,
                };
                if self.secret.as_ref().is_some_and(|s| s.req_id == req_id) {
                    self.secret = None;
                    self.secret_buf.clear();
                }
                if was_key {
                    // Given: the daemon says where it was saved (`provider_key_saved`), so
                    // one line rather than two. Not given: the refusal is the operator's.
                    if !given {
                        self.note(Note::Warned(Warned {
                            code: "provider_key_refused".into(),
                            detail: format!("no key given ({by})"),
                            ts,
                        }));
                    }
                    return Disposition::Rendered;
                }
                self.note(Note::Warned(Warned {
                    code: "sudo".into(),
                    detail: if given {
                        format!("password given by {by}")
                    } else {
                        format!("no password given ({by})")
                    },
                    ts,
                }));
                Disposition::Rendered
            }
            // A command of the operator's own is waiting for an answer: the card is up for as
            // long as the run is blocked and owns the keyboard while it is. Its text is NOT
            // masked - this is a line for a program's stdin; a password has its own card.
            SessionEvent::PromptRequested {
                req_id,
                job,
                command,
                question,
                reading,
            } => {
                // A question from a RUNNING PROGRAM outranks the question about killing one:
                // a prompt can arrive while the confirmation stands, and then `y` would be both
                // this card's yes and that card's text. The confirmation yields, and says so.
                if let Some(ask) = self.term_ask.take() {
                    self.say(&format!(
                        "{} is still running — your command's question came first, so nothing \
                         was ended. `!term close` asks again.",
                        ask.line
                    ));
                }
                self.prompt = Some(PromptAsk {
                    req_id,
                    job,
                    command,
                    question,
                    reading,
                });
                self.prompt_buf.clear();
                // A card that arrives takes the screen, even one re-offered after an answer on
                // a run this daemon cannot read: a head that kept its own "put away" across it
                // would swallow the second question `apt` asks.
                self.prompt_away = false;
                self.redraw = true;
                Disposition::Rendered
            }
            // The card comes down, whether it was answered or the command ended - and the
            // sentence says which, because those are different things to have happened to a
            // person who was about to type.
            SessionEvent::PromptSettled { req_id, sent, by } => {
                if self.prompt.as_ref().is_some_and(|p| p.req_id == req_id) {
                    self.prompt = None;
                    self.prompt_buf.clear();
                    self.prompt_away = false;
                }
                self.note(Note::Warned(Warned {
                    code: "prompt".into(),
                    detail: if sent {
                        format!("answer sent by {by}")
                    } else {
                        format!("nothing sent ({by})")
                    },
                    ts,
                }));
                Disposition::Rendered
            }
            // §6's plan is a document; the transcript is not where it goes.
            SessionEvent::Explain { .. } => Disposition::Filtered,
            // Always rendered, at every verbosity: a denial the operator cannot see
            // manufactures the workaround, so it goes into the transcript beside the call it
            // refused. It reuses `Note::Warned`; `code` carries what a reader needs.
            SessionEvent::DenialRaised {
                request_id,
                tool,
                summary,
                by,
                basis,
                outcome,
                repeat_count,
                breaker_open,
                grant,
                ..
            } => {
                let repeat = if breaker_open {
                    format!(
                        " · breaker OPEN after {repeat_count} consecutive refusals; only you \
                         can lift it"
                    )
                } else if repeat_count > 1 {
                    format!(" · attempt {repeat_count} at the same task direction")
                } else {
                    String::new()
                };
                // A refusal the harness made is not a question for the operator: nobody was
                // asked, no grant lifts it, and the tool's own result row one above already
                // explains it. A refusal somebody DECIDED keeps the loud register.
                if by.starts_with("boundary:") {
                    // Only when it has something the row does not: on a single refusal this
                    // note would say the row's own words twice. What the row cannot say is that
                    // this is a repeat, or that the breaker has closed the direction.
                    if !repeat.is_empty() {
                        self.note(Note::NotRun(Warned {
                            code: format!("denied:{request_id}"),
                            // The gist, not the transcript of it: layer A's
                            // explanation runs to a paragraph per unresolved
                            // construct and every word of it is for the model.
                            detail: format!(
                                "{tool} {outcome} — {}{repeat}",
                                first_sentence(&basis)
                            ),
                            ts,
                        }));
                    }
                } else {
                    self.note(Note::Warned(Warned {
                        code: format!("denied:{request_id}"),
                        detail: format!(
                            "REFUSED {tool} — {summary}. {outcome} by {by}: {basis}{repeat}. {grant}"
                        ),
                        ts,
                    }));
                }
                Disposition::Rendered
            }
            _ => unreachable!("on_ask_event was handed an event it does not handle"),
        }
    }
}
