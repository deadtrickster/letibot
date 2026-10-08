//! **The editor pane: rano, inside the head.**
//!
//! The operator's words for what this is: *"rano blended with letibot — i review in rano, i
//! send for review from it, and i can go to rano from diffs, writes — either via shortcut or
//! mouse click"*. So the pane is three movements and one rectangle:
//!
//! - **Going there.** A click on an edit or write row of the conversation, or `ctrl-]` for the
//!   newest one, opens rano on that file — on the change, with the change drawn over the text
//!   as rano's own review view ([`rano::editor::Editor::open_review`]). See [`FileRef`] for
//!   what a row has to carry to be one you can click.
//! - **Reading and editing there.** The pane takes the conversation's rectangle — the header,
//!   the composer and the hint bar keep their rows, exactly as a `!term` pane leaves them — and
//!   while it has the keyboard every key, paste and mouse report inside it is rano's.
//! - **Coming back with something to say.** rano's `alt-s` sends the reader's place, and this
//!   head's answer to a send is to put a reference to it in the composer — `path:line`, or
//!   `path:start-end` and the selected text fenced — and move the keyboard there. The operator
//!   writes the instruction and sends it with Enter as usual; **nothing is submitted for them**.
//!
//! # Who has the keyboard
//!
//! One side at a time, and `ctrl-]` crosses — from the composer it opens the pane (or goes back
//! into it), from the pane it comes back. rano is told about the chord through a host keymap
//! ([`rano::editor::Editor::push_keymap`]), so it reaches this head over an open prompt or a
//! diff view as well as over the text. With the composer focused the head's keys are exactly
//! the keys it always had.
//!
//! # Leaving
//!
//! rano's own exit (`^X`, or `ctrl-q`, which the host keymap binds to the same command) closes
//! the pane — after rano has asked about unsaved edits, because that question is rano's and a
//! host that closed around it would lose work. `wants_quit` is how the pane learns rano is done;
//! it **never** quits the head.

use super::*;
use rano::editor::{Area, Editor as Rano, KeyOutcome};
use rano::keymap::Keymap;
use rano::review::Change;
use rano::send::SendEvent;
use rano::term::{Event, KeyEvent, MouseButton, MouseKind};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The host command rano hands back for `ctrl-]`: the keyboard goes to the composer.
pub(crate) const TO_COMPOSER: &str = "letibot-to-composer";

/// **The chord that crosses between the pane and the composer**, in rano's notation, for the
/// host keymap. The head's own half of it is [`Key::CtrlBracket`].
const CROSS: &str = "C-]";

/// **The chord that closes the pane** — bound to rano's own `exit`, not to a host command, so
/// it asks about unsaved edits exactly as `^X` does. `ctrl-q` is the head's jobs pane while the
/// composer has the keys; the two cannot collide, because only one side has them.
const CLOSE: &str = "C-q";

/// How long the head's own read waits when nothing arrives (`Terminal::events`, `VTIME=1`).
/// The pane polls first when rano wants to be ticked sooner than this.
pub(crate) const READ_PACE: Duration = Duration::from_millis(100);

/// **A place in a file that a row of the conversation is about** — what a click on that row,
/// or `ctrl-]`, opens.
///
/// Only rows that carry the change itself are places: a finished `edit` or `write` whose
/// before/after excerpt reached this head (`ToolEditExcerpt`). That excerpt is what makes the
/// open a *review* rather than a bare jump — it is the change, at the file's own line numbers —
/// and it is also what names the file and the line. A row without one is not a place, and a
/// click on it does what a click on it always did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileRef {
    /// As the row names it: relative to the session's workspace, or absolute.
    pub(crate) path: String,
    /// The first changed line of the new file, 1-based.
    pub(crate) line: usize,
    /// The change, for rano's review view.
    pub(crate) change: Change,
}

impl FileRef {
    /// The place an edit's excerpt is about.
    pub(crate) fn of(e: &letibot_sessionlog::event::ToolEdit) -> FileRef {
        let change = Change {
            before: e.before.clone(),
            after: e.after.clone(),
            before_start: e.before_start,
            after_start: e.after_start,
        };
        FileRef {
            path: e.path.clone(),
            line: change.first_changed_line(),
            change,
        }
    }
}

