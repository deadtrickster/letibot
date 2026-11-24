//! `letibot catalogue [refresh]`: what this box's model catalogue is, and a fetch on demand.
//!
//! The daemon refreshes the cache by itself when it is a day old and `[catalogue] refresh` is on
//! (`crates/harnessd/src/catalogue_refresh.rs`). A person asking here is not that rule: this
//! fetches whatever the cache's age and that switch say, because somebody typed it, and it says
//! which models came and went instead of only that something was written.

use std::path::Path;
use std::time::SystemTime;

use letibot_provider::catalogue::Catalogue;
use letibot_provider::refresh;

/// What `letibot catalogue [refresh]` answers.
pub fn run(args: &[String]) -> std::process::ExitCode {
    let mut want_refresh = false;
    for arg in args {
        match arg.as_str() {
            "refresh" => want_refresh = true,
            "-h" | "--help" | "help" => {
                println!("{USAGE}");
                return std::process::ExitCode::SUCCESS;
            }
            other => {
                eprintln!(
                    "letibot catalogue: `{other}` is not a verb this reads; it takes `refresh`, \
                     or nothing to show the cache\n\n{USAGE}"
                );
                return std::process::ExitCode::from(2);
            }
        }
    }
    let Some(path) = letibot_provider::catalogue::cache_path() else {
        eprintln!(
            "letibot catalogue: this box has no cache directory - no $XDG_CACHE_HOME and no \
             $HOME - so there is nowhere the catalogue could be written"
        );
        return std::process::ExitCode::FAILURE;
    };
    if !want_refresh {
        for line in show(&path) {
            println!("{line}");
        }
        return std::process::ExitCode::SUCCESS;
    }
    match refresh_at(&path, &refresh::fetch_live, SystemTime::now()) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            std::process::ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("letibot catalogue: {why}");
            eprintln!("         the catalogue at {} is unchanged", path.display());
            std::process::ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
letibot catalogue [refresh]
  bare      the cache this box reads: its path and age, the providers it carries, and the
            model each preset sends when a bare `--model <preset>` names none
  refresh   fetch models.dev now - whatever the cache's age and whatever [catalogue] refresh
            says, because a person asked - and say which models came and went";

/// The state of the cache, as lines: where it is, how old it is, the providers it carries with
/// how many models each, and each preset's default model - the answer to `--model deepseek`.
pub fn show(path: &Path) -> Vec<String> {
    let mut out = vec![format!("catalogue — {}", path.display())];
    out.push(match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(at) => format!("  refreshed  {}", age(SystemTime::now(), at)),
        Err(_) => "  refreshed  never — the daemon's own refresh writes it, or `letibot catalogue refresh`".into(),
    });
    out.push(format!("  source     {}", refresh::URL));
    let cat = Catalogue::read(path).unwrap_or_default();
    let ids = refresh::catalogue_ids();
    let carried: Vec<String> = ids
        .iter()
        .map(|id| {
            let n = cat.model_names(id).len();
            if n == 0 {
                format!("{id} (not carried)")
            } else {
                format!("{id} {n}")
            }
        })
        .collect();
    out.push(format!(
        "  providers  {} of {} the presets name: {}",
        ids.iter()
            .filter(|id| !cat.model_names(id).is_empty())
            .count(),
        ids.len(),
        carried.join(", ")
    ));
    for p in letibot_provider::registry::all(None) {
        out.push(format!(
            "  default    {:<12} -> {}/{}",
            p.name,
            p.name,
            letibot_provider::name::default_model(p, None, &cat)
        ));
    }
    out
}

/// One refresh of `path` through `fetch`, and the lines that say what changed: the seam a test
/// drives with no network and no clock. Every failure leaves the files byte-identical, and the
/// error is the sentence the caller prints.
pub fn refresh_at(
    path: &Path,
    fetch: &refresh::Fetcher<'_>,
    now: SystemTime,
) -> Result<Vec<String>, String> {
    let before = Catalogue::read(path).ok();
    let outcome = refresh::refresh_at(path, fetch, now)?;
    let mut lines = vec![match outcome {
        refresh::Outcome::Written { providers, bytes } => {
            format!(
                "catalogue written — {providers} provider(s), {bytes} bytes at {}",
                path.display()
            )
        }
        refresh::Outcome::Unchanged => {
            "catalogue unchanged — the server says the copy here is current".into()
        }
    }];
    lines.extend(diff(before.as_ref(), Catalogue::read(path).ok().as_ref()));
    Ok(lines)
}

/// What came and went, one line per provider that moved: `zai-coding-plan: +glm-5.4 -glm-4.6`.
/// A provider neither side carries is not a change, and a model both carry is not either.
fn diff(before: Option<&Catalogue>, after: Option<&Catalogue>) -> Vec<String> {
    // No cache behind it is a first fetch, not a hundred additions: one line says the whole thing.
    let Some(before) = before else {
        return vec!["first fetch — every model here is new to this box".into()];
    };
    let empty = Catalogue::default();
    let after = after.unwrap_or(&empty);
    let mut out = Vec::new();
    for id in refresh::catalogue_ids() {
        let was = before.model_names(id);
        let now = after.model_names(id);
        let mut parts: Vec<String> = Vec::new();
        parts.extend(
            now.iter()
                .filter(|m| !was.contains(m))
                .map(|m| format!("+{m}")),
        );
        parts.extend(
            was.iter()
                .filter(|m| !now.contains(m))
                .map(|m| format!("-{m}")),
        );
        if !parts.is_empty() {
            out.push(format!("{id}: {}", parts.join(" ")));
        }
    }
    if out.is_empty() {
        out.push("no model moved".into());
    }
    out
}

