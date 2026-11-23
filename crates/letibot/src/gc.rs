//! `letibot holds` and `letibot gc` - what every owner holds, and what would be reaped
//! (`docs/lifecycle.md`).
//!
//! Both read-only verbs run without a daemon: the owner index is in the store's shared file and
//! the proof of every holding is on the resource itself, so a person whose daemons are all dead can
//! still see the disk they are holding. Plain `gc` prints the plan and changes nothing.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use letibot_tokencore::store::split;
use letibot_tokencore::store::{
    HoldingClass, HoldingRow, HoldingState, OwnerKind, OwnerRow, OwnerState, Store,
};
use letibot_tools::lifecycle::UnitManager as _;
use letibot_tools::lifecycle::{self, Places, Plan};

const USAGE_HOLDS: &str = "\
letibot holds [OWNER] [--folder DIR] [--json] [--line] [--top]
  What the owners of this box hold on disk, one line each and one line per holding: the cgroup a
  session's jobs run in (with its process count and memory, which no `ps` can give a seat in a
  sandbox), a VM, its copies, its daemon, its worktree and target. Then each project's spill
  store under the data directory, with its size, then the daemon logs, store journals and job
  captures with the bounds `[gc.logs]` sets. `OWNER` narrows it to one session id.

  --folder DIR   only the owners working against DIR
  --json         the same facts as one object, for a program
  --line         only the `holds:` lines, which is what `--status` and a prompt row want
  --top          owners by the memory they hold (their session cgroup and their VM's run
                 cgroup), biggest first, then the box's own MemAvailable

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
  --docker-report
                 other projects' docker images and volumes, grouped by compose project and age,
                 with the commands that would remove them. It removes nothing: letibot cannot
                 prove another project's objects are its own. The `[gc.docker]` table is the only
                 policy that ever prunes, and it is off unless it names labels.
  --now          ACT, in one pass. Compute first: an owner idle, past a TTL or under a gone
                 daemon is closed, and its cgroups, VMs and daemons are ended. Then its WORK: a
                 worktree and its branch are kept in refs/letibot/archive/OWNER/ (the uncommitted
                 work as one `wip` commit made with a scratch index) and only then removed, and a
                 tree holding over 512 MB of untracked content goes to <gc root>/<box>/.trash
                 instead of being committed. Then disk: the caches of every owner whose compute is
                 gone, a copy-out after its work is in refs/letibot/archive/, the run directories
                 of VMs that are down, the shared build targets over their quota under the lock
                 `rrun` takes, each remote host's own pass, and rotated daemon logs past
                 `[gc.logs] keep`. Every act is journaled, and an act is done when the thing is
                 ABSENT.

  --targets      the `target-*` dirs under /build (or `--build DIR`): size, last use and the
                 processes working in them. Unused ones are stale, or with `[gc] targets_cap` the
                 unused ones oldest first until the total is under the cap. With `--now`, the
                 stale ones are removed; a dir in use never is.

  --settle OWNER   settle an owner's work NOW, without waiting out the grace
  --restore OWNER  put an owner's archived branches back; with `--worktree`, also a tree at the
                   archived head with its uncommitted work checked out over it
  --drop-archive   the archives a `--restore` could still bring back, by owner and age; with
                   `--now`, let them go (never run by the timer)
  --older D        with `--drop-archive`: only the archives older than this (`90d`)
  --adopt PATTERN  take worktrees of this folder's repository whose path matches PATTERN under an
                   owner `legacy-<date>`, stamped as letibot's; nothing is removed without `--now`
  --archive-settled
                   the sessions whose work is over (a reviewer's verdict delivered or superseded,
                   a one-shot ended, a subagent's result collected, a package landed or
                   abandoned), each with its reason; with `--now`, archive them. Never the
                   operator's own sessions, and never one a writer, a job or an ask holds. Archive
                   only hides: `letibot --session ID` resumes one

  example:
    letibot gc
    letibot gc s-1794509750373521593 --now
    letibot gc --log
    letibot gc --settle s-1794509750373521593
    letibot gc --restore s-1794509750373521593 --worktree
    letibot gc --adopt 'wt-pk-*' --now
    letibot gc --archive-settled
    letibot gc --targets --now";

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
    /// `holds --top`: owners by the memory their cgroups hold, then the box's own free memory.
    top: bool,
    /// `gc --log`: the journal instead of the plan.
    log: bool,
    /// `gc --now`: act, for compute.
    now: bool,
    /// `gc --docker-report`: other people's docker objects, sized and aged, and removed nothing.
    docker_report: bool,
    /// `hold --for`: the span, in milliseconds.
    until: Option<i64>,
    /// `gc --settle`: settle this owner's work now.
    settle: bool,
    /// `gc --restore`: bring an owner's archived work back.
    restore: bool,
    /// `gc --restore --worktree`: and a tree to put it in.
    worktree: bool,
    /// `gc --drop-archive`: the archives, planned; with `--now`, let them go.
    drop_archive: bool,
    /// `gc --adopt PATTERN`: take worktrees nothing recorded under `legacy-<date>`.
    adopt: Option<String>,
    /// `gc --older D`: only archives older than this, in milliseconds.
    older: Option<i64>,
    /// `gc --archive-settled`: the settled sessions, listed; with `--now`, archived.
    archive_settled: bool,
    /// `gc --targets`: the per-package `target-*` dirs instead of the owners.
    targets: bool,
    /// `gc --build DIR`: where those dirs are, `/build` by default.
    build: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        owner: None,
        folder: None,
        json: false,
        line: false,
        top: false,
        log: false,
        now: false,
        docker_report: false,
        until: None,
        settle: false,
        restore: false,
        worktree: false,
        drop_archive: false,
        adopt: None,
        older: None,
        archive_settled: false,
        targets: false,
        build: None,
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
            "--top" => out.top = true,
            "--log" => out.log = true,
            "--now" => out.now = true,
            "--settle" => out.settle = true,
            "--restore" => out.restore = true,
            "--worktree" => out.worktree = true,
            "--drop-archive" => out.drop_archive = true,
            "--archive-settled" => out.archive_settled = true,
            "--adopt" => {
                let pattern = it
                    .next()
                    .ok_or("`--adopt` needs a pattern after it, e.g. `wt-pk-*`")?;
                out.adopt = Some(pattern.to_string());
            }
            "--older" => {
                let span = it
                    .next()
                    .ok_or("`--older` needs a span after it, e.g. `90d`")?;
                out.older = Some(span_ms(span)?);
            }
            "--docker-report" => out.docker_report = true,
            "--targets" => out.targets = true,
            "--build" => {
                let dir = it.next().ok_or("`--build` needs a directory after it")?;
                out.build = Some(PathBuf::from(dir));
            }
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
    let remote = remote_holdings(&store, &places, &owners, parsed.owner.as_deref());
    // Item 6: the VMs firecode holds for this box, whatever the owner rows say - a VM whose owner
    // is gone is exactly what a person is looking for here.
    let firecode = firecode_vms(&store);
    if parsed.json {
        let filtered = Plan {
            owners: owners.iter().map(|o| (*o).clone()).collect(),
            looks_like_ours: plan.looks_like_ours.clone(),
            spills: plan.spills.clone(),
            bounded: plan.bounded.clone(),
            now_ms: plan.now_ms,
            systemd: plan.systemd.clone(),
            systemd_why: plan.systemd_why.clone(),
        };
        let mut json = filtered.to_json();
        json["remote"] = remote_json(&remote);
        json["firecode"] = firecode_json(&firecode);
        println!("{json}");
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
        print_spills(&plan.spills);
        print_bounded(&plan.bounded);
        print_remote(&remote);
        print_firecode(&firecode);
        return Ok(());
    }
    if parsed.top {
        // Owners by memory, then the box's own `MemAvailable`: the one view where the question is
        // "who is holding this box" rather than "what would be reaped".
        print!(
            "{}",
            Plan {
                owners: owners.iter().map(|o| (*o).clone()).collect(),
                looks_like_ours: Vec::new(),
                spills: Vec::new(),
                bounded: Vec::new(),
                now_ms: plan.now_ms,
                systemd: None,
                systemd_why: None,
            }
            .top()
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
            println!("    {}", h.line());
        }
    }
    print_spills(&plan.spills);
    print_bounded(&plan.bounded);
    print_remote(&remote);
    print_firecode(&firecode);
    Ok(())
}

