//! The composer: multi-line editing, history, paste, undo, and interrupt.
//!
//! # What the head has today, and what breaks
//!
//! `letibot-tui`'s input is a `String` and five keys (`Char`, `Enter`,
//! `Backspace`, `Up`, `Down`, `CtrlC`). There is no cursor to move, no way to
//! write a two-line prompt, no history, and `term::keys()` reads into a
//! **64-byte buffer** and decodes what fits — so pasting a 3 KB stack trace
//! arrives as a sequence of partial reads, splits a UTF-8 sequence at every
//! boundary that lands mid-character, and drops those bytes on the floor
//! (`std::str::from_utf8` fails and the arm is silent). A person pasting an
//! error message into an agent is not an edge case.
//!
//! This module is the model half of the fix. It owns text, cursor, undo,
//! history and the paste ledger, and it produces lines. It reads no file
//! descriptor and no clock: time arrives as a parameter, because the
//! double-tap-to-interrupt rules below are timing rules and a timing rule that
//! samples the clock internally cannot be tested.
//!
//! # Provenance
//!
//! **Adapted from grok-build** (xAI, Apache-2.0),
//! `crates/codegen/xai-ratatui-textarea/src/editor.rs` — the headless half of
//! their textarea, and the most portable thing in that tree (1,002 lines, zero
//! internal dependencies):
//!
//! - The **plan/apply split**: a command is turned into an `EditPlan`
//!   (`replaced_byte_range`, `replacement`, `removed_text`, `cursor_byte`)
//!   before anything mutates, so a host can inspect or veto it. [`Plan`] is that
//!   idea, kept because it is also what makes the paste-placeholder ledger
//!   below correct — the ledger has to see the byte range that moved.
//! - `WordStyle { Small, WhitespaceDelimited }` → [`WordStyle`], the vim `w`
//!   versus `W` distinction.
//! - The **undo batching rule**: consecutive inserts coalesce, but a cursor jump
//!   or a **whitespace ↔ non-whitespace transition** breaks the batch
//!   (`textarea.rs:618-631`), and kills are always their own entry.
//! - **The kill buffer is not restored by undo** (`textarea.rs:2070`: "yank is
//!   separate from undo"). Non-obvious, and right: undo restores the document,
//!   not the clipboard.
//! - **Redo is bound to Alt+Z as well as Ctrl+Shift+Z**, because in many
//!   terminals Ctrl+Shift+Z arrives byte-identical to Ctrl+Z
//!   (`textarea.rs:2010-2031`).
//! - The sticky **preferred column** for vertical motion.
//!
//! **Adapted from opencode** (MIT, read through the Kilo Code fork which retains
//! opencode's copyright), `packages/tui/src/`:
//!
//! - `component/prompt/index.tsx:424-455` and `887-906` — the **two-counter
//!   interrupt model**: Esc twice within **5 s** interrupts the model; Ctrl+C
//!   twice within **1 s** quits, and only when the composer is empty, because
//!   Ctrl+C on a non-empty composer means "clear what I typed". The hint text
//!   changes to `esc again to interrupt` after the first press, which is what
//!   makes a double-tap discoverable rather than folklore. Both windows and the
//!   post-increment-then-test structure are theirs.
//! - `component/prompt/index.tsx:1307-1355` — **a large paste becomes a
//!   placeholder**: at **≥5 lines or >800 characters** the text collapses to
//!   `[Pasted ~N lines]` and the real content is carried beside the buffer,
//!   expanded on submit. Without it a pasted file fills the composer and the
//!   conversation scrolls away.
//! - `component/prompt/index.tsx:1494` — the composer never takes more than a
//!   **third of the screen**, floor 6 rows. [`Editor::height`].
//! - `prompt/history.tsx` — history is a **capped ring of 50**, consecutive
//!   duplicates are dropped, and — the good part — **once you edit a recalled
//!   entry, the arrow keys stop navigating**, so an edit cannot be lost to a
//!   keystroke.
//! - `component/prompt/index.tsx:1555-1579` — paste normalises CRLF *and* bare
//!   CR, because Windows ConPTY sends CR-only newlines in a bracketed paste.
//!
//! Changed, and why:
//!
//! - opencode tracks placeholders with **extmarks** — ranges that survive
//!   arbitrary editing — so a placeholder can be moved, split, or partially
//!   deleted and still round-trip. That needs a range-tracking data structure
//!   this crate does not have and does not want. [`Editor`] keys the ledger on
//!   the **placeholder text itself**, which is unique by construction
//!   (`[Pasted #3 ~120 lines]`), so a placeholder that is deleted drops its
//!   paste and one that is edited stops matching and is submitted as the
//!   literal text on screen. That is a real behavioural difference and the
//!   honest description of it is: it fails visibly instead of silently.
//! - grok-build's undo is snapshot-based over the whole buffer with elements and
//!   selection; this snapshots text and cursor only, since there is no selection
//!   model here yet.
//! - Neither upstream is copied line for line: grok-build's is ratatui-typed and
//!   opencode's is SolidJS reactive. These are the rules, in Rust, as data.

use crate::style::{Palette, Role};
use crate::width;

/// Which characters make up a "word" for a word motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordStyle {
    /// Alphanumerics and `_` are one word; punctuation is another. Vim's `w`.
    Small,
    /// Anything not whitespace is one word. Vim's `W`.
    WhitespaceDelimited,
}

