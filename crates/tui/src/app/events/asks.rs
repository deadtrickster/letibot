//! **Events that ask the person something, and their answers**: a decision, a secret, a prompt,
//! an explanation, a denial.

use super::*;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::view::{OpenDecision, SettledDecision, Warned};

impl App {
    /// **Asks and their answers**: decisions, secrets, prompts, explanations, denials. One family of
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
                choices,
                because,
                advice,
                deadline,
                on_timeout,
                subagent,
                ..
            } => {
                self.open.retain(|d| d.req_id != req_id);
                // A fresh question starts at the top of its ladder rather than
                // wherever the last one was left: the highlight must never be
                // somewhere the operator did not put it when Enter is one key away.
                self.sel = 0;
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
                    choices,
                    because,
                    advice,
                    deadline,
                    on_timeout,
                    subagent,
                    // Not `ts`. A head renders how long a decision has been waiting
                    // from the view's own stamp, and this arm is the live one — the
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
                // Three things are read off the open decision **before** it is
                // removed, because the answer event carries only the `req_id`: the
                // summary, the call to put the outcome on, and the oracle's advice.
                // The last is the one the answer event can never carry — its `basis`
                // is the DECIDER's, and under `/supervise` the decider is usually the
                // operator. And which KIND it was, which the live arm needs for the
                // same reason the view keeps it: a question has no ladder, so its
                // ending travels as `Cancelled` with the answer in the basis.
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
                self.open.retain(|d| d.req_id != req_id);
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
                // A permission settles on the call it gated: the approval is a fact
                // about the call, so it rides the call's card in the dim register
                // rather than as a standalone note. A question — or a log recorded
                // before the field existed — has no call to ride, and stays a note.
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
            // **A command of the operator's own is waiting for an answer.** The card is up
            // for as long as the run is blocked, and the keyboard belongs to it while it
            // is — see the `Key` arm and [`App::prompt_lines`].
            //
            // **The text is NOT masked**, and that is the difference from the arm above
            // rather than an oversight: what this card carries is a line for a program's
            // stdin, drawn in the open. A password has its own path and its own card, and
            // the two must not be one.
            SessionEvent::PromptRequested {
                req_id,
                job,
                command,
                question,
                reading,
            } => {
                // **A question from a RUNNING PROGRAM outranks the question about killing one.**
                // The two cards are mutually exclusive by construction everywhere else (the
                // confirmation is raised from the composer, and the prompt card owns the
                // composer while it is up) — but a prompt can arrive *while* the confirmation
                // is standing, and then one keystroke would have two meanings: `y` is this
                // card's yes and that card's text. The operator's rule is that neither may be
                // answerable by the other's keystroke, so the confirmation yields, with a
                // sentence — the program has asked something and must not be killable by the
                // answer to it.
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
                // **A card that arrives takes the screen**, including one re-offered after an
                // answer on a run this daemon cannot read: the offer is the daemon's, and a
                // head that kept its own *put away* across it would swallow the second
                // question `apt` asks. See [`Prompts::send`] on the daemon's side.
                self.prompt_away = false;
                self.redraw = true;
                Disposition::Rendered
            }
            // The card comes down, whether it was answered or the command ended — and the
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
            // **Always rendered, at every verbosity.**
            //
            // `docs/boundary-and-adjudication.md` §4b: a denial the operator cannot
            // see manufactures the workaround, so there is no verbosity at which
            // hiding this is correct — it is a decision taken on their behalf and
            // only they can lift it. It goes into the transcript, in the place it
            // happened, beside the tool call it refused.
            //
            // This reuses `Note::Warned` rather than growing a note kind of its own:
            // making a denial *look* different from a warning is presentation, this
            // head is somebody else's this session, and a placeholder that rendered
            // nothing would be the invisible denial again with a different cause.
            // The `code` carries the distinction a reader needs, and
            // `repeat_count`/`breaker_open` are on the event for a head that later
            // wants to collapse repeats.
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
                // **A refusal the harness made is not a question for the operator.**
                //
                // `boundary:` signs the deterministic ones — the normaliser could not
                // read the command, the host boundary refused it. Nobody was asked,
                // no grant lifts it, and the whole explanation is already on the
                // screen as the tool's own result one row above. Rendering it again
                // in red, in full, is the same wall twice: measured at 20 lines for
                // one shell one-liner, and the operator's answer to it was *"it just
                // throws up on my chat"*.
                //
                // A refusal somebody DECIDED still gets the loud register and the
                // request id, because granting it is a thing the operator can do.
                if by.starts_with("boundary:") {
                    // **And only when it has something the row does not.**
                    //
                    // The refused call is already a row in the transcript, one line
                    // below this, carrying the same first sentence — so on a single
                    // refusal this note is the same words twice, which is the noise
                    // it was just cut down from. What the row cannot say is that
                    // this is the SECOND attempt at the same direction, or that the
                    // breaker has closed the direction for the session: that is a
                    // fact about the shape of the session and not about one call,
                    // and it is the one an operator wants to be told.
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
