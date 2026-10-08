//! **The composer's behaviour**: sending what was typed, completing a slash verb, a path or a
//! shell command, and the prompt history Up recalls.

use super::*;
use letibot_transcript::{TranscriptItem, UserPart};

impl App {
    /// What a submitted line means: a command, an answer to an open decision, or
    /// a prompt.
    pub(crate) fn submit(&mut self, text: String) -> Option<Action> {
        // **A pasted block is not a command, and this is checked before anything parses it.**
        // The composer is one line; the only way a newline gets here is a paste, and a paste
        // whose first line starts with `!` used to become one `!` command whose newlines the
        // shell then split — MEASURED 2026-10-08: a copied `operator_run_unreadable` notice
        // re-ran an earlier `sudo` with the notice's remaining words as its arguments. The
        // rule lives in `sessionlog` so the composer and the daemon cannot disagree about it;
        // the text is KEPT, because it is the person's and their next move is to take the
        // command out of it.
        if let Some(why) = letibot_sessionlog::operator_line_refusal(&text) {
            self.set_composer(&text);
            self.say(&why);
            self.redraw = true;
            return None;
        }
        // **`!term` is checked before the bare `!`**, because `!term mc` is also a perfectly
        // good `!` line — `operator_shell_command` reads it as the command `term mc`, which is
        // a program nobody has. The verb has to be taken first, and the parse is the daemon's
        // own function so the two halves cannot disagree about what a `!term` line is.
        //
        // **No gate, no card, no ladder** — the same sentence `!` gets, for the same reason:
        // it is the operator's own act. The difference from `!` is that this appends no row: a
        // pane is not a transcript item, it is the conversation's rectangle given to a
        // program, and when it closes the transcript is exactly what it was. So the line does
        // not join `pending_prompts` either — there is no `User` row coming to retire it, and
        // an echo that waited for one would wait for ever.
        //
        // **And the verb with nothing after it is not refused any more — it attaches.** It was
        // a sentence (*"`!term` needs a command to run"*), and the operator's own report is why
        // that was wrong: a pane is the *session's*, so a person whose head lost the rectangle
        // still has a program running and no way back to it. `!term` means *the pane this
        // session has*; a session with no pane says so through the same `TermEnded` every other
        // pane that could not start uses, and the note that closes the rectangle is where that
        // sentence is read.
        //
        // **And the third reading is the ending.** `!term close` is not a program to run: it is
        // how a pane is ENDED, and it is the only act that sends `Action::TermClose` — see
        // [`TermPane`] for why the destructive act has to be spelled out while `ctrl-\` only
        // detaches. The parse is `term_line`, the one function both halves share, so a head
        // cannot treat the line as the ending while the daemon runs a program called `close`.
        if let Some(what) = letibot_sessionlog::term_line(&text) {
            if self.detached() {
                self.set_composer(&text);
                self.say(
                    "no daemon connection — nothing was started and nothing was ended. Your line \
                     is held here. It sends when the daemon is back.",
                );
                self.redraw = true;
                return None;
            }
            if matches!(what, letibot_sessionlog::TermLine::Close) {
                return self.begin_close();
            }
            self.scroll = 0;
            // **The pane is created now, empty, and the daemon's first bytes fill it.** The
            // alternative — wait for `TermOutput` before opening the rectangle — would show
            // the transcript for as long as the pty takes to start a program, which is the
            // flicker this pane exists to remove. A pane that never starts is closed by the
            // `TermEnded` that carries the refusal, a moment later; an attach is closed by the
            // same frame when the session has no pane, and filled by the daemon's replay when
            // it has one.
            //
            // The rectangle here is the **last frame's**, which is the best this layer can
            // know; the first `compose_screen` corrects it to the pane's own and sends the
            // `TermResize` that tells the program — and on an attach the daemon has already
            // resized the pty to the rectangle this frame carries.
            self.term = Some(TermPane::new(
                &text,
                self.term_cols.max(1),
                self.screen_rows.max(1),
            ));
            self.redraw = true;
            return Some(Action::TermOpen { line: text });
        }

        if letibot_sessionlog::send_line(&text).is_some() {
            if self.detached() {
                self.set_composer(&text);
                self.say(
                    "no daemon connection — your line is held here. It sends when the daemon \
                     is back.",
                );
                self.redraw = true;
                return None;
            }
            self.scroll = 0;
            // **No echo and no `pending_prompts`.** A `!` line and a `!term` line both put
            // something in the conversation or on the screen; a `!send` line goes into a
            // running program's stdin and leaves no row behind. An echo would be this head
            // claiming a line that the transcript will never carry — the same rule
            // `!term`'s arm states for its own reason.
            return Some(Action::SendLine {
                line: letibot_sessionlog::send_line(&text)
                    .expect("just checked")
                    .to_string(),
            });
        }

        // **A line whose first character is `!` is the operator's own shell command.**
        //
        // The operator's ask: *"when prompt starts with ! it is going to be a shell command
        // from me"*. The bang has to be the FIRST character, exactly as `/` does for verbs —
        // one rule for the two sigils a composer line can start with, so leading whitespace
        // means prose, as it always did. The recognition is the same shape as the `/` arm
        // below and sits beside it for the same reason: while a card or a picker is open, a
        // line that begins with a sigil is that thing and cannot sensibly be anything else.
        //
        // **A line that is nothing but the bang is refused here, with the words kept.** `!`
        // and `!   ` carry no command, and sending one would make the daemon echo a refusal
        // for something the head could see was empty. The daemon re-checks anyway
        // (`operator_shell_command`), because a frame is a socket and not a keyboard.
        //
        // **No gate, no card, no ladder** — not because this head skips one but because
        // there is none on the path: the frame is `OperatorShell`, the daemon runs it through
        // `ToolRuntime::invoke_operator` (the door's own ungated entry), and no
        // `DecisionRequested` can appear for it. Pinned where the run lives:
        // `letibot-harnessd`'s `tests/operator_shell.rs` counts the adjudicator's calls
        // (and the log's decisions) and fails if either moves.
        //
        // The echo joins `pending_prompts` so the line is visibly held until the daemon's
        // `User` row lands and retires it — the same trust a prompt places — and the detached
        // guard is the prompt's own, because a `!` line that cannot reach a daemon must not
        // look sent either.
        if text.starts_with('!') {
            if letibot_sessionlog::operator_shell_command(&text).is_none() {
                self.set_composer(&text);
                self.say("! COMMAND — the bang has to be followed by the command to run");
                self.redraw = true;
                return None;
            }
            if self.detached() {
                self.set_composer(&text);
                self.say(
                    "no daemon connection — your line is held here. It sends when the daemon \
                     is back.",
                );
                self.redraw = true;
                return None;
            }
            self.scroll = 0;
            self.pending_prompts.push(text.clone());
            return Some(Action::OperatorShell { line: text });
        }
        if let Some(rest) = text.strip_prefix('/') {
            return self.command(rest.trim());
        }
        // The picker takes the line as a row number or an id prefix. It is checked
        // before the decision arm and before the prompt arm, because while a picker
        // is on the screen a bare `2` means the second session and cannot sensibly
        // mean anything else.
        if self.picker {
            return self.pick(text.trim());
        }
        // The mode picker takes the line the same way — a row number or a name
        // prefix — for the same reason: while the list is on the screen a bare
        // `2` means the second mode and cannot sensibly mean anything else.
        if self.pick == Some(Pick::Mode) {
            return self.pick_mode(text.trim());
        }
        // An open decision owns Enter, typed line or not. A line that names an
        // option is that answer. Any other line is not disposable: the ask
        // arrived while it was being typed, and Enter on a card means "answer
        // this" — the marked row — so the words go back to the composer and
        // the next Enter, with the ask settled, sends them. Sending them here
        // is how a permission arriving mid-typing turned Enter into "send the
        // half-thought" (the operator, 2026-09-17).
        if let Some(d) = self.open.first().cloned() {
            // **A question is answered with words, and that is not a courtesy**
            // (§1.7). D10's third field is `free` — *"a typed answer, and a
            // first-class one"* — and the requirement names the alternative it is
            // against: *"not claude code 'chat later'"*. So under a question every
            // line is either one of the model's own choices or an answer, and
            // neither is "hold the words and answer the marked row".
            //
            // **A line that IS a choice answers by index.** Matching the text is
            // how a person answers a menu they were shown, and it costs one
            // comparison per choice; `option` is preferred over `free` because the
            // model gets back *which of the three it offered* rather than a
            // sentence it has to re-read as one of them.
            if d.kind == "question" {
                let typed = text.trim();
                if typed.is_empty() {
                    return self.answer_marked();
                }
                let at = d
                    .choices
                    .iter()
                    .position(|c| c.trim().eq_ignore_ascii_case(typed));
                let answer = match at {
                    Some(i) => letibot_sessionlog::question::QuestionAnswer::choosing(i),
                    None => letibot_sessionlog::question::QuestionAnswer::free(typed),
                };
                return Some(Action::AnswerQuestion {
                    req_id: d.req_id.clone(),
                    answer,
                });
            }
            match match_option(&d, text.trim()) {
                OptionChoice::One {
                    option_id,
                    pattern,
                    note,
                } => {
                    return Some(Action::Answer {
                        req_id: d.req_id,
                        option_id,
                        pattern,
                        note,
                    });
                }
                // **A name that fits several options answers nothing.** The line goes
                // back to the composer so it can be finished, the card stays up so the
                // arrows still work, and the sentence names every candidate — because a
                // refusal with no next step is a head that has stopped listening, and
                // answering the marked row here would be the very defect this refusal
                // is against: a grant the operator did not choose, written to the audit
                // under a name they can see they did not type.
                OptionChoice::Ambiguous { word, candidates } => {
                    self.set_composer(&text);
                    self.say(&ambiguous_option_line(&word, &candidates));
                    self.redraw = true;
                    return None;
                }
                // Nothing on the card answers to the line: the mid-typing courtesy,
                // below.
                OptionChoice::Unnamed => {}
            }
            self.set_composer(&text);
            if let Some(a) = self.answer_marked() {
                self.say("answered the ask — your line is held, enter sends it");
                return Some(a);
            }
            // An ask with no options cannot be taken by Enter at all; the
            // line stays held rather than becoming a prompt sent under it.
            self.say("this ask offers no options — your line is held");
            return None;
        }
        // **A line typed while a fill is still running.**
        //
        // The daemon answers a prompt sent mid-import against the whole history — the
        // import is one worker job, so a prompt cannot interleave — but the head must not
        // show one as `queued` for a turn nobody has started. So it is refused and the
        // words go back to the field they were typed in: the §4.2 behaviour (`7b9ca62`),
        // reused, because answering against a half-adopted transcript and then appending
        // the rest would put the conversation in the wrong order (R2's rule with the whole
        // history missing).
        if self.filling.is_some() {
            self.set_composer(&text);
            self.say(
                "the conversation is still being imported — your line is held here. It sends \
                 when the import is done.",
            );
            self.redraw = true;
            return None;
        }
        // **A line typed into a head with no daemon must not look sent.**
        //
        // This is the one place where a detached head could lie quietly. Everything else
        // a key does is local — it moves a cursor, opens a pane, folds a block — but a
        // submit is the head taking the operator's words on the promise that something
        // will read them, and with no daemon nothing will. The failure mode without
        // this guard is the worst of the three: `submit` pushes the echo into
        // `pending_prompts`, the write fails, the conversation shows `queued · <their
        // words>` and the operator believes it was sent — and it is not queued anywhere,
        // so when the daemon comes back the sentence is simply gone, having been shown as
        // held.
        //
        // So: **refused, with the words put back where they were.** `set_composer`
        // restores the text the editor handed over on Enter — without it the refusal
        // would clear the field, which loses the sentence just as thoroughly as sending
        // it nowhere would — and pressing enter again once the daemon is back sends it
        // unchanged. Nothing is pushed to `pending_prompts`, so nothing can be shown as
        // queued and then evaporate.
        if self.detached() {
            self.set_composer(&text);
            self.say(
                "no daemon connection — your line is held here. It sends when the daemon \
                 is back.",
            );
            self.redraw = true;
            return None;
        }
        // **A bare line is not spent on the model while the operator's own command waits.**
        //
        // The measured case, 2026-10-09, and the defect this rule exists for: `! sudo apt
        // install mc`, the password taken, `apt` at `Continue? [Y/n]` under root where
        // `/proc` refuses — so no reading, and no card — and the person typed `y` here. The
        // line became a prompt and reached the MODEL; the command they had typed waited
        // unanswered until its deadline killed it. The daemon's request for their run is
        // **open** in exactly that window and it is the fact this reads: the card up, or put
        // away with `esc` — which does not end the run and does not close the request (see
        // [`App::prompt_away`]).
        //
        // **Held, not swallowed**, and the sentence names both doors — the same shape the
        // import and the detached head use below, and for the same reason: the words are the
        // person's, and a refusal that ate them would lose the answer they were giving. The
        // verbs are already past this point (`!`, `/`, a card's own answer), so `!send LINE`
        // and every slash verb still work while it is held.
        if self.prompt.is_some() {
            self.set_composer(&text);
            self.say(
                "your own command is waiting on a line — `!send LINE` answers it, and this \
                 message goes to the model when the run ends",
            );
            self.redraw = true;
            return None;
        }
        // Sending scrolls back to the tail: the answer is about to arrive at the
        // bottom, and staying parked in the scrollback while it does looks exactly
        // like nothing happening.
        self.scroll = 0;
        // Held here, visibly, until the transcript takes the words over. When the
        // session is idle the user row lands within a tick and this is a one-frame
        // acknowledgement; when a turn is running it is the whole fix — the hub
        // queues the prompt as a follow-up user item and appends it at the next
        // step boundary, and until then this is the only place the sentence exists
        // where the person who typed it can see it.
        //
        // **Behind a running turn the queue is one message.** The engine merges
        // the operator's consecutive steering into one held item (one user turn
        // for the model, not a stack of fragments), so the echo joins the same
        // way — the landing row retires the echo by being its text. Idle submits
        // land each as their own row within a tick, so they stay separate.
        // **Busy, not generating** — see `turn_busy`. This gate decides whether the line joins the
        // last echo or starts a new one, and the daemon merges everything typed during a ROUND
        // while the state name only covers generation: two prompts typed during a tool call got two
        // `queued` rows for one message.
        if self.turn_busy()
            && let Some(last) = self.pending_prompts.last_mut()
        {
            last.push('\n');
            last.push_str(&text);
        } else {
            self.pending_prompts.push(text.clone());
        }
        Some(Action::Prompt(text))
    }

