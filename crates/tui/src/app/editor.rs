//! The editor pane: rano, inside the head. A click on an edit row or `ctrl-]` opens it with the
//! change drawn over the text, and there the pane owns every key, paste and mouse report inside its
//! rectangle. rano's own exit closes the pane; it never quits the head.

use super::*;
use letibot_sessionlog::view::CallState;
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

/// **The chord that closes the pane** - bound to rano's own `exit`, not to a host command, so
/// it asks about unsaved edits exactly as `^X` does. `ctrl-q` is the head's jobs pane while the
/// composer has the keys; the two cannot collide, because only one side has them.
const CLOSE: &str = "C-q";

/// How long the head's own read waits when nothing arrives (`Terminal::events`, `VTIME=1`).
/// The pane polls first when rano wants to be ticked sooner than this.
pub(crate) const READ_PACE: Duration = Duration::from_millis(100);

/// A place in a file that a row is about: what a click on that row, or `ctrl-]`, opens. Only a
/// finished `edit`/`write` whose excerpt reached this head is one, because that excerpt is what
/// makes the open a review at the file's own line numbers rather than a bare jump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileRef {
    /// As the row names it: relative to the session's workspace, or absolute.
    pub(crate) path: String,
    /// The line to put the reader on, 1-based: the first changed line of an edit's new file, or
    /// the first line of a file opened to be read.
    pub(crate) line: usize,
    /// **The change, when the row is about one.** `None` is a row that names a file and nothing
    /// else, and it is what decides the view: a change opens on rano's review, no change opens
    /// on the text itself, from the top.
    pub(crate) change: Option<Change>,
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
            change: Some(change),
        }
    }

    /// A file with no change on it: its first line, to be read rather than reviewed - what the
    /// standing-notes pane's Enter opens. A `line` invented here would be this head guessing which
    /// part of somebody's note matters.
    pub(crate) fn file(path: &str) -> FileRef {
        FileRef {
            path: path.to_string(),
            line: 1,
            change: None,
        }
    }
}

/// **The pane's state**: rano itself, who has the keyboard, and the last frame it drew.
pub struct EditorPane {
    pub(crate) ed: Rano,
    /// The pane has the keyboard. False is the composer's turn, with the pane still drawn.
    pub(crate) focused: bool,
    /// Put away, not closed - `ctrl-e` on an empty prompt. The conversation has its rectangle back
    /// and rano keeps every buffer, cursor and undo; the same key brings it back. Typing in the
    /// prompt never hides it, because the editor is just a pane: only the key does.
    pub(crate) hidden: bool,
    /// The rectangle last given to rano, in terminal cells: mouse reports are hit-tested
    /// against it, and rano maps them through the same one.
    pub(crate) area: Area,
    /// The sends rano made since the head last looked - `on_send` pushes here, because the
    /// callback cannot reach the composer: it runs inside rano, which the head has lent out.
    sends: Rc<RefCell<Vec<SendEvent>>>,
    /// The cells rano draws into, reused across frames.
    pub(crate) buf: rano::render::Buffer,
    /// The last frame's rows, served again while nothing changed.
    pub(crate) rows: Vec<String>,
    /// Where the terminal's caret goes, `(row, column)`, from the last frame - `None` when rano
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
        // No nano chrome: the head frames the pane in its own look - the file in the top edge,
        // rano's keys in the bottom one (`App::editor_rows`) - so rano draws only the text and its
        // status row.
        ed.set_chrome(rano::editor::Chrome::BARE);
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
            hidden: false,
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
    /// Is the editor pane on the screen: open, and not covered by a `!term` pane or the diff popup
    /// that `main_area` draws over it. Asking about the `!term` pane alone would hand rano the keys
    /// and the mouse of a screen showing the popup.
    pub fn editor_drawn(&self) -> bool {
        self.drawn() == Drawn::Editor
    }

    /// **Does the editor pane have the keyboard.**
    pub fn editor_focused(&self) -> bool {
        self.editor_drawn() && self.edit_pane.as_ref().is_some_and(|p| p.focused)
    }

