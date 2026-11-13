//! The editor pane: rano, inside the head. A click on an edit row or `ctrl-]` opens it with the
//! change drawn over the text, and there the pane owns every key, paste and mouse report inside its
//! rectangle. rano's own exit closes the pane; it never quits the head.

use super::*;
use letibot_sessionlog::view::CallState;
use rano::editor::{Area, Editor as Rano, KeyOutcome};
use rano::keymap::Keymap;
use rano::review::Change;
use rano::send::SendEvent;
use rano::style::Role;
use rano::term::{Event, KeyEvent, MouseButton, MouseKind};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The host command rano hands back for `ctrl-]`: the keyboard goes to the composer.
pub(crate) const TO_COMPOSER: &str = "letibot-to-composer";

/// The chord that crosses between the pane and the composer, in rano's notation, for the
/// host keymap. The head's own half of it is [`Key::CtrlBracket`].
const CROSS: &str = "C-]";

/// The chord that closes the pane. `C-x` is rano's own `exit`, either way it reaches - the head's
/// map only puts it ahead of `ctrl-q`, which still closes the pane and is no longer advertised, so
/// a reader who learned the old key keeps it.
const CLOSE: &str = "C-x";
/// The old close chord, bound below the new one and shown nowhere.
const CLOSE_LEGACY: &str = "C-q";

/// The narrowest an editor column may be. With `MAIN_MIN` and the border between them, a terminal
/// under `EDIT_MIN + 1 + MAIN_MIN` columns cannot hold the two side by side.
pub(crate) const EDIT_MIN: usize = 40;
/// Columns one `alt-<` / `alt->` moves the editor's border.
const EDIT_STEP: usize = 4;

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
    /// The change, when the row is about one. `None` is a row that names a file and nothing
    /// else, and it is what decides the view: a change opens on rano's review, no change opens
    /// on the text itself, from the top.
    pub(crate) change: Option<Change>,
}

impl FileRef {
    /// The place an edit's excerpt is about.
    pub(crate) fn of(e: &letibot_sessionlog::event::ToolEdit) -> FileRef {
        let change = super::marks::change_of(e);
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

/// How the pane sits in a frame, decided from the terminal's width and the operator's zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorFit {
    /// No pane for this frame: the conversation has the whole width.
    Off,
    /// Today's layout - the pane takes the conversation's rectangle, and `ctrl-]` flips which of
    /// the two shows. A terminal too narrow to share, or the operator's `alt-m`.
    Zoom,
    /// A column this many columns wide, on the left.
    Column(usize),
}

impl EditorFit {
    pub(crate) fn columns(self) -> usize {
        match self {
            EditorFit::Column(w) => w,
            _ => 0,
        }
    }
}

/// The pane's state: rano itself, who has the keyboard, and the last frame it drew.
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
    /// The quick asks rano handed over, for the same reason: `alt-k` fires inside rano's own key
    /// handling, and what a preset becomes is the head's to decide.
    asks: Rc<RefCell<Vec<rano::ask::AskEvent>>>,
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
    /// The file whose marks the pane has been handed, by canonical name: a switch to another file
    /// is what makes the conversation's marks arrive in the gutter. See [`App::mark_the_shown_file`].
    pub(crate) marked: Option<PathBuf>,
    /// The theme the pane draws under: the operator's own rano theme, resolved once when the pane is
    /// made. `None` inherits the head's, which is the look a pane with no rano theme already had.
    pub(crate) theme: Option<rano::theme::Theme>,
    /// Image ids the terminal holds for this pane, read off the bytes rano emitted: rano offers
    /// no way to free a markdown view's pictures, and the pane's end must not leak them.
    held: std::collections::BTreeSet<u32>,
    /// What rano's own theme file could not be used for, said once when the pane opens.
    theme_notices: Vec<String>,
}