/// What the owners shown hold on other hosts, per host, as the last remote pass measured it.
fn remote_holdings(
    store: &Store,
    places: &Places,
    owners: &[&lifecycle::OwnerView],
    only: Option<&str>,
) -> std::collections::BTreeMap<String, Vec<HoldingRow>> {
    let mut by_host = lifecycle::remote_holdings(store, &places.box_id).unwrap_or_default();
    for rows in by_host.values_mut() {
        rows.retain(|r| {
            only.is_none_or(|o| r.owner_id == o) && owners.iter().any(|o| o.id == r.owner_id)
        });
    }
    by_host.retain(|_, rows| !rows.is_empty());
    by_host
}

fn print_remote(by_host: &std::collections::BTreeMap<String, Vec<HoldingRow>>) {
    if by_host.is_empty() {
        return;
    }
    println!("remote hosts:");
    for line in lifecycle::remote_holdings_lines(by_host) {
        println!("  {line}");
    }
}

/// Every firecode VM this box knows of, and what it costs: item 6 of the wiring. Read from
/// `state ls --json` and `pool status --json`, with the owner's session resolved to its title so a
/// person reads `fc-wiring` and not `s-1794509750373521593`. `None` is a box with no firecode, or
/// one too old to list its state: `holds` is a report and must never fail over a missing tool.
struct FirecodeVms {
    rows: Vec<FirecodeVm>,
    pool: Vec<letibot_tools::firecode::cli::PoolRow>,
    /// The last hourly `gc --state` said, and when.
    gc: Option<letibot_tools::firecode::cli::GcNote>,
}

