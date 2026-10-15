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
    MergeGateOutcome, MergeGateStep, MergeRow, ReviewKind, ReviewRow, ReviewState, TargetHealth,
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
    let output = super::gate::for_the_model(&s.output);
    let lines: Vec<&str> = output.lines().collect();
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
        None => format!(
            "The queue's words:\n\n```\n{}\n```",
            super::gate::for_the_model(&h.evidence)
        ),
    }
}

const TOOL_WAYS: &str = "With the `queue` tool you can read both queues and the gate's evidence \
     (`action: \"status\"`), gate the tip again once the cause is gone — a flake, the machine, a \
     gate fixed outside the target (`\"regate\"`), withdraw a row that no longer means anything \
     (`\"withdraw\"`), and restart a parked one (`\"restart\"`). If the gate is wrong for this \
     branch — it names another branch's artifact, or the project lands on another branch — say \
     so: `\"propose_target\"` puts the `[queue] target` line of the project's `leticode.toml` to \
     the operator, whose decision it is.";

/// **The hub warning for a tip the gate's own setup kept it from judging** (`gate_setup_failed`),
/// raised by `RepoQueue` beside the session notice [`health`] writes. `key` is the repository's
/// queue key. The TUI's red card reads it as one line; its shape is pinned by the fixture test
/// below.
pub fn gate_setup_failed(h: &TargetHealth, key: &str) -> String {
    format!(
        "`{}` at {} in {} is UNJUDGED, not red: {}. Nothing lands on it until its gate can judge \
         it — fix the gate (or the project's `[queue] target`), then `/queue regate {}`.",
        h.target,
        h.sha,
        key,
        h.evidence
            .lines()
            .next()
            .unwrap_or_default()
            .trim_end_matches('.'),
        h.target
    )
}

/// **The hub warning for a tip whose gate step was killed before it judged the code**
/// (`gate_killed`) — at the gate's memory cap, or by a signal. The same shape as
/// [`gate_setup_failed`], the evidence's first line saying which.
pub fn gate_killed(h: &TargetHealth, key: &str) -> String {
    format!(
        "`{}` at {} in {} is UNJUDGED, not red: {}. Nothing lands on it until its gate can judge \
         it — then `/queue regate {}`.",
        h.target,
        h.sha,
        key,
        h.evidence
            .lines()
            .next()
            .unwrap_or_default()
            .trim_end_matches('.'),
        h.target
    )
}

/// **`target_red`** (`RepoQueue::target_went_red`): the emergency, on the repository's hubs.
/// `again` when the same tip was green in between (R92.31). A red target halts both queues
/// (R92.18), and the sentence says so.
pub fn target_red_warning(h: &TargetHealth, key: &str, again: bool) -> String {
    format!(
        "EMERGENCY: `{}` is RED{} at {} in {}. The merge queue lands nothing until it is green \
         again (a fix branch excepted); the red tip is first in review; no other review of it \
         starts and approved branches go back to review; enqueueing goes on. {}",
        h.target,
        if again { " again" } else { "" },
        h.sha,
        key,
        h.evidence
    )
}

/// **`target_green`** (`RepoQueue::target_is_green`): an alarm about the target stood, and it is
/// over — said once per red→green, where the alarm was (R92.6).
pub fn green_again(h: &TargetHealth, key: &str) -> String {
    format!(
        "`{}` is green again at {} in {}: the merge queue lands again, and the reviews it held \
         go on.",
        h.target, h.sha, key
    )
}

/// **`target_timed_out`** (R92.27): the target's gate ran past its deadline.
pub fn target_timed_out(h: &TargetHealth, key: &str) -> String {
    format!(
        "`{}` at {} in {} is UNJUDGED: its gate TIMED OUT ({}). Nothing lands on it until it is \
         gated again — `/queue regate {}`.",
        h.target,
        h.sha,
        key,
        first_line(&h.evidence),
        h.target
    )
}

/// **`target_unjudged`** (R92.27): the gate could not be read, the landing tree could not be
/// made ready, or a step could not run — nothing about the code was judged.
pub fn target_unjudged(h: &TargetHealth, key: &str) -> String {
    format!(
        "`{}` at {} in {} is UNJUDGED, not red: {}. Nothing lands on it until it is gated again — \
         once the cause is gone, `/queue regate {}`.",
        h.target,
        h.sha,
        key,
        first_line(&h.evidence),
        h.target
    )
}

