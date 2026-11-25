//! The session listing as a person reads it: a tree under each folder, in rano's roles, with a
//! mark on the sessions a daemon holds live.
//!
//! Rows are built as [`Line`]s and painted once by [`paint`], so the plain output - a pipe,
//! `NO_COLOR`, a dumb terminal - is the same text with no escapes. Nothing here starts a daemon,
//! and nothing waits on one for longer than [`PROBE`].

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::IsTerminal;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::SessionBrief;
use letibot_sessionlog::wire::{FrameReader, FrameWriter, MAX_DAEMON_FRAME_BYTES};
use letibot_tokencore::store::StoredSession;
use rano::render::{Line, Span};
use rano::style::{Palette, Role};

use crate::cli::{age, group_row, grouped, is_conversation, is_operators, list_label};

/// The columns every row spends besides its name: two gaps, ROWS, AGE, and the gap before the id.
pub(crate) const ROW_FIXED_COLS: usize = 2 + 5 + 2 + 7 + 2;
/// What a tree branch spends before the name. Counted for the census too, which draws no branch:
/// one column rule for both lists is what keeps a title from being cut in one and not the other.
pub(crate) const TREE_COLS: usize = 3;
/// The narrowest name column that still says who a row is.
const MIN_NAME_COLS: usize = 24;

/// How many rows a folder draws before it counts the rest. Standing in a folder is asking about
/// it, so that one gets the longer list.
pub(crate) const HERE_SHOWN: usize = 20;
/// Another folder answers one question - is the conversation I want in there - and its newest
/// five rows do, with the header's count for the rest.
pub(crate) const FOLDER_SHOWN: usize = 5;
/// How many other folders a plain listing draws before it counts the rest.
pub(crate) const FOLDERS_SHOWN: usize = 8;

/// A branch that has more after it, and the one that ends a block.
const BRANCH: &str = "├─ ";
const LAST: &str = "└─ ";
/// The indent the footer's counts sit at: not a branch - they are about the whole list.
const NOTE: &str = "    ";

/// A conversation that moved less than a day ago: its age is drawn in the reader's own foreground
/// and an older one dim. Colour only - the text is the same either way.
const RECENT_MS: i64 = 24 * 60 * 60 * 1000;

/// How long one daemon has to answer the probe's two questions.
const PROBE: Duration = Duration::from_millis(400);
/// How long the listing waits for every daemon it asked. Each answers in its own thread, so this
/// is the whole cost of the marks on a box with ten daemons, not ten times it.
const MARKS_DEADLINE: Duration = Duration::from_millis(1200);

/// What a row's mark says about a session, which only a running daemon can answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Nothing is watching it and nothing is running in it. No glyph: most rows.
    Idle,
    /// A head is attached - a TUI, a bridge, an editor.
    Attached,
    /// A turn is generating right now. Drawn in preference to [`Mark::Attached`] when both are
    /// true: it is the fact that changes on its own.
    Running,
}

impl Mark {
    /// The glyph a row draws, or `""` where there is nothing to say.
    pub fn glyph(self) -> &'static str {
        match self {
            Mark::Idle => "",
            Mark::Attached => "\u{25cf}",
            Mark::Running => "\u{25d0}",
        }
    }

    /// The role the glyph is drawn in.
    fn role(self) -> Role {
        match self {
            Mark::Idle | Mark::Attached => Role::Success,
            Mark::Running => Role::Pending,
        }
    }

    /// What the legend calls it.
    fn legend(self) -> &'static str {
        match self {
            Mark::Idle => "",
            Mark::Attached => "a head is attached",
            Mark::Running => "a turn is running",
        }
    }
}

/// The mark one session's daemon implies: a turn first, then a head, and nothing for a session
/// held live with nobody watching it.
fn mark_of(heads: usize, running: bool) -> Mark {
    match (running, heads) {
        (true, _) => Mark::Running,
        (false, 0) => Mark::Idle,
        (false, _) => Mark::Attached,
    }
}

/// What every daemon on this box answered, by session id. Absent is [`Mark::Idle`], which is what
/// a listing drawn from the store alone shows.
#[derive(Debug, Default, Clone)]
pub struct Marks(HashMap<String, Mark>);

impl Marks {
    pub fn of(&self, id: &str) -> Mark {
        self.0.get(id).copied().unwrap_or(Mark::Idle)
    }

    fn put(&mut self, id: String, mark: Mark) {
        if mark != Mark::Idle {
            self.0.insert(id, mark);
        }
    }

    fn absorb(&mut self, other: Marks) {
        self.0.extend(other.0);
    }
}

/// Which glyphs a listing actually drew. A legend for a glyph nobody showed is a puzzle, and a
/// daemon for another folder can answer a mark no row of this list carries.
#[derive(Debug, Default, Clone, Copy)]
struct Shown {
    attached: bool,
    running: bool,
}

impl Shown {
    fn of(marks: impl Iterator<Item = Mark>) -> Shown {
        let mut out = Shown::default();
        for m in marks {
            match m {
                Mark::Attached => out.attached = true,
                Mark::Running => out.running = true,
                Mark::Idle => {}
            }
        }
        out
    }

    fn any(self) -> bool {
        self.attached || self.running
    }
}

// ---------------------------------------------------------------- the marks, asked of the daemons

/// What each session's daemon says about it, asked of every daemon running on this box once. The
/// one thing only a daemon knows is who is attached and what is turning, so that is the one thing
/// asked - two round trips per daemon, in parallel, and silence from any that does not answer in
/// [`PROBE`].
pub fn marks() -> Marks {
    marks_at(&daemons())
}