struct FirecodeVm {
    name: String,
    owner: Option<String>,
    title: Option<String>,
    purpose: Option<String>,
    size: Option<u64>,
    last_used: Option<String>,
    work: Option<String>,
    flagged: bool,
}

fn firecode_vms(store: &Store) -> FirecodeVms {
    let mut out = FirecodeVms {
        rows: Vec::new(),
        pool: Vec::new(),
        gc: letibot_tools::firecode::cli::read_gc_note(&lifecycle::data_dir()),
    };
    let Some(bin) = letibot_tools::firecode::cli::installed() else {
        return out;
    };
    let Ok(ls) = letibot_tools::firecode::cli::state_ls(&bin) else {
        return out;
    };
    for row in ls.sessions {
        let owner = row.owner_session().map(str::to_string);
        let title = owner
            .as_deref()
            .and_then(|id| store.session(id).ok().flatten())
            .and_then(|s| s.title);
        out.rows.push(FirecodeVm {
            name: row.name_or_project(),
            owner,
            title,
            purpose: row.purpose,
            size: row.size,
            last_used: row.last_used,
            work: row.work,
            flagged: row.flagged,
        });
    }
    out.pool = letibot_tools::firecode::cli::pool_status(&bin)
        .map(|s| s.pool)
        .unwrap_or_default();
    out
}

