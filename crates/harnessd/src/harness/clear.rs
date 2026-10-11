//! **`/clear`: a fresh conversation for the same session.**
//!
//! The operator, verbatim: *"I need to be able to clear context - fresh reseat without loosing
//! session todos and jobs"*, *"and subagents"*, *"this will allow me to fix model degradation"*.
//!
//! # What it is for, and why neither verb it sits beside does it
//!
//! A long session's model degrades. MEASURED 2026-10-11 on pg-noop: 710k of a 1M window, the
//! reasoning full of `✓ ✗ ✓ ✓` glyph spam and the same boilerplate repeated round after round.
//! `/compact` does not cure that — it summarises the history, and the summary is written by the
//! drifted model in its own drifted voice, which then becomes the base every later turn imitates.
//! `/reseat` changes message zero and carries every item across verbatim, which keeps the drift
//! whole. What the operator asked for is the third thing: **the conversation starts over and the
//! session does not**.
//!
//! # What carries on, and why none of it needed carrying
//!
//! Everything the operator listed lives in the [`Harness`], not in its transcript, so a fork that
//! swaps the transcript leaves all of it standing — that is the whole design, and it is why this
//! is a fork rather than a new session:
//!
//! * the **id and title** — a fork is a new `<session>#tN` transcript under the same session row;
//! * the **todo board** — `self.todos`, persisted in the store's `todo` table keyed by session;
//! * **jobs** — the process host is the backend's, and [`JobWatchers`] queue a settlement for
//!   the SESSION (`jobwatch.rs`'s module header), so a job started under the old transcript
//!   is delivered by the next wake into the new one;
//! * **subagents** — the runner's slots are the harness's, a child's registry parent is the
//!   session id, and a child's settlement rides the same watchers as a job;
//! * **workspace, seat, mode, notes, queue rows** — the harness's configuration and the store.
//!
//! # What the new conversation starts with
//!
//! Message zero, freshly composed ([`Harness::reseat_target`]: the prompt, the tools and the
//! standing notes this daemon would seat a NEW conversation with), then ONE item: an orientation
//! the harness writes from STATE — the board, the jobs, the children, the queue — spoken as
//! [`letibot_transcript::Speaker::Agent`], because it is not the operator's words and must never
//! authorise anything. **It carries nothing of the old conversation's prose**: no summary, no
//! last answer, no reasoning. That is exactly the degradation being cleared, and a "short recap"
//! written by the model that drifted would hand the drift straight back.
//!
//! The old transcript stays in the store, whole and resumable; the orientation names it, so the
//! operator can point at something in it and the model can read it with `transcript`.

use std::time::Duration;

use letibot_sessionlog::SessionEvent;
use letibot_transcript::{Speaker, TranscriptItem, UserPart};
use letibot_turn::CompactionOutcome;

use super::{ForkReport, ForkTail, Harness, HarnessError};

/// What a clear did — the fork, the orientation it opened with, and what message zero gained
/// and lost when it was recomposed.
#[derive(Debug, Clone)]
pub struct ClearReport {
    pub fork: ForkReport,
    /// The new transcript's first item, verbatim — so the reply can say what the model was told
    /// and a test can assert it without reading the store.
    pub orientation: String,
    pub gained: Vec<String>,
    pub lost: Vec<String>,
}

