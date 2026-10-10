//! **The todos' state**: the operator's list, the todo card's draft, the repository's queue,
//! and the stops a click lands on.

use super::*;

impl App {
    /// **The operator's rows, out of the union the daemon serves.** The head keeps no second
    /// list: the daemon persists them, every head sees them, and this is the one place that says
    /// which half of that list the operator wrote. Sending is the whole list, so a divergence
    /// between a head's copy and the store is impossible to accumulate.
    pub(crate) fn operator_todos(&self) -> Vec<letibot_sessionlog::event::TodoEntry> {
        self.todos
            .iter()
            .filter(|t| t.by == letibot_sessionlog::event::TodoBy::Operator)
            .cloned()
            .collect()
    }

    /// **Open the new-todo card**, and put the composer where the card expects it.
    ///
    /// **The composer is the field**, so it is emptied and handed to the card: a half-typed prompt
    /// left under a card whose Enter adds an item is the shape that costs somebody a message, which
    /// is the same reason `mode_confirm` takes the keyboard. leticl's `%todo-draft-open`.
    pub(crate) fn open_todo_card(&mut self) {
        if self.session_id.is_empty() {
            self.say("not attached to a session yet");
            return;
        }
        self.set_composer("");
        self.todo_draft = Some(TodoDraft::new());
        self.say(
            "adding a todo item — title, then tab for the description; enter adds it to the \
             session's plan as yours, esc cancels",
        );
        self.redraw = true;
    }

    /// **`/todo …` — the operator's own rows.** The verb's forms, and each one sends the
    /// whole list:
    ///
    /// ```text
    /// /todo finish the parity row        add it, at the end
    /// /todo done 2                       mark the second of MY rows complete
    /// /todo rm 2                         strike it off: it STAYS, marked `[c]`
    /// /todo postpone 2                   set it aside: it stays, and nothing nags about it
    /// /todo resume 2                     put it back in the list
    /// /todo postpone paste the token     the same, naming a row the MODEL wrote
    /// ```
    ///
    /// **A number is numbered over the operator's rows and not the union**, because the model's
    /// rows are not the operator's to edit — that is the same rule `/rename` and `/compact` keep
    /// about acting on the session you are in. The count is the one the pane prints for that half.
    ///
    /// **`postpone`/`resume` also take a row's own WORDS, and that is the reach.** The pane numbers
    /// the operator's rows and only those, so a row the MODEL wrote had no name this verb could use
    /// — and no other door either: `todo_write` refuses `postponed` (a model that could silence its
    /// own plan could abandon it). MEASURED: fourteen turns spent restating one true sentence about
    /// a row that was correct and waiting on a person. The words are the board's one key
    /// (`set_operator_states` resolves the operator's rows by their exact trimmed text), so a
    /// non-numeric argument names any row on the board: the operator's own half is moved in the
    /// list this head owns, and a MODEL's row goes out as a state move beside it — one frame, one
    /// act. A parent's row is refused by name, because a parent owns its own rows' states.
    ///
    /// **Marked complete rather than deleted** by `done`, and **struck off rather than deleted** by
    /// `rm`: the operator's ruling is *"only i should be able to delete todo items. as a rule
    /// everything that ever created stays in history"*, so neither verb removes a row. `done` is a
    /// record of work finished, `rm` is a record of work decided against — the row keeps its words,
    /// its author and its place on the board either way, and `/todo resume N` lifts either. The
    /// model may move a row's STATUS (by quoting its words) and may not remove one, which is the
    /// operator's ruling recorded on `TodoBoard`.
    /// **THE OPERATOR'S OWN ROWS, DRAWN FROM THE MOMENT THEY ARE SENT** — their half of what
    /// `pending_prompts` does for their words.
    ///
    /// The operator: *"when i send todos they appear in the todo pane some time later — it feels
    /// like their appearance depend on the turn state. but to me - im not sure if i lost them or
    /// not."* They did not lose them: the write publishes at once (`set_operator_todos` has a
    /// retry for exactly that), but the COMMAND reaches the worker behind whatever is running
    /// and is injected at the next step boundary (`harness.rs`: *"the step boundary is the only
    /// place any of the three can be injected"*), and until `TodosUpdated` arrives the pane drew
    /// only what the daemon had published — nothing, for the whole of a running turn.
    ///
    /// So the head applies the list it is about to send, the same trust `pending_prompts` places:
    /// show what I sent until the daemon answers. `mine` is the operator's whole half (the
    /// command replaces it), so the optimistic state is the model's rows as they stand with this
    /// half in their place — and `TodosUpdated` replaces the union wholesale when it lands, in
    /// the daemon's own order, which is why nothing here needs to guess at that order for longer
    /// than the boundary.
    pub(crate) fn echo_operator_todos(&mut self, mine: Vec<letibot_sessionlog::event::TodoEntry>) {
        self.todos
            .retain(|t| t.by != letibot_sessionlog::event::TodoBy::Operator);
        self.todos.extend(mine);
        self.redraw = true;
    }