fn print_firecode(vms: &FirecodeVms) {
    if vms.rows.is_empty() && vms.pool.is_empty() && vms.gc.is_none() {
        return;
    }
    println!("firecode VMs:");
    for v in &vms.rows {
        println!("  {}", firecode_line(v));
    }
    if !vms.pool.is_empty() {
        println!(
            "  pool: {}",
            vms.pool
                .iter()
                .map(|p| format!(
                    "{} ({}, {})",
                    p.name.as_deref().unwrap_or("unnamed"),
                    p.state.as_deref().unwrap_or("state not said"),
                    p.purpose.as_deref().unwrap_or("purpose not said")
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(note) = &vms.gc {
        println!("  {}", note.gc.sentence());
    }
}

/// One VM's line: name, whose it is (the session's TITLE when the store knows it), what it is for,
/// what it costs, when it was last used, and whether its work is out.
fn firecode_line(v: &FirecodeVm) -> String {
    format!(
        "{} {} · {} · {} · last used {} · work {}{}",
        v.name,
        match (&v.title, &v.owner) {
            (Some(t), _) => format!("[{t}]"),
            (None, Some(o)) => format!("[{o}]"),
            (None, None) => "[no owner]".to_string(),
        },
        v.purpose.as_deref().unwrap_or("purpose not said"),
        match v.size {
            Some(n) => bytes(n),
            None => "size not said".to_string(),
        },
        v.last_used.as_deref().unwrap_or("never"),
        v.work.as_deref().unwrap_or("unknown"),
        if v.flagged {
            " · FLAGGED: its work was never copied out"
        } else {
            ""
        },
    )
}

fn firecode_json(vms: &FirecodeVms) -> serde_json::Value {
    serde_json::json!({
        "vms": vms
            .rows
            .iter()
            .map(|v| serde_json::json!({
                "name": v.name,
                "owner": v.owner,
                "title": v.title,
                "purpose": v.purpose,
                "bytes": v.size,
                "last_used": v.last_used,
                "work": v.work,
                "flagged": v.flagged,
            }))
            .collect::<Vec<_>>(),
        "pool": vms
            .pool
            .iter()
            .map(|p| serde_json::json!({
                "name": p.name,
                "state": p.state,
                "purpose": p.purpose,
            }))
            .collect::<Vec<_>>(),
        "gc": vms.gc.as_ref().map(|n| serde_json::json!({
            "at_ms": n.at_ms,
            "freed_bytes": n.gc.freed_bytes,
            "target_reached": n.gc.target_reached,
            "reason": n.gc.reason,
        })),
    })
}

fn remote_json(by_host: &std::collections::BTreeMap<String, Vec<HoldingRow>>) -> serde_json::Value {
    by_host
        .iter()
        .map(|(host, rows)| {
            let rows: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "owner": r.owner_id,
                        "kind": r.kind,
                        "handle": r.handle,
                        "bytes": r.bytes,
                        "state": r.state.word(),
                    })
                })
                .collect();
            (host.clone(), serde_json::Value::Array(rows))
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// The spill stores, after the owners: per project, not per owner.
fn print_spills(spills: &[lifecycle::SpillHolding]) {
    for sp in spills {
        println!("{}", sp.line());
    }
}

/// Daemon logs, store journals and job captures: the box's own, after everything an owner holds.
fn print_bounded(bounded: &[lifecycle::logs::BoundedHolding]) {
    for b in bounded {
        println!("{}", b.line());
    }
}

fn gc(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE_GC}");
        return Ok(());
    }
    let parsed = parse(args)?;
    if parsed.settle {
        return gc_settle(parsed.owner.as_deref());
    }
    if parsed.restore {
        return gc_restore(
            parsed.owner.as_deref(),
            parsed.worktree,
            parsed.folder.as_deref(),
        );
    }
    if parsed.drop_archive {
        return gc_drop_archive(parsed.owner.as_deref(), parsed.older, parsed.now);
    }
    if let Some(pattern) = &parsed.adopt {
        return gc_adopt(pattern, parsed.folder.as_deref(), parsed.now);
    }
    if parsed.archive_settled {
        return gc_archive_settled(parsed.now);
    }
    if parsed.docker_report {
        return docker_report(parsed.json);
    }
    if parsed.targets {
        let cap = lifecycle::Settings::load()?.targets_cap;
        let build = parsed.build.unwrap_or_else(|| PathBuf::from("/build"));
        print!(
            "{}",
            crate::targets::gc(&build, Path::new("/proc"), cap, parsed.now, parsed.json)
        );
        return Ok(());
    }
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
    let mut plan = lifecycle::plan(&store, &places, parsed.owner.as_deref(), now)
        .map_err(|e| e.to_string())?;
    // The user manager's table is read HERE and not in `plan()`: `holds` builds
    // the same plan on every turn, and a plan a model reads must not shell out to `systemctl`.
    match lifecycle::Systemd.table() {
        Ok(t) => plan.systemd = Some(t),
        Err(why) => plan.systemd_why = Some(why),
    }
    if parsed.folder.is_some() || parsed.json {
        let filtered = Plan {
            owners: plan
                .owners
                .iter()
                .filter(|o| matches_folder(o.repo.as_deref(), parsed.folder.as_deref()))
                .cloned()
                .collect(),
            looks_like_ours: plan.looks_like_ours.clone(),
            spills: plan.spills.clone(),
            bounded: plan.bounded.clone(),
            now_ms: plan.now_ms,
            systemd: plan.systemd.clone(),
            systemd_why: plan.systemd_why.clone(),
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
    // The unit table belongs to the box, not to an owner: the timer runs this line, so a failed
    // `letibot-*` unit is cleared with no daemon. A failed unit of another name is somebody
    // else's crash record and is left.
    let manager = lifecycle::Systemd;
    let reset = match manager.table() {
        Ok(t) if t.ours_failed.is_empty() => None,
        Ok(t) => Some(manager.reset_failed(&t.ours_failed).map(|()| t)),
        Err(why) => {
            if !json {
                println!("user manager: {why}");
            }
            None
        }
    };
    let pass = lifecycle::pass(&store, &places, &ops, owner, now).map_err(|e| e.to_string())?;
    // Rotated logs past the kept count belong to no owner, so the box pass trims them here; a
    // session's record streams are rotated here too, because rotation is an act and the plan only
    // reads. What the plan listed as due - a session archived past `age_days`, the rotated files
    // past `keep` - is removed by the same branch.
    let rotated = lifecycle::logs::rotate_session_logs(
        &lifecycle::session_logs_dir(&places),
        &places.settings.logs,
        &lifecycle::session_ages(&store),
        now,
    );
    let trimmed: Vec<PathBuf> = lifecycle::bounded_holdings(&places, &store, now)
        .iter()
        .flat_map(|b| b.reap.iter())
        .filter(|p| std::fs::remove_file(p).is_ok())
        .cloned()
        .collect();
    if json {
        let mut out = pass.to_json();
        out["logs_trimmed"] = serde_json::json!(trimmed);
        out["logs_rotated"] = serde_json::json!(rotated);
        println!("{out}");
        return Ok(());
    }
    print!("{}", pass.render());
    if !rotated.is_empty() {
        println!(
            "logs: rotated {} session record stream(s) past {}",
            rotated.len(),
            lifecycle::human_bytes(places.settings.logs.max_bytes)
        );
    }
    if !trimmed.is_empty() {
        println!(
            "logs: removed {} file(s) past their bound ({} kept, a session's logs {} days after \
             it was archived)",
            trimmed.len(),
            places.settings.logs.keep,
            places.settings.logs.age_days
        );
    }
    match reset {
        Some(Ok(t)) => println!(
            "user manager: reset {} failed letibot unit(s): {}",
            t.ours_failed.len(),
            t.ours_failed.join(", ")
        ),
        Some(Err(why)) => println!("user manager: the failed letibot units were not reset: {why}"),
        None => {}
    }
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

/// `letibot gc --settle OWNER`: the grace is skipped and one pass does the work now - the archive,
/// then the removal, then the session out of sight. What `leticode_archive` asks for from a seat.
fn gc_settle(owner: Option<&str>) -> Result<(), String> {
    let Some(owner) = owner else {
        return Err(
            "`--settle` needs the owner whose work should go: `letibot gc --settle s-…`. `letibot \
             gc` lists the owners and what each holds"
                .into(),
        );
    };
    let (store, places) = open_for_acts(Some(owner))?;
    let now = lifecycle::now_ms();
    let row = store
        .owner(owner)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no owner `{owner}` in this store"))?;
    if matches!(row.state, OwnerState::Live | OwnerState::Closing) {
        return Err(format!(
            "`{owner}` is `{}`: its compute is still here, and work is settled after a close - \
             `leticode_stop` or `leticode_archive` first, then this",
            row.state.word()
        ));
    }
    lifecycle::work::set_settling(
        &store,
        &row,
        now,
        "the operator asked for it (`letibot gc --settle`)",
    )
    .map_err(|e| e.to_string())?;
    let ops = lifecycle::Ops::here("letibot gc --settle");
    let pass =
        lifecycle::pass(&store, &places, &ops, Some(owner), now).map_err(|e| e.to_string())?;
    print!("{}", pass.render());
    Ok(())
}

/// `letibot gc --archive-settled [--now]`: the one-time backfill. Lists first; writes only with
/// `--now`, and only `archived_ms` and `archived_reason`.
fn gc_archive_settled(act: bool) -> Result<(), String> {
    use lifecycle::autoarchive;
    let (store, _places) = if act { open_for_acts(None)? } else { open()? };
    let here = autoarchive::Here::this_process();
    let write = autoarchive::write_to(&store);
    let decisions = autoarchive::sweep(
        &store,
        &here,
        &autoarchive::no_daemon,
        0,
        lifecycle::now_ms(),
        act.then_some(&write as autoarchive::ActFn<'_>),
    );
    if decisions.is_empty() {
        println!("no settled session is left in the list");
        return Ok(());
    }
    for d in &decisions {
        println!("{}", d.line());
    }
    let done = decisions.iter().filter(|d| d.archived).count();
    let kept = decisions.iter().filter(|d| d.refused.is_some()).count();
    let open = decisions.len() - done - kept;
    match act {
        true => println!("{done} archived, {kept} kept"),
        false => println!(
            "{open} would be archived, {kept} kept; `letibot gc --archive-settled --now` writes it"
        ),
    }
    Ok(())
}

/// `letibot gc --restore OWNER [--worktree]`: the branches an archive holds come back, and with
/// `--worktree` a tree at the archived head with the uncommitted work over it.
fn gc_restore(owner: Option<&str>, worktree: bool, _folder: Option<&Path>) -> Result<(), String> {
    let Some(owner) = owner else {
        return Err("`--restore` needs the owner to bring back: `letibot gc --restore s-…`".into());
    };
    let (store, places) = open()?;
    let row = store
        .owner(owner)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no owner `{owner}` in this store"))?;
    let repo = row.repo.clone().ok_or_else(|| {
        format!("`{owner}` names no repository, so there is nowhere its archive could be read from")
    })?;
    let ns = lifecycle::namespace(owner, "wt");
    let place = if worktree {
        let refs = lifecycle::refs_under(Path::new(&repo), &ns).map_err(|e| e.to_string())?;
        let branch = refs
            .iter()
            .find_map(|r| r.strip_prefix(&format!("{ns}/heads/")))
            .unwrap_or("restored");
        Some(lifecycle::restore_path(&places, owner, branch))
    } else {
        None
    };
    let restored = lifecycle::restore_worktree(Path::new(&repo), owner, place.as_deref())
        .map_err(|e| e.to_string())?;
    println!("{owner}: restored {}", restored.said());
    Ok(())
}

/// `letibot gc --drop-archive [OWNER] [--older 90d]`: the archives, planned by owner and age - and
/// with `--now`, let go. The timer never runs this: the refs are the only copy of work that was
/// removed, so a person asks.
fn gc_drop_archive(owner: Option<&str>, older: Option<i64>, act: bool) -> Result<(), String> {
    let (store, _places) = open()?;
    let now = lifecycle::now_ms();
    let rows: Vec<OwnerRow> = match owner {
        Some(id) => store
            .owner(id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .collect(),
        None => store.owners().map_err(|e| e.to_string())?,
    };
    let mut any = false;
    for row in rows {
        let Some(repo) = row.repo.as_deref() else {
            continue;
        };
        let ns = lifecycle::namespace(&row.id, "wt");
        let refs = lifecycle::refs_under(Path::new(repo), &ns).unwrap_or_default();
        if refs.is_empty() {
            continue;
        }
        let at = lifecycle::archives(Path::new(repo))
            .into_iter()
            .find(|(id, _, _)| id == &row.id)
            .map(|(_, at, _)| at)
            .unwrap_or(0);
        if let Some(span) = older
            && now - at.saturating_mul(1000) < span
        {
            continue;
        }
        any = true;
        println!(
            "{}: {} ref(s) under {ns}, last commit {}",
            row.id,
            refs.len(),
            lifecycle::stamp_of(at.saturating_mul(1000))
        );
        for r in &refs {
            println!("  {r}");
        }
        if act {
            let gone =
                lifecycle::drop_archive(Path::new(repo), &row.id).map_err(|e| e.to_string())?;
            println!("  dropped {}", gone.len());
        }
    }
    if !any {
        println!(
            "no archive of this box holds a ref{}",
            match older {
                Some(span) => format!(" older than {}", lifecycle::human_millis(span as u64)),
                None => String::new(),
            }
        );
        return Ok(());
    }
    if !act {
        println!(
            "\nthis is a plan: `--now` is what lets them go, and `letibot gc --restore OWNER` is \
             what puts the work back first"
        );
    }
    Ok(())
}

/// `letibot gc --adopt PATTERN`: worktrees of this folder's repository that nothing recorded, taken
/// under an owner `legacy-<date>` whose state is `closed` - so the normal plan and `--now` apply to
/// them, and nothing is removed until `--now` says so.
fn gc_adopt(pattern: &str, folder: Option<&Path>, act: bool) -> Result<(), String> {
    let folder = match folder {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?,
    };
    let (store, places) = open_for_acts(None)?;
    let repo = git_line(&folder, &["rev-parse", "--show-toplevel"]).ok_or_else(|| {
        format!(
            "{} is not in a git checkout, and adoption is about worktrees of one",
            folder.display()
        )
    })?;
    let repo = PathBuf::from(repo.trim());
    let mine: Vec<PathBuf> = worktrees_of(&repo)
        .into_iter()
        .filter(|p| glob_match(pattern, p))
        .collect();
    if mine.is_empty() {
        println!(
            "no worktree of {} matches `{pattern}` (its worktrees: {})",
            repo.display(),
            worktrees_of(&repo)
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        return Ok(());
    }
    let now = lifecycle::now_ms();
    let day: String = lifecycle::stamp_of(now).chars().take(10).collect();
    let owner = format!("legacy-{day}");
    let mut row = lifecycle::owner_for_open(
        &store,
        &owner,
        OwnerKind::Session,
        Some(repo.display().to_string()),
        now,
    )
    .map_err(|e| e.to_string())?;
    row.state = OwnerState::Closed;
    row.closed_ms = Some(now);
    row.closed_why = Some(format!("adopted by hand from `{pattern}`"));
    store.put_owner(&row).map_err(|e| e.to_string())?;
    for wt in &mine {
        let branch = git_line(wt, &["symbolic-ref", "--short", "HEAD"])
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty());
        // The stamp is what a settle step proves it by, and adoption writes it - that is the
        // whole difference between *looks like ours* and *ours*.
        if let Some(branch) = &branch {
            lifecycle::stamp_worktree(&repo, wt, branch, &owner)
                .map_err(|e| format!("{}: {e}", wt.display()))?;
        }
        let said =
            format!("adopted by `letibot gc --adopt {pattern}`; its stamp file names `{owner}`");
        store
            .put_holding(&HoldingRow {
                owner_id: owner.clone(),
                kind: lifecycle::WORKTREE.to_string(),
                host: None,
                handle: wt.display().to_string(),
                class: HoldingClass::Work,
                state: HoldingState::Held,
                stamp: said.clone(),
                bytes: lifecycle::size_of(wt).0,
                archive: None,
                why: Some("nothing recorded it, and it was taken by hand".into()),
                created_ms: now,
                checked_ms: Some(now),
                attempts: 0,
            })
            .map_err(|e| e.to_string())?;
        if let Some(branch) = branch {
            store
                .put_holding(&HoldingRow {
                    owner_id: owner.clone(),
                    kind: lifecycle::work::BRANCH.to_string(),
                    host: None,
                    handle: branch,
                    class: HoldingClass::Work,
                    state: HoldingState::Held,
                    stamp: said,
                    bytes: None,
                    archive: None,
                    why: None,
                    created_ms: now,
                    checked_ms: Some(now),
                    attempts: 0,
                })
                .map_err(|e| e.to_string())?;
        }
        println!("{owner}: adopted {}", wt.display());
    }
    if act {
        let ops = lifecycle::Ops::here("letibot gc --adopt --now");
        let pass =
            lifecycle::pass(&store, &places, &ops, Some(&owner), now).map_err(|e| e.to_string())?;
        print!("{}", pass.render());
    } else {
        println!(
            "\nthis is a plan: `letibot gc --now {owner}` (or `--settle {owner}`) is what acts, \
             and every one of them is archived before it is removed"
        );
    }
    Ok(())
}

/// `letibot gc --docker-report`: other people's images and volumes, grouped by compose project and
/// age, with the commands that would remove them. It removes nothing and it never will - letibot
/// cannot prove another project's objects are its own.
fn docker_report(json: bool) -> Result<(), String> {
    let places = lifecycle::Places::here();
    let Some(engine) = places.docker_engine else {
        println!(
            "no docker engine on this box: no `[docker] engine`, no $DOCKER_HOST, and nothing \
             listening at /run/docker.sock. Nothing to report, and nothing removed."
        );
        return Ok(());
    };
    let socket = engine.clone();
    let engine = letibot_tools::docker::Socket::at(&socket);
    let report = letibot_tools::docker::report(&engine, lifecycle::now_ms())
        .map_err(|e| format!("{}: {e}", socket.display()))?;
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.render());
    }
    Ok(())
}

/// One `git -C dir …`, its trimmed stdout - `None` for anything git refuses.
fn git_line(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every linked worktree of `repo` - the main checkout is not one, and is never adopted.
fn worktrees_of(repo: &Path) -> Vec<PathBuf> {
    let Some(list) = git_line(repo, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let main = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    list.lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .filter(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) != main)
        .collect()
}

/// `wt-pk-*` against a worktree path: the last segment or the whole path, with `*` and `?`.
fn glob_match(pattern: &str, path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let full = path.display().to_string();
    wildcard(pattern, &name) || wildcard(pattern, &full)
}

/// A `*`/`?` matcher, so `--adopt` needs no crate for one line of the design's.
fn wildcard(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
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
    // A set opens without its project file.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn a_vm(title: Option<&str>, work: Option<&str>, flagged: bool) -> FirecodeVm {
        FirecodeVm {
            name: "proj-ab12cd34".into(),
            owner: Some("s-1794".into()),
            title: title.map(str::to_string),
            purpose: Some("reviewer".into()),
            size: Some(412_316_860_416),
            last_used: Some("2026-11-23T10:00:00Z".into()),
            work: work.map(str::to_string),
            flagged,
        }
    }

    /// Item 6: a VM's line names the owner's session by its TITLE when the store knows one, and
    /// says what the VM costs and whether its work came out.
    #[test]
    fn a_firecode_vm_is_a_line_a_person_can_act_on() {
        let line = firecode_line(&a_vm(Some("fc-wiring"), Some("out"), false));
        assert!(line.starts_with("proj-ab12cd34 [fc-wiring]"), "{line}");
        assert!(line.contains("reviewer"), "{line}");
        assert!(line.contains("384.0 GiB") || line.contains("GiB"), "{line}");
        assert!(line.contains("last used 2026-11-23T10:00:00Z"), "{line}");
        assert!(line.contains("work out"), "{line}");
        assert!(!line.contains("FLAGGED"), "{line}");

        // An owner with no title is named by its id; a flagged run says what is wrong.
        let line = firecode_line(&a_vm(None, Some("in"), true));
        assert!(line.contains("[s-1794]"), "{line}");
        assert!(line.contains("work in"), "{line}");
        assert!(
            line.contains("FLAGGED: its work was never copied out"),
            "{line}"
        );

        // And nothing known is said as such, never as a plausible zero.
        let mut bare = a_vm(None, None, false);
        bare.owner = None;
        bare.purpose = None;
        bare.size = None;
        bare.last_used = None;
        let line = firecode_line(&bare);
        assert!(line.contains("[no owner]"), "{line}");
        assert!(line.contains("size not said"), "{line}");
        assert!(line.contains("work unknown"), "{line}");
    }

    /// The same facts as one object, for a program: the `--json` half of item 6.
    #[test]
    fn a_firecode_vm_is_a_row_a_program_can_read() {
        let vms = FirecodeVms {
            rows: vec![a_vm(Some("fc-wiring"), Some("out"), true)],
            pool: vec![letibot_tools::firecode::cli::PoolRow {
                name: Some("proj-ab12cd34".into()),
                purpose: Some("reviewer".into()),
                state: Some("sleeping".into()),
                project: None,
            }],
            gc: Some(letibot_tools::firecode::cli::GcNote {
                at_ms: 1_700_000_000_000,
                gc: letibot_tools::firecode::cli::Gc {
                    freed_bytes: 2048,
                    removed: vec!["a".into()],
                    target_reached: Some(false),
                    reason: Some("every remaining layer is younger than 14d".into()),
                    kept: serde_json::Value::Null,
                },
            }),
        };
        let json = firecode_json(&vms);
        assert_eq!(json["vms"][0]["title"], "fc-wiring");
        assert_eq!(json["vms"][0]["work"], "out");
        assert_eq!(json["vms"][0]["flagged"], true);
        assert_eq!(json["pool"][0]["state"], "sleeping");
        // A gc that stopped short is in the report, which is what item 5 asks for.
        assert_eq!(json["gc"]["target_reached"], false);
        assert_eq!(
            json["gc"]["reason"],
            "every remaining layer is younger than 14d"
        );
    }
}