/// Keeps `held` in step with a kitty command stream: `a=t` uploads an id, `d=I` frees it.
fn track_graphics(held: &mut std::collections::BTreeSet<u32>, bytes: &[u8]) {
    for cmd in String::from_utf8_lossy(bytes).split("\x1b_G").skip(1) {
        let control = cmd.split([';', '\x1b']).next().unwrap_or("");
        let id = control
            .split(',')
            .find_map(|kv| kv.strip_prefix("i=")?.parse::<u32>().ok());
        let Some(id) = id else { continue };
        if control.split(',').any(|kv| kv == "d=I") {
            held.remove(&id);
        } else if control.split(',').any(|kv| kv == "a=t") {
            held.insert(id);
        }
    }
}

/// The theme the operator's own rano config names, resolved the way rano's binary resolves it, so
/// the same file looks the same in both programs. What cannot be used comes back to be said.
#[cfg(not(test))]
fn rano_theme(cfg: &rano::config::Config) -> (Option<rano::theme::Theme>, Vec<String>) {
    let dir = rano::config::config_path()
        .parent()
        .map(|d| d.join("themes"))
        .unwrap_or_default();
    rano::theme::resolve(&dir, cfg.theme.as_deref(), &cfg.colors)
}

impl EditorPane {
    fn new(proto: rano::term::Proto) -> EditorPane {
        // The operator's own rano config in a live head, so the pane is the editor they
        // already use; the defaults under test, so no test reads a home directory.
        #[cfg(not(test))]
        let config = rano::config::load();
        #[cfg(test)]
        let config = rano::config::Config::default();
        #[cfg(not(test))]
        let (theme, problems) = rano_theme(&config);
        #[cfg(test)]
        let (theme, problems) = (None, Vec::new());
        let mut ed = Rano::new(rano::buffer::Buffer::new(), config);
        // No chrome of its own: the head frames the pane and carries its keys on its one hint row,
        // so rano draws the text, and a status row only while it has a prompt or a flash.
        ed.set_chrome(rano::editor::Chrome::BARE);
        ed.set_pictures(proto);
        // One spelling per screen: rano gains a third notation beside emacs and nano - `ctrl-x`,
        // `alt-x` - and the head sets it, so the keys it hands the hint row come back in the spelling
        // the rest of the frame uses. Standalone rano keeps whatever the operator configured.
        ed.config.notation = rano::keymap::Notation::Words;
        let mut keys = Keymap::new("letibot");
        keys.bind(CROSS, TO_COMPOSER);
        // Bound first, so `where_is("exit")` names it in `key_hints`: the two reach the same command
        // and only one of them is worth teaching.
        keys.bind(CLOSE, "exit");
        keys.bind(CLOSE_LEGACY, "exit");
        ed.push_keymap(keys);
        let sends: Rc<RefCell<Vec<SendEvent>>> = Rc::default();
        let queue = sends.clone();
        ed.on_send = Some(Box::new(move |e: &SendEvent| {
            queue.borrow_mut().push(e.clone());
            Ok("Sent to letibot's composer".into())
        }));
        // `alt-k` is rano's prefix and the presets are its config; the head is what turns a picked
        // preset into a prompt. The flash it answers with is dropped below, because the head's own
        // status line is the only one this pane claims to keep.
        let asks: Rc<RefCell<Vec<rano::ask::AskEvent>>> = Rc::default();
        let queue = asks.clone();
        ed.on_ask = Some(Box::new(move |e: &rano::ask::AskEvent| {
            queue.borrow_mut().push(e.clone());
            Ok(String::new())
        }));
        let mut pane = EditorPane {
            ed,
            focused: true,
            hidden: false,
            area: Area::default(),
            sends,
            asks,
            buf: rano::render::Buffer::empty(rano::render::Rect::new(0, 0, 0, 0)),
            rows: Vec::new(),
            caret: None,
            dirty: true,
            palette: None,
            marked: None,
            theme: None,
            theme_notices: problems,
            held: Default::default(),
        };
        pane.set_theme(theme);
        pane
    }

