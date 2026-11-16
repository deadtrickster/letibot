//! Approaching a limit is said before the refusal: a window passing one of its kind's thresholds
//! is one notice, said once per window and again once that window has reset.

use std::collections::BTreeMap;

use crate::usage_poll::Window;

/// The three window kinds a threshold list is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    FiveH,
    Weekly,
    Monthly,
}

impl Kind {
    /// The key in `[provider.usage_warn]`.
    pub fn key(self) -> &'static str {
        match self {
            Kind::FiveH => "5h",
            Kind::Weekly => "weekly",
            Kind::Monthly => "monthly",
        }
    }

    /// The kind of a window by its name: hours are the short window, 7 days a week, 28 to 31 days
    /// or `month` a month. Any other window has no thresholds.
    pub fn of(name: &str) -> Option<Kind> {
        let n = name.trim().to_ascii_lowercase();
        match n.as_str() {
            "week" | "weekly" => return Some(Kind::Weekly),
            "month" | "monthly" => return Some(Kind::Monthly),
            _ => {}
        }
        let (num, unit) = n.split_at(n.find(|c: char| !c.is_ascii_digit())?);
        let num: u64 = num.parse().ok()?;
        match (unit, num) {
            ("h", 1..=24) => Some(Kind::FiveH),
            ("d", 7) => Some(Kind::Weekly),
            ("d", 28..=31) => Some(Kind::Monthly),
            _ => None,
        }
    }
}

/// Percent thresholds per kind, ascending. An empty list warns never.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thresholds {
    pub five_h: Vec<u8>,
    pub weekly: Vec<u8>,
    pub monthly: Vec<u8>,
}

impl Default for Thresholds {
    /// The operator's numbers: the 5h window at 80%, weekly and monthly at 70%, each again at 95%.
    fn default() -> Self {
        Thresholds {
            five_h: vec![80, 95],
            weekly: vec![70, 95],
            monthly: vec![70, 95],
        }
    }
}

impl Thresholds {
    pub fn of(&self, kind: Kind) -> &[u8] {
        match kind {
            Kind::FiveH => &self.five_h,
            Kind::Weekly => &self.weekly,
            Kind::Monthly => &self.monthly,
        }
    }

    /// How many of its kind's thresholds `w` has reached: 0 below the first, or a window with none.
    pub fn level(&self, w: &Window) -> usize {
        let Some(kind) = Kind::of(&w.name) else {
            return 0;
        };
        let pct = w.used * 100.0;
        self.of(kind)
            .iter()
            .filter(|t| pct >= f64::from(**t))
            .count()
    }
}

/// The window the status row shows: the one past most thresholds, then the busiest, then the one
/// that resets first. Its level is what the row is coloured by.
pub fn shown<'a>(windows: &'a [Window], th: &Thresholds) -> Option<(&'a Window, usize)> {
    windows
        .iter()
        .map(|w| (w, th.level(w)))
        .max_by(|(a, la), (b, lb)| {
            la.cmp(lb).then(a.used.total_cmp(&b.used)).then_with(|| {
                b.reset_ms
                    .unwrap_or(u64::MAX)
                    .cmp(&a.reset_ms.unwrap_or(u64::MAX))
            })
        })
}

/// What one session has been told, per window: the window's reset and the level said for it.
#[derive(Debug, Default)]
pub struct Said {
    by_window: BTreeMap<String, (Option<u64>, usize)>,
    /// How far a reset read back from a stored notice can be off, until a reading replaces it.
    slack: BTreeMap<String, u64>,
}

/// Providers report a window's reset with a few seconds of jitter between polls; only a reset
/// that moved by more than that starts a new window.
const RESET_JITTER_MS: u64 = 10 * 60 * 1000;

fn same_reset(a: Option<u64>, b: Option<u64>, slack: u64) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.abs_diff(b) <= RESET_JITTER_MS + slack,
        (a, b) => a == b,
    }
}

impl Said {
    /// What a stored notice already said for `w`'s window, so a reopened session does not say it
    /// again. `slack_ms` is how far `w`'s reset, read back from prose, can be from the true one.
    pub fn seed(&mut self, w: &Window, level: usize, slack_ms: u64) {
        self.by_window.insert(w.name.clone(), (w.reset_ms, level));
        self.slack.insert(w.name.clone(), slack_ms);
    }

