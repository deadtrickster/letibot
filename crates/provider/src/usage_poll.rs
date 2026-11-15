//! A slow background poll of a plan's usage endpoint ([`Preset::usage_url`]), so the operator sees
//! how much of a window is spent before a refusal says so. One poller per endpoint, shared by every
//! session watching it; it ends before its next request once no watcher is still on the preset.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

use crate::keys::Credentials;
use crate::presets::Preset;

/// Minutes, not seconds: the reading is a glance, and the endpoint is the plan's, not ours.
pub const EVERY: Duration = Duration::from_secs(300);

/// One window of a plan as its endpoint reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// `5h`, `month`: short, because it is drawn in the status row.
    pub name: String,
    /// Spent share, 0.0 to 1.0 (over 1.0 when the plan reports an overrun).
    pub used: f64,
    /// When the window resets, Unix millis, if the plan says.
    pub reset_ms: Option<u64>,
}

/// Every window in a usage answer: Kimi's `usages` map (else its `limits[]`), or z.ai's
/// `data.limits[]`. Two windows reporting the same use and reset are one, under the shorter name.
pub fn windows(body: &Value) -> Vec<Window> {
    let mut out: Vec<Window> = Vec::new();
    for (k, w) in body["usages"].as_object().into_iter().flatten() {
        let Some(used) = w["used_ratio"].as_f64() else {
            continue;
        };
        let name = k.strip_prefix("limit_").unwrap_or(k);
        let name = name
            .strip_suffix("_total")
            .unwrap_or(name)
            .replace('_', " ");
        let reset_ms = w["reset_time"]
            .as_str()
            .and_then(letibot_http::reset::rfc3339_ms);
        out.push(Window {
            name,
            used,
            reset_ms,
        });
    }
    if out.is_empty() {
        out.extend(kimi_limits(body));
    }
    out.extend(zai_limits(body));
    let mut kept: Vec<Window> = Vec::new();
    for w in out {
        match kept
            .iter_mut()
            .find(|k| k.used == w.used && k.reset_ms == w.reset_ms)
        {
            Some(k) if w.name.len() < k.name.len() => k.name = w.name,
            Some(_) => {}
            None => kept.push(w),
        }
    }
    kept
}

/// Kimi's `limits[]`: `window.duration` in `timeUnit`, `detail.used` of `detail.limit`.
fn kimi_limits(body: &Value) -> impl Iterator<Item = Window> + '_ {
    body["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let d = &l["detail"];
            let count = |k: &str| -> Option<f64> {
                d[k].as_f64()
                    .or_else(|| d[k].as_str().and_then(|s| s.parse().ok()))
            };
            let limit = count("limit").filter(|n| *n > 0.0)?;
            let minutes = match (
                l["window"]["duration"].as_u64(),
                l["window"]["timeUnit"].as_str(),
            ) {
                (Some(n), Some("TIME_UNIT_MINUTE")) => Some(n),
                (Some(n), Some("TIME_UNIT_HOUR")) => Some(n * 60),
                (Some(n), Some("TIME_UNIT_DAY")) => Some(n * 1440),
                _ => None,
            };
            let name = match minutes {
                Some(m) if m % 1440 == 0 => format!("{}d", m / 1440),
                Some(m) if m % 60 == 0 => format!("{}h", m / 60),
                Some(m) => format!("{m}m"),
                None => "window".to_string(),
            };
            Some(Window {
                name,
                used: count("used")? / limit,
                reset_ms: d["resetTime"]
                    .as_str()
                    .and_then(letibot_http::reset::rfc3339_ms),
            })
        })
}

/// Z.ai's `data.limits[]`: `percentage` 0-100 and `nextResetTime` in millis. `unit` 3 is hours and
/// 6 is read as days (`number` 7 beside a weekly reset); any other window is named by its `type`.
fn zai_limits(body: &Value) -> impl Iterator<Item = Window> + '_ {
    body["data"]["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let name = match (l["unit"].as_u64(), l["number"].as_u64()) {
                (Some(3), Some(n)) => format!("{n}h"),
                (Some(6), Some(n)) => format!("{n}d"),
                _ => l["type"]
                    .as_str()
                    .unwrap_or("window")
                    .trim_end_matches("_LIMIT")
                    .to_lowercase(),
            };
            Some(Window {
                name,
                used: l["percentage"].as_f64()? / 100.0,
                reset_ms: l["nextResetTime"].as_u64().filter(|ms| *ms > 0),
            })
        })
}

/// The window nearest its limit, the one worth the status row; on a tie the one that resets first.
pub fn busiest(windows: &[Window]) -> Option<&Window> {
    windows.iter().max_by(|a, b| {
        a.used.total_cmp(&b.used).then_with(|| {
            b.reset_ms
                .unwrap_or(u64::MAX)
                .cmp(&a.reset_ms.unwrap_or(u64::MAX))
        })
    })
}