    /// Draw the pane under `theme` from the next frame: the operator's rano theme, or a test's. A
    /// theme with no roles is no override, so the pane keeps the head's look.
    pub(crate) fn set_theme(&mut self, theme: Option<rano::theme::Theme>) {
        self.theme = theme;
        self.dirty = true;
    }

    /// What rano's own theme file could not be used for, drained once by whoever made the pane.
    fn theme_problems(&mut self) -> Vec<String> {
        std::mem::take(&mut self.theme_notices)
    }

    /// What rano queued for the terminal, tracked and handed over in write order.
    fn graphics(&mut self) -> Vec<Vec<u8>> {
        let out = self.ed.take_graphics();
        for b in &out {
            track_graphics(&mut self.held, b);
        }
        out
    }

    /// The pane is done: everything it put in the terminal's memory is freed.
    fn release(&mut self) -> Vec<Vec<u8>> {
        self.ed.set_pictures(rano::term::Proto::None);
        let mut out = self.graphics();
        out.extend(
            std::mem::take(&mut self.held)
                .into_iter()
                .map(rano::term::graphics::image_delete),
        );
        out
    }

    /// The file rano has current, as rano names it: what a mark is about when the operator opens
    /// something, and `None` for a scratch buffer that names no file.
    pub(crate) fn shown(&self) -> Option<PathBuf> {
        self.ed
            .buffers
            .get(self.ed.cur)
            .and_then(|b| b.buf.name.clone())
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

    /// Is the pane anywhere on the screen: as the whole rectangle, or as the column beside the
    /// conversation. What the keyboard, the mouse and the frame's pace are handed to.
    pub(crate) fn editor_up(&self) -> bool {
        self.edit_pane.as_ref().is_some_and(|p| !p.hidden)
    }

    /// Does the editor pane have the keyboard.
    pub fn editor_focused(&self) -> bool {
        self.editor_takes_input() && self.edit_pane.as_ref().is_some_and(|p| p.focused)
    }

    /// Is the pane the one the terminal's reports go to: on the screen, and not under a `!term`
    /// pane or the diff popup, which are drawn over it and own the keys themselves.
    pub(crate) fn editor_takes_input(&self) -> bool {
        self.editor_up() && !self.pane_open() && self.diff_popup.is_none()
    }

    /// How the pane sits in a frame `term_w` columns wide. The editor's own width is the
    /// operator's, clamped so the conversation keeps its minimum - and the rooms sidebar's
    /// columns too, when the terminal is wide enough to hold all three.
    pub(crate) fn editor_fit(&self, term_w: usize) -> EditorFit {
        if !self.editor_up() {
            return EditorFit::Off;
        }
        // A `!term` program and the diff popup take the conversation's rectangle whole, as they
        // always have: a screen drawn over the pane is not a column beside it.
        if self.pane_open() || self.diff_popup.is_some() {
            return EditorFit::Zoom;
        }
        let least = EDIT_MIN + 1 + super::sidebar::MAIN_MIN;
        if self.editor_zoom || term_w < least {
            return EditorFit::Zoom;
        }
        let reserve = match term_w >= least + 1 + super::sidebar::SIDE_MIN && self.sidebar.on {
            true => super::sidebar::MAIN_MIN + super::sidebar::SIDE_MIN + 2,
            false => super::sidebar::MAIN_MIN + 1,
        };
        let want = self.editor_width.unwrap_or(term_w / 2);
        let most = term_w.saturating_sub(reserve).max(EDIT_MIN);
        EditorFit::Column(want.clamp(EDIT_MIN, most))
    }

    /// alt-i: the keyboard between the pane and the composer. A pane that was put away comes
    /// back, because a key that only says `it is not here` is a key the operator stops pressing.
    pub(crate) fn editor_focus_key(&mut self) {
        let Some(p) = self.edit_pane.as_mut() else {
            self.say("no editor open — ctrl-e opens one on any file");
            return;
        };
        p.hidden = false;
        p.focused = !p.focused;
        p.dirty = true;
    }

    /// alt-m: the pane takes the whole rectangle instead of sharing it, and back. Below the
    /// width the two need it changes nothing, and says so rather than doing nothing.
    pub(crate) fn editor_zoom_key(&mut self) {
        if self.edit_pane.is_none() {
            self.say("no editor open — ctrl-e opens one on any file");
            return;
        }
        self.editor_zoom = !self.editor_zoom;
        let least = EDIT_MIN + 1 + super::sidebar::MAIN_MIN;
        if !self.editor_zoom && self.editor_term_w > 0 && self.editor_term_w < least {
            self.say(&format!(
                "this terminal is {} columns: the editor needs {least} to sit beside the \
                 conversation",
                self.editor_term_w
            ));
        }
    }

    /// The columns the conversation's own x's are shifted right by on the last frame.
    pub(crate) fn editor_offset(&self) -> usize {
        match self.editor_col {
            0 => 0,
            w => w + 1,
        }
    }

    /// Set the editor's width and keep it: clamped to the minimums, written to `head.toml`.
    fn editor_set_width(&mut self, want: usize) {
        let t = self.editor_term_w;
        let least = EDIT_MIN + 1 + super::sidebar::MAIN_MIN;
        if t < least {
            return;
        }
        let most = t - super::sidebar::MAIN_MIN - 1;
        self.editor_width = Some(want.clamp(EDIT_MIN, most));
    }

    /// alt-< and alt-> while the pane is a column: narrower and wider by a few columns.
    pub(crate) fn editor_resize_key(&mut self, wider: bool) {
        if self.editor_col == 0 {
            return;
        }
        let w = self.editor_col;
        self.editor_set_width(if wider {
            w + EDIT_STEP
        } else {
            w.saturating_sub(EDIT_STEP)
        });
        let _ = self.save_prefs(RetiredWrite::Union);
    }

    /// A press on the border between the two columns starts a drag. `true` when it was the
    /// border's: the x is the terminal's own.
    pub(crate) fn editor_press(&mut self, x: u16, _y: u16) -> bool {
        if self.editor_col > 0 && usize::from(x) == self.editor_col {
            self.editor_dragging = true;
            return true;
        }
        false
    }

    /// Motion while that border is held: the editor's right edge follows the pointer.
    pub(crate) fn editor_drag(&mut self, x: u16) -> bool {
        if !self.editor_dragging {
            return false;
        }
        self.editor_set_width(usize::from(x));
        true
    }

    /// The button came up on the border: keep the width.
    pub(crate) fn editor_release(&mut self) -> bool {
        if !std::mem::take(&mut self.editor_dragging) {
            return false;
        }
        let _ = self.save_prefs(RetiredWrite::Union);
        true
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
        let proto = self.proto;
        let pane = self.edit_pane.get_or_insert_with(|| EditorPane::new(proto));
        // The rectangle first, when a frame has already measured it: rano centres the
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
        self.say_editor_theme();
    }

    /// What the operator's rano theme could not be used for, said once: the pane draws under that
    /// theme, so a line it cannot read is a look they expect and would not get.
    fn say_editor_theme(&mut self) {
        let Some(p) = self.edit_pane.as_mut() else {
            return;
        };
        let said = p.theme_problems();
        if !said.is_empty() {
            self.say(&said.join(" · "));
        }
    }

    /// Close the pane: the conversation has its rectangle back and the composer its keys.
    pub(crate) fn close_editor(&mut self) {
        if let Some(mut p) = self.edit_pane.take() {
            self.image_uploads.extend(p.release());
        }
    }

    /// rano's picture bytes, to the head's uploads: written to the terminal beside the frame.
    /// The placements are virtual (Unicode placeholders in the rows), so they carry no screen
    /// position and the pane's offset needs no translation.
    pub(crate) fn forward_editor_graphics(&mut self) {
        if let Some(p) = self.edit_pane.as_mut() {
            let out = p.graphics();
            self.image_uploads.extend(out);
        }
    }

    /// `ctrl-]` from the composer side: back into an open pane, or open one on the newest
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
            // No surface on the stack, open or covered - every one of them reads `End` as its
            // own key, so the standing pane is covered without a hand-kept list of fields.
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
        let proto = self.proto;
        let fresh = self.edit_pane.is_none();
        let pane = self.edit_pane.get_or_insert_with(|| EditorPane::new(proto));
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
        self.say_editor_theme();
    }

