//! **The diff popup**: a change, read in its whole file, over the conversation.
//!
//! The operator, 2026-10-08: *"I dont need the full rano chrome when I look on the full edit,
//! but i do need the context - so full file with diff and cursor placement at the beginning of
//! the first diff. preferably in a popup and of course Esc must just close it"*. A click on an
//! edit or write row used to open the rano editor — title bar, function bar, status line — on
//! a review view, and Esc dropped into the file's text rather than closing anything. Reading a
//! change is not editing a file: this is a viewer, with the editor one key away (`ctrl-]`).

use std::ops::ControlFlow;

use rano::render::Line;
use rano::style::Role;

use super::editor::FileRef;
use super::*;

/// The popup's state: the change it shows, where the reader is in it, and the rows it last
/// laid out (kept per width and diff shape, because a file is re-diffed only when either
/// changes).
#[derive(Debug, Clone)]
pub struct DiffPopup {
    pub(crate) file: FileRef,
    /// The first row on the screen, among the popup's body rows.
    pub(crate) scroll: usize,
    /// Whether the first frame has placed the reader on the first change yet. The rows are
    /// laid out at the frame's width, so the placement waits for the first draw.
    pub(crate) placed: bool,
    /// The last layout: `(width, split, rows, first changed row, note)`.
    pub(crate) laid: Option<Layout>,
    /// How many body rows the last frame had, for the paging keys.
    pub(crate) room: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct Layout {
    pub(crate) width: usize,
    pub(crate) split: bool,
    pub(crate) rows: Vec<Line>,
    pub(crate) first: usize,
    pub(crate) note: Option<String>,
}

impl App {
    /// Open the popup on `f`.
    pub(crate) fn open_diff_popup(&mut self, f: &FileRef) {
        self.diff_popup = Some(DiffPopup {
            file: f.clone(),
            scroll: 0,
            placed: false,
            laid: None,
            room: 0,
        });
        self.redraw = true;
    }

    /// **The popup's rows at `width`**: the whole file as a diff against itself before the
    /// edit, so every line has its context and the change sits where it is in the file. When
    /// the file is no longer what the edit left (edited since, or gone), the edit's own excerpt
    /// is shown instead, and the note says so.
    pub(crate) fn diff_popup_layout(&self, f: &FileRef, width: usize) -> Layout {
        use letibot_ui::text::without_control_lines;
        let split = self.diff_split;
        let dcfg = rano::diff::DiffConfig {
            width,
            palette: self.cfg.palette(),
            context: usize::MAX / 4,
            line_numbers: true,
            intra_line: true,
            max_rows: usize::MAX,
        };
        let c = &f.change;
        let on_disk = std::fs::read_to_string(self.resolve_path(&f.path)).ok();
        let whole = on_disk
            .as_deref()
            .and_then(|text| whole_file_before(text, c));
        let (before, after, bs, as_, note) = match (&whole, &on_disk) {
            (Some(old), Some(now)) => (old.clone(), now.clone(), 1, 1, None),
            (_, None) => (
                c.before.clone(),
                c.after.clone(),
                c.before_start,
                c.after_start,
                Some("the file is not there any more; this is the edit as it was made".to_string()),
            ),
            (None, Some(_)) => (
                c.before.clone(),
                c.after.clone(),
                c.before_start,
                c.after_start,
                Some("the file has changed since this edit; this is the edit alone".to_string()),
            ),
        };
        let rows = rano::sidediff::render_edit_view(
            &without_control_lines(&f.path),
            &without_control_lines(&before),
            &without_control_lines(&after),
            bs,
            as_,
            &dcfg,
            rano::sidediff::edit_view(split),
        );
        let first = rows.iter().position(changed).unwrap_or(0);
        Layout {
            width,
            split,
            rows,
            first,
            note,
        }
    }

    /// **The popup owns the keyboard while it is open**: the arrows and the paging keys read
    /// it, `ctrl-]` hands the same change to the editor, and Esc closes it — and nothing else.
    pub(crate) fn key_diff_popup(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        let Some(p) = self.diff_popup.as_mut() else {
            return ControlFlow::Continue(());
        };
        let len = p.laid.as_ref().map_or(0, |l| l.rows.len());
        let page = p.room.max(1);
        let max = len.saturating_sub(page);
        match k {
            Key::Esc | Key::CtrlC => {
                self.diff_popup = None;
            }
            Key::CtrlBracket => {
                let f = p.file.clone();
                self.diff_popup = None;
                self.open_in_editor(&f);
            }
            Key::Up => p.scroll = p.scroll.saturating_sub(1),
            Key::Down => p.scroll = (p.scroll + 1).min(max),
            Key::WheelUp => p.scroll = p.scroll.saturating_sub(3),
            Key::WheelDown => p.scroll = (p.scroll + 3).min(max),
            Key::PageUp => p.scroll = p.scroll.saturating_sub(page),
            Key::PageDown => p.scroll = (p.scroll + page).min(max),
            Key::Home => p.scroll = 0,
            Key::End => p.scroll = max,
            // Everything else is the popup's too: a letter typed while reading a diff must not
            // land in the composer underneath, unseen.
            _ => {}
        }
        self.redraw = true;
        ControlFlow::Break(None)
    }
}

/// A row that shows a change: one carrying the diff's added or removed tint.
///
/// **Or the sign column**, because the tint is a colour and a head drawing without colour
/// (`Palette::None`) has no roles on its rows at all: the `-`/`+` the diff writes in its own
/// span is the same fact in both.
fn changed(l: &Line) -> bool {
    let tint = |r: Role| matches!(r, Role::Added | Role::Removed);
    l.style.roles().any(tint)
        || l.spans
            .iter()
            .any(|s| s.style.roles().any(tint) || s.content == "-" || s.content == "+")
}

/// **The whole file as it was before the edit**: the file now, with the edit's `after` excerpt
/// put back to its `before`. `None` when the file at the excerpt's place is not what the edit
/// left — then the whole-file view would be a diff of somebody else's change.
pub(crate) fn whole_file_before(now: &str, c: &rano::review::Change) -> Option<String> {
    let lines: Vec<&str> = now.lines().collect();
    let after: Vec<&str> = c.after.lines().collect();
    let at = c.after_start.saturating_sub(1);
    if at + after.len() > lines.len() || lines[at..at + after.len()] != after[..] {
        return None;
    }
    let mut old: Vec<&str> = lines[..at].to_vec();
    old.extend(c.before.lines());
    old.extend(&lines[at + after.len()..]);
    let mut s = old.join("\n");
    if now.ends_with('\n') {
        s.push('\n');
    }
    Some(s)
}
