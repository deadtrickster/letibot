//! **The reference checker — what a note claims about the tree, and whether the tree still
//! says it.**
//!
//! The design's ruling, verbatim: *"and then we need a periodic poke to review notes for
//! being up-to-dater"*, and its own account of why this half comes first:
//!
//! > **References.** A checker can verify these mechanically: does the path exist, is the
//! > commit sha still reachable, does the symbol still appear, is the line range still inside
//! > the file. That is the shape `warning.rs`'s census test and `check-fmt.sh` already have —
//! > a thing that cannot drift silently — and it produces **evidence** rather than a feeling.
//! > A note whose references no longer resolve is the cheapest honest staleness signal
//! > available.
//!
//! So this module is the cheap half of the keeper: **no model, no network, deterministic**,
//! and it answers a question a reader can check by hand. The keeper's session
//! ([`crate::notes_keeper`]) is asked *after* this has run, and is handed the answer.
//!
//! # The four kinds, and what "resolved" means for each
//!
//! | kind | the claim | resolved means |
//! |---|---|---|
//! | [`RefKind::Path`] | a file or directory is there | it exists under one of the roots |
//! | [`RefKind::Lines`] | `path:120-140` is inside the file | the path exists and has that many lines |
//! | [`RefKind::Sha`] | a commit is real and reachable | `cat-file -e` finds it, and `merge-base --is-ancestor` says whether it is on `HEAD` |
//! | [`RefKind::Symbol`] | a name appears in the code | `git grep` finds it |
//!
//! **A sha that exists but is not an ancestor of `HEAD` is `Resolved`, and it says so.** That
//! is not a dodge: a note citing an unlanded branch's tip is *accurate* while the branch is
//! unlanded, and a note citing a sha that was rebased away is not — and only the keeper, with
//! the note in front of it, can tell those apart. The checker's job is to produce the fact,
//! not to grade it.
//!
//! # Which tree a note is about — and why the workspace is not always the answer
//!
//! MEASURED on this box, and it is the reason [`tree_roots`] exists: the operator's workspace
//! is `/home/dead/Projects/letibot`, its notes are in `.letibot/notes/` there, and the code
//! those notes cite (`crates/tools/src/…`) is in `/home/dead/Projects/letibot/**letibot**/`.
//! The workspace is not a repository at all — it holds five — so resolving `crates/…` against
//! the workspace would report every path in every note as stale, which is the failure mode
//! this whole feature must not have: *"the first time a reader learns to ignore the hint line,
//! the feature is dead."*
//!
//! So the roots are the workspace itself **and the repositories at and under it**, in that
//! order, and the report names the root each path resolved under. Git is asked of the first
//! root that is a repository — for this box, the main tree.
//!
//! # The noise rule, which is not decoration
//!
//! A checker that cries *stale* on a good note is worse than no checker — the design's own rule
//! about hints applies to reports: *"the first time a reader learns to ignore the hint line, the
//! feature is dead."*
//!
//! **This was measured, not reasoned.** The first version of this module, run over this box's 26
//! notes, reported **125 of 582** references stale — 21%, nearly all of them prose:
//!
//! | what it read | what it was |
//! |---|---|
//! | `/queue`, `/notes`, `/reseat` | slash commands, read as absolute paths |
//! | `*.md`, `crates/harnessd/src/*.rs`, `<slug>` | a glob or a template, read as a filename |
//! | `duration/edit/decision` | prose wearing a slash |
//! | `tools/builtins/todo.rs`, `daemon.rs:155` | the note's own shorthand for a path further down |
//! | `.md`, `file://` | a suffix being discussed, and a URL |
//!
//! Each is now a rule, and each errs toward silence:
//!
//! * **A token is only a citation when it looks like one.** A path needs a known extension with a
//!   stem, an absolute or `~` or `./` head with a second segment, or a slash whose head is a
//!   directory the tree has; a symbol needs `::`, a call, or a snake_case name. `2.7`, `the`,
//!   `Verbatim:`, `**bold**`, `/queue` and `.md` are none of those.
//! * **A note's shorthand is followed, once.** `tools/builtins/todo.rs` resolves to
//!   `crates/tools/src/builtins/todo.rs` when exactly one tracked path contains those components
//!   in order; when several do, the reference is `Uncheckable` — *nobody looked* — rather than a
//!   guess at which one the note meant.
//! * **A two-segment token with no extension is a citation only when it resolves** — as a path or
//!   as a git ref. `agent/notes-offer` is a branch and resolves; `and/or` and `read/write` are
//!   prose and do not, and are dropped rather than reported. The cost is stated rather than
//!   hidden: a note naming a branch that has since been deleted is *not* flagged by this rule.
//!
//! A note whose references cannot be checked at all — no git, no repository — is
//! [`Outcome::Uncheckable`], never `Unresolved`. A checker that cannot check has not found
//! anything, and saying otherwise is the one thing it must not do.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The four things a note can claim about the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefKind {
    /// A path exists.
    Path,
    /// A path exists and is at least that long.
    Lines,
    /// A commit is real, and whether it is on `HEAD`.
    Sha,
    /// A name appears in the repository's files.
    Symbol,
}

impl RefKind {
    /// Every kind, in the order the report renders them — the one list, for the reason
    /// [`crate::gatekeeper::Decision::ALL`] is one: the parse and the rendering both ask
    /// *which of the values is this*, and a second list is a second answer.
    pub const ALL: [RefKind; 4] = [RefKind::Path, RefKind::Lines, RefKind::Sha, RefKind::Symbol];

    pub fn as_str(&self) -> &'static str {
        match self {
            RefKind::Path => "path",
            RefKind::Lines => "lines",
            RefKind::Sha => "sha",
            RefKind::Symbol => "symbol",
        }
    }

    /// The kind a word names, if it names one. `None` for anything else — never a default,
    /// because a kind nobody knows is a reference nobody can render.
    pub fn parse(s: &str) -> Option<RefKind> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// What the tree says about one reference.
