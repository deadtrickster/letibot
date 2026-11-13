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
letibot gc [OWNER] [--folder DIR] [--json] [--log | --now [--json]]
  The plan: for every owner, what would be reaped, why, and when or never. A directory that looks
  like letibot's and names no owner the store knows is listed under `unrecorded` and is never
  reaped by a pattern. Without `--now` nothing in this command removes, stops or writes anything.

  --folder DIR   only the owners working against DIR
  --json         the plan (or the pass) as one object
  --log          the journal of what was done to these owners (`reap_event`), oldest first
  --now          ACT, in one pass. Compute first: an owner idle, past a TTL or under a gone
                 daemon is closed, and its cgroups, VMs and daemons are ended. Then disk: the
                 caches of every owner whose compute is gone (a build target, a /tmp, a confined
                 home, the tree a VM was booted on, a firecode state layer), a copy-out after its
                 work is in refs/letibot/archive/, the run directories of VMs that are down, the
                 shared build targets over their quota under the lock `rrun` takes, and each
                 remote host's own pass. Worktrees and branches stay (G3). Every act is
                 journaled, and an act is done when the thing is ABSENT.

  example:
    letibot gc
    letibot gc s-1794509750373521593 --now
    letibot gc --log";

const USAGE_HOLD: &str = "\
letibot hold OWNER [--for 2d]
  Keep an owner from every automatic close: no idle TTL, no hard TTL, no daemon-gone pass. It is
  the only opt-out, it is visible in `letibot holds` and in the plan, and it expires - without
  `--for` it is held for ever. `letibot release OWNER` lets go again.

  --for D        a span: `90s`, `30m`, `6h`, `2d`, or `1h30m`

  example:
    letibot hold s-1794509750373521593 --for 2d";

const USAGE_RELEASE: &str = "\
letibot release OWNER
  Let go of a hold `letibot hold` put on an owner, so the automatic triggers apply again.

  example:
    letibot release s-1794509750373521593";

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

pub fn run_hold(args: &[String]) -> ExitCode {
    match hold(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("letibot hold: {why}");
            ExitCode::FAILURE
        }
    }
}

