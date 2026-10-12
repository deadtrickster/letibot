//! **The todos pane**: the session's plan and the repository's queue (`TODO.md`), and the
//! reader that turns `TODO.md` into its rows. The pane is drawn by `rano::agent::todos`; the
//! reader, the counts and which row the cursor is on are this head's.

use crate::app::*;
use crate::ui::render::{row_strings, wrap};
use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoNeed, TodoStatus};
use rano::agent::pane;
use rano::agent::text::clean_line;
use rano::agent::todos::{RepoRow, TodoLine, TodosPane};
use rano::render::Line;

impl App {
    pub(crate) fn todos_lines(&mut self, w: usize) -> Vec<String> {
        use letibot_sessionlog::event::{TodoBy, TodoStatus};
        let (open, postponed) = todo_counts(&self.todos);
        // **Through the one enumeration**: the row the cursor is on is the stop at
        // `todos_sel` of the same list the keys act on, matched here by identity (the
        // operator's rows by their words, the file's by their place).
        let stops = self.todos_stops();
        let cursor = self.todos_sel.min(stops.len().saturating_sub(1));
        let marked = |want: &TodoStop| stops.get(cursor).is_some_and(|it| it == want);
        let mut mine_at = 0usize;
        let pane = TodosPane {
            open,
            postponed,
            on_add: marked(&TodoStop::Add),
            // **A board row has no BODY, and the title cap is what makes that livable.** rano's
            // `TodoLine` is `mark`/`content`/`mine`/`number`/`waits_on` — there is no field for a
            // detail, and `RepoRow` (the file's rows below) is the only row in this pane that has
            // one. So a row's detail rides in `content` after ` — ` (`TODO_DETAIL_SEP`) and is
            // drawn on the same line, cut at `w` like everything else; what the cap bounds
            // (`TODO_TITLE_MAX`) is the TITLE, so the part that identifies the row always fits and
            // the detail is what runs off the edge. **What would give it a line of its own is a
            // `TodoLine::detail` in rano** — the pinned v0.8.1 has none — and not a row this head
            // splices in under the pane's: a click and the arrows read the row numbers the pane
            // recorded while drawing (`PaneLines::stop_rows`), so one extra line would put every
            // stop below it a row out and the two readings would disagree.
            todos: self
                .todos
                .iter()
                .map(|t| {
                    let mine = t.by == TodoBy::Operator;
                    if mine {
                        mine_at += 1;
                    }
                    TodoLine {
                        mark: match t.status {
                            TodoStatus::Pending => TodoMark::Open,
                            TodoStatus::InProgress => TodoMark::Doing,
                            TodoStatus::Completed => TodoMark::Done,
                            TodoStatus::Postponed => TodoMark::Postponed,
                            // **rano's four marks have no fifth, and this is the one honest gap of
                            // the cancelled row.** `TodoMark` is rano's (`rano::agent::todos`), the
                            // pane that paints it is rano's, and the pinned rano has no
                            // `Cancelled` — so a row the operator struck off draws in the SET-ASIDE
                            // register, which is the closest of the four: faint, kept on the board,
                            // and not asked about. What a reader still gets right is everything that
                            // matters — the row's own words, its author, its place, and the header's
                            // count, which never counted it as open — and `/todo resume N` is how it
                            // comes back. When rano grows the mark, this arm becomes it and nothing
                            // else here moves.
                            TodoStatus::Cancelled => TodoMark::Postponed,
                        },
                        content: t.content.clone(),
                        mine,
                        number: mine_at,
                        // **And a wait `wait_on` armed**, which is a `needs` edge on a job or a
                        // child, not a `when` — the operator: *"will todo pane show me that waits
                        // are armed?"*. Before this only the row's card said so. The first such
                        // edge is the one drawn; the card lists them all.
                        //
                        // **A row waiting on the OPERATOR says `waits on you`, first** — *"the
                        // heads show loud notification that im waited"*; Enter on it opens the
                        // question. A done or struck-off row asks nobody anything.
                        //
                        // **A job is drawn with its slug and a child by its short name** — the
                        // operator: *"\"waits on j97\" - make j97 a link to the real job, also
                        // render job slug"*. `waits on j97 · pg-timing`, from the head's job list
                        // ([`App::wait_label`]); the words are the link a click opens.
                        waits_on: if asks_operator(t) {
                            Some("you".into())
                        } else {
                            wait_of(t).map(|w| self.wait_label(&w))
                        },
                        // **The mark follows the STOP, whichever half the row is on.** rano draws
                        // the `▸` from this flag alone, so a model's row left at `false` would be
                        // a stop the cursor walks onto with nothing drawn under it — the same
                        // defect as a cursor that is not where the keys think it is, one half of
                        // the list over. `todos_stops` is the one enumeration that says which row
                        // that is.
                        cursor: marked(&if mine {
                            TodoStop::Mine(t.content.clone())
                        } else {
                            TodoStop::Theirs(t.content.clone())
                        }),
                    }
                })
                .collect(),
            repo: self.repo_todos.as_ref().map(|rows| {
                rows.iter()
                    .enumerate()
                    .map(|(i, r)| {
                        let here = r.item && marked(&TodoStop::Repo(i));
                        RepoRow {
                            indent: r.indent,
                            mark: r.mark,
                            text: r.text.clone(),
                            body: r.body.clone(),
                            item: r.item,
                            cursor: here,
                            open: here && self.repo_open,
                        }
                    })
                    .collect()
            }),
        };
        // **Drawn once at full width, and cut to `w` from that** — the same cut `content(w)`
        // makes (`PaneLines::trimmed`), kept beside the uncut rows because a link's columns are
        // read off the row as rano built it, before anything was cut away.
        let full = pane.content(usize::MAX);
        let content = full.clone().trimmed(w);
        // **The rows the stops landed on, taken as they went out** — what the arrows scroll by
        // and a click is tested against. The model's rows are stops now and rano does not record
        // them, so the record is completed here; see [`session_stop_rows`].
        self.todos_stop_rows = session_stop_rows(&content, &self.todos);
        self.todos_wait_links = self.wait_links(&full, &content, w);
        row_strings(&content.lines, self.cfg.palette())
    }