/// [`marks`] over the sockets a caller names, so the probe can be asked about a daemon a test
/// started rather than the ones this box happens to be running.
fn marks_at(sockets: &[PathBuf]) -> Marks {
    if sockets.is_empty() {
        return Marks::default();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    for socket in sockets {
        let tx = tx.clone();
        let socket = socket.clone();
        // A probe that outlives the deadline dies with the process: it owns nothing and writes
        // nothing, so there is nothing to join.
        let _ = std::thread::Builder::new()
            .name("listing-probe".into())
            .spawn(move || {
                if let Some(held) = held_at(&socket) {
                    let _ = tx.send(held);
                }
            });
    }
    drop(tx);
    let until = Instant::now() + MARKS_DEADLINE;
    let mut out = Marks::default();
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(held) => out.absorb(held),
            Err(_) => break,
        }
    }
    out
}

/// The sockets of this box's running daemons: the runtime records the launcher wrote beside them,
/// which is the same set `letibot_tools::lifecycle::daemon_records` reads for the reaper and
/// `letibot --ls` reads for its own list. Only the socket is wanted here, and a record whose
/// socket is not there is a daemon that is not running.
fn daemons() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(letibot_tools::lifecycle::rundir()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        // A room and a forward are records of their own kind, and neither serves a session.
        if v.get("room").is_some() || v.get("forward").is_some() {
            continue;
        }
        let Some(socket) = v.get("socket").and_then(|s| s.as_str()) else {
            continue;
        };
        let socket = PathBuf::from(socket);
        if socket.exists() {
            out.push(socket);
        }
    }
    out
}

/// One daemon's answer: what it holds live, and what is happening in each of those.
fn held_at(socket: &Path) -> Option<Marks> {
    let ids = held_ids(socket)?;
    let first = ids.first()?;
    let sessions = hello_sessions(socket, first)?;
    let mut out = Marks::default();
    for s in sessions {
        // A session this daemon holds on disk and nothing more is not one it can say anything
        // about: its status is zeroes, and a zero is not "idle", it is "not here".
        if !s.live {
            continue;
        }
        // The probe itself is one of the heads it counted, and it is not somebody watching: it is
        // attached for the length of this round trip and gone.
        let ours = usize::from(s.session_id == *first);
        out.put(
            s.session_id.clone(),
            mark_of(s.status.heads.saturating_sub(ours), s.status.running),
        );
    }
    Some(out)
}

/// Which sessions a daemon holds live. No frame lists them without a seat, but the refusal of an
/// ATTACH for an unknown id names what is held - so the refusal is the directory, and the id it
/// names is one this connection can then attach to. `crates/tui`'s `--list-sessions` probe reads
/// the same sentence for the same reason.
fn held_ids(socket: &Path) -> Option<Vec<String>> {
    let (mut writer, mut reader) = connect(socket).ok()?;
    // An id no daemon can hold: one that accepted it would be a daemon minting sessions on
    // attach, which none does.
    writer.write(&attach("\u{0}probe", 0)).ok()?;
    match reader.read::<ServerFrame>().ok()? {
        ServerFrame::Bye { reason } => Some(held_in(&reason)),
        _ => None,
    }
}

/// The ids a refusal names: `no such session "…"; it holds a, b, and 2 more`. The sentence is
/// `server::refusal_for`'s, and the count at the end is a count and not an id.
fn held_in(reason: &str) -> Vec<String> {
    let Some((_, list)) = reason.split_once("; it holds ") else {
        return Vec::new();
    };
    list.split(", ")
        .filter(|id| !(id.starts_with("and ") && id.ends_with(" more")))
        .map(str::to_string)
        .collect()
}

/// The sessions a daemon reports on its hello: every session in its store, with `live` saying
/// which of them it holds and the status saying what is happening in those.
fn hello_sessions(socket: &Path, session: &str) -> Option<Vec<SessionBrief>> {
    let (mut writer, mut reader) = connect(socket).ok()?;
    writer.write(&attach(session, u64::MAX)).ok()?;
    let sessions = match reader.read::<ServerFrame>().ok()? {
        ServerFrame::Hello { sessions, .. } => sessions,
        _ => return None,
    };
    // The seat this round trip took is given back before the frame it came for is even parsed
    // further: the daemon is not holding a head for a listing.
    let _ = writer.write(&ClientFrame::Detach);
    Some(sessions)
}

/// The frame that asks a daemon what it holds. `list` and not `remote`: a listing is a peer on
/// this connection, and a peer is refused every one of the operator's doors.
fn attach(session: &str, since_seq: u64) -> ClientFrame {
    ClientFrame::Attach {
        protocol_version: PROTOCOL_VERSION,
        session_id: session.to_string(),
        since_seq,
        kind: "list".to_string(),
        identity: "list".to_string(),
        caps: Caps::default(),
    }
}

/// A connected daemon with a deadline on both directions.
fn connect(socket: &Path) -> std::io::Result<(FrameWriter<UnixStream>, FrameReader<UnixStream>)> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(PROBE))?;
    stream.set_write_timeout(Some(PROBE))?;
    let reader = FrameReader::with_limit(stream.try_clone()?, MAX_DAEMON_FRAME_BYTES);
    Ok((FrameWriter::new(stream), reader))
}

// ------------------------------------------------------------------------------- how it is painted

/// The palette a listing paints with: none unless this is a person's terminal, and none when they
/// said `NO_COLOR` or their terminal is `dumb`.
pub fn palette() -> Palette {
    palette_for(std::io::stdout().is_terminal(), &|k| std::env::var_os(k))
}