    /// The newest file this conversation changed, for `ctrl-]`: the live turn's finished
    /// calls first (their rows may not have landed yet), then the rows, newest first.
    pub(crate) fn newest_change(&self) -> Option<FileRef> {
        let live = self.turn.as_ref().and_then(|t| {
            t.calls.iter().rev().find_map(|c| match &c.state {
                letibot_sessionlog::view::CallState::Finished { edits, .. }
                    if card::Verb::of(crate::ui::drawn_as(&c.name, edits.first())).is_an_edit() =>
                {
                    edits.first().map(FileRef::of)
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

    /// The place row `k` is about, when it is a finished edit or write whose change this
    /// head holds - from the row itself, or from what this head watched the call do.
    pub(crate) fn change_of_row(&self, k: usize) -> Option<FileRef> {
        let it = self.items.get(k)?;
        let Some(letibot_transcript::TranscriptItem::ToolResult { name, edits, .. }) = &it.item
        else {
            return None;
        };
        let edits = self
            .call_edits
            .get(&it.item_id)
            .map(Vec::as_slice)
            .unwrap_or(edits);
        let edit = edits.first();
        // The verb the row was DRAWN as, so ctrl-] opens the file a card drew as an edit - asked of
        // the same layer answer the drawing asked for, and the head's own rule when there is none.
        let verb = if self.card_layers_on() {
            let host = self.plugin_host();
            Cards::call(host.as_ref(), &it.item_id).verb(name, edit)
        } else {
            crate::ui::drawn_as(name, edit).to_string()
        };
        if !card::Verb::of(&verb).is_an_edit() {
            return None;
        }
        edit.map(FileRef::of)
    }

    /// Open what a call the turn is still drawing has behind it: deliberately the same act the
    /// call's own row performs once it lands, because a click that answers nothing reads as a head
    /// that stopped listening. A call that has not returned has no body, so the running case says so.
    pub(crate) fn open_call(&mut self, call_id: &str) {
        // The call, copied out - the frame's record names it by id, and everything below
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
            CallState::Finished { edits, .. } => edits.first().cloned(),
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

    /// How long the head's loop may wait for input before rano wants another tick - `None`
    /// with no pane on the screen, when the head's own pace is the only one.
    pub fn editor_wait(&self) -> Option<Duration> {
        self.edit_pane
            .as_ref()
            .filter(|p| !p.hidden)
            .map(|p| p.ed.next_wakeup())
    }

    /// rano's work between events, once a pass of the head's loop and before the frame  -
    /// rano's own rule: a host that draws must tick first. Then whatever rano decided in the
    /// meantime: a send to take, an exit to honour.
    pub fn editor_tick(&mut self, now: Instant) {
        if let Some(p) = self.edit_pane.as_mut() {
            p.dirty |= p.ed.tick(now);
        }
        self.after_editor();
        self.forward_editor_graphics();
    }

    /// What the terminal said, routed: the pane's share goes to rano, the rest comes back as this
    /// head's keys, in order, plus whether rano took anything - a read rano consumed is not one whose
    /// bytes a `!term` pane may scan for its way out. With a `!term` pane drawn nothing is routed.
    pub fn route(&mut self, events: Vec<Event>) -> (Vec<Key>, bool) {
        let mut keys = Vec::new();
        let mut took = false;
        for e in events {
            if self.editor_takes_input()
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
                        // The head's two keys cross into the pane and out of it, so they are read
                        // before rano: rano has neither bound. A key rano had no use for comes back
                        // as the head's, in this read's order.
                        match key_of(Event::Key(*k)) {
                            Some(Key::AltI) => self.editor_focus_key(),
                            Some(Key::AltM) => self.editor_zoom_key(),
                            _ => {
                                if let Some(key) = self.editor_key(*k) {
                                    keys.push(key);
                                }
                            }
                        }
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
                // The crossing chord acts at once, so the keys after it in the same read go
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

    /// One key into rano, and what rano said back. `Some(Key)` is a key rano had no use for and
    /// the head does - the caller hands it to `App::key` in this read's order.
    fn editor_key(&mut self, k: KeyEvent) -> Option<Key> {
        let mut to_head = None;
        let mut says = None;
        {
            let p = self.edit_pane.as_mut()?;
            match p.ed.handle_key(k) {
                KeyOutcome::Host(name) if name == TO_COMPOSER => p.focused = false,
                KeyOutcome::Unhandled => match refused(k) {
                    Refused::Leave(key) => {
                        p.focused = false;
                        to_head = Some(key);
                    }
                    Refused::Pass(key) => to_head = Some(key),
                    Refused::Hint(word, what) => {
                        says = Some(format!("{word} is {what} — esc first"));
                    }
                    Refused::Drop => {}
                },
                _ => {}
            }
            p.dirty = true;
        }
        if let Some(s) = says {
            self.say(&s);
        }
        self.after_editor();
        to_head
    }

    /// What rano decided, acted on: an exit closes the pane - and only the pane - and each
    /// send lands in the composer.
    fn after_editor(&mut self) {
        let Some(p) = self.edit_pane.as_mut() else {
            return;
        };
        let sends: Vec<SendEvent> = p.sends.borrow_mut().drain(..).collect();
        let asks: Vec<rano::ask::AskEvent> = p.asks.borrow_mut().drain(..).collect();
        // What the operator decided about a hunk: rano has dropped it, so the head's copy of it
        // goes too rather than waiting to be put back on the next open.
        let decided = p.ed.take_mark_outcomes();
        if !asks.is_empty() {
            // rano flashes the ask's answer in a status row of its own; this pane has one status
            // row and the head draws it, so the flash goes before it is ever drawn.
            p.ed.status = None;
        }
        if p.ed.wants_quit() {
            self.close_editor();
        }
        for s in sends {
            self.take_send(&s);
        }
        for a in asks {
            self.ask_in(&a);
        }
        if !decided.is_empty() {
            self.forget_decided_marks(&decided);
        }
        // And the file the pane has moved to carries the marks the conversation set on it.
        self.mark_the_shown_file();
        self.forward_editor_graphics();
    }

    /// A send from rano, into the composer: a chip naming the place, then the keyboard. Nothing is
    /// inserted into the words - the daemon writes the block the model reads - and nothing is sent
    /// either: the instruction is the operator's to write.
    pub(crate) fn take_send(&mut self, e: &SendEvent) {
        self.context_in(e);
    }

    /// The pane's buffers with unsaved edits, as the operator reads a path. rano keeps every buffer
    /// a hidden or switched-away file left behind, so this is the pane's state and not the drawn
    /// buffer's - which is exactly the work a quit discards.
    pub fn modified_buffers(&self) -> Vec<String> {
        self.modified_paths()
            .iter()
            .map(|p| self.workspace_relative(p))
            .collect()
    }

    /// The same buffers as rano holds them, for opening one.
    fn modified_paths(&self) -> Vec<PathBuf> {
        self.edit_pane
            .as_ref()
            .map(|p| {
                p.ed.buffers
                    .iter()
                    .filter(|b| b.buf.modified)
                    .filter_map(|b| b.buf.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The quit card's offer: the pane comes back on the first file leaving would discard, so the
    /// answer is given about files the operator has seen rather than a count nobody read.
    pub(crate) fn show_modified_buffers(&mut self) {
        self.quit_card = false;
        let mut paths = self.modified_paths();
        if paths.is_empty() {
            self.say("no editor buffer has unsaved edits");
            return;
        }
        let first = paths.swap_remove(0);
        let rest = paths.len();
        let shown = self.workspace_relative(&first);
        self.open_in_editor(&FileRef::file(&first.display().to_string()));
        let more = match rest {
            0 => String::new(),
            n => format!(" and {n} more"),
        };
        self.say(&format!(
            "{shown}{more} — ctrl-s writes it; ctrl-c twice asks about leaving again"
        ));
    }

    /// The file the pane is showing, as every path this head draws: relative to the session's
    /// workspace, keeping rano's `[i/n] ` in front of it when several buffers are open. `None` with
    /// no pane; empty when the pane has no file.
    pub(crate) fn editor_name(&self) -> Option<String> {
        let full = self.edit_pane.as_ref()?.ed.frame_title().name;
        if full.is_empty() {
            return Some(String::new());
        }
        let (count, path) = match full.split_once("] ") {
            Some((n, rest)) if full.starts_with('[') => (format!("{n}] "), rest.to_string()),
            _ => (String::new(), full),
        };
        Some(format!(
            "{count}{}",
            self.workspace_relative(Path::new(&path))
        ))
    }

    /// rano's live key hints as the head's hint row says them. Generated from the maps rano is
    /// dispatching from, so the row cannot name a key that does nothing.
    pub(crate) fn editor_key_words(&self) -> String {
        self.edit_pane
            .as_ref()
            .map(|p| crate::ui::editor::pane_keys(&p.ed.key_hints()))
            .unwrap_or_default()
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

    /// The register the editor pane's own frame is drawn in - the focus marker: the side that has
    /// the keyboard is `Strong` and the other is `Faint`, so a frame says whose keys are live
    /// without anybody reading the hint row.
    pub(crate) fn editor_frame(&self) -> Role {
        if self.editor_focused() {
            Role::Strong
        } else {
            Role::Faint
        }
    }

    /// The composer's half of the same marker: `Strong` while the composer has the keys and an
    /// open pane beside it could have them instead. With no pane on the screen there is one side
    /// only, and a marker that alternates with nothing says nothing.
    pub(crate) fn composer_frame(&self) -> Role {
        if self.editor_drawn() && !self.editor_focused() {
            Role::Strong
        } else {
            Role::Faint
        }
    }
}

/// What a key rano had no use for is to the head.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Refused {
    /// The keyboard goes to the composer and this key with it: Esc is the head's, and the press
    /// that leaves the pane is the first of the pair that interrupts.
    Leave(Key),
    /// The head's own key, which nothing on the pane's side of the crossing owns.
    Pass(Key),
    /// A chord that opens something the pane has not got: what to name it with.
    Hint(&'static str, &'static str),
    Drop,
}

/// Read a refused key, from the same tables `App::key` dispatches from. Only the keys that OPEN
/// something earn a sentence: the rest were dropped before and are dropped still.
fn refused(k: KeyEvent) -> Refused {
    match key_of(Event::Key(k)) {
        // Nothing of rano's own is left to cancel, so this is the way out, and it arms the pair.
        Some(Key::Esc) => Refused::Leave(Key::Esc),
        // The one pass-through: a repaint is nobody's state, and the pane's glass is as stale as
        // the conversation's after something else wrote to the terminal.
        Some(Key::CtrlL) => Refused::Pass(Key::CtrlL),
        Some(k) => match super::keys::bindings::head_chord(&k) {
            Some((word, what)) => Refused::Hint(word, what),
            None => Refused::Drop,
        },
        None => Refused::Drop,
    }
}
