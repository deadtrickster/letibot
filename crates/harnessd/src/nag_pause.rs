//! **The operator's pause of the idle plan-check** — `/nag`, `/nag pause [DURATION]`,
//! `/nag resume`.
//!
//! The operator's ask, then its correction, verbatim: *"and i need a way to stop todo nags"*, and
//! *"or rather - pause and the pause must be loud"*. Before this the only lever was per ROW — an
//! operator `Postponed` takes a row out of the check — and nothing could quiet a whole session
//! short of setting every row aside, which says something false about the rows.
//!
//! # What a pause is
//!
//! **One session's switch, on that session's row.** While it stands, the session's
//! [`crate::sessions::TodoNagClock`] arms nothing but the pause's own end: no idle check, no
//! repeat, and not the sooner check an operator's added row earns (R77). The board still takes
//! every edit — the operator's and the model's — because a pause is about being ASKED, not about
//! the plan. It lives on the store's session row (`nags_paused_until`, schema v25) and on the
//! session's [`crate::config::Config`], the `auto_compact_stood_down` precedent: the row is what a
//! restart and a resume read back, the config is what the settings row is drawn from.
//!
//! **Per session, children included and not inherited.** A subagent is a session, and its clock
//! lives on its own thread (`harness::serve_child`); `/nag pause` sent to a child pauses THAT
//! child. A parent's pause does not cover its children: a child is the parent's work in flight,
//! and silencing the checks on work the operator did not look at is the silent board this verb
//! must not produce. Pausing a child is one `/nag pause` attached to it.
//!
//! # Why it must be loud
//!
//! A board that has stopped asking looks exactly like a clock that broke. So every place a reader
//! could take the silence for a fault says it is a pause: the hub carries a `nags_paused` row when
//! it starts and a `nags_resumed` row when it ends (by the verb, by its own deadline, or while the
//! session was closed); the settings row `nags` says `paused until 14:30`; the head draws a
//! resident line while it stands and the picker marks the session.
//!
//! # What happens to results held for the model (R79) while it is paused
//!
//! **A pause does not touch them.** Results are held only while a turn RUNS — the operator,
//! 2026-10-11: *"jobs were ran for a reason and held for a reason. once turn ends no reason to
//! hold"* — so a held batch is delivered as a turn of its own the moment the turn it was held
//! through ends (the turn-end wake, `JobWatchers::turn_ended`), and a result that arrives while the
//! session is idle is delivered at once (`crate::delivery`). Neither is a plan-check, so `/nag
//! pause` neither delays nor stops them: a pause quiets the daemon ASKING about the plan, and a
//! finished job is the world reporting, not the daemon asking. Results the plan waits on (`when:
//! job`, `needs`, a wait, `wake: now`, a failure the plan depends on) were never held to begin with.
//! Every settlement is still published to the hub as it happens, so the head's jobs pane is not
//! behind either.
//!
//! # What the model is told
//!
//! **Nothing, deliberately.** The plan-check is the daemon nudging the model; its absence takes
//! nothing away that the model relies on, and a line in its context saying *the operator paused
//! your reminders* is a sentence about the machinery that it can neither act on nor answer. The
//! notice text is untouched by this module.

use std::time::Duration;

use letibot_sessionlog::NagPause;

/// **What the operator asked `/nag` for.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NagVerb {
    /// `/nag` alone: the current state.
    Report,
    /// `/nag pause` (until resumed) or `/nag pause 2h` (until then). `off` is the same verb.
    Pause(Option<Duration>),
    /// `/nag resume`. `on` is the same verb.
    Resume,
}

/// **`/nag …`'s words after the verb**, parsed. A duration it cannot read is refused by name with
/// the grammar, never guessed at — `/nag pause 2` could be two minutes or two hours.
pub fn parse(args: &[&str]) -> Result<NagVerb, String> {
    match args.first().copied() {
        None | Some("status") | Some("?") => Ok(NagVerb::Report),
        Some("pause") | Some("off") | Some("stop") => match args.get(1) {
            None => Ok(NagVerb::Pause(None)),
            Some(d) => match parse_duration(d) {
                Some(d) => Ok(NagVerb::Pause(Some(d))),
                None => Err(format!(
                    "/nag pause {d}: `{d}` is not a duration — write it with its unit, `90s`, \
                     `30m`, `2h`, `1d` or `1h30m`; `/nag pause` alone holds until `/nag resume`"
                )),
            },
        },
        Some("resume") | Some("on") | Some("start") => Ok(NagVerb::Resume),
        Some(other) => Err(format!(
            "/nag {other}: the verbs are `/nag` (the state), `/nag pause [DURATION]` and \
             `/nag resume`"
        )),
    }
}

/// **`30m`, `2h`, `90s`, `1d`, `1h30m`** — numbers each followed by its unit, summed. Zero is
/// refused: a pause that is already over is a typo, not a request.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let mut total: u64 = 0;
    let mut digits = String::new();
    let mut saw_unit = false;
    for c in s.trim().chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let n: u64 = digits.parse().ok()?;
        digits.clear();
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 60 * 60,
            'd' => 24 * 60 * 60,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(unit)?)?;
        saw_unit = true;
    }
    // Trailing digits with no unit, or nothing at all, are the ambiguity the refusal names.
    if !digits.is_empty() || !saw_unit || total == 0 {
        return None;
    }
    Some(Duration::from_secs(total))
}

