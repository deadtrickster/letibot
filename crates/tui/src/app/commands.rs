//! The slash commands: `/verbosity`, `/notes`, `/todo`, a door tool run by the head, and
//! the dispatcher that reads the rest.

use super::*;

/// The verbs `/todo` answers itself: the four that move a row, plus `when`. Everything else goes to the
/// `todo` layer first, because what a new row may be spelled like is a project's policy and the write
/// stays the head's. A verb in the `match` without a word here is handed to the guest.
pub(crate) const OWN_VERBS: [&str; 5] = ["done", "rm", "postpone", "resume", "when"];

/// Is this word one the head answers, rather than one a layer may own?
pub(crate) fn is_own_verb(verb: &str) -> bool {
    OWN_VERBS.contains(&verb)
}

/// The words out of a `command_todo` answer: `Some(content)` when the layer asked for a row, `None` for
/// every other answer. The one act name is the trust boundary, so a layer that answered `run_shell`
/// gets a line nothing happens to.
pub(crate) fn add_todo_words(answer: &serde_json::Value) -> Option<String> {
    if act_name(answer)? != "add_todo" {
        return None;
    }
    let content = answer
        .get("act")?
        .get("args")?
        .get("content")?
        .as_str()?
        .trim();
    (!content.is_empty()).then(|| content.to_string())
}

