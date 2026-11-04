//! The subagents pane: the tree of children this session spawned, and a child's output.

use crate::app::*;
use crate::ui::render::row_strings;
use crate::ui::*;
use letibot_sessionlog::event::{Envelope, SessionEvent};
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::TranscriptItem;
use letibot_ui::text::without_control_lines;
use rano::agent::subagents::{SubagentOutput, SubagentRow, SubagentsPane};

impl App {
    /// The subagent tree: the children this session spawned, drawn from [`App::subagent_stops`] -
    /// the list the arrows, Enter and `p` read - so the drawn mark and the key that acts cannot
    /// disagree; `&mut self` records those rows, which is what the arrows scroll to.
    pub(crate) fn subagents_lines(&mut self, w: usize) -> Vec<String> {
        let content = self.subagents_view().content(w);
        self.subagents_stop_rows = content.stop_rows;
        row_strings(&content.lines, self.cfg.palette())
    }

    /// The subagents pane's facts, in rano's words: each child's state and what it was
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
                    // The child by its name, `fix-login · …14688401`, where the pane drew the
                    // bare short id - the operator's *"slugs for subagents too"*. A child nobody
                    // named still draws its short id alone.
                    session: s.name(),
                    role: s.role.clone(),
                    model: s.model.clone(),
                    generating: s.generating,
                    answer: s.answer.as_deref().map(one_plain_line),
                })
                .collect(),
            finished_open: self.subagents_finished_open,
            selected: self.subagents_sel,
        }
    }

    /// A peeked session's rows, drawn by the renderer every other row goes through - markdown, the
    /// air rule and tool cards included - from `Peeked::snapshot`'s items, once when they land
    /// rather than per frame. `targets` is seeded from the snapshot's own turn; absent, not invented.
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
        let no_tails = std::collections::HashMap::new();
        let mut out: Vec<String> = Vec::new();
        // The walk's own separator rule: air where the KIND changes, and none between two activity
        // rows. One rule for the transcript and for this pane, or the pane is a second renderer
        // again in the one place nobody would look.
        let mut prev: Option<RowClass> = None;
        for (i, it) in items.iter().enumerate() {
            let answered = round_results(items, i);
            let re = question_before(items, i);
            let ctx = ItemCtx {
                cfg: &cfg,
                think: Fold::Folded,
                tools: Fold::Folded,
                raw: false,
                targets: &targets,
                answered: &answered,
                subagents: &self.subagents,
                // No tails for a peeked child: the jobs it names are its own, and this head
                // asks only for the output of the session it is attached to.
                job_tails: &no_tails,
                // No durations, no diffs and no approvals: this head did not watch these calls run,
                // and the maps that carry them are keyed by THIS session's item ids. The renderer
                // already says *replayed* for all three rather than inventing one.
                drawn_live: false,
                // Not this head's session: its cited files are compared with nothing.
                workspace: "",
                elapsed_ms: None,
                edit: None,
                decision: None,
                bound: None,
                // Nothing here is this head's own echo, so the mark never appears.
                echo_mark: QUEUED,
                echo_open: false,
                // The lifted set, because this pane exists to show a child's rows whole: it is
                // the same "the rung lifted" the open run draws its own rows with, so a peeked
                // session is not filtered by whatever profile the parent happens to be in.
                vis: Visibility::lifted(),
                diff_split: self.diff_split,
                payload_view: None,
                payload_max: None,
                window_rows: usize::MAX,
                payload_newest: None,
                replied_to: re.as_deref(),
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

    /// The output view: the session's own rows, as a terminal scrolls them - or, when the daemon
    /// answered with its event ring, the plain fallback sayings so on the screen.
    pub(crate) fn sub_out_lines(&mut self, room: usize) -> Vec<String> {
        let p = self.cfg.palette();
        let session = match self.sub_out.as_ref() {
            Some(v) => self.child_label(&v.session_id),
            None => return Vec::new(),
        };
        let Some(v) = self.sub_out.as_mut() else {
            return Vec::new();
        };
        let frame = SubagentOutput {
            session,
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

/// A subagent's output: its tool results and its own prose, in the order they were given - a
/// `digest` child answers in text and calls no tools, so tool results alone showed nothing. Control
/// lines are stripped here, at the one builder, and the spill paths are named at the end.
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
                    // A subagent's scrollback is content this head did not author, so it is
                    // sanitised here at the one builder rather than in `sub_out_lines`, and the spill
                    // file gets the same text the pane draws.
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
