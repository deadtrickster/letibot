//! When a provider says its limit lifts: the two machine-readable forms, and nothing else.
//!
//! `Retry-After` (RFC 9110) is a delay in seconds or an HTTP-date; a `429` body is prose that
//! sometimes names a wall clock and sometimes the width of the window it closed. A caller that
//! parks a session on an invented instant is worse off than one that gives up, so every reading
//! here is bounded and the earlier of two readings wins: coming back too soon costs one refused
//! call, coming back hours late costs the session.

/// The shortest a park may be. A wall clock already past is a reading in a zone this box does not
/// know, so it is not a reason to call again at once.
pub const SHORTEST: u64 = 60_000;

/// The longest a park may be. A limit half a day out is a quota, which no wait lifts; a session
/// woken at this bound asks again and reads the next answer, so the bound is not a verdict.
pub const LONGEST: u64 = 6 * 60 * 60 * 1000;

/// `Retry-After` as seconds from `now_ms`: a delta, or one of RFC 9110's three HTTP-dates -
/// IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), RFC 850 and asctime. A date already past is `0`.
pub fn from_retry_after(value: &str, now_ms: u64) -> Option<u64> {
    let v = value.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(secs);
    }
    let at = http_date_ms(v)?;
    Some(at.saturating_sub(now_ms) / 1000)
}

/// The instant a 429's prose names, Unix millis, reading whichever of the two facts it carries:
/// a wall clock (`Your limit will reset at 2026-11-07 21:02:27`) and the window it closed
/// (`Usage limit reached for 5 hour`). The earlier wins - see the module header - and the result
/// is bounded by [`LONGEST`], the park's bound; [`from_prose_unbounded`] is the instant itself.
///
/// The wall clock names no zone. It is read as UTC, which is the reading that makes it the latest
/// plausible instant in the world's offsets, and the window beside it is what keeps a wrong guess
/// from parking a session for a day.
pub fn from_prose(body: &str, now_ms: u64) -> Option<u64> {
    from_prose_unbounded(body, now_ms).map(|at| at.min(now_ms + LONGEST))
}

/// [`from_prose`] without the ceiling: the reset a weekly or monthly limit really names, for a
/// caller that decides by it or tells the operator. Still floored at [`SHORTEST`].
pub fn from_prose_unbounded(body: &str, now_ms: u64) -> Option<u64> {
    let clock = wall_clock_ms(body);
    let window = window_ms(body).map(|d| now_ms.saturating_add(d));
    let at = match (clock, window) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => return None,
    };
    Some(at.max(now_ms + SHORTEST))
}

/// `2026-11-07 21:02:27` (or with a `T` and a trailing `Z`), read as UTC.
fn wall_clock_ms(body: &str) -> Option<u64> {
    let at = body.to_ascii_lowercase();
    let after = at.split_once("reset at ").map(|(_, rest)| rest)?;
    let stamp = after.trim_start();
    let date = stamp.get(0..10)?;
    let time = stamp.get(11..19)?;
    if !matches!(stamp.as_bytes().get(10), Some(b' ' | b'T')) {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (
        date.get(0..4)?.parse().ok()?,
        date.get(5..7)?.parse().ok()?,
        date.get(8..10)?.parse().ok()?,
    );
    let (hh, mm, ss): (i64, i64, i64) = (
        time.get(0..2)?.parse().ok()?,
        time.get(3..5)?.parse().ok()?,
        time.get(6..8)?.parse().ok()?,
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let days = days_from_civil(y, m, d);
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    u64::try_from(secs).ok().map(|s| s * 1000)
}

/// The width of the window the limit closed: `for 5 hour`, `for 30 minutes` (GLM), `5-hour usage
/// limit` (Kimi), or a named period (`weekly`). The first reading in the text wins.
fn window_ms(body: &str) -> Option<u64> {
    let at = body.to_ascii_lowercase();
    let words: Vec<&str> = at
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '(' | ')'))
        .filter(|w| !w.is_empty())
        .collect();
    words.iter().enumerate().find_map(|(i, w)| {
        let named = match *w {
            "daily" => Some(86_400_000),
            "weekly" => Some(7 * 86_400_000),
            "monthly" => Some(30 * 86_400_000),
            _ => None,
        };
        named.or_else(|| match w.split_once('-') {
            Some((n, unit)) => width(n, unit),
            None => width(w, words.get(i + 1)?),
        })
    })
}