    /// **Is a starter-todo seed due for THIS project?** The three-part gate, in one place
    /// because the attach and the seed itself both ask it — a second spelling is how a project
    /// gets seeded by one reading and skipped by the other.
    ///
    /// The gate is leticl's `%seed-operator-todos` verbatim: the switch is on, the workspace is
    /// known (a project it cannot name is not a project it may put rows into — a head loads
    /// before the socket exists), and the record does not hold this workspace. **The record, not
    /// the list**: a starter row the operator deleted must not come back, which is what an *is
    /// the list empty* test would do on every restart.
    pub(crate) fn todo_seed_due(&self) -> bool {
        self.todo_template != crate::prefs::TodoTemplate::Off
            && !self.wiring.workspace.is_empty()
            && !self
                .todo_seed
                .contains(&todo_seed_key(&self.wiring.workspace))
    }

    /// Where the starter todos come from: `todo_template`'s three shapes as a path — leticl's
    /// `todo-template-path`, which its own docstring rules: *“one function because the setting
    /// has three shapes and two callers must not spell them differently.”* `None` when the
    /// switch is off or this head has no config directory to read a default from.
    pub(crate) fn todo_template_file(&self) -> Option<std::path::PathBuf> {
        match &self.todo_template {
            crate::prefs::TodoTemplate::Off => None,
            crate::prefs::TodoTemplate::Path(p) => Some(std::path::PathBuf::from(p)),
            crate::prefs::TodoTemplate::Default => self
                .prefs_path
                .as_ref()
                .and_then(|p| p.parent())
                .map(|d| d.join("todo-template.md")),
        }
    }

