//! `letibot-migrate-split` - R129's migration to the split store (`docs/store-split.md`): the whole
//! file in one offline pass, one project key at a time, or the last step that renames what is left.
//!
//! It refuses to run under a live writer, because the pass copies the rows a daemon is holding: the
//! store's lease is the `session_writer` row taken when a session is opened
//! (`crates/harnessd/src/writer.rs`), and a claim is live while the pid it names is running.

use std::path::{Path, PathBuf};

use letibot_harnessd::mergequeue::lease::this_daemon;
use letibot_tokencore::migrate_split::{self, Carved, Claim, Finished, Report};
use letibot_tokencore::store::Store;

const USAGE: &str = "\
usage: letibot-migrate-split STORE DATA-DIR
       letibot-migrate-split --project KEY STORE DATA-DIR
       letibot-migrate-split --finish STORE DATA-DIR

  STORE     the sessions.db to split (~/.local/share/letibot/sessions.db)
  DATA-DIR  where shared.db and projects/<key>.db go (~/.local/share/letibot)

  --project KEY  carve ONE project out of STORE, so only that project's daemon stops. Repeat it
                 for each key; the shared tables stay in sessions.db until the last step.
  --finish       once every key is carved: check sessions.db holds no project row, drop the empty
                 project tables and rename it to shared.db. Stop every daemon for this one.
  (none)         the whole file in one offline pass, with every daemon stopped.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(said) = run(&args) {
        eprintln!("letibot-migrate-split: {said}");
        std::process::exit(1);
    }
}

/// What the arguments asked for: one whole pass, one key, or the last step.
enum Task {
    Whole,
    Carve(String),
    Finish,
}

fn run(args: &[String]) -> Result<(), String> {
    if args
        .first()
        .is_some_and(|arg| arg == "-h" || arg == "--help")
    {
        println!("{USAGE}");
        return Ok(());
    }
    // A flag is read and dropped, so the two positional paths are all that is left: an unknown one
    // is the usage line rather than a store called `--keys`.
    let mut task = Task::Whole;
    let mut rest: Vec<&String> = Vec::new();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        match arg.as_str() {
            "--project" => {
                let key = args
                    .get(at + 1)
                    .ok_or_else(|| format!("--project needs a KEY\n\n{USAGE}"))?;
                task = Task::Carve(key.clone());
                at += 2;
            }
            "--finish" => {
                task = Task::Finish;
                at += 1;
            }
            other if other.starts_with('-') => return Err(USAGE.to_string()),
            _ => {
                rest.push(arg);
                at += 1;
            }
        }
    }
    let [store, data] = rest.as_slice() else {
        return Err(USAGE.to_string());
    };
    let (store, data) = (PathBuf::from(store), PathBuf::from(data));
    if !store.is_file() {
        return Err(format!(
            "{} is not a store file - give the sessions.db to split",
            store.display()
        ));
    }
    match task {
        Task::Whole => whole(&store, &data),
        Task::Carve(key) => carve(&store, &data, &key),
        Task::Finish => finish(&store, &data),
    }
}

fn whole(store: &Path, data: &Path) -> Result<(), String> {
    let held = held_claims(store)?;
    if let Some(first) = held.first() {
        let more = match held.len() - 1 {
            0 => String::new(),
            n => format!(", and {n} other claim(s)"),
        };
        return Err(format!(
            "a daemon is holding this store: session {} is claimed by {} (pid {} on {}){more}. \
             Stop every daemon that writes it - `letibot --stop` in each folder - and run this \
             again, or carve one project at a time with --project KEY. Nothing was split and \
             nothing was renamed.",
            first.session, first.owner, first.pid, first.host
        ));
    }
    let report = migrate_split::migrate(store, data).map_err(|e| e.to_string())?;
    print_report(&report, data);
    Ok(())
}

fn carve(store: &Path, data: &Path, key: &str) -> Result<(), String> {
    // The claim check is the carve's own, so what stops it is one project's daemon and not the
    // box's: every other project keeps running.
    let carved = migrate_split::carve(store, data, key, &this_daemon().host, alive)
        .map_err(|e| e.to_string())?;
    print_carved(&carved);
    Ok(())
}

fn finish(store: &Path, data: &Path) -> Result<(), String> {
    let finished = migrate_split::finish(store, data, &this_daemon().host, alive)
        .map_err(|e| e.to_string())?;
    print_finished(&finished);
    Ok(())
}

fn print_report(report: &Report, data: &Path) {
    println!(
        "split {} session(s) into {} project file(s) under {}",
        report.sessions,
        report.projects.len(),
        data.display()
    );
    for file in &report.projects {
        println!(
            "  {}: {} session(s) -> {}",
            file.key,
            file.sessions,
            file.path.display()
        );
    }
    for table in &report.carried {
        println!("  the v24 archive's {table} went to shared.db");
    }
    for transcript in &report.unreplayed {
        println!("  {transcript} did not replay in the old store either - copied as stored");
    }
    println!(
        "kept {} until you are sure - delete it when the daemons have run on the split store",
        report.kept.display()
    );
}

fn print_carved(carved: &Carved) {
    println!(
        "carved project {} ({} session(s)) into {}",
        carved.key,
        carved.sessions,
        carved.path.display()
    );
    for transcript in &carved.unreplayed {
        println!("  {transcript} did not replay in the old store either - copied as stored");
    }
    match carved.uncarved.is_empty() {
        true => println!("every key is carved - stop every daemon and run --finish"),
        false => println!(
            "still in sessions.db: {} - restart that project's daemon and it opens its own file",
            carved.uncarved.join(", ")
        ),
    }
}

fn print_finished(finished: &Finished) {
    println!(
        "renamed the carved store to {} ({} project(s): {})",
        finished.shared.display(),
        finished.carved.len(),
        finished.carved.join(", ")
    );
}

/// The claims a daemon could still be writing under. The liveness rule is the daemon's own
/// (`crates/harnessd/src/writer.rs`): a claim on this host is live while its pid runs.
fn held_claims(store: &Path) -> Result<Vec<Claim>, String> {
    let open = Store::open(store).map_err(|e| e.to_string())?;
    let claims = migrate_split::writer_claims(&open).map_err(|e| e.to_string())?;
    let host = this_daemon().host.clone();
    Ok(migrate_split::live_claims(&claims, &host, alive))
}

/// A pid this box can still find running, and not a zombie its parent has not reaped.
fn alive(pid: u32) -> bool {
    letibot_tools::exec::procs::read(pid).is_some_and(|process| process.state != 'Z')
}