    /// Columns the composer's text has, inside the box.
    ///
    /// One function, because the wrap width the editor is *drawn* at and the one
    /// vertical motion is *computed* at have to be the same number — a cursor
    /// that moves by a row the renderer did not draw lands somewhere the person
    /// was not looking.
    pub(crate) fn composer_cols(&self) -> usize {
        self.cfg.width.saturating_sub(4).max(8)
    }

    /// What the composer holds, for a test and for a head that wants to prefill it.
    pub fn input(&self) -> &str {
        self.editor.text()
    }

    /// Take Tab on a `/`-prefixed line.
    ///
    /// A fresh prefix starts a cycle at its first match; a further Tab walks
    /// the cycle, but only while the line is exactly what the cycle last
    /// wrote — a character typed on, or an edit away, starts a fresh match
    /// next time, so the cycle can never clobber what someone typed after it.
    /// A prefix nothing matches says so in the notice line and leaves the
    /// line alone, because deleting what someone typed to explain why nothing
    /// happened would be the completion acting like a decision.
    /// **Every verb this head offers, in one list** — its own and the daemon's (R32).
    ///
    /// The defect this replaces is measured, not argued: the completion table offered 27
    /// verbs while **five working daemon verbs were absent** (`/flowy`, `/gate`, `/job`,
    /// `/login`, `/supervise`) and three of the head's own (`/dismiss`, `/settings`,
    /// `/stats`). `docs/evidence/slash-completion-2026-09-23.py`.
    ///
    /// The two halves have two owners and neither enumerates the other:
    ///
    /// * **the head's**, from [`SLASH_COMMANDS`], which a test in this file holds against
    ///   the dispatcher's own source — so adding an arm without listing it fails the suite;
    /// * **the daemon's**, from the `daemon.verbs` `SettingRow`, because a head that
    ///   guessed at them is exactly how `/gate` and `/flowy` came to be missing while
    ///   working perfectly. An absent row means a daemon older than this one, and then the
    ///   head offers its own verbs and says nothing about the rest — which is the honest
    ///   answer, not a guess.
    ///
    /// The daemon's names carry no hint here: the daemon publishes names, not descriptions,
    /// and a head that invented a sentence about somebody else's verb would be writing the
    /// other half's documentation. They are drawn bare, which is also what tells a reader
    /// the two halves of the list apart without a label.
    pub(crate) fn command_names(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = SLASH_COMMANDS
            .iter()
            .map(|(n, h)| ((*n).to_string(), (*h).to_string()))
            .collect();
        for v in self.daemon_verbs() {
            // **A verb both halves reach is offered once, with the head's hint.** `/jobs`
            // is the live case: the head opens the pane and the daemon reads a job's
            // output, and the head's arm wins. Drawn twice it would read as two verbs.
            if !out.iter().any(|(n, _)| *n == v) {
                out.push((v, String::new()));
            }
        }
        // **And the door's, hyphenated** — R34. Offered from the daemon's own row, so this
        // head still holds no schema: it knows a name, a field and a kind, and the verb it
        // offers is a textual transform of the name rather than a second list.
        for t in self.door_tools() {
            let verb = letibot_sessionlog::head_run_verb(&t.name);
            if out.iter().any(|(n, _)| *n == verb) {
                continue;
            }
            let hint = if t.field.is_empty() {
                // **Said on the row rather than left to a refusal**, which is R31's own
                // requirement: *the head says which kind it is refusing* rather than
                // leaving the operator to guess which tools are which.
                "JSON arguments".to_string()
            } else {
                format!("{} — {}", t.kind, t.field)
            };
            out.push((verb, hint));
        }
        out
    }