/// `5` and `hour` as millis, `None` unless the unit is a time unit.
fn width(n: &str, unit: &str) -> Option<u64> {
    let n: u64 = n.parse().ok()?;
    let unit = unit.strip_suffix('s').unwrap_or(unit);
    let per = match unit {
        "second" => 1_000,
        "minute" => 60_000,
        "hour" => 3_600_000,
        "day" => 86_400_000,
        "week" => 7 * 86_400_000,
        _ => return None,
    };
    Some(n.saturating_mul(per))
}

/// An HTTP-date as Unix millis. Three formats because RFC 9110 requires a recipient to accept all
/// three, and a provider that sends the obsolete ones is exactly the one that sends a limit.
fn http_date_ms(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    // IMF-fixdate: `Sun, 06 Nov 1994 08:49:37 GMT`; asctime: `Sun Nov  6 08:49:37 1994`.
    let (y, mo, d, time) = match parts.as_slice() {
        [_, d, mo, y, time, _] => (y.parse().ok()?, *mo, d.parse().ok()?, *time),
        [_, mo, d, time, y] => (y.parse().ok()?, *mo, d.parse().ok()?, *time),
        // RFC 850: `Sunday, 06-Nov-94 08:49:37 GMT`, the year two digits and the century a guess.
        [_, date, time, _] => {
            let (d, mo, y) = {
                let mut it = date.split('-');
                (it.next()?, it.next()?, it.next()?)
            };
            let y: i64 = y.parse().ok()?;
            let y = if y < 70 { 2_000 + y } else { 1_900 + y };
            (y, mo, d.parse().ok()?, *time)
        }
        _ => return None,
    };
    let month = month_of(mo)?;
    let time: Vec<i64> = time.split(':').map(|n| n.parse().unwrap_or(-1)).collect();
    let [hh, mm, ss] = time.as_slice() else {
        return None;
    };
    if *hh < 0 || *mm < 0 || *ss < 0 {
        return None;
    }
    let secs = days_from_civil(y, month, d) * 86_400 + hh * 3600 + mm * 60 + ss;
    u64::try_from(secs).ok().map(|s| s * 1000)
}

fn month_of(name: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let i = MONTHS.iter().position(|m| m.eq_ignore_ascii_case(name))?;
    Some(i as i64 + 1)
}