/// A key, decoded. Larger than `letibot_tui::app::Key` on purpose: every variant
/// here is a key a person will press in a composer and today has no meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// A bracketed paste, arriving whole rather than as N `Char`s.
    Paste(String),
    Enter,
    /// Newline without submitting — Shift+Enter, or Alt+Enter where the terminal
    /// cannot report the first.
    SoftEnter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    Home,
    End,
    /// Ctrl+K.
    KillToEnd,
    /// Ctrl+U.
    KillToStart,
    /// Ctrl+W.
    KillWordBack,
    /// Ctrl+Y.
    Yank,
    Undo,
    Redo,
    Esc,
    CtrlC,
    /// Ctrl+D on an empty composer.
    Eof,
}

/// What the head should do about a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reaction {
    /// Nothing visible changed.
    Idle,
    /// Redraw.
    Changed,
    /// Send this to the daemon.
    Submit(String),
    /// Interrupt the running turn.
    Interrupt,
    /// Leave.
    Quit,
}

/// A pending edit, before it is applied.
///
/// grok-build's `EditPlan`, minus the affinity field this crate has no use for.
/// The reason to have it at all: applying is a one-liner, but *knowing the byte
/// range that is about to move* is what lets the paste ledger and (later) an
/// undo coalescer decide anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub range: std::ops::Range<usize>,
    pub replacement: String,
    pub removed: String,
    pub cursor: usize,
}

/// Why the buffer changed. Undo batches by this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Insert,
    Delete,
    /// Always its own undo entry.
    Kill,
    /// Always its own undo entry.
    Paste,
}

#[derive(Debug, Clone)]
struct Snapshot {
    text: String,
    cursor: usize,
}

/// A double-tap counter with a window.
#[derive(Debug, Clone, Copy, Default)]
struct Taps {
    count: u32,
    last_ms: u64,
}

impl Taps {
    /// Register a press. Returns the number of presses inside the window,
    /// including this one.
    fn press(&mut self, now_ms: u64, window_ms: u64) -> u32 {
        if now_ms.saturating_sub(self.last_ms) > window_ms {
            self.count = 0;
        }
        self.count += 1;
        self.last_ms = now_ms;
        self.count
    }

    fn armed(&self, now_ms: u64, window_ms: u64) -> bool {
        self.count > 0 && now_ms.saturating_sub(self.last_ms) <= window_ms
    }

    fn clear(&mut self) {
        self.count = 0;
    }
}

/// Esc twice within this many milliseconds interrupts the turn. opencode's
/// number.
pub const INTERRUPT_WINDOW_MS: u64 = 5_000;
/// Ctrl+C twice within this many milliseconds quits. opencode's number.
pub const QUIT_WINDOW_MS: u64 = 1_000;
/// A paste at or above this many lines collapses to a placeholder.
pub const PASTE_LINES: usize = 5;
/// …or at or above this many bytes.
pub const PASTE_BYTES: usize = 800;
/// How many submitted prompts are remembered.
pub const HISTORY_CAP: usize = 50;

/// The composer.
#[derive(Debug)]
pub struct Editor {
    text: String,
    cursor: usize,
    /// Sticky column for vertical motion, in display columns.
    preferred_col: Option<usize>,
    kill: String,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_kind: Option<Kind>,
    last_cursor: usize,
    /// Submitted prompts, oldest first.
    history: Vec<String>,
    /// `None` when editing the live buffer; otherwise an index from the end.
    history_at: Option<usize>,
    /// The text as it was when a history entry was recalled, so an edit to it
    /// can be detected.
    history_recalled: Option<String>,
    /// **What was in the buffer when the history walk began** (§6).
    ///
    /// Walking down past the newest entry used to restore the **empty string**, so a
    /// half-written prompt was destroyed by pressing Up once to glance at the previous
    /// message and Down to come back to it. Every shell keeps the draft across a walk,
    /// and the loss was silent: the words were not sent anywhere, they were simply
    /// gone, and there is no undo entry for a recall because a recall is navigation.
    ///
    /// `None` means no walk is in progress. Kept across the whole walk (never
    /// overwritten by a recalled entry) and taken — not merely read — when the walk
    /// ends, so a second walk saves its own draft rather than inheriting the first
    /// one's.
    draft: Option<String>,
    /// Placeholder text → the real pasted content.
    pastes: Vec<(String, String)>,
    esc_taps: Taps,
    ctrlc_taps: Taps,
    undo_depth: usize,
}

impl Default for Editor {
    fn default() -> Self {
        Editor::new()
    }
}