pub fn run_release(args: &[String]) -> ExitCode {
    match release(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("letibot release: {why}");
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
    /// `gc --now`: act, for compute.
    now: bool,
    /// `hold --for`: the span, in milliseconds.
    until: Option<i64>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        owner: None,
        folder: None,
        json: false,
        line: false,
        log: false,
        now: false,
        until: None,
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
            "--now" => out.now = true,
            "--for" => {
                let span = it
                    .next()
                    .ok_or("`--for` needs a span after it, e.g. `2d`")?;
                out.until = Some(span_ms(span)?);
            }
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

/// `90s`, `30m`, `6h`, `2d`, or `1h30m` as milliseconds. Every unit is a multiple of a second, so a
/// span is read as a sum of them and a bare number is refused rather than guessed at.
fn span_ms(span: &str) -> Result<i64, String> {
    let mut total: i64 = 0;
    let mut digits = String::new();
    let mut any = false;
    for c in span.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let n: i64 = digits
            .parse()
            .map_err(|_| format!("`{span}` is not a span: `2d`, `6h`, `30m` or `90s`"))?;
        digits.clear();
        total += n * match c.to_ascii_lowercase() {
            's' => 1_000,
            'm' => 60 * 1_000,
            'h' => 60 * 60 * 1_000,
            'd' => 24 * 60 * 60 * 1_000,
            other => {
                return Err(format!(
                    "`{other}` is not a unit: `s`, `m`, `h` or `d` (`{span}`)"
                ));
            }
        };
        any = true;
    }
    if !digits.is_empty() || !any {
        return Err(format!(
            "`{span}` names no unit: `2d`, `6h`, `30m` or `90s`"
        ));
    }
    Ok(total)
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
    if parsed.now {
        // Opening for a write is the one thing this verb family does not do read-only, so it is
        // asked BEFORE the plan's reader is opened: one connection, and no second opinion about
        // which file the facts are in.
        return gc_now(
            parsed.owner.as_deref(),
            parsed.folder.as_deref(),
            parsed.json,
        );
    }
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
                "no journal lines: nothing has been reaped yet - `letibot gc --now` is what does it"
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

/// `letibot gc --now`: one reap pass, over every owner or the one named, printed as it goes. The
/// store is opened for WRITING, which is the one place a verb of this family does - and every
/// change is a `reap_event` row, so what it did is readable afterwards with `letibot gc --log`.
fn gc_now(owner: Option<&str>, folder: Option<&Path>, json: bool) -> Result<(), String> {
    let (store, places) = open_for_acts(owner)?;
    let ops = lifecycle::Ops::here("letibot gc --now");
    let now = lifecycle::now_ms();
    let pass = lifecycle::pass(&store, &places, &ops, owner, now).map_err(|e| e.to_string())?;
    if json {
        println!("{}", pass.to_json());
        return Ok(());
    }
    print!("{}", pass.render());
    if let Some(want) = folder {
        // A pass acts on the whole box; the filter is said rather than silently narrowing it.
        println!(
            "(this pass acted on every owner; `--folder {}` narrows the PLAN, not the pass)",
            want.display()
        );
    }
    if !pass.acts.is_empty() {
        println!(
            "`letibot gc --log {}` is the journal",
            owner.unwrap_or_default()
        );
    }
    Ok(())
}

fn hold(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE_HOLD}");
        return Ok(());
    }
    let parsed = parse(args)?;
    let id = parsed
        .owner
        .ok_or("`letibot hold` needs an owner: `letibot hold s-… --for 2d`")?;
    let (store, _) = open_for_acts(Some(&id))?;
    let owner = store
        .owner(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "no owner `{id}` in this store: a hold is on a session letibot recorded, and this \
                 one has no row"
            )
        })?;
    let now = lifecycle::now_ms();
    // No `--for` is a hold for ever, which is `0` and not an accident of parsing.
    let until = parsed.until.map_or(0, |span| now + span);
    store
        .hold_owner(&id, Some(until))
        .map_err(|e| e.to_string())?;
    println!(
        "{}: held {}",
        owner.id,
        match parsed.until {
            Some(span) => format!(
                "for {} (until {}Z)",
                lifecycle::human_millis(span as u64),
                lifecycle::stamp_of(until)
            ),
            None => "for ever (`letibot release` lets go)".to_string(),
        }
    );
    Ok(())
}

fn release(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE_RELEASE}");
        return Ok(());
    }
    let parsed = parse(args)?;
    let id = parsed
        .owner
        .ok_or("`letibot release` needs an owner: `letibot release s-…`")?;
    let (store, _) = open_for_acts(Some(&id))?;
    let owner = store
        .owner(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no owner `{id}` in this store"))?;
    store.hold_owner(&id, None).map_err(|e| e.to_string())?;
    println!(
        "{}: released — the automatic triggers apply again",
        owner.id
    );
    Ok(())
}

/// The store for a verb that ACTS: `$LETIBOT_STORE` else this box's data directory, opened for
/// writing. A split directory is opened as the SET of the owner's own project file, because the
/// `process` rows a reap ends are kept per project - an owner named with no row still opens the
/// index alone.
fn open_for_acts(owner: Option<&str>) -> Result<(Store, Places), String> {
    let target = std::env::var_os("LETIBOT_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(lifecycle::data_dir);
    if !target.is_dir() {
        if !target.is_file() {
            return Err(format!(
                "no store at {}: nothing has been recorded to hold anything",
                target.display()
            ));
        }
        let store = Store::open(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        return Ok((store, Places::here()));
    }
    let key = owner
        .and_then(|id| {
            Store::open_reader(&split::shared_path(&target))
                .ok()?
                .owner(id)
                .ok()
                .flatten()
        })
        .and_then(|o| o.repo)
        .map(|repo| letibot_tokencore::migrate_split::project_key(&repo))
        .unwrap_or_else(|| "000000000000".to_string());
    // An EXISTING set: a verb that acts must not create a project file for a key nobody works in,
    // which is the R129 rake in the other direction. A set opens without its project file.
    let store = Store::open_set_existing(&target, &key)
        .map_err(|e| format!("{}: {e}", target.display()))?;
    Ok((store, Places::here()))
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
