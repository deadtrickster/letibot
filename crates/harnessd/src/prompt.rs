//! **The daemon's half of answering the operator's own command** — who holds the run's input,
//! and how a line gets into it.
//!
//! # What this module is
//!
//! [`letibot_sessionlog::PromptDriver`] is the seam: a trait the *server* can call on the
//! connection's thread, implemented here because this is the crate with the daemon in it.
//! [`Prompts`] is the implementation, and it is small on purpose — the mechanism is
//! [`letibot_tools::exec::Stdin`], which is the handle the exec host took on the run's own
//! **input** — its terminal on the ordinary path, and a pipe only on a box where no pty opens —
//! and the detection is [`letibot_tools::exec::ask`], which reads
//! `/proc` and never the program's words. What is here is the three things that are *the
//! daemon's*:
//!
//! | | |
//! |---|---|
//! | **the run** | the one operator command this session has in flight: its job, the command the person typed, and the input handle |
//! | **the card** | the open `req_id` for that run, minted here and published as [`SessionEvent::PromptRequested`] |
//! | **the write** | [`PromptDriver::send`], which is what a head's answer and a `!send` both become |
//! | **the silence** | [`Prompts::unreadable`], for the run this daemon may not look at — see the report itself |
//!
//! # Why the state is here and not in the harness
//!
//! **The session worker is blocked inside the very command that is asking.** `run_operator_shell`
//! calls `invoke_operator`, which waits on the job — so by the time a card is up, the thread
//! that owns the exec host is inside the wait and cannot be asked anything. The server has to
//! reach the input without it, and this is the object it reaches: created by the harness at
//! open (it is the half that has the handle), handed to the registry
//! ([`Registry::set_prompt`](letibot_sessionlog::Registry::set_prompt)) keyed by the session,
//! and driven from two threads at once.
//!
//! # One per SESSION, and the reason is not tidiness
//!
//! An input handle belongs to one session's run. Two sessions on one daemon each have their own, and a
//! `!send` in one must never write into the other's command — so the registry looks the
//! driver up by session id and [`Prompts::send`] checks the id it was given against its own.
//! That check is not defensive: it is the assertion that the lookup was the right one.
//!
//! # The reports, and which thread each comes from
//!
//! * [`Prompts::opened`] and [`Prompts::ended`] come from the **worker**, on the tool's own
//!   thread, around the wait. `opened` is what makes the manual way in work when no card is
//!   ever raised; `ended` is what takes a card down, so nobody types an answer into a program
//!   that has already exited.
//! * [`Prompts::asking`] comes from the same thread, once per question, when
//!   [`letibot_tools::exec::ask`] says the run is **blocked reading the terminal this daemon
//!   holds**.
//! * [`Prompts::unreadable`] comes from the same thread too, once per run, when `ask` says the
//!   opposite of a reading: **it could not look** — a process of the run belongs to another
//!   uid, which is every `! sudo …` that reaches a program running as root. There is no card
//!   to raise on that, and this is the sentence the person is owed instead: *I cannot tell,
//!   and `!send` is the way in.* See the method: the operator's own report is the state this
//!   exists for.
//! * [`PromptDriver::send`] comes from the **server's reader thread**, because the answer is
//!   not an act on the session's timeline — it is the second half of one already in flight.
//!   See the trait for why the queue cannot carry it.
//!
//! # What this module deliberately does not do
//!
//! * **No secret.** There is no method here that takes a password or returns one, and
//!   [`Prompts`] holds nothing a password could travel on. `SUDO_ASKPASS` and the `askpass`
//!   head keep their own path, and the two must not be mergeable by a later edit — see
//!   `letibot_sessionlog`'s `PromptDriver` for the half of that which is a *shape*.
//! * **No text matching.** Whether a run is waiting is [`letibot_tools::exec::ask`]'s answer
//!   and it is about the process. The last line the program wrote is carried here to be
//!   **shown** and is never looked at.
//! * **No blocking under the lock.** [`Prompts::send`] clones the handle out and writes with
//!   the lock released: the write is a syscall on the run's input, and the run's own wait loop wants
//!   this lock to report the next question.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::{PromptDriver, SessionEvent};
use letibot_tools::exec::Stdin;

/// **One operator command, in flight.**
struct Run {
    /// The job's handle, as `job_list` spells it. The key that ties the three reports
    /// together: a report about a job that is not this run's is about something else.
    job: String,
    /// The command as the operator typed it, verbatim. Carried on the card, because the
    /// daemon cannot know which process in a pipeline asked — `sudo apt install mc` is three
    /// programs and the question is the third one's — so the card names the one thing that
    /// is certain.
    command: String,
    /// **The way in.** A handle on the run's input, cloned out of the job — its own terminal on
    /// the ordinary path. See [`letibot_tools::exec::Stdin`].
    stdin: Stdin,
    /// The card that is up for this run, if any. `None` between the run starting and the
    /// first question, and again after one is answered.
    req: Option<String>,
    /// **Whether the daemon has already said it cannot tell whether this run is waiting.**
    /// Once per run: the report is about a condition that holds for as long as the run does,
    /// and a sentence per beat would be a sentence nobody reads. See [`Prompts::unreadable`].
    unreadable_said: bool,
}