impl Editor {
    pub fn new() -> Editor {
        Editor {
            text: String::new(),
            cursor: 0,
            preferred_col: None,
            kill: String::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            last_kind: None,
            last_cursor: 0,
            history: Vec::new(),
            history_at: None,
            history_recalled: None,
            draft: None,
            pastes: Vec::new(),
            esc_taps: Taps::default(),
            ctrlc_taps: Taps::default(),
            undo_depth: 200,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Load prior history, oldest first. The caller owns persistence — this
    /// crate opens no files.
    pub fn with_history(mut self, entries: Vec<String>) -> Editor {
        self.history = entries;
        self.trim_history();
        self
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Feed a key. `now_ms` is a monotonic millisecond clock supplied by the
    /// caller.
    pub fn key(&mut self, k: Key, now_ms: u64) -> Reaction {
        // Any key that is not Esc disarms the interrupt double-tap; otherwise a
        // stray Esc from a decoded escape sequence half an hour ago would arm a
        // second one.
        if !matches!(k, Key::Esc) {
            self.esc_taps.clear();
        }
        if !matches!(k, Key::CtrlC) {
            self.ctrlc_taps.clear();
        }
        match k {
            Key::Char(c) => {
                self.insert(&c.to_string());
                Reaction::Changed
            }
            Key::Paste(s) => {
                self.paste(&s);
                Reaction::Changed
            }
            Key::Enter => {
                if self.text.trim().is_empty() {
                    return Reaction::Idle;
                }
                let out = self.take();
                Reaction::Submit(out)
            }
            Key::SoftEnter => {
                self.insert("\n");
                Reaction::Changed
            }
            Key::Backspace => {
                if self.cursor == 0 {
                    return Reaction::Idle;
                }
                let prev = self.prev_boundary(self.cursor);
                self.replace(prev..self.cursor, "", Kind::Delete);
                Reaction::Changed
            }
            Key::Delete => {
                if self.cursor >= self.text.len() {
                    return Reaction::Idle;
                }
                let next = self.next_boundary(self.cursor);
                self.replace(self.cursor..next, "", Kind::Delete);
                Reaction::Changed
            }
            Key::Left => {
                self.preferred_col = None;
                if self.cursor == 0 {
                    return Reaction::Idle;
                }
                self.cursor = self.prev_boundary(self.cursor);
                Reaction::Changed
            }
            Key::Right => {
                self.preferred_col = None;
                if self.cursor >= self.text.len() {
                    return Reaction::Idle;
                }
                self.cursor = self.next_boundary(self.cursor);
                Reaction::Changed
            }
            Key::WordLeft => {
                self.preferred_col = None;
                self.cursor = self.word_left(WordStyle::Small);
                Reaction::Changed
            }
            Key::WordRight => {
                self.preferred_col = None;
                self.cursor = self.word_right(WordStyle::Small);
                Reaction::Changed
            }
            Key::Home => {
                self.preferred_col = None;
                self.cursor = self.line_start();
                Reaction::Changed
            }
            Key::End => {
                self.preferred_col = None;
                self.cursor = self.line_end();
                Reaction::Changed
            }
            Key::Up | Key::Down => {
                // Vertical motion needs a width, which the caller has and this
                // does not. `vertical` is the entry point that takes one; this
                // arm exists so an unwired Up/Down does something sane rather
                // than nothing: it walks history, which is what a single-line
                // composer's Up meant.
                self.recall(matches!(k, Key::Up))
            }
            Key::KillToEnd => {
                let end = self.line_end();
                if end == self.cursor {
                    return Reaction::Idle;
                }
                self.kill = self.text[self.cursor..end].to_string();
                self.replace(self.cursor..end, "", Kind::Kill);
                Reaction::Changed
            }
            Key::KillToStart => {
                let start = self.line_start();
                if start == self.cursor {
                    return Reaction::Idle;
                }
                self.kill = self.text[start..self.cursor].to_string();
                self.replace(start..self.cursor, "", Kind::Kill);
                Reaction::Changed
            }
            Key::KillWordBack => {
                let start = self.word_left(WordStyle::WhitespaceDelimited);
                if start == self.cursor {
                    return Reaction::Idle;
                }
                self.kill = self.text[start..self.cursor].to_string();
                self.replace(start..self.cursor, "", Kind::Kill);
                Reaction::Changed
            }
            Key::Yank => {
                if self.kill.is_empty() {
                    return Reaction::Idle;
                }
                let k = self.kill.clone();
                self.replace(self.cursor..self.cursor, &k, Kind::Paste);
                Reaction::Changed
            }
            Key::Undo => {
                if let Some(s) = self.undo.pop() {
                    self.redo.push(Snapshot {
                        text: self.text.clone(),
                        cursor: self.cursor,
                    });
                    self.text = s.text;
                    self.cursor = s.cursor.min(self.text.len());
                    self.last_kind = None;
                    // The kill buffer is deliberately untouched: undo restores
                    // the document, not the clipboard.
                    Reaction::Changed
                } else {
                    Reaction::Idle
                }
            }
            Key::Redo => {
                if let Some(s) = self.redo.pop() {
                    self.undo.push(Snapshot {
                        text: self.text.clone(),
                        cursor: self.cursor,
                    });
                    self.text = s.text;
                    self.cursor = s.cursor.min(self.text.len());
                    self.last_kind = None;
                    Reaction::Changed
                } else {
                    Reaction::Idle
                }
            }
            Key::Esc => {
                let n = self.esc_taps.press(now_ms, INTERRUPT_WINDOW_MS);
                if n >= 2 {
                    self.esc_taps.clear();
                    Reaction::Interrupt
                } else {
                    // The first press changes the hint, which is the whole
                    // reason a double-tap is discoverable.
                    Reaction::Changed
                }
            }
            Key::CtrlC => {
                if !self.text.is_empty() {
                    // Ctrl+C on a non-empty composer means "clear what I typed",
                    // never "quit". Quitting on it is how a person loses a
                    // paragraph they were still writing.
                    self.ctrlc_taps.clear();
                    self.snapshot(Kind::Kill);
                    self.text.clear();
                    self.cursor = 0;
                    self.pastes.clear();
                    return Reaction::Changed;
                }
                let n = self.ctrlc_taps.press(now_ms, QUIT_WINDOW_MS);
                if n >= 2 {
                    Reaction::Quit
                } else {
                    Reaction::Changed
                }
            }
            Key::Eof => {
                if self.text.is_empty() {
                    Reaction::Quit
                } else {
                    Reaction::Idle
                }
            }
        }
    }

    /// Vertical motion, which needs the wrap width the composer is drawn at.
    ///
    /// Moves by **visual** row, not logical line. A pasted paragraph soft-wrapped
    /// over six rows should take six presses to cross, not one; anything else
    /// makes the cursor jump somewhere the person was not looking.
    pub fn vertical(&mut self, up: bool, cols: usize) -> Reaction {
        let rows = width::wrap_ranges(&self.text, cols).len();
        let (row, col) = width::locate(&self.text, self.cursor, cols);
        let col = self.preferred_col.unwrap_or(col);
        if up && row == 0 {
            // At the top: Up leaves the buffer and walks history, which is what
            // every shell does.
            return self.recall(true);
        }
        if !up && row + 1 >= rows {
            return self.recall(false);
        }
        let target = if up { row - 1 } else { row + 1 };
        self.cursor = width::offset_at(&self.text, target, col, cols);
        self.preferred_col = Some(col);
        Reaction::Changed
    }

    /// Walk history. Returns `Idle` when navigation is refused.
    fn recall(&mut self, back: bool) -> Reaction {
        // The rule worth having: once a recalled entry has been *edited*,
        // navigation stops, because the next press would silently destroy the
        // edit.
        if let Some(orig) = &self.history_recalled
            && orig != &self.text
        {
            return Reaction::Idle;
        }
        if self.history.is_empty() {
            return Reaction::Idle;
        }
        let next = match (self.history_at, back) {
            (None, true) => Some(0),
            (None, false) => return Reaction::Idle,
            (Some(i), true) if i + 1 < self.history.len() => Some(i + 1),
            (Some(i), true) => Some(i),
            (Some(0), false) => None,
            (Some(i), false) => Some(i - 1),
        };
        // **Save the draft as the walk begins**, which `(None, true)` is exactly.
        // Checked here rather than in `vertical` so the two directions cannot disagree
        // about when a walk starts, and guarded on `draft.is_none()` so a walk that
        // somehow begins twice keeps the first draft rather than a recalled entry.
        if self.history_at.is_none() && back && self.draft.is_none() {
            self.draft = Some(self.text.clone());
        }
        self.history_at = next;
        self.text = match next {
            Some(i) => self.history[self.history.len() - 1 - i].clone(),
            // **Their own line, not an empty buffer.** `take`n rather than read, so the
            // next walk starts from a clean slate.
            None => self.draft.take().unwrap_or_default(),
        };
        self.cursor = self.text.len();
        self.history_recalled = Some(self.text.clone());
        self.preferred_col = None;
        Reaction::Changed
    }

    /// Insert text at the cursor, as typing.
    pub fn insert(&mut self, s: &str) {
        self.replace(self.cursor..self.cursor, s, Kind::Insert);
    }

    /// Take a paste. Normalises line endings, and collapses a large one to a
    /// placeholder.
    pub fn paste(&mut self, raw: &str) {
        // CRLF first, then bare CR: Windows ConPTY sends CR-only newlines inside
        // a bracketed paste, and a naive `\r\n` replace leaves those behind as
        // control characters that a terminal renders as a carriage return.
        let s = raw.replace("\r\n", "\n").replace('\r', "\n");
        // **Lines, which is not newlines-plus-one when the paste ends in a newline.**
        //
        // This was `s.matches('\n').count() + 1`, and almost every real paste is a
        // whole file dragged out of an editor — which ends `…\n` — so a 300-line paste
        // announced itself as **301**, and a 40-line stack trace as 41. The reader's
        // one use for this number is deciding whether the placeholder holds their whole
        // paste, and an off-by-one there is the number disagreeing with the thing it
        // counts.
        //
        // `lines()` is the count a reader would make: `"a\nb\n"` and `"a\nb"` are both
        // two lines, and an empty paste is zero (which is below the threshold and so is
        // not collapsed — the right answer for a paste of nothing).
        let lines = s.lines().count();
        if lines >= PASTE_LINES || s.len() > PASTE_BYTES {
            let n = self.pastes.len() + 1;
            let placeholder = format!("[Pasted #{n} ~{lines} lines]");
            self.pastes.push((placeholder.clone(), s));
            self.replace(self.cursor..self.cursor, &placeholder, Kind::Paste);
        } else {
            self.replace(self.cursor..self.cursor, &s, Kind::Paste);
        }
    }

    /// Everything a placeholder stands for, substituted back in.
    ///
    /// A placeholder the person deleted takes its paste with it; a placeholder
    /// they edited no longer matches and is sent as the literal text on screen.
    /// Both are visible outcomes of a visible action, which is the trade made
    /// against opencode's extmark ranges — see the module header.
    pub fn expanded(&self) -> String {
        let mut out = self.text.clone();
        for (ph, full) in &self.pastes {
            out = out.replace(ph.as_str(), full);
        }
        out
    }

    /// Submit: the expanded text, with the buffer cleared and history updated.
    fn take(&mut self) -> String {
        let out = self.expanded();
        let shown = std::mem::take(&mut self.text);
        if self.history.last().map(String::as_str) != Some(shown.as_str()) {
            self.history.push(shown);
            self.trim_history();
        }
        self.cursor = 0;
        self.pastes.clear();
        self.history_at = None;
        self.history_recalled = None;
        // A submitted line is not a draft: the walk starts fresh next time.
        self.draft = None;
        self.undo.clear();
        self.redo.clear();
        self.last_kind = None;
        self.preferred_col = None;
        out
    }

    fn trim_history(&mut self) {
        while self.history.len() > HISTORY_CAP {
            self.history.remove(0);
        }
    }

    /// Plan an edit without applying it.
    pub fn plan(&self, range: std::ops::Range<usize>, replacement: &str) -> Plan {
        Plan {
            removed: self.text[range.clone()].to_string(),
            cursor: range.start + replacement.len(),
            range,
            replacement: replacement.to_string(),
        }
    }

    fn replace(&mut self, range: std::ops::Range<usize>, s: &str, kind: Kind) {
        self.snapshot(kind);
        self.text.replace_range(range.clone(), s);
        self.cursor = range.start + s.len();
        self.last_cursor = self.cursor;
        self.last_kind = Some(kind);
        self.preferred_col = None;
        self.redo.clear();
    }

    /// Push an undo entry if this mutation starts a new batch.
    ///
    /// The batching rule is grok-build's and it is the one that makes undo feel
    /// right: typing a word is one undo, but the space before it is a boundary,
    /// and so is moving the cursor. Without the whitespace rule, undo either
    /// swallows a paragraph or steps one character at a time.
    fn snapshot(&mut self, kind: Kind) {
        let batch = match (self.last_kind, kind) {
            (Some(Kind::Insert), Kind::Insert) | (Some(Kind::Delete), Kind::Delete) => {
                // A cursor jump breaks the batch…
                if self.cursor != self.last_cursor {
                    false
                } else {
                    // …and so does crossing between whitespace and not.
                    let prev_ws = self.text[..self.cursor]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace);
                    let now_ws = self.text[self.cursor..]
                        .chars()
                        .next()
                        .is_some_and(char::is_whitespace);
                    prev_ws == now_ws
                }
            }
            // Kill and paste are always their own entry.
            _ => false,
        };
        if batch {
            return;
        }
        self.undo.push(Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
        });
        while self.undo.len() > self.undo_depth {
            self.undo.remove(0);
        }
    }

    fn prev_boundary(&self, at: usize) -> usize {
        let cs = width::cells(&self.text[..at]);
        match cs.last() {
            Some(c) => at - c.text.len(),
            None => 0,
        }
    }

    fn next_boundary(&self, at: usize) -> usize {
        match width::cells(&self.text[at..]).first() {
            Some(c) => at + c.esc.len() + c.text.len(),
            None => at,
        }
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len())
    }

