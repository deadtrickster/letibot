//! `/clear`: a fresh conversation for the same session. `/compact` summarises the history in the drifted voice and
//! `/reseat` carries it verbatim, so neither cures a degraded model. Everything the operator keeps lives in the
//! `Harness` and not its transcript, which is why this forks: a new message zero and one orientation item, no old prose.

use std::time::Duration;

use letibot_sessionlog::SessionEvent;
use letibot_transcript::{Speaker, TranscriptItem, UserPart};
use letibot_turn::CompactionOutcome;

use super::{ForkReport, ForkTail, Harness, HarnessError};

/// What a clear did - the fork, the orientation it opened with, and what message zero gained
/// and lost when it was recomposed.
#[derive(Debug, Clone)]
pub struct ClearReport {
    pub fork: ForkReport,
    /// The new transcript's first item, verbatim - so the reply can say what the model was told
    /// and a test can assert it without reading the store.
    pub orientation: String,
    pub gained: Vec<String>,
    pub lost: Vec<String>,
}

impl Harness {
    /// Clear the conversation and keep the session: a fork onto a fresh message zero whose only
    /// item is the orientation. `next` is the operator's optional sentence, quoted as theirs, and
    /// no turn runs for it. Refuses without a store, because a fork must keep what it leaves.
    pub fn clear_conversation(&mut self, next: Option<&str>) -> Result<ClearReport, HarnessError> {
        if self.store.is_none() {
            return Err(HarnessError::Setup(
                "clearing starts a new transcript for this session and keeps the old one in the \
                 store, and this session has no store to keep it in. Nothing was changed."
                    .into(),
            ));
        }
        // Message zero as a NEW conversation would get it, so the cleared conversation does not
        // speak a prompt that went stale while the old one ran. `None` means keep the current one.
        let reseat = self.reseat_target()?;
        let old_id = self.transcript_id.clone();
        let orientation = self.orientation(next);
        let lead = TranscriptItem::User {
            speaker: Speaker::Agent,
            parts: vec![UserPart::Text {
                text: orientation.clone(),
            }],
        };
        // No summary turn, so the outcome is the empty one - the same record `reingest` writes
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
            budget: None,
            fidelity: None,
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
        // Only after the fork landed - `adopt_reseat`'s own ordering rule.
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

    /// The sentence a clear is announced with on the session's log: the boundary row a head draws
    /// where the old conversation ended. One function so the root's arm and a child's loop agree.
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

    /// The orientation: the session's state as facts, and no prose from the old conversation. Each section is read at
    /// the moment of the clear from the thing that owns it, and an empty section says so in one line rather than
    /// vanishing, because an orientation that leaves out no jobs cannot be told from one that forgot to look.
    pub(crate) fn orientation(&self, next: Option<&str>) -> String {
        let me = self.cfg.session_id.clone();
        let mut out = format!(
            "[cleared] The operator cleared this conversation at their request. Nothing said \
             before this point is carried — not even a summary — so do not assume any decision, \
             finding or instruction from it. This is the same session (`{me}`): its board, jobs, \
             subagents, workspace and settings carried on, and they are listed below as they \
             stand now. The old conversation is still in the store and the `transcript` tool \
             reaches it, because its `history` is on by default and the search walks back through \
             the transcripts this session came from.\n"
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

        // Jobs: the backgrounded and the still running - `worth_listing`'s rule, the same rows
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

        // The queues for the repository this workspace is in - `queue status`'s own reading.
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

/// The verb's own answer, beside the `cleared` row: what the operator does next. Short, because
/// the row above it already said what happened.
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
