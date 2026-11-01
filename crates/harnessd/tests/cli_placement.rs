//! **R100: `--host NAME` and `--vm` are two asks, and the pair is one placement — a VM there.**
//!
//! `--vm` is the LAUNCHER's word for `--where firecode` (`scripts/letibot` rewrites it), so the two
//! orders that reach this door are `--host lab2x1 --where firecode` and its reverse. Both are
//! tried here, through `cli::run`, because argument order was the whole of the original defect.
//!
//! MEASURED before the fix, through this same door: both flags wrote ONE field (`cfg.placement`),
//! so the flag that came LAST won and the other was dropped without a word —
//!
//! * `--host lab2x1 --where firecode` booted a VM **on this box**, on a copy of this box's
//!   workspace, while the operator had said the work is on `lab2x1`;
//! * `--where firecode --host lab2x1` ran the session's tools on `lab2x1` with no VM at all.
//!
//! Neither is a refusal and neither says anything: the session starts and looks healthy in both
//! cases. The fix recorded the two asks separately and resolved them once the whole line was read;
//! the pair was then refused while the combined placement did not exist. It exists now, so what
//! this binary pins is that **neither order is refused, and neither drops the other ask** — the
//! placement each order resolves to is `cli::placement_from_flags`'s own unit test.
//!
//! # One test, and its own process
//!
//! `$HOME`, `$LETIBOT_HOSTS` and the working directory are process-wide, and `cli::run` is the
//! whole daemon's entry point. This file is a test binary of its own with one test in it, which is
//! what makes the environment safe to set: `$HOME` is a fixture's, so nothing here reads the
//! operator's own `hosts.toml`, providers or store.
//!
//! **Every line ends with `--list-sessions`, and that is the clock rather than the subject.** A
//! query answers and exits, so a line that is NOT refused comes back in milliseconds instead of
//! starting a daemon (and, before the fix, booting a VM). What the answer proves is therefore the
//! *absence* of a placement refusal: the error it does carry is the store the query needs.

use std::path::PathBuf;

/// A fixture home, a `hosts.toml` naming `lab2x1`, and nothing else.
fn fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "letibot-cli-placement-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let config = dir.join(".config").join("letibot");
    std::fs::create_dir_all(&config).expect("the config directory");
    std::fs::write(
        config.join("hosts.toml"),
        "[host.lab2x1]\nhostname = \"lab2x1.invalid\"\nuser = \"tester\"\n\
         root = \"~/letibot-remote\"\nmise = false\n",
    )
    .expect("hosts.toml");
    dir
}

/// `cli::run` with one command line, answering its error.
fn said(args: &[&str]) -> String {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string())
        .chain(std::iter::once("--list-sessions".to_string()))
        .collect();
    match letibot_harnessd::cli::run(&args) {
        Ok(code) => panic!("`{}` started something (exit {code})", args.join(" ")),
        Err(why) => why,
    }
}

#[test]
fn a_vm_and_a_host_are_one_placement_whichever_flag_came_first() {
    let home = fixture();
    // SAFETY (Rust 2024's `unsafe` on `set_var`): this binary holds one test and no thread
    // that reads the environment has started. `$HOME` is the fixture's so the operator's own
    // `hosts.toml` and providers are not what this measures.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::remove_var("LETIBOT_HOSTS");
        std::env::set_current_dir(&home).expect("a cwd that is not the repository");
    }

    let lines: [&[&str]; 2] = [
        &["--host", "lab2x1", "--where", "firecode"],
        &["--where", "firecode", "--host", "lab2x1"],
    ];
    for order in lines {
        let answer = said(order);
        // **The store is what stops this line**, which is the query's own prerequisite: it means
        // the command line was read whole, the pair resolved, and nothing was refused for it.
        assert!(
            answer.contains("no --store"),
            "`{}` must reach the query rather than be refused for the pair: {answer}",
            order.join(" ")
        );
        assert!(
            !answer.contains("two placements"),
            "the pair is a placement now, not a refusal: {answer}"
        );
    }

    // **A host nobody declared is still refused**, whatever the pair says: the name is checked
    // the same way whichever flag spelled it.
    let refused = said(&["--host", "lab2", "--where", "firecode"]);
    assert!(refused.contains("there is no host `lab2`"), "{refused}");

    let _ = std::fs::remove_dir_all(&home);
}
