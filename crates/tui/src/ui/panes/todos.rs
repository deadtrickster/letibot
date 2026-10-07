//! **The todos pane**: the session's plan and the repository's queue (`TODO.md`), and the
//! reader that turns `TODO.md` into its rows.

use crate::app::*;
use crate::render::{RenderConfig, sgr, trim_to};
use letibot_ui::text::without_control_lines;

impl App {
    pub(crate) fn todos_lines(&mut self, w: usize) -> Vec<String> {
        // **The rows the stops land on, taken as they go out.** Built local and assigned at the
        // end because the loops below hold `&self.todos` and `&self.repo_todos` while they record.
        let mut stop_rows: Vec<usize> = Vec::new();
        // **A zero is shown only when it means something.** The `open` number is always drawn —
        // an empty plan is exactly the fact a reader opens this pane to confirm, and `0 open` is
        // the answer. `postponed` is drawn only when there is one, because it is a number about a
        // state most lists never use: a permanent `· 0 postponed` would be a word about a feature
        // rather than about the work, on a header that is read at a glance.
        //
        // **And it is here rather than in the chrome**, which is a decision and not an omission:
        // the top edge carries `N jobs running` because a job is news that arrives while the pane
        // is closed, and this is the operator's OWN act — a row they set aside, in the pane whose
        // rows and whose verbs are the whole of the state. The chrome's own rule is the one
        // `jobs_line` writes down (*"a count that is always there is furniture"*), and a second
        // surface drawing this number would be a second place to keep true, which is the defect
        // this header is arranged to prevent.
        let (open, postponed) = todo_counts(&self.todos);
        let header = match postponed {
            0 => format!("todos — {open} open"),
            n => format!("todos — {open} open · {n} postponed"),
        };
        let mut out = vec![colour(&self.cfg, sgr::BOLD, &header)];
        out.push(String::new());
        // **ONE LIST, WITH THE AUTHOR ON EVERY ROW** — R51 item 18: *"the author tag on every row
        // is the requirement (R44)"*. leticl draws it this way and its `todos-lines` gives the
        // shape, with the add control at the head of the session's list because that is where an
        // addition goes:
        //
        // ```text
        //   this session — the plan, and who wrote each line:
        //   [+] add todo item
        //     [ ] check the logs  — you
        //     [x] a model item  — model
        // ```
        //
        // **Two headed sections was the earlier reading and it is wrong** — not because a section
        // is a bad shape, but because the TAG is the fact and a heading is an inference from which
        // section a row is in. `TodoBoard::snapshot` is one union with a `by` on each row; a screen
        // that re-derives authorship from where a row was drawn is the drift this field exists to
        // prevent, and it is the same defect one step earlier than the one that drew the operator's
        // rows under the model's heading.
        out.push(dim(
            &self.cfg,
            "  this session — the plan, and who wrote each line:",
        ));
        // **The cursor, from the same enumeration the keys read** — see [`App::todos_stops`]. The
        // mark and the key that acts are two readings of one index, which is what leticl's
        // `todos-stops` exists for: two enumerations was the defect that produced *"arrows dont go
        // here"* and *"mouse doesnt click"*.
        let stops = self.todos_stops();
        let cursor = self.todos_sel.min(stops.len().saturating_sub(1));
        let marked = |want: &TodoStop| stops.get(cursor).is_some_and(|it| it == want);
        // **The add control, in the items' own mark column and BOLD**, so it reads as a control
        // rather than as a line of the list. leticl's operator, of its plain first cut: *"it looks
        // like a regular text."* The typed door is `/todo TEXT`; this is the one a reader finds.
        // **The control opens the CARD, and the verb is the typed door to the same act.** Both end
        // in `todo_command`, so a card and a line cannot become different things — leticl's `[+]`
        // row is the one its cursor lands on, and the typed form is what a script uses.
        let on_add = marked(&TodoStop::Add);
        stop_rows.push(out.len());
        out.push(format!(
            "  {} {} {}",
            if on_add { "▸" } else { " " },
            colour(&self.cfg, sgr::BOLD, "[+]"),
            colour(
                &self.cfg,
                sgr::BOLD,
                "add todo item — enter opens the card, or /todo TEXT"
            )
        ));
        if self.todos.is_empty() {
            out.push(dim(
                &self.cfg,
                "    none yet. The model writes them with todo_write; `/todo TEXT` adds yours.",
            ));
        }
        // **The operator's rows numbered, so `/todo done N` and `/todo rm N` name the row the
        // reader can count to** — over the operator's half and not the union, since the model's
        // rows are not theirs to edit.
        let mut mine_at = 0usize;
        for t in &self.todos {
            let mark = match t.status {
                letibot_sessionlog::event::TodoStatus::Pending => TodoMark::Open,
                letibot_sessionlog::event::TodoStatus::InProgress => TodoMark::Doing,
                letibot_sessionlog::event::TodoStatus::Completed => TodoMark::Done,
                letibot_sessionlog::event::TodoStatus::Postponed => TodoMark::Postponed,
            };
            let is_mine = t.by == letibot_sessionlog::event::TodoBy::Operator;
            let number = if is_mine {
                mine_at += 1;
                format!("{mine_at:>2}  ")
            } else {
                "    ".to_string()
            };
            let who = if is_mine { "you" } else { "model" };
            // **AND WHAT THE ROW IS WAITING ON, when it is waiting on anything.** The pane never
            // drew a row's condition at all, so a row filed with `when` (or the card's third field)
            // was indistinguishable from an unconditional one once it was on the board — the
            // handle existed in the store and nowhere a reader could see it. It matters most for a
            // POSTPONED row, whose condition is the thing that is *kept and not fired*: without
            // this the row would read as one whose condition had been dropped, which is the one
            // reading the state must not invite.
            let waiting = match &t.when {
                Some(letibot_sessionlog::event::TodoCondition::Job { handle }) => {
                    format!(" · waits on {handle}")
                }
                None => String::new(),
            };
            // **The mark is on the operator's rows only.** The model's rows are not stops — no key
            // acts on one — so a cursor that stopped there would be a cursor the operator presses
            // keys into and nothing happens. See [`App::todos_stops`].
            let cursor_here = is_mine && marked(&TodoStop::Mine(t.content.clone()));
            if is_mine {
                stop_rows.push(out.len());
            }
            out.push(format!(
                "  {} {number}{} {}  {}{}",
                if cursor_here { "▸" } else { " " },
                mark.painted(&self.cfg),
                without_control_lines(&t.content),
                // The tag is FAINT: it is the aside on the row and the content is what is read.
                dim(&self.cfg, &format!("— {who}")),
                // …and so is the condition, for the same reason: it is what the row is WAITING
                // on, which is an aside about the row and not the row.
                dim(&self.cfg, &waiting),
            ));
        }
        out.push(String::new());
        // **What the model may and may not do with these**, from the daemon's own rules: it may
        // move a row's STATUS (by quoting its words — there is no id on the wire) and may not
        // REMOVE one, because membership and order are the head's while status is the daemon's.
        out.push(dim(
            &self.cfg,
            "  the model sees these and is reminded of them; it can mark one done, and cannot \
             remove yours",
        ));
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            // **Not "the operator's queue".** *Queued* is this head's word for a PROMPT the daemon
            // owes a row for — `pub const QUEUED`, and the echo mark on every line the operator has
            // sent and not seen land. Reusing it here says the opposite of what is true: these rows
            // are not waiting for the model, they are what the project intends.
            "  the repo's TODO.md — what the project intends; not the model's plan:",
        ));
        match &self.repo_todos {
            None => out.push(dim(
                &self.cfg,
                "    not read yet — close and reopen the pane.",
            )),
            Some(lines) => {
                if lines.is_empty() {
                    out.push(dim(&self.cfg, "    no sections found."));
                }
                // Only the items take a cursor: a heading is a roll-up of the
                // rows under it and there is nothing to unfold on one.
                for (i, r) in lines.iter().enumerate() {
                    // **From the ONE enumeration, not from `repo_sel`** — which is the shadow the
                    // stop cursor writes through `sync_repo_from_stop`, and reading it here is what
                    // left a `▸` on a repo row while the cursor was up on the add control. Two
                    // cursors drawn from two facts was the defect; there is one fact.
                    let here = r.item && marked(&TodoStop::Repo(i));
                    // **A row's stop is its FIRST line**, so the body of an unfolded item does not
                    // move the cursor's target — recorded before the row draws, because the row
                    // renders to as many lines as its body needs.
                    if r.item {
                        stop_rows.push(out.len());
                    }
                    let pad = " ".repeat(r.indent.saturating_sub(2));
                    let cursor = if here { "▸ " } else { "  " };
                    let open = here && self.repo_open;
                    // `···` says an item has more without saying how much — the
                    // count would be lines, which is not a unit anybody cares
                    // about, and the only useful answer is to look.
                    let more = if !r.body.is_empty() && !open {
                        " ···"
                    } else {
                        ""
                    };
                    out.push(match r.mark {
                        Some(m) => {
                            format!("{pad}{cursor}{} {}{more}", m.painted(&self.cfg), r.text)
                        }
                        // A heading with no items: no box to paint, and the text
                        // is the operator's prose rather than a task.
                        None => dim(&self.cfg, &format!("{pad}{cursor}{}", r.text)),
                    });
                    if open {
                        for l in &r.body {
                            out.push(dim(&self.cfg, &format!("{pad}        {l}")));
                        }
                    }
                }
            }
        }
        self.todos_stop_rows = stop_rows;
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            // **The one sentence that stops the two sections being one list.** The operator's
            // ruling, 2026-09-29: *"host specific todo is actionable but shared todo.md items are
            // promotable."* So the difference between the halves above and the rows here is not
            // which file they came out of — it is that the model is never told about these. The
            // sentence above the section says what the model IS reminded of; without this one, a
            // reader has to infer the negative, and the two sections look alike enough to invite
            // the wrong inference.
            //
            // **Nothing here is promotable yet on this head**, and that is why the sentence names
            // no key: R29's rule is that a disclosure carries the act that undoes it, and a pane
            // that promised a gesture it does not bind would be the failure the rule prevents.
            "  the file itself is in the workspace; this pane never writes it, and the model is \
             never told about it — nothing here is a task it has been given.",
        ));
        // **The keys, said where they are used.** The cursor walks three kinds of row now, and
        // what Enter does depends on which one it is on — a hint that named only the unfold was
        // written when the cursor never left the repo's items.
        out.push(dim(
            &self.cfg,
            "  ↑↓ moves (or click a row) · enter on [+] adds, on your row toggles it, on a repo \
             item unfolds · esc closes",
        ));
        // **And the one act the pane does not bind, said where its rows are.** `[p]` is a mark
        // this pane has and `TODO.md` does not, so it is the one mark a reader cannot look up in
        // org — and the two verbs are named here rather than only in `/help`, because a state you
        // can see and cannot lift is a state that looks like a bug. Two short lines rather than
        // one long one: the pane trims to the window, and a sentence whose second half is off the
        // edge is a sentence that named nothing.
        out.push(dim(
            &self.cfg,
            "  `[p]` is a row you set aside — it stays on the board and the model still sees it:",
        ));
        out.push(dim(
            &self.cfg,
            "  the check stops asking about it · `/todo postpone N` · `/todo resume N`",
        ));
        out.into_iter().map(|l| trim_to(&l, w)).collect()
    }
}