    /// **Where each row's `waits on …` words landed**, as links a click opens — the operator:
    /// *"\"waits on j97\" - make j97 a link to the real job, also render job slug"*.
    ///
    /// # Read off the row rano drew, and checked against it
    ///
    /// rano draws a session row as its furniture, the content, `— who`, and last the run
    /// ` · waits on {waits_on}` (`rano::agent::todos::TodosPane::content`); the session rows are
    /// the block directly under the add control, one row each, in the list's order — the shape
    /// [`session_stop_rows`] relies on too. So the link's first column is the uncut row's width
    /// less that last run's, plus the ` · ` before `waits on`, and its end is the row's width.
    /// **Nothing is assumed that is not checked**: the row's last run must be exactly the words
    /// this head handed rano for that row, or no link is recorded for it — a rano that drew the
    /// row another way loses the link, never moves it onto the wrong words.
    ///
    /// # A row cut at the pane's edge
    ///
    /// `trimmed(w)` cuts a wider row to `w - 1` columns and an ellipsis. The link is then only the
    /// part of its words still on the screen — the cut row's width less the `…` — and a row cut
    /// before `waits on` begins has no link at all.
    fn wait_links(
        &self,
        full: &rano::agent::pane::PaneLines,
        cut: &rano::agent::pane::PaneLines,
        w: usize,
    ) -> Vec<WaitLink> {
        let Some(add) = full.stop_rows.first() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (i, t) in self.todos.iter().enumerate() {
            if asks_operator(t) {
                continue;
            }
            let Some(target) = wait_of(t) else {
                continue;
            };
            let row = add + 1 + i;
            let (Some(line), Some(shown)) = (full.lines.get(row), cut.lines.get(row)) else {
                continue;
            };
            let Some(last) = line.spans.last() else {
                continue;
            };
            let drawn = format!(" · waits on {}", clean_line(&self.wait_label(&target)));
            if last.content != drawn || !line.plain().contains(&clean_line(&t.content)) {
                continue;
            }
            let end = line.width();
            let from = end - last.width() + " · ".chars().count();
            let visible = if end > w {
                shown.width().saturating_sub(1)
            } else {
                end
            };
            let to = end.min(visible);
            if to > from {
                out.push(WaitLink {
                    row,
                    from,
                    to,
                    todo: i,
                    target,
                });
            }
        }
        out
    }

