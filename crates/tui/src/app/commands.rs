//! **The slash commands**: `/verbosity`, `/notes`, `/todo`, a door tool run by the head, and
//! the dispatcher that reads the rest.

use super::*;

impl App {
    /// A fold changes how many lines every cached block renders to, so the history
    /// buffer and every block cache are stale at once.
    pub(crate) fn refold(&mut self) {
        self.invalidate_history();
        self.scroll = 0;
        self.redraw = true;
        self.say(&format!(
            "thinking {} · tool output {} · raw tool calls {}",
            fold_word(self.reasoning),
            fold_word(self.tools),
            if self.raw_calls { "shown" } else { "hidden" }
        ));
    }

    /// **The one place a mode leaves the head**, so the `allow-all` confirmation
    /// cannot be reached by one route and skipped by another. The picker, `/mode
    /// NAME` and the config pane's cycle all end here.
    pub(crate) fn mode_action(&mut self, name: String) -> Option<Action> {
        // The literal, not `Mode::ALLOW_ALL.name`: the head does not link
        // `letibot-tools` and does not keep a mode list — every other name it
        // handles comes from the daemon's `SettingRow::choices`. This is the one
        // name it has to recognise, and it is the daemon's own spelling.
        if name == "allow-all" {
            self.mode_confirm = Some(name);
            self.redraw = true;
            return None;
        }
        Some(Action::Mode {
            name,
            consented: false,
        })
    }

