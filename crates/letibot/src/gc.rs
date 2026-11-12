//! `letibot holds` and `letibot gc` - what every owner holds, and what would be reaped
//! (`docs/lifecycle.md` d.6 step 4 and d.9).
//!
//! Both are read-only and run without a daemon: the owner index is in the store's shared file and
//! the proof of every holding is on the resource itself, so a person whose daemons are all dead can
//! still see the disk they are holding. `gc` prints the plan and changes nothing - the hands are
//! later packages (G2 compute, G3 work, G4 caches), and this verb is the answer they are judged by.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use letibot_tokencore::store::Store;
use letibot_tokencore::store::split;
use letibot_tools::lifecycle::{self, Places, Plan};

const USAGE_HOLDS: &str = "\
letibot holds [OWNER] [--folder DIR] [--json] [--line]
  What the owners of this box hold on disk, one line each and one line per holding: the cgroup a
  session's jobs run in (with its process count and memory, which no `ps` can give a seat in a
  sandbox), a VM, its copies, its daemon, its worktree and target. `OWNER` narrows it to one
  session id.

  --folder DIR   only the owners working against DIR
  --json         the same facts as one object, for a program
  --line         only the `holds:` lines, which is what `--status` and a prompt row want

  Read-only: it opens the store read-only and removes nothing.
  example:
    letibot holds
    letibot holds s-1794509750373521593 --json";

const USAGE_GC: &str = "\
letibot gc [OWNER] [--folder DIR] [--json] [--log]
  The plan: for every owner, what would be reaped, why, and when or never. A directory that looks
  like letibot's and names no owner the store knows is listed under `unrecorded` and is never
  reaped by a pattern. **Nothing in this build removes, stops or writes anything**: compute is G2,
  work (archive, then remove) is G3, caches and records are G4.

  --folder DIR   only the owners working against DIR
  --json         the plan as one object
  --log          the journal of what was done to these owners (`reap_event`), oldest first

  example:
    letibot gc
    letibot gc s-1794509750373521593";

pub fn run_holds(args: &[String]) -> ExitCode {
    match holds(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("letibot holds: {why}");
            ExitCode::FAILURE
        }
    }
}

pub fn run_gc(args: &[String]) -> ExitCode {
    match gc(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("letibot gc: {why}");
            ExitCode::FAILURE
        }
    }
}

/// What both verbs take, parsed once.
struct Args {
    owner: Option<String>,
    folder: Option<PathBuf>,
    json: bool,
    /// `holds --line`: only the `holds:` sentences.
    line: bool,
    /// `gc --log`: the journal instead of the plan.
    log: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        owner: None,
        folder: None,
        json: false,
        line: false,
        log: false,
    };
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--folder" => {
                let dir = it.next().ok_or("`--folder` needs a directory after it")?;
                out.folder = Some(PathBuf::from(dir));
            }
            "--json" => out.json = true,
            "--line" => out.line = true,
            "--log" => out.log = true,
            other if other.starts_with('-') => return Err(format!("no such flag: {other}")),
            _ if out.owner.is_some() => {
                return Err(format!(
                    "one owner at a time, and it already has `{}`",
                    out.owner.clone().unwrap_or_default()
                ));
            }
            other => out.owner = Some(other.to_string()),
        }
    }
    Ok(out)
}

fn holds(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE_HOLDS}");
        return Ok(());
    }
    let parsed = parse(args)?;
    let (store, places) = open()?;
    let now = lifecycle::now_ms();
    let plan = lifecycle::plan(&store, &places, parsed.owner.as_deref(), now)
        .map_err(|e| e.to_string())?;
    let owners: Vec<_> = plan
        .owners
        .iter()
        .filter(|o| matches_folder(o.repo.as_deref(), parsed.folder.as_deref()))
        .collect();
    if parsed.json {
        let filtered = Plan {
            owners: owners.iter().map(|o| (*o).clone()).collect(),
            looks_like_ours: plan.looks_like_ours.clone(),
            now_ms: plan.now_ms,
        };
        println!("{}", filtered.to_json());
        return Ok(());
    }
    if owners.is_empty() {
        println!(
            "no owner of this box is recorded{}; a session records itself when it opens",
            match &parsed.folder {
                Some(f) => format!(" against {}", f.display()),
                None => String::new(),
            }
        );
        return Ok(());
    }
    for o in owners {
        if parsed.line {
            println!("{}: {}", o.id, o.holds_line(now));
            continue;
        }
        println!("{}: {}", o.id, o.holds_line(now));
        println!(
            "  ({}, {}, {} total)",
            o.kind.noun(),
            o.state.word(),
            bytes(o.bytes())
        );
        for h in &o.holds {
            println!("    {} — {}", h.handle, h.line());
        }
    }
    Ok(())
}

fn gc(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE_GC}");
        return Ok(());
    }
    let parsed = parse(args)?;
    let (store, places) = open()?;
    let now = lifecycle::now_ms();
    if parsed.log {
        let owners: Vec<String> = match &parsed.owner {
            Some(id) => vec![id.clone()],
            None => store
                .owners()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|o| o.id)
                .collect(),
        };
        let mut any = false;
        for id in owners {
            for e in store.reap_events_of(&id).map_err(|e| e.to_string())? {
                any = true;
                println!(
                    "{} {id} {} {} — {} ({})",
                    lifecycle::stamp_of(e.at_ms),
                    e.act,
                    e.handle.unwrap_or_default(),
                    e.said,
                    e.by
                );
            }
        }
        if !any {
            println!(
                "no journal lines: nothing has been reaped, which is what this build does - the \
                 plan is all there is"
            );
        }
        return Ok(());
    }
    let plan = lifecycle::plan(&store, &places, parsed.owner.as_deref(), now)
        .map_err(|e| e.to_string())?;
    if parsed.folder.is_some() || parsed.json {
        let filtered = Plan {
            owners: plan
                .owners
                .iter()
                .filter(|o| matches_folder(o.repo.as_deref(), parsed.folder.as_deref()))
                .cloned()
                .collect(),
            looks_like_ours: plan.looks_like_ours.clone(),
            now_ms: plan.now_ms,
        };
        if parsed.json {
            println!("{}", filtered.to_json());
        } else {
            print!("{}", filtered.render());
        }
        return Ok(());
    }
    print!("{}", plan.render());
    Ok(())
}

/// `6.1 GiB`, in the one spelling the plan uses.
pub(crate) fn bytes(n: u64) -> String {
    lifecycle::human_bytes(n)
}

/// Is this owner's repository the folder a caller asked about? `None` means the caller did not
/// narrow, and an owner with no repository is nobody's but still the box's.
fn matches_folder(repo: Option<&str>, folder: Option<&Path>) -> bool {
    match folder {
        None => true,
        Some(want) => {
            let want = std::fs::canonicalize(want).unwrap_or_else(|_| want.to_path_buf());
            repo.is_some_and(|r| {
                std::fs::canonicalize(r).unwrap_or_else(|_| PathBuf::from(r)) == want
            })
        }
    }
}

/// The store and the places: `$LETIBOT_STORE` else this box's data directory, opened READ-ONLY -
/// a plan that could write is a plan nobody can run beside a live daemon with a straight face.
fn open() -> Result<(Store, Places), String> {
    let target = std::env::var_os("LETIBOT_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(lifecycle::data_dir);
    let path = if target.is_dir() {
        split::shared_path(&target)
    } else {
        target
    };
    if !path.is_file() {
        return Err(format!(
            "no store at {}: nothing has been recorded to hold anything",
            path.display()
        ));
    }
    let store = Store::open_reader(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((store, Places::here()))
}