/// [`palette`] with the two questions a test cannot ask of its own process: whether stdout is a
/// terminal, and what the environment says. Every answer a palette depends on goes through `get`,
/// `COLORTERM` included - a terminal reached through the process's own environment cannot be a
/// fixture.
fn palette_for(tty: bool, get: &dyn Fn(&str) -> Option<OsString>) -> Palette {
    if !tty {
        return Palette::None;
    }
    // `NO_COLOR` is a request, and an empty one is not a request: the convention is a variable
    // set to something, which is what `NO_COLOR= letibot …` must not turn off.
    if get("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return Palette::None;
    }
    if get("TERM").is_some_and(|t| t == "dumb") {
        return Palette::None;
    }
    // Not `detected`'s light half: that answer is the terminal's own, and it is an OSC 11 query
    // away. A listing may not spend a round trip on it, and the sixteen slots are chosen for a
    // dark default.
    let said = |k: &str| get(k).and_then(|v| v.into_string().ok());
    Palette::detected_with(false, said)
}

/// The listing as the bytes a terminal gets: ANSI under `palette`, and under [`Palette::None`] the
/// spans' text and nothing else - the same rows, byte for byte.
pub fn paint(lines: &[Line], palette: Palette) -> String {
    let mut out = rano::markdown::to_ansi(lines, palette);
    out.push('\n');
    out
}

// ------------------------------------------------------------------------------------- the layout

/// A folder's header line: which folder, and how many conversations are in it. The folder being
/// listed is the one the reader is standing in, and it is told apart by its hue.
fn folder_head(here: &Path, n: usize, mine: bool) -> Line {
    Line::new(vec![
        Span::role(
            here.display().to_string(),
            if mine {
                Role::Heading
            } else {
                Role::Subheading
            },
        ),
        Span::role(format!(" ({n})"), Role::Faint),
    ])
}

/// The header over the listing, in the same columns as the rows under it - indented by the tree's
/// own branch, so `NAME` sits over the names and not over the glyphs.
fn head_line(wid: usize) -> Line {
    Line::new(vec![Span::role(
        format!(
            "{:width$}{:<wid$}  {:>5}  {:>7}  ID",
            "",
            "NAME",
            "ROWS",
            "AGE",
            width = TREE_COLS
        ),
        Role::Faint,
    )])
}

/// One session on one line: its branch, its name, its rows, how long ago it moved, and the short
/// id `--session` takes. An untitled session leads with its id already, so its id is not printed a
/// second time. `indent` is drawn INSIDE the name column - the census nests a child with it, where
/// the folder listing nests with a branch.
fn row(s: &StoredSession, branch: &str, indent: &str, wid: usize, now: i64, mark: Mark) -> Line {
    let named = s.title.as_deref().is_some_and(|t| !t.is_empty());
    let ms = now - s.last_activity_ms;
    let mut spans = Vec::new();
    if !branch.is_empty() {
        spans.push(Span::role(branch, Role::Faint));
    }
    spans.push(Span::raw(format!(
        "{}  {:>5}",
        fit(&format!("{indent}{}", list_label(s)), wid),
        s.items
    )));
    spans.push(Span::role(format!("  {:>7}", age(ms)), age_role(ms)));
    if named {
        spans.push(Span::role(
            format!("  {}", letibot_sessionlog::registry::short_id(&s.id)),
            Role::Faint,
        ));
    }
    // Where it LIVES, when that is not here. A row whose live copy is on a host is not a session
    // this box is appending to, and a listing that offered it unmarked would be offering a
    // conversation that stopped the day it moved (`docs/remote-hosts.md` RH4).
    if let Some(on) = &s.lives_on {
        spans.push(Span::raw(format!("  [lives on {on}]")));
    }
    // Out of sight, when the listing was asked for those: the word is what says why this row is
    // here at all, under a flag that also drew the ones still in mind.
    match (s.archived_ms, s.archived_reason.as_deref()) {
        (Some(_), Some(why)) if !why.is_empty() => {
            spans.push(Span::role(format!("  [archived: {why}]"), Role::Faint));
        }
        (Some(_), _) => spans.push(Span::role("  [archived]", Role::Faint)),
        (None, _) => {}
    }
    // The mark is a suffix and not a column, like `[lives on …]`: a row with nothing to say about
    // it is the row of the width it always was, and the glyphs of the live rows do not move
    // anybody else's name.
    if !mark.glyph().is_empty() {
        spans.push(Span::role(format!("  {}", mark.glyph()), mark.role()));
    }
    Line::new(spans)
}

/// An age's role: today's is the reader's own foreground, an older one is structure.
fn age_role(ms: i64) -> Role {
    if ms < RECENT_MS {
        Role::Plain
    } else {
        Role::Faint
    }
}

/// The branch a block's `i`-th line of `total` gets.
fn branch(i: usize, total: usize) -> &'static str {
    if i + 1 == total { LAST } else { BRANCH }
}

/// A block's last line when a cap kept rows back: how many, and the listing that draws them. A cap
/// that said nothing would read as a folder that has exactly this many rows.
fn more_line(more: usize) -> Line {
    Line::new(vec![
        Span::role(LAST, Role::Faint),
        Span::role(format!("+{more} more (--all)"), Role::Faint),
    ])
}

/// A line of the footer's counts, which are about the whole list and not about a row.
fn note(text: String) -> Line {
    Line::new(vec![Span::role(format!("{NOTE}{text}"), Role::Faint)])
}