/// **The pane's state**: rano itself, who has the keyboard, and the last frame it drew.
pub struct EditorPane {
    pub(crate) ed: Rano,
    /// The pane has the keyboard. False is the composer's turn, with the pane still drawn.
    pub(crate) focused: bool,
    /// The rectangle last given to rano, in terminal cells: mouse reports are hit-tested
    /// against it, and rano maps them through the same one.
    pub(crate) area: Area,
    /// The sends rano made since the head last looked — `on_send` pushes here, because the
    /// callback cannot reach the composer: it runs inside rano, which the head has lent out.
    sends: Rc<RefCell<Vec<SendEvent>>>,
    /// The cells rano draws into, reused across frames.
    pub(crate) buf: rano::render::Buffer,
    /// The last frame's rows, served again while nothing changed.
    pub(crate) rows: Vec<String>,
    /// Where the terminal's caret goes, `(row, column)`, from the last frame — `None` when rano
    /// hides it (over a diff, a list or a help page).
    pub(crate) caret: Option<(usize, usize)>,
    /// Something changed since `rows` was drawn: a key, a tick that said so, a new area.
    pub(crate) dirty: bool,
    /// The palette `rows` were emitted under.
    pub(crate) palette: Option<rano::style::Palette>,
}

impl EditorPane {
    fn new() -> EditorPane {
        // **The operator's own rano config** in a live head, so the pane is the editor they
        // already use; the defaults under test, so no test reads a home directory.
        #[cfg(not(test))]
        let config = rano::config::load();
        #[cfg(test)]
        let config = rano::config::Config::default();
        let mut ed = Rano::new(rano::buffer::Buffer::new(), config);
        let mut keys = Keymap::new("letibot");
        keys.bind(CROSS, TO_COMPOSER);
        keys.bind(CLOSE, "exit");
        ed.push_keymap(keys);
        let sends: Rc<RefCell<Vec<SendEvent>>> = Rc::default();
        let queue = sends.clone();
        ed.on_send = Some(Box::new(move |e: &SendEvent| {
            queue.borrow_mut().push(e.clone());
            Ok("Sent to letibot's composer".into())
        }));
        EditorPane {
            ed,
            focused: true,
            area: Area::default(),
            sends,
            buf: rano::render::Buffer::empty(rano::render::Rect::new(0, 0, 0, 0)),
            rows: Vec::new(),
            caret: None,
            dirty: true,
            palette: None,
        }
    }

    /// Give rano its rectangle; true when it changed.
    pub(crate) fn set_area(&mut self, area: Area) -> bool {
        self.area = area;
        let moved = self.ed.set_area(area);
        self.dirty |= moved;
        moved
    }
}

impl App {
    /// **Is the editor pane on the screen** — open, and not covered by a `!term` pane, which
    /// takes the rectangle and the keyboard first.
    pub fn editor_drawn(&self) -> bool {
        self.edit_pane.is_some() && !self.pane_open()
    }

    /// **Does the editor pane have the keyboard.**
    pub fn editor_focused(&self) -> bool {
        self.editor_drawn() && self.edit_pane.as_ref().is_some_and(|p| p.focused)
    }

    /// A row's path, as a file this head can open: absolute as it is, relative to the session's
    /// workspace otherwise — which is what an edit's excerpt is relative to.
    pub(crate) fn resolve_path(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() || self.wiring.workspace.is_empty() {
            p.to_path_buf()
        } else {
            Path::new(&self.wiring.workspace).join(p)
        }
    }

    /// **Open the pane on `f`**, on its change, with the keyboard.
    ///
    /// The pane is made on first use and kept: a second open is rano switching to that file
    /// (or to the buffer that already shows it), with whatever else is open left as it was.
    /// A file that cannot be opened is said, and a pane made only for it is not left standing.
    pub(crate) fn open_in_editor(&mut self, f: &FileRef) {
        let path = self.resolve_path(&f.path);
        let fresh = self.edit_pane.is_none();
        let area = self.edit_area;
        let pane = self.edit_pane.get_or_insert_with(EditorPane::new);
        // **The rectangle first**, when a frame has already measured it: rano centres the
        // change in the view it has, and a review drawn for no width is a review redrawn a
        // tick later.
        if !area.is_empty() {
            pane.set_area(area);
        }
        match pane.ed.open_review(&path, f.change.clone()) {
            Ok(()) => {
                pane.focused = true;
                pane.dirty = true;
            }
            Err(why) => {
                if fresh {
                    self.edit_pane = None;
                }
                self.say(&format!("cannot open {}: {why}", path.display()));
            }
        }
    }

    /// **Close the pane**: the conversation has its rectangle back and the composer its keys.
    pub(crate) fn close_editor(&mut self) {
        self.edit_pane = None;
    }

