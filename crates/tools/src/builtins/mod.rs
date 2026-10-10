//! The built-ins, and the helpers clause 1 needs in all of them.
//!
//! # Clause 1, which is the one that carries the value
//!
//! > A miss is self-correcting in the SAME call. The measured case: a model
//! > guessed a CSS selector, the tool answered *"that selector was a guess and the
//! > page does not have it — call read_page with NO selector"* **and handed back
//! > the whole page anyway**, and the model immediately found what it needed. A
//! > bare "not found" leaves the model nothing to correct itself with, so it
//! > guesses again — and the retry dance is what bloats permanent context.
//!
//! What each built-in does about it, in one line each:
//!
//! | tool | its miss |
//! |---|---|
//! | [`read`] | a missing path returns the nearest existing directory's listing and the near-miss names; a directory returns its listing; an offset past the end returns the file from line 1 and says so |
//! | [`grep`] | no match in scope reports **where the term does occur**, after relaxing a too-strict pattern to its bare identifier and reporting the relaxation |
//! | [`glob`] | a miss returns the surrounding listing **and what would have matched**, under a named relaxation |
//! | [`outline`] | an extension with no grammar is NAMED, with the list of grammars there are — never an empty outline; a `kind` filter that selects nothing reports the kinds that are there; a tree with nothing parseable is a failed scope and not an answer |
//! | [`read_spill`] | an unknown hash lists the spilled outputs this session has |
//! | [`retrieval`] | a corpus that does not cover the question abstains, in the `NO_RESULT` envelope, and says what it searched |
//! | [`edit`] | a miss reports **the text that is actually there**, at which lines, and whether the difference is whitespace, indentation or case; more than one match reports every line number |
//! | [`mod@write`] | an overwrite of an unread or changed file is refused **with the file**, and the refusal records it so the retry proceeds |
//! | [`bash`] | a command whose process predicate matches the process that would run it is refused with the pids it matches and the handle-shaped call that does what was meant |
//! | [`jobs`] | an unknown job id comes back with the jobs there are and the nearest; an unknown scope with the scopes there are and the three kinds |
//! | [`external`] | a tool whose infrastructure is not attached names what is missing, what would attach it, and what still works here — and returns `NotRun`, because nothing ran |
//! | [`intent`] | a completion with nothing measured behind it is reported as *claimed*, with the counts and what would settle it; an unmounted board, a headless `ask_user_question` and a lost row-claim each name what is missing rather than defaulting |
//! | [`notes`] | an unknown name comes back with every note that exists and the near misses; a search that matches nothing abstains with the corpus in the same body; a write that would overwrite, create, escape the notes directory **or land as a near-copy of a note that already exists** is refused with the fact it refused about — and the near-copy refusal names the note it would have duplicated. `write` and `edit` refuse a path into that directory by name (`notes::refuse_a_path_into_the_notes_dir`), so the gate is not something a path can walk around |
//!
//! The shared shape: a miss produces **more** output than a hit, not less, and
//! every one of those extra bytes is something the model can act on without
//! another call. The two write tools pay more for it than the read-only ones do,
//! because the alternative to a good miss report is a model guessing at a file it
//! is about to change.

pub mod bash;
pub mod decisions;
pub mod digest;
pub mod edit;
pub mod external;
pub mod glob;
pub mod grep;
pub mod harness_view;
pub mod intent;
pub mod jobs;
pub mod lsp;
pub mod merge_gate;
pub mod monitor;
pub mod notes;
pub mod outline;
pub mod pattern;
pub mod pkill;
pub mod ps;
pub mod read;
pub mod read_spill;
pub mod retrieval;
pub mod skill;
pub mod task;
pub mod todo;
pub mod transcript;
pub mod write;

use crate::backend::{DirEntry, ExecBackend};

/// The deepest ancestor of `path` that exists, and its listing.
///
/// This is what makes a missing path useful rather than merely absent: the model
/// asked for `src/parser/mod.rs`, `src/parser` does not exist, and what it needs
/// to know is what `src` actually holds.
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

/// Entries whose name is close to what was asked for.
///
/// Only near misses: a listing already shows everything, and repeating it as
/// "candidates" would be noise. The threshold is deliberately tight — a suggestion
/// the model then chases is worse than no suggestion.
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

/// Text out of bytes, without ever dropping any.
///
/// A file that is not valid UTF-8 is still a file somebody asked to read, and
/// refusing it teaches nothing. The replacement characters are visible, which is
/// the same rule `TokenDecoder::decode` follows one crate over.
pub(crate) fn text_of(bytes: &[u8]) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), false),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), true),
    }
}

