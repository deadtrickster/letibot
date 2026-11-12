//! The built-in tools.
//! The rule they share: a miss produces more output than a hit, and every extra byte is something
//! the model can act on without another call. The two write tools pay most for it.

pub mod bash;
pub mod changelog;
pub mod decisions;
pub mod digest;
pub mod edit;
pub mod external;
pub mod fanout;
/// `fanout {background: true}`'s half: the handle that stands for N branches. A module of
/// its own because it is a second lifetime over the same plan, not another verb of the tool.
pub mod fanout_bg;
pub mod glob;
pub mod grep;
pub mod harness_view;
/// `host_config`: this box's own settings in the platform's own vocabulary, every change
/// recorded in [`changelog`].
pub mod host_config;
pub mod hosts;
pub mod intent;
pub mod jobs;
pub mod lsp;
pub mod merge_gate;
pub mod monitor;
pub mod notes;
pub mod outline;
/// Reading a search's candidate files: the one-call page and its bounded fallback, shared by
/// `grep` and `outline`.
pub mod paging;
pub mod pattern;
pub mod pkill;
pub mod plugin;
pub mod ps;
pub mod queue;
pub mod read;
pub mod read_spill;
pub mod recall;
pub mod retrieval;
pub mod room;
pub mod scenario;
pub mod screen;
pub mod session_self;
pub mod skill;
pub mod task;
pub mod todo;
pub mod transcript;
pub mod verdict;
pub mod write;

use crate::backend::{DirEntry, ExecBackend};

/// The deepest ancestor of `path` that exists, and its listing. This is what makes a missing path
/// useful rather than merely absent: what the model needs is what the nearest directory holds.
pub(crate) fn nearest_listing(backend: &dyn ExecBackend, path: &str) -> (String, Vec<DirEntry>) {
    let mut cur = path.trim_end_matches('/').to_string();
    loop {
        let parent = match cur.rfind('/') {
            Some(i) => cur[..i].to_string(),
            None => String::new(),
        };
        let probe = if parent.is_empty() {
            ".".to_string()
        } else {
            parent.clone()
        };
        if let Ok(entries) = backend.list(&probe) {
            return (probe, entries);
        }
        if parent.is_empty() {
            return (".".to_string(), backend.list(".").unwrap_or_default());
        }
        cur = parent;
    }
}

/// Entries whose name is close to what was asked for, and only near misses, since a listing already
/// shows everything. The threshold is deliberately tight: a suggestion the model chases is worse.
pub(crate) fn near_names(want: &str, entries: &[DirEntry], limit: usize) -> Vec<String> {
    let want = want.rsplit('/').next().unwrap_or(want).to_lowercase();
    let mut scored: Vec<(usize, &DirEntry)> = entries
        .iter()
        .map(|e| (edit_distance(&want, &e.name.to_lowercase()), e))
        .filter(|(d, e)| {
            *d * 3 <= want.len().max(e.name.len()) * 2
                || e.name.to_lowercase().contains(&want)
                || want.contains(&e.name.to_lowercase())
        })
        .collect();
    scored.sort_by_key(|(d, e)| (*d, e.name.clone()));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, e)| e.path.clone())
        .collect()
}

pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// A directory listing, in the form the model reads it back in.
pub(crate) fn render_listing(dir: &str, entries: &[DirEntry], limit: usize) -> String {
    let mut out = format!(
        "{dir}/ holds {} entr{}:\n",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" }
    );
    for e in entries.iter().take(limit) {
        if e.is_dir {
            out.push_str(&format!("  {}/\n", e.name));
        } else {
            out.push_str(&format!("  {} ({})\n", e.name, human(e.bytes)));
        }
    }
    if entries.len() > limit {
        out.push_str(&format!("  … and {} more\n", entries.len() - limit));
    }
    out
}

pub(crate) fn human(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Text out of bytes, without ever dropping any. A file that is not valid UTF-8 is still a file
/// somebody asked to read, so the replacement characters are visible rather than the call refused.
pub(crate) fn text_of(bytes: &[u8]) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), false),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), true),
    }
}