///
/// Three outcomes and not two, because *"I could not check"* is not *"it is gone"*: a checker
/// with no repository under it has found nothing, and reporting that as staleness would be
/// inventing the evidence the keeper is supposed to be handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The tree still says it. The string is what it resolved to, in the reader's words.
    Resolved(String),
    /// The tree does not say it any more. The string is why, so the keeper can quote it.
    Unresolved(String),
    /// Nobody looked: no git, or no repository. The string is why.
    Uncheckable(String),
}

impl Outcome {
    /// The word the report renders this with.
    pub fn word(&self) -> &'static str {
        match self {
            Outcome::Resolved(_) => "resolved",
            Outcome::Unresolved(_) => "unresolved",
            Outcome::Uncheckable(_) => "unchecked",
        }
    }

    /// The evidence — what it resolved to, or why it did not.
    pub fn why(&self) -> &str {
        match self {
            Outcome::Resolved(s) | Outcome::Unresolved(s) | Outcome::Uncheckable(s) => s,
        }
    }

    pub fn is_unresolved(&self) -> bool {
        matches!(self, Outcome::Unresolved(_))
    }

    pub fn is_resolved(&self) -> bool {
        matches!(self, Outcome::Resolved(_))
    }
}

/// One thing a note cites, and what the tree says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub kind: RefKind,
    /// The token as it is looked up — a call's own name for a symbol (`nearest(want: &str)`
    /// is looked up as `nearest`), the note's spelling for everything else. The first
    /// spelling of a token wins; a note that cites one path five times is one reference.
    pub text: String,
    /// The line of the note it is on, 1-based — so a report can be read beside the note.
    pub line: usize,
    pub outcome: Outcome,
}

/// Every reference one note makes, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceReport {
    /// The roots the paths were resolved against, in the order they were tried.
    pub roots: Vec<PathBuf>,
    /// The repository git was asked of, when there was one.
    pub repo: Option<PathBuf>,
    /// The references, in the order the note cites them.
    pub references: Vec<Reference>,
}

impl ReferenceReport {
    pub fn resolved(&self) -> Vec<&Reference> {
        self.references
            .iter()
            .filter(|r| r.outcome.is_resolved())
            .collect()
    }

    pub fn unresolved(&self) -> Vec<&Reference> {
        self.references
            .iter()
            .filter(|r| r.outcome.is_unresolved())
            .collect()
    }

    pub fn uncheckable(&self) -> Vec<&Reference> {
        self.references
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Uncheckable(_)))
            .collect()
    }

    /// **No reference is unresolved** — the cheap honest staleness signal, at its cheapest.
    /// Unchecked references are not a clean bill of health, and `is_clean` says nothing about
    /// them; the count is in [`ReferenceReport::render`] where a reader cannot miss it.
    pub fn is_clean(&self) -> bool {
        self.unresolved().is_empty()
    }

    /// The evidence, as the keeper's prompt carries it and as the operator's pane shows it:
    /// **the unresolved first**, because that is what a review is about, then the rest.
    pub fn render(&self) -> String {
        let mut out = format!(
            "{} reference(s) checked against {} — {} resolved, {} unresolved, {} unchecked.\n",
            self.references.len(),
            match &self.repo {
                Some(r) => format!("the repository at {}", r.display()),
                None => format!(
                    "{} (no repository was found, so only paths could be checked)",
                    describe_roots(&self.roots)
                ),
            },
            self.resolved().len(),
            self.unresolved().len(),
            self.uncheckable().len(),
        );
        if self.references.is_empty() {
            out.push_str("  (the note cites nothing this checker can resolve)\n");
            return out;
        }
        for r in self.unresolved() {
            out.push_str(&format!(
                "  {:<10} {:<7} {}  (line {}) — {}\n",
                r.outcome.word(),
                r.kind.as_str(),
                r.text,
                r.line,
                r.outcome.why()
            ));
        }
        for r in &self.references {
            if r.outcome.is_unresolved() {
                continue;
            }
            out.push_str(&format!(
                "  {:<10} {:<7} {}  (line {}) — {}\n",
                r.outcome.word(),
                r.kind.as_str(),
                r.text,
                r.line,
                r.outcome.why()
            ));
        }
        out
    }
}

/// **The trees a note in this workspace describes**, in the order they are tried.
///
/// The workspace itself first — a workspace that IS a repository answers everything — then
/// every repository at or directly under it, by name. See the module doc for the measurement
/// that made this necessary: on this box the workspace holds the notes and the repositories
/// hold the code, so one root would report every path in every note as stale.
///
/// One level, deliberately: `repos_under`-style deep search is the daemon's business (it names
/// the repositories a workspace holds), and a checker that descended three levels would start
/// resolving a note's `crates/x.rs` against somebody's vendored copy.
pub fn tree_roots(workspace: &Path) -> Vec<PathBuf> {
    let mut roots = vec![workspace.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(workspace) {
        let mut subs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join(".git").exists())
            .collect();
        subs.sort();
        roots.extend(subs);
    }
    roots
}

/// **Every reference one note makes, resolved.** The one entry point.
///
/// `workspace` is the workspace the note lives in — the directory whose `.letibot/notes/` the
/// harness reads — and [`tree_roots`] turns it into the trees that are actually searched.
pub fn check(workspace: &Path, text: &str) -> ReferenceReport {
    let roots = tree_roots(workspace);
    let repo = roots.iter().find(|r| is_a_repo(r)).cloned();

    let mut seen: BTreeSet<(RefKind, String)> = BTreeSet::new();
    let mut candidates: Vec<(RefKind, String, usize)> = Vec::new();
    for (line, token) in cited(text) {
        let Some((kind, needle)) = classify(&token) else {
            continue;
        };
        if seen.insert((kind, needle.clone())) {
            candidates.push((kind, needle, line));
        }
    }

    // **One git call per kind, not one per reference.** A note cites twenty symbols and a
    // `git grep` per symbol would be twenty processes for one question.
    //
    // A qualified symbol is looked up twice, and the second lookup is what keeps a module alias
    // from reading as staleness: `view::SettledDecision::kind` is written that way in a note and
    // as `SettledDecision::kind` in the tree, so the last segment is asked for too. What the
    // checker does with the two answers is [`Ctx::symbol`]'s.
    let symbols: Vec<String> = candidates
        .iter()
        .filter(|(k, _, _)| *k == RefKind::Symbol)
        .flat_map(|(_, t, _)| symbol_needles(t))
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    let symbol_hits = match &repo {
        Some(repo) if !symbols.is_empty() => grep_symbols(repo, &symbols),
        _ => None,
    };
    let bare: Vec<String> = candidates
        .iter()
        .filter_map(|(kind, text, _)| match kind {
            RefKind::Path => Some(text.clone()),
            RefKind::Lines => split_range(text).map(|(head, _)| head.to_string()),
            _ => None,
        })
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    let shorthand = match &repo {
        Some(repo) if !bare.is_empty() => index_shorthand(repo, &bare),
        _ => None,
    };

    let ctx = Ctx {
        roots: &roots,
        repo: repo.as_deref(),
        symbol_hits: symbol_hits.as_ref(),
        shorthand: shorthand.as_ref(),
    };
    let references: Vec<Reference> = candidates
        .into_iter()
        .filter_map(|(kind, text, line)| {
            ctx.resolve(kind, &text).map(|outcome| Reference {
                kind,
                text,
                line,
                outcome,
            })
        })
        .collect();
    ReferenceReport {
        roots,
        repo,
        references,
    }
}

