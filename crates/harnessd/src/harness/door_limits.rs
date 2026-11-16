//! What the model picker says beside each provider: its newest usage reading, or that it is
//! limited, or that it refuses. Daemon-wide and keyed by door, because a picker is choosing which
//! door to take next and this box holds one key per door.

use std::collections::BTreeMap;
use std::sync::Mutex;

use letibot_provider::usage_poll::{self, Window};
use letibot_provider::usage_warn::Thresholds;
use letibot_sessionlog::protocol::{MODEL_LIMITS_KEY, SettingRow};

/// What a door's own refusals said, as the daemon last heard them.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Trouble {
    /// A named reset not yet passed, Unix millis.
    pub(crate) limited_until_ms: Option<u64>,
    /// A refusal no wait lifts: the GLM code if it carried one, and when.
    pub(crate) refused: Option<(Option<u16>, u64)>,
}

static TROUBLE: Mutex<BTreeMap<String, Trouble>> = Mutex::new(BTreeMap::new());

fn trouble() -> std::sync::MutexGuard<'static, BTreeMap<String, Trouble>> {
    TROUBLE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The door of a switch name, `glm-coding` of `glm-coding/glm-5`.
pub(crate) fn door_of(model: &str) -> &str {
    model.split('/').next().unwrap_or(model)
}

pub(crate) fn note_limited(door: &str, until_ms: u64) {
    let mut g = trouble();
    let t = g.entry(door.to_string()).or_default();
    t.limited_until_ms = Some(t.limited_until_ms.map_or(until_ms, |u| u.max(until_ms)));
}

pub(crate) fn note_refused(door: &str, code: Option<u16>, at_ms: u64) {
    trouble().entry(door.to_string()).or_default().refused = Some((code, at_ms));
}

/// A round on `door` answered: whatever it refused or was limited by has lifted.
pub(crate) fn answered(door: &str) {
    trouble().remove(door);
}

pub(crate) fn snapshot() -> BTreeMap<String, Trouble> {
    trouble().clone()
}

/// The words for a refusal no wait lifts, by the GLM code that named it; a refusal without one
/// was about money.
fn refusal_words(code: Option<u16>) -> String {
    let what = match code {
        Some(1309 | 1314) => "plan expired",
        Some(1311) => "model not in plan",
        Some(1313) => "fair-use restricted",
        Some(1315) => "wrong key type",
        _ => "no balance",
    };
    match code {
        Some(c) => format!("{what} ({c}) - will refuse"),
        None => format!("{what} - will refuse"),
    }
}

/// One door's line and how many thresholds it has reached (2 for limited or refused): the refusal
/// first, then a limit still in force, then its windows. `None` when nothing is known.
pub(crate) fn door_words(
    reading: Option<(&[Window], u64)>,
    t: Option<&Trouble>,
    th: &Thresholds,
    now_ms: u64,
    offset_s: &dyn Fn(u64) -> i64,
) -> Option<(usize, String)> {
    if let Some((code, at)) = t.and_then(|t| t.refused) {
        return Some((
            2,
            refusal_words(code) + &usage_poll::age(at, now_ms, usage_poll::EVERY),
        ));
    }
    let spent = reading.and_then(|(w, _)| {
        w.iter()
            .filter(|w| w.used >= 1.0)
            .filter_map(|w| w.reset_ms)
            .filter(|at| *at > now_ms)
            .max()
    });
    let named = t.and_then(|t| t.limited_until_ms).filter(|at| *at > now_ms);
    if let Some(until) = named.into_iter().chain(spent).max() {
        return Some((
            2,
            format!(
                "limited until {}",
                usage_poll::clock(until, now_ms, offset_s(until))
            ),
        ));
    }
    let (windows, at) = reading?;
    let (top, level) = letibot_provider::usage_warn::shown(windows, th)?;
    let mut parts = vec![usage_poll::label(
        top,
        now_ms,
        offset_s(top.reset_ms.unwrap_or(now_ms)),
    )];
    for w in windows.iter().filter(|w| !std::ptr::eq(*w, top)) {
        parts.push(format!("{} {}%", w.name, (w.used * 100.0).round() as i64));
    }
    Some((
        level,
        parts.join(" · ") + &usage_poll::age(at, now_ms, usage_poll::EVERY),
    ))
}