    /// The windows of this reading that passed a threshold not yet said for them. A window whose
    /// reset moved is a new window, said from zero. A jump past two thresholds is one notice.
    pub fn crossings<'a>(&mut self, windows: &'a [Window], th: &Thresholds) -> Vec<&'a Window> {
        let mut out = Vec::new();
        for w in windows {
            let level = th.level(w);
            let e = self
                .by_window
                .entry(w.name.clone())
                .or_insert((w.reset_ms, 0));
            let slack = self.slack.remove(&w.name).unwrap_or(0);
            if slack > 0 && same_reset(e.0, w.reset_ms, slack) {
                e.0 = w.reset_ms;
            }
            // A plan that names no reset is known to have reset only by its use falling.
            if !same_reset(e.0, w.reset_ms, 0) || (w.reset_ms.is_none() && level < e.1) {
                *e = (w.reset_ms, 0);
            }
            if level > e.1 {
                e.1 = level;
                out.push(w);
            }
        }
        out
    }
}

/// The notice for one crossing: `kimi-code 5h at 82% · resets 07:07`.
pub fn notice(door: &str, w: &Window, now_ms: u64, offset_s: i64) -> String {
    let label = crate::usage_poll::label(w, now_ms, offset_s);
    let rest = label.strip_prefix(&w.name).unwrap_or(&label).trim_start();
    format!("{door} {} at {rest}", w.name)
}

