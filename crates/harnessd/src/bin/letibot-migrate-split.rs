//! `letibot-migrate-split` - R129's P2: the one offline pass that turns a `sessions.db` into
//! `shared.db` and a project file per folder (`docs/store-split.md` § Migration).
//!
//! It refuses to run under a live writer, because the pass renames the file a daemon is holding:
//! the store's lease is the `session_writer` row taken when a session is opened
//! (`crates/harnessd/src/writer.rs`), and a claim is live while the pid it names is running.

use std::path::{Path, PathBuf};

use letibot_harnessd::mergequeue::lease::this_daemon;
use letibot_tokencore::migrate_split::{self, Claim};
use letibot_tokencore::store::Store;

const USAGE: &str = "\
usage: letibot-migrate-split STORE DATA-DIR

  STORE     the sessions.db to split; renamed to sessions.db.pre-split once every check passes
  DATA-DIR  where shared.db and projects/<key>.db go (~/.local/share/letibot)

Every daemon that writes the store must be stopped first: the pass renames the file they hold.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(said) = run(&args) {
        eprintln!("letibot-migrate-split: {said}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<(), String> {
    if args
        .first()
        .is_some_and(|arg| arg == "-h" || arg == "--help")
    {
        println!("{USAGE}");
        return Ok(());
    }
    if args.iter().any(|arg| arg.starts_with('-')) {
        return Err(USAGE.to_string());
    }
    let [store, data] = args else {
        return Err(USAGE.to_string());
    };
    let (store, data) = (PathBuf::from(store), PathBuf::from(data));
    if !store.is_file() {
        return Err(format!(
            "{} is not a store file - give the sessions.db to split",
            store.display()
        ));
    }
    let held = held_claims(&store)?;
    if let Some(first) = held.first() {
        let more = match held.len() - 1 {
            0 => String::new(),
            n => format!(", and {n} other claim(s)"),
        };
        return Err(format!(
            "a daemon is holding this store: session {} is claimed by {} (pid {} on {}){more}. \
             Stop every daemon that writes it - `letibot --stop` in each folder - and run this \
             again. Nothing was split and nothing was renamed.",
            first.session, first.owner, first.pid, first.host
        ));
    }
    let report = migrate_split::migrate(&store, &data).map_err(|e| e.to_string())?;
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
    Ok(())
}

/// The claims a daemon could still be writing under. The liveness rule is the daemon's own
/// (`crates/harnessd/src/writer.rs`): a claim on this host is live while its pid runs.
fn held_claims(store: &Path) -> Result<Vec<Claim>, String> {
    let open = Store::open(store).map_err(|e| e.to_string())?;
    let claims = migrate_split::writer_claims(&open).map_err(|e| e.to_string())?;
    let host = this_daemon().host.clone();
    Ok(migrate_split::live_claims(&claims, &host, |pid| {
        letibot_tools::exec::procs::read(pid).is_some_and(|process| process.state != 'Z')
    }))
}