    /// **Choose the profile `typed` names — one of R38's settings, and a local one.** Returns
    /// `None` because nothing is sent anywhere.
    ///
    /// **The one place a set is set**, called by the card's `Enter`, by `/verbosity NAME` and
    /// by `/v`, so the three cannot disagree about what a word means or about what happens to
    /// the transcript when it changes. The word itself is read by [`Visibility::parse`] — the
    /// same function `head.toml` is read with — so a name this reads here is a name the file
    /// reads there.
    pub(crate) fn set_verbosity(&mut self, typed: &str) -> Option<Action> {
        let t = typed.trim().to_ascii_lowercase();
        let next = if t == "v" || t == "next" {
            // **`/v` is the next profile**, in `Profile::ALL`'s order — which is the ladder's
            // order with `read-edits` in it — and one key's worth of cycling is a promise this
            // head made (R38 does not revoke it: what R38 rules out is having to CYCLE to
            // learn what the values are, and the card is where they are read).
            let at = self
                .visibility
                .profile()
                .and_then(|p| Profile::ALL.iter().position(|q| *q == p))
                .unwrap_or(0);
            Visibility::of(Profile::ALL[(at + 1) % Profile::ALL.len()])
        } else {
            match Visibility::parse(typed) {
                Ok(Change::Set(v)) => v,
                Ok(Change::Switch(s, l)) => self.visibility.with(s, l),
                Err(said) => {
                    self.say(&said);
                    return None;
                }
            }
        };
        // **What cannot be drawn is not stored** — see [`Visibility::undrawable`]. The ladder
        // turns its three switches on in one order, so a set that hides one below a switch it
        // shows would put a word on the status row that the screen does not carry, and the
        // operator's own rule for the whole rewrite is that *"a profile a person can select
        // that changes nothing is worse than an unfinished rewrite, because it lies about the
        // screen."* The refusal names the switch that cannot be honoured and the way round it.
        if let Some(s) = next.undrawable() {
            // **And the sentence names the way round that actually works — with the REMEDY
            // FIRST, because a notice is trimmed to the frame.**
            //
            // Two faults in one line, and the second was found by the test below rather than by
            // reading it. It read *"`{s}` cannot be off while something above it is on"*, which
            // is backwards in BOTH cases this can fire: the ladder turns `tools`, `thinking` and
            // `system` on in that order, so what cannot be drawn is a switch **on** with one below
            // it **off** — `thinking=open` while `tools=hidden` (which is `read-edits` and then the
            // thinking's chord), or `system=open` while the thinking is hidden. The old wording
            // named the switch the reader had just turned ON as the one to turn off, so the only
            // way to follow it was to make the set worse.
            //
            // And when it was fixed the other way round, the test failed: `say` draws the sentence
            // through `trim_to(…, w)`, so at 100 columns a refusal that opened by explaining the
            // ladder was cut off **before the verb that undoes it** — an R29 remedy the reader
            // cannot see is the same as no remedy. So the one word that always works comes first,
            // then which switch is drawn while which is off, then the profiles.
            //
            // **And the switches it names are only the ones BELOW the offending one**, which is a
            // third fault the same test found: the first cut collected every switch that was off,
            // so `read-edits`' thinking refusal read *"`tools` and `system` is off"* — the verb
            // agreeing with nothing, and `system` named as a requirement when it sits ABOVE
            // `thinking` and is nothing of the kind. The ladder is a prefix, so what a switch that
            // is ON needs is the ones below it: the statement is now true, and with one name it
            // reads as a sentence.
            const LADDER: [Show; 3] = [Show::Tools, Show::Thinking, Show::System];
            let below = LADDER.iter().position(|l| *l == s).unwrap_or(0);
            let missing: Vec<String> = LADDER[..below]
                .iter()
                .filter(|l| !next.shows(**l))
                .map(|l| format!("`{}`", l.name()))
                .collect();
            // `undrawable` only fires when one of those IS off, so the list is never empty — but
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
        // **And the body the set names moves with it**, through the one writer: `folded` and
        // `open` are the fold this head has always had (`ctrl-r`, `/t`), so a profile that
        // says `tools: folded` sets the fold and not only the word. `hidden` is left alone —
        // a fold on a row nobody draws is not a fact about the screen, and the rows are the
        // rung's business.
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
        // **A set that hides rows can hide the one the reader is holding** (R37's consequence
        // for R36), so the view moves onto its nearest surviving neighbour at the moment of the
        // change, where the fact is known for certain.
        self.reanchor_off_hidden();
        // **And whatever was OPEN is closed, because it was open in the other rendering**
        // (R37 AMENDED). `payload_sel` names one thing — a payload window under every other
        // rendering, the run `ctrl-t` opened under this one — and the same id means a different
        // thing on either side of the change. Carrying it across opened a payload window on
        // a row the reader had not asked about, which is the surprise this field exists to
        // avoid: *I opened a run, changed my mind about the rung, and a window appeared.*
        self.payload_sel = None;
        self.payload_page = 0;
        self.invalidate_history();
        // **And it is written down**, which is what *"persists headrestarts"* asks for: the
        // change and the file are one act from here, so a set cannot be chosen and then
        // forgotten. `RetiredWrite::Union` because this is not a statement about the retired
        // set — the fold and diff toggles pass it for the same reason.
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

    /// **The one writer of the two folds** — so a switch moved by a chord, by `/t`, by the
    /// config pane or by a profile all land in the same field, and none of them can move the
    /// other's.
    pub(crate) fn set_fold(&mut self, show: Show, fold: Fold) {
        match show {
            Show::Tools => {
                self.tools = fold;
                // **`/t` moves one key for the long rows**, and an echo's headline is one of
                // them (R33), so the two travel together here as they did at every other
                // writing of this field.
                self.echo_open = fold.is_open();
            }
            Show::Thinking => self.reasoning = fold,
            Show::Edits | Show::System | Show::RawCalls => {}
        }
    }

    /// **Set the diff style by name, or refuse by name** — R38's second setting.
    ///
    /// A local setting like the rung above, and one place for the same reason: the card's
    /// `Enter` and the typed word must not be able to disagree.
    pub(crate) fn set_diff(&mut self, typed: &str) -> Option<Action> {
        // **`DiffPref::parse` and not a second list.** The preference file is SHARED with
        // the other head, which accepts `split` / `side-by-side` / `auto` and `unified` /
        // `single` on input and writes back exactly two words — so the spellings a value can
        // arrive in are already a fact of this tree, and a verb with its own list would be a
        // verb that refused a value the file accepts. One parser.
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
        // **The history holds RENDERED rows**, and a diff style decides what one of them
        // renders to — so the whole buffer is stale, not just the row that changed. The same
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

    /// **A door verb the operator typed, turned into the call the wire wants** — R31, R34.
    ///
    /// Returns the tool's OWN name and the arguments object, or a sentence saying why not.
    /// The head's whole knowledge is the three facts on the row: **which field a bare line
    /// goes into, what kind it is, and which fields have defaults.** Nothing here knows what
    /// `web_search` does.
    ///
    /// * **No bare form** (`field` empty) — the row's own `why_json` sentence is returned, so
    ///   the refusal names the reason the daemon gave rather than the head's guess at it.
    /// * **A line with nothing after the verb** — refused by name, because a bare call to a
    ///   tool that needs a query is a call nobody meant.
    /// * **Anything that looks like a JSON object** goes through untouched, which is the form
    ///   R31 keeps for a tool with several arguments. A line that *starts* with `{` is the
    ///   operator asking for the JSON form; there is no tool whose bare text begins that way
    ///   by accident and the ambiguity is resolved in favour of the form they can see.
    pub(crate) fn head_run_call(
        &self,
        tool: &letibot_sessionlog::HeadRunTool,
        line: &str,
    ) -> Result<String, String> {
        let line = line.trim();
        if line.starts_with('{') {
            // **Checked for BEING json and for nothing else** — the original rule, kept.
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
        // **The defaults travel with the line**, so the object the daemon runs is the one a
        // model's minimal call would have produced. Without them the same tool answers two
        // different questions depending on who asked — the daemon published them for exactly
        // this and a head that dropped them would be editing the call.
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

    /// The verbs the daemon published, from its settings row. Empty when it sent none.
    /// **The providers this box holds a key for**, from the daemon's own row —
    /// [`letibot_sessionlog::protocol::MODEL_KEYS_KEY`].
    ///
    /// Empty when the row is absent, which is a daemon older than this one: **no greening**
    /// rather than every row greened, the same rule `daemon_verbs` follows. The head keeps no list
    /// of its own because it *cannot* have one — whether a preset resolves a key is a fact about
    /// this box's files and environment (`keys::resolve`), and the daemon is the half that reads
    /// them.
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

    /// **The rows that need no credential**, from the daemon's own
    /// [`letibot_sessionlog::protocol::MODEL_KEYLESS_KEY`] row: `local` and every local
    /// model declared in `providers.toml`.
    ///
    /// Falls back to `local` alone when the row is absent, which is a daemon older than
    /// the row. That is exactly what this head did before the row existed, so an old
    /// daemon greens what it always greened and nothing reads as newly broken.
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

    /// **Whether a picker row is one this box can actually take.**
    ///
    /// The operator, 2026-10-04: *"model peeker should green models we have keys for. — if i
    /// choose a model without key picker should ask for the key."* So the colour answers *will this
    /// work if I press enter*.
    ///
    /// Two ways to be ready, and they are different facts: a credential this box holds
    /// ([`Self::keyed_providers`]), or **no credential wanted at all**
    /// ([`Self::keyless_choices`]). `local` was once hardcoded here for the second
    /// reason — *"leaving it uncoloured would read as this row has no key about the one
    /// row that never wanted one"* — and the operator found what a hardcoded literal
    /// costs the moment there is a second such row: *"dense78 needs a key this box does
    /// not hold"*, about a LAN box with no key and no meter. The daemon publishes the
    /// list now, for the same reason it publishes the keyed one: it is the half that
    /// knows.
    pub(crate) fn choice_ready(&self, name: &str) -> bool {
        let name = name.trim();
        // **Three ways a row can name its model, so three candidates.**
        //
        // `deepseek/deepseek-flash` -- the preset is the part before the slash.
        // `dense78` -- a declared name, whole, and it may contain anything the operator
        // typed. `dense78 (qwen-3.8-27b at http://192.168.1.78:8082)` -- the row for a
        // session already ON one, where the first WORD is the name; the `model` row's
        // own comment is the rule (*"the first word is what a picker matches on"*), and
        // splitting that on `/` lands inside the url instead.
        let first_word = name.split_whitespace().next().unwrap_or(name);
        let provider = first_word.split('/').next().unwrap_or(first_word);
        let keyless = self.keyless_choices();
        keyless
            .iter()
            .any(|k| k == name || k == first_word || k == provider)
            || self.keyed_providers().iter().any(|k| k == provider)
    }

    /// **Did the daemon publish its key row at all?** An absent `models.keys` is a daemon
    /// older than the field and reads as *no greening* — never as *no keys* — so the ask is
    /// gated on the row being present rather than on the key list being empty, and an older
    /// daemon never finds its switches blocked behind a prompt.
    pub(crate) fn keys_row_present(&self) -> bool {
        self.settings
            .iter()
            .any(|r| r.key == letibot_sessionlog::protocol::MODEL_KEYS_KEY)
    }

    pub(crate) fn command(&mut self, cmd: &str) -> Option<Action> {
        // **The operator's own tool call** — R24 part two's door, R31's bare form, R34's
        // hyphen.
        //
        // First, because a door verb is neither this head's nor the daemon's other half's:
        // it is a TOOL, and it has to be matched against the list the daemon published
        // before anything else gets to refuse it as an unknown word.
        //
        // **Both spellings are accepted and only one is offered** (R34): the operator who
        // types `/web_search` because that is what the daemon calls it *"should not be told
        // they are wrong"*, so the transform runs at the lookup and the tool's own name goes
        // on the wire. The head holds no schema — it knows which field takes the line and
        // what kind it is, both published on the row.
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

        // **`/cells MESSAGE` — the message, and what is on this screen with it.**
        //
        // `harness what=screen` lets the model ASK; this is the operator pointing.
        // Same rows, same bytes, and captured here rather than a moment later on
        // purpose: the screen being talked about is the one that was there when
        // Enter was pressed, and a turn takes seconds during which it moves.
        //
        // Rendered from this head, at this head's size, escape codes intact — the
        // whole point is what is actually painted, not a description of it.
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
            // Delimited rather than introduced by a sentence, so both readers can
            // find the edges: the model knows where the screen stops and the
            // operator's words end, and the head knows which part of its own
            // transcript is a picture of itself and folds it away. A sentence would
            // do the first job and not the second.
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
            // **The same string that was sent.** The pending row is cleared by
            // matching the user item the daemon appends, so an abbreviation here
            // never matches and the `queued` line never leaves. The screen is taken
            // out at RENDER time instead, by `queued_lines` and by `user_block`,
            // which is where a decision about what to show belongs.
            self.pending_prompts.push(text.clone());
            return Some(Action::Prompt(text));
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
            self.picker = true;
            self.redraw = true;
            return Some(Action::ListSessions);
        }
        if let Some(id) = cmd.strip_prefix("switch ") {
            return self.pick(id.trim());
        }
        // Renames the session this head is **in**. Not an arbitrary one: the picker
        // is where another session is on screen, and a `/rename` that could reach a
        // row you were only looking at is one typo away from renaming the wrong
        // conversation.
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
                // Bare `/mode` opens the picker rather than printing a list to
                // copy a name out of. The rows come from the daemon's last
                // answer, and asking again — the way `/config` does on open —
                // is what keeps the `← now` marker honest when the mode moved
                // since this head last asked. The cursor is seeded to the mode
                // the session is already under, so Enter on an untouched list
                // is a no-op; the answer arriving does not re-seed it, so an
                // arrow pressed while the ask was in flight is not undone.
                self.pick = Some(Pick::Mode);
                self.picker = false;
                self.config_pane = false;
                self.seed_pick();
                self.redraw = true;
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
            // **`/notes` — the disclosures this head has shown** (R10), and
            // `/dismiss` — the same action under the word a person types at a wall
            // of red. Both land in `notes_command`, so the two spellings cannot
            // disagree about what retired means.
            //
            // Placed before the one-word arms because it takes arguments: `rest`
            // is everything after the verb, and `/notes` with nothing after it
            // lists rather than acting.
            _ if verb_arg(cmd, "notes")
                .or_else(|| verb_arg(cmd, "dismiss"))
                .is_some() =>
            {
                let (verb, rest) = if let Some(rest) = verb_arg(cmd, "notes") {
                    ("notes", rest)
                } else {
                    ("dismiss", verb_arg(cmd, "dismiss").unwrap_or(""))
                };
                return self.notes_command(verb, rest);
            }
            "help" | "h" | "?" => {
                self.help = !self.help;
                self.pane_scroll = 0;
                self.redraw = true;
                None
            }
            // Where the bottom border's telemetry went. See `App::stats`.
            "status" | "stats" => {
                self.stats = !self.stats;
                self.pane_scroll = 0;
                self.redraw = true;
                // **Opening the screen IS the acknowledgement** (R51 item 17). The `⚠` on the
                // composer's edge is a pointer at these numbers, so the act of going to read them
                // is what clears it — there is no separate key, and there should not be: a second
                // verb for *I have read it* is a second thing to learn about a mark whose whole
                // job is to send you here.
                //
                // **Only on the way IN.** Closing it acknowledges nothing, and a reader who
                // opened it by accident and did not look has still not read the numbers — but they
                // also cannot have missed them, because the screen is the thing they were looking
                // at. The asymmetry that matters is the one `Counters::exceeds` holds: a counter
                // that moves AFTER this brings the mark straight back.
                if self.stats {
                    self.acknowledge_counters();
                }
                None
            }
            "think" | "r" => {
                self.reasoning = self.reasoning.flip();
                self.refold();
                None
            }
            // **`/tools` asks what this conversation can call.** It used to be a
            // second spelling of ctrl-t, which already folds tool output and is
            // the key anybody actually uses for it. The operator: *"it toggles
            // tools view but i think i want it to show me currently seated
            // tools"*. The listing is the question worth a word; the fold keeps
            // its key, and `/t` keeps the old behaviour for the fingers that
            // learnt it.
            // **`/t` unfolds every tool row at once and is the fold's only spelling**
            // (R10 moved it off `ctrl-t`, which now opens one result's window — see
            // `Key::CtrlT`). It used to be the legacy spelling of a chord that already
            // did this, which made it a synonym nothing pointed at; now it is the name.
            "t" => {
                self.tools = self.tools.flip();
                // **One key for the long rows, and that includes an echo** (R33). A
                // queued prompt is drawn as one elided headline and this is what opens
                // it; a second fold chord for a second kind of row is a second thing to
                // learn, and the operator asked for *"expandable the usual way"*.
                self.echo_open = !self.echo_open;
                self.refold();
                None
            }
            // **Bare, it opens the card; named, it sets the rung** — R38's rule, and the
            // shape `/mode` already had: a setting with more than two values is CHOSEN from a
            // card showing all of them, and cycling makes the reader hold the list in their
            // head and discover the current value by changing it. With R37's fourth rung that
            // was up to three presses and three repaints.
            _ if verb_arg(cmd, "verbosity").is_some() => {
                let rest = verb_arg(cmd, "verbosity").unwrap_or("").trim().to_string();
                if rest.is_empty() {
                    self.pick = Some(Pick::Verbosity);
                    self.picker = false;
                    self.config_pane = false;
                    self.seed_pick();
                    self.redraw = true;
                    return None;
                }
                return self.set_verbosity(&rest);
            }
            // **`/diff` — R38's new setting.** It was `slash_refused` until today, and that
            // refusal is the card R29 was filed from.
            _ if verb_arg(cmd, "diff").is_some() => {
                let rest = verb_arg(cmd, "diff").unwrap_or("").trim().to_string();
                if rest.is_empty() {
                    self.pick = Some(Pick::Diff);
                    self.picker = false;
                    self.config_pane = false;
                    self.seed_pick();
                    self.redraw = true;
                    return None;
                }
                return self.set_diff(&rest);
            }
            // **`/v` keeps meaning *the next rung*.** It is an alias in `HEAD_COMMAND_ALIASES`
            // — deliberately not offered by tab, deliberately still taken — and one key's worth
            // of cycling is a promise this head made. R38 does not revoke it: what R38 rules out
            // is having to CYCLE to find out what the values are, and the card is where they are
            // read. It goes through the same function as the card and the long spelling, so the
            // three cannot disagree.
            "v" => return self.set_verbosity("v"),
            "config" | "settings" => {
                self.config_pane = !self.config_pane;
                self.config_sel = 0;
                self.pane_scroll = 0;
                // One list on the screen at a time, the same rule the pickers
                // keep between themselves.
                if self.config_pane {
                    self.pick = None;
                }
                self.redraw = true;
                // Opening asks the daemon for its settings; the head's own are
                // already here. A pane drawn from the last answer would show the
                // mode the session had when this head attached.
                if self.config_pane && !self.session_id.is_empty() {
                    return Some(Action::Settings);
                }
                None
            }
            // **`/jobs` is `toggle_jobs`'s other spelling** — the same function the chord
            // and a click on the count label run, so the three cannot drift.
            "jobs" => self.toggle_jobs(),
            // **The merge queue**, and it is not this session's: there is one `main` and one
            // queue, so the pane asks for the whole thing and the daemon answers with it.
            "queue" => {
                self.queue_pane = !self.queue_pane;
                self.queue_open = None;
                self.pane_scroll = 0;
                self.redraw = true;
                self.queue_pane.then_some(Action::ListMergeQueue)
            }
            // **§6: the panes and the promote, reachable as verbs.**
            //
            // Each of these had a chord and no word, which is two problems: a head
            // driven over a pipe — and a person who has not learnt the chord — cannot
            // reach them at all, and `/help` cannot teach a chord it has no name for.
            // The chords stay, because they are faster; both spellings end in the same
            // function, so they cannot drift.
            "todos" => self.toggle_todos(),
            _ if verb_arg(cmd, "todo").is_some() => {
                return self.todo_command(verb_arg(cmd, "todo").unwrap_or(""));
            }
            "subagents" => {
                self.toggle_subagents();
                None
            }
            "promote" => self.promote(),
            // **`/peek ID` reads one subagent's scrollback into the pane the tree's
            // Enter opens.** The id is the daemon's and an id it does not hold is
            // refused by name — the head keeps no list of subagents to validate
            // against, which would be a second copy of the tree it already folds.
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
                self.sub_out_pending = Some(id.clone());
                Some(Action::Peek(id))
            }
            // **`/resume ID` brings a session that is on disk but not in this daemon
            // back to life, and goes there.** The same two steps `switch_to` takes for a
            // row the picker labels *on disk*: the head attaches to whatever the daemon
            // put it on and then asks, because "not held yet" is exactly the state a
            // resume is for, and `Attach` refuses a session the daemon does not hold.
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
                // The session this head is **in**, for the same reason /rename
                // refuses an arbitrary id: a compaction that could reach a row you
                // were only looking at is one typo away from summarising the wrong
                // conversation.
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                // **R16: the fork this head is asking for.** The daemon announces a
                // manual compaction only when it has finished (`compacted`), so the
                // mark has to be taken here, on the way out.
                self.mark_fork();
                Some(Action::Compact)
            }
            // **The tool list is in the prompt, and a prompt is fixed for a
            // conversation.** So this is the only way a session that opened without
            // a shell ever gets one: summarise, and continue under a prompt built
            // from what is seated now. Same refusal as `/compact` for the same
            // reason — it acts on the session this head is in, never one you are
            // only looking at.
            // Both kinds change message zero; the difference is what happens to
            // everything under it. **The lossless one is the default** — the
            // operator: *"id say flip it - reset is loseless and reset summarize
            // will be not"*. Re-seating is about the prompt, and paying for it
            // with the conversation should be the thing you ask for by name.
            "reseat" | "reseat keep" | "reseat verbatim" | "reseat summarise"
            | "reseat summarize" => {
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                let summarise = cmd.ends_with("summarise") || cmd.ends_with("summarize");
                // **R16: the same mark, for the other fork.**
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
                // **Bare `/models` is the menu.** With a name after it the line
                // goes to the daemon as typed, which is what `/models X --once`
                // and `/models X --key K` need. The operator: *"for starters i
                // want it to be usual menu, like /mode"*.
                if matches!(verb, "models" | "model") && other.trim() == verb {
                    if self.session_id.is_empty() {
                        self.say("not attached to a session yet");
                        return None;
                    }
                    self.pick = Some(Pick::Model);
                    self.picker = false;
                    self.config_pane = false;
                    // Seeded to what answers now, so Enter on an untouched list
                    // is a no-op — the same courtesy the mode picker pays.
                    self.seed_pick();
                    self.redraw = true;
                    // **`Settings`, not the `models` verb.** Asking the daemon to
                    // refresh is right — a session whose model moved in another
                    // head would draw a stale `← now` — but `/models` with no
                    // argument answers with the whole provider listing, which
                    // landed on the session log underneath the card. The operator,
                    // looking at the wall of text this picker exists to replace:
                    // *"models is still not a selector"*.
                    //
                    // `Settings` is what `/mode` asks for and it prints nothing:
                    // it refreshes the rows the picker reads.
                    return Some(Action::Settings);
                }
                // **Every other verb goes to the daemon, and the head keeps no list.**
                //
                // There used to be an allowlist right here — twelve names — and it was
                // a second copy of the daemon's verb table, which is the mistake this
                // file has been burned by twice already (see the `mode` settings row,
                // which the head kept its own copy of and got wrong). Its failure mode
                // is the worst kind: the daemon gains a verb, this head is not rebuilt
                // with it, and the operator is told *"unknown command /import — try
                // /help"* about a command the other half implements. The head is then
                // lying about its own daemon, which is the defect class this repo
                // exists against.
                //
                // The comment four lines up already says the right thing — *"the head
                // does not know them and does not need to: the line goes over as typed
                // and the answer comes back on the session log"* — and then the code
                // refused the ones it had not heard of.
                //
                // Nothing is lost by forwarding, because the daemon answers an
                // unrecognised verb **by name**: `harnessd/src/slash.rs` builds
                // `/{verb} is not a daemon verb; /help lists the head's`, so the
                // question is settled by the half that owns the table and a typo gets
                // a better sentence than this head could write.
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                Some(Action::Slash {
                    line: other.trim().to_string(),
                })
            }
        }
    }

    pub(crate) fn todo_command(&mut self, rest: &str) -> Option<Action> {
        let rest = rest.trim();
        if self.session_id.is_empty() {
            self.say("not attached to a session yet");
            return None;
        }
        let mut mine = self.operator_todos();
        // `done N` and `rm N` — a number is what the pane prints beside each of these rows.
        let (verb, arg) = match rest.split_once(char::is_whitespace) {
            Some((v, a)) => (v, a.trim()),
            None => (rest, ""),
        };
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
                    mine.remove(at - 1);
                    self.say(&format!("row {at} is off the board"));
                } else {
                    mine[at - 1].status = letibot_sessionlog::event::TodoStatus::Completed;
                    self.say(&format!("row {at} is done"));
                }
            }
            // **`postpone N` and `resume N` — the state the operator owns, over the same numbers
            // every other verb uses.**
            //
            // The operator's ask: *"can we handle postponed todo item properly? i.e. they persist
            // but without nag and with some counter visible to me"*. The state is THEIRS and this
            // is the door: a model that could set its own row aside would have a way to silence the
            // check that exists to stop it abandoning a plan, so `todo_write` still takes three
            // words and the two that are missing are here.
            //
            // **The verb pair and not a key on the row.** Enter on one of these rows already means
            // *toggle done* — the pane's own act since R44 — and a second row key would be a second
            // thing to learn for an act that has a typed door; these two are listed in
            // `SLASH_COMMANDS` (which is what `/help` and tab read) and named in the pane's own
            // hint line, which is how every other verb here is found.
            //
            // **`resume` and not a second spelling of `done`,** because the two answers are
            // different questions: `done` is *this is finished*, `resume` is *ask me about this
            // again*. Lifting a row puts it back as `pending` — open work, which is what the queue
            // and the idle check read — and **it keeps whatever condition it was carrying**: the
            // handle is not touched by either verb, so a row set aside while waiting on a job goes
            // back to waiting on the same one.
            //
            // A bare `postpone` or `resume` with no number is the text of a new row, exactly as a
            // bare `done` is — see the arm below, which is the one convention for all of them.
            ("postpone" | "resume", n) if !n.is_empty() => {
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
                if verb == "postpone" {
                    mine[at - 1].status = letibot_sessionlog::event::TodoStatus::Postponed;
                    self.say(&format!(
                        "row {at} is set aside — it stays on your list and the model still sees \
                         it, and nothing is reminded of it until you lift it with `/todo resume \
                         {at}`"
                    ));
                } else {
                    mine[at - 1].status = letibot_sessionlog::event::TodoStatus::Pending;
                    self.say(&format!(
                        "row {at} is back in the list — the check may ask about it again"
                    ));
                }
            }
            // **`when N JOB` — the condition, attached by number.** The operator's own shape: *"if
            // you are telling me 'job ends and i do this and that' then 'this and that' is a todo
            // item, which is conditioned by job status (end)"*, and *"when I file a todo"* is where
            // it belongs — the row is filed first and the condition is put on it here.
            //
            // **`when N -` clears it, and that is not a courtesy.** A condition nobody can take
            // off is a row waiting for ever on a job that already ended, and the store would go on
            // reporting it as due.
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
            // Anything else is the text of a new row — including a line that begins with a number,
            // or with `done` and no argument, because those are sentences somebody could type.
            _ if !rest.is_empty() => {
                mine.push(letibot_sessionlog::event::TodoEntry {
                    content: rest.to_string(),
                    status: letibot_sessionlog::event::TodoStatus::Pending,
                    by: letibot_sessionlog::event::TodoBy::Operator,
                    when: None,
                });
                self.say(&format!("added to your list — {} row(s)", mine.len()));
            }
            // **Bare `/todo` opens the card**, which is the shape `/mode` and `/models` keep: a
            // setting or an act with more than one part is CHOSEN from a card rather than typed
            // blind. The three forms are still there and the card's hint line says where.
            _ => {
                self.open_todo_card();
                return None;
            }
        }
        self.echo_operator_todos(mine.clone());
        Some(Action::SetOperatorTodos(mine))
    }

    /// **`/notes` — what this head has shown, and how to retire it.**
    ///
    /// R10. Three verbs in one, because they are one subject: nothing listed the
    /// notes, nothing retired one, and a reader who has just retired the wall needs
    /// a way back if they were wrong. `rest` is the text after the verb.
    pub(crate) fn notes_command(&mut self, verb: &str, rest: &str) -> Option<Action> {
        // **The listing is the moment to find out what the file says.** `load_prefs` ran
        // once, at startup, and a head up for hours has a `dismissed` that only ever grew
        // from its own presses — so without this, `/notes` shows a note retired on disk as
        // live on the screen, which is the operator's 2026-09-22 report read one way round.
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
                self.pane_scroll = 0;
                self.redraw = true;
                None
            }
            "restore" | "back" | "undismiss" => {
                let back = self.dismissed.len();
                self.dismissed.clear();
                self.invalidate_history();
                self.redraw = true;
                // **`Replace`, and this is the whole of the third item.** `restore` clears
                // this head's list and saves; under a union that save wrote the FILE's keys
                // straight back, so the dismissal survived on disk while the head believed it
                // had undone it — two facts about one key, disagreeing, which is the defect
                // `merge_retired` was written to end. A union can only ever ADD a key; an
                // assertion that a key is *not* retired is a removal, and only a replacement
                // can say it.
                //
                // **What this does not fix, said rather than implied:** another head that
                // still holds that key in its own `dismissed` will union it back on its next
                // save, because from *that* reader's seat nothing has changed. A restore is
                // this head's statement about the whole set; it is not a push to anybody
                // else. Making it one would need a second channel, and the operator asked for
                // the shape that keeps the two verbs distinct.
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
                // **Union.** A dismissal asserts a key IS retired, and no other head's save
                // is evidence to the contrary — this is the write `merge_retired` exists for.
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
    ("switch", "ID — go to another session"),
    ("rename", "NAME — name the session you are in"),
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
        "queue",
        "open or close the merge-queue pane: what is landing on main, and why it is not",
    ),
    // **§6's verbs, and §7's C14 ruling that the table is the UNION.** Five of these
    // had a chord and no word, so they were unreachable from a pipe and `/help` had no
    // name for them; `/models` and `/resync` were implemented and simply not listed.
    // C14 calls the table *vocabulary, not implementation* and wants one shared artefact
    // both heads read — that is a cross-tree change and is filed rather than half-done
    // here, but **listing what this head implements is this head's half of it.**
    ("todos", "open or close the todos pane (ctrl-t)"),
    (
        "todo",
        "TEXT adds one of YOUR rows · done N · rm N · postpone N · resume N — the pane numbers \
         your half",
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
    // **The three this table was missing, and the test below is why they cannot be
    // missed again** (R32). `/dismiss` and `/notes` are one action under two words —
    // the second is what somebody types at a wall of red — and `/settings`/`/stats` are
    // the dispatcher's own aliases for `/config`/`/status`. All three worked and none
    // was offered, which is the whole finding: the table is a registry that was read as
    // if it were the dispatcher.
    (
        "dismiss",
        "retire this head's notes — the same as /notes dismiss",
    ),
    ("settings", "every setting — the same as /config"),
    ("stats", "this head's counters — the same as /status"),
];

