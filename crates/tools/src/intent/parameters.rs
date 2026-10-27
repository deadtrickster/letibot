//! **The parameter expansions whose value this call already fixes**, put back into the
//! command text before the grammar reads it.
//!
//! # The measurement this exists for
//!
//! One package session, 178 gate decisions, 15 of them asks whose baseline read *"the
//! meaning was never resolved (N construct(s); each scope is taken as the widest it
//! could reach)"*. Every one of those commands looked like
//!
//! ```text
//! cd /home/dead/Projects/… && export CARGO_TARGET_DIR=$PWD/target && cargo test --offline -p …
//! ```
//!
//! and none of them was actually a mystery. `$PWD` is the directory the call runs in;
//! `$HOME` is the operator's home; `export X=…` earlier in the same command is a value
//! this text states itself. Writing those off as *unreadable* cost more than the reading
//! did: an unresolved action is `NotRun` for the card's standing offers (a session grant
//! can never cover it — `Grant::covers` refuses before it looks at coverage), so the
//! person was asked the same question for ever with the cheap answers taken away.
//!
//! # What is resolved, and what deliberately is not
//!
//! | | |
//! |---|---|
//! | `$PWD` | the directory the call runs in, **after a literal `cd`** earlier in the command |
//! | `$HOME` | the harness's own home, the same value every path in [`crate::intent`] is placed against |
//! | `$X` where `X` is assigned earlier **as its own statement**, or by `export`/`declare`/`typeset`/`readonly` | the value the text gives it |
//! | `$(…)`, a backquote, `$((…))`, `<(…)`, `$'…'`, `{a,b}` | unresolved, as they were |
//! | `$RANDOM`, `$?`, `$$`, a name nobody assigned | unresolved, as they were |
//! | `X=1 cmd $X` | unresolved: bash expands the argument list BEFORE the prefix assignment lands, and `X=1 echo $X` prints an empty line — so treating it as `1` would be a false claim about what runs |
//!
//! # Why the text is rewritten rather than the words patched
//!
//! Everything downstream reads the [`shell::Normalised`] — regions, the shapes, per-stage
//! intents, the shape a decision is recorded under. A substitution that only fixed the
//! argument list would leave `$PWD/target` placing as `Region::Unbounded` and the shape
//! carrying `<?>`; substituting into the text and re-normalising fixes all of them at
//! once, and it is the definition of *the command now means this* rather than a second
//! opinion beside it.
//!
//! # The one thing that could go wrong, and the guard against it
//!
//! A substitution is a claim, and a wrong one is worse than the unresolved construct it
//! replaced. So a value is only put back when the shell would deliver exactly those
//! bytes as exactly one word: no whitespace (field splitting would make two words), no
//! `*`, `?` or `[` (pathname expansion would make it a set of files), and **plain** — see
//! [`as_one_word`], because a splice lands where the expansion was and that may be inside
//! double quotes. Everything else stays unresolved, which is the fail-closed direction and
//! the one the operator's ruling of 2026-10-02 already covers.

use letibot_code::shell::{self, Construct};

use super::Surroundings;

/// How many times the text is re-read.
///
/// One round resolves every expansion whose value is written literally; a value that is
/// itself an expansion (`export A=x; export B=$A; cat $B/f`) needs the round after the
/// one that read it. The bound only has to be larger than a plausible chain — each round
/// strictly removes a construct or makes no progress and stops, so this is a ceiling on
/// pathology rather than a budget.
const ROUNDS: usize = 8;

/// **The command with every parameter it already fixes written out.**
///
/// Returns the (possibly unchanged) text and a note per substitution, for the audit row:
/// a card whose regions were placed against a directory the operator never typed should
/// say where that directory came from.
pub(super) fn resolve(command: &str, env: &Surroundings) -> (String, Vec<String>) {
    let mut text = command.to_string();
    let mut notes: Vec<String> = Vec::new();
    for _ in 0..ROUNDS {
        let n = shell::normalise(&text);
        if n.is_resolved() {
            break;
        }
        let timeline = timeline(&n, env);
        // **Back to front**, so a splice cannot move the span of one still to be done.
        // The spans are the grammar's own byte offsets, which is why this is exact and
        // a search for the text is not: `'$PWD'` in single quotes is a literal and the
        // grammar knows it, while a string search would rewrite it.
        let mut splices: Vec<(usize, usize, String, String)> = Vec::new();
        for u in &n.unresolved {
            let Construct::ParameterExpansion { name } = &u.construct else {
                continue;
            };
            let Some(value) = value_at(&timeline, name, u.span.start) else {
                continue;
            };
            let Some(word) = as_one_word(&value) else {
                continue;
            };
            splices.push((u.span.start, u.span.end, name.clone(), word.to_string()));
        }
        if splices.is_empty() {
            break;
        }
        splices.sort_by_key(|(start, _, _, _)| *start);
        for (start, end, _, word) in splices.iter().rev() {
            if *end <= text.len() && start < end {
                text.replace_range(*start..*end, word);
            }
        }
        for (_, _, name, word) in splices {
            let note = format!("`${name}` is `{word}`");
            if !notes.contains(&note) {
                notes.push(note);
            }
        }
    }
    (text, notes)
}

