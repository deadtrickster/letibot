//! `letibot session list|show|rename|archive|unarchive|delete`: the one spelling of the stored
//! session verbs. Each is translated to the daemon's store query and answered in this process,
//! so the verb and the old launcher flag it replaces cannot print different things.

use std::process::ExitCode;

const USAGE: &str = "\
letibot session VERB [ARGS] [--store DIR]
  The stored sessions: what is on disk, and naming, archiving or removing one.

  list [--tsv] [--all] [--archived|--archived-only]
                                              this folder's conversations first, then one
                                              block per other folder; --all is every session
  show ID                                     one session's stored facts, one per line
  rename ID [TITLE]                           name it; an empty title clears the name
  archive ID [REASON]                         put it out of sight and KEEP it
  unarchive ID                                put it back in the list
  delete ID                                   remove a session that has no rows

  example:
    letibot session rename s-1789919514688401228 'gate red'
";

/// The daemon's argv for one verb line, or the reason it is refused. `--store DIR` may come
/// anywhere, because the launcher appends it after the person's words.
pub fn translate(args: &[String]) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut store = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--store" {
            store = Some(it.next().ok_or("--store needs a value")?.clone());
        } else {
            words.push(a.clone());
        }
    }
    let Some((verb, rest)) = words.split_first() else {
        return Err("session needs a verb: list, show, rename, archive, unarchive, delete".into());
    };
    let need = |n: usize, what: &str| -> Result<(), String> {
        if rest.len() < n {
            Err(format!(
                "session {verb} needs {what}\n         run `letibot session --help` for what it takes"
            ))
        } else {
            Ok(())
        }
    };
    let mut out: Vec<String> = Vec::new();
    if let Some(s) = store {
        out.push("--store".into());
        out.push(s);
    }
    match verb.as_str() {
        "list" => {
            if let Some(bad) = rest.iter().find(|a| {
                !matches!(
                    a.as_str(),
                    "--tsv" | "--all" | "--archived" | "--archived-only"
                )
            }) {
                return Err(format!(
                    "session list takes --tsv, --all, --archived, --archived-only, not {bad}"
                ));
            }
            out.push("--list-sessions".into());
            out.extend(rest.iter().cloned());
        }
        "show" | "delete" | "unarchive" => {
            need(1, "an ID")?;
            let flag = match verb.as_str() {
                "show" => "--show-session",
                "delete" => "--delete",
                _ => "--unarchive",
            };
            out.extend([flag.to_string(), rest[0].clone()]);
        }
        "rename" | "archive" => {
            need(1, "an ID")?;
            out.extend([format!("--{verb}"), rest[0].clone()]);
            out.push(rest.get(1).cloned().unwrap_or_default());
        }
        other => {
            return Err(format!(
                "session has no verb {other}: list, show, rename, archive, unarchive, delete"
            ));
        }
    }
    Ok(out)
}

pub fn run(args: &[String]) -> ExitCode {
    let words: Vec<&String> = args.iter().filter(|a| *a != "--store").collect();
    // `--store DIR` alone is the launcher's bare `letibot session`.
    if args.is_empty() || (args[0] == "--store" && args.len() <= 2) {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }
    if args[0] == "help" || words.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match translate(args) {
        Ok(line) => crate::roles::run(
            crate::Role::Daemon,
            &line.into_iter().map(Into::into).collect::<Vec<_>>(),
        ),
        Err(e) => {
            eprintln!("letibot: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::translate;

    fn t(line: &[&str]) -> Result<Vec<String>, String> {
        translate(&line.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn each_verb_is_the_daemon_query_it_replaces() {
        let cases: &[(&[&str], &[&str])] = &[
            (
                &["list", "--tsv", "--store", "D"],
                &["--store", "D", "--list-sessions", "--tsv"],
            ),
            // The census is the listing's other shape, and the flag is handed on like `--tsv`.
            (
                &["list", "--all", "--store", "D"],
                &["--store", "D", "--list-sessions", "--all"],
            ),
            (&["show", "s-1"], &["--show-session", "s-1"]),
            (&["rename", "s-1", "gate"], &["--rename", "s-1", "gate"]),
            (&["rename", "s-1"], &["--rename", "s-1", ""]),
            (&["archive", "s-1", "done"], &["--archive", "s-1", "done"]),
            (&["unarchive", "s-1"], &["--unarchive", "s-1"]),
            (&["delete", "s-1"], &["--delete", "s-1"]),
        ];
        for (line, want) in cases {
            assert_eq!(t(line).unwrap(), *want, "{line:?}");
        }
    }

    #[test]
    fn a_missing_id_or_an_unknown_verb_is_refused_by_name() {
        assert!(t(&["rename"]).unwrap_err().contains("rename needs an ID"));
        assert!(t(&["bogus"]).unwrap_err().contains("no verb bogus"));
        assert!(t(&["list", "--json"]).unwrap_err().contains("not --json"));
        assert!(t(&[]).is_err());
    }
}