    /// **`ctrl-]` from the composer side**: back into an open pane, or open one on the newest
    /// change — and when this conversation has changed nothing, say so rather than nothing.
    pub(crate) fn editor_chord(&mut self) -> Option<Action> {
        if let Some(p) = self.edit_pane.as_mut() {
            p.focused = true;
            p.dirty = true;
        } else if let Some(f) = self.newest_change() {
            self.open_in_editor(&f);
        } else {
            self.say("no edit or write in this conversation to open");
        }
        None
    }

    /// **The newest file this conversation changed**, for `ctrl-]`: the live turn's finished
    /// calls first (their rows may not have landed yet), then the rows, newest first.
    pub(crate) fn newest_change(&self) -> Option<FileRef> {
        let live = self.turn.as_ref().and_then(|t| {
            t.calls.iter().rev().find_map(|c| match &c.state {
                letibot_sessionlog::view::CallState::Finished { edit: Some(e), .. }
                    if card::Verb::of(&c.name).is_an_edit() =>
                {
                    Some(FileRef::of(e))
                }
                _ => None,
            })
        });
        live.or_else(|| {
            (0..self.items.len())
                .rev()
                .find_map(|k| self.change_of_row(k))
        })
    }

    /// **The place row `k` is about**, when it is a finished edit or write whose change this
    /// head holds — from the row itself, or from what this head watched the call do.
    pub(crate) fn change_of_row(&self, k: usize) -> Option<FileRef> {
        let it = self.items.get(k)?;
        let Some(letibot_transcript::TranscriptItem::ToolResult { name, edit, .. }) = &it.item
        else {
            return None;
        };
        if !card::Verb::of(name).is_an_edit() {
            return None;
        }
        self.call_edits
            .get(&it.item_id)
            .or(edit.as_ref())
            .map(FileRef::of)
    }

    /// **The file a screen row is about**, from the last frame's map — see `file_rows`.
    pub(crate) fn file_at_row(&self, y: u16) -> Option<FileRef> {
        self.file_rows
            .iter()
            .find(|(row, _)| *row == y as usize)
            .map(|(_, f)| f.clone())
    }

    /// **How long the head's loop may wait for input** before rano wants another tick — `None`
    /// with no pane on the screen, when the head's own pace is the only one.
    pub fn editor_wait(&self) -> Option<Duration> {
        self.edit_pane
            .as_ref()
            .filter(|_| self.editor_drawn())
            .map(|p| p.ed.next_wakeup())
    }

    /// **rano's work between events**, once a pass of the head's loop and before the frame —
    /// rano's own rule: a host that draws must tick first. Then whatever rano decided in the
    /// meantime: a send to take, an exit to honour.
    pub fn editor_tick(&mut self, now: Instant) {
        if let Some(p) = self.edit_pane.as_mut() {
            p.dirty |= p.ed.tick(now);
        }
        self.after_editor();
    }

    /// **What the terminal said, routed**: the pane's share goes to rano, the rest comes back
    /// as this head's keys, in order. Returns those keys, and whether rano took anything — a
    /// read rano consumed is not one whose bytes a `!term` pane may scan for its way out.
    ///
    /// # What is the pane's
    ///
    /// - **Every key and paste while it has the keyboard.** A key rano hands back as the
    ///   crossing chord moves the keyboard to the composer, and the rest of the read follows it.
    /// - **Every mouse report inside its rectangle**, focused or not: the wheel over the pane
    ///   scrolls the file, and a press there also takes the keyboard — clicking into the
    ///   editor is how a reader expects to get into it.
    /// - **A press outside it while it has the keyboard** gives the keyboard back to the
    ///   composer, and is then the head's click like any other.
    ///
    /// The terminal's facts about itself — focus, the background colour — are the head's
    /// always. With a `!term` pane drawn nothing is routed: that pane owns the keyboard.
    pub fn route(&mut self, events: Vec<Event>) -> (Vec<Key>, bool) {
        let mut keys = Vec::new();
        let mut took = false;
        for e in events {
            if self.editor_drawn()
                && let Some(p) = self.edit_pane.as_mut()
            {
                match &e {
                    Event::Mouse(m) if p.area.contains(m.x, m.y) => {
                        took = true;
                        if matches!(m.kind, MouseKind::Press(MouseButton::Left)) {
                            p.focused = true;
                        }
                        p.ed.handle_mouse(*m);
                        p.dirty = true;
                        continue;
                    }
                    Event::Mouse(m)
                        if p.focused && matches!(m.kind, MouseKind::Press(MouseButton::Left)) =>
                    {
                        p.focused = false;
                        p.dirty = true;
                    }
                    Event::Key(k) if p.focused => {
                        took = true;
                        self.editor_key(*k);
                        continue;
                    }
                    Event::Paste(s) if p.focused => {
                        took = true;
                        p.ed.paste_text(s);
                        p.dirty = true;
                        continue;
                    }
                    _ => {}
                }
            }
            match key_of(e) {
                // **The crossing chord acts at once**, so the keys after it in the same read go
                // where it sent the keyboard rather than where the keyboard was.
                Some(Key::CtrlBracket) => {
                    let _ = self.key(Key::CtrlBracket);
                }
                Some(k) => keys.push(k),
                None => {}
            }
        }
        (keys, took)
    }