/// **Which of a daemon row's `choices` its `value` names** — the reading both the picker's cursor
/// and the card's `← now` are made of.
///
/// A row's `value` is not always a choice verbatim. The daemon spells it as a name **plus whatever
/// qualifies it**:
///
/// ```text
/// value   "writes allowed"                     choice  "writes allowed"
/// value   "allow-all (this box, consented)"     choice  "allow-all"
/// value   "local (qwen-3.8-27b)"               choice  "local"
/// value   "deepseek/deepseek-flash --key …"    choice  "deepseek/deepseek-flash"
/// ```
///
/// # The rule, and the two ways of getting it wrong
///
/// **The value itself; failing that, the longest choice the value begins with at a boundary.**
///
/// * **Not the value's first word — that was the defect, and the whole-value comparison is the
///   branch that fixed it.** A NAME CAN ITSELF CONTAIN A SPACE (`Mode::WRITES_ALLOWED` is
///   `writes allowed`), so taking the first word turns it into `writes`, which names no choice,
///   and both readers fall back together to row 0. Measured on the real wire row: `writes allowed`
///   gave no cursor and no `← now` while every other named mode gave both.
///
///   **The daemon's own parser is why nobody noticed.** `Mode::parse` folds `_` and spaces to `-`
///   on *both* sides, so `writes-allowed` and `writes allowed` both select that point — the head's
///   fixture said the hyphenated one and agreed with itself while the wire said the other.
/// * **A boundary, so one name is not read as a prefix of another.** `automode-edits` begins with
///   `automode`, so a bare prefix match would seed on the shorter row. This branch is for the
///   QUALIFIED values the daemon writes — `allow-all (this box, consented)`, `local (qwen-3.8-27b)`
///   — where the qualifier follows a space and the name itself has none, which is why the old
///   first-word rule happened to survive them.
/// A whitespace boundary and not a list of separators: the daemon writes `name (note)` and
/// `name --flag`, and inventing a grammar for the qualifier would be a rule about a spelling this
/// head does not own. If a future row qualifies a name with something that is not whitespace-
/// separated, it shows up here as *no choice named* — which renders as no `← now`, the honest
/// answer, rather than as a mark on the wrong row.
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

/// **A verb and its argument, where the verb has to be the whole word.**
///
/// `cmd.strip_prefix("mode")` matches every command that BEGINS with those four
/// letters, so `/models` arrived as `/mode` with the argument `ls` and the head
/// answered `mode ls requested` — the operator: *"/models doesnt work — printed
/// mode ls requested lol"*. It never reached the daemon at all, because the
/// `mode` arm returns before the fallthrough that forwards unknown verbs.
///
/// `Some("")` for the bare verb, `Some(arg)` when a space follows, and `None`
/// when the word merely starts the same way. Four call sites had the bug and one
/// of them was reported; the other three are `cells`, `new` and `rename`, which
/// would have taken `/newton` as "make a session called ton".
pub(crate) fn verb_arg<'a>(cmd: &'a str, verb: &str) -> Option<&'a str> {
    let rest = cmd.strip_prefix(verb)?;
    if rest.is_empty() {
        return Some("");
    }
    // A digit or letter here means a longer word, not an argument.
    rest.starts_with(char::is_whitespace).then(|| rest.trim())
}
