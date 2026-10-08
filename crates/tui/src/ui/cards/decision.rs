//! **The permission card**: a call waiting on the person, its ladder of options, and what
//! the card says about how it was decided once it has been.

use crate::app::*;
use crate::ui::render::{sgr, wrap};
use crate::ui::*;
use letibot_sessionlog::view::{OpenDecision, SettledDecision};
use letibot_ui::painter::Sgr;
use letibot_ui::progress;
use letibot_ui::text::without_control_lines;
use rano::style::Role;

impl App {
    /// **The card, split where R20 says the split is.**
    ///
    /// `head-parity-2026-09-21.md` **R20**, ruled 2026-09-22 on a permission card carrying a
    /// giant replace or a commit message: *"I'm shown a permission prompt and I just cant
    /// see the selector."* The screen-fit loop in [`App::screen`] shrinks the card with
    /// `dec_rows -= 1`, which trims **from the end**, and the card was built headline,
    /// target, intents, because, advice, options, hint — so the loop ate the hint, then the
    /// options bottom-up, and kept the wall. **A card that has dropped its choices is a
    /// question with no way to answer it**, and the operator was left reading a wall at full
    /// length while the four rows they had to act on were gone.
    ///
    /// So the card has two halves and only one of them gives way:
    ///
    /// * **`content`** — the question, what it is about, and the evidence. A viewport over
    ///   this shrinks to the room that is left, and **scrolls** (`pgup`/`pgdn`), so the whole
    ///   diff or message can still be read;
    /// * **`choices`** — the ladder and what pressing it means: the deadline, the hint, the
    ///   `deny_and_tell` line. **Never trimmed and never scrolled**, because this half is the
    ///   answer.
    ///
    /// The order the card is read in does not change: content above, choices below, which is
    /// the bottom of the card and therefore the row nearest the composer.
    pub(crate) fn decision_card(&self, d: &OpenDecision, w: usize) -> (Vec<String>, Vec<String>) {
        // (helper below the method, so the rendering reads top to bottom)
        // **The question, then the thing itself, then the evidence.**
        //
        // One line used to carry all three — the sentence with the target
        // interpolated into it, layer A's verdict and intent list, and the kind —
        // and the operator's reading of it was *"no possible to see wtf was the
        // command i supposed to approve"*. A shell line inside a sentence inside a
        // taxonomy is not skimmable, and a permission an operator cannot evaluate is
        // one they approve out of fatigue, which is the whole mechanism this gate
        // exists to interrupt.
        //
        // So: the ask in yellow, the target alone and indented under it, the
        // deterministic reading dim below that. The target keeps its own lines even
        // when it wraps — a command is the one thing here worth the rows.
        let headline = match ask_without_target(&d.summary, &d.target) {
            Some(ask) => ask,
            None => d.summary.clone(),
        };
        let mut out = vec![colour(
            &self.cfg,
            sgr::YELLOW,
            &format!("? {headline} [{}]", d.kind),
        )];
        // **Whose call this is, when it is not this session's own.**
        //
        // R58's tree gives a child no head, so a subagent's gate posts its card here, to
        // the ROOT — the operator's ruling: *"who asks subagents permissions? i think they
        // should surface to the parent head all the way to the root obviously"*. Without
        // this clause the card above is indistinguishable from one this session's own
        // model raised, and **a card answered for the wrong thing is the defect**: a
        // person must be able to see which conversation they are approving before they
        // pick an option.
        //
        // **Directly under the question, not below the evidence.** It changes what is
        // being decided, so it is read on the same pass of the eye as the question; under
        // a wall of layer A's prose it would be read after the answer was already chosen.
        // The faint register, like every other clause on this card — it is attribution,
        // not a second question.
        if let Some(s) = &d.subagent {
            let said = if s.task.is_empty() {
                format!("    a subagent's call — {}", s.handle)
            } else {
                format!("    a subagent's call — {} · {}", s.handle, s.task)
            };
            for l in wrap(&without_control_lines(&said), w) {
                out.push(colour(&self.cfg, sgr::DIM, &l));
            }
        }
        if !d.target.is_empty() {
            for l in wrap(&format!("    {}", d.target), w) {
                // Bold rather than yellow: the question is yellow, and the thing
                // being asked about is not a second question.
                out.push(colour(&self.cfg, sgr::BOLD, &l));
            }
        }
        // **AND THE FILES THIS ACTION WOULD WRITE** — R35, and the one fact the classifier has
        // computed since the day it was written and nothing ever showed.
        //
        // The operator: *"it cant catch those pesky python edits"*. It can. `write_targets` resolves
        // an assigned name (`p = Path("src/syntax.rs")` … `open(p,'w').write(s)` — their own card is
        // that shape), a mode spelling, and a method whose receiver is the path; it inserted
        // `Intent::WriteFile`, resolved the regions, and **stopped there**. A `bash` call running a
        // script that rewrites a file drew a card that did not name the file.
        //
        // **At the place and in the style the single `target` already has** — indented four, bold —
        // which is the other head's rule for it and the reason is checkable rather than stylistic: a
        // reader comparing an `edit` card and a script card must not have to know which mechanism
        // produced them.
        //
        // **The count goes ABOVE the names**, because the names are content and content elides to
        // the card's viewport while the number must not: a window that hides three of five paths
        // still says five. One path gets no header, which is what keeps an `edit` card from growing
        // a line.
        //
        // **An unresolved path is drawn in the attention register**, never as a path. A write whose
        // target could not be read — `open(sys.argv[1], 'w')`, `Path.home() / name` — is the case a
        // person most needs to see, and a sentence that looks like a path is a sentence that gets
        // skimmed past.
        //
        // **Nothing is drawn when the list is empty.** Empty means *no write the scanner could
        // place*, and a card that printed *writes nothing* would be claiming a negative the
        // classifier cannot support. It is also what every head drew before this field existed, so
        // an old log replays unchanged.
        if !d.write_targets.is_empty() {
            let unresolved = d.write_targets.iter().filter(|t| t.unresolved).count();
            if d.write_targets.len() > 1 {
                out.push(colour(
                    &self.cfg,
                    sgr::DIM,
                    &format!("    {} files:", d.write_targets.len()),
                ));
            }
            for t in &d.write_targets {
                if t.unresolved {
                    out.push(colour(&self.cfg, sgr::YELLOW, &format!("    {}", t.path)));
                } else {
                    out.push(colour(&self.cfg, sgr::BOLD, &format!("    {}", t.path)));
                }
            }
            // The count of unresolved ones is a SENTENCE, not a header of its own: one says *a write
            // whose target could not be read* and two say *2 writes whose targets could not be read*,
            // which is a number a reader cannot misplace.
            if unresolved > 0 {
                out.push(colour(
                    &self.cfg,
                    sgr::YELLOW,
                    &format!(
                        "    {} whose target could not be read",
                        if unresolved == 1 {
                            "a write".to_string()
                        } else {
                            format!("{unresolved} writes")
                        }
                    ),
                ));
            }
        }
        if !d.detail.is_empty() {
            for l in wrap(&format!("  {}", d.detail), w) {
                out.push(colour(&self.cfg, sgr::DIM, &l));
            }
        }
        // **§11.7: the one sentence that joins the two statements above.**
        //
        // The headline says what the tool **declares** — `wants exec access` — and the
        // line above it is layer A's reading of the **action** — `auto (a read inside
        // the boundary)` — and nothing joined them, so *"the classifier decided this
        // needed no asking"* read as being argued with by the card going up anyway. That
        // is the most expensive shape a card can have: an operator who is asleep cannot
        // tell it apart from a question they should have been asked, and it cost three
        // 300-second refusals in one night (`head-parity` R18).
        //
        // **The missing clause is the access, and it is said only where the declaration
        // is what asks.** A `read` call is asked about because of a rule, a path or a
        // mode, and a sentence about its declaration would be false on the very card
        // carrying it — which is what `because: workspace: /` was (a fact named after one
        // thing and read from another), and the mistake this wording exists not to
        // repeat. An empty `access` is a daemon older than the field: no clause, because
        // the head does not know and will not guess.
        const ACCESS_ASKS: &str = "the access is what asks: a tool declared to `exec` is \
                                   asked about on its declaration, and the line above is a \
                                   reading of this action";
        if d.access == "exec" && d.kind != "question" {
            for l in wrap(&format!("  {ACCESS_ASKS}"), w) {
                out.push(colour(&self.cfg, sgr::DIM, &l));
            }
        }
        // **Why it is asking, in the words of whoever is stuck.**
        //
        // `because` is the one sentence on this card that is not the harness's: on a
        // question it is the model's own line about what it cannot decide without —
        // an `ask_user_question`'s *"one line: what you are stuck on and why the
        // answer changes what you do"*. The head has carried it on `OpenDecision`
        // since the field existed and **never drawn it**, so a card that asks a
        // question gave the person the question and no answer to *why are you asking
        // me this* — which is the whole reason the field is there.
        //
        // **Labelled with its speaker**, like every other borrowed sentence on this
        // card: `model says {would}: {basis}`, `oracle-local · cites …`, `because: …`.
        // An unlabelled line under `detail` is indistinguishable from more of layer
        // A's reading, and a reader who cannot tell whose sentence it is cannot weigh
        // it against their own knowledge — which is the only thing they can do with
        // somebody else's reason.
        if !d.because.is_empty() {
            for l in wrap(&format!("  because: {}", d.because), w) {
                out.push(colour(&self.cfg, sgr::DIM, &l));
            }
        }
        // **The model's verdict, above the ladder.**
        //
        // At `/mode supervised` the question is not *should this run* but *do you
        // agree with the model*, and a person cannot agree with something they were
        // not shown. It sits above the options rather than below because it is read
        // before the choice is made, and it is dim rather than yellow so it reads as
        // evidence beside the question rather than as a second question.
        //
        // Nothing here preselects an option. `self.sel` is untouched: the verdict
        // informs the answer and must never supply it, or the corpus fills with rows
        // recording a keystroke rather than a judgement.
        if let Some(a) = &d.advice {
            // **An oracle that was NOT consulted did not say anything.** `would:
            // "unavailable"` with `consulted: false` is layer A's own answer arriving
            // through layer B's door — an unresolved action, an always-ask entry, a
            // rule — and rendering it as `model says unavailable: …` claims a model
            // spoke. The distinction is the field this commit added, and the sentence
            // is the same one `letibot_tools`' own `ModelAdvice::line()` writes.
            // **R12's four non-answers, and the fifth fact that is an answer** (leticl's
            // ask). Without this field all five reached the glass as `model says ask: <prose>`
            // — one line for facts whose remedies differ: raise the ceiling, read the bytes,
            // re-ask, or accept the refusal. leticl measured it on three real frames: every
            // field a head controls was identical across them.
            //
            // **The head AUTHORS the classification; the daemon's prose follows it as the
            // detail.** That is the division the field makes possible and it is why the two
            // cannot disagree: the word is derived from a token, the sentence is the
            // server's own `basis`, and the head parses nothing.
            let said = if a.consulted {
                match a.unsure.as_deref() {
                    Some("out_of_room") => format!(
                        "  the guard ran out of room before it answered — {} (a budget, not an \
                         opinion; `--oracle-max-tokens` is the knob)",
                        a.basis
                    ),
                    Some("unreadable") => {
                        format!("  the guard's reply was not a verdict — {}", a.basis)
                    }
                    Some("could_not_decide") => {
                        format!("  the guard answered unsure — {}", a.basis)
                    }
                    Some("between_thresholds") => format!(
                        "  the guard's two scores fell between the thresholds — {}",
                        a.basis
                    ),
                    // **A token this head does not know is SHOWN, not swallowed.** A daemon
                    // that gains a fifth kind must be visible rather than silently reading as
                    // one of the four — which is the defect this whole field exists to end.
                    Some(other) => format!("  the guard did not decide ({other}) — {}", a.basis),
                    // The fifth fact: a consulted oracle that answered, and answered *no*.
                    None => format!("  model says {}: {}", a.would, a.basis),
                }
            } else {
                format!("  no model verdict — {}", a.basis)
            };
            for l in wrap(&said, w) {
                out.push(colour(&self.cfg, sgr::DIM, &l));
            }
            // **Said out loud when it is a fact, omitted when it is not one.**
            //
            // An oracle that AUTHORISED something while citing none of your words
            // is the case most worth a second look, and a blank line there reads as
            // "no note" rather than as "it could not ground this". That is the only
            // case this sentence is true about.
            //
            // It was printed unconditionally, and the citations never arrived from
            // the layer below — so a screen carried `the operator authorised this:
            // … (citing trail entry 0)` and `cites nothing from your words` one
            // line apart. The operator read the second: *"it also told that i didnt
            // mention anything while it was clear that i instructed the model to
            // use worktrees"*. An oracle that did not authorise has nothing to cite
            // and saying so about it claims a search that was never the question.
            let grounds = if !a.cites.is_empty() {
                Some(format!("cites {}", a.cites.join(" · ")))
            } else if a.would == "admit" {
                Some("cites nothing from your words".to_string())
            } else {
                None
            };
            let tail = match &grounds {
                Some(g) => format!("  {} · {g} · {} ms", a.by, a.latency_ms),
                None => format!("  {} · {} ms", a.by, a.latency_ms),
            };
            for l in wrap(&tail, w) {
                out.push(colour(&self.cfg, sgr::DIM, &l));
            }
        } else if d.kind != "question" {
            // **And when nobody was asked, the card says that too.**
            //
            // The verdict is evidence and **its absence is evidence**: under
            // `/mode supervised` the question printed above the ladder is not *should
            // this run* but *do you agree with the model*, and a card that draws
            // nothing in the place the verdict goes makes *no oracle was consulted*
            // and *the oracle was asked and said nothing* the same screen. Measured on
            // this box: an oracle answered and its verdict was unreadable, and the
            // card said so — while the card nobody had asked said exactly the same
            // nothing.
            //
            // **Only where a gate is.** A `question` is not one: nothing is consulted
            // for it by construction, so the sentence would answer a question nobody
            // asked, in the dim register a reader is taught to skim. Every other kind
            // on this card is a gate, which is the same reading leticl takes.
            //
            // The claim is exact rather than approximate: the daemon poses an
            // `advice` only when a model adjudicator was actually asked for one
            // (`adjudicate.rs::ask_the_advisor` — `None` off `/supervised` and `None`
            // with no advisor installed), and when an oracle was asked and could not
            // answer it still poses a verdict with `would: unavailable`, which draws
            // above this line and not instead of it.
            for l in wrap(
                "  no oracle was consulted for this one — the judgement is yours alone",
                w,
            ) {
                out.push(colour(&self.cfg, sgr::DIM, &l));
            }
        }
        // **R20: here the card stops being content and becomes the answer.** Everything
        // above this line goes into the viewport that shrinks and scrolls; everything
        // below is the ladder and what pressing it means, which the fit loop may not
        // touch. This is the split point and it is here, at the list, because the list is
        // what the operator has to act on — a card whose wall is cut short is a card you
        // can still answer, and a card whose choices are cut off is not a card at all.
        let mut choices: Vec<String> = Vec::new();
        // **One row per answer, with the highlighted one marked.**
        //
        // They used to be joined with `·` onto one wrapped line, which is readable but
        // is not a control: there was nothing to move and nothing to press, so the only
        // way in was to type the id. A ladder the eye can walk is also a ladder Up/Down
        // can walk, and the two have to agree -- the marker IS the thing Enter takes.
        //
        // **And the rows come from whichever field this kind keeps them in** (§1.7):
        // a question's are its `choices`, and drawing `options` for one is what left the
        // card with an empty ladder under a question. The label for a question is the
        // choice itself, with no `(option_id)` beside it, because a question's answers
        // have no ids — they are the model's prose, offered back to it as an index.
        let rows: Vec<String> = if d.kind == "question" {
            d.choices.clone()
        } else {
            d.options
                .iter()
                .map(|o| format!("{}  ({})", o.label, o.option_id))
                .collect()
        };
        for (i, body) in rows.iter().enumerate() {
            let picked = i == self.sel.min(rows.len().saturating_sub(1));
            // The id stays on the line. Typing it still works, a script still uses it,
            // and a reader learning the ladder sees both spellings of the same choice.
            for l in wrap(&format!("  {} {body}", if picked { "▸" } else { " " }), w) {
                choices.push(if picked {
                    // Inverse video rather than another colour: the prompt is already
                    // yellow, and a highlight that is a second hue reads as a second
                    // kind of thing rather than as "this one".
                    colour(&self.cfg, sgr::REVERSE, &l)
                } else {
                    colour(&self.cfg, sgr::YELLOW, &l)
                });
            }
        }
        // **§1.6: how long there is, and what silence will do.**
        //
        // Both facts were on the wire and neither was drawn, while **this head already
        // drew a countdown on the secret card** — the instrument existed and had never
        // been pointed at the gate, where the consequence of silence is a decision rather
        // than a missing password. The operator was bitten by exactly that: two cards
        // timed out at 300 s with `not_run by gate:timeout`, and nothing on either card
        // had said that was coming.
        //
        // **Below the options, because that is the order a person reads**: the question,
        // the choices, then what happens if they do nothing. And `on_timeout` is drawn
        // only where there is a deadline — `null` is §11.5's *wait forever*, a policy
        // rather than missing information, so there is no silence for a clause to
        // describe. That is §13.2b answered the other way round from `unreadable 0` on
        // `/status`, and deliberately: that one IS shown at zero because a head that
        // does not count unreadable frames is a different head, while a countdown on an
        // ask with no deadline is a number nobody took.
        if let Some(deadline) = d.deadline {
            let mut said: Vec<String> = Vec::new();
            said.push(match deadline.checked_sub(self.now_ms) {
                Some(left) => progress::countdown(left),
                // **A card still on the screen after its own deadline.** That is one the
                // daemon has already settled and this head has not been told the outcome
                // of, so the sentence says the clock ran out and the outcome is
                // unreported — and it must **never count into negative seconds**, which
                // is a number that means nothing and reads as a rendering fault.
                None => format!(
                    "past its deadline by {}s; the daemon has not said what became of it",
                    (self.now_ms - deadline) / 1000
                ),
            });
            if let Some(clause) = timeout_clause(&d.on_timeout) {
                said.push(clause.to_string());
            }
            for l in wrap(&format!("  {}", said.join(" · ")), w) {
                choices.push(colour(&self.cfg, sgr::DIM, &l));
            }
        }
        // The glob line is only shown when an *always allow* is actually on offer.
        // A hint for an option this request does not have is an affordance that does
        // nothing, which teaches the operator to stop reading the hints.
        //
        // **A question's hints are its own** (§1.7). *"or type the id"* names
        // something a question does not have — its rows are the model's prose, not ids
        // — and the whole point of the free field is that a typed line is an answer
        // rather than a mistake to be corrected.
        let hint = if d.kind == "question" {
            "  ↑↓ to choose · Enter to answer · or type your own answer"
        } else if d
            .options
            .iter()
            .any(|o| o.kind == letibot_sessionlog::event::OptionKind::AllowAlways)
        {
            // The rule the *Always allow* answer will write is in the option's
            // own label now, so the hint points at editing it rather than at
            // inventing it: the operator's report (2026-09-18) was that the
            // offer never said which tool and verb it would permit, and a
            // pattern they cannot see is a pattern they cannot adjust.
            "  ↑↓ to choose · Enter to answer · or type the id ·              `allow_always <glob>` to widen or narrow the rule shown above"
        } else {
            "  ↑↓ to choose · Enter to answer · or type the id"
        };
        choices.push(colour(&self.cfg, sgr::YELLOW, hint));
        // **The option that asks for words says where to type them.** Its label
        // promised *"tell the model why"* and the card never said how — so the
        // why was typed into the composer, refused by `match_option`, and left
        // sitting there while nothing was answered.
        if d.options
            .iter()
            .any(|o| o.kind == letibot_sessionlog::event::OptionKind::RejectAlways)
        {
            choices.push(colour(
                &self.cfg,
                sgr::YELLOW,
                "  `deny_and_tell <why>` denies and sends those words to the model",
            ));
        }
        (out, choices)
    }

