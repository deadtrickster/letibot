//! The daemon's half of answering the operator's own command: who holds the run's input, and how
//! a line gets into it.
//!
//! [`letibot_sessionlog::PromptDriver`] is the seam and [`Prompts`] implements it. The mechanism
//! is [`letibot_tools::exec::Stdin`] - a handle on the run's input, its terminal on the ordinary
//! path - and the detection is [`letibot_tools::exec::ask`], which reads `/proc` and never the
//! program's words. What lives here is what the daemon has to hold: the one operator command in
//! flight, the open `req_id` for it, and the write a head's answer and a `!send` both become.
//!
//! The state cannot live in the harness, because the run's own thread is blocked inside the very
//! command that is asking. It is one per SESSION - a `!send` in one session must never write into
//! another's command, and the id check in [`Prompts::send`] is the assertion that the registry's
//! lookup was the right one.
//!
//! Three things it deliberately does not do: hold a secret (`SUDO_ASKPASS` keeps its own path, and
//! the two must not be mergeable by a later edit), match text (the last line a program wrote is
//! carried to be shown and never looked at), or block under the lock (the write is a syscall, and
//! the run's wait loop wants the lock to report the next question).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::{PromptDriver, PromptReading, SessionEvent};
use letibot_tools::exec::Stdin;

/// One operator command, in flight.
struct Run {
    /// The job's handle, as `job_list` spells it. The key that ties the three reports together.
    job: String,
    /// The command as the operator typed it, verbatim. Carried on the card because the daemon
    /// cannot know which process in a pipeline asked - `sudo apt install mc` is three programs and
    /// the question is the third one's - so the card names the one thing that is certain.
    command: String,
    /// The way in: a handle on the run's input, cloned out of the job.
    stdin: Stdin,
    /// The card that is up for this run, if any. `None` between the run starting and the first
    /// question, and again after one is answered.
    req: Option<String>,
    /// Whether the daemon has already said it cannot tell whether this run is waiting. Once per
    /// run: the condition holds as long as the run does, and a sentence per beat is a sentence
    /// nobody reads.
    unreadable_said: bool,
    /// Whether this run is one the daemon may not look at at all - the condition, as distinct
    /// from the once-per-run sentence. It is what brings the card back after an answer, because a
    /// run this daemon cannot read stays that way and `apt` asks more than one question.
    unreadable: bool,
}

/// See the module header.
pub struct Prompts {
    /// The session this driver is for, and the check in [`PromptDriver::send`].
    session: String,
    /// Where a card is raised and a settlement recorded. The hub is the session's own, so a
    /// `PromptRequested` reaches every head attached to this session and no other.
    hub: Arc<Hub>,
    /// The one run, or `None` when nothing of the operator's is in flight.
    inner: Mutex<Option<Run>>,
    /// Mints request ids. Per driver and not per daemon: an id is only ever compared against one
    /// run's own open card, and a daemon-wide counter would make two sessions' ids look related.
    next: AtomicU64,
}