/// The repo's `TODO.md` as a section map: one line per `##` section with its
/// open and done checkbox counts. Read fresh on every pane-open — the file is
/// the operator's to edit, and a cached map is a cache of somebody else's
/// intention. Errors name themselves; a missing file is a fact about the
/// workspace, not a panic in a pane.
/// **The repo's `TODO.md`, as org-mode reads a list of checkboxes.**
///
/// It used to render one line per `## ` heading with the counts beside it —
/// *"Phase 0 — repo — 0 open, 2 done"* — and nothing else. The items themselves,
/// which are the queue, were never drawn. The operator, looking at `leticl`'s:
/// *"our todo pane doesnt render them - only section titles and sub todos
/// count"*.
///
/// So the items are drawn under their heading, and the heading carries the state
/// org would give it: a parent is DONE when every child is, and org's own cookie
/// — `[2/7]` — says how far along the rest are. Three marks, matching the ones
/// this file's own legend defines and the ones `todo_write` uses, so the two
/// halves of this pane read alike:
///
/// ```text
///   [x] Phase 0 — repo                        [2/2]
///       [x] T1 git init, .gitignore, commit PLAN.md + TODO.md.
///       [x] T2 vendor yason + alexandria + trivial-gray-streams, pinned in
///   [~] Phase 7 — parity                      [3/9]
///       [x] S1 the side-by-side diff
///       [~] S2 the session picker
///       [ ] S3 the mode card
/// ```
///
/// A heading with no items at all is not "done": an empty section is a section
/// nobody has filled in, and org does not mark it either. It carries no cookie
/// and no box.
pub(crate) fn repo_todos_map(workspace: &str) -> Vec<TodoRow> {
    // **The project directory, not the workspace** — the same resolution the git field uses
    // (`gitfield::project_dir`), so a nested layout (session in `Projects/x`, repo in
    // `Projects/x/x`) loads one tree's TODO.md in the pane and one tree's branch in the header,
    // and never one of each.
    let path = crate::gitfield::project_dir(workspace).join("TODO.md");
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) => {
            return vec![TodoRow {
                indent: 4,
                mark: None,
                text: format!("(no TODO.md in {workspace}: {e})"),
                body: Vec::new(),
                item: false,
            }];
        }
    };
    render_todo_md(&body)
}

