//! The refresh, without a network and without a clock: a fetch that answers what the test says,
//! and a `now` the test chooses, so "stale" and "fresh" are facts rather than waits.

use super::*;

/// A directory of this test's own. A name keyed on the pid had two parallel tests deleting each
/// other's fixture, so the counter is what makes them different.
fn dir(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("letibot-refresh-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// A models.dev-shaped body: every provider a preset names, one model each, and one provider no
/// preset names - which is the one the trim must drop.
fn body() -> String {
    let mut all = serde_json::Map::new();
    for id in catalogue_ids() {
        all.insert(
            id.to_string(),
            serde_json::json!({
                "id": id,
                "models": {
                    "some-model": {"limit": {"context": 1000, "output": 100},
                                   "cost": {"input": 1.0, "output": 2.0}}
                }
            }),
        );
    }
    all.insert(
        "not-a-preset".into(),
        serde_json::json!({"id": "not-a-preset", "models": {"x": {"limit": {"context": 1}}}}),
    );
    serde_json::to_string(&all).expect("a body")
}

fn ok(etag: Option<&str>) -> Answer {
    Answer {
        status: 200,
        etag: etag.map(str::to_string),
        last_modified: None,
        body: body().into_bytes(),
    }
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("a fixture file");
}

/// The refresh writes where the readers read, and the next lookup sees the new facts: a running
/// daemon's swap is a rename, which is why no client keeps the parse it was built with.
#[test]
fn a_refreshed_file_is_what_the_next_lookup_reads() {
    let d = dir("swap");
    let path = d.join("models.json");
    write(
        &path,
        r#"{"deepseek": {"id": "deepseek", "models": {"a-retired-model": {"limit": {"context": 1}}}}}"#,
    );
    let before = crate::catalogue::cached_read(&path).expect("it parses");
    assert!(before.model("deepseek", "a-retired-model").is_some());

    refresh_at(&path, &|_| Ok(ok(None)), SystemTime::now()).expect("it writes");

    let after = crate::catalogue::cached_read(&path).expect("it parses");
    assert!(
        after.model("deepseek", "a-retired-model").is_none(),
        "the lookup answered from the parse the refresh replaced"
    );
    assert!(after.model("deepseek", "some-model").is_some());
    let _ = std::fs::remove_dir_all(&d);
}

/// The catalogue the presets name, and nothing else: a provider no door can reach is bytes this
/// box would carry and never read.
#[test]
fn the_trim_keeps_the_providers_the_presets_name() {
    let d = dir("trim");
    let path = d.join("models.json");
    let now = SystemTime::now();
    let outcome = refresh_at(&path, &|_| Ok(ok(Some("\"one\""))), now).expect("it writes");
    let Outcome::Written { providers, bytes } = outcome else {
        panic!("a 200 is a write: {outcome:?}");
    };
    assert_eq!(providers, catalogue_ids().len());
    assert!(bytes > 0);

    let kept: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut names: Vec<&String> = kept.as_object().unwrap().keys().collect();
    names.sort();
    let mut want = catalogue_ids();
    want.sort_unstable();
    assert_eq!(names, want.iter().collect::<Vec<_>>());
    assert!(!kept.get("not-a-preset").is_some(), "a stranger was kept");
    // And what was written is a catalogue every reader can parse, not only a JSON object.
    let cat = crate::catalogue::Catalogue::read(&path).expect("it parses");
    assert!(cat.model(catalogue_ids()[0], "some-model").is_some());
    let _ = std::fs::remove_dir_all(&d);
}

/// A body that is not a catalogue is refused before anything is written: the file the box reads
/// is the one that worked.
#[test]
fn a_body_that_is_not_a_catalogue_leaves_the_cache_exactly_as_was() {
    let d = dir("garbled");
    let path = d.join("models.json");
    let old = br#"{"deepseek": {"id": "deepseek", "models": {}}}"#;
    std::fs::write(&path, old).unwrap();
    let err = refresh_at(
        &path,
        &|_| {
            Ok(Answer {
                status: 200,
                etag: None,
                last_modified: None,
                body: b"<html>we are down for maintenance</html>".to_vec(),
            })
        },
        SystemTime::now(),
    )
    .expect_err("a body that is not a catalogue is an error");
    assert!(err.contains("did not answer a catalogue"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), old);
    let _ = std::fs::remove_dir_all(&d);
}

/// A 500, a timeout and a socket that never answered are one outcome here: the old file, byte for
/// byte, and a sentence for the log.
#[test]
fn an_http_error_leaves_the_cache_exactly_as_was() {
    let d = dir("httperror");
    let path = d.join("models.json");
    let old = br#"{"deepseek": {"id": "deepseek", "models": {}}}"#;
    std::fs::write(&path, old).unwrap();
    let err = refresh_at(
        &path,
        &|_| {
            Ok(Answer {
                status: 500,
                ..Answer::default()
            })
        },
        SystemTime::now(),
    )
    .expect_err("a 500 is an error");
    assert!(err.contains("HTTP 500"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), old);
    let _ = std::fs::remove_dir_all(&d);
}

/// The same for the transport: what ureq answers when it gives up is a sentence, and the file is
/// not rewritten to say so.
#[test]
fn a_timeout_leaves_the_cache_exactly_as_was() {
    let d = dir("timeout");
    let path = d.join("models.json");
    let old = br#"{"deepseek": {"id": "deepseek", "models": {}}}"#;
    std::fs::write(&path, old).unwrap();
    let err = refresh_at(
        &path,
        &|_| Err(format!("GET {URL}: http timeout: timed out")),
        SystemTime::now(),
    )
    .expect_err("a timeout is an error");
    assert!(err.contains("timeout"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), old);
    let _ = std::fs::remove_dir_all(&d);
}

/// A provider the catalogue stopped carrying is an error naming it, not a cache with one door
/// fewer: the door would then have no window and no price and nothing would say why.
#[test]
fn a_provider_the_catalogue_lost_is_an_error_and_not_a_shorter_cache() {
    let d = dir("missing");
    let path = d.join("models.json");
    let old = br#"{"deepseek": {"id": "deepseek", "models": {}}}"#;
    std::fs::write(&path, old).unwrap();
    let gone = catalogue_ids()[0];
    let mut all: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&body()).unwrap();
    all.remove(gone);
    let short = serde_json::to_string(&all).unwrap();
    let err = refresh_at(
        &path,
        &|_| {
            Ok(Answer {
                status: 200,
                etag: None,
                last_modified: None,
                body: short.as_bytes().to_vec(),
            })
        },
        SystemTime::now(),
    )
    .expect_err("a missing provider is an error");
    assert!(err.contains(gone), "the error does not name {gone}: {err}");
    assert_eq!(std::fs::read(&path).unwrap(), old);
    let _ = std::fs::remove_dir_all(&d);
}

/// The write is a rename: the file a reader opens is the whole old one or the whole new one, and
/// nothing is left beside it under a temporary name.
#[test]
fn the_write_leaves_the_body_and_its_validators_and_no_temporary_file() {
    let d = dir("atomic");
    let path = d.join("models.json");
    refresh_at(&path, &|_| Ok(ok(Some("\"tag\""))), SystemTime::now()).expect("it writes");
    let mut names: Vec<String> = std::fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["models.json", "models.json.etag"]);
    assert!(std::fs::read_to_string(&path).unwrap().contains("deepseek"));
    let _ = std::fs::remove_dir_all(&d);
}

