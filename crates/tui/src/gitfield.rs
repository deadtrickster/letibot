//! **What the workspace's own repository says**, for the row the operator crosses.
//!
//! The ask is theirs, 2026-10-04: *"git status in the status line — look how leticl did it."*
//! leticl's half is `src/chrome.lisp:644` and this is the same field in the same place: **beside
//! the workspace path**, because that is the thing it is a fact about.
//!
//! **Ported fully, 2026-10-05** — the operator's row: *"port leticl git status line fully —
//! template, colors, config."* What was here first was the plain cousin (a branch, a `*`, the
//! ahead/behind counts); leticl's field is gitstatus's own segment vocabulary, a configurable
//! template, and a colour per segment, and that is what this is now, piece by piece:
//!
//! * **the facts, not the text** (`GitState`) — because the format decides the text and a
//!   preference may change the format, so a cache of strings would be a cache of somebody's old
//!   choice;
//! * **gitstatus's segments and glyphs** — `⇣` behind, `⇡` ahead, `*` stashes, `~` conflicts,
//!   `+` staged, `!` unstaged, `?` untracked, the branch or `@oid` when detached — so a reader
//!   who knows that prompt knows this row;
//! * **a template** (`%b%d%a%s%m%~%+%!%?` by default), `git_format` in `head.toml` to override,
//!   and `/config`'s row to cycle three stops — the row the operator looked for it on;
//! * **one colour per segment, chosen to say what the segment says** — green branch when the
//!   tree is clean, yellow when it is not, red and bold conflicts because nothing else on that
//!   row is a demand, dim untracked because it is usually noise.
//!
//! **A repository this cannot read is ABSENT, not empty.** The state is `None` and the row draws
//! its blank slot — leticl's words, and they are the rule this file keeps: *"a header that said
//! `main` over a directory that is not a repository would be a lie in the one row nobody
//! checks."*
//!
//! **Read from the LOOP, never from a paint.** This spawns a process, and the rule is the dash
//! collectors': a measurement never runs where the frame is drawn. `App::refresh_git` is called
//! by `driver::tick`, and [`GIT_REFRESH_MS`] is leticl's own interval — chosen to hold the process
//! rate down rather than because a frame could not afford it. It is wrapped in coreutils'
//! `timeout`, because a `wait` with no deadline is what turns a slow mount into a frozen head.

use std::process::Command;

/// How many columns a segment's text takes — the same ANSI-aware measure the header
/// centres with (`letibot_ui::width`), because a piece's width is what fitting spends
/// and the pieces are plain text anyway; kept as a local so the module has no renderer
/// import for one `usize`.
fn visible_width(s: &str) -> usize {
    letibot_ui::width::width(s)
}

/// How long a reading is trusted before the loop takes another.
pub const GIT_REFRESH_MS: u64 = 2_000;

/// How long one reading may take before it is killed and treated as absent.
pub const GIT_TIMEOUT_SECS: u64 = 5;

/// **The default format: gitstatus's segments, in gitstatus's order, concatenated.**
///
/// The placeholders ARE the glyphs' meaning, which is the whole mnemonic: `%b` branch (or `@oid`
/// when detached), `%d` behind, `%a` ahead, `%s` stashes, `%m` the action in progress, `%~`
/// conflicts, `%+` staged, `%!` unstaged, `%?` untracked. `%%` is a literal per cent and anything
/// else is literal text. `git_format` in the head's preferences overrides it — one line, and the
/// segments it does not mention are simply not drawn.
pub const GIT_FORMAT_DEFAULT: &str = "%b%d%a%s%m%~%+%!%?";

/// **One reading of the repository, as FACTS and not as text** — leticl's `%git-parts`.
///
/// The counts are `Some` only when there is something to say (leticl keeps zero out on purpose:
/// a segment at zero is a segment the format does not draw, and `?0` on every clean tree would be
/// noise), and `branch` already carries the detached spelling — `@` plus the first eight of the
/// oid, the width gitstatus itself prints — because two consumers would otherwise spell it two
/// ways.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitState {
    /// The branch name, or `@oid` (eight) when detached, or `@` alone when detached with no oid.
    pub branch: String,
    pub detached: bool,
    pub behind: Option<usize>,
    pub ahead: Option<usize>,
    pub stash: Option<usize>,
    /// rebase / merge / cherry-pick / revert / bisect — from the marker files, not from
    /// `git status`, which does not report one in porcelain at all.
    pub action: Option<&'static str>,
    pub conflict: Option<usize>,
    pub staged: Option<usize>,
    pub unstaged: Option<usize>,
    pub untracked: Option<usize>,
}