/// **The two numbers the todos pane's header draws**, from the one list it draws its rows from.
///
/// `(open, postponed)`. `open` is every row the model still owes — `pending` or `in_progress`,
/// which is exactly the set the idle check may ask about — and `postponed` is every row the
/// operator has set aside. `completed` is neither: it is a record, and a header counting it would
/// be answering a question nobody asks at a glance.
///
/// **A free function over the list, and not a second count kept anywhere.** The defect this exists
/// to prevent is a pane that disagrees with itself — a header derived from the wire while the rows
/// come from the head's own copy, or a total maintained beside the list it counts — and the way to
/// make that impossible is for the count and the rows to be two readings of ONE argument.
/// `todos_lines` passes the same `self.todos` it is about to draw.
pub(crate) fn todo_counts(todos: &[letibot_sessionlog::event::TodoEntry]) -> (usize, usize) {
    let mut open = 0usize;
    let mut postponed = 0usize;
    for t in todos {
        match t.status {
            letibot_sessionlog::event::TodoStatus::Pending
            | letibot_sessionlog::event::TodoStatus::InProgress => open += 1,
            letibot_sessionlog::event::TodoStatus::Postponed => postponed += 1,
            letibot_sessionlog::event::TodoStatus::Completed => {}
        }
    }
    (open, postponed)
}

