use super::*;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("letibot-vmboot-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn at(load: f64, cores: usize) -> LoadSample {
    LoadSample {
        load1: load,
        load5: load,
        cores,
    }
}

fn queue(dir: &Path, slots: usize, budget_ms: u64) -> BootQueue {
    BootQueue {
        dir: dir.to_path_buf(),
        slots,
        budget: Duration::from_millis(budget_ms),
        poll: Duration::from_millis(10),
    }
}

#[test]
fn loadavg_is_read_as_the_larger_of_one_and_five_minutes() {
    let l = LoadSample::parse("15.85 51.45 88.28 3/1185 4242\n", 32).unwrap();
    assert_eq!(l.load(), 51.45);
    assert_eq!(l.said(), "load 51 on 32 cores");
    assert!(LoadSample::parse("", 32).is_none());
    assert_eq!(LoadSample::parse("1 2 3", 0).unwrap().cores, 1);
}

/// Measured on lab2x1: preparation grows past 4 per core (a boot at load 202 spent 598 s before
/// the VMM started), so the deadline is the base under the knee and proportional past it.
#[test]
fn the_up_deadline_is_the_base_until_four_per_core_then_grows_with_load() {
    assert_eq!(up_deadline(None), BASE_UP);
    assert_eq!(up_deadline(Some(&at(20.0, 32))), BASE_UP);
    assert_eq!(up_deadline(Some(&at(128.0, 32))), BASE_UP);
    let busy = up_deadline(Some(&at(202.0, 32)));
    assert!(
        busy > Duration::from_secs(900) && busy < Duration::from_secs(1000),
        "{busy:?}"
    );
    assert_eq!(up_deadline(Some(&at(5000.0, 32))), MAX_UP);
    assert_eq!(up_deadline(Some(&at(f64::NAN, 32))), BASE_UP);
}

#[test]
fn a_free_slot_admits_at_once_and_says_nothing() {
    let dir = tmp("free");
    let q = queue(&dir, 2, 1000);
    let a = q.admit("s-a", &|| Some(at(10.0, 32))).expect("a free slot");
    assert!(a.waited < Duration::from_secs(1));
    assert_eq!(a.up, BASE_UP);
    assert_eq!(a.note(), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The fake boot: two holders take both slots, a third boot waits, and is admitted the moment
/// one of them ends - a queue, not a failure.
#[test]
fn a_boot_with_every_slot_taken_waits_and_starts_when_one_frees() {
    let dir = tmp("queue");
    let q = queue(&dir, 2, 10_000);
    let busy = || Some(at(240.0, 32));
    let first = q.admit("s-first", &busy).unwrap();
    let _second = q.admit("s-second", &busy).unwrap();
    assert_eq!(q.holders().len(), 2, "{:?}", q.holders());
    let freer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1200));
        drop(first);
    });
    let third = q.admit("s-third", &busy).expect("queued, then admitted");
    freer.join().unwrap();
    assert!(third.waited >= Duration::from_secs(1), "{:?}", third.waited);
    let note = third.note().expect("a queued boot says so");
    assert!(note.contains("waited 1s for one of 2 boot slots"), "{note}");
    assert!(note.contains("load 240 on 32 cores"), "{note}");
    assert!(note.contains("`firecode up` was given 1125 s"), "{note}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_boot_that_never_gets_a_slot_is_refused_naming_the_load_and_the_holders() {
    let dir = tmp("refused");
    let q = queue(&dir, 1, 150);
    let _held = q.admit("s-holder", &|| None).unwrap();
    let e = q
        .admit("s-late", &|| Some(at(210.0, 32)))
        .expect_err("the budget ran out");
    assert!(e.contains("1 of 1 boots"), "{e}");
    assert!(e.contains("s-holder for"), "{e}");
    assert!(e.contains("load 210 on 32 cores"), "{e}");
    assert!(e.contains("without the VM boundary"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A slot is a file lock, so a holder that ends - here, drops - gives it back with nothing to
/// clean up, and a stale record in the file does not keep it taken.
#[test]
fn a_released_slot_is_free_whatever_its_file_says() {
    let dir = tmp("released");
    let q = queue(&dir, 1, 100);
    drop(q.admit("s-gone", &|| None).unwrap());
    assert!(q.holders().is_empty());
    assert!(q.admit("s-next", &|| None).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unqueued_boot_still_gets_the_load_scaled_deadline() {
    let a = Admitted::unqueued(Some(at(320.0, 32)));
    assert_eq!(a.up, Duration::from_secs(1500));
    assert!(a.probe > crate::firecode::GUEST_TIMEOUT);
    let note = a.note().unwrap();
    assert!(
        note.starts_with("the VM boot started at load 320"),
        "{note}"
    );
}