    /// **One row, whole** — the overlay the todos pane's Enter opens.
    ///
    /// # Why it is here at all
    ///
    /// The pane draws every row to the pane's own width (rano's `TodosPane::content` trims each
    /// line to `w`), and these rows are written by a model. The operator's report: *"in todo list
    /// i cant expand your todo items - only truncated titles available"*. **Nothing was lost on
    /// the wire** — `TodoEntry::content` arrives whole — it was only unreadable, and this is the
    /// one place on the screen where the whole of it is.
    ///
    /// # What it shows beyond the text
    ///
    /// The **author** and the **status**, because both are on the row and reading a row means
    /// reading them: the model's rows and the operator's own look alike in a list and are not the
    /// same fact (one is the operator's to toggle, the other is the model's to retire), and *open*,
    /// *in progress*, *done* and *set aside* are four different answers to *is this still owed*.
    /// What the row **waits on** (`when`) and what it **needs** (`needs`) are on the row as well,
    /// and they are the two facts a reader asks about next — *why has nothing happened* and *what
    /// has to land first*.
    ///
    /// A repo row's facts are the file's rather than a session's: the mark the file gave it, and
    /// its own continuation lines **shown whole under it** rather than behind the pane's `···`.
    ///
    /// # What happens when the row changes while this is open
    ///
    /// The row is looked up **by identity at DRAW time** and never carried in the overlay, so a
    /// `TodosUpdated` landing under an open overlay draws the NEW status rather than the one it
    /// had when Enter was pressed. **The words ARE the identity** — the board has no ids, which is
    /// `TodoStop`'s own docstring — so a row whose text changed is a different row, and the overlay
    /// says the row is gone rather than showing words that are no longer on the board. A repo row's
    /// identity is its place in a file this head re-reads on every draw (`refresh_repo_todos`), and
    /// an item edited away says the same thing.
    pub(crate) fn todos_open_lines(&self, w: usize) -> Vec<String> {
        let Some(stop) = self.todos_open.as_ref() else {
            return Vec::new();
        };
        let mut lines = vec![pane::title("todos — one row"), Line::default()];
        match stop {
            // **Reachable by no key**, and said rather than drawn empty: Enter on the control opens
            // the card, because the control is not a row with words of its own (`key_todos_pane`).
            TodoStop::Add => lines.push(Line::raw(
                "  the `[+]` control has no text of its own — enter there opens the card.",
            )),
            // **The operator's half and the model's half, by the same words and the same rule**:
            // `Mine` is the row whose author is the operator, `Theirs` is every other author, and
            // the lookup takes the first row with those words on that half — the same row the
            // stops were built from, in the list's own order.
            TodoStop::Mine(words) | TodoStop::Theirs(words) => {
                let mine = matches!(stop, TodoStop::Mine(_));
                let held = self
                    .todos
                    .iter()
                    .find(|t| t.content == *words && (t.by == TodoBy::Operator) == mine);
                match held {
                    Some(t) => {
                        lines.push(pane::faint(format!("    {}", self.row_facts(t))));
                        if !t.needs.is_empty() {
                            let needs: Vec<String> =
                                t.needs.iter().map(|n| self.need_words(n)).collect();
                            lines.push(pane::faint(format!("    needs {}", needs.join(", "))));
                        }
                        lines.push(Line::default());
                        push_whole(&mut lines, &t.content, w);
                    }
                    None => lines.push(Line::raw(format!(
                        "  `{}` is not on the board any more — a row's words are its identity, so a \
                         row renamed is a different row. esc goes back to the list.",
                        clean_line(words)
                    ))),
                }
            }
            TodoStop::Repo(i) => match self.repo_todos.as_ref().and_then(|rows| rows.get(*i)) {
                Some(r) if r.item => {
                    lines.push(pane::faint(format!(
                        "    the repo's TODO.md · {}",
                        mark_words(r.mark)
                    )));
                    lines.push(Line::default());
                    push_whole(&mut lines, &r.text, w);
                    if !r.body.is_empty() {
                        lines.push(Line::default());
                        for b in &r.body {
                            push_whole(&mut lines, b, w);
                        }
                    }
                }
                _ => lines.push(Line::raw(format!(
                    "  row {i} of TODO.md is not there any more — the file is read again on every \
                     draw, so an edit under this overlay moves what that row number points at. esc \
                     goes back to the list."
                ))),
            },
        }
        lines.push(Line::default());
        lines.push(pane::faint("    esc goes back to the list"));
        row_strings(&lines, self.cfg.palette())
    }
}

