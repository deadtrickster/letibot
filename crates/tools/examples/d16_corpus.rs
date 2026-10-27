//! **What D16 and R98(1) would change, over a corpus** — the two numbers the rulings were filed
//! on, per host.
//!
//! ```sh
//! cargo run --offline -p letibot-tools --example d16_corpus -- \
//!     --workspace /home/dead/Projects/letibot ~/letibot-corpus/mac.json
//! ```
//!
//! The corpus is read-only and lives outside the tree (`~/letibot-corpus/*.json`, exported from
//! each host's store); nothing here writes anything, asks a model, or runs a command.
//!
//! # What each number means, and the one assumption
//!
//! * **D16** — the cards a *person* answered at `writes-allowed`, and how many of them the
//!   boundary rule would have settled with nobody asked. A corpus row does not record the
//!   confinement the call ran inside, so the column asks the rule with the boundary's own half
//!   assumed: *if this session had been inside a measured boundary*. The rule itself is
//!   `adjudicate::a_command_that_crosses_nothing` — the gate's own function, not a restatement —
//!   so this cannot answer after the gate has stopped.
//! * **R98** — the settles whose deciding mechanism was a standing answer (an approved shape, a
//!   session grant, a `permission` rule) over a command that runs the project's own code, split
//!   by runner. With `[permissions] ask_unconfined_code` OFF — the default — R98 does nothing,
//!   and this is what turning it on would cost: each of these is a call that would go back to
//!   being a card on a host with no boundary.
//!
//! Numbers only. Not one command is printed: the corpus holds real sessions.
use std::path::Path;

use letibot_tools::corpus_replay::{Unasked, unasked};
use letibot_tools::intent::{ShellTrust, Surroundings};

fn main() -> std::process::ExitCode {
    let mut workspace =
        std::env::var("WS").unwrap_or_else(|_| "/home/dead/Projects/letibot".into());
    let mut home = std::env::var("HOME").unwrap_or_else(|_| "/home/dead".into());
    let mut paths: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--workspace" => match args.next() {
                Some(v) => workspace = v,
                None => return fail("--workspace wants a path"),
            },
            "--home" => match args.next() {
                Some(v) => home = v,
                None => return fail("--home wants a path"),
            },
            other => paths.push(other.to_string()),
        }
    }
    if paths.is_empty() {
        return fail("give me one or more corpus files (they live outside this tree)");
    }
    let env = Surroundings {
        home: Some(home),
        workspace: Some(workspace),
        // `None`, not a guess: the scratch is the daemon's own directory, and reading this
        // box's to invent one would place paths the daemon would not.
        scratch: None,
        // The seat a session with a shell gets. `Unknown` would make every bare command name
        // unresolved, which is a different measurement.
        shell: ShellTrust::Pinned {
            how: "the seat a session with a shell gets".into(),
        },
        seen_hosts: Default::default(),
    };

    let mut total = Unasked::default();
    for path in &paths {
        let label = Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(path);
        match unasked(Path::new(path), &env) {
            Ok(u) => {
                print!("{}", u.render(label));
                total.merge(u);
            }
            Err(e) => return fail(&e),
        }
    }
    if paths.len() > 1 {
        print!("\n{}", total.render("all hosts"));
    }
    std::process::ExitCode::SUCCESS
}

fn fail(why: &str) -> std::process::ExitCode {
    eprintln!("d16_corpus: {why}");
    std::process::ExitCode::FAILURE
}
