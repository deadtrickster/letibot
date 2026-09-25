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
//!
//! **R39: `$CWD` is where a script file is resolved from, and `$READ_SCRIPTS=1` turns the
//! reading on.** `python3 foo.py` is judged from `foo.py`'s bytes, and those bytes are read
//! by the session's own reader (`letibot_tools::runtime::scripts_for`) against the session's
//! own workspace — so a corpus sweep has to be told the directory each command ran in. With
//! the flag off this example is exactly the old behaviour, which is what makes a
//! before/after sweep honest without building two revisions: `&[]` is not a
//! reimplementation of the scan, it is the same function with nothing to read.
use std::io::Read;

fn main() {
    let workspace = std::env::var("WS").unwrap_or_else(|_| "/home/dead/Projects/letibot".into());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/dead".into());
    let env = letibot_tools::intent::Surroundings {
        home: Some(home.clone()),
        workspace: Some(workspace.clone()),
        // `None`, not a guess: the scratch is the daemon's own directory and reading this
        // box's `$XDG_RUNTIME_DIR` to invent one would place paths the daemon would not.
        scratch: None,
        shell: letibot_tools::intent::ShellTrust::Pinned {
            how: "this example assumes the seat a session with a shell gets".into(),
        },
        seen_hosts: Default::default(),
    };

    // Where a relative script path is resolved from, and whether to read at all.
    let cwd = std::env::var("CWD").unwrap_or_else(|_| workspace.clone());
    let read_scripts = std::env::var("READ_SCRIPTS").is_ok_and(|v| v == "1");
    let show_scripts = std::env::var("SHOW_SCRIPTS").is_ok();

    // Arguments, or NUL-separated commands on stdin for a bulk pass. The second form is
    // what a corpus sweep wants: `sqlite3 … | cargo run --example classify`.
    //
    // **`PAIRS=1` makes the bulk pass alternate `cwd` and `command`** — R39's sweep, where
    // every row carries the directory it ran in and one process per row would be 30,000
    // processes to answer a question about one flag.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut lines = args;
    let mut pairs = Vec::new();
    if lines.is_empty() {
        let mut raw = Vec::new();
        std::io::stdin().read_to_end(&mut raw).unwrap();
        let fields: Vec<&str> = raw
            .split(|b| *b == 0)
            .filter_map(|c| std::str::from_utf8(c).ok())
            .collect();
        if std::env::var("PAIRS").is_ok() {
            for two in fields.chunks(2) {
                if let [cwd, cmd] = two {
                    pairs.push((cwd.to_string(), cmd.to_string()));
                }
            }
        } else {
            lines = fields
                .into_iter()
                .filter(|c| !c.trim().is_empty())
                .map(str::to_string)
                .collect();
        }
    }
    let default_cwd = cwd.clone();
    for pair in if pairs.is_empty() {
        lines
            .into_iter()
            .map(|c| (default_cwd.clone(), c))
            .collect::<Vec<_>>()
    } else {
        pairs
    } {
        let (cwd, cmd) = pair;
        // **The scripts this command runs, read the way a session's gate reads them.** The
        // same function the daemon calls, so a sweep measures the real reader — including
        // its refusal to open a path in a secret store — rather than a model of it. A
        // relative path resolves against the session's working directory and is confined to
        // the workspace root, which is what `HostBackend::resolve` does.
        let scripts = if read_scripts {
            let root = std::path::PathBuf::from(&workspace);
            let base = std::path::PathBuf::from(&cwd);
            let read = |p: &str| -> Result<Vec<u8>, String> {
                let expanded = match p.strip_prefix("~/") {
                    Some(rest) => format!("{}/{rest}", home.trim_end_matches('/')),
                    None => p.to_string(),
                };
                let full = if expanded.starts_with('/') {
                    std::path::PathBuf::from(expanded)
                } else {
                    base.join(expanded)
                };
                if !full.starts_with(&root) {
                    return Err(format!("Outside({p})"));
                }
                match std::fs::read(&full) {
                    Ok(b) => Ok(b),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        Err(format!("NotFound({p})"))
                    }
                    Err(e) => Err(format!("Io({e})")),
                }
            };
            let found = letibot_tools::runtime::scripts_for(&cmd, Some(&home), read);
            if show_scripts {
                for one in &found {
                    println!("  script {} => {:?}", one.path, one.body);
                }
            }
            found
        } else {
            Vec::new()
        };
        let b = letibot_tools::intent::Baseline::of_command_with(&cmd, &env, &scripts);
        println!(
            "reads={:5} tier={:12} verdict={:8} intents={:?} regions={:?}",
            b.reads_only(),
            b.tier.as_str(),
            b.verdict.as_str(),
            b.intents.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
            b.regions.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
        );
        // **On a character boundary, not a byte.** `&one[..120]` panics on any command
        // whose 120th byte is inside a multi-byte character — which aborted this example
        // after 78 of 7772 corpus commands the first time it was pointed at the store, and
        // silently, because the panic went to stderr.
        let one: String = cmd.replace('\n', " ⏎ ").chars().take(120).collect();
        // **The findings, which are what say WHY.** A tier on its own answers *what*; the
        // rule that fired is the only thing that tells a reader whether the answer is the
        // one they meant. Added while measuring R35, where the seven rows whose tier moved
        // could not be explained from the tier alone.
        for f in &b.findings {
            println!("  ! {f}");
        }
        println!("  {one}\n");
    }
}
