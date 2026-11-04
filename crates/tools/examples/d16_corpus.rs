//! What a boundary rule and a standing-answer rule would change, over a corpus.
//!
//! ```sh
//! cargo run --offline -p letibot-tools --example d16_corpus -- \
//!     --workspace /home/dead/Projects/letibot ~/letibot-corpus/mac.json
//! ```
//!
//! Read-only; numbers only, as the corpus holds real sessions and no command is printed. A row lacks
//! the confinement it ran in, so the boundary half is assumed.
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
        ..Default::default()
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
