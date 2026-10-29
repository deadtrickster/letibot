//! **A plugin's pane as a screen**: the document a `pane_<name>` hook answered
//! (`docs/plugins-slice1.md`'s C3), drawn the way serenedash drew it — an overview of panels,
//! every panel a summary with a full view behind its own key, findings collected in one list,
//! a legend, and one stop's detail.
//!
//! # The one grid
//!
//! Every `Line` goes through [`line_row`], and the columns it is laid out in are measured
//! **once per screen over every line on it** — [`grid`] — not once per panel. That is the
//! whole of "the numbers line up down the frame": two panels whose bars are drawn against two
//! different rulers are two bars a reader cannot compare, which is the defect the shared grid
//! exists to prevent.
//!
//! # Stops, and why the walk is shared
//!
//! A stop is a row the cursor can rest on: a `Line` or a `Row` with an `id`, a markdown item,
//! a finding (C3). [`each`] is the ONE walk over a screen, and both the drawing ([`content`])
//! and the list the keys move through ([`stops`]) read it — so the cursor cannot land on a
//! row the pane does not draw, which is the class of defect this tree has already paid for
//! twice (`app::hits`).
//!
//! # Text here is foreign
//!
//! Everything a view carries came from a plugin — a wasm module in the operator's tree — so
//! it goes through rano's `text::clean` before it is measured: an escape in a label would
//! otherwise be counted by the wrapper and not drawn, and a row measured one way and drawn
//! another is a row that misaligns.

use letibot_sessionlog::pane as pv;
use rano::agent::pane::{self, PaneLines};
use rano::agent::text::{clean, clean_line, wrapped};
use rano::agent::tree_list::{Action as TreeAction, Cell, Row as TreeRow, TreeList};
use rano::render::{Line, Span};
use rano::style::Role;

use crate::ui::render::{trim_to, visible_width};

/// The eight steps a sparkline is drawn in, shortest to tallest — a history read as a shape
/// rather than as numbers.
const STEPS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// A bar's columns. Fixed, because a bar's LENGTH is the thing being read: two bars a row
/// apart have to be measured against the same ruler, so the ruler cannot follow the value.
const BAR: usize = 10;

/// What every row spends before its columns: the stop's mark, and the space after it.
const LEAD: usize = 2;

/// **Which of a pane's screens is drawn.** `Overview` is its head; `Panel(i)` is the view
/// behind panel `i`'s key; `Findings` and `Legend` are the host's two. A stop read whole is
/// not one of these: it is a surface of its own, drawn by [`detail`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Screen {
    #[default]
    Overview,
    Panel(usize),
    Findings,
    Legend,
}

/// **A row the cursor can rest on**, addressed by position rather than by reference: the view
/// it came from is replaced wholesale on every look, so a name would outlive its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// Panel `panel`, block `block`, item `item` — a line, a table or tree row, or a markdown
    /// item, in the order that panel's blocks are drawn.
    Item {
        panel: usize,
        block: usize,
        item: usize,
    },
    /// Finding `index`.
    Finding(usize),
}

/// **One item of a block**, with the address a stop needs and the row's own data.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Item<'a> {
    Line {
        panel: usize,
        block: usize,
        item: usize,
        line: &'a pv::Line,
    },
    Row {
        panel: usize,
        block: usize,
        item: usize,
        row: &'a pv::Row,
    },
    Kv {
        label: &'a str,
        value: &'a str,
    },
    Markdown {
        panel: usize,
        block: usize,
        item: usize,
        text: &'a str,
        heading: bool,
    },
    Finding {
        index: usize,
        finding: &'a pv::Finding,
    },
}

impl Item<'_> {
    /// **The stop this item is, or nothing.** C3's rule: a line or a row is a stop when it
    /// carries an `id` (an action names its row by one, and Enter has to have something to
    /// read); a markdown item and a finding always are.
    pub(crate) fn stop(&self) -> Option<Stop> {
        match self {
            Item::Line {
                panel,
                block,
                item,
                line,
            } if !line.id().is_empty() => Some(Stop::Item {
                panel: *panel,
                block: *block,
                item: *item,
            }),
            Item::Row {
                panel,
                block,
                item,
                row,
                ..
            } if !row.id().is_empty() => Some(Stop::Item {
                panel: *panel,
                block: *block,
                item: *item,
            }),
            Item::Markdown {
                panel, block, item, ..
            } => Some(Stop::Item {
                panel: *panel,
                block: *block,
                item: *item,
            }),
            Item::Finding { index, .. } => Some(Stop::Finding(*index)),
            _ => None,
        }
    }
}

