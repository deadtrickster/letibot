//! The two memory bounds G5 puts ABOVE a job's cap (`docs/lifecycle.md` d.7): `memory.high` on an
//! owner's cgroup, and `MemoryHigh` on `letibot.slice`. Both are throttles - the kernel reclaims
//! under them and kills nothing - which is why the box gets one and only a job gets `memory.max`.
//!
//! ```toml
//! [memory]                       # providers.toml, beside `daemon_high`
//! owner_high = "8G"              # each owner's share; `max` leaves owners unbounded
//! slice_high = "60%"             # letibot as a whole; the default is 60% of the box
//! ```
//!
//! The resolution is here and not at the cgroup, because *what 60% of* is a decision: a size is
//! bytes, a fraction or a percent is of the box's total RAM, and `max` is no bound at all. A
//! malformed value is refused with the file's name rather than silently meaning "no bound".

use letibot_provider::keys::{self, MemorySection};
use letibot_tools::exec::memory::{Amount, Meminfo};

/// What `[memory] slice_high` says when the file says nothing: letibot as a whole is throttled at
/// 60% of the box, so another project's build still has room (`docs/lifecycle.md` d.7).
pub const SLICE_SHARE_DEFAULT: f64 = 0.60;

/// One key of the table, resolved: `None` is no bound at all (`max`, `off`, `none`), and `Err`
/// names the key and what was wrong with it.
fn resolve(raw: Option<&str>, total: u64, key: &str) -> Result<Option<u64>, String> {
    let Some(text) = raw.map(str::trim) else {
        return Ok(None);
    };
    if matches!(text, "max" | "off" | "none") {
        return Ok(None);
    }
    let amount = Amount::parse(text).map_err(|e| format!("[memory] {key}: {e}"))?;
    Ok(Some(amount.resolve(total)))
}

/// The owner's share, from the keys the file holds. `None` is *nobody asked*, which is the
/// default: an operator who has not thought about it gets no throttle on their own session.
pub fn owner_high_from(sec: &MemorySection, total: u64) -> Result<Option<u64>, String> {
    resolve(sec.owner_high.as_deref(), total, "owner_high")
}

/// The slice's bound, from the keys the file holds. Absent means [`SLICE_SHARE_DEFAULT`] of the
/// box; `max` turns it off, which is a decision and not an absence.
pub fn slice_high_from(sec: &MemorySection, total: u64) -> Result<Option<u64>, String> {
    if sec.slice_high.is_none() {
        return Ok(Some((total as f64 * SLICE_SHARE_DEFAULT) as u64));
    }
    resolve(sec.slice_high.as_deref(), total, "slice_high")
}

/// This box's total RAM, or 0 when `/proc/meminfo` is not there to say - a box with no meminfo is
/// a box whose fractions mean nothing, and 0 resolves to *no throttle* rather than to a bound of
/// zero bytes, which would freeze every command it is given.
fn total() -> u64 {
    Meminfo::read().map(|m| m.total).unwrap_or(0)
}

/// [`owner_high_from`] for this box.
pub fn owner_high() -> Option<u64> {
    let sec = keys::memory(None);
    match owner_high_from(&sec, total()) {
        Ok(v) => v.filter(|b| *b > 0),
        Err(why) => {
            eprintln!("harnessd: {why}; owners run without a memory.high");
            None
        }
    }
}

/// [`slice_high_from`] for this box.
pub fn slice_high() -> Result<Option<u64>, String> {
    slice_high_from(&keys::memory(None), total())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(owner: Option<&str>, slice: Option<&str>) -> MemorySection {
        MemorySection {
            owner_high: owner.map(str::to_string),
            slice_high: slice.map(str::to_string),
            ..MemorySection::default()
        }
    }

    #[test]
    fn an_absent_owner_key_is_no_throttle_and_an_absent_slice_key_is_sixty_percent() {
        let total = 32 << 30;
        assert_eq!(owner_high_from(&sec(None, None), total), Ok(None));
        assert_eq!(
            slice_high_from(&sec(None, None), total),
            Ok(Some((total as f64 * 0.60) as u64)),
            "the slice has a default because a box with no slice bound starves its neighbours"
        );
    }

    #[test]
    fn a_size_a_percent_and_max_all_read_as_what_they_mean() {
        let total = 32 << 30;
        assert_eq!(
            owner_high_from(&sec(Some("8G"), None), total),
            Ok(Some(8 << 30))
        );
        assert_eq!(
            slice_high_from(&sec(None, Some("50%")), total),
            Ok(Some(total / 2)),
            "a percent is of the BOX, not of what is free at this instant"
        );
        assert_eq!(
            owner_high_from(&sec(Some("max"), None), total),
            Ok(None),
            "`max` is the operator turning it off, not a malformed value"
        );
        assert_eq!(slice_high_from(&sec(None, Some("off")), total), Ok(None));
    }

    #[test]
    fn a_malformed_key_is_refused_by_name() {
        let why = owner_high_from(&sec(Some("lots"), None), 1 << 30).unwrap_err();
        assert!(why.contains("owner_high"), "{why}");
        assert!(why.contains("lots"), "{why}");
        let why = slice_high_from(&sec(None, Some("60%%")), 1 << 30).unwrap_err();
        assert!(why.contains("slice_high"), "{why}");
    }
}
