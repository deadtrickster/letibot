//! **The subagents pane**: the tree of children this session spawned, and a child's output.

use crate::app::*;
use crate::ui::render::row_strings;
use crate::ui::*;
use letibot_sessionlog::event::{Envelope, SessionEvent};
use letibot_sessionlog::registry::short_id;
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::TranscriptItem;
use letibot_ui::text::without_control_lines;
use rano::agent::subagents::{SubagentOutput, SubagentRow, SubagentsPane};

impl App {
    /// The subagent tree: the subagents this session spawned, their state and their
    /// prompt. A subagent is also a session, so `enter` goes to it and the last line says
    /// so — along with `p` (read it without moving) and `esc` (up to the parent).
    ///
    /// **One enumeration, two groups.** The rows come from [`App::subagent_stops`] — the same
    /// list the arrows, Enter and `p` read — so the drawn `▸` and the key that acts cannot
    /// disagree about which row is selected, which is the defect leticl's `todos-stops`
    /// docstring names. The children still going are drawn first; the finished ones live under
    /// a `finished (N)` row that stays folded unless the operator unfolded it.
    ///
    /// **The pane records where each stop landed**, in [`App::subagents_stop_rows`], which is
    /// what the arrows scroll to: a list longer than the screen can be walked without the cursor
    /// leaving it. Takes `&mut self` for that record alone — the sibling of [`App::todos_lines`].
    pub(crate) fn subagents_lines(&mut self, w: usize) -> Vec<String> {
        let content = self.subagents_view().content(w);
        self.subagents_stop_rows = content.stop_rows;
        row_strings(&content.lines, self.cfg.palette())
    }

    /// **The subagents pane's facts, in rano's words**: each child's state and what it was
    /// asked ([`subagent_asked`], the words the folded notice uses too), its short id, role,
    /// model and answer, the fold of the finished ones, and the cursor.
    pub(crate) fn subagents_view(&self) -> SubagentsPane {
        SubagentsPane {
            agents: self
                .subagents
                .iter()
                .map(|s| SubagentRow {
                    state: s.state.clone(),
                    asked: subagent_asked(s),
                    session: short_id(&s.session_id),
                    role: s.role.clone(),
                    model: s.model.clone(),
                    generating: s.generating,
                    answer: s.answer.clone(),
                })
                .collect(),
            finished_open: self.subagents_finished_open,
            selected: self.subagents_sel,
        }
    }

    /// **A peeked session's rows, drawn by the renderer every other row goes through** — the
    /// deletion this whole change exists for.
    ///
    /// # What it replaces, and it was not the renderer's fault
    ///
    /// A child's output used to be built as plain strings by hand (`subagent_out_lines`), so it had
    /// none of the markdown, none of the air rule and none of the tool cards a parent's rows have —
    /// **in both heads**, leticl having inherited the shape faithfully. The defect was in the WIRE:
    /// `Peeked` answered with events *"for reading, not for folding into the head's state"*, which
    /// left a head nothing to do with them but draw them by hand. `Peeked::snapshot` is that fixed
    /// and this is the renderer half: `item_lines`, once, with the walk's own separator rule.
    ///
    /// # Two decisions worth stating
    ///
    /// * **Rendered once, when the rows land; not per frame.** The pane is a static list of a
    ///   finished read, and `App::screen` runs tens of times a second — see its own note on the cost
    ///   of a frame. The width is the frame's at the moment of the peek, which is the same width the
    ///   pane's own `trim_to` then clamps against.
    /// * **`targets` is seeded from the snapshot's own turn**, because that is where a row's display
    ///   target lives: an assistant row carries the arguments its calls were proposed with, and a
    ///   child's snapshot carries that turn. Rows whose calls are not in it render their correlation
    ///   id instead of a file name, which is `card::Phase::Replayed`'s rule — absent, not invented.
    pub(crate) fn sub_out_from_rows(&self, items: &[SnapshotItem]) -> Vec<String> {
        let cfg = self.cfg.clone();
        let mut targets: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for it in items {
            if let Some(TranscriptItem::Assistant { tool_calls, .. }) = it.item.as_ref() {
                for c in tool_calls {
                    targets.insert(
                        c.id.clone(),
                        letibot_sessionlog::display_target(&c.arguments),
                    );
                }
            }
        }
        let mut out: Vec<String> = Vec::new();
        // The walk's own separator rule: air where the KIND changes, and none between two activity
        // rows. One rule for the transcript and for this pane, or the pane is a second renderer
        // again in the one place nobody would look.
        let mut prev: Option<RowClass> = None;
        for (i, it) in items.iter().enumerate() {
            let answered = round_results(items, i);
            let ctx = ItemCtx {
                cfg: &cfg,
                think: Fold::Folded,
                tools: Fold::Folded,
                raw: false,
                targets: &targets,
                answered: &answered,
                subagents: &self.subagents,
                // No durations, no diffs and no approvals: this head did not watch these calls run,
                // and the maps that carry them are keyed by THIS session's item ids. The renderer
                // already says *replayed* for all three rather than inventing one.
                drawn_live: false,
                elapsed_ms: None,
                edit: None,
                decision: None,
                bound: None,
                // Nothing here is this head's own echo, so the mark never appears.
                echo_mark: QUEUED,
                echo_open: false,
                // **The lifted set**, because this pane exists to show a child's rows whole: it is
                // the same "the rung lifted" the open run draws its own rows with, so a peeked
                // session is not filtered by whatever profile the parent happens to be in.
                vis: Visibility::lifted(),
                diff_split: self.diff_split,
                payload_view: None,
                payload_max: None,
                window_rows: usize::MAX,
                payload_newest: None,
            };
            let (class, rows) = item_lines(it, &ctx);
            if rows.is_empty() {
                continue;
            }
            let pack = prev == Some(RowClass::Activity) && class == RowClass::Activity;
            if !out.is_empty() && !pack {
                out.push(String::new());
            }
            out.extend(rows);
            prev = Some(class);
        }
        out
    }