/// Days since 1970-01-01 for a civil date, proleptic Gregorian - Howard Hinnant's `days_from_civil`,
/// which is the whole reason this module needs no date crate.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The operator's own refusal, verbatim: the text a session died of on 2026-11-07.
    const GLM: &str = "http 429: Usage limit reached for 5 hour. Your limit will reset at \
                       2026-11-07 21:02:27";

    /// 2026-11-07 21:02:27 UTC, in millis.
    const RESET: u64 = 1_794_085_347_000;

    #[test]
    fn the_glm_text_is_a_wall_clock_and_a_window() {
        assert_eq!(wall_clock_ms(GLM), Some(RESET));
        assert_eq!(window_ms(GLM), Some(5 * 3_600_000));
        // An hour before the reset, the wall clock is the earlier reading and it wins.
        let now = RESET - 3_600_000;
        assert_eq!(from_prose(GLM, now), Some(RESET));
    }

    /// The window keeps a wrong zone from parking a session: a wall clock half a day out is not
    /// worth six hours of a session's life when the provider said five hours.
    #[test]
    fn the_window_beside_a_wall_clock_is_a_bound_and_the_earlier_reading_wins() {
        let body = "Usage limit reached for 1 hour. Your limit will reset at 2030-01-01 00:00:00";
        let now = 1_700_000_000_000;
        let hour = now + 3_600_000;
        assert_eq!(from_prose(body, now), Some(hour));
        // A window alone is read, and a wall clock alone is read.
        assert_eq!(
            from_prose("Usage limit reached for 30 minutes.", now),
            Some(now + 1_800_000)
        );
        assert_eq!(
            from_prose("Your limit will reset at 2023-11-14 22:13:20.", now),
            Some(now + SHORTEST),
            "a wall clock in the past is clamped up to the floor, not obeyed"
        );
        assert_eq!(from_prose("rate limited, try again later", now), None);
        assert_eq!(from_prose("", now), None);
    }

    /// A reading further out than the ceiling is capped: a weekly quota is not waited for.
    #[test]
    fn a_reset_beyond_the_ceiling_is_capped_by_it() {
        let now = 1_700_000_000_000;
        let body = "Your limit will reset at 2030-01-01 00:00:00.";
        assert_eq!(from_prose(body, now), Some(now + LONGEST));
    }

    /// Kimi's refusal, verbatim: no instant, the window written as `5-hour`. Read as the window,
    /// so the session parks instead of retrying a limit that the next call cannot lift.
    #[test]
    fn the_kimi_text_is_a_hyphenated_window() {
        let kimi = "http 429: You've reached your 5-hour usage limit. Your quota will reset when \
                    the current 5-hour window ends. To continue now, purchase extra usage or \
                    upgrade your plan: https://www.kimi.com/settings/subscription?tab=quota";
        let now = 1_700_000_000_000;
        assert_eq!(window_ms(kimi), Some(5 * 3_600_000));
        assert_eq!(from_prose(kimi, now), Some(now + 5 * 3_600_000));
        assert_eq!(
            window_ms("your weekly usage limit is reached"),
            Some(7 * 86_400_000)
        );
        // A number beside a word that is not a unit is not a window.
        assert_eq!(window_ms("http 429: 3 requests in flight"), None);
        assert_eq!(window_ms("x-request-id 12-ab"), None);
    }

    /// The unbounded reading is the instant itself: a weekly limit says when it lifts, and a
    /// window alone is read as the window.
    #[test]
    fn the_unbounded_reading_keeps_a_weekly_reset() {
        let now = 1_700_000_000_000; // 2023-11-14 22:13:20 UTC
        let weekly =
            "Weekly/Monthly Limit Exhausted. Your limit will reset at 2023-11-17 22:13:20.";
        assert_eq!(
            from_prose_unbounded(weekly, now),
            Some(now + 3 * 86_400_000)
        );
        assert_eq!(from_prose(weekly, now), Some(now + LONGEST));
        assert_eq!(
            from_prose_unbounded("Usage limit reached for 1 week.", now),
            Some(now + 7 * 86_400_000)
        );
        assert_eq!(
            from_prose_unbounded("Your limit will reset at 2023-11-14 22:13:20.", now),
            Some(now + SHORTEST),
            "a wall clock already past is still floored"
        );
        assert_eq!(from_prose_unbounded("rate limited", now), None);
    }

    /// RFC 9110's two forms: a delay and an HTTP-date, the second against the caller's clock.
    #[test]
    fn retry_after_is_a_delay_or_a_date() {
        let now = 784_111_777_000; // Sun, 06 Nov 1994 08:49:37 GMT
        assert_eq!(from_retry_after("120", now), Some(120));
        assert_eq!(from_retry_after(" 120 ", now), Some(120));
        assert_eq!(
            from_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", now),
            Some(0),
            "a date already reached is no wait at all"
        );
        assert_eq!(
            from_retry_after("Sun, 06 Nov 1994 08:50:37 GMT", now),
            Some(60)
        );
        // asctime and RFC 850 name the same instant.
        assert_eq!(from_retry_after("Sun Nov  6 08:50:37 1994", now), Some(60));
        assert_eq!(
            from_retry_after("Sunday, 06-Nov-94 08:50:37 GMT", now),
            Some(60)
        );
        assert_eq!(from_retry_after("soon", now), None);
        assert_eq!(from_retry_after("-5", now), None);
    }
}