    /// **Copy the template TODO.md's items onto the operator's half of the board** — leticl's
    /// `%seed-operator-todos`, run where leticl runs it: the moment the head has learned its
    /// list. The behaviour copied, piece by piece:
    ///
    /// * **The template is a `TODO.md`, parsed by the same reader the repo section uses**
    ///   (`render_todo_md`) — a starter list is written in the format the operator already
    ///   writes by hand, boxes and indented bodies included, and there is no second syntax
    ///   to learn.
    /// * **ITEMS ONLY are copied**: the checkbox rows, not the headings and their roll-ups.
    ///   What is copied is what leticl copies — the text, the body, and the mark: `[x]`
    ///   seeds a completed row (how a template carries something already settled), anything
    ///   else seeds an open one.
    ///
    ///   The one place this cannot be leticl: **the body rides in `content`, joined `“ · ”`**.
    ///   leticl keeps a `:detail` beside its rows in its own sqlite; this head keeps no second
    ///   list, and the wire's `TodoEntry` is `content`/`status`/`by` — leticl's own push drops
    ///   the detail at the same door. The card already made this head's choice for a typed
    ///   detail (`title “ — ” detail`); body LINES join with `·` so each continuation stays a
    ///   segment rather than merging into one sentence.
    ///
    /// * **ONCE PER PROJECT, marked even when nothing was added** — an empty template records
    ///   the seeding and says so, because the alternative re-reads it on every start. A
    ///   MISSING file is not marked: leticl says why and leaves the project unseeded, so the
    ///   file the operator was going to write still gets its chance.
    /// * **Refusals are notes, not silences** — from the operator's side *the feature did not
    ///   work* and *I never turned it on* look identical otherwise.
    ///
    /// The rows go out through the same door `/todo TEXT` uses — `echo_operator_todos` for the
    /// optimistic view, `SetOperatorTodos` for the whole half — so a seed and a typed row cannot
    /// become different acts.
    pub(crate) fn seed_todos(&mut self) {
        if !self.todo_seed_due() {
            return;
        }
        let Some(path) = self.todo_template_file() else {
            self.say("todo_template is on but this head has no config directory to read it from");
            return;
        };
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) => {
                // Not marked — see the docstring: the file may still be written.
                self.say(&format!(
                    "todo_template is on but {} is not there: {e}",
                    path.display()
                ));
                return;
            }
        };
        let rows = render_todo_md(&body);
        let items: Vec<&TodoRow> = rows
            .iter()
            .filter(|r| r.item && !r.text.trim().is_empty())
            .collect();
        if items.is_empty() {
            self.mark_seeded();
            self.say(&format!(
                "{} has no items in it, so nothing was added",
                path.display()
            ));
            return;
        }
        let mut mine = self.operator_todos();
        // **A template item over the title cap is NOT seeded, and it is said** — the seed's own rule
        // (*"refusals are notes, not silences"*), because from the operator's side a row that
        // silently did not arrive is indistinguishable from a file that was never read. The title is
        // what is measured (`title_refusal`), and the body joined after ` — ` below is the DETAIL.
        let mut skipped: Vec<(String, String)> = Vec::new();
        for r in &items {
            let mut content = r.text.trim().to_string();
            if !r.body.is_empty() {
                content.push_str(" — ");
                content.push_str(&r.body.join(" · "));
            }
            if let Some(why) = title_refusal(&content) {
                skipped.push((content, why));
                continue;
            }
            mine.push(letibot_sessionlog::event::TodoEntry {
                content,
                status: if r.mark == Some(TodoMark::Done) {
                    letibot_sessionlog::event::TodoStatus::Completed
                } else {
                    letibot_sessionlog::event::TodoStatus::Pending
                },
                by: letibot_sessionlog::event::TodoBy::Operator,
                when: None,
                // **A template's items are rows, not a graph**: the file is a checklist and has
                // nowhere to say what waits on what, so a seeded row waits on nothing.
                needs: Vec::new(),
            });
        }
        let n = items.len() - skipped.len();
        self.mark_seeded();
        self.echo_operator_todos(mine.clone());
        self.queued.push(Action::SetOperatorTodos {
            items: mine,
            moved: Vec::new(),
        });
        // **The count, and then every item that was too long to be a title, by name** — one notice,
        // because `say` replaces what it last said: two calls would leave only the refusals on the
        // screen and the count unread. The seed's own rule is *"refusals are notes, not silences"*.
        let mut said = format!(
            "{n} starter todo{} from {}",
            if n == 1 { "" } else { "s" },
            path.display()
        );
        if !skipped.is_empty() {
            let listed: Vec<String> = skipped
                .iter()
                .map(|(content, why)| format!("  {why}\n    `{content}`"))
                .collect();
            said.push_str(&format!(
                "\n{} item{} of the template not seeded:\n{}",
                skipped.len(),
                if skipped.len() == 1 { "" } else { "s" },
                listed.join("\n")
            ));
        }
        self.say(&said);
    }

    /// **Record that this project has had its starter todos** — and do it BEFORE the send, for
    /// leticl's own reason: a record that waits for an acknowledgement re-fires on the next
    /// start if the write failed quietly, and the operator gets the duplicates this feature
    /// exists to avoid. The write is a UNION with whatever the file holds
    /// (`merge_todo_seed`) because two heads share one `head.toml` and a dropped record is a
    /// project that re-seeds.
    pub(crate) fn mark_seeded(&mut self) {
        let key = todo_seed_key(&self.wiring.workspace);
        if !self.todo_seed.contains(&key) {
            self.todo_seed.push(key);
        }
        if let Some(path) = self.prefs_path.clone() {
            let mut p = self.prefs();
            p.todo_seed = crate::prefs::merge_todo_seed(&path, &self.todo_seed);
            self.todo_seed = p.todo_seed.clone();
            if let Err(e) = crate::prefs::save(&path, &p) {
                self.say(&format!("seed not recorded: {e}"));
            }
        }
    }

    /// **`/todos` and `ctrl-p`, as one action.**
    ///
    /// A pane with a chord and no word is unreachable from a pipe and unteachable by
    /// `/help`, and two spellings of one action must not be two implementations.
    ///
    /// Returns the bootstrap read when the pane is opening: the daemon's todo list
    /// rides no snapshot, so a head that attached after the model last wrote has to ask.
    /// Later changes arrive as `TodosUpdated` and need no asking.
    pub(crate) fn toggle_todos(&mut self) -> Option<Action> {
        self.todos_pane = !self.todos_pane;
        // A pane opens at its top. Kept per-pane would be four fields that each go
        // stale; one field reset on every open is the same behaviour with nothing to
        // forget.
        self.pane_scroll = 0;
        // **And a row left open goes with the pane.** The overlay's own Esc already returns to
        // the list, so this is the door that cannot be reached while it is up — but a pane
        // closed by any other means must not leave a row standing over the screen it belonged
        // to, which is `/queue`'s rule for its own overlay.
        self.todos_open = None;
        self.redraw = true;
        if self.todos_pane {
            // Read at open, and re-read on every draw the file has moved under — see
            // `refresh_repo_todos`. The file is the operator's to edit, and a pane
            // showing an old read of it is a pane that lies quietly.
            self.refresh_repo_todos();
        }
        self.todos_pane.then_some(Action::ListTodos)
    }

    /// **EVERY ROW OF THE TODOS PANE THE CURSOR MAY LAND ON, in the order the pane draws them** —
    /// leticl's `todos-stops`, and the ONE enumeration everything about the cursor reads.
    ///
    /// Its docstring is the operator's two reports, and both were the same defect: R44's first cut
    /// spread this over a `-1` sentinel and the repo's own stop indices, and then *"arrows dont go
    /// here"* — the cursor moving to a row whose line the pane computed from another list's
    /// arithmetic — and *"mouse doesnt click"* — a click on the add row computing a negative index,
    /// thrown away. **Two enumerations was the defect.**
    ///
    /// Four kinds, and the tag carries IDENTITY rather than position: a position is a fact about
    /// the list when it was DRAWN, and a list changes between a draw and a keypress (a `TodosUpdated`
    /// arriving, a row removed), so every action would land on whatever took its neighbour's place.
    ///
    /// **The model's rows ARE stops, and Enter on one READS it.**
    ///
    /// They were not, and the reason was sound while it held: no key acted on one — the model
    /// moves its own row's status and the operator may not — and a cursor that stops where no key
    /// acts is a cursor the operator presses keys into and nothing happens. **Enter now acts on
    /// every row: it opens the row's whole text** ([`App::todos_open`]), which is the operator's
    /// own report against this pane — *"in todo list i cant expand your todo items - only
    /// truncated titles available"*, about the model's rows, which are the long ones. So the rule
    /// is kept rather than bent: a stop is where a key acts, and reading is an act. **Reading is
    /// not moving** — nothing here changes a row's state, which is the R44 boundary that still
    /// holds.
    ///
    /// The repo's headings are still skipped, and that is not the same rule broken: the rule is
    /// *a stop must have a key*, not *every key must have a row*. A heading is a section label
    /// rather than a row with words of its own, and a reader who wants the section has the items
    /// under it.
    pub(crate) fn todos_stops(&self) -> Vec<TodoStop> {
        let mut out = vec![TodoStop::Add];
        // **In the order the pane DRAWS them**, which is the daemon's own order over the whole
        // union — one walk, so the cursor's sequence and the rows on the screen cannot come from
        // two readings of the list.
        for t in &self.todos {
            out.push(if t.by == letibot_sessionlog::event::TodoBy::Operator {
                TodoStop::Mine(t.content.clone())
            } else {
                TodoStop::Theirs(t.content.clone())
            });
        }
        if let Some(rows) = &self.repo_todos {
            for (i, r) in rows.iter().enumerate() {
                if r.item {
                    out.push(TodoStop::Repo(i));
                }
            }
        }
        out
    }

    /// **The todos pane's own half of the bottom bar** — the keys THIS pane answers, and the act
    /// depends on the row the cursor is on.
    ///
    /// **The footer used to be rano's.** `HintMode::Todos` is a pinned sentence — *"↑↓ moves · enter
    /// or tab unfolds · pgup/pgdn and the wheel scroll · esc closes"* — and `enter or tab unfolds`
    /// is false on two of the three stops this cursor can be on: `key_todos_pane` opens the card on
    /// the add control and marks one of the operator's rows done (and unmarks it again), and only a
    /// repo item unfolds. **A hint that names the wrong key is worse than none** — the rule rano's
    /// own `QuitCard` sentence is written under — so the head composes this pane's bar and says what
    /// its keys do, taking `Reading`'s tail (`esc closes this`, which is true of it) for the way out.
    ///
    /// **The scroll keys are not named, and that is the bar's capacity rather than a claim about
    /// them.** The renderer keeps the head and cuts the tail, so what has to survive the cut is the
    /// act the cursor is about to take. `/todo rm N` rides along because deleting a row is the
    /// operator's own act, this is the only place it is named, and a verb nobody can find is a verb
    /// nobody uses — see `TodoStop`'s doc for why no key removes one.
    ///
    /// **One precondition is NOT said, and it is the honest gap**: `key_todos_pane` acts on Enter
    /// and Tab only while the composer is EMPTY (a half-typed `/command` owns Tab), so with text in
    /// the composer this bar promises a read Enter will not perform. The sentence it replaces was
    /// false there too — it said `unfolds` — so this is narrower than what was fixed, not wider.
    ///
    /// **Two keys, and the bar says both, because they are now two acts**: Enter reads the row
    /// whole, Tab performs the pane's own act on it. Before this, one sentence covered both
    /// because both keys did the same thing; a bar that went on saying `enter or tab toggles it
    /// done` would name a key that now opens an overlay instead.
    pub(crate) fn todos_hint(&self) -> String {
        let stops = self.todos_stops();
        let at = self.todos_sel.min(stops.len().saturating_sub(1));
        let act = match stops.get(at) {
            Some(TodoStop::Add) => "enter or tab opens the card",
            Some(TodoStop::Mine(_)) => "enter reads it whole · tab toggles it done",
            Some(TodoStop::Theirs(_)) => "enter reads it whole",
            Some(TodoStop::Repo(_)) => "enter reads it whole · tab unfolds the item",
            None => "enter reads the row whole",
        };
        format!("↑↓ moves · {act} · /todo rm N removes one of yours")
    }

    /// **The open row's own half of the bottom bar.**
    ///
    /// The overlay is modal — it takes the whole keyboard while it is up
    /// ([`App::key_todo_open`]) — so the bar has to name the way out, and it names the keys that
    /// move the window too: a row longer than the screen is the case the overlay exists for, and a
    /// reader who does not know the page keys exist would think the rest was not there. The tail
    /// sentence (`esc closes this`) is `Reading`'s, which is what the overlay's mode takes; the
    /// sentence here is the head's half and says what the key actually does — *back to the list*,
    /// because the pane is still behind it.
    pub(crate) fn todos_open_hint(&self) -> String {
        "↑↓ pgup/pgdn and the wheel scroll · esc goes back to the list".to_string()
    }

    /// **The pane row the stop at the cursor was DRAWN on**, read out of
    /// [`App::todos_stop_rows`] — the record the pane wrote while drawing, and not arithmetic over
    /// the lists it drew from. leticl's `todos-lines` second value, an `aref` of the third.
    ///
    /// The clamp is the one thing here that is not a read: a list can change under the cursor — a
    /// `TodosUpdated` arriving, a row removed, another workspace — and a key pressed against a
    /// shorter list must land on a row rather than on an index that no longer exists.
    pub(crate) fn todos_row_of(&self) -> usize {
        let at = self
            .todos_sel
            .min(self.todos_stop_rows.len().saturating_sub(1));
        self.todos_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// **The stop drawn on SCREEN ROW `y`, or nothing** — leticl's `todo-stop-at-line`, and the
    /// answer to the operator's other report on this pane, *"mouse doesnt click"*.
    ///
    /// Read from the rows the last draw recorded, so a click and the drawing cannot disagree about
    /// where a row is. That disagreement is the whole of the old defect: letibot's pane took no
    /// click at all, and leticl's took one that computed the add row as a negative index and threw
    /// it away — the add row being the first selectable row and `line - header` making it negative.
    ///
    /// Guarded on the WINDOW: a row above the pane's top or below its last drawn row is not a row
    /// anybody is looking at, and a click into the blank space under a short list moves nothing.
    pub(crate) fn todo_stop_at_row(&self, y: u16) -> Option<usize> {
        let y = usize::from(y).checked_sub(self.todos_pane_top)?;
        if y >= self.pane_room {
            return None;
        }
        let pane_row = y + self.pane_scroll;
        self.todos_stop_rows.iter().position(|r| *r == pane_row)
    }

    /// **The repo cursor follows the stop cursor**, for the repo's own keys — the unfold and the
    /// body it shows read `repo_sel`, and a second cursor that did not follow would be the two
    /// enumerations this whole change removed.
    pub(crate) fn sync_repo_from_stop(&mut self, stops: &[TodoStop]) {
        let at = self.todos_sel.min(stops.len().saturating_sub(1));
        if let Some(TodoStop::Repo(i)) = stops.get(at) {
            self.repo_sel = *i;
        }
    }

    /// Re-read `TODO.md` when it has changed since the last read, and not
    /// otherwise. Called on open and before every draw of the pane.
    ///
    /// A failed `stat` — the file was deleted, or was never there — reads again,
    /// so the pane's own "no TODO.md" line is the answer and stays current if one
    /// appears. That costs a failed `open` per draw in the case where there is
    /// nothing to show, which is the case nobody is watching.
    pub(crate) fn refresh_repo_todos(&mut self) {
        // **Resolved, like the read it guards** — the mtime watch and the parse must look at the
        // same file, or a nested layout re-reads on every draw (the watch misses, so `now` is
        // None, so the cache never holds).
        let path = crate::gitfield::project_dir(&self.wiring.workspace).join("TODO.md");
        let now = std::fs::metadata(&path)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        if self.repo_todos.is_some() && now.is_some() && now == self.repo_todos_at {
            return;
        }
        self.repo_todos_at = now;
        self.repo_todos = Some(repo_todos_map(&self.wiring.workspace));
    }
}