/// One item's state, in the marks org and `todo_write` share.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoMark {
    Open,
    Doing,
    Done,
    /// **Set aside by the operator** — the session board's fourth state, and the only one a
    /// `TODO.md` has no syntax for: a file cannot say *still owed, and not being asked for*.
    /// `TodoMark::of` therefore never returns it, which is what keeps the FILE's vocabulary three
    /// marks wide while the board's is four. See `TodoStatus::Postponed` for the state itself.
    Postponed,
}

impl TodoMark {
    /// `- [ ]`, `- [~]`, `- [x]` — and `*` for the other bullet org and markdown
    /// both accept. Anything else in the box is not a checkbox this reads.
    ///
    /// **Three marks and not four**: `[p]` is the session board's and is deliberately not read
    /// out of a file, because a file has no way to say *still owed, and not being asked for*.
    pub(crate) fn of(line: &str) -> Option<(TodoMark, &str)> {
        let t = line.trim_start();
        let rest = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))?;
        let (boxed, text) = rest.split_at_checked(3)?;
        let mark = match boxed {
            "[ ]" => TodoMark::Open,
            "[x]" | "[X]" => TodoMark::Done,
            "[~]" | "[-]" => TodoMark::Doing,
            _ => return None,
        };
        Some((mark, text.trim()))
    }

    pub(crate) fn glyph(self) -> &'static str {
        match self {
            TodoMark::Open => "[ ]",
            TodoMark::Doing => "[~]",
            TodoMark::Done => "[x]",
            TodoMark::Postponed => "[p]",
        }
    }

    /// **The glyph, painted.** The same three colours the jobs pane uses for the
    /// same three states, because a head that painted "finished" green in one
    /// pane and plain in another would be teaching two vocabularies for one fact.
    ///
    /// An open item is deliberately uncoloured: it is the default state and the
    /// majority of any list, and colouring the majority spends the signal the
    /// other two carry. Painted here rather than by `colour()` with an empty
    /// code, because that helper appends a RESET unconditionally and so put a
    /// bare `ESC[0m` after every open box — an escape that closes nothing.
    ///
    /// **`[p]` is DIMMED and not given a fourth hue.** A postponed row is not a fourth kind of
    /// thing on the list — it is the same kind of thing, turned down — and the attribute
    /// de-emphasises whatever foreground the reader's theme chose, which is what the frame around
    /// a quotation already uses for the same purpose. A fourth colour would be a fourth thing to
    /// learn, on the one row whose whole meaning is *this one is not shouting*.
    pub(crate) fn painted(self, cfg: &RenderConfig) -> String {
        match self {
            TodoMark::Open => self.glyph().to_string(),
            TodoMark::Doing => colour(cfg, sgr::YELLOW, self.glyph()),
            TodoMark::Done => colour(cfg, sgr::GREEN, self.glyph()),
            TodoMark::Postponed => colour(cfg, sgr::DIM, self.glyph()),
        }
    }
}