/// The age rule: a file written an hour ago is not worth a request, one written yesterday is.
#[test]
fn a_fresh_cache_is_not_fetched_and_a_stale_one_is() {
    let d = dir("age");
    let path = d.join("models.json");
    let now = SystemTime::now();
    write(&path, "{}");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(now - Duration::from_secs(3600)))
        .unwrap();

    let fetched = std::cell::Cell::new(0);
    let count = |_: &Conditional| {
        fetched.set(fetched.get() + 1);
        Ok(ok(None))
    };
    assert_eq!(refresh_if_stale_with(&path, now, &count).unwrap(), None);
    assert_eq!(fetched.get(), 0, "an hour-old cache was fetched again");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{}",
        "it was rewritten anyway"
    );

    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(now - Duration::from_secs(25 * 3600)))
        .unwrap();
    assert!(matches!(
        refresh_if_stale_with(&path, now, &count).unwrap(),
        Some(Outcome::Written { .. })
    ));
    assert_eq!(fetched.get(), 1);
    let _ = std::fs::remove_dir_all(&d);
}

/// A 304 is the server's word that the body here is current: not one byte changes, and the
/// file's date moves so the next look is a day away.
#[test]
fn a_304_moves_the_modification_time_and_not_one_byte() {
    let d = dir("notmodified");
    let path = d.join("models.json");
    let body = body();
    write(&path, &body);
    write(
        &sidecar(&path),
        r#"{"etag":"\"abc\"","last_modified":null}"#,
    );
    let old = SystemTime::now() - Duration::from_secs(25 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old))
        .unwrap();

    let seen = std::cell::RefCell::new(Conditional::default());
    let now = SystemTime::now();
    let outcome = refresh_at(
        &path,
        &|condition| {
            *seen.borrow_mut() = condition.clone();
            Ok(Answer {
                status: 304,
                etag: Some("\"abc\"".into()),
                last_modified: None,
                body: Vec::new(),
            })
        },
        now,
    )
    .expect("a 304 is not a failure");
    assert_eq!(outcome, Outcome::Unchanged);
    assert_eq!(seen.borrow().etag.as_deref(), Some("\"abc\""));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        body,
        "a byte moved"
    );
    let after = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert!(
        after > old + Duration::from_secs(3600),
        "the date did not move: {after:?}"
    );
    assert_eq!(
        std::fs::read_to_string(sidecar(&path)).unwrap(),
        r#"{"etag":"\"abc\"","last_modified":null}"#,
        "a 304 rewrote its own sidecar"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A 200 replaces the body and the validators together: an etag from the old body would vouch for
/// content the server no longer has.
#[test]
fn a_200_replaces_the_body_and_its_validators() {
    let d = dir("replaced");
    let path = d.join("models.json");
    write(&path, r#"{"deepseek": {"id": "deepseek", "models": {}}}"#);
    write(&sidecar(&path), r#"{"etag":"\"old\""}"#);
    let outcome = refresh_at(
        &path,
        &|condition| {
            assert_eq!(
                condition.etag.as_deref(),
                Some("\"old\""),
                "the old etag was not sent"
            );
            Ok(Answer {
                status: 200,
                etag: Some("\"new\"".into()),
                last_modified: Some("Wed, 21 Oct 2026 07:28:00 GMT".into()),
                body: body().into_bytes(),
            })
        },
        SystemTime::now(),
    )
    .expect("it writes");
    assert!(matches!(outcome, Outcome::Written { .. }));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("some-model")
    );
    let kept: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sidecar(&path)).unwrap()).unwrap();
    assert_eq!(kept["etag"], "\"new\"");
    assert_eq!(kept["last_modified"], "Wed, 21 Oct 2026 07:28:00 GMT");
    let _ = std::fs::remove_dir_all(&d);
}