/// A [`notice`] read back: its door, and its window as said. The reset is read at `written_ms`,
/// when the row was stored, and comes with how far it can be off ([`crate::usage_poll::reset_of`]).
pub fn read_notice(text: &str, written_ms: u64, offset_s: i64) -> Option<(&str, Window, u64)> {
    let (door, rest) = text.split_once(' ')?;
    let (name, rest) = rest.split_once(" at ")?;
    let (pct, when) = match rest.split_once(" · resets ") {
        Some((p, w)) => (p, Some(w)),
        None => (rest, None),
    };
    let pct: f64 = pct.strip_suffix('%')?.parse().ok()?;
    let (reset_ms, slack) = match when {
        Some(w) => {
            let (r, s) = crate::usage_poll::reset_of(w, written_ms, offset_s)?;
            (Some(r), s)
        }
        None => (None, 0),
    };
    let w = Window {
        name: name.into(),
        used: pct / 100.0,
        reset_ms,
    };
    Some((door, w, slack))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reset_that_jitters_by_seconds_is_the_same_window_and_says_nothing_new() {
        let th = Thresholds::default();
        let mut said = Said::default();
        let t = 1_794_834_429_000;
        assert_eq!(said.crossings(&[w("5h", 1.0, Some(t))], &th).len(), 1);
        for k in 1..6 {
            assert!(
                said.crossings(&[w("5h", 1.0, Some(t + k * 1000))], &th)
                    .is_empty()
            );
        }
        let next = t + 5 * 3_600_000;
        assert!(said.crossings(&[w("5h", 0.2, Some(next))], &th).is_empty());
        assert_eq!(said.crossings(&[w("5h", 0.85, Some(next))], &th).len(), 1);
    }

    /// rano's 19:05: the old daemon said 5h at 100% for a reset; the new one reads that row back
    /// and a reading of the same window, seconds off, says nothing. The next window still does.
    #[test]
    fn a_stored_notice_is_not_said_again_for_its_window() {
        let th = Thresholds::default();
        let t = 1_794_834_429_000;
        let written = t - 53 * 60_000;
        for offset in [0, 3 * 3600, -5 * 3600] {
            let text = notice("kimi-code-global", &w("5h", 1.0, Some(t)), written, offset);
            let (door, seen, slack) = read_notice(&text, written, offset).unwrap();
            assert_eq!(door, "kimi-code-global");
            assert!(seen.reset_ms.unwrap().abs_diff(t) <= slack, "{text}");
            let mut said = Said::default();
            said.seed(&seen, th.level(&seen), slack);
            assert!(
                said.crossings(&[w("5h", 1.0, Some(t + 2000))], &th)
                    .is_empty()
            );
            let next = t + 5 * 3_600_000;
            assert_eq!(said.crossings(&[w("5h", 0.85, Some(next))], &th).len(), 1);
        }
    }

    #[test]
    fn a_reset_reads_back_from_each_form_of_its_label() {
        let now = 1_794_834_429_000;
        for ahead in [5 * 60_000, 4 * 3_600_000, 3 * 86_400_000, 20 * 86_400_000] {
            let at = now + ahead;
            let text = notice("d", &w("7d", 0.7, Some(at)), now, 3600);
            let (_, seen, slack) = read_notice(&text, now + 60_000, 3600).unwrap();
            assert!(seen.reset_ms.unwrap().abs_diff(at) <= slack, "{text}");
        }
        let (_, seen, _) = read_notice("d 5h at 82%", now, 0).unwrap();
        assert_eq!((seen.name.as_str(), seen.reset_ms), ("5h", None));
        assert!(read_notice("d 5h at lots", now, 0).is_none());
    }

    fn w(name: &str, used: f64, reset_ms: Option<u64>) -> Window {
        Window {
            name: name.into(),
            used,
            reset_ms,
        }
    }

    #[test]
    fn window_names_have_kinds() {
        assert_eq!(Kind::of("5h"), Some(Kind::FiveH));
        assert_eq!(Kind::of("7d"), Some(Kind::Weekly));
        assert_eq!(Kind::of("week"), Some(Kind::Weekly));
        assert_eq!(Kind::of("month"), Some(Kind::Monthly));
        assert_eq!(Kind::of("30d"), Some(Kind::Monthly));
        assert_eq!(Kind::of("time"), None);
        assert_eq!(Kind::of("3d"), None);
    }

    #[test]
    fn eighty_then_ninety_five_then_nothing_until_the_reset() {
        let th = Thresholds::default();
        let mut said = Said::default();
        let r1 = Some(1_000);
        assert!(said.crossings(&[w("5h", 0.79, r1)], &th).is_empty());
        assert_eq!(said.crossings(&[w("5h", 0.82, r1)], &th).len(), 1);
        assert!(said.crossings(&[w("5h", 0.90, r1)], &th).is_empty());
        assert_eq!(said.crossings(&[w("5h", 0.96, r1)], &th).len(), 1);
        assert!(said.crossings(&[w("5h", 1.0, r1)], &th).is_empty());
        let r2 = Some(1_000 + 5 * 3_600_000);
        assert!(
            said.crossings(&[w("5h", 0.1, r2)], &th).is_empty(),
            "the reset alone is no notice"
        );
        assert_eq!(said.crossings(&[w("5h", 0.85, r2)], &th).len(), 1);
    }

    #[test]
    fn a_jump_past_both_thresholds_is_one_notice() {
        let th = Thresholds::default();
        let mut said = Said::default();
        assert_eq!(said.crossings(&[w("5h", 0.97, Some(1))], &th).len(), 1);
        assert!(said.crossings(&[w("5h", 0.98, Some(1))], &th).is_empty());
    }

    #[test]
    fn weekly_and_monthly_warn_at_seventy() {
        let th = Thresholds::default();
        let mut said = Said::default();
        let ws = [
            w("5h", 0.75, Some(1)),
            w("7d", 0.71, Some(2)),
            w("month", 0.70, Some(3)),
            w("time", 0.99, Some(4)),
        ];
        let got = said.crossings(&ws, &th);
        let names: Vec<&str> = got.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["7d", "month"]);
    }

    #[test]
    fn a_window_without_a_reset_is_new_once_its_use_falls() {
        let th = Thresholds::default();
        let mut said = Said::default();
        assert_eq!(said.crossings(&[w("5h", 0.85, None)], &th).len(), 1);
        assert!(said.crossings(&[w("5h", 0.05, None)], &th).is_empty());
        assert_eq!(said.crossings(&[w("5h", 0.81, None)], &th).len(), 1);
    }

    #[test]
    fn the_shown_window_is_the_one_past_most_thresholds() {
        let th = Thresholds::default();
        let ws = [w("5h", 0.75, Some(1)), w("7d", 0.72, Some(2))];
        let (s, level) = shown(&ws, &th).unwrap();
        assert_eq!((s.name.as_str(), level), ("7d", 1));
        let off = Thresholds {
            weekly: vec![],
            ..Thresholds::default()
        };
        let (s, level) = shown(&ws, &off).unwrap();
        assert_eq!((s.name.as_str(), level), ("5h", 0));
    }

    #[test]
    fn the_notice_names_the_door_the_window_and_the_reset() {
        let reset = 7 * 3_600_000 + 7 * 60_000;
        let n = notice("kimi-code", &w("5h", 0.82, Some(reset)), 3 * 3_600_000, 0);
        assert_eq!(n, "kimi-code 5h at 82% · resets 07:07");
        assert_eq!(notice("glm", &w("5h", 0.8, None), 0, 0), "glm 5h at 80%");
    }
}
