//! A slow background poll of a plan's usage endpoint ([`Preset::usage_url`]), so the operator sees
//! how much of a window is spent before a refusal says so. One poller per endpoint, shared by every
//! session watching it; it ends before its next request once no watcher is still on the preset.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::keys::Credentials;
use crate::presets::Preset;

/// Minutes, not seconds: the reading is a glance, and the endpoint is the plan's, not ours.
pub const EVERY: Duration = Duration::from_secs(120);

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
    format!("{} {pct}% · resets {}", w.name, clock(at, now_ms, offset_s))
}

/// `at` in the zone `offset_s` east of UTC, as [`label`] writes a reset.
pub fn clock(at: u64, now_ms: u64, offset_s: i64) -> String {
    let local = (at / 1000) as i64 + offset_s;
    let s = local.rem_euclid(86_400);
    let clock = format!("{:02}:{:02}", s / 3600, s % 3600 / 60);
    let ahead = at.saturating_sub(now_ms);
    if ahead < 24 * 3_600_000 {
        clock
    } else if ahead < 7 * 24 * 3_600_000 {
        format!(
            "{} {clock}",
            DAYS[local.div_euclid(86_400).rem_euclid(7) as usize]
        )
    } else {
        let (_, m, d) = civil(local.div_euclid(86_400));
        format!("{} {d}", MONTHS[(m - 1) as usize])
    }
}

