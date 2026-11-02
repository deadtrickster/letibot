//! The parameter expansions whose value this call already fixes, written back into the
//! command text before the grammar reads it.
//!
//! ```text
//! cd /home/dead/Projects/… && export CARGO_TARGET_DIR=$PWD/target && cargo test --offline -p …
//! ```
//!
//! Resolved: `$PWD` (after a literal `cd`), `$HOME`, `$(pwd)`, a name this command sets
//! earlier as its own statement or by `export`/`declare`/`typeset`/`readonly`, a name the
//! session's shell holds ([`crate::intent::env`]), and the shell's own numbers ([`NUMERIC`]).
//! Everything else stays unresolved, because a value goes back only when the shell would
//! deliver it as exactly one plain word ([`as_one_word`]), and a credential the session
//! holds is never written into the reading - the post-substitution text is the approval key.
//! The text is rewritten rather than the words patched because everything downstream reads
//! the [`shell::Normalised`].

use letibot_code::shell::{self, Construct};

use super::Surroundings;
use super::env::{self, From};

/// How many times the text is re-read.
///
/// A value that is itself an expansion (`export A=x; export B=$A; cat $B/f`) needs the
/// round after the one that read it; each round removes a construct or makes no progress
/// and stops, so this bounds a plausible chain rather than a budget.
const ROUNDS: usize = 8;

/// The shell's own numbers: expansions whose value is a count, a status or a pid.
///
/// A number is not a place: refusing the command instead costs scope, because an
/// unresolved word widens the action to `Region::Unbounded` and no grant can cover that.
/// `$@`, `$*`, `$1` and a name nobody assigned can be paths, so they stay unresolved.
const NUMERIC: &[&str] = &[
    // The special parameters. `$#` is a count, `$$` and `$!` pids, `$?` a status.
    "?",
    "$",
    "!",
    "#",
    // The shell's counter and identity variables.
    "RANDOM",
    "LINENO",
    "SECONDS",
    "BASHPID",
    "PPID",
    "UID",
    "EUID",
    "SHLVL",
    "EPOCHSECONDS",
    "EPOCHREALTIME",
    "PIPESTATUS",
    "BASH_SUBSHELL",
];

/// What a numeric expansion is written back as. A number, because the value IS one, and
/// the alternative - leaving the word unresolved - claims a command whose program,
/// arguments and paths are all read is unreadable. See [`NUMERIC`].
const NUMERIC_WORD: &str = "0";

/// Whether this expansion's value is one of the shell's numbers.
fn numeric(name: &str) -> bool {
    NUMERIC.contains(&name)
}

/// Whether the expansion is exactly one name's value and nothing done to it.
///
/// `${#X}`, `${X%.*}`, `${X/a/b}`, `${X:0:3}`, `${!X}` and `${arr[0]}` all report the same
/// `variable_name` as `${X}`, with the transform in an `operator` or `subscript` child, so
/// the check is lexical against the text rather than on the reported name.
fn plain(text: &str, name: &str) -> bool {
    text == format!("${name}") || text == format!("${{{name}}}")
}

/// A number the shell keeps, read from the TEXT of the expansion.
///
/// `${PIPESTATUS[0]}` is allowed because every element of a numeric array is a number; any
/// other subscript is one element of a value this module does not model, and a transform
/// (`${#X}`, `${!X}`) is never a number.
fn numeric_text(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('$') else {
        return false;
    };
    let Some(inner) = rest.strip_prefix('{').and_then(|t| t.strip_suffix('}')) else {
        return numeric(rest);
    };
    if numeric(inner) {
        return true;
    }
    let Some((name, index)) = inner.split_once('[') else {
        return false;
    };
    let Some(index) = index.strip_suffix(']') else {
        return false;
    };
    numeric(name)
        && !index.is_empty()
        && index
            .chars()
            .all(|c| c.is_ascii_digit() || c == '@' || c == '*')
}

