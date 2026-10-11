//! **A reviewer that could not be PLACED for want of a free VM slot is not an attempt** — and the
//! words a review that stops for a person says about why.
//!
//! The hub row this module was written against, on pg-noop's session, 2026-10-12 ~00:56:
//!
//! > `queue_blocked — the review of agent/panic-groupingsets-join
//! > (s-1789919514688401228-sub-1791743034603570463) is BLOCKED after 3 attempts without a
//! > verdict: the reviewer s-gatekeeper-sub-1791759365511364772 failed: placing this session in a
//! > firecode VM: io: firecode up refused (exit status: 1): [firecode] starting a VM and leaving it
//! > up (log: /home/dead/Projects/firecode/runs/up.6DSWx1.log)`
//!
//! The operator asked to *"investigate"*, and then *"fix them now"*. What the store's history says
//! (`queue_transition` for that row): the three attempts were three reviewers at ONE head
//! (26713ff) that firecode refused to boot within three minutes — 22:55:34, 22:56:04, 22:58:30 —
//! each with *"this run needs tap fccode5, and creating one needs root … 4 tap(s) exist and 4 have
//! carrier, so the slots below this one were taken"*. Minutes later two of the four taps were free
//! again. A capacity shortage, transient, spent the whole attempt budget R61 set aside for
//! reviewers that cannot run, and the entry blocked at high severity naming the first line of the
//! last refusal — which is the one line of firecode's answer that says nothing.
//!
//! So: [`vm_capacity`] recognises firecode's capacity refusal; the review pool sends such a row
//! back to `waiting` WITHOUT counting an attempt, with a not-before time on `deadline_ms`
//! ([`retry_after_ms`]: 1, 2, 5 minutes, then every 5), says so ONCE in the routine register
//! (`queue_waiting_for_vm`), and raises once (`queue_vm_slots_exhausted`, with firecode's own
//! `sudo` lines) when the wait passes [`LONG_WAIT_MS`]. The wait is read back from the row's own
//! history ([`streak`]), so a daemon restart neither forgets it nor says it again.
//!
//! **How capacity is recognised.** firecode has no exit code of its own for it — `die` is
//! `exit 1`, like every other refusal (`bin/firecode`, `net_create_tap`). Its words are the stable
//! part: the refusal is the run needing a tap it may not create (*"needs tap … creating one needs
//! root"*), and the count that makes it capacity rather than a broken host is *"the slots below
//! this one were taken"*. firecode wraps that sentence across lines with indentation, so the text is
//! matched with its whitespace collapsed. A missing firecode, a broken image or a copy over the
//! ceiling says none of this and stays a real placement failure — counted, and blocked on the
//! third, as before.

use letibot_tokencore::store::QueueTransition;

/// **The clause a review waiting for a VM slot leads its move (and `failure`) with** — what the
/// claim reads to hold the row until its not-before time, and what [`streak`] counts.
pub const WAITING_FOR_SLOT: &str = "waiting for a free VM slot";

/// The clause added to the move on which a long wait was raised — so it is raised once.
pub const SLOTS_EXHAUSTED: &str = "VM slots exhausted";

/// **A wait this long is raised** (once): slots are not freeing on their own.
pub const LONG_WAIT_MS: u64 = 30 * 60 * 1000;

/// **firecode's capacity refusal, read.**
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VmCapacity {
    /// The tap the run needed (`fccode5`).
    pub tap: Option<String>,
    /// Taps that exist — firecode's slots.
    pub slots: Option<u32>,
    /// Taps with carrier — slots in use.
    pub busy: Option<u32>,
    /// firecode's own lines for raising the ceiling, verbatim (`sudo ip tuntap add …`).
    pub raise_ceiling: Vec<String>,
}

impl VmCapacity {
    /// *"4 of 4 VM slots are busy"*, or as much of it as firecode said.
    pub fn busy_line(&self) -> String {
        match (self.busy, self.slots) {
            (Some(b), Some(s)) => format!("{b} of {s} VM slots are busy"),
            (None, Some(s)) => format!("all {s} VM slots are busy"),
            _ => "every VM slot is busy".to_string(),
        }
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// **Is this placement failure firecode saying it has no free slot?** `None` for anything else.
pub fn vm_capacity(why: &str) -> Option<VmCapacity> {
    let flat = collapse(why);
    let needs_tap = flat.contains("needs tap") && flat.contains("creating one needs root");
    let taken = flat.contains("slots below this one were taken");
    if !(taken || needs_tap) || !flat.contains("firecode") {
        return None;
    }
    let number_before = |marker: &str| -> Option<u32> {
        let i = flat.find(marker)?;
        flat[..i].split_whitespace().last()?.parse().ok()
    };
    let tap = flat
        .split("needs tap ")
        .nth(1)
        .and_then(|rest| rest.split([',', ' ']).next())
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let raise_ceiling = why
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("sudo "))
        .map(str::to_string)
        .collect();
    let slots = number_before(" tap(s) exist");
    // **No slot at all is not a shortage**: a host on which no tap was ever made never frees one
    // by waiting, so that refusal stays a placement failure a person has to see.
    if slots == Some(0) {
        return None;
    }
    Some(VmCapacity {
        tap,
        slots,
        busy: number_before(" have carrier"),
        raise_ceiling,
    })
}

/// **How long a review waits for a slot before it is tried again** — the `nth` refusal in a row:
/// 1 minute, 2, 5, and every 5 after.
pub fn retry_after_ms(nth: u32) -> u64 {
    const MIN: u64 = 60 * 1000;
    match nth {
        0 | 1 => MIN,
        2 => 2 * MIN,
        _ => 5 * MIN,
    }
}

/// **The capacity wait a row is in, read off its history.**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Streak {
    /// Capacity refusals in a row, newest back to the last other outcome.
    pub refusals: u32,
    /// When the first of them was written.
    pub since_ms: Option<u64>,
    /// Whether one of them already raised the long wait.
    pub raised: bool,
}