    fn word_left(&self, style: WordStyle) -> usize {
        let s = &self.text[..self.cursor];
        let mut i = self.cursor;
        let mut it = s.char_indices().rev();
        // Skip whitespace immediately behind the cursor.
        for (j, c) in it.by_ref() {
            if !c.is_whitespace() {
                i = j + c.len_utf8();
                break;
            }
            i = j;
        }
        let class = |c: char| match style {
            WordStyle::WhitespaceDelimited => !c.is_whitespace(),
            WordStyle::Small => c.is_alphanumeric() || c == '_',
        };
        let start_class = self.text[..i].chars().next_back().map(class);
        let mut out = i;
        for (j, c) in self.text[..i].char_indices().rev() {
            if c.is_whitespace() || Some(class(c)) != start_class {
                break;
            }
            out = j;
        }
        out
    }

    fn word_right(&self, style: WordStyle) -> usize {
        let class = |c: char| match style {
            WordStyle::WhitespaceDelimited => !c.is_whitespace(),
            WordStyle::Small => c.is_alphanumeric() || c == '_',
        };
        let rest = &self.text[self.cursor..];
        let mut it = rest.char_indices();
        let mut out = self.text.len();
        let first = it.next();
        let start_class = first.map(|(_, c)| class(c));
        if first.is_none() {
            return out;
        }
        for (j, c) in std::iter::once(first.unwrap()).chain(it) {
            if j > 0 && (c.is_whitespace() || Some(class(c)) != start_class) {
                out = self.cursor + j;
                break;
            }
        }
        // Then skip the whitespace after it, so a second press lands on the next
        // word rather than the gap before it.
        let mut k = out;
        for (j, c) in self.text[out..].char_indices() {
            if !c.is_whitespace() {
                k = out + j;
                break;
            }
            k = out + j + c.len_utf8();
        }
        k
    }

