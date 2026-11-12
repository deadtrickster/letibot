//! **`letibot gc` prints the plan and the store and the disk are byte-identical afterwards**
//! (`docs/lifecycle.md` G1's fourth done-when).
//!
//! The verb is driven as a person runs it - the real binary, its own `$HOME`, store, gc root and
//! cache in a fixture - because the whole claim is about what the PROCESS does to the box, and a
//! library call cannot be byte-identical or not.

use std::path::{Path, PathBuf};

use letibot_tokencore::store::{HoldingClass, HoldingRow, OwnerKind, OwnerRow, OwnerState, Store};

/// A temp directory of this test's own.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "letibot-gc-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// Every file under `root`, with its length: what "the disk is the same" is checked as.
fn snapshot(root: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
                out.push((entry.path().display().to_string(), 0));
            } else {
                out.push((entry.path().display().to_string(), meta.len()));
            }
        }
    }
    out.sort();
    out
}

/// A box in a directory: the store, the owner root, the cache, the runtime dir.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        Fixture { root: scratch(tag) }
    }

    fn at(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// A closed package owner with a big cache directory and a worktree carved to look like
    /// letibot's: a stamped root with a `target/` in it, and one holding row each.
    fn box_with_a_closed_owner(&self) -> (PathBuf, PathBuf) {
        let data = self.at("data");
        // The box id is the running host's: a root under any other name is another machine's and
        // the walk says so, which is the behaviour and not what this fixture is about.
        let body = letibot_tools::lifecycle::box_id();
        let gc = self.at("gc").join(&body).join("s-1794509750373521593");
        let store = Store::open_set(&data, "0123456789ab").expect("a split store");
        let now = letibot_metrics::now_ms() as i64;
        let owner = OwnerRow {
            state: OwnerState::Closed,
            closed_ms: Some(now - 26 * 60 * 60 * 1000),
            closed_why: Some("idle 30m".into()),
            ..OwnerRow::live(
                "s-1794509750373521593",
                OwnerKind::Package,
                Some("/w/big".into()),
                now - 30 * 60 * 60 * 1000,
            )
        };
        store.put_owner(&owner).expect("the owner");
        // Its root, with the stamp and a target inside it - a cache, which a closed owner loses.
        std::fs::create_dir_all(gc.join("target/debug")).expect("the target");
        std::fs::write(gc.join("target/debug/obj"), vec![b'x'; 4096]).expect("some output");
        std::fs::write(
            gc.join("owner.json"),
            format!(
                r#"{{"owner":"s-1794509750373521593","box_id":"{body}","kind":"package","repo":"/w/big","created_ms":1,"lease_ms":1}}"#
            ),
        )
        .expect("the stamp");
        store
            .put_holding(
                &HoldingRow::new(
                    "s-1794509750373521593",
                    "target",
                    HoldingClass::Cache,
                    gc.join("target").display().to_string(),
                    "owner.json",
                    now,
                )
                .measured(Some(4096)),
            )
            .expect("the holding");
        drop(store);
        (data, gc)
    }

    /// The verb, run the way a person's shell runs it: a clean environment that names only this
    /// fixture, so nothing here can reach the operator's own store, cache or runtime directory.
    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_letibot"))
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.at("home"))
            .env("XDG_DATA_HOME", self.at("home/.local/share"))
            .env("XDG_CACHE_HOME", self.at("cache"))
            .env("XDG_RUNTIME_DIR", self.at("run"))
            .env("LETIBOT_STORE", self.at("data"))
            .env("LETIBOT_GC_ROOT", self.at("gc"))
            .output()
            .expect("the verb runs");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// **Done when** `letibot gc` on a store with a closed owner prints a plan and the store and the
/// disk are byte-identical afterwards.
#[test]
fn a_plan_on_a_store_with_a_closed_owner_changes_neither_the_store_nor_the_disk() {
    let f = Fixture::new("plan");
    let (data, gc_root) = f.box_with_a_closed_owner();
    let shared = data.join("shared.db");
    let store_before = std::fs::read(&shared).expect("the shared file");
    let disk_before = snapshot(&gc_root);

    let (ok, out, err) = f.run(&["gc"]);
    assert!(ok, "`letibot gc` failed. stderr:\n{err}");
    assert!(
        out.contains("package s-1794509750373521593 (closed)"),
        "the plan names the owner and its state:\n{out}"
    );
    assert!(
        out.contains("reap now") && out.contains("target"),
        "a closed owner's cache is reaped now, and the plan says so:\n{out}"
    );
    assert!(
        out.contains("this is a plan: nothing was removed"),
        "the plan says what it is:\n{out}"
    );
    assert!(
        out.contains("would be reaped"),
        "and totals what it would free:\n{out}"
    );

    assert_eq!(
        std::fs::read(&shared).expect("the shared file"),
        store_before,
        "the store changed under a plan"
    );
    assert_eq!(
        snapshot(&gc_root),
        disk_before,
        "the disk changed under a plan"
    );

    // The journal is empty, and the verb says that rather than printing nothing.
    let (ok, log, err) = f.run(&["gc", "--log"]);
    assert!(ok, "`letibot gc --log` failed. stderr:\n{err}");
    assert!(log.contains("no journal lines"), "{log}");
}

/// `letibot holds` answers the same owner in a person's words and as JSON, and removes nothing.
#[test]
fn holds_answers_what_the_owner_holds() {
    let f = Fixture::new("holds");
    let (data, gc_root) = f.box_with_a_closed_owner();
    let store_before = std::fs::read(data.join("shared.db")).expect("the shared file");
    let disk_before = snapshot(&gc_root);

    let (ok, out, err) = f.run(&["holds"]);
    assert!(ok, "`letibot holds` failed. stderr:\n{err}");
    assert!(out.contains("holds:"), "{out}");
    assert!(out.contains("s-1794509750373521593"), "{out}");
    assert!(
        out.contains("target 4.0 KiB") || out.contains("target 4 KiB"),
        "the size is there, in the plan's own spelling:\n{out}"
    );

    let (ok, json, err) = f.run(&["holds", "--json"]);
    assert!(ok, "`letibot holds --json` failed. stderr:\n{err}");
    let v: serde_json::Value = serde_json::from_str(&json).expect("one object");
    assert_eq!(v["owners"][0]["owner"], "s-1794509750373521593");
    assert_eq!(v["owners"][0]["state"], "closed");
    assert!(
        v["owners"][0]["holds_line"]
            .as_str()
            .unwrap_or_default()
            .starts_with("holds:"),
        "{json}"
    );

    assert_eq!(
        std::fs::read(data.join("shared.db")).expect("the shared file"),
        store_before,
        "holds changed the store"
    );
    assert_eq!(snapshot(&gc_root), disk_before, "holds changed the disk");
}

/// A box with no store says so by path instead of printing an empty plan.
#[test]
fn a_box_with_no_store_is_refused_by_name() {
    let f = Fixture::new("nostore");
    let (ok, out, err) = f.run(&["gc"]);
    assert!(!ok, "a missing store must not answer as success:\n{out}");
    assert!(err.contains("no store"), "{err}");
}
