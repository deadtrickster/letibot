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
use crate::ui::transcript::job_card::{EndedJob, JobTail, TAIL_BYTES, notice_jobs};
use crate::ui::transcript::line_cards::is_card_row;

/// The popup's state: what it shows, where the reader is in it, and the rows it last laid out
/// (kept per width and diff shape, because a file is re-diffed only when either changes).
#[derive(Debug, Clone)]
pub struct DiffPopup {
    pub(crate) body: PopupBody,
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

/// **What a popup over the conversation holds.**
///
/// A change is the first thing it held. The second is a job's completion notice — the operator,
/// asking for the notice to be two lines in the transcript: *"click behaves similar to diff
/// click - popup with full card"*. One popup and not two, because what the reader does with it is
/// the same — read, scroll, Esc — and a second overlay would be a second set of keys, a second
/// entry in `drawn()` and a second place the cursor has to be hidden.
#[derive(Debug, Clone)]
pub(crate) enum PopupBody {
    /// An edit or write, read in its whole file.
    Change(FileRef),
    /// **A job's completion notice, whole**: the daemon's text as the model read it, and the
    /// jobs it reports, whose output the popup shows under it when this head holds it.
    Notice {
        text: String,
        jobs: Vec<(bool, EndedJob)>,
        /// **The operator's answer, when the notice is a question to them** (`asked_card`): the
        /// row the session wrote right after it, so the popup can mark what they chose.
        answer: Option<String>,
    },
}

/// **A note drawn as a card's one line** — where the line is in `hist_lines`, and the text the
/// popup shows whole. See [`App::note_cards`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoteCard {
    pub(crate) at: usize,
    pub(crate) lines: usize,
    pub(crate) text: String,
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
        self.open_popup(PopupBody::Change(f.clone()));
    }

    fn open_popup(&mut self, body: PopupBody) {
        self.diff_popup = Some(DiffPopup {
            body,
            scroll: 0,
            placed: false,
            laid: None,
            room: 0,
        });
    }

    /// **Open row `k`'s completion notice whole** — the click on its compact lines. A job whose
    /// output this head has not asked for yet is asked for now, so the popup fills in when the
    /// answer lands. Does nothing for a row that is not a notice this head can read.
    ///
    /// **A card's notice opens the same way** (`line_cards`) — a red target's, a review's: its
    /// text whole, the gate's output or the reviewer's reasons with it, and no jobs under it.
    pub(crate) fn open_notice_popup(&mut self, k: usize) {
        let Some(text) = self.items.get(k).and_then(agent_row_text) else {
            return;
        };
        let jobs = match notice_jobs(&text) {
            Some(jobs) => jobs,
            None if is_card_row(&text) => Vec::new(),
            None => return,
        };
        let asks: Vec<(String, u64)> = jobs
            .iter()
            .filter(|(mine, _)| *mine)
            .map(|(_, j)| (j.id.clone(), j.produced))
            .collect();
        self.ask_tails(asks);
        let answer = crate::ui::transcript::asked_card::question_of(&text)
            .and_then(|_| crate::ui::transcript::asked_card::answer_after(&self.items, k));
        self.open_popup(PopupBody::Notice { text, jobs, answer });
    }

    /// **Open the note card `i` whole** — the click on a card's one line (a `target_red` warning, a
    /// review's warning): the detail as the daemon raised it, the gate's output or the reviewer's
    /// reasons included, in the job card's popup.
    pub(crate) fn open_card_popup(&mut self, i: usize) {
        let Some(card) = self.note_cards.get(i) else {
            return;
        };
        let text = card.text.clone();
        self.open_popup(PopupBody::Notice {
            text,
            jobs: Vec::new(),
            answer: None,
        });
    }

    /// **The note-card lines among lines `start..end` of the conversation**, as `(row of the
    /// window, index into `note_cards`)` — [`App::notice_rows_in`] for the notes.
    pub(crate) fn card_rows_in(&self, start: usize, end: usize) -> Vec<(usize, usize)> {
        let end = end.min(self.hist_lines.len());
        let mut out = Vec::new();
        for (i, c) in self.note_cards.iter().enumerate() {
            for line in c.at..c.at + c.lines {
                if line >= start && line < end {
                    out.push((line - start, i));
                }
            }
        }
        out
    }

    /// **Ask for the end of each job's output that this head does not hold yet** — `(handle,
    /// bytes it wrote)`. Once per job: an answer, a refusal and a pending ask all leave a mark
    /// in `job_tails`, so a notice that stays on the screen is not a read per frame. A job that
    /// wrote nothing needs no read to say so.
    ///
    /// **The tail, not the head.** The offset is [`TAIL_BYTES`] before the end, because a
    /// build or a test run says how it went in its last lines; the first line of such a window
    /// may be cut, which is why only its LAST line is drawn on the row.
    pub(crate) fn ask_tails(&mut self, asks: Vec<(String, u64)>) {
        if self.session_id.is_empty() {
            return;
        }
        for (job, produced) in asks {
            if self.job_tails.contains_key(&job) {
                continue;
            }
            if produced == 0 {
                self.job_tails.insert(job, JobTail::Lines(Vec::new()));
                continue;
            }
            self.job_tails.insert(job.clone(), JobTail::Asked);
            self.queued.push(Action::ReadJobOutput {
                job,
                offset: produced.saturating_sub(TAIL_BYTES),
            });
        }
    }

    /// **A job's tail landed (or was refused)**: kept, and every row that draws it is drawn
    /// again — the notice rows from the first one that names the job, and the popup if it is
    /// open on it.
    pub(crate) fn job_tail_landed(&mut self, job: &str, tail: JobTail) {
        self.job_tails.insert(job.to_string(), tail);
        let first = self.items.iter().position(|it| {
            agent_row_text(it)
                .and_then(|t| notice_jobs(&t))
                .is_some_and(|jobs| jobs.iter().any(|(_, j)| j.id == job))
        });
        if let Some(k) = first {
            self.invalidate_history_from(k);
        }
        if let Some(p) = self.diff_popup.as_mut()
            && let PopupBody::Notice { jobs, .. } = &p.body
            && jobs.iter().any(|(_, j)| j.id == job)
        {
            p.laid = None;
        }
        self.redraw = true;
    }

    /// **The notice rows among lines `start..end` of the conversation**, as `(row of the window,
    /// index of the row in `items`)` — [`App::file_rows_in`]'s walk, for the other row a click
    /// opens. Asks for the tails of the jobs those rows name, so the rows on the screen are the
    /// ones whose output is read.
    pub(crate) fn notice_rows_in(&mut self, start: usize, end: usize) -> Vec<(usize, usize)> {
        let end = end.min(self.hist_lines.len());
        let mut out = Vec::new();
        let mut asks: Vec<(String, u64)> = Vec::new();
        let mut last: Option<(usize, bool)> = None;
        for line in start..end {
            let i = self.spans.partition_point(|sp| sp.at <= line);
            let Some(sp) = i.checked_sub(1).map(|i| self.spans[i]) else {
                continue;
            };
            if line >= sp.at + sp.lines {
                continue;
            }
            let is_notice = match last {
                Some((row, is)) if row == sp.row => is,
                _ => {
                    let text = self.items.get(sp.row).and_then(agent_row_text);
                    let jobs = text.as_deref().and_then(notice_jobs);
                    if let Some(jobs) = &jobs {
                        asks.extend(
                            jobs.iter()
                                .filter(|(mine, _)| *mine)
                                .map(|(_, j)| (j.id.clone(), j.produced)),
                        );
                    }
                    // A card's notice is a notice too (`line_cards`).
                    let is = jobs.is_some() || text.as_deref().is_some_and(is_card_row);
                    last = Some((sp.row, is));
                    is
                }
            };
            if is_notice {
                out.push((line - start, sp.row));
            }
        }
        self.ask_tails(asks);
        out
    }

    /// **The notice popup's rows at `width`**: the notice as the model read it, wrapped — the
    /// paragraph addressed to the model included, because this is the full card the compact
    /// rows stand for — and under it, for each job, the end of what it wrote.
    pub(crate) fn notice_popup_layout(
        &self,
        text: &str,
        jobs: &[(bool, EndedJob)],
        answer: Option<&str>,
        width: usize,
    ) -> Layout {
        use crate::ui::render::wrap;
        use letibot_ui::text::without_control;
        let mut rows: Vec<Line> = Vec::new();
        let push_wrapped = |rows: &mut Vec<Line>, s: &str, role: Option<Role>| {
            for w in wrap(&without_control(s), width.max(4)) {
                rows.push(match role {
                    Some(r) => Line::styled(w, r),
                    None => Line::raw(w),
                });
            }
        };
        // **A question to the operator is drawn with its options one per line**, theirs marked
        // (`asked_card::full_lines`); every other notice as the daemon wrote it.
        let asked = crate::ui::transcript::asked_card::full_lines(text, answer, width);
        let as_written = if asked.is_some() { "" } else { text };
        rows.extend(asked.into_iter().flatten());
        for l in as_written.lines() {
            if l.is_empty() {
                rows.push(Line::default());
            } else {
                push_wrapped(&mut rows, l, None);
            }
        }
        for (mine, j) in jobs {
            rows.push(Line::default());
            let label = match &j.slug {
                Some(s) => format!("{s} · {}", j.id),
                None => j.id.clone(),
            };
            rows.push(Line::styled(
                format!("what {label} wrote, at its end"),
                Role::Strong,
            ));
            match self.job_tails.get(&j.id) {
                Some(JobTail::Lines(ls)) if ls.is_empty() => {
                    push_wrapped(&mut rows, "it wrote nothing", Some(Role::Pending))
                }
                Some(JobTail::Lines(ls)) => {
                    for l in ls {
                        if l.is_empty() {
                            rows.push(Line::default());
                        } else {
                            push_wrapped(&mut rows, l, None);
                        }
                    }
                }
                Some(JobTail::Asked) => push_wrapped(&mut rows, "reading…", Some(Role::Pending)),
                Some(JobTail::Gone(why)) => push_wrapped(&mut rows, why, Some(Role::Pending)),
                None if !mine => push_wrapped(
                    &mut rows,
                    "not readable from this session: the session that started it has stopped",
                    Some(Role::Pending),
                ),
                None if j.produced == 0 => {
                    push_wrapped(&mut rows, "it wrote nothing", Some(Role::Pending))
                }
                None => push_wrapped(&mut rows, "not read", Some(Role::Pending)),
            }
        }
        Layout {
            width,
            split: self.diff_split,
            rows,
            first: 0,
            note: None,
        }
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
        // **A popup is opened only on a row that carries a change** — `file_rows` is built from
        // finished edits and writes, and every `FileRef` in it has one — so a place with no
        // change is not a state this head reaches. It says so rather than drawing a file diffed
        // against nothing.
        let Some(c) = &f.change else {
            return Layout {
                width,
                split,
                rows: Vec::new(),
                first: 0,
                note: Some("nothing to review here: this is a file, not a change".to_string()),
            };
        };
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
        let shown_path = without_control_lines(&f.path);
        let mut rows = rano::sidediff::render_edit_view(
            &shown_path,
            &without_control_lines(&before),
            &without_control_lines(&after),
            bs,
            as_,
            &dcfg,
            rano::sidediff::edit_view(split),
        );
        // **The file is named once, in the popup's top edge.** The renderer heads every edit view
        // with the file's name — right for a card in the conversation, a repeat inside a frame
        // that already says it. Dropped only when the first row IS that name.
        if rows
            .first()
            .is_some_and(|l| l.plain().trim() == shown_path.trim())
        {
            rows.remove(0);
        }
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
    ///
    /// **Except while a decision is open** (R67 item 4). The card is drawn in the chrome under
    /// the popup and the bar says *a row number answers*, and this handler — placed below the
    /// cards and asks precisely so a question arriving mid-read stays answerable — returned
    /// `Break` for every key ahead of `key_decision`: digits, ↑↓ and Enter scrolled the diff or
    /// vanished, and the turn waited on a card nobody could answer. With a decision open the
    /// popup keeps only what cannot be the card's — Esc and `ctrl-c` (close the popup, never
    /// a refusal), `ctrl-]`, the wheel and the page keys — and everything else goes on to the
    /// card and the composer, the typed answer included.
    pub(crate) fn key_diff_popup(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        let deciding = !self.open.is_empty();
        let Some(p) = self.diff_popup.as_mut() else {
            return ControlFlow::Continue(());
        };
        if deciding
            && !matches!(
                k,
                Key::Esc
                    | Key::CtrlC
                    | Key::CtrlBracket
                    | Key::WheelUp
                    | Key::WheelDown
                    | Key::PageUp
                    | Key::PageDown
            )
        {
            return ControlFlow::Continue(());
        }
        let len = p.laid.as_ref().map_or(0, |l| l.rows.len());
        let page = p.room.max(1);
        let max = len.saturating_sub(page);
        match k {
            Key::Esc | Key::CtrlC => {
                self.diff_popup = None;
            }
            // **`ctrl-]` edits a change**; a notice has no file behind it, so the key is the
            // popup's and does nothing.
            Key::CtrlBracket => {
                if let PopupBody::Change(f) = &p.body {
                    let f = f.clone();
                    self.diff_popup = None;
                    self.open_in_editor(&f);
                }
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
        // **No `self.redraw`.** That flag is a full repaint — `ESC[2J` and every row again — and
        // the frame is built and diffed on every tick anyway, so a scroll costs exactly the rows
        // it moved. Setting it here erased the whole screen on every wheel notch: the operator,
        // 2026-10-09, *"if i scroll here back and forth very fast, the very bottom keymap starts
        // to flicker"*.
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