/// `3h ago`, `4d3h ago`: a span a reader can act on, and never a bare timestamp.
fn age(now: SystemTime, at: SystemTime) -> String {
    let Ok(d) = now.duration_since(at) else {
        return "dated in the future, so this box's clock is wrong".into();
    };
    let secs = d.as_secs();
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    match (d, h, m) {
        (0, 0, 0) => "just now".to_string(),
        (0, 0, m) => format!("{m}m ago"),
        (0, h, _) => format!("{h}h ago"),
        (d, h, _) => format!("{d}d{h}h ago"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_provider::refresh::Answer;

    /// A body carrying every provider the presets name, with `models` under `deepseek`.
    fn body(models: &[&str]) -> String {
        let mut all = serde_json::Map::new();
        for id in refresh::catalogue_ids() {
            let named: Vec<&str> = if id == "deepseek" {
                models.to_vec()
            } else {
                vec!["kept"]
            };
            let mut m = serde_json::Map::new();
            for name in named {
                m.insert(
                    name.to_string(),
                    serde_json::json!({"limit": {"context": 100_000}, "cost": {"input": 1.0, "output": 2.0}}),
                );
            }
            all.insert(id.to_string(), serde_json::json!({"models": m}));
        }
        serde_json::to_string(&all).unwrap()
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "letibot-catalogue-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("models.json")
    }

    /// A fetch that answers with one body, and is not the network.
    fn answers(status: u16, body: String) -> Answer {
        Answer {
            status,
            etag: Some("\"probe\"".into()),
            last_modified: None,
            body: body.into_bytes(),
        }
    }

    #[test]
    fn a_refresh_says_which_models_came_and_which_went() {
        let path = scratch("diff");
        std::fs::write(&path, body(&["deepseek-flash", "deepseek-old"])).unwrap();
        let fetch = |_: &refresh::Conditional| {
            Ok::<Answer, String>(answers(200, body(&["deepseek-flash", "deepseek-v4-pro"])))
        };
        let lines = refresh_at(&path, &fetch, SystemTime::now()).expect("the fixture's body");
        assert!(
            lines
                .iter()
                .any(|l| l == "deepseek: +deepseek-v4-pro -deepseek-old"),
            "{lines:?}"
        );
        assert!(
            lines[0].starts_with("catalogue written"),
            "the header names what was written: {lines:?}"
        );
        // One line per provider that moved, and the others are not news.
        assert_eq!(lines.len(), 2, "{lines:?}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The first fetch has nothing to compare against, and a hundred `+model` lines is not news a
    /// reader can use: one line, and the header already says what was written.
    #[test]
    fn a_first_fetch_is_one_line_rather_than_every_model() {
        let path = scratch("first");
        let _ = std::fs::remove_file(&path);
        let fetch = |_: &refresh::Conditional| {
            Ok::<Answer, String>(answers(200, body(&["deepseek-flash", "deepseek-v4-pro"])))
        };
        let lines = refresh_at(&path, &fetch, SystemTime::now()).expect("a first fetch");
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].contains("first fetch"), "{lines:?}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_refresh_the_server_calls_current_writes_nothing_and_says_so() {
        let path = scratch("same");
        std::fs::write(&path, body(&["deepseek-flash"])).unwrap();
        let fetch = |_: &refresh::Conditional| Ok::<Answer, String>(answers(304, String::new()));
        let lines = refresh_at(&path, &fetch, SystemTime::now()).expect("a 304 is an answer");
        assert!(lines[0].contains("unchanged"), "{lines:?}");
        assert_eq!(lines[1], "no model moved", "{lines:?}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_failed_refresh_leaves_the_file_and_answers_the_sentence() {
        let path = scratch("fail");
        let kept = body(&["deepseek-flash"]);
        std::fs::write(&path, &kept).unwrap();
        let fetch = |_: &refresh::Conditional| {
            Err::<Answer, String>("models.dev did not answer: connection refused".into())
        };
        let why = refresh_at(&path, &fetch, SystemTime::now()).expect_err("a failed fetch is Err");
        assert!(why.contains("did not answer"), "{why}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            kept,
            "a failure leaves the cache byte-identical"
        );
        // And a body that is not a catalogue is the same kind of answer: nothing written.
        let fetch =
            |_: &refresh::Conditional| Ok::<Answer, String>(answers(200, "not json".into()));
        let why = refresh_at(&path, &fetch, SystemTime::now()).expect_err("not a catalogue");
        assert!(why.contains("did not answer a catalogue"), "{why}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), kept);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_show_lines_name_the_cache_the_providers_and_each_presets_default() {
        let path = scratch("show");
        std::fs::write(&path, body(&["deepseek-flash"])).unwrap();
        let lines = show(&path);
        assert!(lines[0].contains(&path.display().to_string()), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("providers") && l.contains("deepseek 1")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("  default    deepseek") && l.contains("-> deepseek/")),
            "{lines:?}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_cache_that_is_not_there_is_said_rather_than_read_as_an_empty_one() {
        let path = scratch("absent");
        let _ = std::fs::remove_file(&path);
        let lines = show(&path);
        assert!(lines[1].starts_with("  refreshed  never"), "{lines:?}");
        assert!(lines[2].contains(refresh::URL), "{lines:?}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