/// What the marks mean, for the glyphs this listing actually drew.
fn legend(shown: Shown) -> Line {
    let mut spans = vec![Span::raw(NOTE.to_string())];
    for (drawn, mark) in [
        (shown.attached, Mark::Attached),
        (shown.running, Mark::Running),
    ] {
        if !drawn {
            continue;
        }
        // Three spaces between two entries: the same gap the columns use.
        if spans.len() > 1 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::role(mark.glyph(), mark.role()));
        spans.push(Span::role(format!(" {}", mark.legend()), Role::Faint));
    }
    Line::new(spans)
}

/// A label in its column: cut to `width` with an ellipsis when it does not fit - a title too long
/// for the terminal is truncated rather than wrapped - and padded to it otherwise, so the facts
/// after it line up down the page.
fn fit(label: &str, width: usize) -> String {
    let mut out = match label.chars().count() <= width {
        true => label.to_string(),
        false => {
            let mut cut: String = label.chars().take(width.saturating_sub(1)).collect();
            cut.push('…');
            cut
        }
    };
    let drawn = out.chars().count();
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(drawn)));
    out
}

/// The name column's width: the longest label among the rows about to be PRINTED, and never more
/// than leaves a whole row inside `cols`. Measured over every row in the store it was the longest
/// brief on the box, which padded every line to 350 columns and wrapped it at 140.
pub(crate) fn name_width(labels: &[String], cols: usize, id_w: usize) -> usize {
    let longest = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    // The name is what is left after the fixed columns and the branch that precedes it; the floor
    // keeps a narrow terminal from drawing a blank name column.
    let room = cols
        .saturating_sub(ROW_FIXED_COLS + TREE_COLS + id_w)
        .max(MIN_NAME_COLS);
    longest.max("NAME".len()).min(room)
}

/// The id column's width over the rows a listing prints: the widest short id it shows. An
/// untitled row leaves the column empty, since its name IS its id.
fn id_width(rows: &[&StoredSession]) -> usize {
    rows.iter()
        .filter(|s| s.title.as_deref().is_some_and(|t| !t.is_empty()))
        .map(|s| {
            letibot_sessionlog::registry::short_id(&s.id)
                .chars()
                .count()
        })
        .max()
        .unwrap_or(0)
}

/// Is this session's folder the one asked about, or below it? `Path::starts_with` compares whole
/// components, so `/p/a2` is not under `/p/a`.
fn under(workspace: &str, here: &Path) -> bool {
    Path::new(workspace).starts_with(here)
}

// -------------------------------------------------------------------------------------- the lists

/// One folder in a plain listing: the rows it draws, and how many its cap kept back.
pub(crate) struct Folder<'a> {
    pub(crate) workspace: &'a str,
    pub(crate) rows: Vec<&'a StoredSession>,
    pub(crate) more: usize,
}

impl Folder<'_> {
    /// Every conversation the folder holds, drawn or counted.
    pub(crate) fn total(&self) -> usize {
        self.rows.len() + self.more
    }

    /// The lines the block draws: a branch per row, and the cap's count as the last one. Only the
    /// branch of the last line differs, so a block has to know its own length.
    pub(crate) fn drawn(&self) -> usize {
        self.rows.len() + usize::from(self.more > 0)
    }
}

/// What a plain listing draws, as data: this folder's conversations (its own workspace, then
/// whatever is below it) and one block per other folder, each capped. Pure, because a printer
/// cannot be read back and the ORDER is the whole of this rule.
pub(crate) struct Blocks<'a> {
    pub(crate) mine: Vec<&'a StoredSession>,
    pub(crate) mine_more: usize,
    pub(crate) others: Vec<Folder<'a>>,
    /// Other folders the cap kept off the list. Their count is a line, not a silence.
    pub(crate) others_more: usize,
    /// Conversations in a folder that is no longer on this disk, and how many such folders. One
    /// line for all of them: a place nobody can open is not a place to scan.
    pub(crate) gone: usize,
    pub(crate) gone_folders: usize,
    /// Conversations hidden because nothing was ever said in them - counted, never dropped silently.
    pub(crate) empty: usize,
}

/// The plain listing, as the lines it prints: the header, this folder's conversations, then one
/// block per other folder, and the counts of what was folded away. A folder's header is a line of
/// its own rather than a padded blank - padding it out to the name column is what turned 441
/// sessions into 1158 lines.
pub fn plain(
    all: &[StoredSession],
    here: &Path,
    cols: usize,
    now: i64,
    marks: &Marks,
) -> Vec<Line> {
    let blocks = conversation_blocks(all, here);
    let printed: Vec<&StoredSession> = blocks
        .mine
        .iter()
        .copied()
        .chain(blocks.others.iter().flat_map(|f| f.rows.iter().copied()))
        .collect();
    let labels: Vec<String> = printed.iter().map(|s| list_label(s)).collect();
    let wid = name_width(&labels, cols, id_width(&printed));
    let shown = Shown::of(printed.iter().map(|s| marks.of(&s.id)));

    let mut out = vec![head_line(wid)];
    out.push(folder_head(
        here,
        blocks.mine.len() + blocks.mine_more,
        true,
    ));
    if blocks.mine.is_empty() {
        out.push(Line::new(vec![
            Span::role(LAST, Role::Faint),
            Span::role("nothing here yet", Role::Faint),
        ]));
    }
    let total = blocks.mine.len() + usize::from(blocks.mine_more > 0);
    for (i, s) in blocks.mine.iter().enumerate() {
        out.push(row(s, branch(i, total), "", wid, now, marks.of(&s.id)));
    }
    if blocks.mine_more > 0 {
        out.push(more_line(blocks.mine_more));
    }
    for f in &blocks.others {
        out.push(Line::default());
        out.push(folder_head(Path::new(f.workspace), f.total(), false));
        let total = f.drawn();
        for (i, s) in f.rows.iter().enumerate() {
            out.push(row(s, branch(i, total), "", wid, now, marks.of(&s.id)));
        }
        if f.more > 0 {
            out.push(more_line(f.more));
        }
    }
    footer(&mut out, &blocks, shown);
    out
}