/// **Where a command sends its own output instead of letting the job capture it** —
/// R41, requirement one.
///
/// The operator watched letibot start a background build and then `sleep 200`, and the
/// job they opened said *"waiting for the output"*. The command was:
///
/// ```text
/// cargo build --release … > /tmp/release-build.log 2>&1
/// ```
///
/// **Its captured output is empty by construction.** The job machinery captures what the
/// process writes to the pipe it hands it, and this command told the shell to send the
/// interesting bytes to a file instead — so `job_output` has nothing to return and
/// `job_wait` waits on a job whose signal is somewhere nothing is watching.
///
/// **Knowable before anything runs**, which is the whole point: the redirect is in the
/// command text, and this reads it there. Returns the path the output goes to, or `None`
/// when the command writes to stdout normally.
///
/// # What it does and does not look at
///
/// * **Writes to a file** — `>`, `>>`, `>|`, `<>` — on **stdout or stderr** (a bare `>` is
///   fd 1; `2>` is fd 2; `3>` is some other descriptor and is not what is being captured).
/// * **Not `2>&1`**, which is the opposite of a redirect to a file: it *merges* stderr
///   into stdout, which is exactly what makes the capture complete. The grammar already
///   keeps those apart (`RedirectTarget::Descriptor`), so this cannot mistake one for the
///   other.
/// * **Not `<`** — reading a file is not sending output anywhere.
/// * **Not a here-document or a here-string**, which are stdin.
///
/// The first such target is returned. A command that redirects twice — `> a 2> b` — is
/// named once, which is enough to say *the capture is not where your output is*.
///
/// **Public because the daemon puts it on the wire, not only in a payload** (R51 item 5). A head
/// that wanted to say which of its running jobs is unwatchable would otherwise have to parse a
/// shell command itself — a second spelling of exactly this function, in another language, in a
/// process that does not own the command. The daemon knows it; the row it publishes says it.
pub fn output_redirect_path(command: &str) -> Option<String> {
    let n = letibot_code::shell::normalise(command);
    for stage in &n.stages {
        for r in &stage.redirects {
            if !r.op.writes() {
                continue;
            }
            // Descriptor 1 and 2 are what a job captures. An untagged `>` is 1.
            if !matches!(r.fd, None | Some(1) | Some(2)) {
                continue;
            }
            if let letibot_code::shell::RedirectTarget::File(w) = &r.target
                && let Some(text) = w.literal()
            {
                return Some(text.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod output_redirect_tests {
    use super::output_redirect_path;

    /// **The operator's own command, and the five spellings that must not be mistaken for
    /// it** — R41 requirement one, whose whole value is that it is knowable before anything
    /// runs.
    ///
    /// The command in the report was
    /// `cargo build --release … > /tmp/release-build.log 2>&1`, and the pair of things in it
    /// that a careless reading gets wrong are the `>` (a real redirect, the bug) and the
    /// `2>&1` (a MERGE, which is what makes the capture complete — the opposite).
    #[test]
    fn a_redirected_output_is_found_and_a_merged_descriptor_is_not() {
        let cases: &[(&str, Option<&str>)] = &[
            // Redirected: the capture is not where the output is.
            (
                "cargo build --release > /tmp/release-build.log 2>&1",
                Some("/tmp/release-build.log"),
            ),
            ("make 2> err.log", Some("err.log")),
            ("make >| out.log", Some("out.log")),
            ("make >> append.log", Some("append.log")),
            ("make 1> out.log", Some("out.log")),
            ("make 2> err.log 1> out.log", Some("err.log")),
            // **Not redirected**, and each for its own reason.
            //
            // `2>&1` alone is a merge into the captured stdout — the job sees everything.
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

    /// **`2>&1` after a redirect does not hide it.** The pair is the operator's own command
    /// and the order matters: the first redirect wins the answer, and the merge beside it
    /// must not talk this out of reporting.
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

    /// **The prompt closes a BEHAVIOUR, not a verb** — R41 requirement three.
    ///
    /// R7 closed `job_wait` after backgrounding and the model went on actively waiting with
    /// `sleep 200; tail -3 log` — *"which is worse than what R7 removed. A fixed block with no
    /// completion at all; cannot be woken early; one tool call per poll; 200 seconds even when
    /// the build took 20."* The operator's own reading of why: *"R7 named one spelling.
    /// Naming a verb closes a verb; it does not close the behaviour."*
    ///
    /// So the description — which IS the prompt (clause 6) — must state the rule about clocks
    /// and must not be a list of forbidden verbs. Asserted on the rule's own words, and
    /// asserted to be about *waking* rather than about any one spelling: a future edit that
    /// replaced the rule with `do not write sleep` would keep this test passing on the word
    /// and fail the requirement, so the second assertion demands the reason too.
    #[test]
    fn the_bash_prompt_closes_the_waiting_behaviour_and_not_only_a_verb() {
        let d = super::bash::Bash.schema().description;
        assert!(
            d.contains("do not build your own clock") || d.contains("Do not build your own clock"),
            "the prompt must state the rule about clocks: {d}"
        );
        assert!(
            d.contains("you are woken"),
            "the prompt must say WHY — that the completion wakes you — rather than only \
             forbidding a verb: {d}"
        );
        // **And the one spelling that defeated R7 is named as an EXAMPLE of the rule**, not
        // as the rule itself. Both halves matter: a rule the model cannot map to the code it
        // was about to write is a rule it walks past.
        assert!(
            d.contains("sleep 200") || d.contains("sleep"),
            "the rule must be tied to the spelling that defeated R7: {d}"
        );
    }

    /// **A parameter the tool description does not name does not exist.**
    ///
    /// `context` and `ranges` shipped with careful property descriptions and were
    /// not added to the line above them. The property description is metadata a
    /// model skims; the tool description is the prompt line it reads and
    /// summarises itself with. Measured on the operator's own session, 2026-09-20:
    /// a session whose prefix carried both concluded it had neither, said so, and
    /// went back to `sed` — *"so the pg-noop session which is supposedly started
    /// after our tooling tweaks still uses damn sed"*.
    ///
    /// Worse, `abdc5d0`'s commit message claimed the opposite had been done: *"Only
    /// now does the prompt line have anything to stand on, so it names both"*. The
    /// argument went into a source comment, which nothing reads at runtime.
    ///
    /// Both halves are asserted, because either alone passes on the bug: the
    /// PROPERTY must be in the schema, and the DESCRIPTION must name it.
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