impl Harness {
    /// **Clear the conversation and keep the session** — `/clear [what to do next]`.
    ///
    /// A fork onto a freshly composed message zero whose body is one orientation item and nothing
    /// else. `next` is the operator's optional sentence, quoted in the orientation as theirs; no
    /// turn is run for it, because the slash verb is the operator's act on the session and the
    /// next prompt is theirs to send.
    ///
    /// Refuses without a store — a fork needs somewhere to keep the conversation it leaves — and
    /// that is the only refusal: unlike `/reseat` there is no "nothing to do", because the history
    /// is what is being dropped and an unchanged message zero is simply re-used.
    pub fn clear_conversation(&mut self, next: Option<&str>) -> Result<ClearReport, HarnessError> {
        if self.store.is_none() {
            return Err(HarnessError::Setup(
                "clearing starts a new transcript for this session and keeps the old one in the \
                 store, and this session has no store to keep it in. Nothing was changed."
                    .into(),
            ));
        }
        // **Message zero as a NEW conversation would get it** — the same computation every
        // compaction and `/reseat` makes, so the cleared conversation is not left speaking a
        // prompt that went stale while the old one ran. `None` is the prompt already being
        // spoken, and then the fork keeps it.
        let reseat = self.reseat_target()?;
        let old_id = self.transcript_id.clone();
        let orientation = self.orientation(&old_id, next);
        let lead = TranscriptItem::User {
            speaker: Speaker::Agent,
            parts: vec![UserPart::Text {
                text: orientation.clone(),
            }],
        };
        // No summary turn, so the outcome is the empty one — the same record `reingest` writes
        // for the same reason. The fork reads it only for its `truncated` flag and its own note,
        // and the note is replaced by `lead`.
        let outcome = CompactionOutcome {
            turn_id: format!("{old_id}#clear"),
            summary: String::new(),
            tool_calls: 0,
            truncated: false,
            cached_tokens: 0,
            reusable: 0,
            generated_tokens: 0,
        };
        self.compacting = true;
        let fork = self.fork_onto(
            &outcome,
            reseat.as_ref().map(|(p, _)| p),
            reseat.as_ref().map(|(_, id)| id.as_str()),
            ForkTail::NONE,
            Some(lead),
        );
        self.compacting = false;
        let fork = fork?;
        // Only after the fork landed — `adopt_reseat`'s own ordering rule.
        let (mut gained, mut lost) = self.adopt_reseat(reseat);
        gained.sort();
        lost.sort();
        self.publish_settings();
        Ok(ClearReport {
            fork,
            orientation,
            gained,
            lost,
        })
    }

    /// **The sentence a clear is announced with on the session's log** — the boundary row a head
    /// draws where the old conversation ended. One function so the root's arm and a child's
    /// serving loop say the same thing.
    pub fn publish_cleared(&self, r: &ClearReport) {
        let mut said = format!(
            "conversation cleared: {} → {}. The new conversation is message zero plus a {}-line \
             orientation built from this session's board, jobs, subagents and queue — {} tokens, \
             down from {}. Nothing of the old conversation was summarised or carried; it is \
             still in the store as {}.",
            r.fork.parent_id,
            r.fork.transcript_id,
            r.orientation.lines().count(),
            r.fork.base_tokens,
            r.fork.was_tokens,
            r.fork.parent_id,
        );
        if !r.gained.is_empty() {
            said.push_str(&format!(
                " Message zero was recomposed and the model can now call: {}.",
                r.gained.join(", ")
            ));
        }
        if !r.lost.is_empty() {
            said.push_str(&format!(" It has lost: {}.", r.lost.join(", ")));
        }
        self.hub.publish(SessionEvent::Warning {
            code: "cleared".into(),
            detail: said,
            compaction: None,
        });
    }