/// How a background job's or a subagent's completion reaches the model: one sentence,
/// and every tool text that says how a completion arrives says this one.
pub const DELIVERED: &str = "Its completion reaches you automatically — when your current turn \
     ends, or at once if a todo row needs it or wake is \"now\".";

/// Where a command sends its own output instead of letting the job capture it, knowable from the text:
/// ```text
/// cargo build --release … > /tmp/release-build.log 2>&1
/// ```
/// That command's captured output is empty by construction. Returns the first file stdout is written
/// to, or `None`. Only stdout counts: `2>/dev/null` leaves the capture full. Public because the daemon
/// puts it on the wire, so a head need not parse shell.
pub fn output_redirect_path(command: &str) -> Option<String> {
    redirect_paths(command, |fd| matches!(fd, None | Some(1)))
        .into_iter()
        .next()
}

/// Every file a command sends its output to, in the order the command names them, each once:
/// [`output_redirect_path`]'s rule applied to every stage of a compound command. A reader told where
/// a lost job's output went needs all of it, and only a literal path is provable from the text.
pub fn output_redirect_paths(command: &str) -> Vec<String> {
    redirect_paths(command, |fd| matches!(fd, None | Some(1) | Some(2)))
}

fn redirect_paths(command: &str, captured: impl Fn(Option<u32>) -> bool) -> Vec<String> {
    let n = letibot_code::shell::normalise(command);
    let mut out: Vec<String> = Vec::new();
    for stage in &n.stages {
        for r in &stage.redirects {
            if !r.op.writes() {
                continue;
            }
            // An untagged `>` is descriptor 1.
            if !captured(r.fd) {
                continue;
            }
            if let letibot_code::shell::RedirectTarget::File(w) = &r.target
                && let Some(text) = w.literal()
                && !out.iter().any(|p| p == text)
            {
                out.push(text.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod output_redirect_tests {
    use super::{output_redirect_path, output_redirect_paths};

    /// A redirect to a file, and the spellings that must not be mistaken for one. The `>` is the bug
    /// and the `2>&1` beside it is its opposite: a merge, which is what makes the capture complete.
    #[test]
    fn a_redirected_output_is_found_and_a_merged_descriptor_is_not() {
        let cases: &[(&str, Option<&str>)] = &[
            // Redirected: the capture is not where the output is.
            (
                "cargo build --release > /tmp/release-build.log 2>&1",
                Some("/tmp/release-build.log"),
            ),
            // Only stderr: the capture still holds stdout.
            ("make 2> err.log", None),
            ("grep -rn x . --include='*.md' 2>/dev/null | head -5", None),
            ("make >| out.log", Some("out.log")),
            ("make >> append.log", Some("append.log")),
            ("make 1> out.log", Some("out.log")),
            ("make 2> err.log 1> out.log", Some("out.log")),
            // Not redirected, and each for its own reason.
            //
            // `2>&1` alone is a merge into the captured stdout: the job sees everything.
            ("cargo build 2>&1", None),
            ("cargo test", None),
            // A read is not output going anywhere.
            ("sort < in.txt", None),
            // A here-document is stdin.
            ("cat <<'EOF'\nhi\nEOF", None),
            // Some other descriptor is not what a job captures.
            ("make 3> fd3.log", None),
            // A piped stage's own stdout still reaches the capture.
            ("make | tail -3", None),
            // A path that is built at run time: named for the reader, unprovable here.
            ("make > \"$OUT\"", None),
        ];
        for (command, want) in cases {
            assert_eq!(
                output_redirect_path(command).as_deref(),
                *want,
                "`{command}`"
            );
        }
    }

    /// Every file a compound command writes to, wherever in it the redirect sits.
    #[test]
    fn every_redirect_of_a_compound_command_is_found() {
        let cases: &[(&str, &[&str])] = &[
            // A compound command with the redirect at the end of its first stage.
            (
                "cd /tmp/letibot-scratch-1023699/harness && LETIDB_MODE=memory ./run_acceptance.sh --all > /tmp/acceptance-child.log 2>&1; echo \"exit=$?\"; tail -5 /tmp/acceptance-child.log",
                &["/tmp/acceptance-child.log"],
            ),
            ("a && b > f", &["f"]),
            ("a; b > f", &["f"]),
            ("a || b > f", &["f"]),
            ("a > x && b > y", &["x", "y"]),
            // `2>&1` before the file redirect: the file is still stdout's.
            ("b 2>&1 > f", &["f"]),
            ("b > f 2>&1", &["f"]),
            ("cd /w && make >> build.log 2>&1", &["build.log"]),
            ("make > \"/tmp/with space.log\"", &["/tmp/with space.log"]),
            ("make > '/tmp/q.log'", &["/tmp/q.log"]),
            ("(cd w && make > out.log)", &["out.log"]),
            ("{ make > out.log; }", &["out.log"]),
            ("make > a.log 2> b.log", &["a.log", "b.log"]),
            // Named once however often it is written.
            ("a > f; b >> f", &["f"]),
            // A piped stage's stdout still reaches the next; only its file redirect counts.
            ("make 2>&1 | tee -a x > y", &["y"]),
            // Nothing written: a merge, a read, a variable path.
            ("cd w && make 2>&1", &[]),
            ("cd w && sort < in.txt", &[]),
            ("cd w && make > \"$OUT\"", &[]),
        ];
        for (command, want) in cases {
            let got = output_redirect_paths(command);
            assert_eq!(
                got.iter().map(String::as_str).collect::<Vec<_>>(),
                *want,
                "`{command}`"
            );
            assert_eq!(
                output_redirect_path(command).as_deref(),
                want.first().copied()
            );
        }
    }

    /// `2>&1` after a redirect does not hide it: the merge beside a redirect must not talk
    /// this out of reporting.
    #[test]
    fn a_merge_after_a_redirect_does_not_cancel_it() {
        assert_eq!(
            output_redirect_path("cargo build > log.txt 2>&1").as_deref(),
            Some("log.txt")
        );
    }
}

#[cfg(test)]
mod prompt_line_tests {
    use crate::runtime::Tool;

    /// The prompt closes a behaviour, not a verb. The tool description IS the prompt, so it must
    /// state the rule about clocks rather than list forbidden verbs, and the reason is asserted too:
    /// swapping the rule for `do not write sleep` would keep the words and fail the requirement.
    #[test]
    fn the_bash_prompt_closes_the_waiting_behaviour_and_not_only_a_verb() {
        let d = super::bash::Bash::default().schema().description;
        assert!(
            d.contains("do not build your own clock") || d.contains("Do not build your own clock"),
            "the prompt must state the rule about clocks: {d}"
        );
        assert!(
            d.contains(super::DELIVERED),
            "the prompt must say WHY — that the completion reaches you — rather than only \
             forbidding a verb: {d}"
        );
        // The spelling is named as an example of the rule, not as the rule itself: a rule
        // the model cannot map to the code it was about to write is one it walks past.
        assert!(
            d.contains("sleep 200") || d.contains("sleep"),
            "the rule must be tied to the spelling that defeated R7: {d}"
        );
    }

    /// A parameter the tool description does not name does not exist. The property description is
    /// metadata a model skims; the tool description is the prompt line it reads and summarises itself
    /// with. Both are asserted, since either alone passes on the bug.
    #[test]
    fn the_prompt_line_names_the_parameters_that_replace_the_shell() {
        for (schema, param) in [
            (super::grep::Grep.schema(), "context"),
            (super::read::Read.schema(), "ranges"),
        ] {
            let name = schema.name.clone();
            assert!(
                schema.parameters["properties"].get(param).is_some(),
                "`{name}` must actually take `{param}` — otherwise this test is \
                 asserting nothing about the description below"
            );
            assert!(
                schema.description.contains(param),
                "`{name}`'s DESCRIPTION must name `{param}`; a model reads this line, \
                 not the property table. It says: {}",
                schema.description
            );
        }
    }
}