/// The command with every parameter it already fixes written out, and what it cost.
///
/// The text is the reading and the notes are what the card says about where a value came
/// from; a caller that used one without the other would be making the claim this module is
/// careful about.
#[derive(Debug, Default)]
pub(super) struct Fixed {
    pub text: String,
    pub notes: Vec<String>,
    /// Where the value that resolved `$PATH` came from, when one did.
    ///
    /// Read by `intent::apply_path`'s third outcome only: a stage under a `PATH` this
    /// command's own text fixed was already readable and must not become a refusal, while
    /// one under a `PATH` the session supplied may stay unread - the fail-closed end.
    pub path: Option<Source>,
}

/// Where a value came from. [`Source::Command`] is this text, which needs no
/// justification; the other two are the session's environment ([`crate::intent::env`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    /// An assignment or an `export` in this command, before this point.
    Command,
    /// The pairs every command of this session is spawned with.
    Harness,
    /// The session's shell, in a trailer after a line.
    Shell,
}

impl Source {
    fn phrase(self) -> &'static str {
        match self {
            Source::Command => "",
            Source::Harness => From::Harness.as_phrase(),
            Source::Shell => From::Shell.as_phrase(),
        }
    }

    /// How the card says which `PATH` this was - see `intent::apply_path`.
    pub(super) fn path_phrase(self) -> &'static str {
        match self {
            Source::Command => "this command set",
            Source::Harness => "every command of this session is given",
            Source::Shell => "this session's shell reported",
        }
    }
}

/// The command with every parameter it already fixes written out.
///
/// Returns the (possibly unchanged) text and a note per substitution, for the audit row: a
/// card whose regions were placed against a directory the person never typed should say
/// where that directory came from.
pub(super) fn resolve(command: &str, env: &Surroundings) -> Fixed {
    resolve_without(command, env, &[])
}