    /// The whole card as one list, for the callers that want it whole: the fit loop in
    /// [`App::screen`] does not, because R20 splits it there, so this is the transcript's
    /// shape and the tests' entry point rather than the drawing path.
    pub(crate) fn decision_lines(&self, d: &OpenDecision, w: usize) -> Vec<String> {
        let (mut content, choices) = self.decision_card(d, w);
        content.extend(choices);
        content
    }

    /// **The card's content as a window** (R20).
    ///
    /// `room` is how many rows the viewport may occupy, seam included; returns the lines to
    /// draw — the seam and the window — and the scroll actually used.
    ///
    /// The seam is the whole of the disclosure, so it is written as one: **how many lines
    /// are out of view** and which key moves toward them. It is never "there is more" — a
    /// reader who cannot see the rest has to know whether one line or four hundred are
    /// missing before deciding whether to scroll at all, which is the same rule
    /// `OutputSlice::denominator` keeps for a job's output.
    ///
    /// **The window starts at the head**, because that is where a card is read from: the
    /// question and what it is about are the first lines, and `pgdn` walks down into the
    /// wall. The seam changes ends with the scroll — above the window once there is nothing
    /// left below it — so the sentence is always the boundary the reader is looking at.
    ///
    /// The scroll is clamped here rather than in the key handler, for the reason the panes
    /// clamp in `pane_window`: the drawn length is a function of the width and the fold, and
    /// the key handler knows neither.
    pub(crate) fn card_window(
        &mut self,
        _w: usize,
        content: &[String],
        room: usize,
    ) -> (Vec<String>, usize) {
        self.dec_content_len = content.len();
        if content.is_empty() || room == 0 {
            self.dec_content_room = 0;
            self.dec_scroll = 0;
            return (Vec::new(), 0);
        }
        if content.len() <= room {
            // `room` counts the content lines the viewport actually showed, seam excluded,
            // so "nothing is out of view" is `len <= room` in both places that ask.
            self.dec_content_room = content.len();
            self.dec_scroll = 0;
            return (content.to_vec(), 0);
        }
        // One row of the viewport is the seam, and it is spent even at `room == 1` — a
        // viewport whose whole height is the sentence saying how much is missing is the
        // honest shape of a screen with nowhere to put the content.
        let shown = room - 1;
        self.dec_content_room = shown;
        let max = content.len() - shown;
        let at = self.dec_scroll.min(max);
        self.dec_scroll = at;
        let above = at;
        let below = content.len() - (at + shown);
        let seam = if below == 0 {
            dim(
                &self.cfg,
                &format!("  … {above} line(s) out of view · pgup scrolls"),
            )
        } else if above == 0 {
            dim(
                &self.cfg,
                &format!("  … {below} line(s) out of view · pgdn scrolls"),
            )
        } else {
            dim(
                &self.cfg,
                &format!("  … {above} above, {below} below · pgup/pgdn scrolls"),
            )
        };
        let mut out: Vec<String> = Vec::with_capacity(room);
        if below == 0 {
            out.push(seam.clone());
        }
        out.extend(content[at..at + shown].iter().cloned());
        if below != 0 {
            out.push(seam);
        }
        (out, at)
    }
}

