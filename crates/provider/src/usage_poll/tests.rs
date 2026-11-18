use super::*;
use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Kimi's answer with 44% of the 5-hour window spent and the month duplicated under two names.
const KIMI: &str = r#"{"limits":[{"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},"detail":{"limit":"100","used":"44","resetTime":"2026-11-15T07:07:00Z"}}],"usages":{"limit_5h":{"used_ratio":0.44,"reset_time":"2026-11-15T07:07:00Z"},"limit_month_total":{"used_ratio":0.1688,"reset_time":"2026-12-13T00:07:40Z"},"limit_month_code":{"used_ratio":0.1688,"reset_time":"2026-12-13T00:07:40Z"}}}"#;

fn ms(s: &str) -> u64 {
    letibot_http::reset::rfc3339_ms(s).unwrap()
}

fn creds() -> Credentials {
    Credentials {
        key: "k".into(),
        from: "test".into(),
        prices: Default::default(),
        url: None,
    }
}

/// A usage endpoint on loopback that answers `body` and counts the requests.
fn fake(body: &'static str) -> (String, Arc<AtomicUsize>) {
    fake_for(body, usize::MAX)
}

/// The same, a 500 after the first `good` requests.
fn fake_for(body: &'static str, good: usize) -> (String, Arc<AtomicUsize>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/coding/v1/usages", l.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let n = hits.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(&s);
            let mut line = String::new();
            while r.read_line(&mut line).is_ok_and(|k| k > 2) {
                line.clear();
            }
            if n.fetch_add(1, Ordering::SeqCst) >= good {
                let _ = write!(
                    &s,
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                continue;
            }
            let _ = write!(
                &s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, hits)
}

fn preset_at(url: &str) -> Preset {
    Preset {
        usage_url: Some(Box::leak(url.to_string().into_boxed_str())),
        ..crate::presets::KIMI_CODE
    }
}

fn until(what: impl Fn() -> bool) {
    for _ in 0..200 {
        if what() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out");
}

#[test]
fn kimi_windows_are_read_and_the_duplicate_month_is_one() {
    let w = windows(&serde_json::from_str(KIMI).unwrap());
    assert_eq!(
        w,
        vec![
            Window {
                name: "5h".into(),
                used: 0.44,
                reset_ms: Some(ms("2026-11-15T07:07:00Z")),
            },
            Window {
                name: "month".into(),
                used: 0.1688,
                reset_ms: Some(ms("2026-12-13T00:07:40Z")),
            },
        ]
    );
    let limits_only: Value =
        serde_json::json!({"limits": serde_json::from_str::<Value>(KIMI).unwrap()["limits"]});
    assert_eq!(windows(&limits_only)[0].name, "5h");
    assert_eq!(windows(&limits_only)[0].used, 0.44);
}

/// Kimi's captured body with its two shapes disagreeing on the 5-hour window: whichever reads
/// higher is the reading, and the other shape's lag cannot hide a spent window.
#[test]
fn a_kimi_body_with_both_shapes_reports_the_greater_used() {
    let mut body: Value = serde_json::from_str(KIMI).unwrap();
    body["limits"][0]["detail"]["used"] = serde_json::json!("100");
    body["limits"][0]["detail"]["resetTime"] = serde_json::json!("2026-11-15T07:07:01.5Z");
    let w = windows(&body);
    assert_eq!(w.len(), 2, "{w:?}");
    assert_eq!(w[0].name, "5h");
    assert_eq!(w[0].used, 1.0);
    assert_eq!(w[0].reset_ms, Some(ms("2026-11-15T07:07:01.5Z")));
    // The other way round: `usages` ahead of `limits[]`.
    let mut body: Value = serde_json::from_str(KIMI).unwrap();
    body["usages"]["limit_5h"]["used_ratio"] = serde_json::json!(0.9);
    assert_eq!(windows(&body)[0].used, 0.9);
}

#[test]
fn zai_windows_are_read_off_percentage() {
    let body = serde_json::json!({"data": {"limits": [
        {"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 12, "nextResetTime": 1_794_766_268_000u64},
        {"type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 41, "nextResetTime": 1_795_000_000_000u64},
        {"type": "TIME_LIMIT", "unit": 5, "number": 1, "percentage": 3, "nextResetTime": 0}]}});
    let w = windows(&body);
    assert_eq!(w[0].name, "5h");
    assert_eq!(w[0].used, 0.12);
    assert_eq!(w[1].name, "7d");
    assert_eq!(w[2].name, "time");
    assert_eq!(w[2].reset_ms, None);
}

/// The coding plan's weekly window as its console reports it: `unit` 6 with `number` 1. Named by its
/// seven days, which the status bar used to print as `1d` - and which is the one spelling
/// `usage_warn::Kind::of` reads as weekly, so the 70% notice still fires.
#[test]
fn a_zai_week_is_seven_days_and_not_one() {
    let body = serde_json::json!({"code": 200, "success": true,
        "data": {"level": "pro", "limits": [
            {"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 4,
             "nextResetTime": 1_794_766_268_000u64},
            {"type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 70,
             "nextResetTime": 1_794_668_460_000u64}]}});
    let w = windows(&body);
    assert_eq!(w[1].name, "7d", "the weekly window is a week, not a day");
    assert_eq!(w[1].used, 0.70);
    assert_eq!(
        crate::usage_warn::Kind::of(&w[1].name),
        Some(crate::usage_warn::Kind::Weekly),
        "the name a week gets is the one the threshold list knows"
    );
    assert_eq!(
        label(&w[1], ms("2026-11-11T00:00:00Z"), 0),
        "7d 70% · resets Sat 15:01",
        "the operator's own line, with the window named for what it is"
    );
    // The other spelling of the same window - seven days - still reads `7d`.
    let spelled_as_days = serde_json::json!({"data": {"limits": [
        {"type": "TOKENS_LIMIT", "unit": 6, "number": 7, "percentage": 70,
         "nextResetTime": 1_794_668_460_000u64}]}});
    assert_eq!(windows(&spelled_as_days)[0].name, "7d");
}

#[test]
fn the_label_is_the_busiest_window_in_local_time() {
    let w = windows(&serde_json::from_str(KIMI).unwrap());
    let b = busiest(&w).unwrap();
    let now = ms("2026-11-15T03:00:00Z");
    assert_eq!(label(b, now, 0), "5h 44% · resets 07:07");
    assert_eq!(label(b, now, 3 * 3600), "5h 44% · resets 10:07");
    assert_eq!(label(&w[1], now, 0), "month 17% · resets Dec 13");
    let week = Window {
        name: "7d".into(),
        used: 0.71,
        reset_ms: Some(ms("2026-11-16T08:00:00Z")),
    };
    assert_eq!(
        label(&week, ms("2026-11-14T03:00:00Z"), 0),
        "7d 71% · resets Mon 08:00"
    );
    let bare = Window {
        name: "5h".into(),
        used: 1.0,
        reset_ms: None,
    };
    assert_eq!(label(&bare, now, 0), "5h 100%");
}

#[test]
fn one_poll_serves_every_watcher_and_stops_when_none_is_on_the_preset() {
    let (url, hits) = fake(KIMI);
    let preset = preset_at(&url);
    let on = Arc::new(AtomicBool::new(true));
    let told = Arc::new(AtomicUsize::new(0));
    let watcher = |on: &Arc<AtomicBool>, told: &Arc<AtomicUsize>| {
        let (on, told) = (on.clone(), told.clone());
        Watcher {
            still: Box::new(move || on.load(Ordering::SeqCst)),
            tell: Box::new(move |w, _| {
                assert_eq!(w[0].name, "5h");
                told.fetch_add(1, Ordering::SeqCst);
            }),
        }
    };
    let every = Duration::from_millis(40);
    assert_eq!(
        watch(&preset, &creds(), "s-a", every, watcher(&on, &told)),
        None
    );
    until(|| told.load(Ordering::SeqCst) >= 1);
    // A second session gets the cached reading at once and starts no second poller.
    let cached = watch(&preset, &creds(), "s-b", every, watcher(&on, &told));
    assert_eq!(cached.unwrap().0[0].used, 0.44);
    assert_eq!(reading(&url).unwrap().0.len(), 2);
    until(|| hits.load(Ordering::SeqCst) >= 3);
    assert!(
        told.load(Ordering::SeqCst) >= hits.load(Ordering::SeqCst),
        "both watchers are told from one request"
    );
    on.store(false, Ordering::SeqCst);
    until(|| !polls().get(&url).is_some_and(|p| p.running));
    let after = hits.load(Ordering::SeqCst);
    std::thread::sleep(every * 4);
    assert_eq!(hits.load(Ordering::SeqCst), after, "no request once unused");
    assert!(reading(&url).is_some(), "the last reading stays cached");
}

#[test]
fn a_preset_without_a_usage_url_is_never_polled() {
    let told = Arc::new(AtomicBool::new(false));
    let t = told.clone();
    let w = Watcher {
        still: Box::new(|| true),
        tell: Box::new(move |_, _| t.store(true, Ordering::SeqCst)),
    };
    let got = watch(
        &crate::presets::DEEPSEEK,
        &creds(),
        "s-none",
        Duration::from_millis(10),
        w,
    );
    assert_eq!(got, None);
    assert!(polls().values().all(|p| !p.watchers.contains_key("s-none")));
    std::thread::sleep(Duration::from_millis(50));
    assert!(!told.load(Ordering::SeqCst));
}

#[test]
fn a_reading_older_than_the_period_says_its_age() {
    let every = Duration::from_secs(120);
    let at = 1_000_000_000_000;
    assert_eq!(age(at, at + 60_000, every), "");
    assert_eq!(age(at, at + 120_000, every), "");
    assert_eq!(age(at, at + 4 * 60_000 + 5_000, every), " (4m ago)");
    assert_eq!(age(at, at + 3 * 3_600_000, every), " (3h ago)");
    assert_eq!(
        age(at + 5, at, every),
        "",
        "a clock behind the reading is no age"
    );
}

/// Every round tells the watcher, a failed request included: the kept reading comes again with
/// the time it was taken, so the row can say how old it is.
#[test]
fn a_failed_round_tells_the_kept_reading_with_its_time() {
    let (url, hits) = fake_for(KIMI, 1);
    let preset = preset_at(&url);
    let told: Arc<Mutex<Vec<u64>>> = Arc::default();
    let t = told.clone();
    let w = Watcher {
        still: Box::new(|| true),
        tell: Box::new(move |w, at| {
            assert_eq!(w[0].used, 0.44);
            t.lock().unwrap().push(at);
        }),
    };
    watch(&preset, &creds(), "s-stale", Duration::from_millis(30), w);
    until(|| told.lock().unwrap().len() >= 3);
    assert!(hits.load(Ordering::SeqCst) >= 2, "the poll kept asking");
    let at = told.lock().unwrap().clone();
    assert!(
        at.iter().all(|a| *a == at[0]),
        "one reading, told again: {at:?}"
    );
    unwatch("s-stale");
}

/// A watched door's reading is listed under its name, and a watcher may list them while it is
/// being told: the telling holds the poll's lock and the listing must not need it.
#[test]
fn a_door_reading_is_listed_and_readable_inside_a_tell() {
    let (url, _) = fake(KIMI);
    let p = Preset {
        name: "listed-door",
        ..preset_at(&url)
    };
    let seen = Arc::new(AtomicBool::new(false));
    let s = seen.clone();
    let w = Watcher {
        still: Box::new(|| false),
        tell: Box::new(move |_, _| {
            let listed = door_readings()
                .into_iter()
                .any(|(d, w, _)| d == "listed-door" && w[0].name == "5h");
            s.store(listed, Ordering::SeqCst);
        }),
    };
    watch(&p, &creds(), "s-listed", Duration::from_millis(20), w);
    until(|| seen.load(Ordering::SeqCst));
}

/// A picker's poll reads a door once, and a second ask inside the period sends nothing.
#[test]
fn a_pickers_poll_is_debounced_per_door() {
    let (url, hits) = fake(KIMI);
    let p = Preset {
        name: "picker-door",
        ..preset_at(&url)
    };
    assert!(poll_once(&p, &creds()));
    assert!(
        door_readings()
            .iter()
            .any(|(d, w, _)| d == "picker-door" && w[0].name == "5h")
    );
    assert!(!poll_once(&p, &creds()), "asked inside the period");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}