/// The same, with one name deliberately left unresolved.
///
/// One caller: [`crate::intent::Baseline::of_command_with`], which asks again with `PATH`
/// left in place when it cannot name the file a bare stage would then run - so the command
/// ends up exactly as unread as it was, which is what makes the feature safe.
pub(super) fn resolve_without(command: &str, env: &Surroundings, skip: &[&str]) -> Fixed {
    let mut text = command.to_string();
    let mut notes: Vec<String> = Vec::new();
    // A value that is held and not written into the reading still has to be explained, or
    // the card's silence would be indistinguishable from the session not holding it.
    let mut refused: Vec<String> = Vec::new();
    let mut path: Option<Source> = None;
    for _ in 0..ROUNDS {
        let n = shell::normalise(&text);
        if n.is_resolved() {
            break;
        }
        let timeline = timeline(&n, env);
        // Back to front, so a splice cannot move the span of one still to be done. The
        // spans are the grammar's byte offsets: `'$PWD'` in single quotes is a literal the
        // grammar knows, and a search for the text would rewrite it.
        let mut splices: Vec<(usize, usize, String, String, Option<Source>)> = Vec::new();
        for u in &n.unresolved {
            let written = n.source.get(u.span.start..u.span.end).unwrap_or("");
            // `$(pwd)` is `$PWD`, which [`timeline`] already has, and the one substitution
            // whose value IS in the text. Nothing else inside a `$(...)` is touched: its
            // inner command is read on its own terms and its output is a runtime fact.
            if u.construct == Construct::CommandSubstitutionValue
                && is_pwd(written)
                && let Some((value, _)) = value_at(&timeline, env, "PWD", u.span.start)
                && let Some(word) = as_one_word(&value)
            {
                splices.push((
                    u.span.start,
                    u.span.end,
                    format!("`$(pwd)` is `{word}`"),
                    word.to_string(),
                    None,
                ));
                continue;
            }
            let Construct::ParameterExpansion { name } = &u.construct else {
                continue;
            };
            // Only the expansions that ARE the value of one name, which [`plain`] is the one
            // place to decide: a transform added to the grammar must not silently become a
            // value here.
            // What the text states comes first: `X=/tmp; cat $X` is about `/tmp`, and no
            // rule about numbers may overrule it. The session's environment is consulted
            // only where the command says nothing, which [`value_at`] explains.
            if plain(written, name)
                && !skip.contains(&name.as_str())
                && let Some((value, from)) = value_at(&timeline, env, name, u.span.start)
                && let Some(word) = as_one_word(&value)
            {
                // A credential is held and not written down: the post-substitution text is
                // the approval key every row stores and what the oracle is shown, so a token
                // spliced here is a token in the store. The card says why.
                if from != Source::Command && env.session.is_secret(name) {
                    let note = format!(
                        "`${name}` is `{}` — {}, and it looks like a credential, so it is not \
                         written into this reading",
                        env.session.masked(name).unwrap_or_default(),
                        from.phrase()
                    );
                    if !refused.contains(&note) {
                        refused.push(note);
                    }
                    continue;
                }
                let note = match from {
                    Source::Command => format!("`${name}` is `{word}`"),
                    other => format!("`${name}` is `{word}` — {}", other.phrase()),
                };
                splices.push((
                    u.span.start,
                    u.span.end,
                    note,
                    word.to_string(),
                    (name == "PATH").then_some(from),
                ));
                continue;
            }
            // Then the shell's own numbers; [`NUMERIC`] says what the claim is and is not.
            if numeric_text(written) {
                splices.push((
                    u.span.start,
                    u.span.end,
                    format!(
                        "`${name}` is a number the shell keeps rather than a value in this \
                         text — a status, a count or a pid — and a number is not a place, so \
                         it is read as `{NUMERIC_WORD}`; nothing the gate places depends on \
                         which number it turns out to be"
                    ),
                    NUMERIC_WORD.to_string(),
                    None,
                ));
            }
        }
        if splices.is_empty() {
            break;
        }
        // One splice per span, and no two splices may touch the same byte: a `for` body is
        // walked once per pass with the SAME offsets, and a nested construct is reported at
        // both its spans. Two splices at one place replace the value AND the bytes after it,
        // so the FIRST of an overlapping run is kept and the other stays unresolved.
        splices.sort_by_key(|(start, _, _, _, _)| *start);
        let mut disjoint: Vec<(usize, usize, String, String, Option<Source>)> = Vec::new();
        for s in splices {
            if disjoint.last().is_some_and(|k| s.0 < k.1) {
                continue;
            }
            disjoint.push(s);
        }
        let mut next = text.clone();
        for (start, end, _, word, named) in disjoint.iter().rev() {
            if *end <= next.len() && start < end {
                next.replace_range(*start..*end, word);
            }
            if let Some(from) = named {
                path = Some(*from);
            }
        }
        // A substitution may never make a command LESS parseable than it was: a span is a
        // claim about bytes and a wrong one replaces the value AND its neighbour. The whole
        // round is abandoned, leaving the command as unread as it arrived.
        if !n.partial && shell::normalise(&next).partial {
            break;
        }
        text = next;
        for (_, _, note, _, _) in disjoint {
            if !notes.contains(&note) {
                notes.push(note);
            }
        }
    }
    notes.extend(refused);
    Fixed { text, notes, path }
}

/// Whether this substitution is `pwd` and nothing else - `$(pwd)`, `` `pwd` ``, and the same
/// with the spaces bash allows inside the parens.
///
/// `$(pwd -P)` resolves symlinks and `$(cd x && pwd)` is a directory, so neither is where
/// the call runs; `$(pwd)/x` is this substitution plus a literal, which the ordinary splice
/// handles.
fn is_pwd(text: &str) -> bool {
    let inner = text
        .strip_prefix("$(")
        .and_then(|t| t.strip_suffix(')'))
        .or_else(|| text.strip_prefix('`').and_then(|t| t.strip_suffix('`')));
    inner.is_some_and(|i| i.trim() == "pwd")
}

/// One value in force for one name, from one byte offset of the command on.
struct Binding {
    at: usize,
    name: String,
    /// `None` is `unset`: the name stops having a value here, which differs from never
    /// having had one, and a later round must not reuse the old one.
    value: Option<String>,
}

