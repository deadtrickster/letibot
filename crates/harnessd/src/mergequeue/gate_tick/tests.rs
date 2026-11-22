//! The tick, as both ends spell it: the launcher text a step is started with, and a real shell
//! running that text while this test plays the daemon - poking it, then not.

use super::probe::{gone_within, in_group};
use super::*;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("letibot-gate-tick-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The step's own far-side files under one directory, on a tick short enough to watch a real step:
/// a tenth of a second, so the window is 400 ms and not the production 20 s.
fn files(dir: &Path, tick: Tick) -> crate::mergequeue::gate_host::Files {
    let run = dir.display().to_string();
    crate::mergequeue::gate_host::Files {
        launcher: format!("{run}/step-0.sh"),
        pid: format!("{run}/step-0.pid"),
        out: format!("{run}/step-0.out"),
        exit: format!("{run}/step-0.exit"),
        beat: format!("{run}/step-0.beat"),
        scratch: format!("{run}/env"),
        tick,
    }
}

/// A step that lives a long time, started exactly as the far side starts one: its launcher script,
/// in a process group of its own, detached from this test's streams. The pid in the pid file is the
/// launcher's, and the launcher is the group's leader, so `pgid` is both.
fn started(dir: &Path, tick: Tick) -> (Child, u32) {
    let files = files(dir, tick);
    let step = "echo started; sleep 300";
    let launcher = crate::mergequeue::gate_host::launcher_script(
        step,
        &dir.display().to_string(),
        &dir.join("target").display().to_string(),
        &files,
        false,
    );
    std::fs::write(&files.launcher, launcher).unwrap();
    std::fs::create_dir_all(dir.join("target")).unwrap();
    let child = Command::new("bash")
        .arg(&files.launcher)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("a launcher");
    let pgid = child.id();
    assert!(
        crate::testing::wait_until(crate::testing::PATIENCE, || std::fs::read_to_string(
            &files.pid
        )
        .is_ok_and(|p| p.trim() == pgid.to_string())),
        "the launcher never wrote its pid"
    );
    (child, pgid)
}

fn poke_as_the_poll_does(files: &crate::mergequeue::gate_host::Files) {
    let out = Command::new("sh")
        .args(["-c", &poke_script(&files.beat)])
        .output()
        .expect("the far side's poke");
    assert!(out.status.success(), "poke: {out:?}");
}

/// The operator's numbers, and the arithmetic that makes them a rule rather than three constants: a
/// poll may be slow, but a slow poll plus the tick that follows it has to fit inside the window -
/// or a daemon that is merely waiting on ssh would be read as a dead one.
#[test]
fn the_window_is_four_ticks_and_a_slow_poll_fits_inside_it() {
    let tick = Tick::default();
    assert_eq!(tick.every, Duration::from_secs(5));
    assert_eq!(tick.miss, MISSES);
    assert_eq!(tick.window(), Duration::from_secs(20));
    let bound = crate::mergequeue::gate_host::POLL_BOUND;
    assert!(
        bound + tick.every < tick.window(),
        "a poll of {bound:?} plus a tick does not fit the {:?} window",
        tick.window()
    );
}

/// The launcher a host gate starts a step with carries the watchdog, and the poll that fetches a
/// step's output is the poke - one direction, so the far side never has to reach this box.
#[test]
fn the_launcher_carries_the_watchdog_and_the_poll_carries_the_poke() {
    let dir = tmp("text");
    let files = files(&dir, Tick::default());
    let launcher = crate::mergequeue::gate_host::launcher_script(
        "cargo test",
        "/h/tree",
        "/h/target",
        &files,
        false,
    );
    // Primed before the watchdog reads it, so a daemon that dies between the spawn and its first
    // poke is still a daemon whose step ends.
    assert!(
        launcher.contains(&format!("printf '0\\n' > '{}'", files.beat)),
        "{launcher}"
    );
    assert!(launcher.contains(&files.beat), "{launcher}");
    for want in [
        "trap '' TERM",
        "seen=$(cat",
        "missed=$((missed + 1))",
        "-ge 4",
        "sleep 5",
        "kill -TERM 0",
        "kill -KILL 0",
    ] {
        assert!(launcher.contains(want), "{want:?} is not in:\n{launcher}");
    }
    // The watchdog is started BESIDE the step and before it, and the step itself is untouched.
    let wd = launcher.find("trap '' TERM").unwrap();
    let run = launcher.find("( sh '/h/tree/scripts/gate-env.sh'").unwrap();
    assert!(wd < run, "the watchdog starts first:\n{launcher}");
    // The local gate's launcher is the same watchdog in front of the step's own command.
    let local = watched("cargo test", &dir.join("beat"), &Tick::default());
    assert!(local.contains("trap '' TERM"), "{local}");
    assert!(local.ends_with("cargo test"), "{local}");
    // And a poll pokes: it bumps the counter the watchdog compares, and swaps it in by rename so a
    // watchdog reading it mid-poke sees the old number rather than half of a new one.
    let poll = crate::mergequeue::gate_host::poll_script("@@tag@@", &files, 0);
    for want in ["b=$((b + 1))", "mv -f", &files.beat] {
        assert!(poll.contains(want), "{want:?} is not in:\n{poll}");
    }
    assert!(
        poll.find("b=$((b + 1))") < poll.find("@@tag@@"),
        "the poke is the first thing a poll does:\n{poll}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A poked step lives: five windows of poking every half tick, and the step is still there - so
/// what ends a step is the ABSENCE of pokes and not the watchdog's own clock.
#[test]
fn a_poked_step_lives() {
    let tick = Tick::new(Duration::from_millis(100), 4);
    let dir = tmp("poked");
    let files = files(&dir, tick);
    let (mut child, pgid) = started(&dir, tick);
    let until = Instant::now() + tick.window() * 5;
    while Instant::now() < until {
        poke_as_the_poll_does(&files);
        std::thread::sleep(tick.every / 2);
        assert!(
            !in_group(pgid).is_empty(),
            "a step poked every half tick was ended"
        );
    }
    assert!(child.try_wait().unwrap().is_none(), "the step ended early");
    // Then the pokes stop, and it ends by itself - the same group, the same watchdog.
    assert!(
        gone_within(pgid, tick.window() + Duration::from_secs(10)),
        "left in the group: {:?}",
        in_group(pgid)
    );
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A step whose daemon stops poking ends within the window: the leader, the step, and the watchdog
/// with them - nothing is left for a next daemon to find.
#[test]
fn a_step_whose_daemon_stops_poking_ends_within_the_window() {
    let tick = Tick::new(Duration::from_millis(100), 4);
    let dir = tmp("stops");
    let files = files(&dir, tick);
    let (mut child, pgid) = started(&dir, tick);
    // Poked for two ticks and then not: the step must still be there, because a window is four.
    for _ in 0..2 {
        poke_as_the_poll_does(&files);
        std::thread::sleep(tick.every);
    }
    assert!(
        !in_group(pgid).is_empty(),
        "ended before its window was out: {:?}",
        in_group(pgid)
    );
    let stopped = Instant::now();
    assert!(
        gone_within(pgid, tick.window() + Duration::from_secs(10)),
        "the step outlived its window: {:?}",
        in_group(pgid)
    );
    assert!(
        stopped.elapsed() >= tick.every,
        "killed before the window could be counted: {:?}",
        stopped.elapsed()
    );
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A step that ends by itself takes its watchdog with it: the launcher's leader is gone, and a
/// watchdog that outlived it would keep a group id alive - and could end somebody else's step once
/// the kernel handed that pid out again.
#[test]
fn a_step_that_ends_by_itself_leaves_no_watchdog_behind() {
    let tick = Tick::new(Duration::from_millis(50), 4);
    let dir = tmp("ends");
    let files = files(&dir, tick);
    let launcher = crate::mergequeue::gate_host::launcher_script(
        "echo done",
        &dir.display().to_string(),
        &dir.join("target").display().to_string(),
        &files,
        false,
    );
    assert!(launcher.contains("trap '' TERM"), "{launcher}");
    std::fs::write(&files.launcher, launcher).unwrap();
    std::fs::create_dir_all(dir.join("target")).unwrap();
    let mut child = Command::new("bash")
        .arg(&files.launcher)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("a launcher");
    let pgid = child.id();
    assert!(child.wait().unwrap().success(), "the step did not run");
    // No poke ever came, and the step ended anyway: the watchdog went with its leader rather than
    // staying to count a window against a group nobody owns any more.
    assert!(
        gone_within(pgid, Duration::from_secs(10)),
        "a watchdog outlived its step: {:?}",
        in_group(pgid)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The beat beside a step's file, and the counter a poke leaves there: one more than what was
/// there, whatever that was - a file that cannot be read is a first poke, not an error.
#[test]
fn a_poke_counts_up_from_whatever_is_there() {
    let dir = tmp("beat");
    let beat = beat_beside(&dir.join("step-0.log"));
    assert_eq!(
        beat,
        dir.join("step-0.beat"),
        "beside the file it belongs to"
    );
    assert_eq!(std::fs::read_to_string(&beat).unwrap_or_default(), "");
    poke(&beat).unwrap();
    assert_eq!(std::fs::read_to_string(&beat).unwrap(), "1\n");
    poke(&beat).unwrap();
    std::fs::write(&beat, "41\n").unwrap();
    poke(&beat).unwrap();
    assert_eq!(std::fs::read_to_string(&beat).unwrap(), "42\n");
    std::fs::write(&beat, "not a number\n").unwrap();
    poke(&beat).unwrap();
    assert_eq!(std::fs::read_to_string(&beat).unwrap(), "1\n");
    // A path with a quote in it survives the shell: the watchdog's own quoting, not string splicing.
    let odd = dir.join("it's a name");
    std::fs::create_dir_all(&odd).unwrap();
    let beat = beat_beside(&odd.join("step-1.log"));
    std::fs::write(&beat, "0\n").unwrap();
    let run = Command::new("sh")
        .args(["-c", &poke_script(&beat.display().to_string())])
        .output()
        .expect("the far side's poke");
    assert!(run.status.success(), "{run:?}");
    assert_eq!(std::fs::read_to_string(&beat).unwrap(), "1\n");
    let _ = std::fs::remove_dir_all(&dir);
}
