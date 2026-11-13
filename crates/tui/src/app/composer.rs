//! The composer's behaviour: sending what was typed, completing a slash verb, a path or a
//! shell command, and the prompt history Up recalls.

use super::*;
use letibot_transcript::{TranscriptItem, UserPart};

impl App {
    /// What a submitted line means: a command, an answer to an open decision, or
    /// a prompt.
    pub(crate) fn submit(&mut self, text: String) -> Option<Action> {
        // A read-only attach refuses to send, says the informed sentence and holds the line, before anything parses
        // it: the gate is not about what the line is but about what sending anything costs.
        if self.skew_locked() {
            self.set_composer(&text);
            self.refused_while_locked();
            return None;
        }
        // A pasted block is not a command, and this is checked before anything parses it: the composer is one line, so
        // the only way a newline gets here is a paste, and a paste whose first line starts with `!` would become one
        // command with the rest of the block in it.
        if let Some(why) = letibot_sessionlog::operator_line_refusal(&text) {
            self.set_composer(&text);
            self.say(&why);
            return None;
        }
        // `!term` is checked before the bare `!`, because `!term mc` is also a good `!` line: the verb has to be
        // taken first, and the parse is what decides whether the rest is a program to start.
        if let Some(what) = letibot_sessionlog::term_line(&text) {
            if self.detached() {
                self.set_composer(&text);
                self.say(
                    "no daemon connection — nothing was started and nothing was ended. Your line \
                     is held here. It sends when the daemon is back.",
                );
                return None;
            }
            if let letibot_sessionlog::TermLine::Close(which) = what {
                return self.begin_close(which);
            }
            self.scroll = 0;
            // The pane is created now, empty, and the daemon's first bytes fill it: waiting for `TermOutput` first
            // would show the transcript for as long as the pty takes to start a program.
            self.term = Some(TermPane::new(
                &text,
                self.term_cols.max(1),
                self.screen_rows.max(1),
            ));
            return Some(Action::TermOpen { line: text });
        }

        if letibot_sessionlog::send_line(&text).is_some() {
            if self.detached() {
                self.set_composer(&text);
                self.say(
                    "no daemon connection — your line is held here. It sends when the daemon \
                     is back.",
                );
                return None;
            }
            self.scroll = 0;
            // No echo and no `pending_prompts`: a `!send` line goes into a running program's stdin and leaves no row
            // behind, so an echo would be a promise of a row nobody is going to get.
            return Some(Action::SendLine {
                line: letibot_sessionlog::send_line(&text)
                    .expect("just checked")
                    .to_string(),
            });
        }

        // A line whose first character is `!` is the person's own shell command. The bang has to be the first
        // character, exactly as `/` does, so a line indented by a space is prose and cannot become a command.
        if text.starts_with('!') {
            if letibot_sessionlog::operator_shell_command(&text).is_none() {
                self.set_composer(&text);
                self.say("! COMMAND — the bang has to be followed by the command to run");
                return None;
            }
            if self.detached() {
                self.set_composer(&text);
                self.say(
                    "no daemon connection — your line is held here. It sends when the daemon \
                     is back.",
                );
                return None;
            }
            self.scroll = 0;
            self.pending_prompts.push(text.clone());
            return Some(Action::OperatorShell { line: text });
        }
        if let Some(rest) = text.strip_prefix('/') {
            return self.command(rest.trim());
        }
        // The picker takes the line as a row number or an id prefix, and it is checked before the decision and prompt
        // arms, because while a picker is up a bare `2` means the second session.
        if self.focused_surface() == Some(Surface::Picker) {
            return self.pick(text.trim());
        }
        // In the rooms view a line is a message to the selected channel - or, when a chat layer
        // owns the input surface, whatever that layer answers `command_chat` with (`X5`).
        if self.focused_surface() == Some(Surface::Rooms) {
            if self.chat_owns_input() {
                return self.chat_command(&text);
            }
            return self.rooms_say(&text);
        }
        // The setting card takes the line while it is the card in the slot: a row number or a
        // name on the mode card. Guarded on the card being DRAWN, because a decision in front owns
        // Enter - a line typed there is the decision's. The model card owns its own search field and
        // never reads the composer, so it is not in this arm.
        if self.pick_card_up() && self.pick == Some(Pick::Mode) {
            return self.pick_mode(text.trim());
        }
        // An open decision owns Enter, typed line or not, and a line that names an option is that answer. Any other
        // line is not disposable: the ask arrived while it was being typed, and Enter on a card means answer this.
        if let Some(d) = self.open.first().cloned() {
            // A question is answered with words, and that is not a courtesy: a typed answer is first-class, and the
            // requirement names the alternative it is against.
            if d.kind == "question" {
                let typed = text.trim();
                if typed.is_empty() {
                    return self.answer_marked();
                }
                return Some(
                    self.settle_question(d.req_id.clone(), question_answer(&d.choices, typed)),
                );
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
                // A name that fits several options answers nothing: the line goes back to the composer so it can be
                // finished, the card stays up so the arrows still work, and the sentence names every candidate.
                OptionChoice::Ambiguous { word, candidates } => {
                    self.set_composer(&text);
                    self.say(&ambiguous_option_line(&word, &candidates));
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
        // A line typed while a fill is still running: the daemon answers it against the whole history, since an import
        // is one worker job, and the head must not show it as though it had been read already.
        if let Some((what, ..)) = &self.filling {
            let said = if what.contains("import") {
                "the conversation is still being imported — your line is held here. It sends \
                 when the import is done."
                    .to_string()
            } else {
                format!(
                    "the daemon is busy {what} — your line is held here. Enter sends it when \
                     that is done."
                )
            };
            self.set_composer(&text);
            self.say(&said);
            return None;
        }
        // A line typed into a head with no daemon must not look sent. Everything else a key does is local, so this is
        // the one place a detached head could lie quietly.
        if self.detached() {
            self.set_composer(&text);
            self.say(
                "no daemon connection — your line is held here. It sends when the daemon \
                 is back.",
            );
            return None;
        }
        // A bare line is not spent on the model while the person's own command waits: with `apt` at its prompt, their
        // `y` typed at the composer would become a prompt for the model while their own command died at its deadline.
        if self.prompt.is_some() {
            self.set_composer(&text);
            self.say(
                "your own command is waiting on a line — `!send LINE` answers it; this message \
                 is held here, and enter sends it to the model once the run ends",
            );
            return None;
        }
        // The model is checked before the words leave - see `app::reach`. Held there, not
        // here: a message the chooser does not take comes back to this field.
        if self.hold_for_model(&text) {
            return None;
        }
        // The chips this message carries, read before the words leave and refused while they are still the person's: a
        // chip whose file has gone is not sent empty and not sent without, and the line goes back to the composer with
        // a sentence.
        let attachments = self.chips_to_send(&text)?;
        // The places `alt-s` attached, read before the words leave: a prompt that carried them
        // without a word being written is the whole point of the chip.
        let context = self.contexts_to_send();
        // Sending scrolls back to the tail: the answer is about to arrive at the
        // bottom, and staying parked in the scrollback while it does looks exactly
        // like nothing happening.
        self.scroll = 0;
        // Held here, visibly, until the transcript takes the words over: idle, this is a one-frame acknowledgement,
        // and mid-turn it is the whole fix, since the hub queues the words and appends them at a later boundary.
        if self.turn_busy()
            && let Some(last) = self.pending_prompts.last_mut()
        {
            last.push('\n');
            last.push_str(&text);
        } else {
            self.pending_prompts.push(text.clone());
        }
        // The chips went with it. The composer is empty again - the editor took its buffer -
        // so a chip left in this list would be a chip with no words to be found in, and the next
        // message would carry it a second time. A decision goes the same way: the session has been
        // told, and repeating it is the same sentence twice.
        self.chips.clear();
        self.contexts.clear();
        self.outcomes.clear();
        Some(Action::Prompt(Prompt::with_context(
            text,
            attachments,
            context,
        )))
    }

    /// Columns the composer's text has, inside the box. One function, because the width the editor is drawn at and
    /// the one vertical motion is computed at have to be the same number, or a cursor moves by a row the renderer did
    /// not wrap.
    pub(crate) fn composer_cols(&self) -> usize {
        self.cfg.width.saturating_sub(4).max(8)
    }

    /// What the composer holds, for a test and for a head that wants to prefill it.
    pub fn input(&self) -> &str {
        self.editor.text()
    }

    /// Take Tab on a `/`-prefixed line: a fresh prefix starts a cycle at its first match, and a further Tab walks
    /// it only while the line is exactly what the cycle last wrote, so a character typed on starts fresh.
    pub(crate) fn command_names(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = SLASH_COMMANDS
            .iter()
            .map(|(n, h)| ((*n).to_string(), (*h).to_string()))
            .collect();
        for v in self.daemon_verbs() {
            // A verb both halves reach is offered once, with the head's hint. `/jobs`
            // is the live case: the head opens the pane and the daemon reads a job's
            // output, and the head's arm wins. Drawn twice it would read as two verbs.
            if !out.iter().any(|(n, _)| *n == v) {
                out.push((v, String::new()));
            }
        }
        // And the door's, hyphenated. Offered from the daemon's own row, so this
        // head still holds no schema: it knows a name, a field and a kind, and the verb it
        // offers is a textual transform of the name rather than a second list.
        for t in self.door_tools() {
            let verb = letibot_sessionlog::head_run_verb(&t.name);
            if out.iter().any(|(n, _)| *n == verb) {
                continue;
            }
            let hint = if t.field.is_empty() {
                // Said on the row rather than left to a refusal: the head says which kind it
                // is refusing rather than
                // leaving the operator to guess which tools are which.
                "JSON arguments".to_string()
            } else {
                format!("{} — {}", t.kind, t.field)
            };
            out.push((verb, hint));
        }
        out
    }

    /// The door's tools, as the daemon described them.
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

    /// The repositories of this folder, as the daemon's `listen.NAME` rows name them.
    pub(crate) fn listen_repos(&self) -> Vec<String> {
        self.settings
            .iter()
            .filter_map(|r| r.key.strip_prefix(crate::app::prefs::LISTEN_ROW))
            .map(str::to_string)
            .collect()
    }

    /// Tab inside `/listen WORDS`: the last word completes to a repository (after `+` or `-`, or
    /// after `only`), or to `only`, `all`, `this`. One match fills it in, several fill their common
    /// start and are named.
    fn complete_listen(&mut self, text: &str) {
        let (head, last) = match text.rfind(char::is_whitespace) {
            Some(i) => (&text[..=i], &text[i + 1..]),
            None => return,
        };
        let after_only = head.trim_end().ends_with(" only");
        let repos = self.listen_repos();
        let (sign, stem) = match last.chars().next() {
            Some(c @ ('+' | '-')) => (c.to_string(), &last[1..]),
            _ => (String::new(), last),
        };
        let mut options: Vec<String> = if !sign.is_empty() || after_only {
            repos.clone()
        } else {
            let mut o = vec!["only".to_string(), "all".to_string(), "this".to_string()];
            o.extend(repos.iter().cloned());
            o
        };
        options.retain(|o| o.starts_with(stem));
        let Some(first) = options.first().cloned() else {
            self.say("no repository or word of /listen starts so");
            return;
        };
        let mut common = first;
        for o in &options[1..] {
            while !o.starts_with(&common) {
                common.pop();
            }
        }
        if options.len() > 1 {
            self.say(&format!("/listen: {}", options.join(" · ")));
        }
        self.set_composer(&format!("{head}{sign}{common}"));
    }

    pub(crate) fn complete_slash(&mut self) {
        let text = self.editor.text().to_string();
        if text.starts_with("/listen ") {
            self.complete_listen(&text);
            return;
        }
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
        // The needle is normalised the same way the lookup is, so a person who typed `/web_` because that is what the
        // daemon calls the tool gets an offer rather than *no command starts with*.
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
        // and a bang with no command after it is not one - so `!` alone does
        // nothing, the same refusal the send makes.
        if letibot_sessionlog::operator_shell_command(&text).is_none() {
            return;
        }
        // A file name first, the way a shell completes one: a path had to be typed out whole before, and the history
        // and the model complete lines rather than paths.
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
        // The model's cycle if it is live, checked first because it is the more recent answer: the person exhausted
        // the history to get here. An empty list is the waiting state.
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
        // The history's cycle if it is live, and when it is exhausted the model is the fallback: the wrap is what
        // changes, since another Tab after it asks the model instead of cycling again.
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

    /// The model's half of the `!` completion, asked once per prefix and position: called when the history has no
    /// match or its cycle is exhausted, and it is the whole of the ask-the-model decision.
    pub(crate) fn shell_model_fallback(&mut self, text: &str) {
        let position = self.items.len() as u64;
        let key = (text.to_string(), position);
        // Already answered for this prefix at this position: cycle the cached lines,
        // or say that the model had nothing and do not ask again.
        if let Some(lines) = self.shell_suggestions.get(&key) {
            let lines = lines.clone();
            if lines.is_empty() {
                // The model said nothing usable, which is a good answer rather than a failure, since a wrong
                // suggestion is worse than none: it is said plainly and the cycle stays live.
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

    /// The transcript moved, so everything derived from it is stale: the `!` candidate list and the model's
    /// suggestions, both held between frames. See [`App::shell_candidates_memo`].
    pub(crate) fn the_rows_moved(&mut self) {
        self.shell_candidates_memo = None;
        self.clear_shell_suggestions();
    }

    /// The model's suggestions are stale, because the conversation moved: a suggestion is built on the conversation
    /// as it was when it was asked, so a row landing or a snapshot replacing the rows is a different question.
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

    /// The whole `!` lines this session has run, newest first and deduped: the person's own rows verbatim, and the
    /// model's `bash` calls as `! ` plus the command. Newest first because that is what a person re-running one wants.
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

    /// The walk itself, which is what [`App::shell_candidates`] caches: it reads the snapshot items, `item` being an
    /// `Option` because a row can be announced before its body lands.
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

    /// Replace the whole composer line. Completion words are single tokens, so clearing to the end and inserting is
    /// the honest way: the editor has no text setter, and these ops keep its undo and history true.
    pub(crate) fn set_composer(&mut self, text: &str) {
        self.editor.key(letibot_ui::editor::Key::Home, self.now_ms);
        self.editor
            .key(letibot_ui::editor::Key::KillToEnd, self.now_ms);
        self.editor.insert(text);
    }

    /// Whether the composer holds a line the completion row belongs to: the SHAPE that could be completed, whether
    /// or not anything matches. The row's gate and the frame's reservation in one place, because the height matters.
    pub(crate) fn completion_slot(&self) -> bool {
        let text = self.editor.text();
        text.starts_with('!') || (text.starts_with('/') && !text.contains(char::is_whitespace))
    }
}

/// What a Tab on a `!` line's last word found in the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathCompletion {
    /// One match: the whole line with the word completed - `/` after a directory, a space
    /// after a file, so the next word can be typed at once.
    Line(String),
    /// Several: the line with the word extended to what they share (maybe unchanged), and
    /// the names to show.
    Choices { line: String, names: Vec<String> },
}

/// Complete the last word of a `!` line as a path, relative to `workspace`, which is where the command runs, with
/// `~/` as the home directory. The word in command position is a program, and is completed only as one.
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
