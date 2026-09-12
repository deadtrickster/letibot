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
//!
//! The shared shape: a miss produces **more** output than a hit, not less, and
//! every one of those extra bytes is something the model can act on without
//! another call. The two write tools pay more for it than the read-only ones do,
//! because the alternative to a good miss report is a model guessing at a file it
//! is about to change.

pub mod bash;
pub mod edit;
pub mod external;
pub mod glob;
pub mod grep;
pub mod intent;
pub mod jobs;
pub mod monitor;
pub mod outline;
pub mod pattern;
pub mod read;
pub mod read_spill;
pub mod retrieval;
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