/// **The title cap, at the OPERATOR's own door** — the refusal `/todo TEXT`, the new-todo card and
/// the template seed all ask, so one rule is spelled once for the person the same way
/// `todo_write` spells it for the model.
///
/// The rule and the number are `letibot_tokencore::store::TODO_TITLE_MAX`'s, and the head reads them
/// from the WIRE's copy of them (`letibot_sessionlog::event`) because that is where every other
/// todo fact the head holds comes from — `TodoEntry`, `TodoStatus`, `TodoBy` — and because the head
/// does not link the store: the crate is a dev-dependency here (the apparatus machinery), and the
/// TUI binary has no business pulling rusqlite in for a constant. The wire's copy is pinned to the
/// store's by a test in that crate, which does see both. The arithmetic: the operator's terminal is
/// 160 cells, the todos pane draws a row at 156, and a row's own furniture — the mark, its `/todo`
/// number, `— model` — takes 21, so a longer title is CUT at the pane's right edge and read by
/// nobody. What is capped is the TITLE, so the part after ` — ` is the row's detail and is not
/// capped — which is the shape the card's own Detail field already writes.
///
/// **It is asked where a row is CREATED and not where the half is sent.** `SetOperatorTodos`
/// replaces the operator's half wholesale, so the rows that come back with it include every row
/// written before this cap — a check at the door would refuse the operator's own `/todo done 2`
/// because an old row is long, and would make a row older than the cap impossible to close. The
/// head is the writer of the operator's words, and this is the moment they are written.
pub(crate) fn title_refusal(text: &str) -> Option<String> {
    use letibot_sessionlog::event::{TODO_TITLE_MAX, todo_title};
    let title = todo_title(text);
    let n = title.chars().count();
    (n > TODO_TITLE_MAX).then(|| {
        format!(
            "that title is {n} characters, and a title is at most {TODO_TITLE_MAX} — the width the \
             todos pane draws a row at. The rest is the DETAIL: put it after ` — ` and the pane \
             shows it after the title"
        )
    })
}