/// **What silence does, in the daemon's own three words** (§1.6).
///
/// The order a person reads a card in is the question, the choices, then what happens
/// if they do nothing — and until this existed the third of those was unanswerable
/// from the screen. The operator: *"two gate cards timed out unanswered at 300 seconds
/// with `not_run by gate:timeout` — nothing on the card had said that was coming."*
///
/// ```text
///   deny  -> if nobody answers, nothing runs
///   allow -> if nobody answers, it RUNS anyway
///   ask   -> if nobody answers, the guard model decides
/// ```
///
/// **The upper case on `RUNS` is the point of the line.** It is the only one of the
/// three that does something nobody asked for, and an operator who walked away
/// believing the default was `deny` when it was `allow` has been told nothing at all by
/// a clock.
///
/// # The unknown-word case has nothing to catch, and that is worth saying
///
/// leticl's spec adds *"a word this head does not know draws no clause rather than a
/// guess"*, and this head cannot reach that: `OnTimeout` is an **enum on the wire**, so
/// a fourth value is not a word this function fails to recognise — it is a frame the
/// deserializer refuses, which lands in the `unreadable` bucket with everything else
/// this build cannot read (R3). The fallback the requirement asks for is therefore
/// `None` here and a counted frame one layer down, and saying which is which is the
/// part that matters: a wrong consequence is worse than an absent one, and neither is
/// silence.
pub(crate) fn timeout_clause(on: &letibot_sessionlog::event::OnTimeout) -> Option<&'static str> {
    use letibot_sessionlog::event::OnTimeout as O;
    match on {
        O::Deny => Some("if nobody answers, nothing runs"),
        O::Allow => Some("if nobody answers, it RUNS anyway"),
        O::Ask => Some("if nobody answers, the guard model decides"),
    }
}

