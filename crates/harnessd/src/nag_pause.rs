//! The pause of the idle plan-check: `/nag`, `/nag pause [DURATION]` `/nag resume`. A pause is
//! one session's switch, on that session's row. While it stands the session's
//! [`crate::sessions::TodoNagClock`] arms nothing but the pause's own end. A pause lasts at most
//! one daemon's life: the next daemon opens the session with nags on and says so.

use std::sync::OnceLock;
use std::time::Duration;

use letibot_sessionlog::NagPause;

/// Who paused or resumed the plan-check, from the identity its command carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NagBy {
    /// A typed `/nag` from a head, or `--slash`.
    Operator,
    /// This session's own model, through `nag_pause_self`.
    Model,
    /// Another session's model.
    Session(String),
    /// The MCP or browser bridge.
    Bridge,
    /// The board.
    Board,
    /// A row written before the author was recorded.
    Unknown,
}

impl NagBy {
    /// `identity` is the queued command's; `session` the session it acts on.
    pub fn from_identity(identity: &str, session: &str) -> NagBy {
        if let Some(who) = identity.strip_prefix("agent:") {
            return if who == session {
                NagBy::Model
            } else {
                NagBy::Session(who.to_string())
            };
        }
        if identity == letibot_sessionlog::MCP_IDENTITY || identity == "letibot-browser" {
            return NagBy::Bridge;
        }
        if identity.starts_with("board") {
            return NagBy::Board;
        }
        NagBy::Operator
    }

    pub fn words(&self) -> String {
        match self {
            NagBy::Operator => "the operator".into(),
            NagBy::Model => "the model".into(),
            NagBy::Session(id) => format!("session {id}"),
            NagBy::Bridge => "the bridge".into(),
            NagBy::Board => "the board".into(),
            NagBy::Unknown => "an unrecorded author".into(),
        }
    }

    pub fn to_stored(&self) -> String {
        match self {
            NagBy::Operator => "operator".into(),
            NagBy::Model => "model".into(),
            NagBy::Session(id) => format!("session:{id}"),
            NagBy::Bridge => "bridge".into(),
            NagBy::Board => "board".into(),
            NagBy::Unknown => "unknown".into(),
        }
    }

    pub fn from_stored(s: &str) -> NagBy {
        match s {
            "operator" => NagBy::Operator,
            "model" => NagBy::Model,
            "bridge" => NagBy::Bridge,
            "board" => NagBy::Board,
            _ => match s.strip_prefix("session:") {
                Some(id) => NagBy::Session(id.to_string()),
                None => NagBy::Unknown,
            },
        }
    }
}

/// Who made a pause and when. `at_ms` is `None` for a row written before it was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PausedBy {
    pub by: NagBy,
    pub at_ms: Option<u64>,
}

impl PausedBy {
    /// `(by the operator, since 23:38)`.
    pub fn words(&self, now_ms: u64) -> String {
        match self.at_ms {
            Some(at) => format!("(by {}, since {})", self.by.words(), clock_at(at, now_ms)),
            None => format!("(by {})", self.by.words()),
        }
    }
}

/// When this daemon's life began, as far as pauses are concerned: first read before any pause
/// is written, so every pause this process made is at or after it and every older one before.
pub fn daemon_life_started_ms() -> u64 {
    static STARTED: OnceLock<u64> = OnceLock::new();
    *STARTED.get_or_init(letibot_sessionlog::registry::now_ms)
}

/// The one row a session gets when a pause from an earlier daemon is dropped at open.
pub fn restart_words(by: &PausedBy, now_ms: u64) -> String {
    let since = match by.at_ms {
        Some(at) => format!("since {}", clock_at(at, now_ms)),
        None => "since an earlier daemon".into(),
    };
    format!(
        "plan-checks were paused before the restart ({since}, by {}); they are on again - \
         `/nag pause` to pause",
        by.by.words()
    )
}

/// What the operator asked `/nag` for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NagVerb {
    /// `/nag` alone: the current state.
    Report,
    /// `/nag pause` (until resumed) or `/nag pause 2h` (until then). `off` is the same verb.
    Pause(Option<Duration>),
    /// `/nag resume`. `on` is the same verb.
    Resume,
}

/// `/nag …`'s words after the verb, parsed. A duration it cannot read is refused
/// by name with the grammar, never guessed at: `/nag pause 2` is two of what?
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

/// `30m`, `2h`, `90s`, `1d`, `1h30m`: numbers each followed by its unit, summed.
/// Zero is refused, because a pause that is already over is a typo.
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