/// **One row of the todos pane the cursor may land on** — see [`App::todos_stops`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TodoStop {
    /// The `[+] add todo item` control, at the head of the list.
    Add,
    /// **One of the operator's own rows, BY ITS WORDS.** There is no id on the wire —
    /// `TodoEntry` is `content`, `status`, `by`, and the operator has ruled out a bump for one — so
    /// the words are the identity, which is the same key `/todo done N` uses. A row renamed is a
    /// different row, and that is the honest reading of a list with no ids.
    Mine(String),
    /// **One of the MODEL's rows — `Mine`'s other half, by its words**, and the rows the operator's
    /// report was about: *"in todo list i cant expand your todo items"*. They are stops now because
    /// Enter READS a row ([`App::todos_open`]) and a stop is where a key acts; nothing here moves
    /// one, which is the boundary that used to be the reason they were skipped (see
    /// [`App::todos_stops`]).
    ///
    /// It carries the words rather than the author, and that is the board's own rule: a row's
    /// identity is its exact text, and the lookup that draws the overlay takes the first row with
    /// those words that is not the operator's — which is the same row, because the stops are built
    /// in the list's own order. A `Parent <id>` or a `queue` row is one of these too: none of the
    /// three is the operator's to move.
    Theirs(String),
    /// A row of the workspace's `TODO.md`, by index into `repo_todos` — the file's own order IS
    /// its identity, because the pane re-reads the file.
    Repo(usize),
}

