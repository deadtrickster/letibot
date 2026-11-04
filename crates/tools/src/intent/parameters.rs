//! ```text
//! cd /home/dead/Projects/… && export CARGO_TARGET_DIR=$PWD/target && cargo test --offline -p …
//! ```

use letibot_code::shell::{self, Construct};

use super::Surroundings;
use super::env::{self, From};

/// How many times the text is re-read.
/// One round resolves the values written literally and a value that is itself an
/// expansion needs the round after; a round that makes no progress stops.
const ROUNDS: usize = 8;

/// The shell's own numbers: expansions whose value is a count, a status or a pid.
/// A number is not a place, so these are read as `0` rather than left unresolved; `$@`,
/// `$1` and unassigned names can be paths, so they stay unresolved and fail closed.
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

/// What a numeric expansion is written back as. See [`NUMERIC`].
const NUMERIC_WORD: &str = "0";

/// Whether this expansion's value is one of the shell's numbers.
fn numeric(name: &str) -> bool {
    NUMERIC.contains(&name)
}

/// Whether the expansion is exactly one name's value, with nothing done to it.
/// The grammar reports `${#X}`, `${X%.*}`, `${!X}` and `${arr[0]}` under the same name,
/// so the check is lexical against the operator's own text.
fn plain(text: &str, name: &str) -> bool {
    text == format!("${name}") || text == format!("${{{name}}}")
}

/// A number the shell keeps, read from the text of the expansion.
/// Only a `[0]` or `[@]` subscript is a number: `${arr[0]}` is one element of a value
/// this module does not model, and a transform is never a number.
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
/// The text is the reading; the notes are what the card says about where a value came from.
#[derive(Debug, Default)]
pub(super) struct Fixed {
    pub text: String,
    pub notes: Vec<String>,
    /// Where the value that resolved `$PATH` came from, when one did.
    /// A `PATH` this command's own text fixed must not turn a stage into a refusal, but one
    /// that needed the session's environment is the new capability and stays unread.
    pub path: Option<Source>,
}

/// Where a value came from. `Source::Command` is this text; the other two are the
/// session's environment (see `crate::intent::env` for which is true of what).
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

    /// How the card says which `PATH` this was; see `intent::apply_path`.
    pub(super) fn path_phrase(self) -> &'static str {
        match self {
            Source::Command => "this command set",
            Source::Harness => "every command of this session is given",
            Source::Shell => "this session's shell reported",
        }
    }
}

/// The command with every parameter it already fixes written out.
/// Returns the (possibly unchanged) text and a note per substitution, so a card whose
/// regions were placed against a directory nobody typed says where it came from.
pub(super) fn resolve(command: &str, env: &Surroundings) -> Fixed {
    resolve_without(command, env, &[])
}

/// The same, with one name deliberately left unresolved.
/// The caller asks again with `PATH` unwritten when it cannot name the file a bare stage
/// would run, which leaves the command exactly as unread as it was before.
pub(super) fn resolve_without(command: &str, env: &Surroundings, skip: &[&str]) -> Fixed {
    let mut text = command.to_string();
    let mut notes: Vec<String> = Vec::new();
    // A value that is held and not written into the reading still has to be explained, or
    // the card's silence would be indistinguishable from *the session does not hold it*.
    let mut refused: Vec<String> = Vec::new();
    let mut path: Option<Source> = None;
    for _ in 0..ROUNDS {
        let n = shell::normalise(&text);
        if n.is_resolved() {
            break;
        }
        let timeline = timeline(&n, env);
        // Back to front, so a splice cannot move the span of one still to be done: the
        // spans are the grammar's byte offsets, so `'$PWD'` in single quotes is not rewritten.
        let mut splices: Vec<(usize, usize, String, String, Option<Source>)> = Vec::new();
        for u in &n.unresolved {
            let written = n.source.get(u.span.start..u.span.end).unwrap_or("");
            // `$(pwd)` is `$PWD`, which `timeline` already has: the one substitution
            // whose value is in the text. Nothing else inside a `$(...)` is touched.
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
            // Only the expansions that ARE the value of one name: `${#X}`, `${X%.*}` and
            // `${arr[0]}` are other expressions of it (see `plain`). What the text states
            // comes first; the session's environment answers only where the command is silent.
            if plain(written, name)
                && !skip.contains(&name.as_str())
                && let Some((value, from)) = value_at(&timeline, env, name, u.span.start)
                && let Some(word) = as_one_word(&value)
            {
                // A credential is held and not written down: the post-substitution text is
                // what the approval key stores and what the oracle is shown, so it stays
                // unresolved and the card says why.
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
            // Then the shell's own numbers: see `NUMERIC` for what the claim is and is not.
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
        // walked once per pass with the same offsets, and two splices on one place replace
        // the value and the bytes after it. The first of an overlapping run wins.
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
        // A substitution may never make a command less parseable than it was. A span is a
        // claim about bytes, so a round that breaks the parse is abandoned whole and the
        // command keeps its constructs, which is the fail-closed answer.
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

/// Whether this substitution is `pwd` and nothing else: `$(pwd)`, `` `pwd` ``, and the
/// spaces bash allows inside the parens.
/// `$(pwd -P)`, `$(cd x && pwd)` and `$(date)` are values the text does not hold.
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
    /// `None` is `unset`: the name stops having a value here, which is a different fact
    /// from never having had one only in that a later round must not reuse the old one.
    value: Option<String>,
}

/// What the shell would have in each name, and from where.
/// Ordered by the end of the construct that sets it, so `X=$X` is not a fixed point.
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
    // against, so the two cannot disagree; a `bash` call naming its own `cwd` is not modelled.
    let mut cwd = env.workspace.clone();
    if let Some(w) = &cwd {
        set(0, "PWD", Some(w));
    }
    if let Some(h) = &env.home {
        set(0, "HOME", Some(h));
    }
    // A standalone `X=...` sets the shell's variable for everything after it. A value this
    // round cannot read sets NOTHING rather than keeping the old one, which would read
    // `X=/w; X=$X/y` back as `/w`; the next round reads it once the assignment is literal.
    for a in &n.assignments {
        set(a.span.end, &a.name, a.value.literal());
    }
    for stage in &n.stages {
        match stage.program_name().unwrap_or("") {
            // `export X=...` hands a value to the stages after it; the grammar records it
            // on the stage, which is why this reads `stage.assignments` and not `argv`.
            "export" | "declare" | "typeset" | "readonly" => {
                for a in &stage.assignments {
                    set(stage.span.end, &a.name, a.value.literal());
                }
            }
            // A name that is unset has no value, and a stale binding would let the next
            // round resolve `$X` to its old value.
            "unset" => {
                for w in &stage.argv {
                    if let Some(t) = w.literal() {
                        set(stage.span.end, t, None);
                    }
                }
            }
            // A `cd` moves `$PWD` for every stage after it, so it moves it here too. Only
            // a literal target: nothing on this side guesses at a directory.
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
    // A stable sort keeps two bindings set at the same offset in the order they were
    // made, so the later statement wins and `binding.at <= at` reads "at or before".
    out.sort_by_key(|b| b.at);
    out
}

/// The value in force for `name` at byte offset `at`, and where it came from.
/// What this command says wins, including `unset`, which is why a binding to nothing
/// differs from no binding; otherwise the session's environment answers, with its source.
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
/// safely: whitespace splits fields, `*` `?` `[` expand to files, and a quote of our own
/// inside double quotes would become part of the argument.
fn as_one_word(v: &str) -> Option<&str> {
    let plain = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%,".contains(c));
    plain.then_some(v)
}