    /// **The orientation: the session's state, as facts, and no prose from the old
    /// conversation.**
    ///
    /// Each section is read from the thing that owns it — the board, the process host, the
    /// subagent runner, the store's queues — at the moment of the clear. An empty section says so
    /// in one line rather than vanishing, because an orientation that leaves out "no jobs" is one
    /// the model cannot tell from an orientation that forgot to look.
    pub(crate) fn orientation(&self, old_id: &str, next: Option<&str>) -> String {
        let me = self.cfg.session_id.clone();
        let mut out = format!(
            "[cleared] The operator cleared this conversation at their request. Nothing said \
             before this point is carried — not even a summary — so do not assume any decision, \
             finding or instruction from it. This is the same session (`{me}`): its board, jobs, \
             subagents, workspace and settings carried on, and they are listed below as they \
             stand now. The old conversation is transcript `{old_id}` in the store; if the \
             operator points at something in it, read it with the `transcript` tool rather than \
             guessing.\n"
        );
        if let Some(next) = next.map(str::trim).filter(|t| !t.is_empty()) {
            out.push_str(&format!("\nThe operator said, with the clear: «{next}»\n"));
        }

        // The board, in the words `todo_write` answers with.
        let todos = self.todos.snapshot();
        out.push_str("\nTodo board: ");
        out.push_str(&letibot_tools::builtins::todo::board_text(&todos));
        if !out.ends_with('\n') {
            out.push('\n');
        }

        // Jobs: the backgrounded and the still running — `worth_listing`'s rule, the same rows
        // `/job` and the jobs pane show.
        let jobs: Vec<String> = match self.runtime.backend.processes() {
            None => vec!["  (this session has no process host, so it has no jobs)".into()],
            Some(host) => host
                .jobs()
                .iter()
                .filter(|j| Self::worth_listing(j))
                .map(|j| {
                    format!(
                        "  {}  {}  {}  {}",
                        j.label(),
                        j.state.word(),
                        how_long(j.ran_for.unwrap_or(j.elapsed)),
                        Self::one_line(&j.command, 120)
                    )
                })
                .collect(),
        };
        if jobs.is_empty() {
            out.push_str("\nJobs: none backgrounded or running.\n");
        } else {
            out.push_str(&format!(
                "\nJobs ({}) — a running one's completion still reaches you on its own:\n{}\n",
                jobs.len(),
                jobs.join("\n")
            ));
        }

        // Subagents this session started that are still here: in a turn, or between turns and
        // addressable. A child that failed or whose thread has gone is history, not state.
        let mut children = Vec::new();
        for handle in self.subagents.started() {
            if self.session_registry.parent_of(&handle).as_deref() != Some(me.as_str()) {
                continue;
            }
            let status = self.subagents.collect(&handle, Duration::ZERO);
            use letibot_tools::builtins::task::TaskStatus;
            let state = match &status {
                TaskStatus::Running { .. } => status
                    .doing()
                    .map(|d| d.said().to_string())
                    .unwrap_or_else(|| "running".into()),
                TaskStatus::Done { child: Some(_), .. } => status
                    .doing()
                    .map(|d| format!("answered; {}", d.said()))
                    .unwrap_or_else(|| "answered".into()),
                TaskStatus::Done { child: None, .. }
                | TaskStatus::Failed { .. }
                | TaskStatus::Unknown => continue,
            };
            let asked = self
                .session_registry
                .title_of(&handle)
                .filter(|t| !t.trim().is_empty())
                .map(|t| one_line(&t, 120))
                .unwrap_or_else(|| "(no brief recorded)".into());
            children.push(format!("  {handle}  {state}  — asked: {asked}"));
        }
        if children.is_empty() {
            out.push_str("\nSubagents: none live.\n");
        } else {
            out.push_str(&format!(
                "\nSubagents ({}) — still yours; their answers reach you on their own, \
                 `task_result` reads one and `task_message` speaks to one:\n{}\n",
                children.len(),
                children.join("\n")
            ));
        }

        // The queues for the repository this workspace is in — `queue status`'s own reading.
        match (
            self.store.as_ref(),
            crate::mergequeue::desk::resolve(&self.cfg.workspace, None, None),
        ) {
            (Some(store), Ok((repo, _))) => {
                match crate::mergequeue::desk::status(store, Some(&repo)) {
                    Ok(s) => out.push_str(&format!("\nQueue for {repo}:\n{}", s.trim_end())),
                    Err(e) => out.push_str(&format!("\nQueue for {repo}: could not be read ({e})")),
                }
                out.push('\n');
            }
            (_, Err(_)) => out.push_str(
                "\nQueue: the workspace is not in one git repository, so there is none.\n",
            ),
            (None, Ok(_)) => {}
        }
        out.trim_end().to_string()
    }
}

/// **The verb's own answer**, beside the `cleared` row: what the operator does next. Short,
/// because the row above it already said what happened.
pub fn clear_reply(r: &ClearReport) -> String {
    format!(
        "cleared: now on {}. The model has not been asked anything yet — your next prompt is the \
         first thing it answers, on a conversation that starts with the orientation.",
        r.fork.transcript_id
    )
}

/// A duration as a person reads it in a row: `12s`, `4m`, `2h05m`.
fn how_long(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

fn one_line(s: &str, n: usize) -> String {
    super::one_line(s, n)
}

#[cfg(test)]
mod tests {
    use super::how_long;
    use std::time::Duration;

    #[test]
    fn a_jobs_age_reads_the_way_a_row_is_read() {
        assert_eq!(how_long(Duration::from_secs(12)), "12s");
        assert_eq!(how_long(Duration::from_secs(250)), "4m");
        assert_eq!(how_long(Duration::from_secs(7500)), "2h05m");
    }
}