/// **What a stop IS**, once it has been found again — what Enter reads, what an action key
/// names, and what the hint bar counts as selected.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Target<'a> {
    Line(&'a pv::Line),
    Row(&'a pv::Row),
    Markdown(&'a str),
    Finding(&'a pv::Finding),
}

impl Target<'_> {
    /// The id an action's `item` carries; `""` for a markdown item, whose text is its name.
    pub(crate) fn id(&self) -> &str {
        match self {
            Target::Line(l) => l.id(),
            Target::Row(r) => r.id(),
            Target::Markdown(_) => "",
            Target::Finding(f) => f.id.as_str(),
        }
    }

    /// The words the detail's title is cut from.
    pub(crate) fn label(&self) -> String {
        match self {
            Target::Line(l) => clean_line(&l.label),
            Target::Row(r) => clean_line(r.cells.first().map(|c| c.text.as_str()).unwrap_or("")),
            Target::Markdown(text) => clean_line(first_line(text)),
            Target::Finding(f) => {
                let what = clean_line(&f.what);
                if what.is_empty() {
                    clean_line(&f.id)
                } else {
                    what
                }
            }
        }
    }

    /// The keys the hint bar shows for it.
    pub(crate) fn actions(&self) -> &[pv::Action] {
        match self {
            Target::Line(l) => &l.actions,
            Target::Row(r) => &r.actions,
            Target::Markdown(_) => &[],
            Target::Finding(f) => &f.actions,
        }
    }

    /// The markdown Enter reads, if the row has any.
    pub(crate) fn detail(&self) -> &str {
        match self {
            Target::Line(l) => l.detail.as_deref().unwrap_or(""),
            Target::Row(r) => r.detail.as_deref().unwrap_or(""),
            Target::Markdown(text) => text,
            Target::Finding(f) => &f.detail,
        }
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// **One step of the walk over a screen**: a panel's title, a block's own header, or an item.
enum Walk<'a> {
    Panel { index: usize, panel: &'a pv::Panel },
    Block { block: &'a pv::Block },
    Item(Item<'a>),
}

/// **The ONE walk over a screen** — the drawing and the stops both read it, so a row the
/// cursor can land on is always a row that was drawn.
fn each<'a>(view: &'a pv::View, screen: Screen, f: &mut impl FnMut(Walk<'a>)) {
    match screen {
        Screen::Overview => {
            for (i, p) in view.panels.iter().enumerate() {
                f(Walk::Panel { index: i, panel: p });
                // **An unavailable panel draws why and how to fix it, and none of its
                // blocks**: its summary is a drawing of data it could not get, and drawing
                // that would be drawing an empty result as though it were an answer.
                if p.unavailable.is_some() {
                    continue;
                }
                blocks(i, &p.summary, f);
            }
        }
        Screen::Panel(i) => {
            let Some(p) = view.panels.get(i) else { return };
            f(Walk::Panel { index: i, panel: p });
            blocks(i, p.shown(), f);
        }
        Screen::Findings => {
            for (index, finding) in view.findings.iter().enumerate() {
                f(Walk::Item(Item::Finding { index, finding }));
            }
        }
        // A legend's rows are documentation, not things to act on: nothing there is a stop.
        Screen::Legend => {}
    }
}

fn blocks<'a>(panel: usize, list: &'a [pv::Block], f: &mut impl FnMut(Walk<'a>)) {
    for (block, b) in list.iter().enumerate() {
        f(Walk::Block { block: b });
        match b {
            pv::Block::Lines(lines) => {
                for (item, line) in lines.iter().enumerate() {
                    f(Walk::Item(Item::Line {
                        panel,
                        block,
                        item,
                        line,
                    }));
                }
            }
            pv::Block::Table(t) => {
                for (item, row) in t.rows.iter().enumerate() {
                    f(Walk::Item(Item::Row {
                        panel,
                        block,
                        item,
                        row,
                    }));
                }
            }
            // **A tree's rows are stops of the walk and rows of one list**: the walk is what the
            // arrows, Enter and a click read, and the drawing hands the whole stand to rano's
            // list at once (`tree_lines`), so the depth it needs is `rows_deep`'s and not this
            // item's.
            pv::Block::Tree(t) => {
                for (item, (_, row)) in pv::rows_deep(&t.rows).into_iter().enumerate() {
                    f(Walk::Item(Item::Row {
                        panel,
                        block,
                        item,
                        row,
                    }));
                }
            }
            pv::Block::Kv(rows) => {
                for (label, value) in rows {
                    f(Walk::Item(Item::Kv { label, value }));
                }
            }
            pv::Block::Markdown(text) => {
                for (item, (text, heading)) in markdown_items(text).into_iter().enumerate() {
                    f(Walk::Item(Item::Markdown {
                        panel,
                        block,
                        item,
                        text,
                        heading,
                    }));
                }
            }
            // No items: the kind is drawn by `Walk::Block`, as one muted line naming it.
            pv::Block::Unknown(_) => {}
        }
    }
}

/// **A markdown block itemized**: a heading or a list item begins an item, and the lines
/// under it belong to it. C3 makes each of them a stop, which is what lets ↑↓ walk a pane's
/// prose the way the transcript's markers walk a reply.
fn markdown_items(text: &str) -> Vec<(&str, bool)> {
    let text = text.trim_matches('\n');
    if text.trim().is_empty() {
        return Vec::new();
    }
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0usize;
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        let begins = t.starts_with('#')
            || t.starts_with("- ")
            || t.starts_with("* ")
            || t.starts_with("+ ")
            || t.split_once(". ")
                .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if begins || starts.is_empty() {
            starts.push(at);
        }
        at += line.len();
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, start) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(text.len());
        let item = text[*start..end].trim_end_matches('\n');
        if item.trim().is_empty() {
            continue;
        }
        out.push((item, item.trim_start().starts_with('#')));
    }
    out
}

/// **The stops of a screen, in the order the cursor walks them.**
pub(crate) fn stops(view: &pv::View, screen: Screen) -> Vec<Stop> {
    let mut out = Vec::new();
    each(view, screen, &mut |w| {
        if let Walk::Item(it) = w
            && let Some(s) = it.stop()
        {
            out.push(s);
        }
    });
    out
}

/// **The stop at `index`, found again by position.** `None` for an index the view no longer
/// has — the view is replaced wholesale on every look, and the cursor can outlive a row.
pub(crate) fn target_of(view: &pv::View, screen: Screen, index: usize) -> Option<Target<'_>> {
    let stop = *stops(view, screen).get(index)?;
    Some(match stop {
        Stop::Finding(i) => Target::Finding(view.findings.get(i)?),
        Stop::Item { panel, block, item } => {
            let p = view.panels.get(panel)?;
            let list = match screen {
                Screen::Panel(_) => p.shown(),
                _ => &p.summary,
            };
            match list.get(block)? {
                pv::Block::Lines(l) => Target::Line(l.get(item)?),
                pv::Block::Table(t) => Target::Row(t.rows.get(item)?),
                pv::Block::Tree(t) => Target::Row(pv::rows_deep(&t.rows).get(item)?.1),
                pv::Block::Markdown(text) => Target::Markdown(markdown_items(text).get(item)?.0),
                pv::Block::Kv(_) | pv::Block::Unknown(_) => return None,
            }
        }
    })
}

/// **The palette's role, as rano's** — C3's six names are these six, and a role the host does
/// not know is drawn plain rather than guessed at.
pub(crate) fn role(r: Option<pv::Role>) -> Role {
    match r {
        Some(pv::Role::Muted) => Role::Faint,
        Some(pv::Role::Working) => Role::Pending,
        Some(pv::Role::Attention) => Role::Attention,
        Some(pv::Role::Success) => Role::Success,
        Some(pv::Role::Failure) => Role::Failure,
        // **The three words a list needed and a dashboard did not**: `strong` is *where I am*,
        // `faint` is *a fact rather than good news*, `pending` is *a turn is running*. They map
        // to the roles this head already draws those things in.
        Some(pv::Role::Strong) => Role::Strong,
        Some(pv::Role::Faint) => Role::Faint,
        Some(pv::Role::Pending) => Role::Pending,
        Some(pv::Role::Plain) | None => Role::Plain,
    }
}

/// **The cell of a C3 row, as rano's list draws it** — a cell that said nothing takes the row's
/// role, which is the point of the untagged spelling: only a cell that means something else carries
/// its own.
fn tree_cell(c: &pv::Cell) -> Cell {
    match c.role {
        Some(x) => Cell::role(clean_line(&c.text), role(Some(x))),
        None => Cell::new(clean_line(&c.text)),
    }
}

/// **C3's `Row` as rano's one tree list draws it** — the plain function this port exists for:
/// no drawing code, no terminal, just the row's own facts.
///
/// **`detail` is deliberately NOT rano's `Row::detail`.** C3's `detail` is prose *read whole on
/// Enter* (`docs/plugins-slice1.md` C3), where rano's is the faint line drawn **under** the row;
/// mapping one onto the other would draw every row's whole prose beneath it and double a pane's
/// height. So C3's `detail` stays where the keys already read it (the walk's own [`Target`]) and
/// the tree draws the row alone.
///
/// **`under` IS rano's `detail`**, and that is not the same field under another name: C3's `under`
/// is defined as *the faint line(s) UNDER the row* — a reason, an address, a legend sentence —
/// which is exactly what the list draws. A row that carries neither draws one line, as before.
///
/// `open` is `None` because the list reserves no fold column here (see [`tree_lines`]).
fn tree_row(depth: usize, r: &pv::Row) -> TreeRow {
    TreeRow {
        // **Stop-ness is C3's `id`, and an empty one is not a stop** — the same field
        // `Item::stop` reads, so a row the list draws and a row the cursor walks to are one
        // row.
        id: Some(r.id().to_string()).filter(|id| !id.is_empty()),
        depth,
        number: None,
        open: None,
        role: Some(role(r.role)),
        cells: r.cells.iter().map(tree_cell).collect(),
        // **The right-hand column**, which C3 calls `facts` and the list calls the same — the
        // age, the owner, the one verb that would move the row. Dropping them (as this function
        // did) is a pane whose rows say a third of what the plugin answered.
        facts: r.facts.iter().map(tree_cell).collect(),
        detail: r.under.iter().map(|u| clean_line(u)).collect(),
        detail_column: None,
        actions: r
            .actions
            .iter()
            .map(|a| TreeAction::new(clean_line(&a.key), clean_line(&a.label)))
            .collect(),
        children: Vec::new(),
    }
}

/// **One `Tree` block's rows, drawn by rano's list** — in the order the pane's walk numbers them,
/// and where each row's first line landed.
///
/// `depth_first` is the block's rows as [`pv::rows_deep`] flattens them — the pane's OWN walk,
/// which is what the arrows, Enter and a click read — so the drawing and the keys cannot come to
/// different conclusions about how many rows there are or which one is third. Rano's list draws
/// a flat list happily: "a caller that already holds a flat list sets `Row::depth` itself and
/// leaves `children` empty", which is this caller exactly.
///
/// `cursor` is where THIS pane's cursor sits among the tree's own stops, or `None` when it is on
/// another block. **`None` is rano's *nothing is selected***: the rows keep their ids — they are
/// still stops, and the pane's click map is written from the same ids — and the list draws no
/// mark, no inverse row and no row's actions in its hint. The version before this one handed the
/// list its rows with the ids CLEARED, which suppressed the mark by throwing away the stop-ness
/// the host's clicks read; the primitive grew the state so the host would not have to.
///
/// **A row may draw more than one line**, because C3's `under` is a list and each entry is a faint
/// line beneath the row. So the answer is the lines AND `starts`, the index of each row's first
/// line — the caller splices row `i`'s stop-ness and legend entry onto `starts[i]`, which is the
/// only place that knows it. (This used to be `drawn[i]`, exact only while every row was one line,
/// which is why `under` could not be drawn at all.)
///
/// One row is drawn per entry of `depth_first`, in order; the list is given no title and no frame
/// lines, so `starts` is the whole of the mapping.
fn tree_lines(
    depth_first: &[(usize, &pv::Row)],
    cursor: Option<usize>,
    w: usize,
) -> (PaneLines, Vec<usize>) {
    let rows: Vec<TreeRow> = depth_first
        .iter()
        .map(|&(depth, r)| tree_row(depth, r))
        .collect();
    // **Where each row's first line will land**, counted before the list draws: a row takes one
    // line for itself plus one for each `under` entry. The list appends them in this order and
    // draws nothing of its own (no title, no hint, no notes), so the count is the answer.
    let mut starts = Vec::with_capacity(rows.len());
    let mut at = 0usize;
    for r in &rows {
        starts.push(at);
        at += 1 + r.detail.len();
    }
    let list = TreeList {
        // The pane draws the panel's title itself; the list is the rows.
        title: String::new(),
        rows,
        selected: cursor,
        empty: String::new(),
        // No fold column: see [`tree_row`].
        fold_column: false,
        detail_column: 0,
        // The margin and the frame lines (empty state, hint, notes) are this pane's, and the
        // list draws none it was not given.
        margin: 0,
        hint: Vec::new(),
        notes: Vec::new(),
    };
    let mut content = list.content(w);
    // **The rows, and only the rows.** The list also draws its own hint under them — the
    // selected row's action letters, which is what a pane that IS a list wants and a block in
    // the middle of a dashboard does not: a board of three panels would carry three hints. This
    // pane's keys are the head's, said once in the bottom bar (`App::plugin_actions_hint`), and
    // the rows themselves the list owns.
    content.lines.truncate(at);
    (content, starts)
}

/// The severity's word and role, for the findings list and the summary line.
pub(crate) fn severity(s: pv::Severity) -> (&'static str, Role) {
    match s {
        pv::Severity::Tripped => ("tripped", Role::Failure),
        pv::Severity::Missing => ("missing", Role::Attention),
        pv::Severity::Passed => ("passed", Role::Success),
    }
}

/// **The pane's counter for the status row** — `(words, role)`, or nothing.
pub(crate) fn badge(view: &pv::View) -> Option<(String, Role)> {
    let b = view.badge.as_ref()?;
    let text = clean_line(&b.text);
    (!text.is_empty()).then(|| (text, role(b.role)))
}

/// **The columns a screen's rows share**, measured over every line on it. See the module note:
/// one grid per screen, not one per panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Grid {
    pub(crate) label: usize,
    pub(crate) value: usize,
    pub(crate) glyph: usize,
    pub(crate) tail: usize,
}

impl Grid {
    /// How many columns a grid's rows use.
    fn used(&self) -> usize {
        LEAD + self.label
            + self.value
            + self.glyph
            + self.tail
            + usize::from(self.value > 0)
            + usize::from(self.glyph > 0)
            + usize::from(self.tail > 0)
    }

    /// **The grid the width can hold**, giving up in the order a reader would: the tail first
    /// (it is a note beside the number), then the glyph column (a bar is a second reading of
    /// the value beside it), then the value, and the label last — a row with a value and no
    /// words is not a row anybody can read.
    fn fit(w: usize, label: usize, value: usize, glyph: usize, tail: usize) -> Grid {
        let mut g = Grid {
            label,
            value,
            glyph,
            tail,
        };
        while g.used() > w && g.tail > 0 {
            g.tail -= 1;
        }
        while g.used() > w && g.glyph > 0 {
            g.glyph -= 1;
        }
        while g.used() > w && g.value > 0 {
            g.value -= 1;
        }
        while g.used() > w && g.label > 0 {
            g.label -= 1;
        }
        g
    }
}

/// **The glyph a line draws** — a bar of its `frac`, or a sparkline of its history, in the
/// line's own role. A bar and a spark are the same reading twice over, so a line carrying
/// both shows the bar.
fn glyph(l: &pv::Line) -> String {
    if let Some(bar) = &l.bar {
        let filled = (bar.frac.clamp(0.0, 1.0) * BAR as f64).round() as usize;
        let filled = filled.min(BAR);
        return format!("{}{}", "█".repeat(filled), "░".repeat(BAR - filled));
    }
    let Some(spark) = &l.spark else {
        return String::new();
    };
    spark
        .iter()
        .map(|v| {
            let at = (v.clamp(0.0, 1.0) * (STEPS.len() - 1) as f64).round() as usize;
            STEPS[at.min(STEPS.len() - 1)]
        })
        .collect()
}

/// **Cut `s` to `max` columns, with a `…` when anything went** — the pane's rule for every
/// column: a truncated label says it was truncated rather than running into the next column.
/// `trim_to` is rano's truncate, which adds the `…` itself and counts it in `max`.
pub(crate) fn cut(s: &str, max: usize) -> String {
    if visible_width(s) <= max {
        return s.to_string();
    }
    trim_to(s, max)
}

fn pad_to(s: &str, w: usize) -> String {
    let pad = w.saturating_sub(visible_width(s));
    format!("{s}{}", " ".repeat(pad))
}

fn pad_left(s: &str, w: usize) -> String {
    let pad = w.saturating_sub(visible_width(s));
    format!("{}{s}", " ".repeat(pad))
}

/// **The grid, measured over every `Line` the screen draws.** A second walk of a frame, and a
/// cheap one: a view is tens of rows, and the alternative is a per-panel ruler.
pub(crate) fn grid(view: &pv::View, screen: Screen, w: usize) -> Grid {
    let (mut label, mut value, mut glyph_w, mut tail) = (0, 0, 0, 0);
    each(view, screen, &mut |walk| {
        if let Walk::Item(Item::Line { line, .. }) = walk {
            label = label.max(visible_width(&clean_line(&line.label)));
            value = value.max(
                line.value
                    .as_deref()
                    .map_or(0, |v| visible_width(&clean_line(v))),
            );
            glyph_w = glyph_w.max(visible_width(&glyph(line)));
            tail = tail.max(
                line.tail
                    .as_deref()
                    .map_or(0, |t| visible_width(&clean_line(t))),
            );
        }
    });
    // No column is more than a third of what is left after the label's own minimum: a value
    // column measured from one enormous number would squeeze every label to nothing.
    let room = w.saturating_sub(LEAD + 8);
    Grid::fit(
        w,
        label,
        value.min(room / 3),
        glyph_w.min(room / 3),
        tail.min(room / 3),
    )
}

/// **One `Line`, laid out in the grid** — the layout function every line of every panel goes
/// through: `label | value | glyph | tail`, left to right, cut to the pane.
pub(crate) fn line_row(l: &pv::Line, g: &Grid, lead: &str) -> Line {
    let r = role(l.role);
    let mut out = Line::new(vec![Span::role(lead.to_string(), Role::Faint)]);
    out.push(Span::role(
        pad_to(&cut(&clean_line(&l.label), g.label), g.label),
        r,
    ));
    if g.value > 0 {
        let v = cut(&clean_line(l.value.as_deref().unwrap_or("")), g.value);
        out.push(Span::role(format!(" {}", pad_left(&v, g.value)), r));
    }
    if g.glyph > 0 {
        let mut glyph = glyph(l);
        // **Cut from the left**: a spark's newest sample is its right end, and a history that
        // lost its newest is a history about the past alone.
        if visible_width(&glyph) > g.glyph {
            let skip = visible_width(&glyph) - g.glyph;
            glyph = glyph.chars().skip(skip).collect();
        }
        out.push(Span::role(
            format!(" {}", pad_to(&glyph, g.glyph)),
            if r == Role::Plain { Role::Faint } else { r },
        ));
    }
    if g.tail > 0 {
        let tail = cut(&clean_line(l.tail.as_deref().unwrap_or("")), g.tail);
        out.push(Span::role(format!(" {tail}"), Role::Faint));
    }
    out
}

/// **A panel's title row**: its key in the key role — the key is what opens it — its name
/// strong, and its denominator dim beside it, because a bar with no denominator is a number
/// with no meaning.
fn panel_title(p: &pv::Panel) -> Line {
    let mut out = Line::new(vec![Span::role(cut(&clean_line(&p.key), 1), Role::Key)]);
    let name = if p.title.is_empty() {
        clean_line(&p.id)
    } else {
        clean_line(&p.title)
    };
    out.push(Span::role(format!(" {name}"), Role::Strong));
    if let Some(d) = &p.denominator
        && !d.is_empty()
    {
        out.push(Span::role(format!("  {}", clean_line(d)), Role::Faint));
    }
    out
}

/// **The line under the title that counts the findings by severity** (C3) — the overview's
/// one-line answer to *how bad is it*, in the register each count belongs to.
fn summary_line(view: &pv::View) -> Option<Line> {
    let (tripped, missing, passed) = view.counted();
    if tripped + missing + passed == 0 {
        return None;
    }
    let mut out = Line::default();
    for (n, word, r) in [
        (tripped, "tripped", Role::Failure),
        (missing, "missing", Role::Attention),
        (passed, "passed", Role::Success),
    ] {
        if out.width() > 0 {
            out.push(Span::role(" · ", Role::Faint));
        }
        out.push(Span::role(format!("{n} {word}"), r));
    }
    Some(out)
}

/// **What a screen's drawing produced**: its rows, where each stop landed, and where each
/// panel's title landed — the three records the frame makes, so the arrows, a click on a stop
/// and a click on a panel's title are all measured against what was drawn.
pub(crate) struct Content {
    pub(crate) lines: PaneLines,
    /// `(pane row, panel index)` for every panel title drawn.
    pub(crate) panels: Vec<(usize, usize)>,
    /// **`(pane row, legend key)`** for every line or row whose label the view's legend
    /// documents — the record a hover reads. C3 gives a legend two jobs, *the `l` view and the
    /// hover text*, and this is the second one's: written where the row is drawn, because only
    /// the draw knows which row the pointer's row became.
    pub(crate) legend: Vec<(usize, String)>,
}

/// **The rows of a screen**, and where each stop landed — what the pane draws, the arrows
/// scroll by, and a click is tested against.
pub(crate) fn content(view: &pv::View, screen: Screen, sel: usize, w: usize) -> Content {
    let g = grid(view, screen, w);
    let mut out = PaneLines::new();
    let mut panels: Vec<(usize, usize)> = Vec::new();
    let mut legend: Vec<(usize, String)> = Vec::new();
    // The screen's own head, above the walk.
    match screen {
        Screen::Overview => {
            let title = clean_line(if view.title.is_empty() {
                "a plugin's pane"
            } else {
                &view.title
            });
            out.push(pane::title(cut(&title, w)));
            if let Some(s) = summary_line(view) {
                out.push(s);
            }
        }
        Screen::Panel(i) => {
            let title = view
                .panels
                .get(i)
                .map(|p| {
                    let t = clean_line(&p.title);
                    if t.is_empty() { clean_line(&p.id) } else { t }
                })
                .unwrap_or_default();
            out.push(pane::title(cut(&title, w)));
        }
        Screen::Findings => out.push(pane::title("findings")),
        Screen::Legend => out.push(pane::title("legend")),
    }
    out.blank();
    let before = out.lines.len();

    let mut next = 0usize;
    // **Whether the items now arriving belong to a tree block**, whose rows their block already
    // drew as one list. Set on every block, so an item always knows which block it is under.
    let mut in_tree = false;
    let selected = |it: &Item<'_>, next: &mut usize| -> bool {
        if it.stop().is_some() {
            let on = *next == sel;
            *next += 1;
            on
        } else {
            false
        }
    };
    each(view, screen, &mut |walk| match walk {
        Walk::Panel { index, panel } => {
            if out.lines.len() > before {
                out.blank();
            }
            panels.push((out.lines.len(), index));
            out.push(panel_title(panel));
            if let Some(u) = &panel.unavailable {
                if !u.why.is_empty() {
                    out.push(Line::new(vec![
                        Span::raw("   "),
                        Span::role(clean_line(&u.why), Role::Attention),
                    ]));
                }
                if !u.fix.is_empty() {
                    out.push(Line::new(vec![
                        Span::role("   fix ", Role::Faint),
                        Span::raw(clean_line(&u.fix)),
                    ]));
                }
            }
        }
        // **A block's own header**: a table's column names, a tree's whole stand of rows, and
        // the kind of a block this host does not draw — one muted line naming it, which is
        // C3's rule.
        Walk::Block { block } => {
            // **The tree is drawn HERE, as one list**, and its rows are skipped below: rano's
            // list draws a mark per row from one `selected`, so it has to see the whole stand at
            // once, and the pane's stop counter is advanced here instead of once per item below.
            in_tree = matches!(block, pv::Block::Tree(_));
            match block {
                pv::Block::Table(t) if !t.columns.is_empty() => {
                    let cells: Vec<String> = t.columns.iter().map(|c| clean_line(c)).collect();
                    out.push(pane::faint(format!(
                        "   {}",
                        cut(&cells.join("  "), w.saturating_sub(4))
                    )));
                }
                // **A plugin's tree, drawn by rano's one list** ([`tree_lines`]). Where its
                // stops sit in the pane's own numbering is what tells the list whether the
                // cursor is in this block at all: `next` is the first stop this tree contributes
                // and `sel` is the pane's cursor.
                pv::Block::Tree(t) => {
                    let deep = pv::rows_deep(&t.rows);
                    let stops_n = deep.iter().filter(|(_, r)| !r.id().is_empty()).count();
                    let here = (sel >= next && sel < next + stops_n).then(|| sel - next);
                    next += stops_n;
                    let (drawn, starts) = tree_lines(&deep, here, w);
                    // **One drawn line per row's FIRST line, and `starts[i]` says which** — a
                    // row may carry faint lines under it (C3's `under`), so the row's index and
                    // its first line's index are not the same number, and the mapping is the
                    // drawing's rather than arithmetic over the primitive's internals.
                    let drawn_lines = drawn.lines;
                    let mut next_row = 0usize;
                    for (i, line) in drawn_lines.into_iter().enumerate() {
                        // Which row this line belongs to: the last whose `starts` is at or before
                        // it. `starts` is ascending and one row per entry, so a cursor works.
                        while next_row + 1 < starts.len() && starts[next_row + 1] <= i {
                            next_row += 1;
                        }
                        let row = deep[next_row].1;
                        let first = starts.get(next_row).copied() == Some(i);
                        if !first || row.id().is_empty() {
                            out.push(line);
                        } else {
                            out.push_stop(line);
                        }
                        // A row whose first cell the legend documents answers on hover, exactly
                        // as a line does: the record is written where the row is drawn.
                        if first
                            && !view.legend.is_empty()
                            && let Some(label) = row.cells.first().map(|c| clean_line(&c.text))
                            && view.legend.contains_key(&label)
                        {
                            legend.push((out.lines.len() - 1, label));
                        }
                    }
                }
                pv::Block::Unknown(kind) => {
                    let kind = if kind.is_empty() {
                        "unknown"
                    } else {
                        kind.as_str()
                    };
                    out.push(pane::faint(format!(
                        "   {kind} — this head does not draw that block kind"
                    )));
                }
                _ => {}
            }
        }
        Walk::Item(it) => {
            // A tree's rows were drawn with their block, above, and counted there.
            if in_tree {
                return;
            }
            let on = selected(&it, &mut next);
            let lead = if it.stop().is_some() {
                format!("{} ", pane::mark(on))
            } else {
                "  ".to_string()
            };
            let mut rows: Vec<Line> = Vec::new();
            let first = match it {
                Item::Line { line, .. } => line_row(line, &g, &lead),
                // **A table's row**, which is the pane's only flat row: a tree's rows go through
                // rano's list with their block, above.
                Item::Row { row, .. } => {
                    let cells: Vec<String> =
                        row.cells.iter().map(|c| clean_line(&c.text)).collect();
                    Line::new(vec![
                        Span::role(lead, Role::Faint),
                        Span::role(
                            cut(&cells.join("  "), w.saturating_sub(LEAD)),
                            role(row.role),
                        ),
                    ])
                }
                Item::Kv { label, value } => Line::new(vec![
                    Span::role(lead, Role::Faint),
                    Span::role(
                        pad_to(&cut(&clean_line(label), g.label), g.label),
                        Role::Plain,
                    ),
                    Span::raw(" "),
                    Span::raw(cut(
                        &clean_line(value),
                        w.saturating_sub(g.label + LEAD + 1),
                    )),
                ]),
                Item::Markdown { text, heading, .. } => {
                    let body = if heading {
                        text.trim_start().trim_start_matches('#').trim_start()
                    } else {
                        text
                    };
                    let r = if heading { Role::Heading } else { Role::Plain };
                    let body = clean(body);
                    let mut lines = wrapped(&body, w.saturating_sub(LEAD), r);
                    // The rest of a wrapped item is drawn with it and is no stop: the cursor
                    // rests on the item, not on the screen row.
                    let first = if lines.is_empty() {
                        Line::default()
                    } else {
                        lines.remove(0)
                    };
                    rows.extend(lines);
                    let mut row = Line::new(vec![Span::role(lead, Role::Faint)]);
                    row.spans.extend(first.spans);
                    row
                }
                Item::Finding { finding, .. } => {
                    let (word, r) = severity(finding.severity);
                    Line::new(vec![
                        Span::role(lead, Role::Faint),
                        Span::role(format!("{word:<8}"), r),
                        Span::raw(cut(&clean_line(&finding.what), w.saturating_sub(LEAD + 9))),
                    ])
                }
            };
            out.push_stop(first);
            // **A label the legend documents is a row the pointer can explain.** The key is the
            // label as DRAWN (`clean_line`, the same reading the drawing measured), so a legend
            // entry and the row it documents cannot disagree about what a label says.
            if !view.legend.is_empty()
                && let Some(label) = match &it {
                    Item::Line { line, .. } => Some(clean_line(&line.label)),
                    Item::Row { row, .. } => row.cells.first().map(|c| clean_line(&c.text)),
                    _ => None,
                }
                && view.legend.contains_key(&label)
            {
                legend.push((out.lines.len() - 1, label));
            }
            for r in rows {
                out.push(r);
            }
        }
    });

    if screen == Screen::Findings && view.findings.is_empty() {
        out.push(pane::faint("  none — nothing tripped, nothing missing"));
    }
    if screen == Screen::Legend {
        legend_rows(view, w, &mut out);
    }
    if out.lines.len() == before {
        out.push(pane::faint("  this pane has nothing to draw yet"));
    }
    Content {
        lines: out,
        panels,
        legend,
    }
}

/// A legend's `label  meaning` rows: the documentation of every label in the pane.
fn legend_rows(view: &pv::View, w: usize, out: &mut PaneLines) {
    if view.legend.is_empty() {
        out.push(pane::faint("  no legend — this pane documents no labels"));
        return;
    }
    let label = view
        .legend
        .keys()
        .map(|k| visible_width(k))
        .max()
        .unwrap_or(0)
        .min(w / 3);
    for (k, v) in &view.legend {
        out.push(Line::new(vec![
            Span::role(pad_to(&cut(&clean_line(k), label), label), Role::Strong),
            Span::raw("  "),
            Span::raw(cut(&clean_line(v), w.saturating_sub(label + 2))),
        ]));
    }
}

/// **One stop read whole** — what Enter opens (C3): the row's own words, and for a finding the
/// numbers behind it, what to do, and how to check it. `screen` is the screen the stop was
/// selected on: a detail is a surface of its own, and the row it reads is the one under it.
pub(crate) fn detail(view: &pv::View, screen: Screen, index: usize, w: usize) -> PaneLines {
    let mut out = PaneLines::new();
    let target = target_of(view, screen, index);
    out.push(pane::title(cut(
        &target.as_ref().map(|t| t.label()).unwrap_or_default(),
        w,
    )));
    out.blank();
    let Some(target) = target else {
        out.push(pane::faint("  that row is gone"));
        return out;
    };
    let detail = target.detail();
    for l in wrapped(&clean(detail), w.saturating_sub(2), Role::Plain) {
        let mut row = Line::new(vec![Span::raw("  ")]);
        row.spans.extend(l.spans);
        out.push(row);
    }
    if let Target::Finding(f) = target {
        if !f.evidence.is_empty() {
            out.blank();
            out.push(pane::faint("  evidence"));
            let label = f
                .evidence
                .keys()
                .map(|k| visible_width(k))
                .max()
                .unwrap_or(0)
                .min(w / 3);
            for (k, v) in &f.evidence {
                out.push(Line::new(vec![
                    Span::raw("    "),
                    Span::role(pad_to(&clean_line(k), label), Role::Strong),
                    Span::raw("  "),
                    Span::raw(cut(&evidence(v), w.saturating_sub(label + 6))),
                ]));
            }
        }
        for (head, text) in [("fix", &f.fix), ("verify", &f.verify)] {
            if text.trim().is_empty() {
                continue;
            }
            out.blank();
            out.push(pane::faint(format!("  {head}")));
            for l in wrapped(&clean(text), w.saturating_sub(4), Role::Plain) {
                let mut row = Line::new(vec![Span::raw("    ")]);
                row.spans.extend(l.spans);
                out.push(row);
            }
        }
        if !f.panel.is_empty() {
            out.blank();
            out.push(pane::faint(format!("  panel {}", clean_line(&f.panel))));
        }
    }
    if out.lines.len() == 2 {
        out.push(pane::faint("  this row has nothing more to say"));
    }
    out
}

/// One evidence value as one line: a string bare, everything else as its JSON.
fn evidence(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => clean_line(s),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> pv::View {
        serde_json::from_str(
            r#"{"title":"a dashboard","findings":[{"id":"f","what":"w","severity":"tripped"}],
                "panels":[{"id":"p","key":"p","title":"panel","summary":[
                    {"lines":[{"label":"one","value":"1/2","id":"a"}]}]}]}"#,
        )
        .unwrap()
    }

    /// **A markdown block is itemized into its headings and its list items**, with the lines
    /// under one belonging to it — which is what makes them stops.
    #[test]
    fn markdown_items_are_headings_and_list_items() {
        let items = markdown_items("# head\nprose under it\n- one\n  more\n- two\n");
        assert_eq!(items.len(), 3, "{items:?}");
        assert!(items[0].1, "a heading is a heading");
        assert!(items[0].0.starts_with("# head"), "{:?}", items[0].0);
        assert!(items[1].0.starts_with("- one"), "{:?}", items[1].0);
        assert!(items[1].0.contains("more"), "continuation belongs to it");
        assert_eq!(markdown_items(""), Vec::new());
    }

    /// **One ruler for the screen**: the widest label of any panel sets the label column, so
    /// two panels' values are drawn in the same place.
    #[test]
    fn the_grid_measures_every_line_of_the_screen() {
        let v: pv::View = serde_json::from_str(
            r#"{"panels":[
                {"id":"a","key":"a","title":"a","summary":[{"lines":[{"label":"x","value":"14/22"}]}]},
                {"id":"b","key":"b","title":"b","summary":[{"lines":[{"label":"a longer label","value":"3"}]}]}]}"#,
        )
        .unwrap();
        let g = grid(&v, Screen::Overview, 100);
        assert!(
            g.label >= visible_width("a longer label"),
            "the widest label sets the column: {g:?}"
        );
        let rows = content(&v, Screen::Overview, 0, 100);
        let widths: Vec<usize> = rows.lines.lines.iter().map(|l| l.width()).collect();
        assert!(widths.iter().all(|w| *w <= 100), "{widths:?}");
        assert_eq!(rows.panels.len(), 2, "both panel titles are recorded");
    }

    /// A column too narrow for its words says they were cut rather than running on.
    #[test]
    fn a_label_is_cut_with_an_ellipsis() {
        assert_eq!(cut("abcdef", 3), "ab…");
        assert_eq!(cut("abcdef", 10), "abcdef");
        assert_eq!(cut("abcdef", 0), "");
    }

    /// **The stops a screen has**: a line with an id, and a finding; a line with no id is a
    /// number and the cursor walks past it.
    #[test]
    fn only_rows_with_an_id_are_stops() {
        let v = view();
        let s = stops(&v, Screen::Overview);
        assert_eq!(s.len(), 1, "{s:?}");
        assert!(matches!(
            target_of(&v, Screen::Overview, 0),
            Some(Target::Line(_))
        ));
        assert_eq!(stops(&v, Screen::Findings).len(), 1);
        assert_eq!(stops(&v, Screen::Legend).len(), 0);
    }

    /// **An unknown block kind is one muted line naming it**, inside the panel it belongs to.
    #[test]
    fn an_unknown_block_names_its_kind() {
        let v: pv::View = serde_json::from_str(
            r#"{"panels":[{"id":"p","key":"p","title":"p","summary":[{"progress":{"done":1}}]}]}"#,
        )
        .unwrap();
        let rows = content(&v, Screen::Overview, 0, 80);
        let plain: Vec<String> = rows.lines.lines.iter().map(|l| l.plain()).collect();
        assert!(plain.iter().any(|l| l.contains("progress")), "{plain:?}");
    }

    /// **The columns are one grid even when the rows are not**: two lines' values start in the
    /// same column, with or without a bar — which is the whole reason the grid is measured once
    /// per screen and not per row.
    #[test]
    fn a_bar_and_a_bare_value_share_the_glyph_column() {
        let g = Grid::fit(100, 6, 5, BAR, 8);
        let with: pv::Line =
            serde_json::from_str(r#"{"label":"a","value":"1/2","bar":{"frac":0.5},"tail":"half"}"#)
                .unwrap();
        let without: pv::Line = serde_json::from_str(r#"{"label":"b","value":"1/2"}"#).unwrap();
        let a = line_row(&with, &g, "  ");
        let b = line_row(&without, &g, "  ");
        assert_eq!(
            a.plain().find("1/2"),
            b.plain().find("1/2"),
            "one value column: {:?} vs {:?}",
            a.plain(),
            b.plain()
        );
        assert!(a.plain().contains('█'), "{}", a.plain());
        assert!(a.plain().ends_with("half"), "{}", a.plain());
    }
}