    /// The output view: the session's own rows, as a terminal scrolls them — or, when the daemon
    /// answered with its event ring, the plain fallback sayings so on the screen.
    pub(crate) fn sub_out_lines(&mut self, room: usize) -> Vec<String> {
        let p = self.cfg.palette();
        let Some(v) = self.sub_out.as_mut() else {
            return Vec::new();
        };
        let frame = SubagentOutput {
            session: short_id(&v.session_id),
            dropped: v.dropped,
            degraded: v.degraded,
            spill: v.spill.clone(),
        };
        let l = frame.layout(room, v.lines.len(), v.scroll);
        v.scroll = l.scroll;
        let mut out = row_strings(&l.head, p);
        // The child's own rows, already drawn by this head's renderer.
        out.extend(v.lines[l.start..l.end].iter().cloned());
        out.extend(std::iter::repeat_n(String::new(), l.pad));
        out.push(crate::ui::render::row(&l.footer, p));
        out.truncate(room);
        out
    }
}

/// The output pane's lines from a peeked scrollback: one block per tool result,
/// in order, the payload verbatim — that payload is the stdout and stderr the
/// tool produced, as the model received it. A `ToolFinished`'s spill locator is
/// the full output on disk when the inline payload was bounded; those paths are
/// named at the end, because *"there is more"* without a *where* is a dead end.
/// **What a subagent did, and what it SAID.**
///
/// This used to render tool results and nothing else, which is right for a
/// subagent that goes and does something and wrong for one whose whole product
/// is prose. A `digest` subagent is handed a slice of transcript in its prompt
/// and answers in text: it calls no tools by design, so the pane found nothing
/// to show and said "no tool output in this subagent's scrollback" — throwing
/// away the entire point of having run it.
///
/// Measured in the operator's store, 2026-09-20. Six subagents spawned to scout
/// a transcript: one user item, one reasoning item, one assistant item each, and
/// ZERO tool results. Their words: *"i see them and i see their output but when
/// i enter - no output"*. The output was there; this function did not look at it.
///
/// Reasoning is still left out — it is the model thinking, not its answer, and
/// that is the same line `compaction::harvest` draws for the same reason.
pub(crate) fn subagent_out_lines(events: &[Envelope]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut spills: Vec<String> = Vec::new();
    for env in events {
        match &env.event {
            SessionEvent::TranscriptContent { item, .. } => match &**item {
                TranscriptItem::ToolResult {
                    name,
                    outcome,
                    payload,
                    ..
                } => {
                    out.push(format!("· {name} — {}", outcome_word(outcome)));
                    // **§3.1: a subagent's scrollback is content this head did not
                    // author** — it is another session's tool payloads and prose, read
                    // back over a `Peek` and drawn into this head's frame. Sanitised
                    // here, at the one builder, rather than in `sub_out_lines`, so the
                    // spill file gets the same text the pane draws.
                    for line in without_control_lines(payload).lines() {
                        out.push(format!("  {line}"));
                    }
                    out.push(String::new());
                }
                // The answer, in the order it was given, so a subagent that
                // worked and then reported reads as the one conversation it was.
                TranscriptItem::Assistant { text, .. } if !text.trim().is_empty() => {
                    for line in without_control_lines(text).lines() {
                        out.push(line.to_string());
                    }
                    out.push(String::new());
                }
                _ => {}
            },
            SessionEvent::ToolFinished {
                spill: Some(path), ..
            } => spills.push(path.clone()),
            _ => {}
        }
    }
    if !spills.is_empty() {
        out.push("full output on disk:".to_string());
        for s in spills {
            out.push(format!("  {s}"));
        }
        out.push(String::new());
    }
    out
}