/// A wall-clock time the way a person reads a deadline: `14:30` when it is within
/// a day of `now_ms` either way, `10-12 14:30` when it is not, in the local zone.
pub fn clock_at(ms: u64, now_ms: u64) -> String {
    let secs = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`; both are owned here. The `_r` form
    // is the one that does not hand back a shared static, and a harness runs on many threads.
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if !ok {
        return format!("unix ms {ms}");
    }
    if ms.abs_diff(now_ms) < 24 * 60 * 60 * 1000 {
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

/// Until when, in words: `until 14:30`, or `until you resume them`.
pub fn until_words(p: NagPause, now_ms: u64) -> String {
    match p {
        NagPause::UntilResumed => "until you resume them".into(),
        NagPause::Until { ms } => format!("until {}", clock_at(ms, now_ms)),
    }
}

/// The settings row's value: `on`, or `paused until … (by …, since …)`. The head keys on the
/// `paused` spelling, so that word is the contract and the rest is the sentence.
pub fn setting_value(p: Option<NagPause>, by: Option<&PausedBy>, now_ms: u64) -> String {
    match (p, by) {
        (None, _) => "on".into(),
        (Some(p), None) => format!("paused {}", until_words(p, now_ms)),
        (Some(p), Some(by)) => format!("paused {} {}", until_words(p, now_ms), by.words(now_ms)),
    }
}

/// `/nag` alone: the state, and the verb that changes it.
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

/// `/nag pause`'s answer: what stops, what does not, and who it does not cover.
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

/// Why a pause ended, for the `nags_resumed` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resumed {
    /// `/nag resume`, and who typed or called it.
    ByVerb(NagBy),
    /// The pause's own deadline arrived while the session was held.
    AtItsEnd,
    /// The deadline passed while no daemon held the session; noticed at open.
    WhileClosed,
}

/// The `nags_resumed` row's sentence.
pub fn resumed_words(why: Resumed, was: NagPause, now_ms: u64) -> String {
    let end = match was {
        NagPause::Until { ms } => clock_at(ms, now_ms),
        NagPause::UntilResumed => String::new(),
    };
    match why {
        Resumed::ByVerb(by) => format!(
            "nags RESUMED by {}: the plan-check is armed again from now",
            by.words()
        ),
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
        // The ambiguity is refused, not guessed: two of what?
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
        assert_eq!(setting_value(None, None, 0), "on");
        assert_eq!(
            setting_value(Some(NagPause::UntilResumed), None, 0),
            "paused until you resume them"
        );
        let now = 1_790_000_000_000;
        let v = setting_value(
            Some(NagPause::Until {
                ms: now + 3_600_000,
            }),
            None,
            now,
        );
        assert!(v.starts_with("paused until "), "{v}");
        assert!(
            v.len() == "paused until HH:MM".len(),
            "within a day it is HH:MM: {v}"
        );
    }

    #[test]
    fn the_author_is_read_off_the_commands_identity() {
        assert_eq!(NagBy::from_identity("dead", "s1"), NagBy::Operator);
        assert_eq!(NagBy::from_identity("agent:s1", "s1"), NagBy::Model);
        assert_eq!(
            NagBy::from_identity("agent:s2", "s1"),
            NagBy::Session("s2".into())
        );
        assert_eq!(
            NagBy::from_identity(letibot_sessionlog::MCP_IDENTITY, "s1"),
            NagBy::Bridge
        );
        for by in [
            NagBy::Operator,
            NagBy::Model,
            NagBy::Session("s2".into()),
            NagBy::Bridge,
            NagBy::Board,
        ] {
            assert_eq!(NagBy::from_stored(&by.to_stored()), by);
        }
        assert_eq!(NagBy::from_stored("whatever"), NagBy::Unknown);
    }

    #[test]
    fn the_settings_value_names_who_paused_and_when() {
        let now = 1_790_000_000_000;
        let by = PausedBy {
            by: NagBy::Model,
            at_ms: Some(now - 60_000),
        };
        let v = setting_value(Some(NagPause::UntilResumed), Some(&by), now);
        assert!(
            v.starts_with("paused until you resume them (by the model, since "),
            "{v}"
        );
        assert_eq!(
            v.len(),
            "paused until you resume them (by the model, since HH:MM)".len()
        );
    }

    #[test]
    fn the_restart_row_says_since_when_and_by_whom() {
        let now = 1_790_000_000_000;
        let by = PausedBy {
            by: NagBy::Operator,
            at_ms: Some(now - 3_600_000),
        };
        let w = restart_words(&by, now);
        assert!(
            w.starts_with("plan-checks were paused before the restart (since "),
            "{w}"
        );
        assert!(
            w.ends_with(", by the operator); they are on again - `/nag pause` to pause"),
            "{w}"
        );
    }
}
