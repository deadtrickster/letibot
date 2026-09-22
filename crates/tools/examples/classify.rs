//! **What layer A thinks of a command line** — one command per argument.
//!
//! `cargo run --example classify -p letibot-tools -- 'head -c 200 src/app.rs'`
//!
//! The gate's whole design turns on this reading: the intents, the regions, the tier, and
//! (since R21) whether the line is a **read** — which is what decides whether a shell call
//! is judged on its work or on the vehicle that carried it. Asking a live daemon is the
//! other way to find out, and it costs a real call, a corpus row, and a gate.
//!
//! **A pinned shell**, because that is what a session that can exec is judged under
//! (`harnessd`'s `surroundings_for`): a bare name resolves through the PATH fixed at seat
//! time. `$HOME` and `$WS` override the two paths it places against, so a reader can ask
//! *what would this be under another workspace* without touching the box.
use std::io::Read;

fn main() {
    let env = letibot_tools::intent::Surroundings {
        home: Some(std::env::var("HOME").unwrap_or_else(|_| "/home/dead".into())),
        workspace: Some(
            std::env::var("WS").unwrap_or_else(|_| "/home/dead/Projects/letibot".into()),
        ),
        // `None`, not a guess: the scratch is the daemon's own directory and reading this
        // box's `$XDG_RUNTIME_DIR` to invent one would place paths the daemon would not.
        scratch: None,
        shell: letibot_tools::intent::ShellTrust::Pinned {
            how: "this example assumes the seat a session with a shell gets".into(),
        },
        seen_hosts: Default::default(),
    };

    // Arguments, or NUL-separated commands on stdin for a bulk pass. The second form is
    // what a corpus sweep wants: `sqlite3 … | cargo run --example classify`.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut lines = args;
    if lines.is_empty() {
        let mut raw = Vec::new();
        std::io::stdin().read_to_end(&mut raw).unwrap();
        lines = raw
            .split(|b| *b == 0)
            .filter_map(|c| std::str::from_utf8(c).ok())
            .filter(|c| !c.trim().is_empty())
            .map(str::to_string)
            .collect();
    }

    for cmd in lines {
        let b = letibot_tools::intent::Baseline::of_command(&cmd, &env);
        println!(
            "reads={:5} tier={:12} verdict={:8} intents={:?} regions={:?}",
            b.reads_only(),
            b.tier.as_str(),
            b.verdict.as_str(),
            b.intents.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
            b.regions.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
        );
        let one = cmd.replace('\n', " ⏎ ");
        println!("  {}\n", &one[..one.len().min(120)]);
    }
}