/// **A todo being filed from the card** — its fields, and which one owns the composer.
///
/// The FOCUS was a `bool` while there were two fields, and a third is exactly what a bool cannot
/// hold: *typing the detail?* answers nothing about a `when` field, so every reader of that flag
/// would have grown a second one and the two could disagree. One enum, and Tab cycles it.
///
/// **The composer holds the focused field and the draft holds the rest**, which is what keeps the
/// row being typed from being a keystroke behind — leticl's `%todo-draft-focus`, and the reason
/// [`TodoDraft::take`] exists: every key that LEAVES a field commits the composer into it first, so
/// nothing typed is ever lost to a Tab.
pub(crate) struct TodoDraft {
    pub(crate) title: String,
    pub(crate) detail: String,
    /// **The handle this row waits on, or empty for a row that waits on nothing.** A bare handle:
    /// the *condition* is what the row holds, and the card does not collect a kind because there is
    /// one kind — see `TodoCondition`.
    pub(crate) when: String,
    pub(crate) focus: TodoField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoField {
    Title,
    Detail,
    When,
}

impl TodoDraft {
    pub(crate) fn new() -> TodoDraft {
        TodoDraft {
            title: String::new(),
            detail: String::new(),
            when: String::new(),
            focus: TodoField::Title,
        }
    }