/// `5h 44% · resets 07:07`, the reset in the zone `offset_s` east of UTC: a clock within a day,
/// a weekday and clock (`Mon 08:00`) within a week, a date (`Dec 13`) beyond it.
pub fn label(w: &Window, now_ms: u64, offset_s: i64) -> String {
    let pct = (w.used * 100.0).round() as i64;
    let Some(at) = w.reset_ms else {
        return format!("{} {pct}%", w.name);
    };
    let local = (at / 1000) as i64 + offset_s;
    let s = local.rem_euclid(86_400);
    let clock = format!("{:02}:{:02}", s / 3600, s % 3600 / 60);
    let ahead = at.saturating_sub(now_ms);
    let when = if ahead < 24 * 3_600_000 {
        clock
    } else if ahead < 7 * 24 * 3_600_000 {
        // 1970-01-01 was a Thursday.
        const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
        format!(
            "{} {clock}",
            DAYS[local.div_euclid(86_400).rem_euclid(7) as usize]
        )
    } else {
        let (_, m, d) = civil(local.div_euclid(86_400));
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!("{} {d}", MONTHS[(m - 1) as usize])
    };
    format!("{} {pct}% · resets {when}", w.name)
}

/// Days since 1970-01-01 as `(year, month, day)` (Howard Hinnant's `civil_from_days`).
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// One request to the endpoint, parsed. `None` on any failure: the last reading stands.
pub fn fetch(url: &str, creds: &Credentials) -> Option<Vec<Window>> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(crate::usage::TIMEOUT))
            .build(),
    );
    // Z.ai's monitor takes the bare key: `Bearer <key>` is a 401 there.
    let (auth, ua) = if url.contains(crate::usage::ZAI_QUOTA) {
        (creds.key.clone(), "letibot")
    } else {
        (format!("Bearer {}", creds.key), crate::kimi::USER_AGENT)
    };
    let body: Value = agent
        .get(url)
        .header("Authorization", &auth)
        .header("User-Agent", ua)
        .call()
        .ok()?
        .body_mut()
        .read_json()
        .ok()?;
    let w = windows(&body);
    (!w.is_empty()).then_some(w)
}

/// What a watcher is handed each reading.
pub type Tell = Box<dyn Fn(&[Window]) + Send>;

/// A session watching an endpoint: `still` says it is on the preset yet, `tell` hands it a reading.
pub struct Watcher {
    pub still: Box<dyn Fn() -> bool + Send>,
    pub tell: Tell,
}

#[derive(Default)]
struct Poll {
    reading: Option<Vec<Window>>,
    watchers: BTreeMap<String, Watcher>,
    running: bool,
}

static POLLS: Mutex<BTreeMap<String, Poll>> = Mutex::new(BTreeMap::new());

fn polls() -> std::sync::MutexGuard<'static, BTreeMap<String, Poll>> {
    POLLS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Watch `preset`'s endpoint as `who`, polled every `every`, and the last reading if there is one.
/// A preset with no `usage_url` is no poll: `who` stops watching anything and `None` comes back.
pub fn watch(
    preset: &Preset,
    creds: &Credentials,
    who: &str,
    every: Duration,
    watcher: Watcher,
) -> Option<Vec<Window>> {
    let Some(url) = crate::usage::url(preset, creds) else {
        unwatch(who);
        return None;
    };
    let mut g = polls();
    for (u, p) in g.iter_mut() {
        if *u != url {
            p.watchers.remove(who);
        }
    }
    let poll = g.entry(url.clone()).or_default();
    poll.watchers.insert(who.to_string(), watcher);
    if !poll.running {
        poll.running = true;
        let creds = creds.clone();
        std::thread::spawn(move || run(url, creds, every));
    }
    poll.reading.clone()
}

/// `who` is on no endpoint any more.
pub fn unwatch(who: &str) {
    for p in polls().values_mut() {
        p.watchers.remove(who);
    }
}

/// The last reading of `url`, without watching it.
pub fn reading(url: &str) -> Option<Vec<Window>> {
    polls().get(url).and_then(|p| p.reading.clone())
}

fn run(url: String, creds: Credentials, every: Duration) {
    // The first request needs no check: `watch` was the proof someone is on the preset.
    loop {
        if let Some(w) = fetch(&url, &creds) {
            let mut g = polls();
            if let Some(p) = g.get_mut(&url) {
                for watcher in p.watchers.values() {
                    (watcher.tell)(&w);
                }
                p.reading = Some(w);
            }
        }
        std::thread::sleep(every);
        let mut g = polls();
        let Some(p) = g.get_mut(&url) else { return };
        p.watchers.retain(|_, w| (w.still)());
        if p.watchers.is_empty() {
            p.running = false;
            return;
        }
    }
}

#[cfg(test)]
mod tests;