/// Walk the row's moves newest first: capacity refusals count, the claims and starts between
/// them (and a note written on the waiting row) are passed over, anything else ends the streak (a verdict, a restart, a counted failure).
pub fn streak(history: &[QueueTransition]) -> Streak {
    let mut s = Streak::default();
    for t in history.iter().rev() {
        let from = t.from_state.as_deref().unwrap_or("");
        if t.to_state == "waiting" && t.why.starts_with(WAITING_FOR_SLOT) {
            s.refusals += 1;
            s.since_ms = Some(t.at_ms);
            s.raised |= t.why.contains(SLOTS_EXHAUSTED);
        } else if (t.to_state == "reviewing" && matches!(from, "waiting" | "reviewing"))
            || (t.to_state == "waiting" && from == "waiting")
        {
            // A claim, a reviewer started, or a note on a waiting row: none ends a wait.
            continue;
        } else {
            break;
        }
    }
    s
}

/// **Is a waiting row held back by a capacity wait?** — its `failure` leads with
/// [`WAITING_FOR_SLOT`] and its not-before time is still ahead.
pub fn held_until(failure: &str, deadline_ms: Option<u64>, now_ms: u64) -> bool {
    failure.starts_with(WAITING_FOR_SLOT) && deadline_ms.is_some_and(|d| d > now_ms)
}

/// **The one line of a failure a person reads** — firecode's own `error:` sentence when it is
/// there (the first line of its answer is *"starting a VM and leaving it up"*, which says
/// nothing), otherwise the failure's first line; at most ~240 characters.
pub fn gist(why: &str) -> String {
    let line = match why.find("[firecode] error:") {
        Some(i) => {
            let rest = collapse(&why[i + "[firecode] error:".len()..]);
            let end = rest.find(". ").map(|e| e + 1).unwrap_or(rest.len());
            let placing = why.contains("placing this session in a firecode VM");
            format!(
                "{}firecode: {}",
                if placing {
                    "the reviewer's VM did not start — "
                } else {
                    ""
                },
                &rest[..end]
            )
        }
        None => why.lines().next().unwrap_or("").trim().to_string(),
    };
    clip(&line, 240)
}

/// At most `max` characters, on a character boundary, with `…` when cut.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// **The sentence a reviewer's `needs_human` turned on** — the one that says what stops it: a
/// sentence opening `BUT`/`But`, else one with a `BUT` in it, else one with ` but `, else the
/// first. The reasons as the reviewer wrote them, one per item; never paraphrased, only chosen.
pub fn decisive_reason(reasons: &[String]) -> String {
    let sentences: Vec<String> = reasons.iter().flat_map(|r| sentences(r)).collect();
    let pick = sentences
        .iter()
        .find(|s| s.starts_with("BUT ") || s.starts_with("But "))
        .or_else(|| sentences.iter().find(|s| s.contains(" BUT ")))
        .or_else(|| sentences.iter().find(|s| s.contains(" but ")))
        .or_else(|| sentences.first());
    clip(pick.map(String::as_str).unwrap_or(""), 400)
}

/// Sentences of one reason: split after `.`, `!` or `?` followed by a space and a capital (or a
/// backtick) — so `mod.rs:670` and `i.e. pristine` are not cut.
fn sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        cur.push(chars[i]);
        let end = matches!(chars[i], '.' | '!' | '?')
            && chars.get(i + 1) == Some(&' ')
            && chars
                .get(i + 2)
                .is_some_and(|c| c.is_uppercase() || *c == '`');
        if end {
            let s = cur.trim().to_string();
            if !s.is_empty() {
                out.push(s);
            }
            cur.clear();
        }
        i += 1;
    }
    let s = cur.trim().to_string();
    if !s.is_empty() {
        out.push(s);
    }
    out
}

/// **The heads reviewers could not decide at**, oldest first, read off the row's history (the
/// verdict's move says *"… could not decide at SHA: …"*).
pub fn undecided_heads(history: &[QueueTransition]) -> Vec<String> {
    history
        .iter()
        .filter(|t| t.to_state == "cannot_decide")
        .filter_map(|t| {
            let rest = t.why.split(" could not decide at ").nth(1)?;
            let sha = rest.split([':', ' ']).next()?.trim();
            (!sha.is_empty()).then(|| sha.to_string())
        })
        .collect()
}

/// The short form of a head a sentence carries.
pub fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

#[cfg(test)]
pub(crate) mod tests;