    /// **What one field holds**, with the composer's live text standing in for the focused one.
    pub(crate) fn shown(&self, live: &str, which: TodoField) -> String {
        if self.focus == which {
            return live.to_string();
        }
        match which {
            TodoField::Title => self.title.clone(),
            TodoField::Detail => self.detail.clone(),
            TodoField::When => self.when.clone(),
        }
    }

    /// **Commit the composer into the field it belongs to** — every key that leaves a field does
    /// this first, so a Tab cannot lose what was just typed.
    pub(crate) fn take(&mut self, live: &str) {
        let into = match self.focus {
            TodoField::Title => &mut self.title,
            TodoField::Detail => &mut self.detail,
            TodoField::When => &mut self.when,
        };
        *into = live.to_string();
    }

    /// The field Tab goes to next, wrapping — a cycle, so there is no field a reader cannot reach.
    pub(crate) fn next(&self) -> TodoField {
        match self.focus {
            TodoField::Title => TodoField::Detail,
            TodoField::Detail => TodoField::When,
            TodoField::When => TodoField::Title,
        }
    }
}

/// **A workspace's key in the seeded-projects record** — its FNV-1a hash, in `note_key`'s
/// shape: the record lives in `head.toml` as a comma list, so a key that contains a comma or
/// whitespace (as a path can) would corrupt the list, and hashing is the same answer
/// `note_key` gives for the same reason.
pub(crate) fn todo_seed_key(ws: &str) -> String {
    format!("{:016x}", fnv1a(ws))
}
