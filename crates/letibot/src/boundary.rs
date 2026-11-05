//! `letibot boundary` - what a command gets in a top-level session and in a subagent, from the
//! `[boundary]` tables of the user's and this folder's `leticode.toml`. Reads files, starts no
//! daemon, and prints no secret value.

use std::path::PathBuf;
use std::process::ExitCode;

use letibot_harnessd::perimeter;
use letibot_tools::exec::seat::Level;
use letibot_tools::mode::Mode;

const USAGE: &str = "\
letibot boundary [--mode NAME] [--dir PATH]
  What a command gets in a top-level session and in a subagent: paths (ro/rw and why), network,
  env pairs (secrets redacted) and hidden paths. Config: `[boundary]` in
  ~/.config/letibot/leticode.toml and the project's leticode.toml.

  --mode NAME             the mode that decides the network rule (default: writes allowed)
  --dir PATH              the project folder (default: the current one)";

pub fn run(args: &[String]) -> ExitCode {
    let mut mode = Mode::WRITES_ALLOWED;
    let mut dir: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--mode" => match it.next().map(|m| Mode::parse(m)) {
                Some(Ok(m)) => mode = m,
                Some(Err(e)) => {
                    eprintln!("letibot boundary: {e}");
                    return ExitCode::from(2);
                }
                None => {
                    eprintln!("letibot boundary: --mode needs a name");
                    return ExitCode::from(2);
                }
            },
            "--dir" => match it.next() {
                Some(d) => dir = Some(PathBuf::from(d)),
                None => {
                    eprintln!("letibot boundary: --dir needs a path");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("letibot boundary: no such argument: {other}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let dir = dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let mut refused = false;
    for level in [Level::Wide, Level::Tight] {
        let lines = perimeter::lines(level, &mode, &dir);
        refused |= lines
            .first()
            .is_some_and(|l| l.starts_with("boundary config refused"));
        println!("{}\n", lines.join("\n"));
    }
    if refused {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