/// The bottom of a listing: what the marks mean, then what each cap kept back and the flag that
/// draws it. Dim, and together, so the list ends in one place.
fn footer(out: &mut Vec<Line>, blocks: &Blocks, shown: Shown) {
    if shown.any() {
        out.push(Line::default());
        out.push(legend(shown));
    }
    if blocks.others_more > 0 {
        out.push(note(format!(
            "+{} more folders (--all)",
            blocks.others_more
        )));
    }
    if blocks.gone > 0 {
        out.push(note(format!(
            "+{} sessions in {} folders that no longer exist (--all)",
            blocks.gone, blocks.gone_folders
        )));
    }
    if blocks.empty > 0 {
        out.push(note(format!("+{} with no rows (--all)", blocks.empty)));
    }
}

/// `--all`: the census, one group per kind of session with its count and legend, drawing the same
/// one-line rows the plain listing does - every session in the store, grouped by what it is. The
/// groups are not folders and are not a tree: a child is indented under the parent it sits under,
/// and the row already says that in its name column.
pub fn census(all: &[StoredSession], cols: usize, now: i64, marks: &Marks) -> Vec<Line> {
    let groups = grouped(all);
    let rows: Vec<letibot_sessionlog::GroupRow<'_>> = all.iter().map(group_row).collect();
    // The indent is a fact about the whole list - a child is indented only under a parent that is
    // on it - so it is settled before anything is measured or drawn.
    let mut drawn: Vec<(
        &StoredSession,
        &'static str,
        letibot_sessionlog::SessionGroup,
    )> = Vec::new();
    for (group, members) in &groups {
        for s in members {
            let indent = if s.parent_session_id.as_deref().is_some_and(|p| {
                all.iter()
                    .any(|o| o.id == p && letibot_sessionlog::group_of(&rows, &o.id) == *group)
            }) {
                "  "
            } else {
                ""
            };
            drawn.push((s, indent, *group));
        }
    }
    let printed: Vec<&StoredSession> = drawn.iter().map(|(s, _, _)| *s).collect();
    let labels: Vec<String> = drawn
        .iter()
        .map(|(s, indent, _)| format!("{indent}{}", list_label(s)))
        .collect();
    let wid = name_width(&labels, cols, id_width(&printed));
    let shown = Shown::of(printed.iter().map(|s| marks.of(&s.id)));

    let mut out = vec![head_line(wid)];
    let mut seen: Option<letibot_sessionlog::SessionGroup> = None;
    for (s, indent, group) in &drawn {
        if seen != Some(*group) {
            // A group's line, and the count that is the whole reason for it. No line for an empty
            // group, for the reason the picker draws none: a header saying `(0)` teaches a reader
            // to skip headers.
            let n = groups
                .iter()
                .find(|(g, _)| g == group)
                .map_or(0, |(_, members)| members.len());
            out.push(Line::default());
            out.push(Line::new(vec![
                Span::role(format!("{} ({n})", group.title()), Role::Heading),
                Span::role(format!(" — {}", group.legend()), Role::Faint),
            ]));
            seen = Some(*group);
        }
        // The census has no branches: its indent is drawn inside the name column.
        out.push(row(s, "", indent, wid, now, marks.of(&s.id)));
    }
    if shown.any() {
        out.push(Line::default());
        out.push(legend(shown));
    }
    out
}

/// What a plain listing draws, as the lists it prints: this folder's conversations, one block per
/// other folder that is still there, and what it folded away. Pure, because a printer cannot be
/// read back and the ORDER is the whole of this rule.
pub(crate) fn conversation_blocks<'a>(all: &'a [StoredSession], here: &Path) -> Blocks<'a> {
    let rows: Vec<&StoredSession> = all
        .iter()
        .filter(|s| is_conversation(s) && is_operators(s))
        .collect();
    // A session nothing was ever said in is not a conversation a person can pick, and a row of
    // zeros in the list is the noise the list exists to be rid of. `--all` is the census and draws
    // them; the count of what was held back is owed to the reader, and `plain` says it.
    let empty = rows.iter().filter(|s| s.items == 0).count();
    let rows: Vec<&StoredSession> = rows.into_iter().filter(|s| s.items > 0).collect();
    let mut mine: Vec<&StoredSession> = rows
        .iter()
        .copied()
        .filter(|s| under(&s.workspace_root, here))
        .collect();
    // Exact workspace first, then the ones below it. The store's own order is newest activity
    // first and the sort is stable, so each half keeps it.
    mine.sort_by_key(|s| Path::new(&s.workspace_root) != here);
    // A gone folder is not a place a person can pick from the list: its row can still be resumed
    // by id, and `--all` draws it. Counted, never silently dropped. A folder holding a session
    // that lives on a host is kept: it is the answer to where that conversation went.
    let mut folders: Vec<Folder<'a>> = Vec::new();
    for s in rows.into_iter().filter(|s| !under(&s.workspace_root, here)) {
        match folders.iter_mut().find(|f| f.workspace == s.workspace_root) {
            Some(f) => f.rows.push(s),
            None => folders.push(Folder {
                workspace: s.workspace_root.as_str(),
                rows: vec![s],
                more: 0,
            }),
        }
    }
    let (gone, mut others): (Vec<Folder<'a>>, Vec<Folder<'a>>) =
        folders.into_iter().partition(|f| {
            !Path::new(f.workspace).exists() && f.rows.iter().all(|s| s.lives_on.is_none())
        });
    let gone_rows: usize = gone.iter().map(Folder::total).sum();
    // The folder somebody was last in comes first: its newest member's activity is its recency.
    others.sort_by_key(|f| std::cmp::Reverse(f.rows[0].last_activity_ms));
    let mine_more = cap(&mut mine, HERE_SHOWN);
    // The cap is on whole FOLDERS, and a folder that is drawn keeps its own newest rows: eight
    // headers of one row each teach less than two with five, and the rest is one count.
    let others_more = others.len().saturating_sub(FOLDERS_SHOWN);
    others.truncate(FOLDERS_SHOWN);
    for f in &mut others {
        f.more = cap(&mut f.rows, FOLDER_SHOWN);
    }
    Blocks {
        mine,
        mine_more,
        others,
        others_more,
        gone: gone_rows,
        gone_folders: gone.len(),
        empty,
    }
}

