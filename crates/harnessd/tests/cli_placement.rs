//! **R100: `--host NAME` and `--where firecode` are two asks, and the pair is refused by name.**
//!
//! MEASURED before the fix, through this same door: both flags wrote ONE field
//! (`cfg.placement`), so the flag that came LAST won and the other was dropped without a
//! word —
//!
//! * `--host lab2x1 --where firecode` booted a VM **on this box**, on a copy of this box's
//!   workspace, while the operator had said the work is on `lab2x1`;
//! * `--where firecode --host lab2x1` ran the session's tools on `lab2x1` with no VM at all.
//!
//! Neither is a refusal and neither says anything: the session starts and looks healthy in
//! both cases. What the pair *should* mean is a VM on `lab2x1`, whose copy source is the
//! remote checkout — which this build cannot make (see `cli::placement_from_flags`), so the
//! answer is a sentence, not a silent pick.
//!
//! # One test, and its own process
//!
//! `$HOME`, `$LETIBOT_HOSTS` and the working directory are process-wide, and `cli::run` is
//! the whole daemon's entry point. This file is a test binary of its own with one test in it,
//! which is what makes the environment safe to set: `$HOME` is a fixture's, so nothing here
//! reads the operator's own `hosts.toml`, providers or store — the refusal happens while the
//! command line is being read, before any of them would be opened.

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

/// `cli::run` with one command line, answering its error (a refusal is the whole subject).
///
/// **Every line here ends with `--list-sessions`, and that is the test's clock rather than a
/// part of the subject.** Without it a line that is NOT refused goes on to start a daemon —
/// and the pre-fix behaviour was worse than a wrong answer: `--host lab2x1 --where firecode`
/// booted a VM here, so the test did not fail, it hung (MEASURED: killed at 120 s against the
/// pre-fix tree). A query answers and exits, so the same mistake is reported in milliseconds
/// as *started something*, which is what a refusal must never do.
fn refused(args: &[&str]) -> String {
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
fn a_vm_on_a_remote_host_is_refused_by_name_whichever_flag_came_first() {
    let home = fixture();
    // SAFETY (Rust 2024's `unsafe` on `set_var`): this binary holds one test and no thread
    // that reads the environment has started. `$HOME` is the fixture's so the operator's own
    // `hosts.toml` and providers are not what this measures.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::remove_var("LETIBOT_HOSTS");
        std::env::set_current_dir(&home).expect("a cwd that is not the repository");
    }

    for order in [
        ["--host", "lab2x1", "--where", "firecode"],
        ["--where", "firecode", "--host", "lab2x1"],
    ] {
        let said = refused(&order);
        assert!(
            said.contains("`--host lab2x1`") && said.contains("`--vm`"),
            "the refusal names both asks, in the order they can be typed: {said}"
        );
        assert!(
            said.contains("Nothing was started"),
            "a refusal that started nothing says so: {said}"
        );
        assert!(
            said.contains("not built") && said.contains("firecode") && said.contains("/dev/kvm"),
            "and it says what the pair SHOULD mean and why it is not there: {said}"
        );
    }

    // **Each flag alone is still exactly what it was.** Which placement each one resolves to is
    // `cli`'s own unit test (`placement_from_flags`); what this binary adds is that the refusal
    // is about the PAIR and reaches the operator through the real entry point, in both orders.
    let _ = std::fs::remove_dir_all(&home);
}