    /// A row's path, as a file this head can open: absolute as it is, relative to the session's
    /// workspace otherwise - which is what an edit's excerpt is relative to.
    pub(crate) fn resolve_path(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() || self.wiring.workspace.is_empty() {
            p.to_path_buf()
        } else {
            Path::new(&self.wiring.workspace).join(p)
        }
    }

    /// Open the pane on `f` - on its change, or on the file itself when the row carries none - with
    /// the keyboard. The pane is made once and kept, so a second open is rano switching to that
    /// file and costs nobody the file they were reading; a failed open leaves no pane standing.
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
        // Two kinds of place, one door: a change opens rano's review view, a bare path opens the
        // text from its first line. Either way the reader gets the FILE, editable - which is why
        // the note pane comes through here rather than rendering what the index carries.
        let opened = match &f.change {
            Some(change) => pane.ed.open_review(&path, change.clone()),
            None => pane.ed.open_at(&path, f.line, None),
        };
        match opened {
            Ok(()) => {
                pane.hidden = false;
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
    /// change - and when this conversation has changed nothing, say so rather than nothing.
    pub(crate) fn editor_chord(&mut self) -> Option<Action> {
        if let Some(p) = self.edit_pane.as_mut() {
            p.hidden = false;
            p.focused = true;
            p.dirty = true;
        } else if let Some(f) = self.newest_change() {
            self.open_in_editor(&f);
        } else {
            self.say(
                "no edit or write in this conversation to open — ctrl-e on an empty prompt \
                 opens the editor on any file",
            );
        }
        None
    }

    /// Whether `ctrl-e` is the editor right now: the prompt is empty and the conversation is what
    /// is in front of the operator. Every card, pane and listing reads `End` as its own key, so an
    /// open editor pane is the one surface that does not count against it - `ctrl-e` goes back in.
    pub(crate) fn ctrl_e_opens_editor(&self) -> bool {
        self.editor.text().is_empty()
            && self.mode_confirm.is_none()
            && self.todo_draft.is_none()
            && self.secret.is_none()
            && self.term_ask.is_none()
            && self.prompt.is_none()
            && self.key_ask.is_none()
            && self.diff_popup.is_none()
            && self.payload_sel.is_none()
            && !self.pane_open()
            // **No surface on the stack**, open or covered - every one of them reads `End` as its
            // own key, and this used to be a list of eleven fields that the standing pane was
            // missing from.
            && self.surfaces.is_empty()
    }

    /// `ctrl-e` on an empty prompt: the editor on any file, and away again. With no pane rano opens
    /// its own `Open:` prompt, so nothing has to have changed first; with a pane the key toggles it,
    /// because the key only reaches here from the composer - inside rano `ctrl-e` is rano's.
    pub(crate) fn open_editor(&mut self) {
        if let Some(p) = self.edit_pane.as_mut() {
            p.hidden = !p.hidden;
            p.focused = !p.hidden;
            p.dirty = true;
            return;
        }
        let area = self.edit_area;
        let fresh = self.edit_pane.is_none();
        let pane = self.edit_pane.get_or_insert_with(EditorPane::new);
        if !area.is_empty() {
            pane.set_area(area);
        }
        // Asked the way a person asks: F8 is rano's `open-file`, in its global map, and a key is
        // the one door into rano's commands its API keeps public.
        if fresh {
            let _ = pane
                .ed
                .handle_key(KeyEvent::plain(rano::term::KeyCode::F(8)));
        }
        pane.focused = true;
        pane.dirty = true;
    }

    /// **The newest file this conversation changed**, for `ctrl-]`: the live turn's finished
    /// calls first (their rows may not have landed yet), then the rows, newest first.
    pub(crate) fn newest_change(&self) -> Option<FileRef> {
        let live = self.turn.as_ref().and_then(|t| {
            t.calls.iter().rev().find_map(|c| match &c.state {
                letibot_sessionlog::view::CallState::Finished { edit: Some(e), .. }
                    if card::Verb::of(crate::ui::drawn_as(&c.name, Some(e))).is_an_edit() =>
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
    /// head holds - from the row itself, or from what this head watched the call do.
    pub(crate) fn change_of_row(&self, k: usize) -> Option<FileRef> {
        let it = self.items.get(k)?;
        let Some(letibot_transcript::TranscriptItem::ToolResult { name, edit, .. }) = &it.item
        else {
            return None;
        };
        let edit = self.call_edits.get(&it.item_id).or(edit.as_ref());
        if !card::Verb::of(crate::ui::drawn_as(name, edit)).is_an_edit() {
            return None;
        }
        edit.map(FileRef::of)
    }

    /// Open what a call the turn is still drawing has behind it: deliberately the same act the
    /// call's own row performs once it lands, because a click that answers nothing reads as a head
    /// that stopped listening. A call that has not returned has no body, so the running case says so.
    pub(crate) fn open_call(&mut self, call_id: &str) {
        // **The call, copied out** - the frame's record names it by id, and everything below
        // writes to the head.
        let (name, state) = match self
            .turn
            .as_ref()
            .and_then(|t| t.calls.iter().find(|c| c.call_id == call_id))
        {
            Some(c) => (c.name.clone(), c.state.clone()),
            // The call this frame drew is not in the turn any more - a new turn started between
            // the frame and the click. Nothing to open and nothing to explain.
            None => return,
        };
        let edit = match &state {
            CallState::Finished { edit, .. } => edit.clone(),
            _ => None,
        };
        // The verb the card was DRAWN as, so this asks the same question the drawing did.
        let name = crate::ui::drawn_as(&name, edit.as_ref()).to_string();
        // The change is on the card already, and the popup is the whole file with it - the same
        // place a click on the landed row opens, so the two cannot disagree about a change's door.
        // The gate is `change_of_row`'s: an edit verb, and an excerpt to show.
        if let Some(e) = edit.as_ref().filter(|_| card::Verb::of(&name).is_an_edit()) {
            let f = FileRef::of(e);
            self.open_diff_popup(&f);
            return;
        }
        match state {
            CallState::Proposed | CallState::Running => self.say(&format!(
                "nothing to open yet — `{name}` is still running, and a call's body arrives when \
                 it returns"
            )),
            CallState::Finished { .. } => {
                self.say(&format!("nothing to open — `{name}` changed no file"))
            }
        }
    }

    /// **How long the head's loop may wait for input** before rano wants another tick - `None`
    /// with no pane on the screen, when the head's own pace is the only one.
    pub fn editor_wait(&self) -> Option<Duration> {
        self.edit_pane
            .as_ref()
            .filter(|_| self.editor_drawn())
            .map(|p| p.ed.next_wakeup())
    }

    /// **rano's work between events**, once a pass of the head's loop and before the frame  -
    /// rano's own rule: a host that draws must tick first. Then whatever rano decided in the
    /// meantime: a send to take, an exit to honour.
    pub fn editor_tick(&mut self, now: Instant) {
        if let Some(p) = self.edit_pane.as_mut() {
            p.dirty |= p.ed.tick(now);
        }
        self.after_editor();
    }

    /// What the terminal said, routed: the pane's share goes to rano, the rest comes back as this
    /// head's keys, in order, plus whether rano took anything - a read rano consumed is not one whose
    /// bytes a `!term` pane may scan for its way out. With a `!term` pane drawn nothing is routed.
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

    /// **What rano decided, acted on**: an exit closes the pane - and only the pane - and each
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

    /// A send from rano, into the composer: the reference, then the keyboard. Inserted at the
    /// composer's cursor so a draft keeps its words and gains the reference, and nothing is sent -
    /// the instruction is the operator's to write.
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

    /// How a send reads in the composer: `path:line` with no selection, or `path:start-end` and the
    /// selection fenced. The path is the workspace's relative one where the file is inside it, and a
    /// buffer with unsaved edits is marked, because the model reading the file will not see them.
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
    /// canonicalised, because rano's path is and a workspace spelled through a symlink would
    /// otherwise never be a prefix of it. No workspace is the directory this head runs in.
    pub(crate) fn workspace_relative(&self, p: &Path) -> String {
        let ws = &self.wiring.workspace;
        let root = if ws.is_empty() {
            std::env::current_dir().ok()
        } else {
            Some(PathBuf::from(ws))
        };
        if let Some(root) = root {
            let root = std::fs::canonicalize(&root).unwrap_or(root);
            if let Ok(rest) = p.strip_prefix(&root) {
                return rest.display().to_string();
            }
        }
        p.display().to_string()
    }
}