/// **The style a segment is painted with** — one role per segment, and the roles are leticl's
/// `+git-styles+` exactly: chosen to say what the segment SAYS rather than to be pretty.
///
/// A branch is green when the tree is clean and yellow when it is not — the one fact a person
/// reads at a glance — staged work is green, unstaged is yellow, conflicts are red and bold
/// because nothing else on that row is a demand, and untracked files are dim because they are
/// usually noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitRole {
    BranchClean,
    BranchDirty,
    Behind,
    Ahead,
    Stash,
    Action,
    Conflict,
    Staged,
    Unstaged,
    Untracked,
}

/// **`git status --porcelain=v2 --branch --show-stash` as the facts** — leticl's `%git-parts`.
///
/// The v2 shape this reads, measured on this checkout:
///
/// ```text
/// # branch.oid 31f30adfa3266f3ee47186ae4a33f22a43af50ce
/// # branch.head main
/// # branch.upstream origin/main
/// # branch.ab +0 -0
/// # stash 2
/// 1 .M N... 100644 100644 100644 aaa bbb src/app.rs
/// u N... 100644 100644 100644 aaa bbb src/conflict.rs
/// ? src/untracked.rs
/// ```
///
/// Every `1 XY`/`2 XY` line is one changed path: **X is index-vs-HEAD and Y is
/// workdir-vs-index**, so a `.` in X with an `M` in Y is unstaged work and the reverse is staged
/// — which is how `%+` and `%!` come apart, the count letibot's first field collapsed into one
/// `*`. `u` lines are unmerged paths and `?` lines untracked ones. `branch.ab` and
/// `branch.upstream` are ABSENT on a branch with no upstream — which is a fact about the branch,
/// not about the repository, so their absence is not an error either. `# stash N` is only printed
/// under `--show-stash`, which is why the reader's argv carries it.
///
/// `None` when the text names no branch at all — an empty reading, or git's own complaint, both
/// of which are *this is not a repository I can read* rather than *a repository with nothing in
/// it*.
pub fn git_parts(porcelain: &str, action: Option<&'static str>) -> Option<GitState> {
    let mut head: Option<String> = None;
    let mut oid: Option<String> = None;
    let mut ab: Option<(u64, u64)> = None;
    let mut stash: Option<usize> = None;
    let mut staged = 0usize;
    let mut unstaged = 0usize;
    let mut untracked = 0usize;
    let mut conflict = 0usize;
    for line in porcelain.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            head = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.oid ") {
            oid = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            let mut it = rest.split_whitespace();
            let ahead = it
                .next()
                .and_then(|s| s.strip_prefix('+'))
                .and_then(|n| n.parse::<u64>().ok());
            let behind = it
                .next()
                .and_then(|s| s.strip_prefix('-'))
                .and_then(|n| n.parse::<u64>().ok());
            if let (Some(a), Some(b)) = (ahead, behind) {
                ab = Some((a, b));
            }
        } else if let Some(rest) = line.strip_prefix("# stash ") {
            stash = rest.trim().parse::<usize>().ok();
        } else if let Some("1" | "2") = line.get(0..1) {
            // **X is index-vs-HEAD, Y is workdir-vs-index** — a `.` means that side is clean.
            // The guard is the PREFIX'S OWN LENGTH: `line.get(3..4)` on a shorter line is
            // `None`, not `false`, which is the bounds error leticl measured on a three
            // character `? h` line.
            if line.get(2..3).is_some_and(|x| x != ".") {
                staged += 1;
            }
            if line.get(3..4).is_some_and(|y| y != ".") {
                unstaged += 1;
            }
        } else if line.starts_with('u') {
            conflict += 1;
        } else if line.starts_with('?') {
            untracked += 1;
        }
    }
    let head = head?;
    let (branch, detached) = if head == "(detached)" {
        // **A detached HEAD is not branch-less.** gitstatus shows the commit and not the
        // branch, `@` plus the first eight of the oid — the width its own README prints.
        let at = oid
            .as_deref()
            .map(|o| o.chars().take(8).collect::<String>());
        (format!("@{}", at.unwrap_or_default()), true)
    } else {
        (head, false)
    };
    let pos = |n: u64| (n > 0).then_some(n as usize);
    Some(GitState {
        branch,
        detached,
        behind: ab.and_then(|(_, b)| pos(b)),
        ahead: ab.and_then(|(a, _)| pos(a)),
        stash: stash.filter(|n| *n > 0),
        action,
        conflict: (conflict > 0).then_some(conflict),
        staged: (staged > 0).then_some(staged),
        unstaged: (unstaged > 0).then_some(unstaged),
        untracked: (untracked > 0).then_some(untracked),
    })
}