    /// One key into rano, and what rano said back.
    fn editor_key(&mut self, k: KeyEvent) {
        let Some(p) = self.edit_pane.as_mut() else {
            return;
        };
        if let KeyOutcome::Host(name) = p.ed.handle_key(k)
            && name == TO_COMPOSER
        {
            p.focused = false;
        }
        p.dirty = true;
        self.after_editor();
    }

    /// **What rano decided, acted on**: an exit closes the pane — and only the pane — and each
    /// send lands in the composer.
    fn after_editor(&mut self) {
        let Some(p) = self.edit_pane.as_mut() else {
            return;
        };
        let sends: Vec<SendEvent> = p.sends.borrow_mut().drain(..).collect();
        if p.ed.wants_quit() {
            self.close_editor();
        }
        for s in sends {
            self.take_send(&s);
        }
    }

    /// **A send from rano, into the composer**: the reference, then the keyboard.
    ///
    /// Inserted where the composer's cursor is, so a draft already there keeps its words and
    /// gains the reference — and nothing is sent: the instruction is the operator's to write.
    pub(crate) fn take_send(&mut self, e: &SendEvent) {
        let text = self.reference(e);
        let draft = self.editor.text();
        let joint = match draft.chars().last() {
            Some(c) if !c.is_whitespace() => {
                if text.contains('\n') {
                    "\n"
                } else {
                    " "
                }
            }
            _ => "",
        };
        self.editor.insert(&format!("{joint}{text}"));
        if let Some(p) = self.edit_pane.as_mut() {
            p.focused = false;
            p.dirty = true;
        }
    }

    /// **How a send reads in the composer**: `path:line ` with no selection — the operator's
    /// words follow on the same line — and `path:start-end` then the selection fenced, with the
    /// operator's words to follow underneath, when there is one.
    ///
    /// The path is the workspace's relative one where the file is inside it, because that is
    /// the spelling the model's own tools use for it. A selection that ends at the start of a
    /// line does not include that line (rano's end is exclusive), so the range says so too. A
    /// buffer with edits rano has not saved is marked, because the model reading the file will
    /// not see them.
    pub(crate) fn reference(&self, e: &SendEvent) -> String {
        let path = match &e.path {
            Some(p) => self.workspace_relative(p),
            None => "(unsaved buffer)".to_string(),
        };
        let unsaved = if e.modified { " (unsaved edits)" } else { "" };
        match &e.selection {
            None => format!("{path}:{}{unsaved} ", e.cursor.line),
            Some(s) => {
                let last = if s.end.column == 1 && s.end.line > s.start.line {
                    s.end.line - 1
                } else {
                    s.end.line
                };
                let place = if last == s.start.line {
                    format!("{path}:{}", s.start.line)
                } else {
                    format!("{path}:{}-{last}", s.start.line)
                };
                let lang = e
                    .path
                    .as_deref()
                    .and_then(Path::extension)
                    .and_then(|x| x.to_str())
                    .unwrap_or("");
                let body = s.text.trim_end_matches('\n');
                format!("{place}{unsaved}\n```{lang}\n{body}\n```\n")
            }
        }
    }

    /// `p` relative to the workspace when it is inside it, as it is otherwise. Both sides are
    /// canonicalised, because rano's path is and a workspace spelled through a symlink
    /// (`/tmp` on macOS) would otherwise never be a prefix of it.
    fn workspace_relative(&self, p: &Path) -> String {
        let ws = &self.wiring.workspace;
        if !ws.is_empty() {
            let root = std::fs::canonicalize(ws).unwrap_or_else(|_| PathBuf::from(ws));
            if let Ok(rest) = p.strip_prefix(&root) {
                return rest.display().to_string();
            }
        }
        p.display().to_string()
    }
}