/// **A row's own words, whole, wrapped and indented** — the one place in this pane where a row is
/// not cut to the pane's width.
///
/// **Split on newlines first, and cleaned line by line**: `clean_line` turns a newline into a space
/// (§3.1's guard, because a row is text this head did not write), so a row that carries newlines —
/// which a model may write — has to be broken before it is cleaned or the shape it was written in
/// is lost. A line that wraps to nothing is kept as a blank row, because the blank is part of what
/// the author wrote.
fn push_whole(out: &mut Vec<Line>, text: &str, w: usize) {
    for l in text.split('\n') {
        let rows = wrap(&clean_line(l), w.saturating_sub(2).max(1));
        if rows.is_empty() {
            out.push(Line::default());
        }
        for one in rows {
            out.push(Line::raw(format!("  {one}")));
        }
    }
}

impl App {
    /// **Who wrote a row and where it stands** — the overlay's own header line, and the two facts
    /// the pane's row carries in its margin (`— model`, the mark). A `when` job is said as the
    /// pane says it, with its slug: `waits on j97 · pg-timing`.
    fn row_facts(&self, t: &TodoEntry) -> String {
        let mut s = format!("{} · {}", author_words(&t.by), status_words(t.status));
        if let Some(letibot_sessionlog::event::TodoCondition::Job { handle }) = &t.when {
            let label = self.wait_label(&WaitOn::Job(handle.clone()));
            s.push_str(&format!(" · waits on {}", clean_line(&label)));
        }
        s
    }

    /// One edge of `needs`, as the sentence a reader can act on. A job carries its slug as the
    /// pane draws it; a child its short name **and its whole id**, because this view is where the
    /// id the pane shortened is still to be read.
    fn need_words(&self, n: &TodoNeed) -> String {
        match n {
            TodoNeed::Job { handle } => {
                format!(
                    "job {}",
                    clean_line(&self.wait_label(&WaitOn::Job(handle.clone())))
                )
            }
            TodoNeed::Child { id } => {
                let label = self.wait_label(&WaitOn::Child(id.clone()));
                if label == *id {
                    format!("child {}", clean_line(id))
                } else {
                    format!("child {} ({})", clean_line(&label), clean_line(id))
                }
            }
            other => need_words(other),
        }
    }
}

/// **The row waits on the OPERATOR** — an open row with a question edge. It draws `waits on you`
/// and its act is the question card, not a link.
fn asks_operator(t: &TodoEntry) -> bool {
    !matches!(t.status, TodoStatus::Completed | TodoStatus::Cancelled)
        && t.needs
            .iter()
            .any(|n| matches!(n, TodoNeed::Operator { .. }))
}

/// The author, in the pane's own words — `you` and `the model`, one row apart.
///
/// **A parent's row shows the string it arrived with** (`Parent <session-id>`): the variant
/// carries the author exactly as the operator specified it, and re-spelling it here would be a
/// second name for one fact.
fn author_words(by: &TodoBy) -> String {
    match by {
        TodoBy::Operator => "you".into(),
        TodoBy::Model => "the model".into(),
        TodoBy::Parent(who) => clean_line(who),
        TodoBy::Queue => "the merge queue".into(),
    }
}

/// The row's state as a reader's word. The pane draws rano's four marks (`[ ]`, `[~]`, `[x]`,
/// `[p]`); this is the same four said out, plus the fifth state the marks have no glyph for.
fn status_words(s: TodoStatus) -> &'static str {
    match s {
        TodoStatus::Pending => "open",
        TodoStatus::InProgress => "in progress",
        TodoStatus::Completed => "done",
        TodoStatus::Postponed => "set aside",
        TodoStatus::Cancelled => "cancelled",
    }
}

