//! The look, without a network and without an hour: the switch, the age rule, and the line each
//! answer leaves in the daemon's log.

use super::*;
use std::path::PathBuf;

/// A directory of this test's own, so parallel tests do not share a cache file.
fn dir(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("letibot-catfresh-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("a scratch directory");
    d
}

/// A cache file whose age the test chooses.
fn aged(path: &Path, now: SystemTime, hours: u64) {
    std::fs::write(path, "{}").unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(now - Duration::from_secs(hours * 3600)))
        .unwrap();
}

/// The switch is the whole answer: `[catalogue] refresh = false` and the operator's pointer both
/// mean this box does not fetch, and then nothing reaches the network at all.
#[test]
fn the_switch_off_never_fetches() {
    let d = dir("off");
    let path = d.join("models.json");
    let fetched = std::cell::Cell::new(false);
    let look = look(Some(&path), false, SystemTime::now(), || {
        fetched.set(true);
        Ok(refresh::Outcome::Unchanged)
    });
    assert_eq!(look, Look::Off);
    assert!(!fetched.get(), "a switch that is off still fetched");
    assert_eq!(
        look.said(),
        None,
        "a switch that is off says a line every hour"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// The age rule: an hour-old cache is left alone, a day-old one is fetched, and a missing one is
/// fetched whatever the clock says.
#[test]
fn a_fresh_cache_is_left_and_a_day_old_one_is_fetched() {
    let d = dir("age");
    let path = d.join("models.json");
    let now = SystemTime::now();
    let fetched = std::cell::Cell::new(0);
    let fetch = || {
        fetched.set(fetched.get() + 1);
        Ok(refresh::Outcome::Written {
            providers: 7,
            bytes: 100,
        })
    };

    aged(&path, now, 1);
    assert_eq!(look(Some(&path), true, now, fetch), Look::Fresh);
    assert_eq!(fetched.get(), 0, "an hour-old cache was fetched again");

    aged(&path, now, 25);
    assert_eq!(
        look(Some(&path), true, now, fetch),
        Look::Written {
            providers: 7,
            bytes: 100
        }
    );
    assert_eq!(fetched.get(), 1);

    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        look(Some(&path), true, now, fetch),
        Look::Written { .. }
    ));
    assert_eq!(fetched.get(), 2, "a missing cache was not fetched");
    let _ = std::fs::remove_dir_all(&d);
}

/// A failure is one sentence in the log and nothing else: no card, no error, and the cache that is
/// there is what every reader goes on using.
#[test]
fn a_failure_is_one_line_and_no_card() {
    let d = dir("failed");
    let path = d.join("models.json");
    aged(&path, SystemTime::now(), 48);
    let look = look(Some(&path), true, SystemTime::now(), || {
        Err("GET https://models.dev/api.json: http timeout".into())
    });
    assert_eq!(
        look.failed(),
        Some("GET https://models.dev/api.json: http timeout")
    );
    let said = look.said().expect("a failure is worth a line");
    assert!(said.starts_with("could not refresh"), "{said}");
    let _ = std::fs::remove_dir_all(&d);
}

/// A box with no cache directory has nothing to write to, and says so once rather than failing an
/// hour later in the same words.
#[test]
fn no_cache_directory_is_a_line_and_not_a_fetch() {
    let look = look(None, true, SystemTime::now(), || {
        panic!("there is no file to fetch into")
    });
    assert_eq!(look, Look::NoHome);
    assert!(look.said().is_some());
}

/// The switch reads the operator's file at both levels, and the pointer in the environment wins
/// over a file that says yes.
#[test]
fn wanted_reads_both_levels_and_the_pointer() {
    let mut env = letibot_testenv::EnvGuard::lock();
    let d = dir("wanted");
    let config = d.join("config");
    std::fs::create_dir_all(&config).unwrap();
    let workspace = d.join("work");
    std::fs::create_dir_all(&workspace).unwrap();
    env.set("XDG_CONFIG_HOME", &config);
    env.remove("LETIBOT_MODELS_JSON");

    assert!(
        wanted(&workspace),
        "a box whose files say nothing does not fetch"
    );

    // The project's file, and the user's, are one answer each: whichever level says off, it is off.
    std::fs::write(
        workspace.join("leticode.toml"),
        "[catalogue]\nrefresh = false\n",
    )
    .unwrap();
    assert!(
        !wanted(&workspace),
        "the project's `refresh = false` was ignored"
    );
    std::fs::remove_file(workspace.join("leticode.toml")).unwrap();
    let user = config.join("letibot");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(user.join("leticode.toml"), "[catalogue]\nrefresh = false\n").unwrap();
    assert!(
        !wanted(&workspace),
        "the user file's `refresh = false` was ignored"
    );

    // And the operator's own pointer beats a file that says yes.
    std::fs::write(user.join("leticode.toml"), "[catalogue]\nrefresh = true\n").unwrap();
    assert!(wanted(&workspace));
    env.set("LETIBOT_MODELS_JSON", "/tmp/models-of-theirs.json");
    assert!(
        !wanted(&workspace),
        "a catalogue the operator pointed at was one this box would have replaced"
    );
    drop(env);
    let _ = std::fs::remove_dir_all(&d);
}

/// The row is counted against the UTC day, so a daemon that fails every hour files one row and not
/// twenty-four.
#[test]
fn the_day_a_warning_is_counted_against_is_utc() {
    let midnight = UNIX_EPOCH + Duration::from_secs(1_700_000_000); // a fixed instant
    let day = day_of(midnight);
    assert_eq!(day_of(midnight + Duration::from_secs(3600)), day);
    assert_eq!(day_of(midnight + Duration::from_secs(86_400)), day + 1);
    assert_eq!(day_of(UNIX_EPOCH), 0);
}