/// **One segment's `(text, role)`** — leticl's `%git-segment`. Glyphs are gitstatus's own.
fn git_segment(state: &GitState, slot: GitSlot) -> Option<(String, GitRole)> {
    let dirty = state.staged.is_some()
        || state.unstaged.is_some()
        || state.untracked.is_some()
        || state.conflict.is_some();
    match slot {
        GitSlot::Branch => Some((
            state.branch.clone(),
            if dirty {
                GitRole::BranchDirty
            } else {
                GitRole::BranchClean
            },
        )),
        GitSlot::Action => state.action.map(|a| (a.to_string(), GitRole::Action)),
        GitSlot::Behind => state.behind.map(|n| (format!("⇣{n}"), GitRole::Behind)),
        GitSlot::Ahead => state.ahead.map(|n| (format!("⇡{n}"), GitRole::Ahead)),
        GitSlot::Stash => state.stash.map(|n| (format!("*{n}"), GitRole::Stash)),
        GitSlot::Conflict => state.conflict.map(|n| (format!("~{n}"), GitRole::Conflict)),
        GitSlot::Staged => state.staged.map(|n| (format!("+{n}"), GitRole::Staged)),
        GitSlot::Unstaged => state.unstaged.map(|n| (format!("!{n}"), GitRole::Unstaged)),
        GitSlot::Untracked => state
            .untracked
            .map(|n| (format!("?{n}"), GitRole::Untracked)),
    }
}

/// The format's placeholders — leticl's `+git-slots+`, in the order the default spells them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitSlot {
    Branch,
    Behind,
    Ahead,
    Stash,
    Action,
    Conflict,
    Staged,
    Unstaged,
    Untracked,
}

impl GitSlot {
    /// `%b` and friends. An enum rather than the string, so a template's `%x` cannot be read as
    /// a slot this build does not have.
    fn of(two: &str) -> Option<GitSlot> {
        Some(match two {
            "%b" => GitSlot::Branch,
            "%d" => GitSlot::Behind,
            "%a" => GitSlot::Ahead,
            "%s" => GitSlot::Stash,
            "%m" => GitSlot::Action,
            "%~" => GitSlot::Conflict,
            "%+" => GitSlot::Staged,
            "%!" => GitSlot::Unstaged,
            "%?" => GitSlot::Untracked,
            _ => return None,
        })
    }
}

/// **STATE as the pieces the FORMAT asks for** — leticl's `%git-pieces`.
///
/// Literals are attached to the piece they PRECEDE — a space before a mark travels with the mark
/// — so that fitting drops whole pieces and never half of one. A literal that precedes a segment
/// with nothing to say is dropped with it: a separator whose mark is absent is a dangling space.
/// `%%` is a literal per cent; a `%` that names no slot is a literal per cent too, and a trailing
/// `%` is literal rather than an error. A trailing literal after the last piece attaches to that
/// piece's text (and is dropped when there is no piece to attach to) — the same rule, read from
/// the other end.
pub fn git_pieces(state: &GitState, format: &str) -> Vec<(String, GitRole)> {
    let chars: Vec<char> = format.chars().collect();
    let mut out: Vec<(String, GitRole)> = Vec::new();
    let mut pending = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '%' {
            pending.push(chars[i]);
            i += 1;
            continue;
        }
        let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
        if let Some(slot) = GitSlot::of(&two) {
            if let Some((text, role)) = git_segment(state, slot) {
                pending.push_str(&text);
                out.push((std::mem::take(&mut pending), role));
            } else {
                // The segment has nothing to say: its literal goes with it.
                pending.clear();
            }
            i += 2;
        } else {
            // `%%` is a literal per cent, and a trailing `%` is literal too.
            pending.push('%');
            i += if two.len() == 2 { 2 } else { 1 };
        }
    }
    if !pending.is_empty()
        && let Some(last) = out.last_mut()
    {
        last.0.push_str(&pending);
    }
    out
}