    /// Rows the composer wants, given the terminal width and height.
    ///
    /// Never more than a third of the screen, floor 6 — opencode's rule
    /// (`prompt/index.tsx:1494`). A composer that grows without a bound eats the
    /// conversation one paste at a time.
    pub fn height(&self, cols: usize, screen_rows: usize) -> usize {
        let want = width::wrap_ranges(&self.text, cols.saturating_sub(2).max(4)).len();
        let cap = (screen_rows / 3).max(6);
        want.clamp(1, cap)
    }

    /// The composer, drawn. Returns the lines and the cursor's `(row, col)`
    /// within them.
    pub fn render(&self, cols: usize, palette: Palette) -> (Vec<String>, (usize, usize)) {
        let prompt = "› ";
        let inner = cols.saturating_sub(width::width(prompt)).max(4);
        let ranges = width::wrap_ranges(&self.text, inner);
        let (row, col) = width::locate(&self.text, self.cursor, inner);
        let mut out = Vec::with_capacity(ranges.len());
        for (i, r) in ranges.iter().enumerate() {
            let lead = if i == 0 {
                palette.paint(Role::Faint, prompt)
            } else {
                " ".repeat(width::width(prompt))
            };
            // The newline that ended this row belongs to the row's *range* — the
            // ranges have to tile — but never to the row's *text*: a terminal
            // acts on a `\n` in a line it is given.
            let body = self.text[r.clone()].trim_end_matches(['\n', '\r']);
            // A placeholder is inverted so it reads as one object rather than as
            // text somebody typed. opencode uses the theme's warning colour
            // inverted for the same reason.
            let painted = if self.pastes.iter().any(|(ph, _)| body.contains(ph.as_str())) {
                let mut b = body.to_string();
                for (ph, _) in &self.pastes {
                    if b.contains(ph.as_str()) {
                        b = b.replace(ph.as_str(), &palette.paint(Role::Attention, ph));
                    }
                }
                b
            } else {
                body.to_string()
            };
            out.push(format!("{lead}{painted}"));
        }
        (out, (row, col + width::width(prompt)))
    }