/// Draw at most `shown` rows, and answer how many were left back.
fn cap(rows: &mut Vec<&StoredSession>, shown: usize) -> usize {
    let over = rows.len().saturating_sub(shown);
    rows.truncate(shown);
    over
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::{SessionRecord, Store};

    /// The listing is drawn at one fixed instant, so every age in a test is a number and not a
    /// race: 2026-11-25T00:00:00Z, well away from any clock a machine boots with.
    const NOW: i64 = 1_795_564_800_000;
    const HOUR: i64 = 60 * 60 * 1000;
    const DAY: i64 = 24 * HOUR;

    /// Two folders that are really there: a folder that is not is counted as gone, and the
    /// fixtures are about what the list draws. Named by pid AND by a nanosecond, because a seat's
    /// boundary gives every test binary the same pid and a leftover folder would be read as this
    /// run's.
    struct Dirs(Vec<PathBuf>);

    impl Dirs {
        fn new() -> Dirs {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let base = std::env::temp_dir()
                .join(format!("letibot-listing-{}-{stamp}", std::process::id()));
            let dirs = vec![base.join("a"), base.join("b")];
            for d in &dirs {
                std::fs::create_dir_all(d).expect("a fixture folder");
            }
            Dirs(dirs)
        }
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            if let Some(base) = self.0.first().and_then(|d| d.parent()) {
                let _ = std::fs::remove_dir_all(base);
            }
        }
    }

    /// Rows to render, from a real store: what a store row only learns from what came after it -
    /// how many rows it holds and when it last moved - is set here, since a fresh row has neither.
    fn rows(here: &Path, there: &Path) -> Vec<StoredSession> {
        let store = Store::open_in_memory().expect("a store");
        let put = |id: &str, title: Option<&str>, ws: &Path, parent: Option<&str>| {
            store
                .put_session(&SessionRecord {
                    id: id.into(),
                    title: title.map(Into::into),
                    model_id: "m".into(),
                    dialect_sha: "d".into(),
                    workspace_root: ws.display().to_string(),
                    owner: "op".into(),
                    role: None,
                    approvers: vec![],
                    parent_session_id: parent.map(Into::into),
                })
                .expect("a fixture row");
        };
        put("s-mine", Some("the conversation"), here, None);
        put("s-quiet", Some("the quiet one"), here, None);
        put("s-child", Some("a child"), here, Some("s-mine"));
        put("s-there", Some("another folder's"), there, None);
        put("s-empty", Some("never spoken in"), here, None);
        let mut all = store.list_sessions().expect("the rows");
        let mut set = |id: &str, items: u32, age: i64| {
            let r = all.iter_mut().find(|s| s.id == id).expect("a fixture row");
            r.items = items;
            r.last_activity_ms = NOW - age;
        };
        set("s-mine", 12, 2 * HOUR);
        set("s-quiet", 3, 9 * DAY);
        set("s-child", 4, HOUR);
        set("s-there", 7, 5 * HOUR);
        set("s-empty", 0, HOUR);
        // The order the store gives a listing is newest activity first, and it decided that BEFORE
        // these rows learned their own times: a store whose rows all moved at once has no order to
        // keep, so it is re-made here rather than left to whatever the query happened to return.
        all.sort_by_key(|s| std::cmp::Reverse(s.last_activity_ms));
        all
    }

    /// What a listing draws, as the text a pipe gets: the spans' text and nothing else.
    fn plain_text(lines: &[Line]) -> String {
        rano::markdown::to_plain(lines)
    }

    fn marks(pairs: &[(&str, Mark)]) -> Marks {
        let mut out = Marks::default();
        for (id, m) in pairs {
            out.put((*id).to_string(), *m);
        }
        out
    }

    /// Every row of a folder hangs off it, and the last one ends the block: that is the whole of
    /// what a tree says that a flat list does not.
    #[test]
    fn the_plain_listing_is_a_tree_under_each_folder() {
        let dirs = Dirs::new();
        let all = rows(&dirs.0[0], &dirs.0[1]);
        let text = plain_text(&plain(&all, &dirs.0[0], 100, NOW, &Marks::default()));
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[0].starts_with("   NAME"),
            "the header is indented by the branch it sits over: {:?}",
            lines[0]
        );
        assert_eq!(
            lines[1],
            format!("{} (2)", dirs.0[0].display()),
            "this folder, and how many conversations are in it"
        );
        assert!(
            lines[2].starts_with("\u{251c}\u{2500} the conversation")
                && lines[3].starts_with("\u{2514}\u{2500} the quiet one"),
            "two branches, and the last one ends the block:\n{text}"
        );
        assert!(
            text.contains(&format!("{} (1)", dirs.0[1].display())),
            "another folder is a block of its own:\n{text}"
        );
        assert!(
            !text.contains("a child") && !text.contains("never spoken in"),
            "a child is not a conversation to continue, and an empty one is counted and not 
             drawn:\n{text}"
        );
        assert!(
            text.contains("    +1 with no rows (--all)"),
            "what was held back is a line, not a silence:\n{text}"
        );
    }

    /// A cap's count is the last branch, so a block never ends on a row that has more under it.
    #[test]
    fn the_cap_is_the_last_branch_and_a_full_folder_ends_on_its_row() {
        let dirs = Dirs::new();
        let all = rows(&dirs.0[0], &dirs.0[1]);
        let mut many = all.clone();
        // A top-level row of this folder, cloned: a child's copy would be filtered out of the list
        // these fillers are here to overflow.
        let template = all
            .iter()
            .find(|s| s.id == "s-mine")
            .expect("a row")
            .clone();
        for i in 0..25 {
            let mut r = template.clone();
            r.id = format!("s-extra-{i}");
            r.title = Some(format!("filler {i}"));
            r.last_activity_ms = NOW - i as i64 * 1000;
            many.push(r);
        }
        let text = plain_text(&plain(&many, &dirs.0[0], 100, NOW, &Marks::default()));
        let here: Vec<&str> = text
            .lines()
            .skip_while(|l| !l.starts_with(&format!("{} (", dirs.0[0].display())))
            .take_while(|l| !l.is_empty())
            .collect();
        assert_eq!(
            here.last().copied(),
            Some("\u{2514}\u{2500} +7 more (--all)"),
            "twenty rows and the count that ends the block:\n{text}"
        );
        assert_eq!(
            here.iter()
                .filter(|l| l.starts_with("\u{251c}\u{2500} "))
                .count(),
            20,
            "the cap holds this folder to twenty rows"
        );
    }

    /// The styles are what a reader scans by: the folder stood in is a different hue from the
    /// others, and the age of an old conversation is structure.
    #[test]
    fn the_listing_is_painted_in_rano_roles() {
        let dirs = Dirs::new();
        let all = rows(&dirs.0[0], &dirs.0[1]);
        let lines = plain(&all, &dirs.0[0], 100, NOW, &Marks::default());
        let ansi = rano::markdown::to_ansi(&lines, Palette::Colour);
        let heading = Palette::Colour.open(Role::Heading);
        let subheading = Palette::Colour.open(Role::Subheading);
        let faint = Palette::Colour.open(Role::Faint);
        assert!(
            ansi.contains(&format!("{heading}{}", dirs.0[0].display())),
            "the folder this list is of is its own role:\n{ansi}"
        );
        assert!(
            ansi.contains(&format!("{subheading}{}", dirs.0[1].display())),
            "and every other folder a different one:\n{ansi}"
        );
        assert!(
            ansi.contains(&format!("{faint}{:>9}", "9d")),
            "an age nine days old is dim:\n{ansi}"
        );
        assert!(
            !ansi.contains(&format!("{faint}{:>9}", "2h")),
            "and one from this morning is not:\n{ansi}"
        );
    }

    /// Two spellings of the same rows: the plain one is the spans' text, so a pipe, a log and a
    /// colour terminal show the same columns.
    #[test]
    fn the_plain_and_the_painted_listing_are_the_same_text() {
        let dirs = Dirs::new();
        let all = rows(&dirs.0[0], &dirs.0[1]);
        let lines = plain(&all, &dirs.0[0], 100, NOW, &Marks::default());
        let plain_painted = paint(&lines, Palette::None);
        assert_eq!(
            plain_painted,
            format!("{}\n", rano::markdown::to_plain(&lines)),
            "no palette, no escapes - and the newline a shell expects exactly once"
        );
        assert!(palette_for(false, &|_| None) == Palette::None);
    }

    /// A mark is a suffix on the row it belongs to, and the legend is owed only for a glyph the
    /// list actually shows.
    #[test]
    fn a_mark_trails_its_own_row_and_the_legend_only_lists_what_is_drawn() {
        let dirs = Dirs::new();
        let all = rows(&dirs.0[0], &dirs.0[1]);
        let live = marks(&[("s-mine", Mark::Attached), ("s-there", Mark::Running)]);
        let text = plain_text(&plain(&all, &dirs.0[0], 100, NOW, &live));
        let row = |id: &str| {
            text.lines()
                .find(|l| l.contains(id))
                .unwrap_or_else(|| panic!("no row for {id}:\n{text}"))
                .to_string()
        };
        assert!(
            row("the conversation").ends_with(" \u{25cf}"),
            "a head is attached, said on that row:\n{text}"
        );
        assert!(
            row("another folder's").ends_with(" \u{25d0}"),
            "a turn is running, said on that row:\n{text}"
        );
        assert!(
            !row("the quiet one").contains('\u{25cf}'),
            "and a session nobody is watching says nothing:\n{text}"
        );
        assert!(
            text.contains("    \u{25cf} a head is attached   \u{25d0} a turn is running"),
            "the legend names both glyphs this list drew:\n{text}"
        );
        let only_one = marks(&[("s-mine", Mark::Attached)]);
        let text = plain_text(&plain(&all, &dirs.0[0], 100, NOW, &only_one));
        assert_eq!(
            text.lines()
                .filter(|l| l.contains("a head is attached"))
                .count(),
            1,
            "and only the glyphs it drew:\n{text}"
        );
        assert!(
            !text.contains("a turn is running"),
            "a legend for a glyph on no row is a puzzle:\n{text}"
        );
    }

    /// Nothing live, no legend: a store-only listing ends with its counts.
    #[test]
    fn a_listing_with_nothing_live_has_no_legend() {
        let dirs = Dirs::new();
        let all = rows(&dirs.0[0], &dirs.0[1]);
        let text = plain_text(&plain(&all, &dirs.0[0], 100, NOW, &Marks::default()));
        assert!(!text.contains("a head is attached"), "{text}");
        assert!(
            text.ends_with("    +1 with no rows (--all)"),
            "the counts are the last thing said, and there is no legend before them: {text}"
        );
    }

    /// A turn is the fact that changes on its own, so it is the one drawn when a session has both.
    #[test]
    fn a_running_turn_is_drawn_over_a_quiet_head_and_a_held_nobody_is_idle() {
        assert_eq!(mark_of(0, false), Mark::Idle);
        assert_eq!(mark_of(2, false), Mark::Attached);
        assert_eq!(mark_of(0, true), Mark::Running);
        assert_eq!(mark_of(3, true), Mark::Running);
        assert_eq!(Mark::Idle.glyph(), "", "idle has no glyph at all");
    }

    /// A pipe, `NO_COLOR` and `TERM=dumb` are three ways of saying "no escapes", and an empty
    /// `NO_COLOR` is not one of them.
    #[test]
    fn a_pipe_no_color_and_a_dumb_terminal_are_all_plain() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        assert_eq!(palette_for(false, &|_| None), Palette::None, "a pipe");
        assert_eq!(
            palette_for(true, &env(&[("NO_COLOR", "1")])),
            Palette::None,
            "NO_COLOR=1"
        );
        assert_eq!(
            palette_for(true, &env(&[("TERM", "dumb")])),
            Palette::None,
            "TERM=dumb"
        );
        assert_eq!(
            palette_for(true, &env(&[("NO_COLOR", "")])),
            Palette::Colour,
            "an empty NO_COLOR is not a request; no COLORTERM, no exact colours"
        );
        assert_eq!(
            palette_for(true, &env(&[("COLORTERM", "truecolor")])),
            Palette::Exact,
            "a terminal that draws exact colours gets them"
        );
    }

    /// The width the terminal gives is the width the whole row gets, branch and all.
    #[test]
    fn a_row_inside_the_terminal_counts_its_branch() {
        let dirs = Dirs::new();
        let mut all = rows(&dirs.0[0], &dirs.0[1]);
        let long = all.iter_mut().find(|s| s.id == "s-mine").expect("a row");
        long.title = Some("a title nobody would fit in a narrow terminal, not even close".into());
        let cols = 60;
        let lines = plain(&all, &dirs.0[0], cols, NOW, &Marks::default());
        let text = plain_text(&lines);
        for line in text.lines() {
            assert!(
                line.chars().count() <= cols,
                "a row past the terminal wraps and reads as two: {line:?}"
            );
        }
        let named = text
            .lines()
            .find(|l| l.starts_with("\u{251c}\u{2500} a title"))
            .expect("the long row");
        assert!(named.contains('\u{2026}'), "cut, not wrapped: {named:?}");
    }

    /// A daemon's refusal is the only door that says what it holds, and the count in it is a
    /// count and not an id.
    #[test]
    fn the_ids_a_daemon_holds_are_read_from_its_refusal() {
        assert_eq!(
            held_in("no such session \"\u{0}probe\"; it holds s-1, s-2, and 3 more"),
            vec!["s-1".to_string(), "s-2".to_string()]
        );
        assert!(
            held_in("no such session \"x\": this daemon holds none live").is_empty(),
            "a daemon holding nothing says so and names nothing"
        );
    }

    /// The probe asks a real daemon, and is not a head it leaves behind: the session it attached
    /// to is drawn as unattended once the round trip is over, because the one head on it was the
    /// probe's own.
    #[test]
    fn the_probe_reads_the_daemon_and_leaves_no_head() {
        let dirs = Dirs::new();
        let socket = dirs.0[0].join("room.sock");
        let registry = letibot_sessionlog::Registry::new();
        let hub = registry
            .create(
                "s-probed",
                "probed",
                letibot_sessionlog::SessionWiring::default(),
            )
            .expect("a session to hold");
        let _serving = letibot_sessionlog::server::serve_registry(registry.clone(), &socket)
            .expect("a socket to probe");
        assert_eq!(
            marks_at(std::slice::from_ref(&socket)).of("s-probed"),
            Mark::Idle,
            "a session nobody is watching"
        );
        // A head of somebody's own: the probe must see it, and must leave only it.
        let (_head, _hello, _reader) = letibot_sessionlog::client::HeadClient::attach(
            &socket,
            "s-probed",
            0,
            "tui",
            "test",
            letibot_sessionlog::protocol::Caps::default(),
        )
        .expect("a head attached");
        assert_eq!(
            marks_at(std::slice::from_ref(&socket)).of("s-probed"),
            Mark::Attached,
            "a head attached is drawn"
        );
        assert_eq!(
            hub.attached_heads(),
            1,
            "and the probe's own head is gone again: one head, the test's"
        );
    }
}