/// What resolution needs, gathered once.
///
/// `symbol_hits` and `shorthand` are `None` when git could not be asked at all — a box with no
/// git, or a call that failed for a reason that is not *no matches*. The distinction is the
/// module's whole three-outcome rule: a symbol nobody looked for is `Uncheckable`, never
/// `Unresolved`, because reporting it stale would be inventing evidence.
struct Ctx<'a> {
    roots: &'a [PathBuf],
    repo: Option<&'a Path>,
    symbol_hits: Option<&'a HashMap<String, usize>>,
    shorthand: Option<&'a HashMap<String, Shorthand>>,
}

/// What a note's shorthand names, when the token as written is not a path from any root.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shorthand {
    /// Exactly one file's components contain the token's, in order.
    One(PathBuf),
    /// Several do, and the note's shorthand names none of them in particular.
    Many(usize),
}

/// Where a token turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Placement {
    At(PathBuf),
    /// A git ref — a branch or a tag, which is a thing the tree really holds.
    Ref(String),
    /// The shorthand matched several files.
    Many(usize),
    /// Nowhere, and [`Ctx::not_a_citation`] decides whether that is a finding or prose.
    Nothing,
}

impl Ctx<'_> {
    /// `None` means **this is not a citation** — a token that resolved as nothing and is not
    /// shaped like a claim about the tree. See the module doc's noise rule.
    fn resolve(&self, kind: RefKind, text: &str) -> Option<Outcome> {
        match kind {
            RefKind::Path => self.path(text),
            RefKind::Lines => self.lines(text),
            RefKind::Sha => Some(self.sha(text)),
            RefKind::Symbol => Some(self.symbol(text)),
        }
    }

    /// **Where a token is**, in the order the lookups are cheap and the answers are specific:
    /// the path as written under each root, then a bare name under that root's own
    /// `.letibot/notes/` (a note citing another note spells it by name), then a git ref, then the
    /// shorthand index.
    fn locate(&self, token: &str) -> Placement {
        for root in self.roots {
            let at = join(root, token);
            if std::fs::metadata(&at).is_ok() {
                return Placement::At(at);
            }
            if !token.contains('/') {
                let note = root.join(".letibot").join("notes").join(token);
                if std::fs::metadata(&note).is_ok() {
                    return Placement::At(note);
                }
            }
        }
        if let Some(repo) = self.repo {
            if let Some(name) = git_ref(repo, token) {
                return Placement::Ref(name);
            }
        }
        match self.shorthand.and_then(|m| m.get(token)) {
            Some(Shorthand::One(rel)) => Placement::At(
                self.repo
                    .map(|r| r.join(rel))
                    .unwrap_or_else(|| rel.clone()),
            ),
            Some(Shorthand::Many(n)) => Placement::Many(*n),
            None => Placement::Nothing,
        }
    }

    fn path(&self, token: &str) -> Option<Outcome> {
        match self.locate(token) {
            Placement::At(at) => Some(Outcome::Resolved(format!(
                "{} at {}",
                if at.is_dir() { "a directory" } else { "a file" },
                at.display()
            ))),
            Placement::Ref(name) => Some(Outcome::Resolved(format!(
                "a ref in this repository (`{name}`)"
            ))),
            Placement::Many(n) => Some(Outcome::Uncheckable(format!(
                "{n} files of this name are tracked here and the note's shorthand names none of \
                 them in particular, so this was not resolved"
            ))),
            Placement::Nothing => self.is_a_citation(token).then(|| {
                Outcome::Unresolved(format!(
                    "no such path under {}, and no ref or file of that name",
                    describe_roots(self.roots)
                ))
            }),
        }
    }

    fn lines(&self, token: &str) -> Option<Outcome> {
        let Some((head, end)) = split_range(token) else {
            return Some(Outcome::Uncheckable(format!(
                "`{token}` is not a line range this can read"
            )));
        };
        match self.locate(head) {
            Placement::At(at) => {
                let Ok(text) = std::fs::read_to_string(&at) else {
                    return Some(Outcome::Uncheckable(format!(
                        "{} could not be read",
                        at.display()
                    )));
                };
                let have = text.lines().count();
                Some(if end <= have {
                    Outcome::Resolved(format!("{} has {have} lines", at.display()))
                } else {
                    Outcome::Unresolved(format!(
                        "{} has only {have} lines, and the note cites line {end}",
                        at.display()
                    ))
                })
            }
            Placement::Ref(_) => Some(Outcome::Uncheckable(format!(
                "`{head}` is a ref rather than a file, so it has no lines to be inside"
            ))),
            Placement::Many(n) => Some(Outcome::Uncheckable(format!(
                "{n} files are tracked under this name and the note's shorthand names none of \
                 them in particular, so line {end} was not checked"
            ))),
            Placement::Nothing => self.is_a_citation(head).then(|| {
                Outcome::Unresolved(format!(
                    "no such path under {}, and no ref or file of that name",
                    describe_roots(self.roots)
                ))
            }),
        }
    }

    /// **Whether a token that resolved as nothing is a finding rather than prose.**
    ///
    /// Every rule here errs toward silence, and each one was written because the corpus said so:
    /// on this box's 26 notes, a checker without them reported **125 of 582** references stale,
    /// almost all of them prose — `/queue` and `/notes` read as absolute paths, `*.md` and
    /// `<slug>` read as filenames, `duration/edit/decision` read as a path, and
    /// `tools/builtins/todo.rs` read as a path from the root when it is a shorthand for a file
    /// four directories down. A checker that cries stale on a good note is worse than none.
    fn is_a_citation(&self, token: &str) -> bool {
        // A URL, a glob, or a template: not a claim about where a file is.
        if token.contains("://") {
            return false;
        }
        if token
            .chars()
            .any(|c| matches!(c, '*' | '?' | '<' | '>' | '{' | '}' | '\u{2026}'))
        {
            return false;
        }
        // An absolute token needs a second segment or an extension: `/queue`, `/notes` and
        // `/reseat` are slash commands, and a checker that read them as paths reported five of
        // them in one note.
        if let Some(rest) = token.strip_prefix('/') {
            return rest.contains('/') || has_extension(token);
        }
        if !has_extension(token) {
            // A bare word is not a path.
            if !token.contains('/') {
                return false;
            }
            // A multi-segment token with no extension is a citation only when its head is a
            // directory the tree really has (`crates/tools/src`) — otherwise it is prose wearing
            // a slash, or a branch name that has since gone, and both are silence.
            let head = token.split('/').next().unwrap_or("");
            return self.roots.iter().any(|r| r.join(head).is_dir());
        }
        true
    }

    fn sha(&self, token: &str) -> Outcome {
        let Some(repo) = self.repo else {
            return Outcome::Uncheckable(
                "no repository was found, so a commit cannot be looked up".into(),
            );
        };
        if !git_ok(repo, &["cat-file", "-e", &format!("{token}^{{commit}}")]) {
            return Outcome::Unresolved(format!(
                "no commit beginning {token} exists in {}",
                repo.display()
            ));
        }
        if git_ok(repo, &["merge-base", "--is-ancestor", token, "HEAD"]) {
            Outcome::Resolved("a commit on HEAD's history".into())
        } else {
            Outcome::Resolved(
                "a commit that exists but is NOT on HEAD's history — an unlanded branch, or one \
                 that was rebased"
                    .into(),
            )
        }
    }

    fn symbol(&self, token: &str) -> Outcome {
        let Some(hits) = self.symbol_hits else {
            return Outcome::Uncheckable(match self.repo {
                Some(_) => "git could not be asked about symbols in this repository".into(),
                None => "no repository was found, so a symbol cannot be looked up".to_string(),
            });
        };
        if let Some(n) = hits.get(token) {
            return Outcome::Resolved(format!("{n} hit(s) of `{token}` in this repository"));
        }
        // **A qualified name that is absent while its last segment is present is NOT stale.**
        // A note writes `view::SettledDecision::kind` for a `use`-aliased path, and a checker
        // that called that stale would be reporting its own lack of a name resolver. Nobody
        // looked at whether the symbol moved, so the answer is `Uncheckable` — and the evidence
        // for the reader to judge is in the sentence.
        let last = token.rsplit("::").next().unwrap_or(token);
        if last != token {
            if let Some(n) = hits.get(last) {
                return Outcome::Uncheckable(format!(
                    "`{token}` appears nowhere as written, but `{last}` appears {n} time(s) — a \
                     module alias, or a rename, and this cannot tell the two apart"
                ));
            }
            return Outcome::Unresolved(format!(
                "neither `{token}` nor `{last}` appears in any file of this repository"
            ));
        }
        Outcome::Unresolved(format!("`{token}` appears in no file of this repository"))
    }
}