/// The parsing and rendering, split from the read so it can be tested without a
/// file.
/// One rendered row: how far it is indented, the mark to paint, and the text.
///
/// The indent is carried rather than baked into the text because it belongs
/// BEFORE the mark, and the mark is the part the pane paints — a row that
/// arrived pre-indented painted as `[x]     Phase 0` with the colour in the
/// wrong place entirely. Split so the parse is testable without a palette.
/// One rendered row: how far it is indented, the mark to paint, the text, and
/// the item's own continuation lines when it has any.
///
/// The indent is carried rather than baked into the text because it belongs
/// BEFORE the mark, and the mark is the part the pane paints — a row that
/// arrived pre-indented painted as `[x]     Phase 0` with the colour in the
/// wrong place entirely. Split so the parse is testable without a palette.
pub(crate) struct TodoRow {
    /// Columns before the mark. Carried rather than baked into the text because
    /// it belongs BEFORE the mark, and the mark is the part the pane paints — a
    /// pre-indented row painted as `[x]     Phase 0`, the colour in front of the
    /// whitespace rather than on the box.
    pub(crate) indent: usize,
    /// `None` only for a heading with no checkboxes under it, which org does not
    /// mark either.
    pub(crate) mark: Option<TodoMark>,
    pub(crate) text: String,
    pub(crate) body: Vec<String>,
    /// **A heading carries a mark too** — the roll-up of the rows beneath it —
    /// so the mark cannot be what tells the two apart, and the cursor landed on
    /// headings when it was. There is nothing to unfold on one.
    pub(crate) item: bool,
}

