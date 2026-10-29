//! **The pool of machines a reviewer may go to** — what it counts, whom it picks, and what it does
//! when a machine says no.
//!
//! Every test here installs its own snapshot (`ReviewHosts::set_spare`) rather than probing a
//! machine: the question is what the pool does with an answer, and a test that had to fake a host
//! to ask it would be measuring the fake.

use std::path::PathBuf;

use super::*;
use crate::mergequeue::capacity;

/// **A `hosts.toml` of this test's own** — `$LETIBOT_HOSTS`, the test door `letibot_tools` already
/// keeps, restored when the guard drops so two tests in one binary cannot see each other's file.
///
/// The lock is held for the guard's whole life because the environment is process-wide and
/// `letibot_tools::remote::hosts_file` reads it: two of these alive at once would be two pools.
pub(crate) struct HostsFile {
    path: PathBuf,
    was: Option<std::ffi::OsString>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

static HOSTS_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl HostsFile {
    pub(crate) fn with(names: &[&str]) -> HostsFile {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let guard = HOSTS_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "letibot-review-hosts-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hosts.toml");
        let mut text = String::new();
        for n in names {
            text.push_str(&format!(
                "[host.{n}]\nhostname = \"{n}\"\nuser = \"dead\"\nroot = \"~/letibot-remote\"\n\n"
            ));
        }
        std::fs::write(&path, text).unwrap();
        let was = std::env::var_os("LETIBOT_HOSTS");
        // SAFETY: the mutex above is held for the guard's whole life, so no other test in this
        // binary is reading the environment while it says which file the hosts are in.
        unsafe { std::env::set_var("LETIBOT_HOSTS", &path) };
        HostsFile {
            path,
            was,
            _guard: guard,
        }
    }
}