/// **`merge_conflict`** (R92.28): an approved branch does not rebase onto the target.
pub fn merge_conflict(m: &MergeRow, why: &str) -> String {
    format!(
        "{why}. Nothing lands it: rebase `{}` onto `{}` (or resolve it on the branch) and commit; \
         the new head goes back to review. `/queue rm {}` withdraws it.",
        m.branch, m.target, m.id
    )
}

/// **`merge_failed`** (R92.28): an approved branch's landing gate failed — the branch is bad on
/// the target's tip.
pub fn merge_failed(m: &MergeRow, words: &str) -> String {
    format!(
        "the landing gate of `{}` (`{}`) FAILED on `{}`, so it did not land: {}. Fix the branch \
         and commit; the new head goes back to review.",
        m.branch,
        m.id,
        m.target,
        first_line(words)
    )
}

/// **A branch row's landing gate killed at its memory cap** (R92.25) — it ran and was killed; the
/// verb that gates a branch row again is `restart`.
pub fn merge_gate_killed(m: &MergeRow, words: &str) -> String {
    format!(
        "the landing gate of `{}` was KILLED at its memory cap, so it is unjudged and parked: {}. \
         Raise `[memory] gate` in providers.toml, then `/queue restart {}` gates it again.",
        m.branch,
        first_line(words),
        m.id
    )
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or_default().trim_end_matches('.')
}