    /// **The door's tools, as the daemon described them** — R31.
    pub(crate) fn door_tools(&self) -> Vec<letibot_sessionlog::HeadRunTool> {
        self.settings
            .iter()
            .find(|r| r.key == letibot_sessionlog::HEAD_RUN_TOOLS_KEY)
            .map(|r| r.tools.clone())
            .unwrap_or_default()
    }

    pub(crate) fn daemon_verbs(&self) -> Vec<String> {
        self.settings
            .iter()
            .find(|r| r.key == letibot_sessionlog::protocol::DAEMON_VERBS_KEY)
            .map(|r| {
                r.value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn complete_slash(&mut self) {
        let text = self.editor.text().to_string();
        if !text.starts_with('/') || text.contains(char::is_whitespace) {
            return;
        }
        if let Some((_, names, idx)) = &mut self.completion {
            let live = names
                .get(*idx)
                .is_some_and(|current| text == format!("/{current}"));
            if live && !names.is_empty() {
                *idx = (*idx + 1) % names.len();
                let word = names[*idx].clone();
                self.set_composer(&format!("/{word}"));
                return;
            }
        }
        // **The needle is normalised the same way the lookup is** (R34), so a person who
        // typed `/web_` because that is what the daemon calls the tool gets `/web-search`
        // offered rather than *no /command starts with*. The transform runs on both sides
        // of the comparison, which is what makes it a transform rather than a second list.
        let needle = text[1..].replace('_', "-");
        let names: Vec<String> = self
            .command_names()
            .into_iter()
            .map(|(n, _)| n)
            .filter(|n| n.replace('_', "-").starts_with(&needle))
            .collect();
        match names.first() {
            Some(first) => {
                let first = first.clone();
                self.completion = Some((text.clone(), names, 0));
                self.set_composer(&format!("/{first}"));
            }
            None => {
                self.completion = None;
                self.say(&format!("no /command starts with {text:?}"));
            }
        }
    }

    pub(crate) fn complete_shell(&mut self) {
        let text = self.editor.text().to_string();
        // The one recogniser: a `!` line is what the sessionlog crate says it is,
        // and a bang with no command after it is not one — so `!` alone does
        // nothing, the same refusal the send makes.
        if letibot_sessionlog::operator_shell_command(&text).is_none() {
            return;
        }
        // **A file name first, the way a shell completes one** — the operator, after
        // `! ./stroppy/build/stroppy` had to be typed out whole: *"when i do ! <command> i
        // dont get path name or context suggestion"*. The history and the model complete
        // whole LINES; a path is a word, and the filesystem is the one completer that is
        // free, local and never wrong about what exists. Only with the cursor at the end,
        // and only when it finds something — otherwise the line cycles below as before.
        if self.editor.cursor() == text.len()
            && let Some(done) = complete_path_word(&text, &self.wiring.workspace)
        {
            match done {
                PathCompletion::Line(line) => {
                    self.path_matches = None;
                    self.set_composer(&line);
                }
                PathCompletion::Choices { line, names } => {
                    if line != text {
                        self.set_composer(&line);
                    }
                    self.path_matches = Some((line, names));
                }
            }
            return;
        }
        // **The model's cycle, if it is live.** It is checked first because it is the
        // more recent answer: the operator exhausted the history to get here. An empty
        // lines list is the *waiting* state — the history is exhausted and the model
        // has not answered yet — and a Tab in that state re-checks the cache rather
        // than asking again.
        if let Some((prefix, lines, idx)) = &mut self.shell_model {
            let live = text.starts_with(prefix.as_str())
                && (lines.is_empty() || lines.get(*idx).is_some_and(|current| text == *current));
            if live {
                if lines.is_empty() {
                    self.shell_model_fallback(&text);
                    return;
                }
                *idx = (*idx + 1) % lines.len();
                let line = lines[*idx].clone();
                self.set_composer(&line);
                return;
            }
        }
        // **The history's cycle, if it is live.** When it is exhausted — the next Tab
        // would wrap to the first history candidate — the model is the fallback, asked
        // once, and its suggestions are cycled instead. **The wrap is what changes**:
        // the composer stays on the last candidate rather than snapping back to the
        // first, and that candidate is the prefix the model is asked about, because a
        // proposal for the line the operator is looking at is a completion and a
        // proposal for a line they have already scrolled past is the same line offered
        // twice. A model with nothing to offer leaves the composer where it is and says
        // so; the history's candidates are all still one character away, since a prefix
        // typed on is matched fresh.
        if let Some((_, lines, idx)) = &mut self.completion {
            let live = lines.get(*idx).is_some_and(|current| text == *current);
            if live && !lines.is_empty() {
                let next = (*idx + 1) % lines.len();
                if next == 0 {
                    self.completion = None;
                    self.shell_model_fallback(&text);
                    return;
                }
                *idx = next;
                let line = lines[*idx].clone();
                self.set_composer(&line);
                return;
            }
        }
        // No live cycle: start the history's, or fall to the model when the history
        // has nothing for this prefix.
        let lines: Vec<String> = self
            .shell_candidates()
            .iter()
            .filter(|line| line.starts_with(&text))
            .cloned()
            .collect();
        match lines.first() {
            Some(first) => {
                let first = first.clone();
                self.completion = Some((text.clone(), lines, 0));
                self.set_composer(&first);
            }
            None => {
                self.completion = None;
                self.shell_model_fallback(&text);
            }
        }
    }

    /// **The model's half of the `!` completion, asked once per (prefix, position).**
    ///
    /// Called when the history has no match for the prefix, or its cycle is exhausted.
    /// It is the whole of the "ask the model" decision, and the rule it keeps is that
    /// **the same prefix asked twice is not two model calls**: an answered ask is
    /// cycled from the cache, an in-flight ask is waited on, and only a prefix never
    /// asked is sent to the daemon.
    ///
    /// **The prefix is the line in the composer, not the line the operator started
    /// with.** The two are the same thing whenever the history had no match, which is
    /// the case the feature is for. At the end of a history cycle they are not: the
    /// composer holds the last candidate the history offered, and that is the line the
    /// model is asked to complete. Asking about the typed prefix instead would let a
    /// proposal come back that the operator has just cycled past — the same line
    /// offered twice in a row, once as a fact and once as a guess — and the rule the
    /// daemon's prompt carries is that a wrong suggestion is worse than none.
    ///
    /// **Presence in the cache is the answer, not the length of the list.** A model
    /// that said nothing usable *said something*, and an empty answer read as *not
    /// asked* would send a fresh call on every Tab for the same prefix — the one thing
    /// the (prefix, position) key exists to prevent.
    ///
    /// The position is the number of transcript rows, so a row landing is a new
    /// position and a fresh ask — the cache is cleared when the transcript advances,
    /// and a suggestion built on the conversation as it was is a suggestion about
    /// that conversation.
    ///
    /// **Nothing here submits.** The answer is a list of candidate lines for the
    /// composer, drawn as candidates with their provenance, and Enter is still the
    /// operator's.
    pub(crate) fn shell_model_fallback(&mut self, text: &str) {
        let position = self.items.len() as u64;
        let key = (text.to_string(), position);
        // Already answered for this prefix at this position: cycle the cached lines,
        // or say that the model had nothing and do not ask again.
        if let Some(lines) = self.shell_suggestions.get(&key) {
            let lines = lines.clone();
            if lines.is_empty() {
                // The model said nothing usable. That is a good answer rather than a
                // failure — the rule it was given is that a wrong suggestion is worse
                // than none — so it is said plainly, and the cycle stays live with
                // nothing in it: the next Tab says the same thing without a call.
                self.shell_model = Some((text.to_string(), Vec::new(), 0));
                self.say(&format!("the model has no ! line starting with {text:?}"));
                return;
            }
            let first = lines[0].clone();
            self.shell_model = Some((text.to_string(), lines, 0));
            self.set_composer(&first);
            return;
        }
        // Already asked for this prefix at this position: the response is in flight.
        // Enter the waiting state and say so, rather than asking again.
        if self
            .shell_ask
            .values()
            .any(|(p, n)| p == text && *n == position)
        {
            self.shell_model = Some((text.to_string(), Vec::new(), 0));
            self.say(&format!(
                "asking the model for a ! line starting with {text:?}…"
            ));
            return;
        }
        // Ask the model, once. The id is minted here so the head can recognise the
        // answer on the way back; the daemon echoes it in `ShellSuggestions`.
        let id = self.next_shell_ask_id();
        self.shell_ask
            .insert(id.clone(), (text.to_string(), position));
        self.shell_model = Some((text.to_string(), Vec::new(), 0));
        self.queued.push(Action::SuggestShell {
            prefix: text.to_string(),
            client_request_id: id,
        });
        self.say(&format!(
            "asking the model for a ! line starting with {text:?}…"
        ));
    }

    /// The next `SuggestShell`'s `client_request_id`, beside `next_head_run` and for
    /// the same reason: the id has to be unique per head, and the head is the one
    /// that has to recognise it when the answer comes back on the pump.
    pub(crate) fn next_shell_ask_id(&mut self) -> String {
        self.shell_ask_seq += 1;
        format!("{}-s{}", self.head_id, self.shell_ask_seq)
    }

    /// **The transcript moved, so everything derived from it is stale.**
    ///
    /// Two things are derived from the rows and both are held between frames: the `!`
    /// candidate list ([`App::shell_candidates_memo`]), and the model's suggestions —
    /// which are answers about the conversation *as it was*, and a conversation that
    /// moved is a different question. One method, because the two call sites are the
    /// two ways the transcript changes and a third caller is a third chance to
    /// remember only one of them.
    ///
    /// **A body landing counts as a move**, which is why [`App::record_item`] calls it
    /// too: a row announced with no body carries no tool calls yet, and a prompt built
    /// on it would be a prompt about a row that had not arrived.
    pub(crate) fn the_rows_moved(&mut self) {
        self.shell_candidates_memo = None;
        self.clear_shell_suggestions();
    }

    /// **The model's suggestions are stale: the conversation moved.**
    ///
    /// A suggestion is built on the conversation as it was when it was asked, and a
    /// conversation that moved is a different question. So when a row lands — or a
    /// snapshot replaces the rows — the asks in flight and the answered asks are
    /// both dropped, and the next Tab for the same prefix is a fresh ask rather than
    /// a stale answer. The model's cycle is dropped too: it is cycling lines about a
    /// conversation that no longer is, and a character typed on would match fresh
    /// anyway.
    ///
    /// **Called from [`App::the_rows_moved`], which is the only caller besides the link
    /// going down** — that is deliberate, because the two are always stale together and
    /// a call site that remembered one of them would be a call site that forgot the
    /// other.
    pub(crate) fn clear_shell_suggestions(&mut self) {
        if self.shell_ask.is_empty()
            && self.shell_suggestions.is_empty()
            && self.shell_model.is_none()
        {
            return;
        }
        self.shell_ask.clear();
        self.shell_suggestions.clear();
        self.shell_model = None;
    }

    /// **The whole `!` lines this session has run**, newest first, deduped: the
    /// operator's own `!` rows verbatim, and the model's `bash` calls as `! ` plus
    /// the command they ran.
    ///
    /// **Newest first, because that is what a person re-running a command wants**:
    /// the last thing they did is the most likely thing they are about to do again.
    /// Deduped keeping the newest, so a command run twice is offered once, as the
    /// line it most recently was.
    ///
    /// **Held between frames**, because this is the render path's as well as Tab's:
    /// see [`App::shell_candidates_memo`] for the measurement that made it one walk
    /// per row change rather than one per frame.
    /// **The composer's Up history is the session's, not this head's.**
    ///
    /// The operator: *"i worked - sent 30 prompts. then restart, send 2. and arrow up sees
    /// only these two"*. The editor's history lived in the head process, so a restarted
    /// head — or a second head on the same session — recalled only what it had typed
    /// itself. The session's own record has every prompt: the operator's `User` rows, in
    /// order, which is what the `!` candidates already walk. Refreshed at the start of a
    /// recall, so it is the session on screen now; lines this head typed that the session
    /// does not hold (yet) stay at the end, newest last.
    pub(crate) fn refresh_prompt_history(&mut self) {
        let mut merged: Vec<String> = Vec::new();
        for r in &self.items {
            let Some(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts,
                ..
            }) = r.item.as_ref()
            else {
                continue;
            };
            let text = parts
                .iter()
                .filter_map(|p| match p {
                    UserPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            let text = text.trim();
            if !text.is_empty() && merged.last().map(String::as_str) != Some(text) {
                merged.push(text.to_string());
            }
        }
        for own in self.editor.history().to_vec() {
            if !merged.contains(&own) {
                merged.push(own);
            }
        }
        self.editor.set_history(merged);
    }

    pub(crate) fn shell_candidates(&mut self) -> &[String] {
        if self.shell_candidates_memo.is_none() {
            self.shell_walks += 1;
            self.shell_candidates_memo = Some(self.walk_shell_candidates());
        }
        self.shell_candidates_memo.as_deref().unwrap_or(&[])
    }

    /// The walk itself — what [`App::shell_candidates`] caches, and the only place that
    /// reads the rows for it.
    ///
    /// **The walk is the head's own rows** — the snapshot items, `item` an
    /// `Option` because a row can be announced before its body lands — walked the
    /// way `targets_before` walks them. A `bash` call whose arguments do not parse,
    /// or that carries no `command`, is skipped: a candidate that cannot be re-run
    /// is not a candidate.
    pub(crate) fn walk_shell_candidates(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for r in self.items.iter().rev() {
            let Some(item) = r.item.as_ref() else {
                continue;
            };
            match item {
                TranscriptItem::User {
                    speaker: letibot_transcript::Speaker::Operator,
                    parts,
                    ..
                } => {
                    // The row's text, the way the renderer reads it: the text parts
                    // joined. A `!` line is one part, so this is the line verbatim.
                    let text = parts
                        .iter()
                        .filter_map(|p| match p {
                            UserPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    if text.starts_with('!') {
                        out.push(text);
                    }
                }
                TranscriptItem::Assistant { tool_calls, .. } => {
                    for c in tool_calls {
                        if c.name != "bash" {
                            continue;
                        }
                        let Ok(v) = serde_json::from_str::<serde_json::Value>(&c.arguments) else {
                            continue;
                        };
                        let Some(cmd) = v.get("command").and_then(|c| c.as_str()) else {
                            continue;
                        };
                        out.push(format!("! {cmd}"));
                    }
                }
                _ => {}
            }
        }
        // Dedupe keeping the newest (first) occurrence.
        let mut seen = std::collections::HashSet::new();
        out.retain(|line| seen.insert(line.clone()));
        out
    }

    /// Replace the whole composer line. Completion words are single tokens, so
    /// Home + kill-to-end + insert is the honest way there: the editor has no
    /// text setter, and the three public ops keep its undo and history exactly
    /// as true as any typed edit.
    pub(crate) fn set_composer(&mut self, text: &str) {
        self.editor.key(letibot_ui::editor::Key::Home, self.now_ms);
        self.editor
            .key(letibot_ui::editor::Key::KillToEnd, self.now_ms);
        self.editor.insert(text);
    }

    /// **Whether the composer holds a line the completion row belongs to** — the SHAPE
    /// that *could* be completed, whether or not anything matches it right now.
    ///
    /// This is the row's gate and the frame's reservation in one place, because the height
    /// is computed from it and the content is computed from it: two spellings of the shape
    /// would be a frame that reserves a row and draws nothing in it, or draws a row it did
    /// not count — and either one is the transcript moving on its own.
    ///
    /// **The shape, and not the candidate list.** A predicate that answered *there is a
    /// match* would flicker as the operator types — `! cargo` matches and `! cargo x` does
    /// not — and every flicker takes a line from the conversation above it, which is the
    /// defect the reservation exists to stop. The shape changes only when the operator
    /// starts or abandons such a line; an ordinary line, and an empty composer, are not
    /// ones: `hello` completes nothing, so it pays nothing.
    pub(crate) fn completion_slot(&self) -> bool {
        let text = self.editor.text();
        text.starts_with('!') || (text.starts_with('/') && !text.contains(char::is_whitespace))
    }
}

/// What a Tab on a `!` line's last word found in the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathCompletion {
    /// One match: the whole line with the word completed — `/` after a directory, a space
    /// after a file, so the next word can be typed at once.
    Line(String),
    /// Several: the line with the word extended to what they share (maybe unchanged), and
    /// the names to show.
    Choices { line: String, names: Vec<String> },
}

/// **Complete the last word of a `!` line as a path**, relative to `workspace` — where an
/// operator's `!` command runs — with `~/` as the home directory.
///
/// The word in command position (the first after `!`) is a program, and is completed only
/// when it is spelled as a path (`./build/x`, `/usr/bin/x`, `~/bin/x`); every later word is
/// an argument and is completed as a file. A word with quotes or `$` in it is left alone —
/// what it names is the shell's to work out. `None` when nothing matches, so a Tab falls
/// through to the line completions.
pub(crate) fn complete_path_word(text: &str, workspace: &str) -> Option<PathCompletion> {
    let cmd = text.strip_prefix('!')?;
    let start = text.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(1);
    let word = &text[start..];
    if word.contains(['\'', '"', '$', '`']) {
        return None;
    }
    let first = cmd.trim_start().find(char::is_whitespace).is_none();
    let pathish = word.starts_with(['.', '/', '~']) || word.contains('/');
    if first && !pathish {
        return None;
    }
    // Split at the last `/`: what is listed, and the prefix the names must start with.
    let (dir_part, base) = match word.rfind('/') {
        Some(i) => (&word[..=i], &word[i + 1..]),
        None => ("", word),
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = if let Some(rest) = dir_part.strip_prefix("~/") {
        std::path::Path::new(&home).join(rest)
    } else if dir_part == "~" {
        std::path::PathBuf::from(&home)
    } else if dir_part.starts_with('/') {
        std::path::PathBuf::from(dir_part)
    } else {
        std::path::Path::new(workspace).join(dir_part)
    };
    let mut names: Vec<(String, bool)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            // Hidden names only when asked for, as a shell does.
            if !name.starts_with(base) || (name.starts_with('.') && !base.starts_with('.')) {
                return None;
            }
            let is_dir = e.path().is_dir();
            Some((name, is_dir))
        })
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort();
    let escape = |n: &str| n.replace(' ', "\\ ");
    if let [(name, is_dir)] = names.as_slice() {
        let tail = if *is_dir { "/" } else { " " };
        return Some(PathCompletion::Line(format!(
            "{}{dir_part}{}{tail}",
            &text[..start],
            escape(name)
        )));
    }
    // The longest prefix every match shares, in whole characters.
    let mut common: String = names[0].0.clone();
    for (n, _) in &names[1..] {
        let keep = common
            .char_indices()
            .zip(n.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map(|((i, c), _)| i + c.len_utf8())
            .unwrap_or(0);
        common.truncate(keep);
    }
    let line = format!("{}{dir_part}{}", &text[..start], escape(&common));
    let shown = names
        .into_iter()
        .map(|(n, d)| if d { format!("{n}/") } else { n })
        .collect();
    Some(PathCompletion::Choices { line, names: shown })
}
