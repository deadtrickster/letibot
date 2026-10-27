//! **`gate_corpus`: replay a gate-decision corpus through today's parser and gate.**
//!
//! The instrument `letibot_tools::corpus_replay` describes; this is its command line.
//! It reads corpus files named on the command line — nothing in the tree points at
//! where they live, because they hold the operator's real commands.
//!
//! ```sh
//! cargo run --offline -p letibot-tools --example gate_corpus -- \
//!     --home /home/someone --workspace /home/someone/project \
//!     ~/letibot-corpus/box.json
//! ```
//!
//! `--home` and `--workspace` are the sweep's assumption about where the commands ran:
//! the export records the command and not the directory, and the region half of the
//! reading needs one. Every file named is swept and then a total is printed, so the
//! same invocation gives both the per-host table and the overall one.

use std::path::PathBuf;
use std::process::ExitCode;

use letibot_tools::corpus_replay::{Report, sweep};
use letibot_tools::intent::Surroundings;

const USAGE: &str = "\
gate_corpus — replay gate decisions through today's parser and gate

  gate_corpus [--home DIR] [--workspace DIR] FILE...

  --home DIR       the home the commands ran with (default: $HOME)
  --workspace DIR  the workspace the commands ran in (default: <home>/project; the
                   export does not record it, so the report names what was assumed)
";

fn main() -> ExitCode {
    let mut home = std::env::var("HOME").unwrap_or_default();
    let mut workspace: Option<String> = None;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--home" => home = args.next().unwrap_or_default(),
            "--workspace" => workspace = args.next(),
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => files.push(PathBuf::from(other)),
        }
    }
    if files.is_empty() {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }
    let workspace = workspace.unwrap_or_else(|| format!("{home}/project"));
    // The daemon's gate declares the shell (`Surroundings::with_pinned_shell`); left
    // undeclared, every bare program name is shadowable and 88% of a corpus reads as
    // unresolved for a reason that has nothing to do with the parser.
    let env = Surroundings {
        home: Some(home.clone()),
        workspace: Some(workspace.clone()),
        ..Surroundings::default()
    }
    .with_pinned_shell("the corpus: the harness that recorded it ran a fixed shell");

    println!(
        "gate corpus replay — home {home}, workspace {workspace} (assumed: the export \
         does not record either)\n"
    );
    let mut total = Report::default();
    let mut failed = false;
    for file in &files {
        let label = file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| file.display().to_string());
        match sweep(file, &env) {
            Ok(r) => {
                print!("{}", r.render(&label));
                total.merge(r);
            }
            Err(e) => {
                eprintln!("{e}");
                failed = true;
            }
        }
    }
    if files.len() > 1 {
        print!("{}", total.render("TOTAL"));
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