impl Drop for HostsFile {
    fn drop(&mut self) {
        // SAFETY: as in `with` — this guard is the only holder of the environment lock.
        unsafe {
            match &self.was {
                Some(v) => std::env::set_var("LETIBOT_HOSTS", v),
                None => std::env::remove_var("LETIBOT_HOSTS"),
            }
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn pool(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

/// **The machine with the most spare takes the reviewer, and this machine is always in the pool.**
#[test]
fn the_pool_offers_this_machine_and_every_declared_one_and_picks_the_most_spare() {
    let _file = HostsFile::with(&["lab2x1", "lubuntu2"]);
    let hosts = ReviewHosts::new();
    hosts.set_spare(None, Some(8.0), None);
    hosts.set_spare(Some("lab2x1"), Some(31.9), None);
    hosts.set_spare(Some("lubuntu2"), Some(12.0), None);

    let machines = hosts.machines(None);
    let names: Vec<String> = machines.iter().map(|m| m.label()).collect();
    assert_eq!(names, vec!["local", "lab2x1", "lubuntu2"]);

    // **Four on the 32-core box, three on the twelve-core one, two here.** `spare / 4`, floored,
    // and never more than `PER_HOST_MAX`.
    assert_eq!(hosts.capacity(None), 4 + 3 + 2);
    let chosen = hosts.choose(None).expect("a machine with room");
    assert_eq!(
        chosen.name.as_deref(),
        Some("lab2x1"),
        "the most spare wins, and it is not this machine"
    );

    // **And the ordering is SPARE, not *is it a host*** — this machine with more spare than every
    // host is the one a reviewer goes to. The first cut ranked `(is_remote, spare)`, so any host beat
    // this machine whatever its load, which is a rule nobody asked for.
    let hosts = ReviewHosts::new();
    hosts.set_spare(None, Some(40.0), None);
    hosts.set_spare(Some("lab2x1"), Some(12.0), None);
    assert_eq!(
        hosts.choose(None).map(|m| m.label()),
        Some("local".to_string()),
        "the box with the most spare is chosen even when it is this one"
    );
    // **A tie is this machine's**, because a reviewer here costs no transport at all.
    let hosts = ReviewHosts::new();
    hosts.set_spare(None, Some(16.0), None);
    hosts.set_spare(Some("lab2x1"), Some(16.0), None);
    assert_eq!(
        hosts.choose(None).map(|m| m.label()),
        Some("local".to_string()),
        "equal spare goes to this machine"
    );
}

/// **The per-host cap refuses a second reviewer past it** — the point of the cap: one reviewer's
/// build cannot eat a box another reviewer needs.
#[test]
fn a_machine_takes_at_most_its_room_and_then_the_next_goes_somewhere_else() {
    let _file = HostsFile::with(&["lab2x1", "lubuntu2"]);
    let hosts = ReviewHosts::new();
    // **A machine with 64 spare cores still takes four** — `PER_HOST_MAX`, not the arithmetic.
    hosts.set_spare(None, Some(0.0), None);
    hosts.set_spare(Some("lab2x1"), Some(64.0), None);
    hosts.set_spare(Some("lubuntu2"), Some(4.0), None);

    for n in 0..capacity::PER_HOST_MAX {
        let m = hosts.choose(None).expect("room for another");
        assert_eq!(m.name.as_deref(), Some("lab2x1"), "reviewer {n}");
        hosts.started(&format!("child-{n}"), &m);
    }
    let next = hosts.choose(None).expect("lubuntu2 has room for one");
    assert_eq!(
        next.name.as_deref(),
        Some("lubuntu2"),
        "the fifth reviewer does not go to the box that already has four"
    );
    hosts.started("child-fifth", &next);
    assert!(
        hosts.choose(None).is_none(),
        "four on one box, one on the other, and this machine has no spare: the pool is full"
    );
    assert_eq!(hosts.capacity(None), 0, "and it claims nothing more");

    // **A reviewer that is over frees the room it held** — the desk's list, which is what the pass
    // hands to `keep`. This is the line the first cut got wrong: the place was freed at each place a
    // reviewer ENDED and the normal ending (a verdict) was missed, so a pool ran down to nothing.
    // Two places come back, one on each machine: `child-0` held one of `lab2x1`'s four and
    // `child-fifth` held `lubuntu2`'s only one.
    let live: Vec<String> = (1..capacity::PER_HOST_MAX)
        .map(|n| format!("child-{n}"))
        .collect();
    hosts.keep(&live);
    assert_eq!(
        hosts.capacity(None),
        2,
        "`child-0` and `child-fifth` are over, so both their places are back"
    );
    assert!(
        hosts.where_is("child-0").is_none(),
        "and nothing remembers where it was"
    );
    assert!(
        hosts.where_is("child-1").is_some(),
        "while the reviewers still on the desk are still counted"
    );
    // **And the machine that was full takes one again** — the queue's claim limit is this number.
    assert_eq!(
        hosts.choose(None).and_then(|m| m.name).as_deref(),
        Some("lab2x1"),
        "the box with the most spare after the freed place"
    );
}

/// **`reserve` is subtracted, and it is what makes a big box small** — the arithmetic the operator
/// asked for: `cores − load − reserve`.
#[test]
fn reserve_comes_off_the_spare_before_the_cap_is_counted() {
    let host = letibot_tools::remote::Host {
        name: "lab2x1".into(),
        hostname: "lab2x1".into(),
        user: None,
        key: None,
        path: Vec::new(),
        root: "~/letibot-remote".into(),
        mise: None,
        // The operator's own etcd fuzz runs on twenty-four of its thirty-two cores.
        reserve: 24,
        flags: Vec::new(),
        never_touch: Vec::new(),
    };
    let spare = host
        .spare_cores(32, "0.5")
        .expect("cores and load both said");
    assert_eq!(spare, 7.5);
    assert_eq!(
        capacity::reviewers_fit(spare),
        1,
        "seven and a half spare cores is one reviewer, not seven"
    );
    // And the same box unreserved takes four — the cap, not the cores.
    let spare = host
        .spare_cores(32, "0.5")
        .expect("cores and load both said")
        + 24.0;
    assert_eq!(capacity::reviewers_fit(spare), capacity::PER_HOST_MAX);
    // **A box with no spare takes nobody**, which is a wait and not a refusal for ever.
    assert_eq!(capacity::reviewers_fit(0.0), 0);
    assert_eq!(capacity::reviewers_fit(-3.0), 0, "an oversubscribed box");
}

/// **A host that refused a placement is skipped for the rest of the pass, and tried again next
/// pass** — fourteen rows must not each pay the same timeout, and a reboot must not park the queue
/// on this machine for ever.
#[test]
fn a_host_that_said_no_is_skipped_this_pass_and_asked_again_next_one() {
    let _file = HostsFile::with(&["lab2x1", "lubuntu2"]);
    let hosts = ReviewHosts::new();
    hosts.set_spare(None, Some(0.0), None);
    hosts.set_spare(Some("lab2x1"), Some(31.9), None);
    hosts.set_spare(Some("lubuntu2"), Some(31.9), None);

    assert_eq!(
        hosts.choose(None).and_then(|m| m.name).as_deref(),
        Some("lab2x1")
    );
    hosts.mark_down(
        Some("lab2x1"),
        "placing this session on lab2x1: ssh exited 255".into(),
    );
    assert!(hosts.down(Some("lab2x1")).is_some());
    assert_eq!(
        hosts.choose(None).and_then(|m| m.name).as_deref(),
        Some("lubuntu2"),
        "the next reviewer goes to the machine that is still answering"
    );
    // The capacity the claim limit reads does not count a machine that is down: claiming a row for
    // it would leave the row `reviewing` with nobody on it.
    assert_eq!(hosts.capacity(None), capacity::PER_HOST_MAX);

    hosts.end_pass();
    assert!(hosts.down(Some("lab2x1")).is_none());
    assert_eq!(hosts.capacity(None), capacity::PER_HOST_MAX * 2);
}

/// **A pool of this machine alone** — what a fixture gets, and what `["local"]` means: no probe, no
/// ssh, and a reviewer that runs here.
#[test]
fn a_local_pool_has_one_machine_and_never_asks_a_host() {
    let _file = HostsFile::with(&["lab2x1"]);
    let hosts = ReviewHosts::new();
    hosts.set_spare(None, Some(16.0), None);
    let only = pool(&["local"]);
    assert_eq!(
        hosts.machines(Some(&only)).len(),
        1,
        "this machine and nothing else"
    );
    assert_eq!(hosts.capacity(Some(&only)), capacity::PER_HOST_MAX);
    assert_eq!(
        hosts.choose(Some(&only)).map(|m| m.name),
        Some(None),
        "`local` is this machine"
    );
    assert!(hosts.unknown(Some(&only)).is_empty());
}

/// **A name nobody declared is reported, and does not empty the pool** — a project that names a
/// machine that has been removed runs here and is told, rather than stopping.
#[test]
fn a_host_nobody_declared_is_reported_and_the_pool_keeps_this_machine() {
    let _file = HostsFile::with(&["lab2x1"]);
    let hosts = ReviewHosts::new();
    hosts.set_spare(None, Some(16.0), None);
    let asked = pool(&["lab2x1", "lab2"]);
    assert_eq!(hosts.unknown(Some(&asked)), vec!["lab2".to_string()]);
    let machines = hosts.machines(Some(&asked));
    assert_eq!(
        machines.iter().map(|m| m.label()).collect::<Vec<_>>(),
        vec!["local".to_string(), "lab2x1".to_string()],
        "the typo is not a machine, and it is not a reason to stop"
    );
}

/// **Before the first snapshot the pool is this machine** — the queue starts here and fans out as
/// soon as the probe lands, which is what keeps it from stalling on a network.
#[test]
fn before_any_probe_the_pool_is_this_machine_and_the_capacity_is_its_own() {
    let _file = HostsFile::with(&["lab2x1", "lubuntu2"]);
    let hosts = ReviewHosts::new();
    // No `set_spare`: nothing has been asked.
    let machines = hosts.machines(None);
    assert_eq!(machines.len(), 3, "this machine and both declared hosts");
    assert_eq!(machines[1].spare, None, "a host nobody has asked yet");
    assert_eq!(
        machines[1].room(0),
        0,
        "and an unasked host is not counted in the claim limit"
    );
    assert!(
        hosts.choose(None).is_some_and(|m| m.name.is_none()),
        "so the first reviewer runs here"
    );
}