impl Prompts {
    /// A driver for one session. Called by the session's own harness at open - the half that owns
    /// the exec host, and so the half that has the handle.
    pub fn new(session: &str, hub: Arc<Hub>) -> Arc<Prompts> {
        Arc::new(Prompts {
            session: session.to_string(),
            hub,
            inner: Mutex::new(None),
            next: AtomicU64::new(0),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Run>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The operator's own command has started, and it has a stdin. This is what makes `!send`
    /// work when no card is ever raised, which is the miss the feature is allowed to have because
    /// a person can still answer.
    ///
    /// A new run replaces the old one, and a card left up for the old one is settled first: a
    /// session runs one operator command at a time, so a run already held means the previous
    /// one's `Ended` did not arrive, and a stale handle behind a live card is the worst state
    /// this object can be in - a line typed for `apt` written into whatever started next.
    pub fn opened(&self, job: &str, command: &str, stdin: Stdin) {
        let stale = {
            let mut g = self.lock();
            g.take().and_then(|r| r.req)
        };
        if let Some(req_id) = stale {
            self.hub.publish(SessionEvent::PromptSettled {
                req_id,
                sent: false,
                by: "a new command started".to_string(),
            });
        }
        *self.lock() = Some(Run {
            job: job.to_string(),
            command: command.to_string(),
            stdin,
            req: None,
            unreadable_said: false,
            unreadable: false,
        });
    }

    /// Mint a request and publish the card - the one place a card is raised, so the reading-backed
    /// card and the offer cannot drift apart in what they carry.
    ///
    /// `reading` is which of the two facts the card is raised on; `question` is the program's own
    /// last line when there is one to show, and `None` on [`PromptReading::Unreadable`], where the
    /// card's own words are the honest sentence.
    fn offer(&self, job: &str, question: Option<&str>, reading: PromptReading) {
        let (req_id, command) = {
            let mut g = self.lock();
            let Some(run) = g.as_mut() else {
                // A question about a run this daemon never saw start. Nothing to raise a card
                // *for*: the card's answer has to have somewhere to go.
                return;
            };
            if run.job != job || run.req.is_some() {
                return;
            }
            let id = format!(
                "prompt-{}-{}",
                self.session,
                self.next.fetch_add(1, Ordering::Relaxed) + 1
            );
            run.req = Some(id.clone());
            (id, run.command.clone())
        };
        self.hub.publish(SessionEvent::PromptRequested {
            req_id,
            job: job.to_string(),
            command,
            question: question.map(str::to_string),
            reading,
        });
    }

    /// The run looks like it is waiting for an answer, by `letibot_tools::exec::ask`'s reading of
    /// the process - never of its words.
    ///
    /// `question` is the last line the program wrote, carried to be shown; `None` because a
    /// program blocked before its first byte is a real case (`! cat`).
    ///
    /// One card per run at a time: the tool already refuses to report the same question twice, and
    /// this refuses independently - a run that is blocked stays blocked, and a second card would
    /// be a second field on the screen for one question.
    pub fn asking(&self, job: &str, question: Option<&str>) {
        self.offer(job, question, PromptReading::Blocked);
    }

    /// This daemon cannot tell whether the run is waiting for a line, and it says so - and offers
    /// the way in as a card that does not claim the program asked.
    ///
    /// The card is raised on a fact this object holds - a run of the operator's own is in flight
    /// - and not on a guess about the process, which `ask` cannot read when one of the run's
    ///   processes belongs to another uid. Raising no card at all left the person typing `y` at the
    ///   composer, where it reached the model while their own command died at its deadline. The
    ///   sentence stays beside the card, because `!send` is the floor under every miss this
    ///   heuristic has; both are said once per run, since a condition reported per beat is a red
    ///   block nobody reads.
    ///
    /// `quiet` is the beat the run had, or had not. A quiet run may well be waiting; a run that is
    /// still writing is either working or blocked with something drawing, and the daemon cannot
    /// tell those apart - worth saying, because a `!` line's output does not land until it ends.
    pub fn unreadable(&self, job: &str, quiet: bool) {
        let command = {
            let mut g = self.lock();
            let Some(run) = g.as_mut() else {
                // A report about a run this daemon never saw start, or one that has ended.
                // There is nothing to name and nobody to tell.
                return;
            };
            if run.job != job || run.unreadable_said {
                return;
            }
            run.unreadable_said = true;
            // The condition itself, kept apart from the once-per-run sentence: it is what
            // re-offers the card after an answer in [`Prompts::send`].
            run.unreadable = true;
            run.command.clone()
        };
        // The two facts, in two sentences: a run that has been quiet may be waiting, and a run
        // that is still writing is either working or blocked with something drawing. Everything
        // after the first clause is true of both and is said once.
        let lead = if quiet {
            format!(
                "`{command}` has been quiet for a beat and this daemon cannot tell whether it is \
                 waiting for a line"
            )
        } else {
            format!(
                "`{command}` is still writing output and this daemon cannot tell whether it is \
                 waiting for a line"
            )
        };
        let working = if quiet {
            ""
        } else {
            " It is working, or it is blocked with something still drawing: from here those are \
             the same picture, and none of the command's own output reaches you until it ends."
        };
        self.hub.publish(SessionEvent::Warning {
            code: "operator_run_unreadable".to_string(),
            detail: format!(
                "{lead}: one of its processes belongs to another user, so `/proc` refuses for \
                 it.{working} If it is waiting — `sudo` reaching `apt`'s `Continue? [Y/n]` is the \
                 case this was measured on — the way in is `!send <line>`, which needs no card. \
                 Until the command ends it keeps a thread of its own; the rest of the daemon \
                 runs on, and another `!` line of yours waits for this one rather than \
                 overlapping it."
            ),

            compaction: None,
        });
        // And the offer, beside the sentence - after it, because the disclosure is what the offer
        // stands on. `offer` declines if a card is already up, so this cannot double-card a run.
        self.offer(job, None, PromptReading::Unreadable);
    }

    /// The run is over. The card comes down, and the handle is forgotten.
    ///
    /// `sent: false` with a sentence in `by` rather than an identity: nothing was sent and nobody
    /// is being named for it - the command ended, which is the ordinary ending of every run that
    /// was never answered.
    pub fn ended(&self, job: &str) {
        let settled = {
            let mut g = self.lock();
            match g.as_ref() {
                Some(run) if run.job == job => {
                    let req = run.req.clone();
                    *g = None;
                    req
                }
                // A report about a job that is not the run in flight: an old run's `Ended`
                // arriving after a new one started, or a job this daemon never tracked. Doing
                // nothing is right in both cases, and taking the run anyway would clear a live
                // handle on a stale message.
                _ => None,
            }
        };
        if let Some(req_id) = settled {
            self.hub.publish(SessionEvent::PromptSettled {
                req_id,
                sent: false,
                by: "the command ended".to_string(),
            });
        }
    }
}

impl PromptDriver for Prompts {
    /// Write one line to this session's own running command. See the trait for the two shapes of
    /// `req` and for why nothing here may block.
    fn send(
        &self,
        session_id: &str,
        req: Option<&str>,
        line: &str,
    ) -> Result<Option<String>, String> {
        // The lookup was by session id; this is the assertion that it was the right one.
        if session_id != self.session {
            return Err(format!(
                "this driver writes to session `{}` and was called for `{session_id}`",
                self.session
            ));
        }
        // The lock is released before the write: `send_line` is a blocking syscall on the run's
        // input, and the run's own wait loop wants this lock to report the next question - so
        // holding it across the write would let one unread line stall the loop that raises the
        // card.
        let (stdin, settled) = {
            let g = self.lock();
            let Some(run) = g.as_ref() else {
                return Err(
                    "no command of yours is running in this session, so nothing was sent. \
                     `!send` reaches the operator's own `!` line — the one you typed — and \
                     only while it is running."
                        .to_string(),
                );
            };
            match req {
                Some(id) if run.req.as_deref() != Some(id) => {
                    return Err(format!(
                        "nothing was waiting on `{id}` — the command had ended, or another \
                         head answered first. Nothing was sent."
                    ));
                }
                _ => {}
            }
            (run.stdin.clone(), run.req.clone())
        };
        stdin.send_line(line)?;
        // The card comes down, if it is still the one this answered. Re-locked rather than held
        // see above.
        //
        // And on a run this daemon cannot read, the offer comes straight back: the tool reports
        // `Unreadable` once per run, so nothing else will re-raise it, and `apt` asks more than
        // one question. Only when the answer came as a CARD's - a `!send` reaches here as `None`
        // and it is the act of somebody who has already chosen the verb.
        let reoffered = {
            let mut g = self.lock();
            let mut again = None;
            if let Some(run) = g.as_mut()
                && run.req == settled
            {
                run.req = None;
                if run.unreadable && req.is_some() {
                    let id = format!(
                        "prompt-{}-{}",
                        self.session,
                        self.next.fetch_add(1, Ordering::Relaxed) + 1
                    );
                    run.req = Some(id.clone());
                    again = Some((id, run.job.clone(), run.command.clone()));
                }
            }
            again
        };
        if let Some((req_id, job, command)) = reoffered {
            self.hub.publish(SessionEvent::PromptRequested {
                req_id,
                job,
                command,
                question: None,
                reading: PromptReading::Unreadable,
            });
        }
        Ok(settled)
    }

    /// The run in flight, for the stop that is about to end it. The host's ender names the jobs it
    /// ends; this is the operator's own command, which the same stop kills and which no other
    /// table knows is the person's.
    fn in_flight(&self) -> Option<(String, String)> {
        self.lock()
            .as_ref()
            .map(|run| (run.job.clone(), run.command.clone()))
    }
}

impl std::fmt::Debug for Prompts {
    /// By what it is holding, and never the line or the command: a driver that printed its state
    /// would be a second place a person's answer could reach.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let g = self.lock();
        f.debug_struct("Prompts")
            .field("session", &self.session)
            .field(
                "run",
                &g.as_ref().map(|r| (r.job.clone(), r.stdin.is_open())),
            )
            .field(
                "card_open",
                &g.as_ref().and_then(|r| r.req.is_some().then_some(())),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_sessionlog::SessionEvent;

    /// A hub with one session, for the events a card raises.
    fn a_hub() -> Arc<Hub> {
        Hub::new("p-1")
    }

    fn events(hub: &Arc<Hub>) -> Vec<SessionEvent> {
        hub.retained().into_iter().map(|e| e.event).collect()
    }

    /// A card is raised once per run, and it names the operator's own command.
    ///
    /// The second `asking` for the same run is refused here as well as in the tool: a run that is
    /// blocked stays blocked, so a card per tick would be a hundred fields on the screen for one
    /// question.
    #[test]
    fn a_card_is_raised_once_for_a_run_and_names_the_operators_command() {
        let hub = a_hub();
        let p = Prompts::new("p-1", hub.clone());
        // No run yet: a question about something this daemon never saw start has nowhere to
        // send its answer, so nothing is raised.
        p.asking("j1", Some("Continue? [Y/n]"));
        assert!(events(&hub).is_empty(), "no run, no card");

        p.opened("j1", "sudo apt install mc", Stdin::none());
        p.asking("j1", Some("Continue? [Y/n]"));
        p.asking("j1", Some("Continue? [Y/n]"));
        let raised: Vec<String> = events(&hub)
            .into_iter()
            .filter_map(|e| match e {
                SessionEvent::PromptRequested {
                    command, question, ..
                } => {
                    assert_eq!(question.as_deref(), Some("Continue? [Y/n]"));
                    Some(command)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            raised,
            vec!["sudo apt install mc".to_string()],
            "one card, naming the command the operator typed"
        );
    }

    /// A run this daemon cannot look at is said out loud, once - and says which fact it is.
    ///
    /// `! sudo apt install mc`, the password given, and then nothing: `apt` waits at
    /// `Continue? [Y/n]` as root, `/proc/<pid>/fd/0` is `EACCES` for the daemon, and the run holds
    /// its thread until its deadline. The sentence has to name the way in, which is `!send`.
    ///
    /// Beside it, the offer: a card raised on the run being in flight rather than on a reading.
    /// Both once per run, because a condition reported per beat is a red block nobody reads.
    ///
    /// And the two facts are two sentences: a run that has gone quiet may well be waiting, and a
    /// run that is still writing is either working or blocked with something drawing.
    #[test]
    fn a_run_this_daemon_cannot_look_at_is_said_once_and_names_the_way_in() {
        let hub = a_hub();
        let p = Prompts::new("p-1", hub.clone());
        // Nothing running: a report about a run this daemon never saw start has nobody to tell
        // and no command to name.
        p.unreadable("j1", true);
        assert!(
            !events(&hub)
                .into_iter()
                .any(|e| matches!(e, SessionEvent::Warning { .. })),
            "a report about no run says nothing"
        );

        p.opened("j1", "sudo apt install mc", Stdin::none());
        p.unreadable("j1", true);
        p.unreadable("j1", true);
        p.unreadable("j1", true);
        // And now the card, raised on the fact that the run is in flight. Its `reading` says the
        // daemon could not look, and it carries no question line: a card that showed the
        // program's last line under a headline claiming it asked would be the guess this design
        // refuses.
        let cards: Vec<(Option<String>, letibot_sessionlog::PromptReading)> = events(&hub)
            .into_iter()
            .filter_map(|e| match e {
                SessionEvent::PromptRequested {
                    question, reading, ..
                } => Some((question, reading)),
                _ => None,
            })
            .collect();
        assert_eq!(
            cards,
            vec![(None, letibot_sessionlog::PromptReading::Unreadable)],
            "one card, raised on the absence of a reading and claiming none: {cards:?}"
        );
        let said: Vec<String> = events(&hub)
            .into_iter()
            .filter_map(|e| match e {
                SessionEvent::Warning { code, detail, .. } => Some(format!("{code}: {detail}")),
                _ => None,
            })
            .collect();
        assert_eq!(said.len(), 1, "one sentence for one run: {said:?}");
        assert!(
            said[0].starts_with("operator_run_unreadable:"),
            "the code is the register's, so the head paints it as the caveat it is: {said:?}"
        );
        assert!(
            said[0].contains("sudo apt install mc"),
            "it names the command the person typed: {said:?}"
        );
        assert!(
            said[0].contains("has been quiet for a beat"),
            "a quiet run is told so — it may well be the one that is waiting: {said:?}"
        );
        assert!(
            said[0].contains("!send"),
            "and the way in, which needs no card: {said:?}"
        );
        // The card carries no question and says it was raised without a reading, so the offer can
        // only ever do what `!send` would have.
        //
        // A report about a job that is not this run's is about something else, and a new run
        // starts its own once.
        p.unreadable("j2", true);
        assert_eq!(
            events(&hub)
                .into_iter()
                .filter(|e| matches!(e, SessionEvent::Warning { .. }))
                .count(),
            1
        );
        p.opened("j2", "sudo apt install mc", Stdin::none());
        // The other fact, in the other sentence: the run is still writing, so the sentence may
        // not claim the beat it never earned - and it has to say the one thing the person cannot
        // get from their own screen, because a `!` line's output does not land until the run ends.
        p.unreadable("j2", false);
        let all: Vec<String> = events(&hub)
            .into_iter()
            .filter_map(|e| match e {
                SessionEvent::Warning { detail, .. } => Some(detail),
                _ => None,
            })
            .collect();
        assert_eq!(all.len(), 2, "a second run gets its own sentence: {all:?}");
        assert!(
            all[1].contains("is still writing output"),
            "a run that is writing is told THAT, not told it was quiet: {all:?}"
        );
        assert!(
            !all[1].contains("has been quiet for a beat"),
            "the sentence claims a beat the run did not have: {all:?}"
        );
        assert!(
            all[1].contains("!send"),
            "and the way in is named on this path too: {all:?}"
        );
    }

    /// An unreadable run offers the card as well as the sentence.
    ///
    /// The card's raise reason is not *the run is blocked reading its terminal* - a reading - but
    /// *a run of the operator's own is in flight*, which is a fact this object holds. The sentence
    /// stays beside it: `!send` is the floor, and the offer is added to the floor rather than in
    /// place of it.
    #[test]
    fn an_unreadable_run_offers_the_card_beside_the_sentence() {
        let hub = a_hub();
        let p = Prompts::new("p-1", hub.clone());
        p.opened("j1", "sudo apt install mc", Stdin::none());
        p.unreadable("j1", true);
        assert!(
            events(&hub)
                .into_iter()
                .any(|e| matches!(e, SessionEvent::PromptRequested { .. })),
            "the run is in flight and this daemon holds its stdin: the offer is a fact this \
             daemon has, not a guess about the process"
        );
        assert!(
            events(&hub).into_iter().any(|e| matches!(
                e,
                SessionEvent::Warning { code, .. } if code == "operator_run_unreadable"
            )),
            "and the sentence stays: `!send` is the floor under the offer"
        );
    }

    /// Answering the offer brings it back, because the run is still in flight and still
    /// unlookable - and `apt` asks more than one question.
    ///
    /// The tool reports `Unreadable` once per run, so the re-offer is the daemon's own act, made
    /// only when what was answered was a card: a `!send` is the act of somebody who has already
    /// chosen the verb. The stdin is a real pipe to a real `cat`, because the re-offer only
    /// happens after a write that succeeded.
    #[test]
    fn answering_the_offer_brings_it_back_and_a_send_does_not() {
        use std::process::{Command, Stdio};
        let mut cat = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("a cat to write to");
        let pipe = cat.stdin.take().expect("the pipe");
        let stdin = Stdin::pipe(pipe);
        let offers = |hub: &Arc<Hub>| {
            events(hub)
                .into_iter()
                .filter_map(|e| match e {
                    SessionEvent::PromptRequested { req_id, .. } => Some(req_id),
                    _ => None,
                })
                .collect::<Vec<String>>()
        };

        // Run one: a `!send` answers the offer and raises nothing. The manual verb keeps the
        // screen it chose - it settles the card that was up, and does not bring one back.
        let hub = a_hub();
        let p = Prompts::new("p-1", hub.clone());
        p.opened("j1", "sudo apt install mc", stdin.clone());
        p.unreadable("j1", true);
        assert_eq!(offers(&hub).len(), 1, "the offer was raised");
        p.send("p-1", None, "n")
            .expect("the line went into the pipe");
        assert_eq!(
            offers(&hub).len(),
            1,
            "a `!send` must not be answered with a card"
        );
        p.ended("j1");

        // Run two: a card's answer settles that card and offers the next one immediately - a
        // different `req_id`, so a stale answer for the first cannot reach the second.
        p.opened("j2", "sudo apt install mc", stdin);
        p.unreadable("j2", true);
        let first = offers(&hub).last().cloned().expect("the offer was raised");
        let settled = p
            .send("p-1", Some(&first), "y")
            .expect("the answer went into the pipe");
        assert_eq!(settled.as_deref(), Some(first.as_str()));
        let got = offers(&hub);
        assert_eq!(got.len(), 3, "the offer came back: {got:?}");
        assert_ne!(got[1], got[2], "a fresh request, not the old one");
        // And the old card can no longer answer anything.
        let refused = p
            .send("p-1", Some(&first), "y")
            .expect_err("the answered card is spent");
        assert!(refused.contains("nothing was waiting"), "{refused}");

        // And the run ending takes the re-offered card down, so nobody types into a program that
        // has exited.
        p.ended("j2");
        let settled = events(&hub)
            .into_iter()
            .filter_map(|e| match e {
                SessionEvent::PromptSettled { req_id, sent, by } => Some((req_id, sent, by)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            settled.iter().any(|(id, sent, _)| id == &got[2] && !sent),
            "the re-offered card came down with the run: {settled:?}"
        );
        let _ = cat.kill();
        let _ = cat.wait();
    }

    /// A run that ends takes its card down, and the settlement says nobody answered it - without
    /// which a person would type an answer into a program that had already exited.
    #[test]
    fn the_run_ending_takes_the_card_down() {
        let hub = a_hub();
        let p = Prompts::new("p-1", hub.clone());
        p.opened("j1", "apt install mc", Stdin::none());
        p.asking("j1", Some("Continue? [Y/n]"));
        let req = events(&hub)
            .into_iter()
            .find_map(|e| match e {
                SessionEvent::PromptRequested { req_id, .. } => Some(req_id),
                _ => None,
            })
            .expect("a card was raised");
        p.ended("j1");
        let settled = events(&hub)
            .into_iter()
            .find_map(|e| match e {
                SessionEvent::PromptSettled { req_id, sent, by } => Some((req_id, sent, by)),
                _ => None,
            })
            .expect("the card was settled");
        assert_eq!(settled.0, req);
        assert!(!settled.1, "nothing was sent — the command ended");
        assert!(
            settled.2.contains("ended"),
            "the settlement says why: {}",
            settled.2
        );
        // And a second `ended` for the same job is a no-op rather than a second settlement.
        p.ended("j1");
        let settlements = events(&hub)
            .into_iter()
            .filter(|e| matches!(e, SessionEvent::PromptSettled { .. }))
            .count();
        assert_eq!(settlements, 1);
    }

    /// A stale card cannot answer a later command, and it is refused in a sentence rather than
    /// silently.
    ///
    /// The whole of why the card carries a `req_id`: a card raised for `apt` and answered after
    /// `apt` died, while something else runs, is a line written into the wrong program's stdin.
    /// The manual `!send` is the shape that does mean *whatever is running*, which is why it
    /// passes `None` - and it is refused too when nothing is.
    #[test]
    fn a_card_for_a_run_that_is_gone_cannot_answer_the_next_one() {
        let hub = a_hub();
        let p = Prompts::new("p-1", hub.clone());
        p.opened("j1", "apt install mc", Stdin::none());
        p.asking("j1", Some("Continue? [Y/n]"));
        let old = events(&hub)
            .into_iter()
            .find_map(|e| match e {
                SessionEvent::PromptRequested { req_id, .. } => Some(req_id),
                _ => None,
            })
            .expect("a card was raised");
        p.ended("j1");
        p.opened("j2", "sleep 300", Stdin::none());

        let refused = p
            .send("p-1", Some(&old), "Y")
            .expect_err("the old card must not answer the new run");
        assert!(refused.contains(&old), "{refused}");
        assert!(
            refused.contains("nothing was waiting"),
            "the refusal says what happened: {refused}"
        );
        // And with nothing running at all, the manual form is refused with its own sentence.
        p.ended("j2");
        let nothing = p.send("p-1", None, "Y").expect_err("nothing is running");
        assert!(nothing.contains("no command of yours"), "{nothing}");
        // The driver is for one session, and it says so rather than writing somewhere else.
        let wrong = p
            .send("p-2", None, "Y")
            .expect_err("another session's send must not reach this run");
        assert!(wrong.contains("p-2"), "{wrong}");
    }

    /// `opened` replaces a run, and settles the card the old one left up.
    ///
    /// A session runs one operator command at a time, so reaching `opened` with a run already held
    /// means the previous one's `Ended` never arrived. A stale handle behind a live card is the
    /// worst state this object can be in: a line typed for `apt` written into whatever started
    /// next.
    #[test]
    fn a_second_run_settles_the_first_ones_card() {
        let hub = a_hub();
        let p = Prompts::new("p-1", hub.clone());
        p.opened("j1", "apt install mc", Stdin::none());
        p.asking("j1", Some("Continue? [Y/n]"));
        p.opened("j2", "sleep 300", Stdin::none());
        let settled = events(&hub)
            .into_iter()
            .filter_map(|e| match e {
                SessionEvent::PromptSettled { sent, by, .. } => Some((sent, by)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(settled.len(), 1);
        assert!(!settled[0].0);
        assert!(
            settled[0].1.contains("a new command started"),
            "{settled:?}"
        );
        // And the new run can raise its own card.
        p.asking("j2", None);
        assert!(
            events(&hub)
                .into_iter()
                .any(|e| matches!(e, SessionEvent::PromptRequested { .. })),
            "the new run's card is raised"
        );
    }
}