/// The `models.limits` row: one `<door> <level> <words>` choice per door the daemon knows anything
/// of. `None` when it knows nothing, so no door is drawn with a guessed figure.
pub(crate) fn limits_row(
    readings: &[(String, Vec<Window>, u64)],
    trouble: &BTreeMap<String, Trouble>,
    th: &dyn Fn(&str) -> Thresholds,
    now_ms: u64,
    offset_s: &dyn Fn(u64) -> i64,
) -> Option<SettingRow> {
    let mut doors: Vec<&str> = readings.iter().map(|(d, _, _)| d.as_str()).collect();
    doors.extend(trouble.keys().map(String::as_str));
    doors.sort_unstable();
    doors.dedup();
    let choices: Vec<String> = doors
        .into_iter()
        .filter_map(|door| {
            let reading = readings
                .iter()
                .find(|(d, _, _)| d == door)
                .map(|(_, w, at)| (w.as_slice(), *at));
            let (level, words) =
                door_words(reading, trouble.get(door), &th(door), now_ms, offset_s)?;
            Some(format!("{door} {level} {words}"))
        })
        .collect();
    (!choices.is_empty()).then(|| SettingRow {
        key: MODEL_LIMITS_KEY.into(),
        value: "one line per door: its usage, limit or refusal".into(),
        source: "the usage poll and this daemon's refusals".into(),
        editable: String::new(),
        choices,
        tools: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;
    const H: u64 = 3_600_000;

    fn utc(_: u64) -> i64 {
        0
    }

    fn w(name: &str, used: f64, reset_ms: Option<u64>) -> Window {
        Window {
            name: name.into(),
            used,
            reset_ms,
        }
    }

    fn words(reading: Option<(&[Window], u64)>, t: Option<&Trouble>) -> (usize, String) {
        door_words(reading, t, &Thresholds::default(), NOW, &utc).unwrap()
    }

    #[test]
    fn a_reading_is_every_window_with_the_shown_ones_reset() {
        let ws = [
            w("5h", 0.81, Some(NOW + H)),
            w("week", 0.4, Some(NOW + 50 * H)),
        ];
        let (level, s) = words(Some((&ws, NOW)), None);
        assert_eq!(s, "5h 81% · resets 09:00 · week 40%");
        assert_eq!(level, 1, "81% is past the 5h window's 80");
    }

    #[test]
    fn a_stale_reading_says_its_age() {
        let ws = [w("5h", 0.2, None)];
        assert_eq!(
            words(Some((&ws, NOW - 30 * 60_000)), None).1,
            "5h 20% (30m ago)"
        );
    }

    #[test]
    fn a_limit_in_force_beats_the_windows_and_a_spent_window_is_one() {
        let ws = [w("5h", 0.3, Some(NOW + H))];
        let t = Trouble {
            limited_until_ms: Some(NOW + 2 * H),
            refused: None,
        };
        assert_eq!(
            words(Some((&ws, NOW)), Some(&t)),
            (2, "limited until 10:00".into())
        );
        let spent = [w("5h", 1.0, Some(NOW + H))];
        assert_eq!(words(Some((&spent, NOW)), None).1, "limited until 09:00");
        let past = Trouble {
            limited_until_ms: Some(NOW - 1),
            refused: None,
        };
        assert_eq!(
            words(Some((&ws, NOW)), Some(&past)).1,
            "5h 30% · resets 09:00"
        );
    }

    #[test]
    fn a_billing_refusal_says_no_balance() {
        let t = Trouble {
            limited_until_ms: None,
            refused: Some((Some(1113), NOW)),
        };
        assert_eq!(
            words(None, Some(&t)),
            (2, "no balance (1113) - will refuse".into())
        );
        let money = Trouble {
            limited_until_ms: None,
            refused: Some((None, NOW)),
        };
        assert_eq!(words(None, Some(&money)).1, "no balance - will refuse");
    }

    #[test]
    fn a_door_with_nothing_known_is_no_row() {
        let th = |_: &str| Thresholds::default();
        assert!(limits_row(&[], &BTreeMap::new(), &th, NOW, &utc).is_none());
        let readings = vec![("kimi-code".to_string(), vec![w("5h", 0.1, None)], NOW)];
        let mut t = BTreeMap::new();
        t.insert(
            "glm".to_string(),
            Trouble {
                limited_until_ms: None,
                refused: Some((Some(1113), NOW)),
            },
        );
        let r = limits_row(&readings, &t, &th, NOW, &utc).unwrap();
        assert_eq!(r.key, MODEL_LIMITS_KEY);
        assert_eq!(
            r.choices,
            [
                "glm 2 no balance (1113) - will refuse",
                "kimi-code 0 5h 10%"
            ]
        );
    }
}