/// One item as the file has it: its mark, its first line, and the continuation
/// lines under it.
///
/// **The body was dropped**, so `T2 vendor yason + alexandria +
/// trivial-gray-streams, pinned in` was the whole of what the pane showed — an
/// item trailing off mid-sentence, with the commits it pins and the `Deps:` line
/// that says what blocks it both gone. The operator, after the items were finally
/// drawn at all: *"if a todo has some associated text? should i be able to expand
/// it somehow?"*
pub(crate) struct TodoItem {
    pub(crate) mark: TodoMark,
    pub(crate) head: String,
    pub(crate) body: Vec<String>,
}

pub(crate) fn render_todo_md(body: &str) -> Vec<TodoRow> {
    let mut out: Vec<TodoRow> = Vec::new();
    let mut section: Option<String> = None;
    let mut items: Vec<TodoItem> = Vec::new();

    let flush = |out: &mut Vec<TodoRow>, section: &Option<String>, items: &[TodoItem]| {
        let Some(name) = section else { return };
        if items.is_empty() {
            // Not `[x]`: an empty section is one nobody has filled in, and org
            // does not mark it done either.
            out.push(TodoRow {
                indent: 6,
                mark: None,
                text: name.clone(),
                body: Vec::new(),
                item: false,
            });
            return;
        }
        let done = items.iter().filter(|i| i.mark == TodoMark::Done).count();
        // **Org's rule for a parent.** Every child done makes the parent done;
        // any child started makes it started; otherwise it is open.
        let roll = if done == items.len() {
            TodoMark::Done
        } else if items.iter().any(|i| i.mark != TodoMark::Open) {
            TodoMark::Doing
        } else {
            TodoMark::Open
        };
        out.push(TodoRow {
            indent: 4,
            mark: Some(roll),
            text: format!("{name}  [{done}/{}]", items.len()),
            body: Vec::new(),
            item: false,
        });
        for i in items {
            out.push(TodoRow {
                indent: 8,
                mark: Some(i.mark),
                text: i.head.clone(),
                body: i.body.clone(),
                item: true,
            });
        }
    };

    // Whether the last item is still collecting continuation lines. A blank line
    // closes it: two items separated by one would otherwise merge, and the prose
    // between a heading and its list would land on whatever came before.
    let mut collecting = false;

    for line in body.lines() {
        // `##` and deeper: `###` is a subsection and its items belong to it, not
        // to the `##` above, which is what org's outline says too.
        if let Some(name) = line
            .strip_prefix("## ")
            .or_else(|| line.strip_prefix("### "))
        {
            flush(&mut out, &section, &items);
            section = Some(name.trim().to_string());
            items.clear();
            collecting = false;
        } else if let Some((mark, text)) = TodoMark::of(line) {
            items.push(TodoItem {
                mark,
                head: strip_markup(text),
                body: Vec::new(),
            });
            collecting = true;
        } else if line.trim().is_empty() {
            collecting = false;
        } else if collecting
            && (line.starts_with(' ') || line.starts_with('\t'))
            && let Some(last) = items.last_mut()
        {
            // An indented line under an item is that item's detail — which is
            // where a TODO.md puts the commit it pins and the `Deps:` that says
            // what blocks it.
            last.body.push(strip_markup(line.trim()));
        } else {
            // Anything at column zero that is not a checkbox ends the item.
            collecting = false;
        }
    }
    flush(&mut out, &section, &items);
    out
}

/// `**T1** git init` → `T1 git init`. The pane has one style for this text and
/// markdown's emphasis markers are noise in it; the backticks go for the same
/// reason. Nothing else is interpreted — this is a reader, not a renderer.
pub(crate) fn strip_markup(text: &str) -> String {
    text.replace("**", "").replace('`', "")
}