/// See the module header.
pub struct Prompts {
    /// The session this driver is for, and the check in [`PromptDriver::send`].
    session: String,
    /// Where a card is raised and a settlement recorded. The hub is the session's own, so a
    /// `PromptRequested` reaches every head attached to *this* session and no other.
    hub: Arc<Hub>,
    /// The one run, or `None` when nothing of the operator's is in flight.
    inner: Mutex<Option<Run>>,
    /// Mints request ids. Per driver and not per daemon: an id is only ever compared against
    /// one run's own open card, so the session's name is the whole of the uniqueness that
    /// matters, and a daemon-wide counter would make two sessions' ids look related.
    next: AtomicU64,
}

impl Prompts {
    /// **A driver for one session.** Called by the session's own harness at open — the half
    /// that owns the exec host, and so the half that has the handle.
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

    /// **The operator's own command has started, and it has a stdin.** See [`OperatorRun`]'s
    /// counterpart in `letibot_tools::runtime` for why this report exists at all: it is what
    /// makes `!send` work **when no card is ever raised**, which is the miss this whole
    /// feature is allowed to have because a person can still answer.
    ///
    /// **A new run replaces the old one**, and a card left up for the old one is settled
    /// first. A session runs one operator command at a time, so reaching here with a run
    /// already held means the previous one's `Ended` did not arrive — a path this does not
    /// know about, or a panic — and a stale handle behind a live card is the worst state this
    /// object can be in: a line typed for `apt` written into whatever started next.
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
        });
    }

    /// **The run looks like it is waiting for an answer**, by `letibot_tools::exec::ask`'s
    /// reading of the process — never of its words.
    ///
    /// `question` is the last line the program wrote, carried to be **shown**. It is
    /// `Option` because a program blocked before its first byte is a real case (`! cat`), and
    /// *nothing to show* is a different thing from an empty line.
    ///
    /// **One card per run at a time.** The tool already refuses to report the same question
    /// twice, and this refuses independently: a run that is blocked stays blocked, and a
    /// second card for it would be a second field on the screen for one question.
    pub fn asking(&self, job: &str, question: Option<&str>) {
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
        });
    }

    /// **This daemon cannot tell whether the run is waiting for a line**, and it says so.
    ///
    /// # Why this is a sentence and not a card
    ///
    /// The card is raised on a **reading of the process** — `letibot_tools::exec::ask` — and
    /// the whole reason it is a reading rather than a guess is the operator's own correction:
    /// *"i think `Continue?` is an overfit"*. [`OperatorRun::Unreadable`] is the third answer,
    /// *"I could not look"*: one process of the run belongs to a uid this daemon is not (the
    /// `sudo` case), or a `/proc` a confined session's daemon may not open. A card raised on
    /// it would be a guess, and a wrong one for every long quiet command that is not asking
    /// anything — so no card, which is what `ask`'s miss 4 already ruled.
    ///
    /// # What the person was owed instead
    ///
    /// *Something*. The operator's report — `! sudo apt install mc`, *"after entering the
    /// sudo password, the command appears queued and the daemon hangs"* — is exactly this
    /// state with nothing said about it: `apt` waits at `Continue? [Y/n]` as root, where
    /// `/proc/<pid>/fd/0` is `EACCES` for the daemon, so no card can be raised and the run
    /// holds the daemon's one worker until its deadline. The person has no way to learn that
    /// the command is waiting at all.
    ///
    /// So the daemon says the one thing that is true and the one thing that helps: it cannot
    /// tell, and **`!send` is the way in** — the verb that needs no signal, which the card's
    /// own docs already name as the floor under every miss the heuristic has. The command is
    /// named because the person typed it, and the job is named because `job_output` reads it.
    ///
    /// **Once per run.** A condition, not an event: the run stays unreadable for as long as it
    /// lasts, and one sentence per beat would be a red block nobody reads.
    ///
    /// # And it says WHICH of the two facts it is
    ///
    /// `quiet` is the beat the run had — or had not — when the tool reported. The two facts are
    /// different things to a person, and one sentence for both would be the defect the askpass
    /// deadline's own line carries (*"no head answered before the deadline, or the person
    /// refused"*, one sentence for two facts and wrong for one of them):
    ///
    /// * **quiet** — the run has written nothing for a beat, so it may well be *waiting*, and
    ///   the sentence says so;
    /// * **still writing** — the run is producing output, so it is either working or blocked
    ///   with something still drawing, and **this daemon cannot tell the two apart**. That is
    ///   the case the operator's second report is about: `! sudo apt install mc` on a fresh
    ///   daemon, where `apt` streamed its progress and nothing was said to them at all.
    ///
    /// The second sentence has to do more work than the first, and the extra is the fact the
    /// person cannot get anywhere else: **a `!` line's own output does not land until the run
    /// ends**, so a command that is working and a command that is hung look exactly the same on
    /// their screen. Saying *still writing* is what makes the difference legible, and saying
    /// nothing — which is what the beat used to do — leaves them to guess.
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
            run.command.clone()
        };
        // **The two facts, in two sentences.** The first clause is the whole of what differs:
        // a run that has been quiet may be waiting, and a run that is still writing is either
        // working or blocked with something drawing. Everything after it — that `/proc` refuses,
        // that `!send` is the way in, that the worker is held — is true of both and is said once.
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
                 Until the command ends it holds this daemon's worker, so nothing else of yours \
                 runs either."
            ),

            compaction: None,
        });
    }

    /// **The run is over.** The card comes down, and the handle is forgotten.
    ///
    /// `sent: false` with a sentence in `by` rather than an identity: nothing was sent and
    /// nobody is being named for it — the command ended, which is the ordinary ending of
    /// every run that was never answered.
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
                // nothing is right in both cases, and taking the run anyway would be this
                // object clearing a live handle on a stale message.
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
    /// **Write one line to this session's own running command.** See the trait for the two
    /// shapes of `req` and for why nothing here may block.
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
        // **The lock is released before the write.** `send_line` is a blocking syscall on the
        // run's input, and the run's own wait loop wants this lock to report the next question — so
        // holding it across the write would let one unread line stall the very loop that
        // raises the card.
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
        // The card comes down, if it is still the one this answered. Re-locked rather than
        // held: see above.
        {
            let mut g = self.lock();
            if let Some(run) = g.as_mut()
                && run.req == settled
            {
                run.req = None;
            }
        }
        Ok(settled)
    }
}

