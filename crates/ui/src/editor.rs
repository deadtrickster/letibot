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
        self.history_at = next;
        self.text = match next {
            Some(i) => self.history[self.history.len() - 1 - i].clone(),
            None => String::new(),
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
        let lines = s.matches('\n').count() + 1;
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
            let body = &self.text[r.clone()];
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
    /// It changes after the first Esc and the first Ctrl+C, which is the entire
    /// mechanism by which anybody learns the double-tap exists.
    pub fn hint(&self, running: bool, now_ms: u64, palette: Palette) -> String {
        if self.esc_taps.armed(now_ms, INTERRUPT_WINDOW_MS) {
            return palette.paint(Role::Attention, "esc again to interrupt");
        }
        if self.ctrlc_taps.armed(now_ms, QUIT_WINDOW_MS) {
            return palette.paint(Role::Attention, "ctrl+c again to exit");
        }
        let s = if running {
            "esc interrupt · ctrl+c clear"
        } else if self.text.is_empty() {
            "enter send · ctrl+c exit"
        } else {
            "enter send · alt+enter newline · ctrl+c clear"
        };
        palette.paint(Role::Faint, s)
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

    #[test]
    fn a_large_paste_becomes_a_placeholder_and_expands_on_submit() {
        // The symptom: a pasted stack trace fills the composer and scrolls the
        // conversation away.
        let mut e = ed();
        type_str(&mut e, "look at this: ");
        let trace = (0..40).map(|i| format!("  at frame {i}\n")).collect::<String>();
        e.key(Key::Paste(trace.clone()), 0);
        assert!(e.text().contains("[Pasted #1 ~41 lines]"), "{}", e.text());
        assert!(e.text().len() < 60, "the composer stayed small");
        let out = match e.key(Key::Enter, 0) {
            Reaction::Submit(s) => s,
            r => panic!("{r:?}"),
        };
        assert!(out.contains("at frame 39"), "the real text must be sent");
        assert!(!out.contains("[Pasted"), "the placeholder must not be sent");
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
        let before = e.hint(true, 1_000, Palette::None);
        e.key(Key::Esc, 1_000);
        let after = e.hint(true, 1_100, Palette::None);
        assert_ne!(before, after);
        assert!(after.contains("again"), "{after}");
        // And it lapses.
        assert!(!e.hint(true, 9_000, Palette::None).contains("again"));
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
        assert!(lines[0].contains(Palette::Colour.open(Role::Attention)), "{:?}", lines[0]);
    }

    #[test]
    fn submitting_an_empty_composer_does_nothing() {
        let mut e = ed();
        assert_eq!(e.key(Key::Enter, 0), Reaction::Idle);
        type_str(&mut e, "   ");
        assert_eq!(e.key(Key::Enter, 0), Reaction::Idle);
    }
}
