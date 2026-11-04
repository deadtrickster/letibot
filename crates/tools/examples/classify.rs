//! What layer A thinks of a command line, one command per argument.
//! `cargo run --example classify -p letibot-tools -- 'head -c 200 src/app.rs'`
//! `$HOME`, `$WS`, `$CWD` and `$READ_SCRIPTS` stand in for the session a corpus row does not record.
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
        session: Default::default(),
        // `None`, for the reason `scratch` is: a corpus is a record of decisions and
        // not the tree they were taken in, so nothing here opens a file to find out
        // which binary a bare name would have run.
        programs: Default::default(),
        shell_path: None,
    };

    // Where a relative script path is resolved from, and whether to read at all.
    let cwd = std::env::var("CWD").unwrap_or_else(|_| workspace.clone());
    let read_scripts = std::env::var("READ_SCRIPTS").is_ok_and(|v| v == "1");
    let show_scripts = std::env::var("SHOW_SCRIPTS").is_ok();

    // Arguments, or NUL-separated commands on stdin for a bulk pass, which is what a corpus
    // sweep wants: `sqlite3 ... | cargo run --example classify`. `PAIRS=1` alternates `cwd`
    // and `command`, because one process per row would be tens of thousands of processes.
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
        // The scripts this command runs, read by the same function the daemon calls, so a
        // sweep measures the real reader. A relative path resolves against the session's
        // working directory and is confined to the workspace root, as `HostBackend::resolve` does.
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
        // On a character boundary, not a byte: `&one[..120]` panics on any command whose
        // 120th byte is inside a multi-byte character, and the panic goes to stderr.
        let one: String = cmd.replace('\n', " ⏎ ").chars().take(120).collect();
        // The findings are what say why: a tier answers what, and the rule that fired is
        // the only thing that tells a reader whether the answer is the one they meant.
        for f in &b.findings {
            println!("  ! {f}");
        }
        println!("  {one}\n");
    }
}