// 1970-01-01 was a Thursday.
const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The reset a [`label`]'s `resets WHEN` names, read back at `near_ms`, no earlier than the label
/// was made, with how far the true instant can be from it: a clock is cut to the minute, a date
/// to the day. `None` for text `label` does not write.
pub fn reset_of(when: &str, near_ms: u64, offset_s: i64) -> Option<(u64, u64)> {
    const DAY: i64 = 86_400;
    let clock = |s: &str| {
        let (h, m) = s.split_once(':')?;
        let (h, m) = (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?);
        ((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * 3600 + m * 60)
    };
    let near = (near_ms / 1000) as i64 + offset_s;
    let today = near.div_euclid(DAY);
    let (first, at, half) = match when.split_once(' ') {
        None => {
            let at = clock(when)?;
            (
                (today..=today + 1).find(|d| d * DAY + at + 60 > near)?,
                at,
                30,
            )
        }
        Some((day, hm)) if DAYS.contains(&day) => {
            let at = clock(hm)?;
            let first = (today..=today + 7)
                .find(|d| DAYS[d.rem_euclid(7) as usize] == day && d * DAY + at + 60 > near)?;
            (first, at, 30)
        }
        Some((mon, dd)) => {
            let m = MONTHS.iter().position(|x| *x == mon)? as i64 + 1;
            let dd: i64 = dd.parse().ok()?;
            let first = (today..=today + 366).find(|d| {
                let (_, cm, cd) = civil(*d);
                (cm, cd) == (m, dd)
            })?;
            (first, DAY / 2, DAY / 2)
        }
    };
    let local = first * DAY + at + half;
    Some(((local - offset_s).max(0) as u64 * 1000, half as u64 * 1000))
}

/// ` (4m ago)` for a reading taken at `at_ms` that is older than `every`, else nothing: a figure
/// the poll could not refresh says so instead of passing for the current one.
pub fn age(at_ms: u64, now_ms: u64, every: Duration) -> String {
    let old = now_ms.saturating_sub(at_ms);
    if old <= every.as_millis() as u64 {
        return String::new();
    }
    let m = old / 60_000;
    if m < 60 {
        format!(" ({m}m ago)")
    } else {
        format!(" ({}h ago)", m / 60)
    }
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
    let w = windows(&fetch_body(url, creds)?);
    (!w.is_empty()).then_some(w)
}

fn fetch_body(url: &str, creds: &Credentials) -> Option<Value> {
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
    agent
        .get(url)
        .header("Authorization", &auth)
        .header("User-Agent", ua)
        .call()
        .ok()?
        .body_mut()
        .read_json()
        .ok()
}

/// What a watcher is handed each round: the newest reading and when it was taken, Unix millis.
pub type Tell = Box<dyn Fn(&[Window], u64) + Send>;

/// A session watching an endpoint: `still` says it is on the preset yet, `tell` hands it a reading.
pub struct Watcher {
    pub still: Box<dyn Fn() -> bool + Send>,
    pub tell: Tell,
}

/// One answer of the endpoint: its windows, the latest reset among its spent ones, and when.
#[derive(Clone)]
struct Reading {
    windows: Vec<Window>,
    spent_reset_ms: Option<u64>,
    at: Instant,
    at_ms: u64,
}

#[derive(Default)]
struct Poll {
    reading: Option<Reading>,
    watchers: BTreeMap<String, Watcher>,
    running: bool,
    /// The key of the latest `watch`, so a key changed since the poll started is the one sent.
    creds: Option<Credentials>,
}

/// Clears `running` when the poll thread panics, so the next `watch` starts another. A clean end
/// clears it under the same lock that saw no watcher, so no `watch` slips between the two.
struct Stopped(String);

impl Drop for Stopped {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Some(p) = polls().get_mut(&self.0)
        {
            p.running = false;
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

static POLLS: Mutex<BTreeMap<String, Poll>> = Mutex::new(BTreeMap::new());

/// The endpoint each door was last watched or polled on.
static DOORS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// The newest reading per endpoint, copied out of [`POLLS`] so a reader never takes that lock: a
/// watcher is told under it, and a settings read can run inside the telling.
static LATEST: Mutex<BTreeMap<String, (Vec<Window>, u64)>> = Mutex::new(BTreeMap::new());

/// One request at a time per endpoint, so a caller arriving while one is out reads its answer.
static GATES: Mutex<BTreeMap<String, Arc<Mutex<()>>>> = Mutex::new(BTreeMap::new());

fn polls() -> std::sync::MutexGuard<'static, BTreeMap<String, Poll>> {
    POLLS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Watch `preset`'s endpoint as `who`, polled every `every`, and the last reading with when it was
/// taken, if there is one. A preset with no `usage_url` is no poll: `who` stops watching anything
/// and `None` comes back.
pub fn watch(
    preset: &Preset,
    creds: &Credentials,
    who: &str,
    every: Duration,
    watcher: Watcher,
) -> Option<(Vec<Window>, u64)> {
    let Some(url) = crate::usage::url(preset, creds) else {
        unwatch(who);
        return None;
    };
    note_door(preset.name, &url);
    let mut g = polls();
    for (u, p) in g.iter_mut() {
        if *u != url {
            p.watchers.remove(who);
        }
    }
    let poll = g.entry(url.clone()).or_default();
    poll.watchers.insert(who.to_string(), watcher);
    poll.creds = Some(creds.clone());
    if !poll.running {
        poll.running = true;
        std::thread::spawn(move || run(url, every));
    }
    poll.reading.as_ref().map(|r| (r.windows.clone(), r.at_ms))
}

/// `who` is on no endpoint any more.
pub fn unwatch(who: &str) {
    for p in polls().values_mut() {
        p.watchers.remove(who);
    }
}

/// The last reading of `url` and when it was taken, without watching it.
pub fn reading(url: &str) -> Option<(Vec<Window>, u64)> {
    polls()
        .get(url)
        .and_then(|p| p.reading.as_ref())
        .map(|r| (r.windows.clone(), r.at_ms))
}

fn note_door(door: &str, url: &str) {
    DOORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(door.to_string(), url.to_string());
}

/// Each door's newest reading and when it was taken, for every door watched or polled since the
/// daemon started. Takes no lock a watcher is told under, so a [`Tell`] may call it.
pub fn door_readings() -> Vec<(String, Vec<Window>, u64)> {
    let doors = DOORS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let latest = LATEST.lock().unwrap_or_else(|e| e.into_inner());
    doors
        .into_iter()
        .filter_map(|(door, url)| {
            let (w, at) = latest.get(&url)?;
            Some((door, w.clone(), *at))
        })
        .collect()
}

/// When each door was last polled on a picker's asking.
static ASKED: Mutex<BTreeMap<String, Instant>> = Mutex::new(BTreeMap::new());

/// One reading of `preset`'s endpoint for a picker that opened, unless this door was asked within
/// [`EVERY`] or holds a reading younger than that. Blocks for the request; `true` when a reading
/// was taken.
pub fn poll_once(preset: &Preset, creds: &Credentials) -> bool {
    let Some(url) = crate::usage::url(preset, creds) else {
        return false;
    };
    {
        let mut asked = ASKED.lock().unwrap_or_else(|e| e.into_inner());
        if asked
            .get(preset.name)
            .is_some_and(|at| at.elapsed() < EVERY)
        {
            return false;
        }
        asked.insert(preset.name.to_string(), Instant::now());
    }
    note_door(preset.name, &url);
    read(&url, creds, EVERY, false).is_some_and(|r| !r.windows.is_empty())
}

/// The latest reset among the spent windows of a reading of `url`: the poll's own when it is
/// younger than `fresh` and shows one, else one request made now, which every watcher is told.
pub fn spent_reset_ms(url: &str, creds: &Credentials, fresh: Duration) -> Option<u64> {
    read(url, creds, fresh, true)?.spent_reset_ms
}

/// A reading younger than `fresh` (one with a spent window, if `spent`), else a request now.
fn read(url: &str, creds: &Credentials, fresh: Duration, spent: bool) -> Option<Reading> {
    let gate = GATES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(url.to_string())
        .or_default()
        .clone();
    let _one = gate.lock().unwrap_or_else(|e| e.into_inner());
    let kept = polls().get(url).and_then(|p| p.reading.clone());
    if let Some(r) =
        kept.filter(|r| r.at.elapsed() < fresh && (!spent || r.spent_reset_ms.is_some()))
    {
        return Some(r);
    }
    let body = fetch_body(url, creds)?;
    let r = Reading {
        windows: windows(&body),
        spent_reset_ms: crate::usage::reset_of(&body),
        at: Instant::now(),
        at_ms: now_ms(),
    };
    if r.windows.is_empty() {
        return Some(r);
    }
    let mut g = polls();
    LATEST
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(url.to_string(), (r.windows.clone(), r.at_ms));
    let p = g.entry(url.to_string()).or_default();
    for watcher in p.watchers.values() {
        (watcher.tell)(&r.windows, r.at_ms);
    }
    p.reading = Some(r.clone());
    Some(r)
}

fn run(url: String, every: Duration) {
    let _stopped = Stopped(url.clone());
    // The first request needs no check: `watch` was the proof someone is on the preset.
    loop {
        let Some(creds) = polls().get(&url).and_then(|p| p.creds.clone()) else {
            return;
        };
        // A reading somebody else took since the last one serves this round, and told them.
        let fresh = read(&url, &creds, every, false).is_some_and(|r| !r.windows.is_empty());
        if !fresh {
            // The request failed: the kept reading is told again so its age shows.
            let g = polls();
            if let Some(p) = g.get(&url)
                && let Some(r) = &p.reading
            {
                for watcher in p.watchers.values() {
                    (watcher.tell)(&r.windows, r.at_ms);
                }
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