/// One value in force for one name, from one byte offset of the command on.
struct Binding {
    at: usize,
    name: String,
    /// `None` is `unset`: the name stops having a value here, which is a different fact
    /// from never having had one only in that a later round must not reuse the old one.
    value: Option<String>,
}

/// **What the shell would have in each name, and from where** — because a value in an
/// assignment is a value only from the point the shell runs it.
///
/// Ordered by the END of the construct that sets it, so an expansion inside the
/// assignment itself is not resolved by its own value: `X=$X` is not a fixed point.
fn timeline(n: &shell::Normalised, env: &Surroundings) -> Vec<Binding> {
    let mut out: Vec<Binding> = Vec::new();
    let mut set = |at: usize, name: &str, value: Option<&str>| {
        out.push(Binding {
            at,
            name: name.to_string(),
            value: value.map(str::to_string),
        })
    };
    // `$PWD` is where the call runs. That is `env.workspace`, the same directory every
    // relative operand is placed against (`Baseline::cwd`), so the two cannot disagree
    // about where a relative path lands — including its limitation, which is that a
    // `bash` call naming its own `cwd` argument is not modelled here.
    let mut cwd = env.workspace.clone();
    if let Some(w) = &cwd {
        set(0, "PWD", Some(w));
    }
    if let Some(h) = &env.home {
        set(0, "HOME", Some(h));
    }
    // A standalone `X=…` is a statement of its own: it sets the shell's variable for
    // everything after it.
    for a in &n.assignments {
        if let Some(v) = a.value.literal() {
            set(a.span.end, &a.name, Some(v));
        }
    }
    for stage in &n.stages {
        match stage.program_name().unwrap_or("") {
            // `export X=…` is how a command hands a value to the stages after it. The
            // grammar records the assignment on the stage (`export` is its program and
            // `X=…` its argument), which is why this reads `stage.assignments` rather
            // than `argv`.
            "export" | "declare" | "typeset" | "readonly" => {
                for a in &stage.assignments {
                    if let Some(v) = a.value.literal() {
                        set(stage.span.end, &a.name, Some(v));
                    }
                }
            }
            // A name that is unset has no value, and a stale binding would let the next
            // round resolve `$X` to what it used to hold.
            "unset" => {
                for w in &stage.argv {
                    if let Some(t) = w.literal() {
                        set(stage.span.end, t, None);
                    }
                }
            }
            // **A `cd` moves `$PWD` for every stage after it**, so it moves it here too.
            // Only a literal target: nothing on this side guesses at a directory, for the
            // reason `Baseline::of_command_with` gives when it places a relative path.
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
    // made, so the later statement wins — `binding.at <= at` below reads "set at or
    // before this point".
    out.sort_by_key(|b| b.at);
    out
}

/// The value in force for `name` at byte offset `at`, or `None` when nothing fixed it.
fn value_at(timeline: &[Binding], name: &str, at: usize) -> Option<String> {
    timeline
        .iter()
        .rfind(|b| b.at <= at && b.name == name)
        .and_then(|b| b.value.clone())
}

/// **The value as the one word the shell will deliver**, or `None` when it cannot be put
/// back safely.
///
/// The exclusions are the ways a shell changes an expansion on its way out, and each of
/// them would make the substitution a claim about a command that is not the one in front of
/// the operator:
///
/// * **whitespace** — field splitting. `X='a b'; cmd $X` is two arguments, and writing `a
///   b` back into the text makes it two words of a different command.
/// * **`*`, `?`, `[`** — pathname expansion. `X='*'; cmd $X` is every file in the
///   directory.
/// * **anything outside the plain set** — because the splice lands exactly where the
///   expansion was, and that may be **inside double quotes** (`echo "p=$PWD"`), where a
///   quote of our own would become part of the argument. A plain word is the same bytes
///   bare and inside quotes, which is why it is the only shape written back. The cost is
///   named: a value containing `=`, `+` or `~` stays unresolved and the command goes on
///   asking, which is the fail-closed direction.
fn as_one_word(v: &str) -> Option<&str> {
    let plain = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%,".contains(c));
    plain.then_some(v)
}