/// A sidecar with no cache behind it describes nothing: the GET must carry no validator, or a 304
/// would vouch for a file that is not there.
#[test]
fn a_sidecar_with_no_cache_behind_it_is_a_plain_get() {
    let d = dir("sidecaronly");
    let path = d.join("models.json");
    write(&sidecar(&path), r#"{"etag":"\"abc\""}"#);
    let seen = std::cell::RefCell::new(Conditional::default());
    refresh_at(
        &path,
        &|condition| {
            *seen.borrow_mut() = condition.clone();
            Ok(ok(None))
        },
        SystemTime::now(),
    )
    .expect("it writes");
    assert_eq!(*seen.borrow(), Conditional::default());
    let _ = std::fs::remove_dir_all(&d);
}

/// A sidecar nobody can read is a full GET: the alternative is trusting bytes that might name
/// another body.
#[test]
fn a_garbled_sidecar_is_a_plain_get() {
    let d = dir("garbledsidecar");
    let path = d.join("models.json");
    write(&path, "{}");
    write(&sidecar(&path), "not json at all");
    let seen = std::cell::RefCell::new(Conditional::default());
    refresh_at(
        &path,
        &|condition| {
            *seen.borrow_mut() = condition.clone();
            Ok(ok(Some("\"fresh\"")))
        },
        SystemTime::now(),
    )
    .expect("it writes");
    assert_eq!(*seen.borrow(), Conditional::default());
    let _ = std::fs::remove_dir_all(&d);
}

/// The operator's pointer wins over the daemon's own fetching: a file they chose is not one this
/// box may overwrite, and `[catalogue] refresh = false` is the switch for the same thing.
#[test]
fn the_pointer_and_the_switch_both_stop_the_refresh() {
    assert!(wanted_with(true, None));
    assert!(!wanted_with(false, None), "`refresh = false` still fetched");
    assert!(
        !wanted_with(true, Some("/tmp/mine.json")),
        "the pointer was overwritten"
    );
    assert!(
        wanted_with(true, Some("")),
        "an empty variable is not a pointer"
    );
}

/// One id per CATALOGUE, not per door: four presets share two ids, and the trim asks for each
/// provider once.
#[test]
fn the_catalogue_ids_are_one_per_provider() {
    let ids = catalogue_ids();
    assert!(ids.len() >= 5, "{ids:?}");
    for (i, id) in ids.iter().enumerate() {
        assert!(!ids[..i].contains(id), "{id} is asked for twice");
    }
    // Every preset's door is one of them, and every one of them is a preset's.
    for p in crate::presets::ALL {
        assert!(ids.contains(&p.catalogue_id), "{} was left out", p.name);
    }
    assert_eq!(ids.len(), {
        let mut all: Vec<&str> = crate::presets::ALL.iter().map(|p| p.catalogue_id).collect();
        all.sort_unstable();
        all.dedup();
        all.len()
    });
}

/// The snapshot script keeps a `WANT` list, and it must stay the presets' own: a preset whose id
/// the script does not know makes the vendored floor miss a door the daemon offers.
#[test]
fn the_snapshot_scripts_list_is_the_presets_own() {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/refresh-models-snapshot.sh");
    let text = std::fs::read_to_string(&script)
        .unwrap_or_else(|e| panic!("reading {}: {e}", script.display()));
    let start = text.find("WANT = [").expect("the script's WANT list");
    let rest = &text[start..];
    let end = rest.find(']').expect("the end of the WANT list");
    let mut listed: Vec<&str> = rest[..end].split('"').skip(1).step_by(2).collect();
    listed.sort_unstable();
    let mut wanted = catalogue_ids();
    wanted.sort_unstable();
    assert_eq!(
        listed,
        wanted,
        "{} and the presets disagree",
        script.display()
    );
}
