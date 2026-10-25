//! **A guest module cannot afford a date crate, and does not need one.** The only timestamp this
//! plugin reads is the status file's `updated`, which is always the one shape (RFC3339, `Z` or an
//! offset), so a civil-date to seconds conversion is a tenth of what `chrono` would cost in the
//! `.wasm` and it is testable against the arithmetic it really is.

/// **Seconds since the Unix epoch for an RFC3339 stamp**, or `None` when the text is not one.
///
/// The calendar arithmetic is Howard Hinnant's `days_from_civil`: proleptic Gregorian days, which
/// is what every timestamp in a status file means and what `date -u` prints back.
pub fn epoch(stamp: &str) -> Option<i64> {
    let stamp = stamp.trim();
    let (date, rest) = stamp.split_once(['T', 't', ' '])?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if d.next().is_some() {
        return None;
    }
    let (time, offset) = split_offset(rest)?;
    let mut t = time.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    // A fraction is legal and irrelevant to an age measured in hours.
    let second: i64 = t.next()?.split(['.', ',']).next()?.parse().ok()?;
    if t.next().is_some()
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset)
}

/// The `Z`/offset tail of a stamp, split from the wall clock: `("21:55:00", 0)` for `Z`, and the
/// offset in seconds (positive east) for `+02:00`, which is subtracted from the derived instant.
fn split_offset(rest: &str) -> Option<(&str, i64)> {
    if let Some(time) = rest.strip_suffix(['Z', 'z']) {
        return Some((time, 0));
    }
    // The sign of an offset is the last `+` or `-` after the time's own digits; a `-` inside the
    // time cannot appear, so `rfind` is unambiguous.
    let at = rest.rfind(['+', '-'])?;
    let (time, offset) = rest.split_at(at);
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let mut parts = offset[1..].split(':');
    let hours: i64 = parts.next()?.parse().ok()?;
    let minutes: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || hours > 23 || minutes > 59 {
        return None;
    }
    Some((time, sign * (hours * 3_600 + minutes * 60)))
}

/// Days between 1970-01-01 and the civil date, proleptic Gregorian (Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_is_the_epoch() {
        assert_eq!(epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch("2000-01-01T00:00:00Z"), Some(946_684_800));
        // Checked against the box's own `date -u -j -f %Y-%m-%dT%H:%M:%SZ … +%s`.
        assert_eq!(epoch("2026-10-25T21:55:00Z"), Some(1_792_965_300));
    }

    #[test]
    fn an_offset_is_subtracted_from_the_wall_clock() {
        assert_eq!(
            epoch("2026-10-25T21:55:00Z"),
            epoch("2026-10-25T22:55:00+01:00")
        );
        assert_eq!(
            epoch("2026-10-25T21:55:00Z").map(|t| t + 7_200),
            epoch("2026-10-25T21:55:00-02:00")
        );
    }

    #[test]
    fn a_fraction_is_ignored_and_a_shape_that_is_not_a_stamp_is_none() {
        assert_eq!(
            epoch("2026-10-25T21:55:00.750Z"),
            epoch("2026-10-25T21:55:00Z")
        );
        for bad in [
            "",
            "yesterday",
            "2026-10-25",
            "2026-13-01T00:00:00Z",
            "2026-10-32T00:00:00Z",
            "2026-10-25T25:00:00Z",
            "2026-10-25T21:55:00+0100",
        ] {
            assert_eq!(epoch(bad), None, "{bad}");
        }
    }
}