impl std::fmt::Debug for Prompts {
    /// By what it is holding, and **never the line or the command**: a driver that printed
    /// its state would be a second place a person's answer could reach.
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

    /// **A card is raised once per run, and it names the operator's own command.**
    ///
    /// The second `asking` for the same run is refused here as well as in the tool, and the
    /// reason is the same in both places: a run that is blocked stays blocked, so a card per
    /// tick would be a hundred fields on the screen for one question.
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

    /// **A run this daemon cannot look at is said out loud, once — and says WHICH fact it is.**
    ///
    /// The operator's report, as an assertion: `! sudo apt install mc`, the password given,
    /// and then *nothing* — `apt` waits at `Continue? [Y/n]` as root, `/proc/<pid>/fd/0` is
    /// `EACCES` for the daemon, so no card can be raised (a card is a reading and this is not
    /// one) and the run holds the worker until its deadline. The sentence is the whole of what
    /// the person is owed there, and it has to name the way in: **`!send`**, the verb that
    /// needs no signal at all.
    ///
    /// **Once per run**, and that is the assertion this test exists for beside the wording: a
    /// condition reported per beat is a red block nobody reads, which is the failure mode the
    /// warning register is written against.
    ///
    /// **And the two facts are two sentences.** The operator's second report is a fresh daemon
    /// where `apt` *streamed its progress* and nothing was said at all, because the report was
    /// gated behind the beat the card needs and a streaming run never has one. So the report is
    /// made either way now, and the sentence has to say which of the two it is: a run that has
    /// gone quiet may well be waiting, and a run that is still writing is either working or
    /// blocked with something drawing. One sentence for both would be the defect the askpass
    /// deadline's line has — *"no head answered before the deadline, or the person refused"*.
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
        // **No card.** This is the other half of the ruling and the reason the report is a
        // Warning: `ask::Waiting::Unreadable` is *I could not look*, and a card raised on it
        // would be a guess — wrong for every long quiet command that is not asking anything.
        assert!(
            !events(&hub)
                .into_iter()
                .any(|e| matches!(e, SessionEvent::PromptRequested { .. })),
            "an unreadable run raises no card: a card is a reading of the process"
        );
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
        // **The other fact, in the other sentence.** The run is still writing, so the sentence
        // may not claim the beat it never earned — and it has to say the one thing the person
        // cannot get from their own screen, which is that the command is still producing
        // something, because a `!` line's output does not land until the run ends.
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

    /// **A run that ends takes its card down**, and the settlement says nobody answered it.
    ///
    /// Without this the card would outlive the command it was about: a person would type an
    /// answer into a program that had already exited.
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

    /// **A stale card cannot answer a later command**, and it is refused in a sentence
    /// rather than silently.
    ///
    /// The whole of why the card carries a `req_id`: a card raised for `apt` and answered
    /// after `apt` died, while something else runs, is a line written into the wrong
    /// program's stdin. The manual `!send` is the shape that *does* mean *whatever is
    /// running*, which is why it passes `None` — and it is refused too when nothing is.
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

    /// **`opened` replaces a run, and settles the card the old one left up.**
    ///
    /// A session runs one operator command at a time, so reaching `opened` with a run already
    /// held means the previous one's `Ended` never arrived. A stale handle behind a live card
    /// is the worst state this object can be in: a line typed for `apt` written into whatever
    /// started next.
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