/// **The target's own trouble** — red, timed out, or unjudged because the gate's setup failed.
pub fn health(h: &TargetHealth) -> String {
    use letibot_tokencore::store::HealthState as H;
    let at = format!(
        "`{}` at {} in {}",
        h.target,
        h.sha.chars().take(12).collect::<String>(),
        h.repository
    );
    let mut first = step_and_tail(h);
    // The landing tree's missing gitignored inputs, when the gate's failure named them
    // (`carry::hint`): firecode's red was `images/` and `vendor/` absent, not the code.
    if let Some(lacks) = super::carry::hint_in(&h.evidence) {
        first.push_str("\n\n");
        first.push_str(lacks);
    }
    match h.state {
        H::Blocked if h.evidence.starts_with(super::gate::GATE_KILLED) => format!(
            "The merge queue CANNOT JUDGE {at}: {}.\n\n{first}\n\nThis is not the code being red \
             — the gate's step was killed before it judged anything — but nothing lands on `{}` \
             until it is gated again. If it was the memory cap, that is the operator's \
             `[memory] gate` in providers.toml; once there is room, gate the tip again. \
             {TOOL_WAYS}",
            h.evidence
                .lines()
                .next()
                .unwrap_or_default()
                .trim_end_matches('.'),
            h.target,
        ),
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
                 does), no other review of it starts, and branches already approved go back to \
                 review. The red tip is first in the review queue{review}; a reviewer looks for \
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

// **The rest of what the review queue says about a verdict**, moved here from where each was
// built inline (`review`, `serve`, `verbs`, `Harness::handover_notices`) — byte for byte — so
// that one test can write the TUI's review-card fixtures from the daemon's own words. The TUI
// draws each as one line (`ui::transcript::review_card`); the model reads every word.

/// The reviewer's reasons, one bullet each, as a refusal lists them.
fn reason_list(reasons: &[String]) -> String {
    reasons
        .iter()
        .map(|x| format!("  - {x}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// **A refusal, to the child that wrote the branch** (`review::deliver_refusal`).
pub fn refused(r: &ReviewRow, reasons: &[String]) -> String {
    let list = reason_list(reasons);
    let wt = r.worktree.as_deref().unwrap_or("its worktree");
    format!(
        "Your branch `{branch}` was REFUSED by the review at {head}, so it does not land. The \
         reviewer's reasons:\n\n{list}\n\nThe branch is still in {wt}. Answer the reasons there \
         and commit: a new head goes back to review by itself (the verdict is bound to the head it \
         judged).",
        branch = r.branch,
        head = r.head_sha,
    )
}

/// **A refusal, to the root of a child that could not be told** (`review::deliver_refusal`);
/// `missed` is why the child was not.
pub fn refused_to_root(r: &ReviewRow, reasons: &[String], child: &str, missed: &str) -> String {
    let list = reason_list(reasons);
    let wt = r.worktree.as_deref().unwrap_or("its worktree");
    format!(
        "The review of `{branch}` — the branch of a child you started (`{child}`), which could \
         not be told ({why}) — REFUSED it at {head}, so it does not land. The reviewer's \
         reasons:\n\n{list}\n\nThe branch is in {wt}, and it is yours to move: answer the \
         reasons there and commit (a new head goes back to review by itself), or ask the \
         operator for `/queue approve {id}` or `/queue rm {id}`.",
        branch = r.branch,
        head = r.head_sha,
        why = missed,
        id = r.id,
    )
}

/// **`refusal_undelivered`**: a refusal nobody could be told of (`review::deliver_refusal`).
pub fn refusal_undelivered(r: &ReviewRow, missed: &str) -> String {
    format!(
        "the review of `{}` (`{}`) refused it, and the complaints reached nobody ({}). The \
         verdict stands and is on the row; `/queue restart {}`, `/queue approve {}` or \
         `/queue rm {}`.",
        r.branch, r.id, missed, r.id, r.id, r.id
    )
}

/// **`queue_needs_human`**: the reviewer could not decide (`review::verdict`). A review that
/// stops because its reviewer could not decide says THAT — the head, the sentence the verdict
/// turned on (`decisive`: the reviewer's own words, chosen by `capacity::decisive_reason`, not
/// paraphrased), how many times reviewers could not decide on this branch and at which heads
/// (`heads`, oldest first, this one included), and the operator's verbs. A failure to place or
/// run a reviewer is not mentioned: it is not why this row stopped. The hub row of 2026-10-12
/// named a placement error for a branch whose history was three `could not decide` verdicts
/// (TODO R86).
pub fn needs_human(r: &ReviewRow, by: &str, decisive: &str, heads: &[String]) -> String {
    let short = |s: &str| s.chars().take(7).collect::<String>();
    let decisive = if decisive.is_empty() {
        "it gave no reasons"
    } else {
        decisive
    };
    let times = if heads.len() > 1 {
        format!(
            " That is {} × could not decide on this branch (at {}); every one was a verdict, \
             none a review that failed to run.",
            heads.len(),
            heads
                .iter()
                .map(|h| short(h))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        String::new()
    };
    match r.kind {
        ReviewKind::Branch => format!(
            "the review of `{branch}` (`{id}`) stops for a person: the reviewer could not decide at \
             {head} — \"{decisive}\" ({by}; every reason is on the row).{times} What the operator \
             can do: `/queue approve {id}` approves it over the reviewer at its head now (the \
             merge gate still runs); `/queue veto {id} WHY` sends it back to its author with your \
             words; `/queue rm {id}` withdraws it. A changed ask is answered on the branch: a new \
             head goes back to review by itself.",
            branch = r.branch,
            id = r.id,
            head = short(&r.head_sha),
        ),
        ReviewKind::Target => format!(
            "the review of `{target}`'s red tip (`{id}`) stops for a person: the reviewer could \
             not decide at {head} — \"{decisive}\" ({by}; every reason is on the row).{times} \
             `/queue regate` gates the tip again once the cause is gone; `/queue rm {id}` \
             withdraws the review.",
            target = r.target,
            id = r.id,
            head = short(&r.head_sha),
        ),
    }
}

/// **Why a review's attempts ran out, in one sentence a person can act on** — the row's
/// `failure` and the heart of [`blocked_after`]: every counted attempt at this head by its gist
/// (`capacity::gist`: firecode's `error:` sentence, not its banner), oldest first; and, when
/// every one was a reviewer whose VM did not start (`placement`), that it was placement and not
/// a judgement of the branch.
pub fn attempts_spent(attempts: u32, head: &str, gists: &[String], placement: bool) -> String {
    let listed = gists
        .iter()
        .enumerate()
        .map(|(i, g)| format!("({}) {g}", i + 1))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "{attempts} attempts to review it at {} ended without a verdict — no reviewer judged \
         the branch: {listed}.{}",
        head.chars().take(7).collect::<String>(),
        if placement {
            " Each was a reviewer that could not be PLACED (its VM did not start), not a \
             judgement of the branch."
        } else {
            ""
        }
    )
}

/// **`queue_blocked`**: the attempts ran out without a verdict (`review::Pool::attempt_failed`);
/// `spent` is [`attempts_spent`].
pub fn blocked_after(r: &ReviewRow, spent: &str) -> String {
    format!(
        "the review of `{}` (`{}`) is BLOCKED: {spent} `/queue restart {}` asks again; `/queue \
         approve {}` approves it over the reviewer (the merge gate still runs); `/queue rm {}` \
         withdraws it.",
        r.branch, r.id, r.id, r.id, r.id
    )
}

/// **A capacity wait's move and `failure`** (`review::Pool::wait_for_slot`) — it LEADS with
/// `capacity::WAITING_FOR_SLOT`, which the claim and `capacity::streak` read; `long` is the
/// minutes waited when this refusal is the one that raised [`vm_slots_exhausted`].
pub fn capacity_wait(
    r: &ReviewRow,
    busy: &str,
    delay_min: u64,
    nth: u32,
    long: Option<u64>,
) -> String {
    let mut why = format!(
        "{} for `{}`: {busy} — no attempt is counted; asked again in {delay_min} min (refusal \
         {nth} in a row)",
        super::capacity::WAITING_FOR_SLOT,
        r.branch,
    );
    if let Some(waited) = long {
        why.push_str(&format!(
            "; {} — waiting {waited} min",
            super::capacity::SLOTS_EXHAUSTED
        ));
    }
    why
}

/// **`queue_waiting_for_vm`** (Routine, once per wait): firecode had no free slot for the
/// reviewer, and the review waits — not blocked, no attempt counted. `busy` is
/// `VmCapacity::busy_line` (*"4 of 4 VM slots are busy"*).
pub fn waiting_for_vm(r: &ReviewRow, busy: &str) -> String {
    format!(
        "the review of `{}` (`{}`) is waiting for a free VM slot: {busy}. It is not blocked and \
         no attempt is counted; it is asked again in 1, 2 and 5 minutes, then every 5, until a \
         slot frees.",
        r.branch, r.id
    )
}

/// **`queue_vm_slots_exhausted`** (once, past `capacity::LONG_WAIT_MS`): slots are not freeing
/// on their own; `ceiling` is firecode's own `sudo` lines, verbatim, when its refusal had them.
///
/// `holders` is what holds each busy slot (`crate::vm_reap::slot_holders`), one line per tap; the
/// notice names them after its own sentence, so a person reads what to free rather than only that
/// every slot is busy (2026-10-15: four taps, four orphans, and a notice that said "every slot is
/// busy" for hours).
pub fn vm_slots_exhausted(
    r: &ReviewRow,
    waited_min: u64,
    busy: &str,
    ceiling: &[String],
    holders: &[String],
) -> String {
    let ceiling = if ceiling.is_empty() {
        "firecode's refusal names the `sudo ip tuntap add` lines that raise it".to_string()
    } else {
        format!(
            "firecode's own lines raise it, once, in a terminal: {}",
            ceiling
                .iter()
                .map(|l| format!("`{l}`"))
                .collect::<Vec<_>>()
                .join(" then ")
        )
    };
    let mut said = format!(
        "{}: the review of `{}` (`{}`) has waited {waited_min} min for a free VM slot ({busy}). \
         Free a slot (`firecode ps`; a tap keeps carrier while ANY VM holds it, an orphaned one \
         included), or raise the ceiling — {ceiling}. The review keeps waiting and is asked \
         again every 5 minutes; this is said once.",
        super::capacity::SLOTS_EXHAUSTED.to_uppercase(),
        r.branch,
        r.id,
    );
    if !holders.is_empty() {
        said.push_str(" What holds the slots: ");
        said.push_str(
            &holders
                .iter()
                .enumerate()
                .map(|(i, h)| format!("({}) {h}", i + 1))
                .collect::<Vec<_>>()
                .join("; "),
        );
        said.push('.');
    }
    said
}

/// **`queue_blocked`**: a reviewer that could not start (`review::serve`); `why` already says
/// so, `a reviewer for `BRANCH` could not start: …`.
pub fn reviewer_could_not_start(r: &ReviewRow, why: &str) -> String {
    format!(
        "{why}. The review is BLOCKED (`{}`): `/queue restart {}` asks again, \
         `/queue approve {}` approves it over the reviewer — the merge gate \
         still runs.",
        r.id, r.id, r.id
    )
}

/// **`queue_blocked`**: a review that cannot run (`RepoQueue::block`).
pub fn review_cannot_run(r: &ReviewRow, why: &str, merge_via_session: bool) -> String {
    format!(
        "the review of `{}` (`{}`) cannot run and is BLOCKED: {why} {}",
        r.branch,
        r.id,
        if merge_via_session {
            "This project merges such a branch via its main session, which is told next."
        } else {
            "Nothing lands it until a person acts: `/queue restart ID` asks again, \
             `/queue approve ID` approves it over the reviewer (the merge gate still \
             runs)."
        }
    )
}

/// **A review handed to the project's main session** (`Harness::handover_notices`).
pub fn handed_over(r: &ReviewRow) -> String {
    format!(
        "The review queue could not review `{branch}` (row `{id}`): {failure} This project \
         merges such a branch via its main session (`[queue] merge_via_session`), so it is \
         yours: the branch is in {wt}. Exception to the queue rule: run `{target}`'s merge \
         gate (the `Merge gate` section of its AGENTS.md) there, and land it on `{target}` \
         yourself if it is green — the queue sees the landing and withdraws the row. Or ask \
         the operator with `ask_user_question` whether to `/queue approve {id}` instead, \
         which lands it through the queue's own gate.",
        branch = r.branch,
        id = r.id,
        failure = r.failure,
        wt = r.worktree.as_deref().unwrap_or("its worktree"),
        target = r.target,
    )
}

/// **The operator's veto, to the child that wrote the branch** (`verbs::veto`).
pub fn vetoed(r: &ReviewRow, words: &str) -> String {
    format!(
        "The operator VETOED `{}`: {words}. It does not land. Answer it in the branch \
         and commit; the new head goes back to review by itself.",
        r.branch
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::HealthState;

    /// **The red `main` the operator pasted**, as the store holds it: `bin/firecode test` failed
    /// at 60bf819c in firecode, its output cut at the front the way `gate::tail` cuts it and
    /// ending with the gate's memory line.
    fn firecode_red() -> TargetHealth {
        let mut out = String::from("… [12500 byte(s) dropped from the front]\n");
        for i in 0..96 {
            out.push_str(&format!("test scene::tests::case_{i:03} ... ok\n"));
        }
        out.push_str(
            "test scene::tests::frame_matches_golden ... FAILED\n\nfailures:\n\n---- \
             scene::tests::frame_matches_golden stdout ----\nthread \
             'scene::tests::frame_matches_golden' panicked at crates/scene/src/tests.rs:212:5:\n\
             assertion `left == right` failed: frame 14 differs from the golden\n  left: \
             \"d41d8cd98f00b204\"\n right: \"9e107d9d372bb682\"\nnote: run with \
             `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n\nfailures:\n    \
             scene::tests::frame_matches_golden\n\ntest result: FAILED. 96 passed; 1 failed; 0 \
             ignored; 0 measured; 0 filtered out; finished in 4.17s\n\nerror: test failed, to \
             rerun pass `-p firecode-scene --lib`\n[gate memory: peak 1.6 GiB of a 40.9 GiB cap]",
        );
        TargetHealth {
            repository: "/home/dead/Projects/firecode".into(),
            target: "main".into(),
            sha: "60bf819c33425f282d547ffa2df5e2d840a75bef".into(),
            state: HealthState::Red,
            checked_ms: 1,
            evidence: format!("`bin/firecode test` failed:\n{out}"),
            gate_steps: vec![MergeGateStep {
                command: "bin/firecode test".into(),
                outcome: MergeGateOutcome::Failed,
                output: out,
                started_ms: 1,
                elapsed_ms: 61_000,
            }],
        }
    }

    /// **A `main` whose gate step was killed at the gate's memory cap** — the tip UNJUDGED, not
    /// red (`gate::killed_at_cap_words`), as letibot's own gate was killed building its tests.
    fn letibot_main_killed() -> TargetHealth {
        let output = "warning: `letibot-tui` (lib test) generated 17 warnings\n[gate memory: peak \
                      24.1 GiB of a 24.9 GiB cap — near its cap]";
        TargetHealth {
            repository: "/home/dead/Projects/letibot/letibot".into(),
            target: "main".into(),
            sha: "9562d70c4be1c07d9a2e5f3b8c6d1e0f7a9b2c4d".into(),
            state: HealthState::Blocked,
            checked_ms: 1,
            evidence: super::super::gate::killed_at_cap_words(
                "sh scripts/gate.sh",
                "peak 24.1 GiB of a 24.9 GiB cap",
                "it died with its peak within 5% of the cap",
                128,
            ),
            gate_steps: vec![MergeGateStep {
                command: "sh scripts/gate.sh".into(),
                outcome: MergeGateOutcome::Killed,
                output: output.into(),
                started_ms: 1,
                elapsed_ms: 74_332,
            }],
        }
    }

    /// **The TUI's red-card fixtures are what the daemon writes** — the session notice
    /// [`health`] says, and the `target_red` warning `RepoQueue::target_went_red` raises (its
    /// sentence spelled here as `serve.rs` spells it). `red_card` in the TUI parses these to draw
    /// each as one line; a reworded notice fails here before it falls back on a screen. Re-bless
    /// with `LETIBOT_BLESS_RED_CARD=1`, and teach `red_card` the new shape if it moved a part it
    /// keys on (the opening, the target, the sha, the repository, the failing step).
    #[test]
    fn the_tuis_red_card_fixtures_are_what_the_queue_writes() {
        let h = firecode_red();
        let notice = health(&h);
        let warning = target_red_warning(&h, &h.repository, false);
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tui/src/app/tests/fixtures");
        let killed = letibot_main_killed();
        let killed_notice = health(&killed);
        let killed_warning = gate_killed(&killed, &killed.repository);
        for (name, text) in [
            ("red_main_notice.txt", &notice),
            ("red_main_warning.txt", &warning),
            ("unjudged_main_killed_notice.txt", &killed_notice),
            ("unjudged_main_killed_warning.txt", &killed_warning),
        ] {
            let path = dir.join(name);
            if std::env::var_os("LETIBOT_BLESS_RED_CARD").is_some() {
                std::fs::write(&path, text).unwrap();
            }
            let pinned = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(
                text,
                &pinned,
                "the queue no longer writes what the TUI's red card parses ({})",
                path.display()
            );
        }
    }

    /// **The review the operator pasted** — `agent/panic-groupingsets-join`, whose reviewer could
    /// not decide, parked for a person. The reasons are the reviewer's as the paste shows them
    /// (it opened with these words and was cut where the operator stopped copying), completed.
    /// **R93.F50: landing a handed-over branch yourself is a named exception to the queue
    /// rule**, and the choice the operator makes is asked through `ask_user_question`.
    #[test]
    fn a_handover_names_its_exception_to_the_queue_rule() {
        let said = handed_over(&groupingsets());
        assert!(said.contains("Exception to the queue rule:"), "{said}");
        assert!(said.contains("`ask_user_question`"), "{said}");
    }

    fn groupingsets() -> ReviewRow {
        ReviewRow {
            id: "s-1789919514688401228-sub-1791743034603570463".into(),
            repository: "/home/dead/Projects/pgmem".into(),
            target: "main".into(),
            kind: ReviewKind::Branch,
            session_id: "s-1789919514688401228-sub-1791743034603570463".into(),
            branch: "agent/panic-groupingsets-join".into(),
            worktree: Some("/home/dead/Projects/pgmem/.worktrees/panic-groupingsets-join".into()),
            base_sha: "4be1c07d9a2e5f3b8c6d1e0f7a9b2c4d6e8f0a1b".into(),
            head_sha: "9d3e5a7c1b2f4e6d8c0a9b7e5f3d1c2b4a6e8f0d".into(),
            brief: "Fix the two panics in GROUPING SETS and in the hash join.".into(),
            priority: letibot_tokencore::store::MergePriority::Subagent,
            fixes: None,
            state: ReviewState::CannotDecide,
            owner: String::new(),
            reviewer: None,
            deadline_ms: None,
            attempts: 0,
            failure: String::new(),
            verdict: Some("needs_human".into()),
            verdict_by: Some("reviewer:s-gatekeeper-sub-17917".into()),
            reviewed_sha: Some("9d3e5a7c1b2f4e6d8c0a9b7e5f3d1c2b4a6e8f0d".into()),
            reasons: vec![
                "The two root causes are the right ones and both patches are correct in \
                 themselves. `groupingsets`: an empty grouping set made a zero-width key that the \
                 executor indexed past, and the patch gives the empty set a key of its own"
                    .into(),
                "The guard the brief demands is there. `GROUPING SETS (())` now plans to one \
                 group, and the regression test that panicked on main passes on the branch"
                    .into(),
                "The `join` patch is at the right site. The hash join no longer unwraps a missing \
                 build-side row; it emits the NULL-extended row the outer join asks for"
                    .into(),
                "BUT the ask's acceptance cannot be met on this base, and the branch's own \
                 measurements say it was not met. On main the memory engine cannot hold row \
                 values: COPY FROM STDIN only counts the rows it is given, so the acceptance query \
                 returns zero rows on the branch and on main alike, and a fix cannot be told from \
                 a no-op"
                    .into(),
            ],
            files: vec![],
            commands: vec![],
            evidence: "reviewer:s-gatekeeper-sub-17917 could not decide".into(),
            created_ms: 1,
            state_ms: 2,
            updated_ms: 2,
        }
    }

    /// **The TUI's review-card fixtures are what the queue writes** — one per shape a review's
    /// outcome reaches a session or a hub in: the row parked for a person ([`parked`], four
    /// shapes), the refusal and the veto said to the branch's session as a parent's message
    /// (`parent_message_text`, the words the session's row holds), the review handed to the
    /// project, and the hub warnings the review queue raises. `review_card` in the TUI parses
    /// these to draw each as one line — the operator, of the pasted wall of text: *"same story
    /// about reviews - … one line + popup"*. A reworded notice fails here before it falls back
    /// on a screen. Re-bless with `LETIBOT_BLESS_REVIEW_CARD=1`, and teach `review_card` the new
    /// shape if it moved a part it keys on (the opening, the branch, the verdict's words).
    #[test]
    fn the_tuis_review_card_fixtures_are_what_the_queue_writes() {
        use crate::sessions::gatekeeper::SESSION;
        let r = groupingsets();
        let as_message = |text: String| crate::harness::parent_message_text(SESSION, &text);

        let blocked = ReviewRow {
            state: ReviewState::Blocked,
            attempts: 3,
            failure: "the reviewer `s-gatekeeper-sub-17920` vanished without a verdict".into(),
            ..r.clone()
        };
        let red_tip = ReviewRow {
            id: "red-main-60bf819c3342".into(),
            kind: ReviewKind::Target,
            branch: "main".into(),
            head_sha: "60bf819c33425f282d547ffa2df5e2d840a75bef".into(),
            reasons: vec![
                "The failure is a golden frame that differs, and both commits since the last \
                 green tip touch the palette"
                    .into(),
                "BUT neither commit's own gate run was kept, so which one broke it cannot be told \
                 from here"
                    .into(),
            ],
            ..r.clone()
        };
        let red_tip_blocked = ReviewRow {
            state: ReviewState::Blocked,
            failure: "a reviewer for `main` could not start: the VM did not boot: no KVM on this \
                      host"
                .into(),
            ..red_tip.clone()
        };
        let refused = ReviewRow {
            state: ReviewState::Rejected,
            reasons: vec![
                "The panic is gone, but the patch catches it rather than removing its cause: the \
                 executor still indexes past a zero-width key, and only the unwrap moved"
                    .into(),
                "The regression test asserts that nothing panics and nothing about the rows".into(),
            ],
            ..r.clone()
        };
        let handed = ReviewRow {
            state: ReviewState::Blocked,
            failure: "the review's worktree is gone: /home/dead/Projects/pgmem/.worktrees/\
                      panic-groupingsets-join does not exist."
                .into(),
            evidence: crate::mergequeue::HANDED_TO_PROJECT.into(),
            ..r.clone()
        };
        let child_gone = "`s-1789919514688401228-sub-1791743034603570463`: no session \
                          `s-1789919514688401228-sub-1791743034603570463` is live in this daemon";
        // Three reviewers whose VM did not start, as `attempt_failed` counts them.
        let placement_failures = [
            "the reviewer `s-gatekeeper-sub-17918` failed: placing this session in a firecode VM: \
             io: running firecode: No such file or directory",
            "the reviewer `s-gatekeeper-sub-17919` failed: placing this session in a firecode VM: \
             io: firecode up refused (exit status: 1): [firecode] starting a VM and leaving it up \
             (log: /home/dead/Projects/firecode/runs/up.Xq81aa.log)\n[firecode] the VM failed to \
             start:\n[firecode] error: the image rootfs.ext4 does not mount. Rebuild it.",
            "the reviewer `s-gatekeeper-sub-17920` failed: placing this session in a firecode VM: \
             io: running firecode: No such file or directory",
        ];
        let could_not_start_why = format!(
            "a reviewer for `{}` could not start: the VM did not boot: no KVM on this host",
            r.branch
        );
        let shapes: Vec<(&str, String)> = vec![
            ("review_cannot_decide_notice.txt", parked(&r)),
            ("review_blocked_notice.txt", parked(&blocked)),
            ("review_red_tip_cannot_decide_notice.txt", parked(&red_tip)),
            (
                "review_red_tip_blocked_notice.txt",
                parked(&red_tip_blocked),
            ),
            (
                "review_refused_message.txt",
                as_message(super::refused(&refused, &refused.reasons)),
            ),
            (
                "review_refused_root_message.txt",
                as_message(refused_to_root(
                    &refused,
                    &refused.reasons,
                    &refused.session_id,
                    child_gone,
                )),
            ),
            (
                "review_vetoed_message.txt",
                as_message(vetoed(
                    &r,
                    "the acceptance query has to return rows on the branch; land the COPY fix first",
                )),
            ),
            ("review_handed_over_notice.txt", handed_over(&handed)),
            (
                "review_needs_human_warning.txt",
                needs_human(
                    &r,
                    "reviewer:s-gatekeeper-sub-17917",
                    &crate::mergequeue::capacity::decisive_reason(&r.reasons),
                    &[
                        "2cefd4b97b287d1aaf6cafe6bfdc70af696575a2".to_string(),
                        "2ef5c95fa50f185772144660a54c769d695cfd06".to_string(),
                        r.head_sha.clone(),
                    ],
                ),
            ),
            (
                "review_needs_human_red_tip_warning.txt",
                needs_human(
                    &red_tip,
                    "reviewer:s-gatekeeper-sub-17930",
                    &crate::mergequeue::capacity::decisive_reason(&red_tip.reasons),
                    std::slice::from_ref(&red_tip.head_sha),
                ),
            ),
            (
                "review_blocked_after_warning.txt",
                blocked_after(
                    &r,
                    &attempts_spent(
                        3,
                        &r.head_sha,
                        &placement_failures
                            .iter()
                            .map(|w| crate::mergequeue::capacity::gist(w))
                            .collect::<Vec<_>>(),
                        true,
                    ),
                ),
            ),
            (
                "review_waiting_for_vm_warning.txt",
                waiting_for_vm(&r, "4 of 4 VM slots are busy"),
            ),
            (
                "review_vm_slots_exhausted_warning.txt",
                vm_slots_exhausted(
                    &r,
                    31,
                    "4 of 4 VM slots are busy",
                    &[
                        "sudo ip tuntap add dev fccode5 mode tap user $(id -u)".to_string(),
                        "sudo ip addr add 172.16.5.1/30 dev fccode5 && sudo ip link set fccode5 up"
                            .to_string(),
                    ],
                    &[],
                ),
            ),
            (
                "review_could_not_start_warning.txt",
                reviewer_could_not_start(&r, &could_not_start_why),
            ),
            (
                "review_cannot_run_warning.txt",
                review_cannot_run(
                    &r,
                    "the branch `agent/panic-groupingsets-join` is gone from the repository.",
                    false,
                ),
            ),
            (
                "review_refusal_undelivered_warning.txt",
                refusal_undelivered(&refused, child_gone),
            ),
        ];
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tui/src/app/tests/fixtures");
        for (name, text) in &shapes {
            let path = dir.join(name);
            if std::env::var_os("LETIBOT_BLESS_REVIEW_CARD").is_some() {
                std::fs::write(&path, text).unwrap();
            }
            let pinned = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(
                text,
                &pinned,
                "the queue no longer writes what the TUI's review card parses ({})",
                path.display()
            );
        }
    }
}
