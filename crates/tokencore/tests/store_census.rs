//! **A store whose migration fails was never a store this process counted** — the census
//! `Store::live()` answers must come back to exactly where it was, because `Drop` gives the count
//! back on every path, including the one that returns `Err` out of `Store::open`.
//!
//! # Why this is a test binary of its own
//!
//! `Store::live()` is the PROCESS's count, and `cargo test --lib` runs a crate's tests in parallel
//! threads: a sibling opening a store inside the window between the two reads below would be
//! counted as this test's arithmetic, and the numbers could only be bounded, not pinned. One test
//! in one binary is a quiet process, so the arithmetic here is exact.
//!
//! # What the two failures are, kept apart
//!
//! * a store that was never counted must leave the census where it found it — before the fix this
//!   read `usize::MAX`, because `Drop` subtracted from a counter the failing open never added to;
//! * and a store that DOES open must still be counted, so "always zero" is not a way to pass the
//!   first assertion.

use std::path::{Path, PathBuf};

use letibot_tokencore::rusqlite::{Connection, params};
use letibot_tokencore::store::{SCHEMA_VERSION, Store};

/// A directory of this test's own, with its `-wal` and `-shm` companions, removed when it drops.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "letibot-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("a temp dir for the fixture");
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **A store of this build's own, walked forward to one this build may not read.**
///
/// The refusal is `migrate`'s own — a floor above this build — and not a hand-written error: the
/// file is written by `Store::open` first, so what is refused is a real store, and what fails is
/// the migration `from_connection` runs before it hands the store back.
fn a_store_this_build_may_not_read(dir: &Path) -> PathBuf {
    let path = dir.join("refused.db");
    drop(Store::open(&path).expect("the fixture store is written by this build"));
    let c = Connection::open(&path).expect("the fixture store opens to its own writer");
    c.execute(
        "UPDATE schema_version SET version = ?1",
        params![SCHEMA_VERSION + 2],
    )
    .expect("the fixture stamps a version ahead of this build");
    c.execute(
        "UPDATE schema_reader_floor SET min_reader = ?1 WHERE only_row = 1",
        params![SCHEMA_VERSION + 1],
    )
    .expect("the fixture records a floor above this build");
    path
}

#[test]
fn a_store_whose_migration_fails_is_not_counted_and_does_not_underflow_the_census() {
    let dir = TempDir::new("tokencore-store-census");
    let refused = a_store_this_build_may_not_read(&dir.0);

    // The fixture's own store was opened and dropped, so this process holds none — the number
    // every read below is measured against.
    assert_eq!(Store::live(), 0, "the fixture's store was given back");

    let err = Store::open(&refused)
        .err()
        .expect("a store whose floor is above this build is refused");
    assert!(
        err.to_string().contains(&format!(
            "the store needs a reader of at least {}",
            SCHEMA_VERSION + 1
        )),
        "the open failed for the fixture's reason and not some other: {err}"
    );

    // **The number, and not "not panicking"**: a store that was never counted must leave the
    // census where it found it. This read `18446744073709551615` before the fix.
    assert_eq!(
        Store::live(),
        0,
        "a store whose migration failed was counted by its drop and not by its open, so the \
         census is now off by one — and it has wrapped to `usize::MAX`, which makes every later \
         measurement in this process nonsense"
    );

    // And a store that DOES open is still counted, so the assertion above cannot be satisfied by
    // a counter that never counts anything.
    let good = Store::open(&dir.0.join("good.db")).expect("a fresh store opens");
    assert_eq!(Store::live(), 1, "one open store");
    drop(good);
    assert_eq!(Store::live(), 0, "and it is given back");
}
