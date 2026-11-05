//! `letibot notes` - where this project's notes are, and the one step that sends the notes
//! checkout to its remote. Writes commit locally; nothing pushes but `letibot notes push`.

use std::path::PathBuf;
use std::process::ExitCode;

use letibot_harnessd::config::Config;
use letibot_harnessd::leticode_config::{self, LeticodeConfig, Level};

const USAGE: &str = "\
letibot notes path [--dir PATH]   the notes checkout and this project's directory in it
letibot notes push [--dir PATH]   fetch, rebase onto origin/main and push the notes checkout
  Config: `[notes] repo` and `remote` in ~/.config/letibot/leticode.toml (the user's file wins
  over the project's). Default checkout: ~/.local/share/letibot/notes.";

pub fn run(args: &[String]) -> ExitCode {
    let mut verb: Option<&str> = None;
    let mut dir: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--dir" => match it.next() {
                Some(d) => dir = Some(PathBuf::from(d)),
                None => return usage_error("--dir needs a path"),
            },
            v @ ("path" | "push") if verb.is_none() => verb = Some(v),
            other => return usage_error(&format!("no such argument: {other}")),
        }
    }
    let Some(verb) = verb else {
        return usage_error("which verb: path or push");
    };
    let dir = dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let cfg = match config(dir) {
        Ok(cfg) => cfg,
        Err(why) => {
            eprintln!("letibot notes: {why}");
            return ExitCode::from(1);
        }
    };
    let Some(repo) = cfg.notes_repo() else {
        eprintln!("letibot notes: no notes checkout: neither $XDG_DATA_HOME nor $HOME is set");
        return ExitCode::from(1);
    };
    match verb {
        "path" => {
            println!("repo     {}", repo.display());
            println!("project  {}", cfg.project_notes().display());
            if let Some(remote) = cfg.notes_remote() {
                println!("remote   {remote}");
            }
            ExitCode::SUCCESS
        }
        _ => match letibot_tools::notes_repo::push(&repo, cfg.notes_remote().as_deref()) {
            Ok(said) => {
                println!("{said}");
                ExitCode::SUCCESS
            }
            Err(why) => {
                eprintln!("letibot notes push: {why}");
                ExitCode::from(1)
            }
        },
    }
}

/// The two `leticode.toml` levels and the daemon's own notes default, as the daemon reads them.
fn config(dir: PathBuf) -> Result<Config, String> {
    let mut cfg = Config::for_this_box(dir);
    cfg.read_the_box();
    cfg.read_notes_home();
    let user = LeticodeConfig::load_as(&leticode_config::user_path(), Level::User)?;
    let project = match LeticodeConfig::discover(&cfg.workspace) {
        Some(path) => LeticodeConfig::load(&path)?,
        None => LeticodeConfig::default(),
    };
    cfg.leticode = user.overridden_by(&project);
    Ok(cfg)
}

fn usage_error(why: &str) -> ExitCode {
    eprintln!("letibot notes: {why}\n{USAGE}");
    ExitCode::from(2)
}