/// The ask with its target taken off the end: `` `bash` wants exec access `` from
/// `` `bash` wants exec access to `cargo test` ``.
///
/// The daemon sends both the sentence and the target, and the sentence is the one
/// every other reader of the log already has — the audit row, the denial notice, a
/// second head. Rather than change what that sentence is, the head that wants to
/// lay the two out separately takes the target back off. `None` when the sentence
/// does not end in the target, which is the honest answer for a summary some other
/// builder wrote: then the whole sentence is shown and nothing is lost.
pub(crate) fn ask_without_target(summary: &str, target: &str) -> Option<String> {
    if target.is_empty() {
        return None;
    }
    let head = summary.strip_suffix(&format!("`{target}`"))?;
    // " to " is the joint in every sentence this builder writes; trimming it is what
    // makes the remainder read as a heading rather than as a clipped sentence.
    let head = head.trim_end();
    Some(head.strip_suffix(" to").unwrap_or(head).to_string())
}

/// The decision a settled call was gated by, in the dim register: the approval is
/// a fact about the call, not a stray note. Folded it is one line — who decided
/// and how; open it adds what the oracle was shown and what it said back. Shared
/// by the one-line (inline) and the folded arms, because a gated call whose result
/// fit on the header is no less gated for it.
pub(crate) fn decision_lines(
    d: &letibot_sessionlog::view::SettledDecision,
    tools: Fold,
    w: usize,
    p: rano::style::Palette,
) -> Vec<String> {
    use letibot_sessionlog::event::DecisionOutcome as O;
    let mut out = Vec::new();
    let word = match &d.outcome {
        O::Selected { option_id } if option_id.starts_with("allow") => "allowed",
        O::Selected { .. } => "refused",
        O::Cancelled => "cancelled",
        O::TimedOut => "not answered",
    };
    let who = if d.by.identity.is_empty() {
        d.by.kind.clone()
    } else {
        format!("{} {}", d.by.kind, d.by.identity)
    };
    out.push(p.painted(Role::Faint, &format!("  · {word}, by {who}")));
    if tools.is_open() {
        for l in decision_detail(d, w.saturating_sub(4)) {
            out.push(p.painted(Role::Faint, &format!("    {l}")));
        }
    }
    out
}