/// **The longest PREFIX of the pieces that fits in ROOM columns** — leticl's `%git-fit`.
///
/// The field degrades by DELETION, like every other thing on this row: the branch is the floor
/// and the marks fall off its right in gitstatus's own order, so a narrow screen loses `?4` and
/// not the branch. A field dropped whole is the behaviour this replaces.
pub fn git_fit(pieces: &[(String, GitRole)], room: usize) -> Vec<&(String, GitRole)> {
    let mut out = Vec::new();
    let mut used = 3usize; // the " (" and ")" the row wraps the field in
    for piece in pieces {
        let w = visible_width(&piece.0);
        if used + w + 1 > room {
            break;
        }
        used += w;
        out.push(piece);
    }
    out
}

/// **An action in progress, from the marker files git itself leaves.**
///
/// `git status --porcelain` does not report one at all — the long format says *rebase in progress*
/// and the porcelain says nothing — so the markers are the only source, which is what leticl's
/// `%git-action` found too. A `.git` that is a FILE is a linked worktree: its markers live beside
/// the real repository, and this returns `None` for it. An absence rather than a guess.
pub fn git_action(workspace: &str) -> Option<&'static str> {
    let git = std::path::Path::new(workspace).join(".git");
    if !git.is_dir() {
        return None;
    }
    let there = |n: &str| git.join(n).exists();
    if there("rebase-merge") || there("rebase-apply") {
        Some("rebase")
    } else if there("MERGE_HEAD") {
        Some("merge")
    } else if there("CHERRY_PICK_HEAD") {
        Some("cherry-pick")
    } else if there("REVERT_HEAD") {
        Some("revert")
    } else if there("BISECT_LOG") {
        Some("bisect")
    } else {
        None
    }
}

/// **One directory down, when the repository is not the workspace itself** — the nested layout.
///
/// The operator's own box is the case: the session lives in `Projects/letibot` and the repository
/// in `Projects/letibot/letibot` — **the same name one level down** — so neither the git field nor
/// the todos pane found anything at the workspace, and both drew nothing (their honest absence,
/// working exactly as designed on a layout nobody had told them about).
///
/// The rule, in order, and it is a RESOLUTION rather than a guess at every step:
///
/// 1. **the workspace itself**, when it is a repository (`.git` present — a directory, or a file
///    for a linked worktree) or carries a `TODO.md` — the ordinary layout, unchanged;
/// 2. **the same-named child** (`<workspace>/<basename>`), when that is a repository — the nested
///    naming above, and the reason this function exists;
/// 3. **the one repository child**, when exactly one child is one — a parent holding two
///    repositories is a parent nobody chose between, and picking one would be the absent-field
///    rule broken by the very code that exists to keep it;
/// 4. otherwise **the workspace unchanged**, so whatever reads it fails as it always did and the
///    field stays absent rather than wrong.
///
/// A workspace that is merely *inside* a repository needs none of this: `git` walks up on its
/// own, and step 1 not matching simply leaves it to git.
///
/// Shared by the git field and the todos pane so the two cannot disagree about which directory is
/// the project — a branch from one tree and a `TODO.md` from another would each be true alone.
pub fn project_dir(ws: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(ws);
    let is_repo = |d: &std::path::Path| d.join(".git").exists();
    if dir.join("TODO.md").is_file() || is_repo(dir) || dir.read_dir().is_err() {
        return dir.to_path_buf();
    }
    // The same-named child first: nested naming is a convention, not a coincidence.
    let own_name = dir.file_name().map(std::ffi::OsStr::to_owned);
    if let Some(name) = own_name
        && is_repo(&dir.join(&name))
    {
        return dir.join(name);
    }
    // Then a single repository child — and only one. Two is a choice this function does not make.
    let repos: Vec<std::path::PathBuf> = dir
        .read_dir()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| is_repo(p))
        .collect();
    match repos.as_slice() {
        [only] => only.clone(),
        _ => dir.to_path_buf(),
    }
}

