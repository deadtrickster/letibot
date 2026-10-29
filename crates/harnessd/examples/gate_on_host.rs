//! **Run a repository's own merge gate on a declared host, by hand** — the instrument behind the
//! numbers in `docs/remote-hosts.md` § *The merge gate on a declared host*, and the way to try a
//! host before a project's `[queue] gate_host` names it.
//!
//! ```sh
//! cargo run --offline -p letibot-harnessd --example gate_on_host -- \
//!     lab2x1 ~/.local/share/letibot/landing/letibot-0123abcd
//! ```
//!
//! The steps are the ones the TARGET declares (`AGENTS.md`'s `## Merge gate`, read from the tree's
//! `main`), so this measures what the queue would measure; a trailing `--` list of commands
//! replaces them, which is how a cheap measurement of the transport is taken:
//!
//! ```sh
//! cargo run --offline -p letibot-harnessd --example gate_on_host -- \
//!     lab2x1 /tmp/a-landing-tree -- "echo hello" "cargo test -p letibot-tokencore"
//! ```
//!
//! It prints one line per step (outcome, milliseconds, the end of its output) and the verdict. It
//! writes nothing to the store, claims no lease and runs no queue: the gate, and nothing else.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use letibot_harnessd::mergequeue::gate::{self, GateControl, GateVerdict, run_gate};
use letibot_tools::exec::memory::Ask;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(host) = args.next() else {
        eprintln!("usage: gate_on_host HOST TREE [STEP…]");
        std::process::exit(2);
    };
    let Some(tree) = args.next() else {
        eprintln!("usage: gate_on_host HOST TREE [STEP…]");
        std::process::exit(2);
    };
    let tree = PathBuf::from(tree);
    let mut steps: Vec<String> = args.collect();
    // A leading `--` is the shell's separator, not a step.
    if steps.first().is_some_and(|s| s == "--") {
        steps.remove(0);
    }
    if steps.is_empty() {
        let target = gate::declared_gate(&tree, "main")
            .unwrap_or_else(|e| panic!("reading the gate from {}: {e}", tree.display()));
        match target {
            Some((steps_from_target, _install)) => steps = steps_from_target,
            None => {
                eprintln!("{} declares no `## Merge gate` on main", tree.display());
                std::process::exit(1);
            }
        }
    }
    println!("gate on {host} in {}", tree.display());
    for (i, s) in steps.iter().enumerate() {
        println!("  {}. {s}", i + 1);
    }
    let stop = AtomicBool::new(false);
    let alive = || true;
    let ctl = GateControl {
        deadline: Instant::now() + gate::GATE_DEADLINE,
        stop: &stop,
        alive: &alive,
    };
    let began = Instant::now();
    let run = run_gate(Some(&host), &tree, &steps, &ctl, Ask::Default);
    let total = began.elapsed();
    for step in &run.steps {
        println!(
            "\n[{:?} in {} ms] {}",
            step.outcome, step.elapsed_ms, step.command
        );
        if !step.output.is_empty() {
            println!("{}", step.output.trim_end());
        }
    }
    println!("\n{} — {total:.1?} in all", verdict_words(&run.verdict));
    if !matches!(run.verdict, GateVerdict::Passed) {
        std::process::exit(1);
    }
}

fn verdict_words(v: &GateVerdict) -> String {
    match v {
        GateVerdict::Passed => "PASSED".into(),
        GateVerdict::Failed(w) => format!("FAILED: {w}"),
        GateVerdict::TimedOut(w) => format!("TIMED OUT: {w}"),
        GateVerdict::Stopped(w) => format!("STOPPED: {w}"),
        GateVerdict::Infra(w) => format!("COULD NOT JUDGE: {w}"),
    }
}