/// **The two reasons a settled decision carries, labelled as whose they are.**
///
/// `basis` is the DECIDER's — for an operator answer, `dead chose `allow_once` at
/// the head`. `advice` is the guard model's, and only exists when one was
/// consulted. They used to be one line, rendered as `oracle: {basis}`, which under
/// `/supervise` printed the operator's own words under the oracle's name.
///
/// One function so the card and the settled row cannot label them differently.
/// Returns wrapped, unpainted lines; each caller indents and paints its own way.
pub(crate) fn decision_detail(d: &SettledDecision, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    // **§3.1, and this is the last of the untrusted free text on a row.** Three
    // sentences here are somebody else's: the ask the daemon wrote, the DECIDER's
    // basis (a person's words, or a policy rule), and the guard model's verdict with
    // the operator's phrases it cites. All of them end up in a row `paint_full`
    // writes verbatim, so all of them are sanitised before they are wrapped.
    //
    // One `without_control_lines` per line rather than per field, because the
    // `format!` is where the string is built and therefore where the escaping has to
    // happen — sanitising the inputs would leave the separators unguarded and reads
    // as if the format string were trusted, which is the habit this whole item is
    // against.
    if !d.summary.is_empty() {
        out.extend(wrap(
            &without_control_lines(&format!("asked: {}", d.summary)),
            w,
        ));
    }
    if !d.basis.is_empty() {
        // Named by the decider's own kind, so "decided:" never stands in for a
        // model when a person chose, or the reverse.
        let who = if d.by.kind.is_empty() {
            "decided"
        } else {
            &d.by.kind
        };
        out.extend(wrap(
            &without_control_lines(&format!("{who}: {}", d.basis)),
            w,
        ));
    }
    match &d.advice {
        Some(a) => {
            // **The same distinction the card draws**: a verdict from an oracle that was
            // asked, and layer A's answer from one that was not.
            out.extend(wrap(
                &without_control_lines(&if a.consulted {
                    format!(
                        "oracle ({}, {}ms) would {}: {}",
                        a.by, a.latency_ms, a.would, a.basis
                    )
                } else {
                    format!("no model verdict — {}", a.basis)
                }),
                w,
            ));
            // **Empty cites is loud.** An authorisation the oracle could not ground
            // in anything the operator said is a different fact from one it grounded
            // in four utterances, and rendering nothing for the first makes them
            // look the same.
            if a.cites.is_empty() {
                out.extend(wrap(
                    "oracle cited: nothing — it could not ground this in anything you said",
                    w,
                ));
            } else {
                for c in &a.cites {
                    out.extend(wrap(
                        &without_control_lines(&format!("oracle cited: {c}")),
                        w,
                    ));
                }
            }
        }
        // Said out loud rather than left blank: "no oracle was asked" and "an
        // oracle was asked and said nothing" are different, and a blank looks
        // like the second.
        None => out.extend(wrap("no oracle was consulted for this one", w)),
    }
    out
}