/// A path under `root`, with `~` and an absolute token meaning what they say.
fn join(root: &Path, token: &str) -> PathBuf {
    if let Some(rest) = token.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    if token.starts_with('/') {
        return PathBuf::from(token);
    }
    root.join(token.trim_start_matches("./"))
}

/// How the report names the roots, when a reader has to know what was searched.
fn describe_roots(roots: &[PathBuf]) -> String {
    match roots {
        [] => "(no roots)".to_string(),
        [one] => one.display().to_string(),
        many => format!(
            "{} roots ({})",
            many.len(),
            many.iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// --- the extraction -----------------------------------------------------------------------

/// Every token the note cites, with the line it is on.
///
/// Backticked spans are the house's way of naming code and are read as such; the prose around
/// them is read too, because this corpus cites `crates/tools/src/x.rs` in a sentence at least
/// as often as in backticks. Fenced code blocks are skipped: a block is a quotation of code,
/// not a claim about where code lives.
fn cited(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut fenced = false;
    for (i, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        for raw in line.split('`').flat_map(str::split_whitespace) {
            if let Some(t) = trim_token(raw) {
                out.push((i + 1, t));
            }
        }
    }
    out
}

/// A token with the punctuation a sentence puts around it taken off — but never a leading `.`
/// (`.letibot/notes/x.md` is a path) and never a trailing `:` that a line number follows.
fn trim_token(raw: &str) -> Option<String> {
    let mut t = raw.trim_matches(|c: char| {
        matches!(
            c,
            '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '"' | '\'' | ',' | ';' | '*' | '!'
        )
    });
    t = t.trim_end_matches(['.', ':', '?']);
    if t.chars().count() < 3 || t.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(t.to_string())
}

/// What a token is, if it is a citation at all — and the text it is looked up by.
fn classify(token: &str) -> Option<(RefKind, String)> {
    if let Some(sha) = as_sha(token) {
        return Some((RefKind::Sha, sha));
    }
    if let Some((head, _)) = split_range(token) {
        if looks_like_a_path(head) {
            return Some((RefKind::Lines, token.to_string()));
        }
    }
    if looks_like_a_path(token) {
        return Some((RefKind::Path, token.to_string()));
    }
    as_symbol(token).map(|s| (RefKind::Symbol, s))
}

/// A commit abbreviation: 7 to 40 lowercase hex characters, and — below the full length — at
/// least one digit, so that an English word spelled entirely in `abcdef` (`defaced`) is not
/// read as a sha.
fn as_sha(token: &str) -> Option<String> {
    if token.len() < 7 || token.len() > 40 {
        return None;
    }
    if !token.chars().all(|c| c.is_ascii_hexdigit())
        || token.chars().any(|c| c.is_ascii_uppercase())
    {
        return None;
    }
    if token.len() < 40 && !token.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(token.to_string())
}

/// `path:120`, `path:120-140` and the en dash a note's own typography uses.
fn split_range(token: &str) -> Option<(&str, usize)> {
    let (head, tail) = token.rsplit_once(':')?;
    if head.is_empty() {
        return None;
    }
    let tail = tail.replace('\u{2013}', "-");
    let (_, end) = tail
        .split_once('-')
        .unwrap_or((tail.as_str(), tail.as_str()));
    let end: usize = end.parse().ok()?;
    Some((head, end))
}

/// The extensions a citation may end in. A closed list, and it is the list that keeps `2.7` and
/// `e.g.` out of the report: an unknown suffix is not a filename, it is a word.
const EXTENSIONS: [&str; 24] = [
    "rs", "md", "toml", "sh", "json", "jsonl", "py", "js", "ts", "txt", "yaml", "yml", "sql", "db",
    "lock", "cfg", "html", "css", "c", "h", "go", "java", "log", "tsv",
];

fn has_extension(token: &str) -> bool {
    token.rsplit_once('.').is_some_and(|(stem, ext)| {
        // A stem, and not just an extension: a note discussing `2.7 s` or a `.md` file is not
        // citing a file called `.md`.
        !stem.is_empty() && EXTENSIONS.contains(&ext)
    })
}

/// **Whether a token is shaped like a path** — not whether it resolves. Every slash token is a
/// candidate, because `agent/notes-offer` is a branch and this is where it is recognised;
/// [`Ctx::is_a_citation`] is what decides whether one that resolved as nothing is a finding.
fn looks_like_a_path(token: &str) -> bool {
    if token.contains("::") || token.contains('(') || token.contains(')') || token.contains(' ') {
        return false;
    }
    token.starts_with('/')
        || token.starts_with("~/")
        || token.starts_with("./")
        || token.starts_with("../")
        || has_extension(token)
        || token.contains('/')
}

/// The name a symbol-like token is looked up by: `Corpus::nearest` as itself, a call as the
/// function it calls (`nearest(want: &str, …)` is looked up as `nearest`), and a snake_case
/// identifier as itself.
///
/// The snake_case rule requires an underscore and four characters, which is what keeps `main`,
/// `read`, `list` and `bash` out of it: those are words a note uses in prose, and every one of
/// them appears in the tree, so they would resolve and tell a reader nothing.
fn as_symbol(token: &str) -> Option<String> {
    if token.contains("::") {
        let parts: Vec<&str> = token.split("::").collect();
        if parts.iter().all(|p| is_ident(p)) {
            return Some(token.to_string());
        }
        return None;
    }
    if let Some((head, _)) = token.split_once('(') {
        if is_ident(head) {
            return Some(head.to_string());
        }
        return None;
    }
    let snake = token.contains('_')
        && token.chars().count() >= 4
        && token.starts_with(|c: char| c.is_ascii_lowercase())
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    snake.then(|| token.to_string())
}

/// The needles one symbol is looked up by: the token itself, and — for a qualified name — its
/// last segment, so that a note's `use`-alias is not read as a rename. See [`Ctx::symbol`].
fn symbol_needles(token: &str) -> Vec<String> {
    match token.rsplit_once("::") {
        Some((_, last)) if !last.is_empty() && last != token => {
            vec![token.to_string(), last.to_string()]
        }
        _ => vec![token.to_string()],
    }
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// --- git ----------------------------------------------------------------------------------

fn git(root: &Path, args: &[&str]) -> Option<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()
}

fn git_ok(root: &Path, args: &[&str]) -> bool {
    git(root, args).is_some_and(|o| o.status.success())
}

/// Whether git considers `root` a repository — the question every other git call depends on, so
/// a box with no git answers `false` here and every reference becomes `Uncheckable` rather than
/// `Unresolved`.
fn is_a_repo(root: &Path) -> bool {
    git(root, &["rev-parse", "--show-toplevel"]).is_some_and(|o| o.status.success())
}

/// A ref by that name, resolved to its full name — `agent/notes-offer` in a note is a branch,
/// and a note that names a branch is naming something the tree really holds.
fn git_ref(root: &Path, name: &str) -> Option<String> {
    if name.contains('.') && has_extension(name) {
        return None;
    }
    let out = git(root, &["rev-parse", "--verify", "--quiet", name])?;
    if !out.status.success() {
        return None;
    }
    let full = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!full.is_empty()).then_some(full)
}

/// How many times each symbol appears, in one `git grep` for all of them.
///
/// `-F` because a symbol is a string and not a pattern — `::` and `(` are regex metacharacters
/// and a search that quietly compiled them would answer a different question. `-o` so each
/// match arrives as its own record and the needle can be read back out of it, which is what
/// makes one call enough for twenty symbols. **`-n` is required beside `-o`**: without it git
/// prints `path:match` and drops the line number, which is a record this parser cannot read
/// (MEASURED, git 2.53).
///
/// **`None` is *git could not be asked*, and it is not the same answer as *no matches*.** A
/// `git grep` that found nothing exits 1, which is an answer; anything else (128, no git, a
/// broken index) is not, and a caller that read the second as the first would report every
/// symbol in every note as stale.
fn grep_symbols(repo: &Path, symbols: &[String]) -> Option<HashMap<String, usize>> {
    let mut args: Vec<&str> = vec!["grep", "-F", "-o", "-n", "--no-color", "-I"];
    let needles: Vec<String> = symbols.iter().map(|s| format!("-e{s}")).collect();
    args.extend(needles.iter().map(String::as_str));
    let out = git(repo, &args)?;
    if !out.status.success() && out.status.code() != Some(1) {
        return None;
    }
    let mut hits: HashMap<String, usize> = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut parts = line.splitn(3, ':');
        let (Some(_path), Some(_n), Some(found)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if symbols.iter().any(|s| s == found) {
            *hits.entry(found.to_string()).or_insert(0) += 1;
        }
    }
    Some(hits)
}

/// **The shorthand index** — where a note's own way of naming a file lands.
///
/// This corpus cites `tools/builtins/todo.rs` for `crates/tools/src/builtins/todo.rs`,
/// `daemon.rs:155` for one file of that name, and `tokencore/store.rs` for a path three
/// directories down. None of those is a path from any root, and MEASURED against the corpus
/// they were the largest single source of false staleness — which is the failure this whole
/// feature must not have.
///
/// So: a token whose components appear **in order** inside a tracked path is that path, when
/// exactly one tracked path contains them; when several do, nobody looked, and the outcome is
/// `Uncheckable` rather than a guess. One `git ls-files` answers it for every token at once.
/// `None` is *git could not be asked*, on [`grep_symbols`]' rule.
fn index_shorthand(repo: &Path, tokens: &[String]) -> Option<HashMap<String, Shorthand>> {
    let out = git(repo, &["ls-files"])?;
    if !out.status.success() {
        return None;
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    let files: Vec<&str> = listing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut index: HashMap<String, Shorthand> = HashMap::new();
    for token in tokens {
        let comps: Vec<&str> = token.split('/').filter(|c| !c.is_empty()).collect();
        if comps.is_empty() {
            continue;
        }
        let hits: Vec<&str> = files
            .iter()
            .copied()
            .filter(|f| components_in_order(f, &comps))
            .collect();
        match hits.len() {
            0 => {}
            1 => {
                index.insert(token.clone(), Shorthand::One(PathBuf::from(hits[0])));
            }
            n => {
                index.insert(token.clone(), Shorthand::Many(n));
            }
        }
    }
    Some(index)
}

/// Whether every component of `want` appears in `path`, in order and as whole components.
/// `tools/builtins/todo.rs` is in `crates/tools/src/builtins/todo.rs`; `tools/src/builtins` is
/// not, and neither is a component that only matches part of one.
fn components_in_order(path: &str, want: &[&str]) -> bool {
    let mut at = 0;
    for component in path.split('/') {
        if at < want.len() && component == want[at] {
            at += 1;
        }
    }
    at == want.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree with the two things every check needs: a file with lines, and a repository to ask
    /// git of. The fixture is a real repository because a fixture that is not one would test
    /// the `Uncheckable` path and nothing else.
    fn tree(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("letibot-references-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/tools/src")).expect("mkdir");
        std::fs::write(
            root.join("crates/tools/src/here.rs"),
            "one\ntwo\nthree\nfour\nfive\n",
        )
        .expect("write");
        // The symbols a note can cite, in a file of their own so the line counts above stay
        // where they are.
        std::fs::write(
            root.join("crates/tools/src/symbols.rs"),
            "fn git_ref() {}\nuse RefKind::Path;\n",
        )
        .expect("write");
        std::fs::write(root.join("AGENTS.md"), "# the operator's own\n").expect("write");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "t@t"]);
        git(&root, &["config", "user.name", "t"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "first"]);
        root
    }

    fn git(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn check_in(root: &Path, text: &str) -> ReferenceReport {
        let r = check(root, text);
        let _ = std::fs::remove_dir_all(root);
        r
    }

    /// The whole point, in one test: a note whose references are good and a note whose
    /// references are not are told apart, and the difference is the evidence.
    #[test]
    fn a_good_reference_resolves_and_a_gone_one_does_not() {
        let root = tree("resolve");
        let r = check_in(
            &root,
            "the checker is `crates/tools/src/here.rs` and the tree is `crates/tools/src/gone.rs`.",
        );
        let good = r
            .references
            .iter()
            .find(|x| x.text == "crates/tools/src/here.rs")
            .expect("the good path");
        assert!(good.outcome.is_resolved(), "{good:?}");
        assert_eq!(good.kind, RefKind::Path);
        let gone = r
            .references
            .iter()
            .find(|x| x.text == "crates/tools/src/gone.rs")
            .expect("the gone path");
        assert!(gone.outcome.is_unresolved(), "{gone:?}");
        assert!(!r.is_clean());
        assert_eq!(r.resolved().len(), 1);
    }

    /// A line range inside the file resolves; one past its end does not, and the evidence is
    /// the count.
    #[test]
    fn a_line_range_is_checked_against_the_files_own_length() {
        let root = tree("lines");
        let r = check_in(
            &root,
            "see `crates/tools/src/here.rs:3` and then `crates/tools/src/here.rs:900`.",
        );
        let inside = r
            .references
            .iter()
            .find(|x| x.text.ends_with(":3"))
            .expect("the range inside");
        assert!(inside.outcome.is_resolved(), "{inside:?}");
        assert_eq!(inside.kind, RefKind::Lines);
        let past = r
            .references
            .iter()
            .find(|x| x.text.ends_with(":900"))
            .expect("the range past the end");
        assert!(past.outcome.is_unresolved(), "{past:?}");
        assert!(past.outcome.why().contains("only 5 lines"), "{past:?}");
    }

    /// A sha on `HEAD` resolves; a sha that exists nowhere does not; and a sha that exists but
    /// is off `HEAD` resolves **and says which it is** — the note about an unlanded branch is
    /// accurate and the checker must not call it stale.
    #[test]
    fn a_sha_says_whether_it_is_on_head() {
        let root = tree("sha");
        let head = {
            let out = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["rev-parse", "HEAD"])
                .output()
                .expect("git");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        // A commit on another branch, never merged.
        git(&root, &["checkout", "-q", "-b", "side"]);
        std::fs::write(root.join("side.txt"), "x\n").expect("write");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "side"]);
        let off = {
            let out = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["rev-parse", "HEAD"])
                .output()
                .expect("git");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&root, &["checkout", "-q", "-"]);

        let text = format!("landed in `{head}`; `{off}`; and `deadbee1`.");
        let r = check_in(&root, &text);
        let on = r.references.iter().find(|x| x.text == head).expect("head");
        assert!(on.outcome.is_resolved(), "{on:?}");
        assert!(on.outcome.why().contains("on HEAD's history"), "{on:?}");
        let elsewhere = r
            .references
            .iter()
            .find(|x| x.text == off)
            .expect("off head");
        assert!(elsewhere.outcome.is_resolved(), "{elsewhere:?}");
        assert!(
            elsewhere.outcome.why().contains("NOT on HEAD"),
            "{elsewhere:?}"
        );
        let missing = r
            .references
            .iter()
            .find(|x| x.text == "deadbee1")
            .expect("missing");
        assert!(missing.outcome.is_unresolved(), "{missing:?}");
    }

    /// A symbol the tree holds resolves; one it does not is unresolved — and the needle for a
    /// call is the function's name, not the call's arguments.
    #[test]
    fn a_symbol_is_looked_up_by_its_name() {
        let root = tree("symbol");
        let r = check_in(
            &root,
            "`git_ref(name)` and `RefKind::Path` are here; `no_such_symbol_anywhere` is not.",
        );
        let call = r
            .references
            .iter()
            .find(|x| x.text == "git_ref")
            .expect("the call's name");
        assert!(call.outcome.is_resolved(), "{call:?}");
        assert_eq!(call.kind, RefKind::Symbol);
        let qualified = r
            .references
            .iter()
            .find(|x| x.text == "RefKind::Path")
            .expect("the qualified name");
        assert!(qualified.outcome.is_resolved(), "{qualified:?}");
        let missing = r
            .references
            .iter()
            .find(|x| x.text == "no_such_symbol_anywhere")
            .expect("the missing name");
        assert!(missing.outcome.is_unresolved(), "{missing:?}");
    }

    /// **The noise rule.** Prose that happens to contain a slash is not a reference, and a
    /// checker that reported it would be teaching its reader to ignore the report.
    #[test]
    fn prose_wearing_a_slash_is_not_a_reference() {
        let root = tree("noise");
        let r = check_in(
            &root,
            "either/or and read/write and host/port are words, and 2.7 is a number, and \
             `**Verbatim:**` is a label. `crates/tools/src` is a directory.",
        );
        for word in ["either/or", "read/write", "host/port", "2.7", "Verbatim"] {
            assert!(
                !r.references.iter().any(|x| x.text == word),
                "`{word}` was read as a reference: {:?}",
                r.references
            );
        }
        let dir = r
            .references
            .iter()
            .find(|x| x.text == "crates/tools/src")
            .expect("a three-segment token is a citation");
        assert!(dir.outcome.is_resolved(), "{dir:?}");
    }

    /// A branch name is a reference, and it resolves while the branch is there — the two-segment
    /// rule, which is what keeps `agent/notes-offer` out of the noise bin.
    #[test]
    fn a_branch_name_resolves_as_a_ref() {
        let root = tree("branch");
        git(&root, &["branch", "agent/some-work"]);
        let r = check_in(&root, "on `agent/some-work`, which is enqueued.");
        let b = r
            .references
            .iter()
            .find(|x| x.text == "agent/some-work")
            .expect("the branch");
        assert!(b.outcome.is_resolved(), "{b:?}");
        assert!(b.outcome.why().contains("a ref in"), "{b:?}");
    }

    /// A bare filename is looked for by name in the repository, so a note that names a file
    /// without its directory is not reported stale for the omission.
    #[test]
    fn a_bare_filename_is_looked_for_by_name() {
        let root = tree("bare");
        let r = check_in(
            &root,
            "the operator's own `AGENTS.md`, and `no-such-file.md`.",
        );
        let agents = r
            .references
            .iter()
            .find(|x| x.text == "AGENTS.md")
            .expect("AGENTS.md");
        assert!(agents.outcome.is_resolved(), "{agents:?}");
        let gone = r
            .references
            .iter()
            .find(|x| x.text == "no-such-file.md")
            .expect("the missing file");
        assert!(gone.outcome.is_unresolved(), "{gone:?}");
    }

    /// **No repository is not a stale reference.** The three-outcome rule, as a test: the paths
    /// still resolve (they need no git), the symbols and shas say *nobody looked*.
    #[test]
    fn without_a_repository_a_symbol_is_unchecked_rather_than_stale() {
        let root =
            std::env::temp_dir().join(format!("letibot-references-{}-norepo", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates")).expect("mkdir");
        std::fs::write(root.join("crates/here.rs"), "one\n").expect("write");
        let r = check(
            &root,
            "`crates/here.rs` and `some_symbol_here` and `3d5d273`.",
        );
        let _ = std::fs::remove_dir_all(&root);
        assert!(r.repo.is_none(), "a bare directory is not a repository");
        let path = r
            .references
            .iter()
            .find(|x| x.text == "crates/here.rs")
            .expect("the path");
        assert!(path.outcome.is_resolved(), "{path:?}");
        for text in ["some_symbol_here", "3d5d273"] {
            let r = r.references.iter().find(|x| x.text == text).expect(text);
            assert!(
                matches!(r.outcome, Outcome::Uncheckable(_)),
                "{text}: {r:?}"
            );
        }
        assert!(r.is_clean(), "nothing was found, so nothing is stale");
    }

    /// One reference is one line of the report however many times the note says it, and the
    /// line number is the first place it was said.
    #[test]
    fn a_note_that_cites_one_path_five_times_makes_one_reference() {
        let root = tree("dedupe");
        let r = check_in(
            &root,
            "`crates/tools/src/here.rs`\nand again `crates/tools/src/here.rs`\nand `crates/tools/src/here.rs`.",
        );
        let all: Vec<&Reference> = r
            .references
            .iter()
            .filter(|x| x.text == "crates/tools/src/here.rs")
            .collect();
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].line, 1);
    }

    /// **The workspace that is not the tree**, which is this box's own shape: the notes live in
    /// a workspace that holds the repositories rather than being one. Resolving against the
    /// workspace alone would call every path in every note stale.
    #[test]
    fn a_workspace_holding_a_repository_resolves_against_it() {
        let outer =
            std::env::temp_dir().join(format!("letibot-references-{}-outer", std::process::id()));
        let _ = std::fs::remove_dir_all(&outer);
        let repo = outer.join("letibot");
        std::fs::create_dir_all(repo.join("crates/tools/src")).expect("mkdir");
        std::fs::write(repo.join("crates/tools/src/here.rs"), "one\n").expect("write");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "first"]);

        let roots = tree_roots(&outer);
        assert_eq!(roots[0], outer, "the workspace is tried first");
        assert!(roots.contains(&repo), "{roots:?}");
        let r = check(&outer, "the checker is `crates/tools/src/here.rs`.");
        assert_eq!(r.repo.as_deref(), Some(repo.as_path()), "{:?}", r.repo);
        let path = r
            .references
            .iter()
            .find(|x| x.text == "crates/tools/src/here.rs")
            .expect("the path");
        assert!(path.outcome.is_resolved(), "{path:?}");
        assert!(path.outcome.why().contains("letibot/"), "{path:?}");
        let _ = std::fs::remove_dir_all(&outer);
    }

    /// A fenced block is a quotation, not a claim: code inside it is not a reference.
    #[test]
    fn a_fenced_code_block_is_not_read_as_references() {
        let root = tree("fence");
        let r = check_in(
            &root,
            "the rule is:\n\n```text\nverdict: accept | reject\ncrates/tools/src/gone.rs\n```\n\nand that is all.",
        );
        assert!(r.references.is_empty(), "{:?}", r.references);
    }

    /// The report is the evidence a reader acts on, so the unresolved ones come first and each
    /// carries the why.
    #[test]
    fn the_rendered_report_puts_the_unresolved_first_and_says_why() {
        let root = tree("render");
        let r = check_in(
            &root,
            "`crates/tools/src/gone.rs` is not here; `crates/tools/src/here.rs` is.",
        );
        let out = r.render();
        let gone = out.find("unresolved").expect("the unresolved line");
        let here = out
            .find("crates/tools/src/here.rs")
            .expect("the resolved line");
        assert!(gone < here, "unresolved first:\n{out}");
        assert!(out.contains("no such path"), "{out}");
    }

    /// **The note's own shorthand is followed, once.** `src/here.rs` is how this corpus cites
    /// `crates/tools/src/here.rs`, and a checker that read it as a path from the root reported
    /// every such citation stale — MEASURED, the largest single source of false staleness.
    #[test]
    fn a_notes_shorthand_is_followed_when_exactly_one_file_matches() {
        let root = tree("shorthand");
        let r = check_in(
            &root,
            "the checker is `src/here.rs` and `tools/src/here.rs`.",
        );
        for token in ["src/here.rs", "tools/src/here.rs"] {
            let got = r
                .references
                .iter()
                .find(|x| x.text == token)
                .unwrap_or_else(|| panic!("{token}: {:?}", r.references));
            assert!(got.outcome.is_resolved(), "{token}: {got:?}");
            assert!(got.outcome.why().contains("here.rs"), "{token}: {got:?}");
        }
    }

    /// **And it is followed only when the answer is one file.** Two files of one name is not an
    /// answer, and a checker that picked the first would be guessing at what the note meant.
    #[test]
    fn a_shorthand_that_matches_several_files_is_unchecked_rather_than_guessed() {
        let root = tree("ambiguous");
        std::fs::create_dir_all(root.join("crates/tui/src/cards")).expect("mkdir");
        std::fs::write(root.join("crates/tui/src/cards/here.rs"), "one\n").expect("write");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "a second here.rs"]);
        let r = check_in(&root, "`here.rs` is where it is.");
        let got = r
            .references
            .iter()
            .find(|x| x.text == "here.rs")
            .expect("the name");
        assert!(matches!(got.outcome, Outcome::Uncheckable(_)), "{got:?}");
        assert!(got.outcome.why().contains("2 files"), "{got:?}");
        assert!(r.is_clean(), "nobody looked is not staleness");
    }

    /// **A qualified name that is absent while its last segment is present is NOT stale.** A
    /// note writes `view::SettledDecision::kind` for a `use`-aliased path, and a checker that
    /// called that stale would be reporting its own lack of a name resolver.
    #[test]
    fn a_qualified_symbol_absent_as_written_is_unchecked_when_its_name_is_there() {
        let root = tree("alias");
        let r = check_in(
            &root,
            "`view::RefKind::Path` is the alias; `view::no_such_name_at_all` is not there.",
        );
        let alias = r
            .references
            .iter()
            .find(|x| x.text == "view::RefKind::Path")
            .expect("the aliased path");
        assert!(
            matches!(alias.outcome, Outcome::Uncheckable(_)),
            "{alias:?}"
        );
        assert!(alias.outcome.why().contains("module alias"), "{alias:?}");
        let gone = r
            .references
            .iter()
            .find(|x| x.text == "view::no_such_name_at_all")
            .expect("the missing name");
        assert!(gone.outcome.is_unresolved(), "{gone:?}");
        assert!(gone.outcome.why().contains("nor"), "{gone:?}");
    }

    /// **Slash commands, globs and templates are not paths.** MEASURED on this box's corpus:
    /// `/queue`, `/notes` and `/reseat` were read as absolute paths, and `*.md`,
    /// `crates/harnessd/src/*.rs` and `<slug>` as filenames — twelve findings, none of them a
    /// claim about where a file is.
    #[test]
    fn a_slash_command_a_glob_and_a_template_are_not_references() {
        let root = tree("shapes");
        let r = check_in(
            &root,
            "`/queue reset` and `/notes`; `*.md` and `crates/harnessd/src/*.rs`; and \
             `<worktree>/target`.",
        );
        assert!(r.references.is_empty(), "{:?}", r.references);
    }

    /// A miss names every root that was searched, so a reader can tell *not here* from *not
    /// looked for*.
    #[test]
    fn a_miss_names_every_root_that_was_searched() {
        let root = tree("roots");
        let r = check_in(&root, "`crates/tools/src/never.rs` is not here.");
        let got = r
            .references
            .iter()
            .find(|x| x.text == "crates/tools/src/never.rs")
            .expect("the miss");
        assert!(got.outcome.is_unresolved(), "{got:?}");
        assert!(
            got.outcome.why().contains(&root.display().to_string()),
            "{got:?}"
        );
    }
}