/// The words out of a `say` act: a sentence on the head's notice row. The second act a `command_`
/// layer may ask for, and the whole of what one that only answers something needs.
fn said_words(answer: &serde_json::Value) -> Option<String> {
    if act_name(answer)? != "say" {
        return None;
    }
    let text = answer
        .get("act")?
        .get("args")?
        .get("text")?
        .as_str()?
        .trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The act name an answer carries, whatever it is - so the head can say WHICH act it refused
/// instead of leaving the person with a line that did nothing.
fn act_name(answer: &serde_json::Value) -> Option<&str> {
    answer.get("act")?.get("name")?.as_str()
}

impl App {
    /// A fold changes how many lines every cached block renders to, so the history
    /// buffer and every block cache are stale at once.
    pub(crate) fn refold(&mut self) {
        self.invalidate_history();
        self.scroll = 0;
        self.say(&format!(
            "thinking {} · tool output {} · raw tool calls {}",
            fold_word(self.reasoning),
            fold_word(self.tools),
            if self.raw_calls { "shown" } else { "hidden" }
        ));
    }

    /// The one place a mode leaves the head, so the `allow-all` confirmation
    /// cannot be reached by one route and skipped by another. The picker, `/mode
    /// NAME` and the config pane's cycle all end here.
    pub(crate) fn mode_action(&mut self, name: String) -> Option<Action> {
        // The literal, not `Mode::ALLOW_ALL.name`: the head does not link `letibot-tools` and keeps no
        // mode list, and this is the one name it has to recognise.
        if name == "allow-all" {
            // This card is the operator's own, so nothing another head asked for may still
            // be riding it: an answer that carried a stale request id would settle somebody
            // else's question with this person's keystroke.
            self.open_mode_card(name, None);
            return None;
        }
        Some(Action::Mode {
            name,
            consented: false,
        })
    }

    /// Choose the profile `typed` names, a local setting, so nothing is sent anywhere. The one place a
    /// set is set, called by the card, by `/verbosity` and by `/v`, and the word is read by
    /// [`Visibility::parse`], the same function `head.toml` is read with.
    pub(crate) fn set_verbosity(&mut self, typed: &str) -> Option<Action> {
        let t = typed.trim().to_ascii_lowercase();
        let next = if t == "v" || t == "next" {
            // `/v` is the next profile in `Profile::ALL`'s order, and cycling one key's worth is a
            // promise this head made; the new setting only rules out having to cycle to learn the values.
            let at = self
                .visibility
                .profile()
                .and_then(|p| Profile::ALL.iter().position(|q| *q == p))
                .unwrap_or(0);
            Visibility::of(Profile::ALL[(at + 1) % Profile::ALL.len()])
        } else {
            match Visibility::parse_with(typed, &self.profiles) {
                Ok(Change::Set(v)) => v,
                Ok(Change::Switch(s, l)) => self.visibility.with(s, l),
                Err(said) => {
                    self.say(&said);
                    return None;
                }
            }
        };
        // What cannot be drawn is not stored ([`Visibility::undrawable`]): a profile that hides a switch
        // below one it shows puts a word on the status row the screen does not carry, and a profile that
        // changes nothing lies about the screen. The refusal names the switch and the way round it.
        if let Some(s) = next.undrawable() {
            // The sentence names the way round with the remedy first, because `say` trims it to the frame;
            // the ladder turns switches on in order, so only those below the offending one are named.
            const LADDER: [Show; 3] = [Show::Tools, Show::Thinking, Show::System];
            let below = LADDER.iter().position(|l| *l == s).unwrap_or(0);
            let missing: Vec<String> = LADDER[..below]
                .iter()
                .filter(|l| !next.shows(**l))
                .map(|l| format!("`{}`", l.name()))
                .collect();
            // `undrawable` only fires when one of those IS off, so the list is never empty - but
            // the sentence is built to read as a sentence anyway rather than to rely on it.
            let verb = if missing.len() == 1 { "is" } else { "are" };
            self.say(&format!(
                "`/verbosity {}=hidden` undoes it: `{}` {} drawn while {} {} off, and no rung \
                 draws that set — the ladder turns `tools`, `thinking` and `system` on in one \
                 order. Or choose a profile — {}",
                s.name(),
                s.name(),
                "is",
                names(&missing),
                verb,
                Profile::ALL
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return None;
        }
        let was = self.visibility;
        if next == was {
            // Nothing to do, and the notice would be a sentence about a change that did not
            // happen. (The card's Enter can land on the row already in force.)
            self.pick = None;
            return None;
        }
        self.visibility = next;
        // The body the set names moves with it, through the one writer: a profile saying `tools: folded`
        // sets the fold and not only the word. `hidden` is left alone, since the rows are the rung's business.
        for (show, level) in [
            (Show::Tools, next.level(Show::Tools)),
            (Show::Thinking, next.level(Show::Thinking)),
        ] {
            match level {
                Level::Open => self.set_fold(show, Fold::Open),
                Level::Folded => self.set_fold(show, Fold::Folded),
                Level::Hidden => {}
            }
        }
        self.raw_calls = next.shows(Show::RawCalls);
        // A set that hides rows can hide the one the reader is holding,
        // so the view moves onto its nearest surviving neighbour at the moment of the
        // change, where the fact is known for certain.
        self.reanchor_off_hidden();
        // Whatever was open is closed, because it was open in the other rendering: `payload_sel` names one
        // thing, and the same id means a different thing on either side of a rung, so carrying it across
        // opened a window on a row nobody asked about.
        self.payload_sel = None;
        self.payload_page = 0;
        // The one switch goes with it: a lift names a run under the rendering it was thrown in, and the
        // panel is the lift's only exit, so a lift without one cannot be closed by any key. Through
        // `drop_lift`, so the field has two writers and not three.
        self.drop_lift();
        self.invalidate_history();
        // And it is written down, which is what persisting across restarts asks for: the change and the
        // file are one act from here. `RetiredWrite::Union`, since this says nothing about the retired set.
        let saved = self.save_prefs(RetiredWrite::Union);
        self.say(&if next.hides_the_working() {
            format!(
                "verbosity {} (was {}) — the conversation and the edit cards, and nothing else \
                 the head made. Tool calls, reasoning and head arrivals are HIDDEN, not \
                 dropped: `/verbosity` brings them back and the span you had it on is drawn \
                 again. This applies to the whole transcript, already drawn.{saved}",
                next.as_str(),
                was.as_str()
            )
        } else {
            format!(
                "verbosity {} (was {}) — this applies to the whole transcript, already drawn, \
                 not only to what comes next.{saved}",
                next.as_str(),
                was.as_str()
            )
        });
        None
    }

    /// The one writer of the two folds - so a switch moved by a chord, by `/t`, by the
    /// config pane or by a profile all land in the same field, and none of them can move the
    /// other's.
    pub(crate) fn set_fold(&mut self, show: Show, fold: Fold) {
        match show {
            Show::Tools => {
                self.tools = fold;
                // `/t` moves one key for the long rows, and an echo's headline is one of
                // them, so the two travel together here as they did at every other
                // writing of this field.
                self.echo_open = fold.is_open();
            }
            Show::Thinking => self.reasoning = fold,
            Show::Edits | Show::System | Show::RawCalls => {}
        }
    }

    /// Set the diff style by name, or refuse by name. A local setting, in one place so the card's `Enter`
    /// and the typed word cannot disagree.
    pub(crate) fn set_diff(&mut self, typed: &str) -> Option<Action> {
        // `DiffPref::parse` and not a second list: the preference file is shared with the other head, so
        // the spellings a value can arrive in are already a fact of this tree, and a verb with its own
        // list would refuse a value the file accepts.
        let split = match crate::prefs::DiffPref::parse(typed) {
            Some(crate::prefs::DiffPref::Split) => true,
            Some(crate::prefs::DiffPref::Unified) => false,
            None => {
                self.say(&format!(
                    "`{typed}` is not a diff style — the two are {}; `/diff` with nothing after \
                     it shows what each one means",
                    DIFF_VALUES
                        .iter()
                        .map(|(v, _)| *v)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                return None;
            }
        };
        if split == self.diff_split {
            self.say(&format!(
                "diff is already {} — nothing changed",
                if split { "split" } else { "unified" }
            ));
            return None;
        }
        self.diff_split = split;
        // The history holds RENDERED rows, and a diff style decides what one of them
        // renders to - so the whole buffer is stale, not just the row that changed. The same
        // invalidation `/think` and `/t` make, for the same reason.
        self.invalidate_history();
        let saved = self.save_prefs(RetiredWrite::Union);
        self.say(&format!(
            "diff {} — every edit card, drawn and future{saved}",
            if split {
                "split (side by side)"
            } else {
                "unified"
            }
        ));
        None
    }

    /// A door verb the person typed, turned into the call the wire wants, or a sentence saying why not.
    /// The head knows only the three facts on the row: which field a bare line fills, what kind it is, and
    /// which fields have defaults. A row with no bare form returns the daemon's own `why_json`.
    pub(crate) fn head_run_call(
        &self,
        tool: &letibot_sessionlog::HeadRunTool,
        line: &str,
    ) -> Result<String, String> {
        let line = line.trim();
        if line.starts_with('{') {
            // Checked for BEING json and for nothing else - the original rule, kept.
            let v: serde_json::Value = serde_json::from_str(line)
                .map_err(|e| format!("`{}` with `{{…}}` arguments: {e}", tool.name))?;
            if !v.is_object() {
                return Err(format!(
                    "`{}` takes an object of arguments; `{line}` is a {}",
                    tool.name,
                    match v {
                        serde_json::Value::Array(_) => "list",
                        serde_json::Value::String(_) => "string",
                        serde_json::Value::Number(_) => "number",
                        serde_json::Value::Bool(_) => "boolean",
                        serde_json::Value::Null => "null",
                        serde_json::Value::Object(_) => "object",
                    }
                ));
            }
            return Ok(v.to_string());
        }
        if tool.field.is_empty() {
            return Err(tool.why_json.clone());
        }
        if line.is_empty() {
            return Err(format!(
                "/{} WHAT — this one puts a bare line into `{}` ({})",
                letibot_sessionlog::head_run_verb(&tool.name),
                tool.field,
                tool.kind
            ));
        }
        // The defaults travel with the line, so the object the daemon runs is the one a model's minimal
        // call would have produced; a head that dropped them would be editing the call.
        let mut obj = serde_json::Map::new();
        obj.insert(
            tool.field.clone(),
            serde_json::Value::String(line.to_string()),
        );
        for (k, v) in &tool.defaults {
            obj.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        Ok(serde_json::Value::Object(obj).to_string())
    }

    /// The providers this box holds a key for, from the daemon's own row. Empty when the row is absent,
    /// which is an older daemon: no greening rather than every row greened. The head cannot keep its own
    /// list, since whether a preset resolves a key is a fact about this box's files.
    pub(crate) fn keyed_providers(&self) -> Vec<String> {
        self.settings
            .iter()
            .find(|r| r.key == letibot_sessionlog::protocol::MODEL_KEYS_KEY)
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

    /// The rows that need no credential, from the daemon's own row: `local` and every local model declared
    /// in `providers.toml`. An absent row falls back to `local` alone, which is what this head did before.
    pub(crate) fn keyless_choices(&self) -> Vec<String> {
        self.settings
            .iter()
            .find(|r| r.key == letibot_sessionlog::protocol::MODEL_KEYLESS_KEY)
            .map(|r| {
                r.value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| vec!["local".to_string()])
    }

    /// Whether a picker row is one this box can actually take, so the colour answers *will this work if I
    /// press enter*. Two ways to be ready: a credential this box holds, or no credential wanted at all.
    /// The keyless list comes from the daemon because the daemon is the half that knows.
    pub(crate) fn choice_ready(&self, name: &str) -> bool {
        let name = name.trim();
        // Three ways a row can name its model: `preset/model` (the part before the slash), a declared name
        // whole, or `<name> (<model> at <url>)` where the first word is the name. Splitting that on `/`
        // lands inside the url instead.
        let first_word = name.split_whitespace().next().unwrap_or(name);
        let provider = first_word.split('/').next().unwrap_or(first_word);
        let keyless = self.keyless_choices();
        keyless
            .iter()
            .any(|k| k == name || k == first_word || k == provider)
            || self.keyed_providers().iter().any(|k| k == provider)
    }

    /// Did the daemon publish its key row at all? An absent row is an older daemon and reads as no greening,
    /// never as no keys, so an older daemon never finds its switches blocked behind a prompt.
    pub(crate) fn keys_row_present(&self) -> bool {
        self.settings
            .iter()
            .any(|r| r.key == letibot_sessionlog::protocol::MODEL_KEYS_KEY)
    }

    pub(crate) fn command(&mut self, cmd: &str) -> Option<Action> {
        // The person's own tool call first: a door verb is a tool, and it must be matched against the
        // published list before anything refuses it as an unknown word. Both spellings are accepted and
        // one is offered, so a hyphen is looked up and the tool's own name goes on the wire.
        let (typed_verb, rest) = match cmd.trim().split_once(char::is_whitespace) {
            Some((v, r)) => (v, r),
            None => (cmd.trim(), ""),
        };
        if !typed_verb.is_empty() {
            let tools = self.door_tools();
            let allow = letibot_sessionlog::HEAD_RUN_TOOLS;
            if let Some(tool) = letibot_sessionlog::head_run_tool(typed_verb, &allow)
                .and_then(|name| tools.iter().find(|t| t.name == name))
            {
                return match self.head_run_call(tool, rest) {
                    Ok(arguments) => Some(Action::HeadRun {
                        name: tool.name.clone(),
                        arguments,
                    }),
                    Err(why) => {
                        self.say(&why);
                        None
                    }
                };
            }
        }

        // `/cells MESSAGE`: the message and the screen with it, captured at Enter because a turn takes
        // seconds and the screen meant is the one that was there. Rendered here at this head's size,
        // escapes intact. An operator's mark: `app/feedback.rs`.
        if let Some(a) = self.feedback_command(cmd) {
            return a;
        }
        if let Some(rest) = verb_arg(cmd, "cells") {
            let message = rest.trim().to_string();
            let (w, h) = (self.term_cols, self.screen_rows);
            if w == 0 || h == 0 {
                // Nothing has been drawn yet, so there is nothing to send. Said
                // rather than sending an empty block that reads as a blank screen.
                self.say("nothing has been drawn on this head yet — no cells to send");
                return None;
            }
            let rows = self.screen(w, h);
            let mut text = if message.is_empty() {
                String::new()
            } else {
                format!("{message}\n\n")
            };
            // Delimited rather than introduced by a sentence, so both readers find the edges: the model
            // knows where the words stop and the picture starts, and the head knows which part of its own
            // transcript to fold away.
            text.push_str(&format!(
                "{CELLS_OPEN}{w}x{h} — my terminal exactly as this head drew it, ANSI \
                 escape codes included, so what you are reading IS the rendering and \
                 not a description of it{CELLS_MARK_END}\n"
            ));
            for r in &rows {
                text.push_str(r);
                text.push('\n');
            }
            text.push_str(CELLS_CLOSE);
            text.push('\n');
            self.scroll = 0;
            // The same string that was sent: the pending row is cleared by matching the item the daemon
            // appends, so an abbreviation never matches and the `queued` line never leaves. The screen is
            // taken out at render time instead, which is where that decision belongs.
            self.pending_prompts.push(text.clone());
            return Some(Action::Prompt(Prompt::new(text)));
        }
        if let Some(title) = verb_arg(cmd, "new") {
            self.want_new_session = true;
            self.say("making a session…");
            return Some(Action::NewSession(title.trim().to_string()));
        }
        if cmd == "copy" {
            self.copy_command();
            return None;
        }
        if matches!(cmd, "sessions" | "s") {
            return self.open_surface(Surface::Picker);
        }
        if let Some(id) = cmd.strip_prefix("switch ") {
            return self.pick(id.trim());
        }
        // `/forget` is the destructive sense of removing a session, and it says no: a letter bound in the
        // picker is matched when the composer is empty, which is the first character of a filter, and a
        // line starting with `/` cannot be one. See `app::removal`.
        if let Some(what) = verb_arg(cmd, "forget") {
            self.forget_named(what);
            return None;
        }
        // Renames the session this head is IN and not an arbitrary one: the picker is where another session is on
        // screen, and a `/rename` that could reach a row you were looking at is one typo from renaming the wrong
        // conversation.
        for verb in ["invite", "leave"] {
            if let Some(args) = verb_arg(cmd, verb) {
                return self.room_guest(verb, args);
            }
        }
        if let Some(title) = verb_arg(cmd, "rename") {
            let title = title.trim().to_string();
            if self.session_id.is_empty() {
                self.say("not attached to a session yet");
                return None;
            }
            if title.is_empty() {
                self.say("/rename NAME — or /rename with nothing clears the name");
            }
            return Some(Action::Rename {
                session_id: self.session_id.clone(),
                title,
            });
        }
        if let Some(name) = verb_arg(cmd, "mode") {
            let name = name.trim().to_string();
            if self.session_id.is_empty() {
                self.say("not attached to a session yet");
                return None;
            }
            if name.is_empty() {
                // Bare `/mode` opens the picker rather than printing a list to copy a name out of. The rows come from
                // the daemon's last answer, and asking again is what keeps the `← now` marker honest.
                self.pick = Some(Pick::Mode);
                self.close_surface(Surface::Picker);
                self.close_surface(Surface::Config);
                self.seed_pick();
                return Some(Action::Settings);
            }
            return self.mode_action(name);
        }
        match cmd {
            "quit" | "q" => {
                self.quit = true;
                Some(Action::Quit)
            }
            "resync" => Some(Action::Resync),
            // `/notes` is the disclosures this head has shown, and `/dismiss` is the same action under the word a
            // person types at a wall of red. Both land in `notes_command`, so the two spellings cannot disagree about
            // what retired means.
            _ if verb_arg(cmd, "notes")
                .or_else(|| verb_arg(cmd, "dismiss"))
                .is_some() =>
            {
                let (verb, rest) = if let Some(rest) = verb_arg(cmd, "notes") {
                    ("notes", rest)
                } else {
                    ("dismiss", verb_arg(cmd, "dismiss").unwrap_or(""))
                };
                self.notes_command(verb, rest)
            }
            "help" | "h" | "?" => self.toggle_surface(Surface::Help),
            // Where the bottom border's telemetry went. See `App::stats`.
            "status" | "stats" => {
                // Opening the screen IS the acknowledgement: the `⚠` on the composer's edge is a pointer at these
                // numbers, so going to read them is what clears it, and there is no separate key.
                self.toggle_surface(Surface::Stats)
            }
            "think" | "r" => {
                // The one verb, not a second writer of the fold: `/think`, `ctrl-r`, the config row, a click on the
                // marker's thinking clause and `esc` all ask [`App::flip_thinking`], so none can move a field the
                // others do not.
                self.flip_thinking()
            }
            // `/tools` asks what this conversation can call. It is not a second spelling of ctrl-t, which
            // already folds tool output and is the key anybody uses for it.
            // With a run open it is that run's rows the verb unfolds, as the rows' own seams say.
            "t" if self.flip_run_tools() => None,
            "t" => {
                self.tools = self.tools.flip();
                // One key for the long rows, and that includes an echo: a queued prompt is one elided headline and
                // this opens it, since a second fold chord for a second kind of row is a second thing to learn.
                self.echo_open = !self.echo_open;
                self.refold();
                None
            }
            // Bare, it opens the card; named, it sets the rung. A setting with more than two values is chosen from a
            // card showing all of them, and cycling makes the reader hold the list in their head.
            _ if verb_arg(cmd, "verbosity").is_some() => {
                let rest = verb_arg(cmd, "verbosity").unwrap_or("").trim().to_string();
                if rest.is_empty() {
                    self.pick = Some(Pick::Verbosity);
                    self.close_surface(Surface::Picker);
                    self.close_surface(Surface::Config);
                    self.seed_pick();
                    return None;
                }
                self.set_verbosity(&rest)
            }
            // `/diff` - the diff layout setting. A refused slash would be
            // the card this setting exists to avoid.
            // `/repo` - the repo card, which a click on the header's git field also opens.
            _ if verb_arg(cmd, "repo").is_some() => {
                self.git_repos = crate::gitfield::repos_in(&self.wiring.workspace);
                self.repo_rows = self.read_repo_rows();
                self.pick = Some(Pick::Repo);
                self.close_surface(Surface::Picker);
                self.close_surface(Surface::Config);
                self.seed_pick();
                None
            }
            _ if verb_arg(cmd, "diff").is_some() => {
                let rest = verb_arg(cmd, "diff").unwrap_or("").trim().to_string();
                if rest.is_empty() {
                    self.pick = Some(Pick::Diff);
                    self.close_surface(Surface::Picker);
                    self.close_surface(Surface::Config);
                    self.seed_pick();
                    return None;
                }
                self.set_diff(&rest)
            }
            // `/v` keeps meaning the next rung: it is an alias in `HEAD_COMMAND_ALIASES`, deliberately not offered by
            // tab and deliberately still taken, since one key's worth of cycling is a promise this head made.
            "v" => self.set_verbosity("v"),
            // One list on the screen at a time, and the settings asked for on the way in - both
            // in `open_surface`'s arm for `Config`.
            "config" | "settings" => self.toggle_surface(Surface::Config),
            // `/jobs` is `ctrl-q`'s other spelling - the same function the chord and a click
            // on the count label run, so the three cannot drift.
            "jobs" => self.toggle_surface(Surface::Jobs),
            // Every question waiting on you, answering none - see `app/waiting.rs`.
            "asks" => {
                self.slash_out = Some(("/asks".to_string(), self.asks_lines()));
                self.push_surface(Surface::Listing);
                None
            }
            // What this session is watching - the monitors the model armed.
            "monitors" => self.toggle_surface(Surface::Monitors),
            // The plugin panes this session has - C3/C4's `pane_` hooks. Bare, it is the
            // list of them by title; `/pane NAME` opens one, through the same door a row of
            // that list names, and asks the daemon for a fresh look as it opens.
            "pane" => {
                self.slash_out = Some(("/pane".to_string(), self.plugin_listing()));
                self.push_surface(Surface::Listing);
                None
            }
            _ if verb_arg(cmd, "pane").is_some() => {
                let name = verb_arg(cmd, "pane").unwrap_or("").trim().to_string();
                self.open_plugin(&name)
            }
            // `/plugin` lists the layers a person placed with what the book says about each, and `/plugin trust NAME`
            // pins the bytes on disk now. It is the person's door and the `plugin` tool has no such verb.
            "plugin" => {
                self.slash_out = Some(("/plugin".to_string(), self.plugin_trust_listing()));
                self.push_surface(Surface::Listing);
                None
            }
            _ if verb_arg(cmd, "plugin").is_some() => {
                let said = self.trust_plugin(verb_arg(cmd, "plugin").unwrap_or(""));
                self.say(&said);
                None
            }
            // The merge queue, which is not this session's: there is one queue, so the pane asks for the whole thing
            // and the daemon answers with it, scoped to this session's repository unless a word says otherwise.
            "queue" => {
                if !self.is_open(Surface::Queue) && self.queue_scope != QueueScope::Here {
                    self.queue_scope = QueueScope::Here;
                    self.rescope_queues();
                }
                self.toggle_surface(Surface::Queue)
            }
            _ if verb_arg(cmd, "queue") == Some("builtin") => {
                self.say(
                    "there is no built-in queue pane any more - the `queue` layer draws it, and \
                     `/queue plugin` asks that layer again",
                );
                None
            }
            _ if verb_arg(cmd, "queue") == Some("plugin") => {
                self.say("the queue pane is drawn by the `queue` layer");
                self.queue_drawing_again()
            }
            _ if verb_arg(cmd, "queue") == Some("all") => self.queue_in(QueueScope::All),
            _ if verb_arg(cmd, "queue")
                .is_some_and(|a| a.starts_with('/') && !a.contains(char::is_whitespace)) =>
            {
                let p = verb_arg(cmd, "queue").unwrap_or_default().to_string();
                self.queue_in(QueueScope::Path(p))
            }
            // `/standing` is the standing notes and not `/notes`: the word is the module's own, since `standing_notes`
            // is what the harness reads into the system prompt and the two must not be confused.
            "standing" => self.toggle_surface(Surface::Standing),
            // `/skills` is the box's skills, and `/skills NAME rest` hands one over: the body is the daemon's to
            // inject, since it loads the skill, writes the person's message and runs the turn.
            "skills" => self.toggle_surface(Surface::Skills),
            "rooms" => self.toggle_surface(Surface::Rooms),
            // `/search TEXT` over the channel the room view has selected: the loaded messages, since
            // the ring is the history. `/search` with no text closes it again.
            _ if verb_arg(cmd, "search").is_some() => {
                let rest = verb_arg(cmd, "search").unwrap_or("").to_string();
                if !self.is_open(Surface::Rooms) {
                    self.say("/search works on the channel /rooms has open — open it first");
                    return None;
                }
                self.rooms_search(&rest);
                None
            }
            _ if verb_arg(cmd, "dm").is_some() => {
                let rest = verb_arg(cmd, "dm").unwrap_or("").to_string();
                self.rooms_dm(&rest)
            }
            _ if verb_arg(cmd, "skills").is_some() => {
                let rest = verb_arg(cmd, "skills").unwrap_or("").trim();
                let (name, then) = match rest.split_once(char::is_whitespace) {
                    Some((n, t)) => (n.trim().to_string(), t.trim().to_string()),
                    None => (rest.to_string(), String::new()),
                };
                if name.is_empty() {
                    self.say("/skills NAME — one of the names the skills pane lists");
                    return None;
                }
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                Some(Action::LoadSkill { name, then })
            }
            // The panes and the promote, reachable as verbs: each had a chord and no word, so a head driven over a
            // pipe and a person who had not learnt the chord could not reach them at all.
            "todos" => self.toggle_surface(Surface::Todos),
            _ if verb_arg(cmd, "todo").is_some() => {
                self.todo_command(verb_arg(cmd, "todo").unwrap_or(""))
            }
            "subagents" => self.toggle_surface(Surface::Subagents),
            "promote" => self.promote(),
            // `/peek ID` reads one subagent's scrollback into the pane the tree's Enter opens. An id the daemon does
            // not hold is refused by name, since the head keeps no list of subagents to validate against.
            _ if verb_arg(cmd, "peek").is_some() => {
                let id = verb_arg(cmd, "peek").unwrap_or("").trim().to_string();
                if id.is_empty() {
                    self.say("/peek ID — the subagent to read; ctrl-g lists them");
                    return None;
                }
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                self.ask_peek(&id);
                Some(Action::Peek(id))
            }
            // `/resume ID` brings a session that is on disk but not in this daemon back, and goes there: the same two
            // steps `switch_to` takes for a row the picker labels *on disk*.
            _ if verb_arg(cmd, "resume").is_some() => {
                let id = verb_arg(cmd, "resume").unwrap_or("").trim().to_string();
                if id.is_empty() {
                    self.say(
                        "/resume ID — a session on disk that this daemon is not holding; \
                         /sessions lists them",
                    );
                    return None;
                }
                self.want_new_session = true;
                self.say(&format!(
                    "resuming {} from the store…",
                    self.session_label(&id)
                ));
                Some(Action::ResumeSession(id))
            }
            "interrupt" | "i" => Some(Action::Interrupt("operator typed /interrupt".into())),
            "compact" => {
                // The session this head is in, for the reason `/rename` refuses an arbitrary id: a compaction that
                // could reach a row you were only looking at is one typo from summarising the wrong conversation.
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                // The fork this head is asking for. The daemon announces a
                // manual compaction only when it has finished (`compacted`), so the
                // mark has to be taken here, on the way out.
                self.mark_fork();
                Some(Action::Compact)
            }
            // The tool list is in the prompt and a prompt is fixed for a conversation, so this is the only way a
            // session that opened without a shell ever gets one: summarise, and continue under a prompt built from
            // what is seated now.
            "reseat" | "reseat keep" | "reseat verbatim" | "reseat summarise"
            | "reseat summarize" => {
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                let summarise = cmd.ends_with("summarise") || cmd.ends_with("summarize");
                // The same mark, for the other fork.
                self.mark_fork();
                if summarise {
                    self.say("re-seating: summarising, so the summary replaces the conversation…");
                } else {
                    self.say(
                        "re-seating: carrying the conversation across as it is. The next \
                         turn re-sends all of it once.",
                    );
                }
                Some(Action::Reseat { summarise })
            }
            other => {
                // The daemon's verbs. The head does not know them and does not
                // need to: the line goes over as typed and the answer comes back
                // on the session log.
                let verb = other.split_whitespace().next().unwrap_or("");
                // Bare `/models` is the menu: with a name after it the line goes to the daemon as typed, which is what
                // `/models X --once` and `/models X --key K` need.
                if matches!(verb, "models" | "model") && other.trim() == verb {
                    // Seeded to what answers now, so Enter on an untouched list
                    // is a no-op - the same courtesy the mode picker pays. One
                    // opener, shared with the unreachable-model check (`app::reach`).
                    if !self.open_model_pick() {
                        return None;
                    }
                    // `Settings` and not the `models` verb: asking the daemon to refresh is right, since a session whose
                    // model moved in another head would draw a stale `← now`, but `/models` with no argument answers with
                    // the whole provider listing.
                    return Some(Action::Settings);
                }
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                // X23: a line this head has not got is offered to `command_<verb>` before it is the daemon's. A layer
                // that answered it owns it; one that passed leaves the line exactly as it was, and the head keeps no list
                // of the daemon's verbs. The layer is handed the REST of the line, the shape `/todo`'s layer has.
                let rest = other
                    .trim()
                    .strip_prefix(verb)
                    .map(str::trim)
                    .unwrap_or_default();
                if let Some(taken) = self.command_layer(verb, rest) {
                    return taken;
                }
                let mut line = other.trim().to_string();
                if verb == "listen" {
                    line = self.listen_line_with_this(&line);
                }
                Some(Action::Slash { line })
            }
        }
    }

    /// `/listen only this`: `this` is the repository the header names, which only the head knows.
    fn listen_line_with_this(&self, line: &str) -> String {
        let here = self.header_workspace();
        let name = std::path::Path::new(&here)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        line.split_whitespace()
            .map(|w| if w == "this" && !name.is_empty() { name.as_str() } else { w })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub(crate) fn todo_command(&mut self, rest: &str) -> Option<Action> {
        let rest = rest.trim();
        if self.session_id.is_empty() {
            self.say("not attached to a session yet");
            return None;
        }
        let mut mine = self.operator_todos();
        // The state moves this command makes on rows that are not the operator's - see the
        // `postpone|resume` arm below, which is the only verb that can make one. Sent beside the
        // half, in one frame, because both are the board as the operator wants it.
        let mut moved: Vec<letibot_sessionlog::event::TodoState> = Vec::new();
        // `done N` and `rm N` - a number is what the pane prints beside each of these rows.
        let (verb, arg) = match rest.split_once(char::is_whitespace) {
            Some((v, a)) => (v, a.trim()),
            None => (rest, ""),
        };
        // `/todo title:"..." body:"..."` and `/todo buy milk` add a row, and the words are the `todo` layer's to
        // parse: what a new row may be spelled like is a project's policy, and the write stays the head's.
        if !is_own_verb(verb) && !rest.is_empty() {
            let input = serde_json::json!({"line": rest, "feeds": self.command_feeds()});
            let asked = {
                let host = self.plugin_host();
                host.as_ref().map(|h| h.command("todo", &input))
            };
            // A layer that could not answer is said HERE, because this is the moment it was asked: the stack already
            // fell through to the layer below, and `Host::command` kept the sentence for whoever has a screen.
            let broke = self.take_sessions_failure();
            if let Some(answer) = asked.and_then(|r| r.answer) {
                let done = match add_todo_words(&answer) {
                    Some(content) => self.add_operator_row(content, mine, moved),
                    None => {
                        // Said, not swallowed. The line did nothing, and the person who typed it
                        // is owed the reason: a layer may ask only for what its `plugin.toml`
                        // declares, and this head performs exactly one act name.
                        let what = act_name(&answer).unwrap_or("no act at all");
                        let what: String = what.chars().take(40).collect();
                        self.say(&format!(
                            "the `todo` layer answered `{what}`, and this head performs only \
                             `add_todo` — nothing was added"
                        ));
                        None
                    }
                };
                if let Some(why) = broke {
                    self.say(&why);
                }
                return done;
            }
            // The layer passed, so the line is the head's exactly as it was before there was a layer, and the match
            // below answers it. A failure met on the way here is held and not said, since `say` keeps only the last
            // line.
            self.layer_failure = broke;
        }
        match (verb, arg) {
            ("done" | "rm", n) if !n.is_empty() => {
                let Ok(at) = n.parse::<usize>() else {
                    self.say(&format!("`{n}` is not a row number — `/todo` lists yours"));
                    return None;
                };
                if at < 1 || at > mine.len() {
                    self.say(&format!(
                        "there is no row {at} of yours — you have {}",
                        mine.len()
                    ));
                    return None;
                }
                if verb == "rm" {
                    // Struck off and not removed: the row is marked and kept, because everything that was ever created
                    // stays in history and only the person deletes.
                    mine[at - 1].status = letibot_sessionlog::event::TodoStatus::Cancelled;
                    self.say(&format!(
                        "row {at} is struck off — it stays on the board, marked `[c]`, and the \
                         model still sees it; `/todo resume {at}` puts it back"
                    ));
                } else {
                    mine[at - 1].status = letibot_sessionlog::event::TodoStatus::Completed;
                    self.say(&format!("row {at} is done"));
                }
            }
            // `postpone N` and `resume N`: the state the person owns, over the same numbers every other verb uses and
            // over the words of a row that has no number. A row the model wrote had no name any verb could use.
            ("postpone" | "resume", n) if !n.is_empty() => {
                use letibot_sessionlog::event::{TodoBy, TodoState, TodoStatus};
                let set_aside = verb == "postpone";
                let status = if set_aside {
                    TodoStatus::Postponed
                } else {
                    TodoStatus::Pending
                };
                // A NUMBER is one of the operator's own rows - the numbers the pane prints.
                if let Ok(at) = n.parse::<usize>() {
                    if at < 1 || at > mine.len() {
                        self.say(&format!(
                            "there is no row {at} of yours — you have {}",
                            mine.len()
                        ));
                        return None;
                    }
                    mine[at - 1].status = status;
                    if set_aside {
                        self.say(&format!(
                            "row {at} is set aside — it stays on your list and the model still sees \
                             it, and nothing is reminded of it until you lift it with `/todo resume \
                             {at}`"
                        ));
                    } else {
                        self.say(&format!(
                            "row {at} is back in the list — the check may ask about it again"
                        ));
                    }
                    // The optimistic view and the frame are the function's tail: `mine` has moved.
                } else {
                    // Anything else is the row's own WORDS, over the whole board.
                    let want = n.trim();
                    let hit: Option<(String, TodoBy)> = self
                        .todos
                        .iter()
                        .find(|t| t.content.trim() == want)
                        .map(|t| (t.content.clone(), t.by.clone()));
                    // More than one row can say the same words - the halves are separate lists and
                    // nothing stops a model and a person writing the same sentence.
                    let ambiguous = self
                        .todos
                        .iter()
                        .filter(|t| t.content.trim() == want)
                        .count()
                        > 1;
                    let Some((content, by)) = hit else {
                        self.say(&format!(
                            "no row this head knows says `{want}` — open the todos pane (ctrl-t) so \
                             it has read the board, and quote the row's own words exactly; a number \
                             (`/todo postpone N`) is one of YOUR rows"
                        ));
                        return None;
                    };
                    if ambiguous {
                        self.say(&format!(
                            "two rows say `{want}`, so which one moves is not something this can \
                             know — quote the one you mean, or use its number if it is yours"
                        ));
                        return None;
                    }
                    match by {
                        TodoBy::Operator => {
                            if let Some(t) = mine.iter_mut().find(|t| t.content == content) {
                                t.status = status;
                            }
                        }
                        TodoBy::Model => {
                            moved.push(TodoState {
                                content: content.clone(),
                                status,
                            });
                            // Shown at once, the same trust `echo_operator_todos` places: the
                            // write publishes when it lands, and until then the pane draws what was
                            // sent rather than a row that has not moved yet.
                            if let Some(t) = self.todos.iter_mut().find(|t| t.content == content) {
                                t.status = status;
                            }
                        }
                        TodoBy::Parent(who) => {
                            self.say(&format!(
                                "that row is {who}'s — a parent's rows are the parent's to retire, \
                                 and a state set here would be taken back by its next write"
                            ));
                            return None;
                        }
                        // A queue row is the merge queue's and not a state anybody sets: it comes off when that entry's
                        // review is answered, and the daemon replaces the half.
                        TodoBy::Queue => {
                            self.say(
                                "that row is the merge queue's — it is one entry of this daemon's \
                                 review queue, shown on this session's board, and it comes off when \
                                 the entry's review is answered. Nothing here moves it.",
                            );
                            return None;
                        }
                    }
                    if set_aside {
                        self.say(&format!(
                            "`{content}` is set aside — it stays on the board and the model still \
                             sees it, and nothing is reminded of it until you lift it with `/todo \
                             resume`"
                        ));
                    } else {
                        self.say(&format!(
                            "`{content}` is back in the list — the check may ask about it again"
                        ));
                    }
                }
            }
            // `when N JOB`: the condition attached by number, since *job ends and then this* is a todo row conditioned
            // on the job's status.
            ("when", both) => {
                let Some((n, handle)) = both.split_once(char::is_whitespace) else {
                    self.say(
                        "`/todo when N JOB` — a row number and the handle it waits on. \
                         `/todo when N -` takes the condition off.",
                    );
                    return None;
                };
                let (n, handle) = (n.trim(), handle.trim());
                let Ok(at) = n.parse::<usize>() else {
                    self.say(&format!("`{n}` is not a row number — `/todo` lists yours"));
                    return None;
                };
                if at < 1 || at > mine.len() {
                    self.say(&format!(
                        "there is no row {at} of yours — you have {}",
                        mine.len()
                    ));
                    return None;
                }
                if handle == "-" {
                    mine[at - 1].when = None;
                    self.say(&format!("row {at} no longer waits on anything"));
                } else {
                    mine[at - 1].when = Some(letibot_sessionlog::event::TodoCondition::Job {
                        handle: handle.to_string(),
                    });
                    self.say(&format!(
                        "row {at} is due once `{handle}` is not running — a job this daemon has \
                         never heard of counts as ended, which is what a restart looks like."
                    ));
                }
            }
            // Anything else is the text of a new row, including a line that begins with a number or with `done` and no
            // argument, since those are sentences somebody could type. Reached only when no layer answered.
            _ if !rest.is_empty() => return self.add_operator_row(rest.to_string(), mine, moved),
            // Bare `/todo` opens the card, which is the shape `/mode` and `/models` keep: a
            // setting or an act with more than one part is CHOSEN from a card rather than typed
            // blind. The three forms are still there and the card's hint line says where.
            _ => {
                self.open_todo_card();
                return None;
            }
        }
        self.echo_operator_todos(mine.clone());
        Some(Action::SetOperatorTodos { items: mine, moved })
    }

    /// One row on the person's own half of the board: the single door an add goes through, whichever half parsed the
    /// words. Two readers of a `/todo` line now, and a row added by one must be the same act as the other.
    fn add_operator_row(
        &mut self,
        content: String,
        mut mine: Vec<letibot_sessionlog::event::TodoEntry>,
        moved: Vec<letibot_sessionlog::event::TodoState>,
    ) -> Option<Action> {
        // The layer's failure is said LAST, after this row's own sentence, because `say` keeps only the last line:
        // said first, the row overwrites it and a session whose layer is broken adds rows silently.
        let broke = self.layer_failure.take();
        if let Some(why) = row_refusal(&content) {
            self.say(&why);
            if let Some(why) = broke {
                self.say(&why);
            }
            return None;
        }
        mine.push(letibot_sessionlog::event::TodoEntry {
            content,
            status: letibot_sessionlog::event::TodoStatus::Pending,
            by: letibot_sessionlog::event::TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        });
        self.say(&format!("added to your list — {} row(s)", mine.len()));
        if let Some(why) = broke {
            self.say(&why);
        }
        self.echo_operator_todos(mine.clone());
        Some(Action::SetOperatorTodos { items: mine, moved })
    }

    /// X23: one `/` line the head has not got, offered to `command_<verb>` before the daemon. The outer
    /// `Option` is *was the line TAKEN* - a layer that answered owns the verb, and the line does not also go
    /// to the daemon; the inner is what to do about it, `None` when the head said why it would not. A layer
    /// that passed (`-1`), trapped, or does not exist is `None`, which is the shape every `/unknown` had
    /// before there was a command hook.
    fn command_layer(&mut self, verb: &str, line: &str) -> Option<Option<Action>> {
        let input = serde_json::json!({
            "v": letibot_plugin::V,
            "verb": verb,
            "line": line,
            "session": &self.session_id,
            "feeds": self.command_feeds(),
        });
        let asked = {
            let host = self.plugin_host();
            host.as_ref().map(|h| h.command(verb, &input))
        };
        // The layer's failure is said HERE, at the moment it was asked: the stack fell through to the layer below, and a
        // person whose layer is broken is owed the sentence rather than a silent fall-through.
        let broke = self.take_sessions_failure();
        let Some(answer) = asked.and_then(|r| r.answer) else {
            if let Some(why) = broke {
                self.say(&why);
            }
            return None;
        };
        if let Some(content) = add_todo_words(&answer) {
            let mine = self.operator_todos();
            self.layer_failure = broke;
            return Some(self.add_operator_row(content, mine, Vec::new()));
        }
        // The other act this head performs: a line said to the person, which is what a verb that
        // only answers something needs. Anything else is named rather than half-performed.
        if let Some(text) = said_words(&answer) {
            if let Some(why) = broke {
                self.say(&why);
            }
            self.say(&text);
            return Some(None);
        }
        if let Some(refusal) = answer.get("refusal").and_then(|r| r.as_str()) {
            self.say(refusal);
            return Some(None);
        }
        let what = act_name(&answer).unwrap_or("no act at all");
        let what: String = what.chars().take(40).collect();
        self.say(&format!(
            "the `{verb}` layer answered `{what}`, and this head performs only `add_todo` and \
             `say` — nothing was done, and the line was not sent to the daemon"
        ));
        Some(None)
    }

    /// What a `command_` layer is handed beside the line - the `feeds` object of
    /// `docs/plugins-plan.md` Feeds section, in the head's own reading of it.
    ///
    /// A feed is a fact the head ALREADY HOLDS and shows; handing it over adjudicates and grants nothing, so it is a field on the
    /// input. Each shape is written by hand, not `serde_json::to_value` of the head's struct, so a new field is not silently contract:
    ///
    /// ```jsonc
    /// {"todos":      [{"content": "…", "status": "pending", "by": "operator"}],
    ///  "session":    {"id": "s-1793…", "workspace": "/home/dead/Projects/x"},
    ///  "repo_todos": [{"text": "T1 git init", "mark": "[x]", "indent": 4, "item": true}]}
    /// ```
    ///
    /// `todos` is the WHOLE board (model's and operator's halves) in board order, with the wire's words for `status` and `by`.
    /// `session` is where the head stands: the id and folder. `repo_todos` is the workspace's `TODO.md` as the todos pane drew it,
    /// absent (not empty) when the head has not read the file, since `[]` would say the file is empty.
    /// Not here: a row's `when` and `needs`, which no act in `actions` can move.
    fn command_feeds(&self) -> serde_json::Value {
        let todos: Vec<serde_json::Value> = self
            .todos
            .iter()
            .map(|t| {
                serde_json::json!({
                    "content": &t.content,
                    "status": t.status,
                    "by": t.by,
                })
            })
            .collect();
        let mut feeds = serde_json::Map::new();
        feeds.insert("todos".into(), serde_json::Value::Array(todos));
        feeds.insert(
            "session".into(),
            serde_json::json!({
                "id": &self.session_id,
                "workspace": self.workspace_path().to_string_lossy(),
            }),
        );
        if let Some(rows) = &self.repo_todos {
            let rows: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "text": &r.text,
                        "mark": r.mark.map(|m| m.glyph()),
                        "indent": r.indent,
                        "item": r.item,
                    })
                })
                .collect();
            feeds.insert("repo_todos".into(), serde_json::Value::Array(rows));
        }
        serde_json::Value::Object(feeds)
    }

    /// `/notes`: what this head has shown, and how to retire it. Three verbs in one subject, since nothing listed the
    /// notes, nothing retired one, and a reader who has just retired the wall needs a way back if they were wrong.
    pub(crate) fn notes_command(&mut self, verb: &str, rest: &str) -> Option<Action> {
        // The listing is the moment to find out what the file says: `load_prefs` ran once at startup, so a head up for
        // hours had a `dismissed` that only grew from its own presses, and `/notes` showed a retired note as live.
        self.refresh_retired();
        // `/dismiss` is the same action under the word the operator would type at a
        // red wall; `/notes dismiss` is where it is documented.
        let rest = if verb == "dismiss" && rest.is_empty() {
            "all"
        } else {
            rest
        };
        match rest {
            "" => {
                let lines = self.notes_lines();
                self.slash_out = Some(("/notes".to_string(), lines));
                self.push_surface(Surface::Listing);
                None
            }
            "restore" | "back" | "undismiss" => {
                let back = self.dismissed.len();
                self.dismissed.clear();
                // And the watermark goes with the key set, because it is the same statement: a restore asserts that
                // nothing this head was hiding stays hidden, and leaving the marks up would hide the wall while `/notes`
                // said otherwise.
                self.read_marks.clear();
                self.invalidate_history();
                // `Replace`, and this is the whole of the third item: under a union the save wrote the file's keys
                // straight back, so the dismissal survived on disk while the head believed it had restored.
                let saved = self.save_prefs(RetiredWrite::Replace);
                self.say(&if back == 0 {
                    "nothing was retired, so nothing came back".to_string()
                } else {
                    format!(
                        "{back} retired note(s) back on the screen — the log was never \
                         the thing they were hidden from{saved}"
                    )
                });
                None
            }
            other => {
                // `dismiss` is the word itself: `/notes dismiss` and `/dismiss all`
                // both land here.
                let arg = other.strip_prefix("dismiss").map(str::trim).unwrap_or("");
                // `ctrl-n`'s arm and the one place the watermark is set: all of them is a statement about the reader's
                // reading and not about a list of copies, so it leaves a mark per kind as well as the key set.
                let all = matches!(arg, "" | "all");
                let keys: Vec<String> = match arg {
                    "" | "all" => self.notes.iter().map(|(_, n)| note_key(n)).collect(),
                    n => {
                        let Some(n) = n.parse::<usize>().ok().filter(|k| *k >= 1) else {
                            self.say(&format!(
                                "`{n}` is not a number — `/notes` lists them, and \
                                 `/notes dismiss N` retires the Nth"
                            ));
                            return None;
                        };
                        match self.notes.get(n - 1) {
                            Some((_, note)) => vec![note_key(note)],
                            None => {
                                self.say(&format!(
                                    "there is no note {n} — `/notes` lists the {} this \
                                     head holds",
                                    self.notes.len()
                                ));
                                return None;
                            }
                        }
                    }
                };
                let hidden = self.retire(keys);
                // The mark AFTER the keys, and the order is load-bearing: `retire` answers how many this press newly
                // hid, and a mark set first would have hidden them through `hide_read` and made the answer zero.
                if all {
                    self.mark_read();
                }
                // Nothing to retire means nothing LEFT to retire and not no notes held: a retired note stays in
                // `notes` on purpose, so after a first press the set is all retired rather than empty.
                if all && hidden == 0 {
                    self.say("nothing to retire");
                    return None;
                }
                // Union. A dismissal asserts a key IS retired, and no other head's save
                // is evidence to the contrary - this is the write `merge_retired` exists for.
                let saved = self.save_prefs(RetiredWrite::Union);
                self.say(&format!(
                    "retired {hidden} note(s) — hidden, still counted on /status, and \
                     `/notes` shows them{saved}"
                ));
                None
            }
        }
    }
}

pub(crate) const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("new", "TITLE — start a fresh session"),
    ("sessions", "the session picker"),
    ("switch", "ID or list number — go to another session"),
    (
        "forget",
        "ID — forget a session in the store (refused: see the sentence it answers)",
    ),
    ("rename", "NAME — name the session you are in"),
    (
        "invite",
        "SESSION [ROOM] - let a session hear and speak in another project's room",
    ),
    (
        "leave",
        "SESSION [ROOM] - take an invited session out of a project room",
    ),
    ("help", "the key and command reference"),
    ("status", "the bottom border's telemetry, full screen"),
    ("think", "fold or unfold the model's reasoning"),
    (
        "tools",
        "what this conversation can call, and what it only looks like it can",
    ),
    (
        "default-model",
        "what a NEW session starts on; /models switches this one",
    ),
    (
        "verbosity",
        "how much reaches the transcript: the card, or a rung by name",
    ),
    ("diff", "how a diff is drawn: unified or side by side"),
    (
        "repo",
        "which of the workspace's repositories the header follows",
    ),
    (
        "copy",
        "the open ctrl-v output, or the last reply, onto the clipboard",
    ),
    ("notes", "what this head has shown — and how to retire one"),
    (
        "config",
        "every setting, the runtime-editable ones editable in place",
    ),
    ("mode", "the mode picker — or /mode NAME to type it"),
    ("jobs", "open or close the background-jobs pane"),
    (
        "asks",
        "every question waiting on you, listed whole without answering any",
    ),
    (
        "monitors",
        "open or close the monitors pane: what this session is watching",
    ),
    (
        "pane",
        "the plugin panes this session has — /pane NAME opens one: a dashboard a plugin \
         describes, drawn by this head",
    ),
    (
        "plugin",
        "the plugin layers a person placed here, and whether each is pinned — /plugin trust NAME \
         pins the bytes on disk now (a built-in needs no pin: it is this binary's own), \
         /plugin untrust NAME takes a pin back",
    ),
    (
        "queue",
        "open or close the merge-queue pane: what is landing on main, and why it is not",
    ),
    // `/skills NAME` hands the body over; the daemon injects it, so the head sends the name and
    // the rest of the line.
    (
        "skills",
        "the skills this box has, one row each: enter opens the skill's file in the editor, `s` \
         (or `/skills NAME`) hands its body to the session, and `/skills NAME rest of line` \
         sends the rest on as your prompt in the same turn",
    ),
    (
        "rooms",
        "open or close the chat rooms: the host room, one room per project and your DMs; what you \
         type posts as operator",
    ),
    (
        "dm",
        "NAME TEXT - a direct message as operator, in the rooms view",
    ),
    (
        "search",
        "TEXT - matches in the channel /rooms has open; `n` and `N` step through them, and \
         `/search` with nothing after it closes the search",
    ),
    (
        "standing",
        "open or close the standing-notes pane: every note the harness reads into the prompt, \
         with the form the budget gave each one — verbatim, or an index over it",
    ),
    // The verbs and the ruling that the table is the union: five of these had a chord and no word, so they were
    // unreachable from a pipe and `/help` had no name for them, and two were implemented and simply not listed.
    ("todos", "open or close the todos pane (ctrl-t)"),
    (
        "todo",
        "TEXT adds one of YOUR rows · done N · rm N · postpone N · resume N — N numbers your half; \
         `postpone`/`resume` also take a row's own WORDS, which is how you set aside one the model \
         wrote",
    ),
    ("subagents", "open or close the subagent tree (ctrl-g)"),
    (
        "peek",
        "ID — read one subagent's output without leaving this session",
    ),
    ("resume", "ID — bring a session on disk back and go there"),
    (
        "promote",
        "move the running command to the background (ctrl-o)",
    ),
    (
        "models",
        "which model answers: /models is a menu, /models PROVIDER/MODEL switches \
         (/models glm-coding --key PASTE stores the key)",
    ),
    (
        "resync",
        "throw this head's state away and take a fresh snapshot",
    ),
    ("cells", "MESSAGE — send it with a copy of this screen"),
    (
        "fb",
        "NOTE — mark this moment (fb+ good, fb- bad; #words are tags; alt-g / alt-b without typing)",
    ),
    ("compact", "summarise this session and fork it"),
    (
        "reseat",
        "rebuild the prompt from the tools seated now, keeping the conversation",
    ),
    (
        "reseat summarise",
        "the same, but summarise the conversation instead of carrying it",
    ),
    ("interrupt", "stop the running turn"),
    ("quit", "leave the head"),
    // The three this table was missing, and the test below is why they cannot be missed again: `/dismiss` and
    // `/notes` are one action under two words, and two aliases the dispatcher already answered.
    (
        "dismiss",
        "retire this head's notes — the same as /notes dismiss",
    ),
    ("settings", "every setting — the same as /config"),
    ("stats", "this head's counters — the same as /status"),
];

/// Which of a daemon row's `choices` its `value` names - the reading both the picker's cursor and the card's `← now` use.
/// A row's `value` is a name plus whatever qualifies it:
///
/// ```text
/// value   "writes allowed"                     choice  "writes allowed"
/// value   "allow-all (this box, consented)"     choice  "allow-all"
/// value   "local (qwen-3.8-27b)"               choice  "local"
/// value   "deepseek/deepseek-flash --key …"    choice  "deepseek/deepseek-flash"
/// ```
/// The value itself; failing that, the longest choice the value begins with at a whitespace boundary. Not the first word:
/// a name can contain a space (`writes allowed`), and `Mode::parse` folds `_` and spaces to `-`, so a fixture can agree with
/// itself while the wire differs. A boundary, so `automode-edits` is not read as `automode`. There is no separator list:
/// an unrecognised qualifier shows as *no choice named* and no `← now`, never a mark on the wrong row.
pub(crate) fn named_choice<'a>(value: &str, choices: &'a [String]) -> Option<&'a str> {
    choices
        .iter()
        .filter(|c| {
            value.len() > c.len()
                && value.starts_with(c.as_str())
                && value[c.len()..].starts_with(char::is_whitespace)
        })
        .chain(choices.iter().filter(|c| value == c.as_str()))
        .max_by_key(|c| c.len())
        .map(String::as_str)
}

/// A verb and its argument, where the verb has to be the whole word: `strip_prefix("mode")` matches every command
/// that begins with those letters, so `/models` arrived as `/mode` with the argument `ls`.
pub(crate) fn verb_arg<'a>(cmd: &'a str, verb: &str) -> Option<&'a str> {
    let rest = cmd.strip_prefix(verb)?;
    if rest.is_empty() {
        return Some("");
    }
    // A digit or letter here means a longer word, not an argument.
    rest.starts_with(char::is_whitespace).then(|| rest.trim())
}
