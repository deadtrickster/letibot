//! **What a project's model is told when its target is in trouble** (R76 item 1) — the words
//! `Harness::queue_emergency_notices` puts in front of a root session's model, once per red tip
//! or parked row.
//!
//! The failing step and the end of its output come FIRST: the operator's live case was a gate
//! that failed after five green steps on one line at the very end, and a message that opened with
//! the queue's policy buried the only line that said what was wrong. Then what the session can
//! do about it with the `queue` tool — and what is the operator's to decide (`[queue] target` in
//! the project's `leticode.toml`), which the tool may only propose.

use letibot_tokencore::store::{
    MergeGateOutcome, MergeGateStep, ReviewKind, ReviewRow, ReviewState, TargetHealth,
};

use super::setup::GATE_SETUP_FAILED;

/// The lines of a failing step's output a message carries.
pub const TAIL_LINES: usize = 30;

/// **The step that did not pass**, numbered from 1, and the end of its output.
pub fn failing_step(steps: &[MergeGateStep]) -> Option<(usize, usize, &MergeGateStep, String)> {
    let i = steps.iter().position(|s| {
        matches!(
            s.outcome,
            MergeGateOutcome::Failed | MergeGateOutcome::TimedOut | MergeGateOutcome::Killed
        )
    })?;
    let s = &steps[i];
    let lines: Vec<&str> = s.output.lines().collect();
    let tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    Some((i + 1, steps.len(), s, tail))
}

fn step_and_tail(h: &TargetHealth) -> String {
    match failing_step(&h.gate_steps) {
        Some((n, of, s, tail)) => format!(
            "The failing step is {n} of {of}, `{}`. The end of its output:\n\n```\n{}\n```",
            s.command,
            if tail.trim().is_empty() {
                "(it printed nothing)"
            } else {
                tail.as_str()
            }
        ),
        None => format!("The queue's words:\n\n```\n{}\n```", h.evidence),
    }
}

const TOOL_WAYS: &str = "With the `queue` tool you can read both queues and the gate's evidence \
     (`action: \"status\"`), gate the tip again once the cause is gone — a flake, the machine, a \
     gate fixed outside the target (`\"regate\"`), withdraw a row that no longer means anything \
     (`\"withdraw\"`), and restart a parked one (`\"restart\"`). If the gate is wrong for this \
     branch — it names another branch's artifact, or the project lands on another branch — say \
     so: `\"propose_target\"` puts the `[queue] target` line of the project's `leticode.toml` to \
     the operator, whose decision it is.";

/// **The target's own trouble** — red, timed out, or unjudged because the gate's setup failed.
pub fn health(h: &TargetHealth) -> String {
    use letibot_tokencore::store::HealthState as H;
    let at = format!(
        "`{}` at {} in {}",
        h.target,
        h.sha.chars().take(12).collect::<String>(),
        h.repository
    );
    let first = step_and_tail(h);
    match h.state {
        H::Blocked if h.evidence.starts_with(GATE_SETUP_FAILED) => format!(
            "The merge queue CANNOT JUDGE {at}: {}.\n\n{first}\n\nThis is not the code being red \
             — the gate never got to judge it — but nothing lands on `{}` until it does. Fix the \
             gate (the `Merge gate` section of AGENTS.md on `{}`, or the script it runs), then \
             gate the tip again. {TOOL_WAYS}",
            h.evidence
                .lines()
                .next()
                .unwrap_or_default()
                .trim_end_matches('.'),
            h.target,
            h.target,
        ),
        H::TimedOut => format!(
            "The merge queue's gate of {at} TIMED OUT, so the tip is unjudged and nothing lands \
             on `{}`.\n\n{first}\n\n{TOOL_WAYS}",
            h.target
        ),
        _ => {
            // The id `RepoQueue::target_went_red` gives the red tip's review.
            let review = format!(
                " (`red-{}-{}`)",
                h.target.replace('/', "-"),
                h.sha.chars().take(12).collect::<String>()
            );
            format!(
                "EMERGENCY — {at} is RED: its own merge gate fails there.\n\n{first}\n\nNothing \
                 lands on `{target}` until it is green again (only a branch opened to fix it \
                 does). The red tip is first in the review queue{review}; a reviewer looks for \
                 what in `{target}`'s history broke it. {TOOL_WAYS}",
                target = h.target,
            )
        }
    }
}

/// **A row parked for a person** — a review that cannot run, or a reviewer that cannot decide.
pub fn parked(r: &ReviewRow) -> String {
    let reasons = if r.reasons.is_empty() {
        "it gave no reasons".to_string()
    } else {
        r.reasons.join("; ")
    };
    let failure = if r.failure.trim().is_empty() {
        r.evidence.as_str()
    } else {
        r.failure.as_str()
    };
    match (r.kind, r.state) {
        (ReviewKind::Target, ReviewState::Blocked) => format!(
            "The review of `{target}`'s red tip {head} (`{id}`) is BLOCKED: {failure}\n\nNothing \
             reviews it until somebody acts. `queue` with `action: \"restart\"` and `id: \"{id}\"` \
             asks again; if `{target}` has been rewritten past {head} the row is superseded by \
             itself, and a stale one can be withdrawn (`\"withdraw\"`). Once the cause of the red \
             is gone, `\"regate\"` gates the tip again.",
            target = r.target,
            head = r.head_sha,
            id = r.id,
        ),
        (ReviewKind::Target, _) => format!(
            "The reviewer of `{target}`'s red tip {head} (`{id}`) could not decide: {reasons}. The \
             operator is asked. If you can tell what broke `{target}`, say so with the evidence; \
             once the cause is gone, `queue` with `action: \"regate\"` gates the tip again.",
            target = r.target,
            head = r.head_sha,
            id = r.id,
        ),
        (ReviewKind::Branch, ReviewState::Blocked) => format!(
            "The review of `{branch}` (`{id}`), a branch of your tree, is BLOCKED and nothing \
             lands it: {failure}\n\n`queue` with `action: \"restart\"` and `id: \"{id}\"` asks a \
             reviewer again; the operator's `/queue approve {id}` approves it over the reviewer \
             (the merge gate still runs).",
            branch = r.branch,
            id = r.id,
        ),
        (ReviewKind::Branch, _) => format!(
            "The reviewer of `{branch}` (`{id}`), a branch of your tree, could not decide: \
             {reasons}. The operator is asked (`/queue approve {id}` or `/queue veto {id} WHY`). \
             If you can answer what the reviewer could not tell, do it in the branch: a new head \
             goes back to review by itself.",
            branch = r.branch,
            id = r.id,
        ),
    }
}