/// What the shell would have in each name, and from where - a value in an assignment is a
/// value only from the point the shell runs it.
///
/// Ordered by the END of the construct that sets it, so an expansion inside the assignment
/// itself is not resolved by its own value: `X=$X` is not a fixed point.
fn timeline(n: &shell::Normalised, env: &Surroundings) -> Vec<Binding> {
    let mut out: Vec<Binding> = Vec::new();
    let mut set = |at: usize, name: &str, value: Option<&str>| {
        out.push(Binding {
            at,
            name: name.to_string(),
            value: value.map(str::to_string),
        })
    };
    // `$PWD` is where the call runs, the same directory every relative operand is placed
    // against (`Baseline::cwd`), so the two cannot disagree about where a relative path
    // lands. A `bash` call naming its own `cwd` argument is not modelled.
    let mut cwd = env.workspace.clone();
    if let Some(w) = &cwd {
        set(0, "PWD", Some(w));
    }
    if let Some(h) = &env.home {
        set(0, "HOME", Some(h));
    }
    // A standalone `X=...` is a statement of its own: it sets the shell's variable for
    // everything after it.
    for a in &n.assignments {
        if let Some(v) = a.value.literal() {
            set(a.span.end, &a.name, Some(v));
        }
    }
    for stage in &n.stages {
        match stage.program_name().unwrap_or("") {
            // `export X=...` is how a command hands a value to the stages after it. The
            // grammar records the assignment on the stage (`export` is its program and
            // `X=...` its argument), which is why this reads `stage.assignments`.
            "export" | "declare" | "typeset" | "readonly" => {
                for a in &stage.assignments {
                    if let Some(v) = a.value.literal() {
                        set(stage.span.end, &a.name, Some(v));
                    }
                }
            }
            // A name that is unset has no value, and a stale binding would let the next
            // round resolve `$X` to a value the name no longer has.
            "unset" => {
                for w in &stage.argv {
                    if let Some(t) = w.literal() {
                        set(stage.span.end, t, None);
                    }
                }
            }
            // A `cd` moves `$PWD` for every stage after it, so it moves it here too. Only a
            // literal target: nothing on this side guesses at a directory.
            "cd" | "pushd" => {
                if let Some(t) = stage.argv.first().and_then(|w| w.literal())
                    && !t.is_empty()
                    && t != "-"
                {
                    let target = env.expand(t);
                    let base = cwd.clone().unwrap_or_else(|| "/".into());
                    let joined = if target.starts_with('/') {
                        target
                    } else {
                        format!("{base}/{target}")
                    };
                    cwd = Some(super::collapse(&joined));
                    set(stage.span.end, "PWD", cwd.as_deref());
                }
            }
            _ => {}
        }
    }
    // A stable sort keeps two bindings set at the same offset in the order they were made,
    // so the later statement wins and `binding.at <= at` below reads "set at or before this
    // point".
    out.sort_by_key(|b| b.at);
    out
}

/// The value in force for `name` at byte offset `at`, and where it came from.
///
/// What this command says wins, including a binding to nothing: `unset X; cat $X` must not
/// fall through to a value the session holds, which is why this is not a `find` over a
/// map. Only where the command is silent does the session's environment answer.
fn value_at(
    timeline: &[Binding],
    env: &Surroundings,
    name: &str,
    at: usize,
) -> Option<(String, Source)> {
    if let Some(b) = timeline.iter().rfind(|b| b.at <= at && b.name == name) {
        return b.value.clone().map(|v| (v, Source::Command));
    }
    let held = env.session.get(name)?;
    Some((
        held.value.clone(),
        match held.from {
            env::From::Harness => Source::Harness,
            env::From::Shell => Source::Shell,
        },
    ))
}

/// The value as the one word the shell will deliver, or `None` when it cannot be put back
/// safely.
///
/// Whitespace is field splitting and `*`, `?`, `[` are pathname expansion, so writing
/// either back makes the text a different command. The splice lands where the expansion was,
/// possibly inside double quotes, where a quote of our own would become part of the
/// argument - a plain word is the same bytes bare and quoted.
fn as_one_word(v: &str) -> Option<&str> {
    let plain = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%,".contains(c));
    plain.then_some(v)
}