/// **A wall-clock time the way a person reads a deadline** — `14:30` when it is within a day of
/// `now_ms`, `10-12 14:30` when it is not, in the local zone. An unreadable clock is said.
pub fn clock_at(ms: u64, now_ms: u64) -> String {
    let secs = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`; both are owned here. The `_r` form
    // is the one that does not hand back a shared static — a harness runs on more than one thread.
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if !ok {
        return format!("unix ms {ms}");
    }
    if ms.saturating_sub(now_ms) < 24 * 60 * 60 * 1000 {
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    } else {
        format!(
            "{:02}-{:02} {:02}:{:02}",
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min
        )
    }
}

/// **Until when, in words** — `until 14:30`, or `until you resume them`.
pub fn until_words(p: NagPause, now_ms: u64) -> String {
    match p {
        NagPause::UntilResumed => "until you resume them".into(),
        NagPause::Until { ms } => format!("until {}", clock_at(ms, now_ms)),
    }
}

/// **The settings row's value** — `on`, or `paused until …`. The head keys on the `paused`
/// spelling (its resident line and `/status` read it), so the word is the contract and the rest
/// is the sentence.
pub fn setting_value(p: Option<NagPause>, now_ms: u64) -> String {
    match p {
        None => "on".into(),
        Some(p) => format!("paused {}", until_words(p, now_ms)),
    }
}

/// **`/nag` alone** — the state, and the verb that changes it.
pub fn report(p: Option<NagPause>, now_ms: u64) -> String {
    match p {
        None => "nags are ON for this session: an open plan is checked after a quiet minute. \
                 `/nag pause [30m|2h]` pauses them."
            .into(),
        Some(p) => format!(
            "nags are PAUSED for this session {}: no plan-check is armed or sent. `/nag resume` \
             turns them back on.",
            until_words(p, now_ms)
        ),
    }
}

/// **`/nag pause`'s answer** — what stops, what does not, and who it does not cover.
pub fn paused_reply(p: NagPause, now_ms: u64) -> String {
    format!(
        "nags PAUSED for this session {}: no plan-check is armed or sent — not the idle one, and \
         not the sooner one a row you add earns. The board still takes your edits. Its \
         subagents keep their own clocks; attach to one and `/nag pause` there to pause it. \
         `/nag resume` ends the pause{}.",
        until_words(p, now_ms),
        match p {
            NagPause::UntilResumed => "",
            NagPause::Until { .. } => " sooner",
        }
    )
}

/// **Why a pause ended**, for the `nags_resumed` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resumed {
    /// `/nag resume`.
    ByVerb,
    /// The pause's own deadline arrived while the session was held.
    AtItsEnd,
    /// The deadline passed while no daemon held the session; noticed at open.
    WhileClosed,
}

/// **The `nags_resumed` row's sentence.**
pub fn resumed_words(why: Resumed, was: NagPause, now_ms: u64) -> String {
    let end = match was {
        NagPause::Until { ms } => clock_at(ms, now_ms),
        NagPause::UntilResumed => String::new(),
    };
    match why {
        Resumed::ByVerb => {
            "nags RESUMED by the operator: the plan-check is armed again from now".into()
        }
        Resumed::AtItsEnd => format!(
            "nags RESUMED: the pause ran out at {end}, and the plan-check is armed again from now"
        ),
        Resumed::WhileClosed => format!(
            "nags RESUMED: the pause ran out at {end} while the session was closed, and the \
             plan-check is armed again"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_is_numbers_with_their_units_and_nothing_else() {
        assert_eq!(parse_duration("30m"), Some(Duration::from_secs(30 * 60)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(2 * 3600)));
        assert_eq!(parse_duration("1h30m"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_duration("1s"), Some(Duration::from_secs(1)));
        assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86_400)));
        // **The ambiguity is refused, not guessed**: two of what?
        assert_eq!(parse_duration("2"), None);
        assert_eq!(parse_duration("2x"), None);
        assert_eq!(parse_duration("0m"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("m"), None);
    }

    #[test]
    fn the_verb_takes_pause_and_resume_and_keeps_off_and_on_as_aliases() {
        assert_eq!(parse(&[]), Ok(NagVerb::Report));
        assert_eq!(parse(&["pause"]), Ok(NagVerb::Pause(None)));
        assert_eq!(
            parse(&["pause", "2h"]),
            Ok(NagVerb::Pause(Some(Duration::from_secs(7200))))
        );
        assert_eq!(
            parse(&["off", "30m"]),
            Ok(NagVerb::Pause(Some(Duration::from_secs(1800))))
        );
        assert_eq!(parse(&["resume"]), Ok(NagVerb::Resume));
        assert_eq!(parse(&["on"]), Ok(NagVerb::Resume));
        assert!(parse(&["pause", "2"]).is_err());
        assert!(parse(&["sometimes"]).is_err());
    }

    #[test]
    fn the_settings_value_starts_with_the_word_the_head_keys_on() {
        assert_eq!(setting_value(None, 0), "on");
        assert_eq!(
            setting_value(Some(NagPause::UntilResumed), 0),
            "paused until you resume them"
        );
        let now = 1_790_000_000_000;
        let v = setting_value(
            Some(NagPause::Until {
                ms: now + 3_600_000,
            }),
            now,
        );
        assert!(v.starts_with("paused until "), "{v}");
        assert!(
            v.len() == "paused until HH:MM".len(),
            "within a day it is HH:MM: {v}"
        );
    }
}