/// One edge of `needs`, as the sentence a reader can act on.
fn need_words(n: &TodoNeed) -> String {
    match n {
        TodoNeed::Row { content } => format!("row `{}`", clean_line(content)),
        TodoNeed::Child { id } => format!("child {}", clean_line(id)),
        TodoNeed::Job { handle } => format!("job {}", clean_line(handle)),
        TodoNeed::Operator { question, options } => format!(
            "YOU — \"{}\" ({}) — enter on the row answers it",
            clean_line(question),
            options
                .iter()
                .map(|o| clean_line(o))
                .collect::<Vec<_>>()
                .join(" / ")
        ),
        TodoNeed::Answered { question, reply } => format!(
            "answered — \"{}\": {}",
            clean_line(question),
            clean_line(reply)
        ),
        // **Kept rather than dropped** — a plan is what would be lost — and said as the one thing
        // this build can honestly say about it.
        TodoNeed::Unknown => "an edge this build does not know".into(),
    }
}

/// A `TODO.md` item's mark as a word, for the overlay's header line.
fn mark_words(m: Option<TodoMark>) -> &'static str {
    match m {
        Some(TodoMark::Open) => "open",
        Some(TodoMark::Doing) => "in progress",
        Some(TodoMark::Done) => "done",
        Some(TodoMark::Postponed) => "set aside",
        // A heading carries no mark; a heading is not a stop, so this is the honest answer rather
        // than a guess at a state the file never gave.
        None => "unmarked",
    }
}

/// **Every stop's drawn row, with the session's rows put back in.**
///
/// rano's `TodosPane::content` records a stop row only for the rows its own pane treats as stops —
/// the add control, the operator's rows (`mine`) and the repo's items — because its pane has no act
/// on a model's row. This head's cursor does now (Enter reads one whole, `App::todos_stops`), so
/// without this the record and the enumeration would be two lists of different lengths: every stop
/// after the first model's row would scroll the cursor to a row that is not its own, and a click
/// would move the cursor to a different row than the one under the pointer. That is the
/// two-enumerations defect this pane's docstrings are a monument to, arriving from the library side.
///
/// **The session's rows are one contiguous block of exactly `todos.len()` rows directly under the
/// add control**, which is what rano draws (the control, then the list, one row each, in order), so
/// the block is placed from the control's own recorded row and the repo's recorded rows follow it
/// untouched. **The shape is checked rather than trusted**: if the record is shorter than the
/// layout implies, or the block would run past the rows that were drawn, rano's own record is kept
/// — the model's rows then scroll to their neighbour's row rather than the whole list pointing
/// somewhere wrong.
fn session_stop_rows(
    content: &rano::agent::pane::PaneLines,
    todos: &[letibot_sessionlog::event::TodoEntry],
) -> Vec<usize> {
    use letibot_sessionlog::event::TodoBy;
    let recorded = &content.stop_rows;
    // The recorded stops are the control, the operator's rows in order, then the repo's items.
    let mine = todos.iter().filter(|t| t.by == TodoBy::Operator).count();
    let repo_from = 1 + mine;
    let start = match recorded.first() {
        Some(first) if recorded.len() >= repo_from => first + 1,
        _ => return recorded.clone(),
    };
    if start + todos.len() > content.lines.len() {
        return recorded.clone();
    }
    let mut out = Vec::with_capacity(recorded.len() + todos.len() - mine);
    out.push(recorded[0]);
    out.extend(start..start + todos.len());
    out.extend_from_slice(&recorded[repo_from..]);
    out
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
/// be answering a question nobody asks at a glance. **`cancelled` is neither for the same reason**,
/// and it is not folded into `postponed` even though both draw `[p]`: this count is read off the
/// list, and the list knows the difference.
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
            letibot_sessionlog::event::TodoStatus::Completed
            | letibot_sessionlog::event::TodoStatus::Cancelled => {}
        }
    }
    (open, postponed)
}

/// One item's state, in the marks org and `todo_write` share.
/// **A checklist mark** — `[ ]`, `[~]`, `[x]`, `[p]` — rano's, because the pane that paints it
/// is rano's; the reader below parses the file's spellings with its `of`.
pub(crate) use rano::agent::todos::TodoMark;

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