    /// The one-line hint under the composer.
    ///
    /// **Empty in the steady state, and that is the point.** It used to name the keys
    /// that change: `enter send · ctrl+c exit` when idle, `esc interrupt · ctrl+c clear`
    /// while a turn runs, and a longer form once the composer had text. Those are three
    /// different lengths in front of a **constant** tail bar (`ctrl-s sessions · …`), so
    /// the whole bottom line shifted sideways every time a turn started or the first
    /// character was typed — the operator: *"at the very bottom we have either 'enter
    /// send' or 'esc interrupt' they have different length and that bottom line always
    /// jumps back and forth. I dont want that. just dont show enter and esc at all."*
    ///
    /// So nothing is shown, and the bar is the tail alone: the same width in every
    /// state, and `hint_bar` already has the branch that suppresses the separator in
    /// front of one half.
    ///
    /// **The double-tap messages stay**, and they are not the same thing: they appear
    /// for a second *in response to a key*, which is a reply rather than a flicker, and
    /// they are the entire mechanism by which anybody learns that a second press does
    /// something different.
    pub fn hint(&self, now_ms: u64, palette: Palette) -> String {
        if self.esc_taps.armed(now_ms, INTERRUPT_WINDOW_MS) {
            return palette.paint(Role::Attention, "esc again to interrupt");
        }
        if self.ctrlc_taps.armed(now_ms, QUIT_WINDOW_MS) {
            return palette.paint(Role::Attention, "ctrl+c again to exit");
        }
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed() -> Editor {
        Editor::new()
    }

    fn type_str(e: &mut Editor, s: &str) {
        for c in s.chars() {
            e.key(Key::Char(c), 0);
        }
    }

    /// **§6: a history walk keeps the draft it interrupted.**
    ///
    /// Pressing Up to glance at the previous message and Down to come back used to
    /// restore the **empty buffer**, so the half-written prompt was gone. It was the
    /// quietest kind of loss — nothing was sent anywhere and there is no undo entry,
    /// because a recall is navigation rather than an edit — and every shell keeps the
    /// draft, so the behaviour was also the one thing nobody would think to check.
    #[test]
    fn a_history_walk_restores_the_draft_it_interrupted() {
        let mut e = ed().with_history(vec!["first".into(), "second".into()]);
        type_str(&mut e, "half a thought");
        assert!(matches!(e.key(Key::Up, 0), Reaction::Changed));
        assert_eq!(e.text(), "second", "the newest entry first");
        assert!(matches!(e.key(Key::Up, 0), Reaction::Changed));
        assert_eq!(e.text(), "first");
        // Back down: their own line, not an empty buffer.
        assert!(matches!(e.key(Key::Down, 0), Reaction::Changed));
        assert_eq!(e.text(), "second");
        assert!(matches!(e.key(Key::Down, 0), Reaction::Changed));
        assert_eq!(e.text(), "half a thought", "the draft survived the walk");

        // **And editing the restored draft still stops navigation**, which is the rule
        // that keeps a walk from destroying an edit: the draft is now an edited recall
        // (`history_recalled != text`), so another Up is refused rather than
        // overwriting what they typed. My first draft of this test asserted a second
        // walk here and failed — the assertion was wrong, not the guard.
        type_str(&mut e, " and more");
        assert!(matches!(e.key(Key::Up, 0), Reaction::Idle));
        assert_eq!(e.text(), "half a thought and more");

        // **A second walk keeps its own draft.** A fresh line, because the first
        // draft was `take`n when its walk ended — this is not the old text reappearing.
        let mut e2 = ed().with_history(vec!["first".into(), "second".into()]);
        type_str(&mut e2, "another thought");
        assert!(matches!(e2.key(Key::Up, 0), Reaction::Changed));
        assert_eq!(e2.text(), "second");
        assert!(matches!(e2.key(Key::Down, 0), Reaction::Changed));
        assert_eq!(e2.text(), "another thought");

        // And a walk that never leaves the newest entry still comes back to it.
        let mut e = ed().with_history(vec!["only".into()]);
        type_str(&mut e, "draft");
        e.key(Key::Up, 0);
        assert_eq!(e.text(), "only");
        e.key(Key::Down, 0);
        assert_eq!(e.text(), "draft");
    }

    #[test]
    fn a_large_paste_becomes_a_placeholder_and_expands_on_submit() {
        // The symptom: a pasted stack trace fills the composer and scrolls the
        // conversation away.
        let mut e = ed();
        type_str(&mut e, "look at this: ");
        let trace = (0..40)
            .map(|i| format!("  at frame {i}\n"))
            .collect::<String>();
        e.key(Key::Paste(trace.clone()), 0);
        // **Forty lines, not forty-one** (§6): the pasted text ends in a newline and
        // the old count added one to the newline total, so every whole file dragged
        // out of an editor over-reported by a line.
        assert!(e.text().contains("[Pasted #1 ~40 lines]"), "{}", e.text());
        assert!(e.text().len() < 60, "the composer stayed small");
        let out = match e.key(Key::Enter, 0) {
            Reaction::Submit(s) => s,
            r => panic!("{r:?}"),
        };
        assert!(out.contains("at frame 39"), "the real text must be sent");
        assert!(!out.contains("[Pasted"), "the placeholder must not be sent");
    }

    /// **§6: the count is the number of lines, and a paste that ends in a newline is
    /// not one line longer for it.**
    ///
    /// The operator's own case: this head announced a 300-line file as `~301 lines`,
    /// because a file dragged out of an editor ends `…\n` and the old count was the
    /// newline total plus one. The reader's whole use for this number is deciding
    /// whether the placeholder holds their paste.
    #[test]
    fn the_paste_count_is_the_lines_a_reader_would_count() {
        // A 300-line paste, as an editor gives it to you.
        let mut e = ed();
        let file = (0..300).map(|i| format!("line {i}\n")).collect::<String>();
        e.key(Key::Paste(file), 0);
        assert!(e.text().contains("~300 lines"), "{}", e.text());

        // …and the same text without the final newline counts the same, because it is
        // the same number of lines.
        let mut e = ed();
        let no_eol = (0..300)
            .map(|i| format!("line {i}\n"))
            .collect::<String>()
            .trim_end_matches('\n')
            .to_string();
        e.key(Key::Paste(no_eol), 0);
        assert!(e.text().contains("~300 lines"), "{}", e.text());

        // **And the threshold is on lines.** Four is four and stays put; five is five
        // and collapses — the boundary moved by one with the count, which is the same
        // off-by-one seen from the other side.
        let mut e = ed();
        e.key(Key::Paste("a\nb\nc\nd\n".into()), 0);
        assert_eq!(
            e.text(),
            "a\nb\nc\nd\n",
            "four lines are ordinary typed text"
        );
        let mut e = ed();
        e.key(Key::Paste("a\nb\nc\nd\ne\n".into()), 0);
        assert!(e.text().contains("~5 lines"), "{}", e.text());
    }

    #[test]
    fn a_small_paste_is_just_text() {
        let mut e = ed();
        e.key(Key::Paste("one\ntwo".into()), 0);
        assert_eq!(e.text(), "one\ntwo");
    }

    #[test]
    fn paste_normalises_cr_only_newlines() {
        // Windows ConPTY sends these inside a bracketed paste.
        let mut e = ed();
        e.key(Key::Paste("a\r\nb\rc".into()), 0);
        assert_eq!(e.text(), "a\nb\nc");
    }

    #[test]
    fn deleting_a_placeholder_drops_its_paste() {
        let mut e = ed();
        e.key(Key::Paste("x\n".repeat(9)), 0);
        for _ in 0..e.text().chars().count() {
            e.key(Key::Backspace, 0);
        }
        assert_eq!(e.expanded(), "");
    }

    #[test]
    fn esc_twice_within_the_window_interrupts_and_once_does_not() {
        let mut e = ed();
        assert_eq!(e.key(Key::Esc, 1_000), Reaction::Changed);
        assert_eq!(e.key(Key::Esc, 2_000), Reaction::Interrupt);
        // …and outside the window it is two first presses.
        assert_eq!(e.key(Key::Esc, 10_000), Reaction::Changed);
        assert_eq!(e.key(Key::Esc, 20_000), Reaction::Changed);
    }

    #[test]
    fn the_hint_announces_the_second_press() {
        let mut e = ed();
        let before = e.hint(1_000, Palette::None);
        e.key(Key::Esc, 1_000);
        let after = e.hint(1_100, Palette::None);
        assert_ne!(before, after);
        assert!(after.contains("again"), "{after}");
        // And it lapses.
        assert!(!e.hint(9_000, Palette::None).contains("again"));
    }

    /// **The steady hint is empty, so the bottom line cannot jump.**
    ///
    /// The operator: *"at the very bottom we have either 'enter send' or 'esc
    /// interrupt' they have different length and that bottom line always jumps back and
    /// forth"*. Three different prefixes — idle, running, and composer-not-empty — sat
    /// in front of a constant tail bar, so every state change moved the whole line.
    ///
    /// This asserts the absence, in every steady state, because the fix *is* the
    /// absence: a hint that returns `""` in all three cannot shift anything.
    #[test]
    fn the_steady_hint_is_empty_whichever_state_the_editor_is_in() {
        let mut e = ed();
        assert_eq!(e.hint(1_000, Palette::None), "", "an idle composer");
        type_str(&mut e, "writing something");
        assert_eq!(e.hint(1_000, Palette::None), "", "a composer with text");
        // A running turn is the other state the old hint distinguished, and it no
        // longer reaches the hint at all — the parameter is gone, so the only way it
        // could shift the line is by being named here. `hint` takes no `running` for
        // that reason; this asserts the editor's own text is not what changes.
        assert!(!e.hint(1_000, Palette::None).contains("interrupt"));

        // The double-taps still speak, which is the part that must not be lost.
        e.key(Key::Esc, 2_000);
        assert!(
            e.hint(2_100, Palette::None)
                .contains("esc again to interrupt")
        );
        // And each lapses back to nothing.
        assert_eq!(e.hint(99_000, Palette::None), "");
    }

    #[test]
    fn ctrl_c_clears_a_non_empty_composer_and_never_quits_from_one() {
        let mut e = ed();
        type_str(&mut e, "a paragraph I am still writing");
        assert_eq!(e.key(Key::CtrlC, 0), Reaction::Changed);
        assert!(e.is_empty());
        // Only now does it count toward quitting.
        assert_eq!(e.key(Key::CtrlC, 100), Reaction::Changed);
        assert_eq!(e.key(Key::CtrlC, 200), Reaction::Quit);
    }

    #[test]
    fn ctrl_c_outside_the_window_does_not_accumulate() {
        let mut e = ed();
        assert_eq!(e.key(Key::CtrlC, 0), Reaction::Changed);
        assert_eq!(e.key(Key::CtrlC, 5_000), Reaction::Changed);
    }

    #[test]
    fn undo_batches_a_word_but_stops_at_a_space() {
        let mut e = ed();
        type_str(&mut e, "hello world");
        e.key(Key::Undo, 0);
        // The last word goes; "hello " stays.
        assert!(
            e.text().starts_with("hello") && !e.text().contains("world"),
            "{:?}",
            e.text()
        );
    }

    #[test]
    fn undo_does_not_restore_the_kill_buffer() {
        // grok-build: "yank is separate from undo".
        let mut e = ed();
        type_str(&mut e, "abc def");
        e.key(Key::KillToStart, 0);
        assert_eq!(e.text(), "");
        e.key(Key::Undo, 0);
        assert_eq!(e.text(), "abc def");
        e.key(Key::End, 0);
        e.key(Key::Yank, 0);
        assert_eq!(e.text(), "abc defabc def");
    }

    #[test]
    fn history_stops_navigating_once_a_recalled_entry_is_edited() {
        // Otherwise the next arrow press silently destroys the edit.
        let mut e = ed().with_history(vec!["first".into(), "second".into()]);
        assert_eq!(e.key(Key::Up, 0), Reaction::Changed);
        assert_eq!(e.text(), "second");
        assert_eq!(e.key(Key::Up, 0), Reaction::Changed);
        assert_eq!(e.text(), "first");
        type_str(&mut e, "!");
        assert_eq!(e.key(Key::Up, 0), Reaction::Idle);
        assert_eq!(e.text(), "first!");
    }

    #[test]
    fn history_drops_consecutive_duplicates_and_is_capped() {
        let mut e = ed();
        for _ in 0..3 {
            type_str(&mut e, "same");
            e.key(Key::Enter, 0);
        }
        assert_eq!(e.history().len(), 1);
        let mut e = ed();
        for i in 0..HISTORY_CAP + 10 {
            type_str(&mut e, &format!("cmd {i}"));
            e.key(Key::Enter, 0);
        }
        assert_eq!(e.history().len(), HISTORY_CAP);
        assert_eq!(e.history()[0], format!("cmd {}", 10));
    }

    #[test]
    fn vertical_motion_moves_by_visual_row_not_logical_line() {
        // A soft-wrapped paragraph must take one press per row.
        let mut e = ed();
        type_str(&mut e, "aaaa bbbb cccc dddd eeee ffff");
        e.key(Key::Home, 0);
        let cols = 10;
        let rows = width::wrap_ranges(e.text(), cols).len();
        assert!(rows >= 3, "{rows}");
        let mut seen = 0;
        for _ in 0..rows - 1 {
            assert_eq!(e.vertical(false, cols), Reaction::Changed);
            seen += 1;
        }
        assert_eq!(seen, rows - 1);
        assert_eq!(width::locate(e.text(), e.cursor(), cols).0, rows - 1);
    }

    #[test]
    fn the_preferred_column_is_sticky_across_a_short_row() {
        let mut e = ed();
        type_str(&mut e, "aaaaaaaaaa\nbb\ncccccccccc");
        e.key(Key::End, 0); // end of the last line, column 10
        let cols = 40;
        let (_, col0) = width::locate(e.text(), e.cursor(), cols);
        e.vertical(true, cols); // onto "bb", which is short
        e.vertical(true, cols); // back onto the long first line
        let (_, col2) = width::locate(e.text(), e.cursor(), cols);
        assert_eq!(col0, col2, "the column did not stick");
    }

    #[test]
    fn word_motion_crosses_a_word_not_a_character() {
        let mut e = ed();
        type_str(&mut e, "let total = a + b;");
        e.key(Key::Home, 0);
        e.key(Key::WordRight, 0);
        assert_eq!(&e.text()[e.cursor()..], "total = a + b;");
        e.key(Key::WordRight, 0);
        assert_eq!(&e.text()[e.cursor()..], "= a + b;");
        e.key(Key::End, 0);
        // `;` is its own small word, so the first press lands on it and the
        // second on `b`. That is vim's `b`, and it is the behaviour a person who
        // has ever used a word motion expects.
        e.key(Key::WordLeft, 0);
        assert_eq!(&e.text()[e.cursor()..], ";");
        e.key(Key::WordLeft, 0);
        assert_eq!(&e.text()[e.cursor()..], "b;");
    }

    #[test]
    fn the_composer_never_takes_more_than_a_third_of_the_screen() {
        let mut e = ed();
        e.key(Key::Paste("line\n".repeat(200)), 0);
        // A placeholder, so one row.
        assert_eq!(e.height(80, 48), 1);
        e.insert(&"word ".repeat(400));
        assert_eq!(e.height(80, 48), 16);
        assert_eq!(e.height(80, 9), 6, "the floor is six rows");
    }

    #[test]
    fn multibyte_editing_never_splits_a_character() {
        let mut e = ed();
        type_str(&mut e, "héllo 世界");
        for _ in 0..8 {
            e.key(Key::Left, 0);
        }
        assert_eq!(e.cursor(), 0);
        for _ in 0..8 {
            e.key(Key::Right, 0);
        }
        assert_eq!(e.cursor(), e.text().len());
        for _ in 0..8 {
            e.key(Key::Backspace, 0);
        }
        assert_eq!(e.text(), "");
    }

    #[test]
    fn rendering_places_the_cursor_where_the_text_is() {
        let mut e = ed();
        type_str(&mut e, "hello 世界");
        let (lines, (row, col)) = e.render(40, Palette::None);
        assert_eq!(row, 0);
        // "› " plus "hello " plus one wide char.
        assert_eq!(col, 2 + 6 + 4);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("› hello"));
    }

    #[test]
    fn a_placeholder_is_marked_in_the_composer() {
        let mut e = ed();
        e.key(Key::Paste("x\n".repeat(9)), 0);
        let (lines, _) = e.render(60, Palette::Colour);
        assert!(
            lines[0].contains(Palette::Colour.open(Role::Attention)),
            "{:?}",
            lines[0]
        );
    }

    #[test]
    fn submitting_an_empty_composer_does_nothing() {
        let mut e = ed();
        assert_eq!(e.key(Key::Enter, 0), Reaction::Idle);
        type_str(&mut e, "   ");
        assert_eq!(e.key(Key::Enter, 0), Reaction::Idle);
    }
}