/// **One reading of the workspace, or `None`** — leticl's `%git-refresh`, without its cache: the
/// caller is the loop, which has its own interval and its own cache (`App::git_read`).
///
/// **The argv is leticl's `%git-command` verbatim** — `41d8935`, "the reader's argv is back, in
/// v2": `--no-optional-locks` is the one flag that is not about WHAT is read — a plain
/// `git status` may take the index lock and write the refreshed index back, a READER writing to
/// the repository it is reporting on, which is exactly what an indicator must not do — and
/// `--show-stash` is where gitstatus's `%s` segment comes from. The process call is kept apart
/// from the parse so the parse is testable without a repository.
///
/// **The action's markers are read from the PROJECT directory**, not the workspace: a nested
/// layout keeps them one level down with the repository, and the old call here passed the
/// workspace — an action nobody could ever see on this box's own layout.
pub fn read_state(workspace: &str) -> Option<GitState> {
    if workspace.is_empty() {
        return None;
    }
    // **Resolved, not assumed** — the workspace may be the parent of the repository rather than
    // the repository (`project_dir`), which is the nested layout both this field and the todos
    // pane were drawing nothing over.
    let dir = project_dir(workspace);
    let out = Command::new("timeout")
        .arg(GIT_TIMEOUT_SECS.to_string())
        .arg("git")
        .arg("-C")
        .arg(&dir)
        // A reader writes nothing to the repository it reports on.
        .arg("--no-optional-locks")
        .args(["status", "--porcelain=v2", "--branch", "--show-stash"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let action = git_action(dir.to_str()?);
    git_parts(&text, action)
}

/// **The field as one plain string through the DEFAULT format** — what the tests assert, and the
/// shape the first cut of this field drew. No colours, no fitting: the text and nothing else.
pub fn git_text(state: &GitState) -> String {
    git_pieces(state, GIT_FORMAT_DEFAULT)
        .into_iter()
        .map(|(t, _)| t)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The README's own example state, one fixture every reader of a segment shares: the
    /// gitstatus table itself — one of everything, so a missing glyph is a wrong line, not a
    /// quiet row.
    fn everything() -> GitState {
        GitState {
            branch: "master".into(),
            detached: false,
            behind: Some(1),
            ahead: Some(2),
            stash: Some(5),
            action: Some("merge"),
            conflict: Some(6),
            staged: Some(7),
            unstaged: Some(8),
            untracked: Some(9),
        }
    }

    #[test]
    fn a_clean_branch_is_its_name() {
        let clean = "# branch.oid 31f30adfa3266f3ee47186ae4a33f22a43af50ce\n# branch.head main\n\
                     # branch.upstream origin/main\n# branch.ab +0 -0\n";
        let state = git_parts(clean, None).unwrap();
        assert_eq!(git_text(&state), "main");
        // A zero ahead/behind is NOTHING to say, not `⇣0⇡0` on every clean tree.
        assert_eq!(state.behind, None);
        assert_eq!(state.ahead, None);
    }

    /// **Three facts, three marks, and the marks are the vocabulary** — the operator asked for the
    /// field, and a field that showed `main` for a tree with nine changed files would be the row
    /// lying politely. The first cut of this field said `main*` for all of it; the port says
    /// which KIND of dirty, gitstatus's own separation: X is staged, Y is unstaged.
    #[test]
    fn a_dirty_branch_separates_staged_from_unstaged() {
        let dirty = "# branch.oid abc1234\n# branch.head main\n# branch.upstream origin/main\n\
                     # branch.ab +2 -1\n# stash 3\n1 .M N... 100644 100644 100644 aaa bbb src/app.rs\n\
                     1 M. N... 100644 100644 100644 ccc ddd src/lib.rs\n\
                     1 MM N... 100644 100644 100644 eee fff src/both.rs\n\
                     ? src/untracked.rs\nu N... 100644 100644 100644 ggg hhh src/conflict.rs\n";
        let state = git_parts(dirty, None).unwrap();
        assert_eq!(
            git_text(&state),
            "main⇣1⇡2*3~1+2!2?1",
            "gitstatus's segments, in gitstatus's order: behind, ahead, stash, conflicts,\
             staged, unstaged, untracked"
        );
        // A branch with no upstream at all: no `branch.ab`, no `branch.upstream`, still a branch.
        let lonely = "# branch.oid abc1234\n# branch.head feature/thing\n";
        assert_eq!(
            git_text(&git_parts(lonely, None).as_ref().unwrap()),
            "feature/thing"
        );
    }

    /// **A detached HEAD says which commit**, because `(detached)` alone is a name with no content
    /// — `@` plus the first eight of the oid, the width gitstatus itself prints.
    #[test]
    fn a_detached_head_is_the_commit_at_gitstatuses_width() {
        let detached = "# branch.oid 31f30adfa3266f3ee47186ae4a33f22a43af50ce\n\
                        # branch.head (detached)\n";
        let state = git_parts(detached, None).unwrap();
        assert!(state.detached);
        assert_eq!(state.branch, "@31f30adf");
    }

    /// **An action in progress is a segment, from the marker files** — `%m`, magenta and bold,
    /// between the stash and the conflicts: gitstatus's own order.
    #[test]
    fn an_action_is_a_segment_between_the_stash_and_the_conflicts() {
        let dirty = "# branch.oid abc1234\n# branch.head main\n# stash 1\n1 .M N... a b c\n";
        let state = git_parts(dirty, Some("rebase")).unwrap();
        assert_eq!(git_text(&state), "main*1rebase!1");
    }

    /// **The template decides the text** — the port's own point. Segments the template does not
    /// name are not drawn; literals travel with the piece they precede; `%%` is a per cent; an
    /// unknown `%x` is a literal per cent; a literal before an ABSENT segment is dropped with it.
    #[test]
    fn the_format_decides_the_text() {
        let state = everything();
        // A spaced format: the space travels with the mark that follows it. leticl's preset
        // verbatim — `%b %!%+` — whose space sits before `%!` only, so `+7` follows unspaced.
        let pieces = git_pieces(&state, "%b %!%+");
        assert_eq!(
            pieces.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(),
            vec!["master", " !8", "+7"]
        );
        // The branch alone.
        assert_eq!(
            git_pieces(&state, "%b")
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["master"]
        );
        // A literal before an absent segment is dropped with it — and a literal AFTER one
        // travels with the next segment, which is the same rule read from the other side:
        // literals attach to the piece they precede, so the " (" before the absent stash
        // goes, and the ") " that follows attaches to the conflict mark.
        let no_stash = GitState {
            stash: None,
            ..everything()
        };
        assert_eq!(
            git_pieces(&no_stash, "%b (%s) %~")
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["master", ") ~6"],
            "the literal before the absent stash went with it; the one after did not"
        );
        // `%%` and an unknown slot — both are a literal per cent, and the unknown slot's
        // letter is CONSUMED with it (leticl skips two, the `%` and the letter).
        assert_eq!(
            git_pieces(&state, "%% %z%b")
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["% %master"]
        );
        // A trailing literal attaches to the last piece — the same rule, read from the other end.
        assert_eq!(
            git_pieces(&state, "%b!")
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["master!"]
        );
    }

    /// **The branch is the floor** — fitting drops whole pieces from the right, so a narrow
    /// screen loses `?9` and not the branch.
    #[test]
    fn fitting_drops_whole_pieces_and_keeps_the_branch() {
        let pieces = git_pieces(&everything(), GIT_FORMAT_DEFAULT);
        let full: Vec<&str> = git_fit(&pieces, 80).iter().map(|p| p.0.as_str()).collect();
        assert_eq!(
            full,
            vec!["master", "⇣1", "⇡2", "*5", "merge", "~6", "+7", "!8", "?9"]
        );
        // Room for the branch and the first two marks only.
        let tight: Vec<&str> = git_fit(&pieces, 14).iter().map(|p| p.0.as_str()).collect();
        assert_eq!(tight, vec!["master", "⇣1", "⇡2"]);
        // Not even room for the branch: nothing, not a truncated half of one.
        assert!(git_fit(&pieces, 8).is_empty());
    }

    /// **The branch's colour is the one fact a person reads at a glance** — clean green, dirty
    /// yellow — and it is dirty on ANY of the four work counts, untracked included.
    #[test]
    fn the_branch_role_says_whether_the_tree_is_clean() {
        let clean = git_parts(
            "# branch.oid abc1234\n# branch.head main\n# branch.ab +0 -0\n",
            None,
        )
        .unwrap();
        assert_eq!(
            git_pieces(&clean, "%b")[0].1,
            GitRole::BranchClean,
            "a clean tree is green"
        );
        for mutate in [
            |s: &mut GitState| s.staged = Some(1),
            |s: &mut GitState| s.unstaged = Some(1),
            |s: &mut GitState| s.untracked = Some(1),
            |s: &mut GitState| s.conflict = Some(1),
        ] {
            let mut dirty = clean.clone();
            mutate(&mut dirty);
            assert_eq!(
                git_pieces(&dirty, "%b")[0].1,
                GitRole::BranchDirty,
                "any work in the tree yellows the branch"
            );
        }
    }
}
