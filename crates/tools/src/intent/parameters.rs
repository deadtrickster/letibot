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
//! | `$?`, `$$`, `$!`, `$#`, `$RANDOM`, `$PIPESTATUS`, … — **the shell's own numbers** | the number `0`, because a number is not a place (see [`NUMERIC`]) |
//! | `$(…)`, a backquote, `$((…))`, `<(…)`, `$'…'`, `{a,b}` | unresolved, as they were |
//! | `$@`, `$*`, `$1`, `$0`, a name nobody assigned | unresolved, as they were |
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

/// **The shell's own numbers: expansions whose value is a count, a status or a pid.**
///
/// None of them is in the text, and none of them ever will be — and that is not a reason
/// to refuse the command, because **a number is not a place**. What an unknown word costs
/// the gate is scope: an unresolvable argument widens the action to
/// `Region::Unbounded` (`intent::Baseline::widen_argument`), which is outside everything
/// and which no session grant can cover. A value that can only be `0`–`255` (`$?`), a
/// count (`$#`), a pid (`$$`, `$!`) or a small integer (`$RANDOM`, `$LINENO`) reaches no
/// directory that a number does not already name.
///
/// # The measurement that made this a rule
///
/// MEASURED 2026-10-26, the operator's three corpora (161,099 bash decisions): after `$PWD`
/// and the earlier assignments were resolved, **`$?` was the largest single reason a
/// command was still unresolved — 6,019 commands, 4,827 of them unresolved for that alone**
/// — and every one of them was the same idiom: a build or a test, then `echo "exit=$?"` to
/// say how it went. The ruling of 2026-10-02 already says what that costs and how to read
/// it, in the operator's own words about this very shape — *"`$?` inside `echo` widens
/// nothing `echo` can act on, so it is admitted even in a narrow mode — which is the
/// actually-correct answer, not a concession"*. It was not admitted, because the parser's
/// refusal outranked the reading. This list is that ruling applied where it belongs.
///
/// # What is written back, and why `0` rather than a placeholder of another shape
///
/// A number, because the value IS a number: `echo "exit=0"`, `head -$?` → `head -0`. The
/// alternative — keeping the word unresolved — claims the command is unreadable, which is
/// false: the program, its arguments and every path in them are all read. The one thing
/// this can get wrong is WHICH number, and a number names a file only when a program
/// treats it as a filename — `cat $?`, `rm dir/$?` — where the difference is one digit of
/// one path inside the same region, not a different region, program or intent.
///
/// **What is deliberately absent**: `$@`, `$*`, `$1`, `$0`, `$-` and every name nobody
/// assigned. Those CAN be paths, so they stay unresolved and fail closed. `${!x}` (indirect)
/// is a name, not a number, and `named()` reports its `x` rather than its `!` — it never
/// arrives here.
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

/// **Whether the expansion is exactly one name's value — and nothing done to it.**
///
/// The grammar nests a great deal inside one `expansion` node and hands every one of them
/// the same `variable_name`: `${#X}` (a LENGTH), `${X%.*}` (a suffix stripped), `${X/a/b}`
/// (replaced), `${X:0:3}` (a slice), `${!X}` (the value of the variable X NAMES) and
/// `${arr[0]}` (one element of an array). A resolver keyed on the name alone writes the
/// value of `X` for all six, which is a claim about a command the operator did not write.
///
/// MEASURED 2026-10-26 on the grammar's own tree: `${!X}` and `${#X}` come back with the
/// transform in an `operator` child and the name `X` beside it, so the reported NAME cannot
/// tell them apart from `${X}` — and for `${PIPESTATUS[0]}` the reported name is the whole
/// braces content, `{PIPESTATUS[0]}`, because the `variable_name` is inside a `subscript`.
/// The check is therefore lexical and exact, against the operator's own text.
fn plain(text: &str, name: &str) -> bool {
    text == format!("${name}") || text == format!("${{{name}}}")
}

/// A number the shell keeps, read from the TEXT of the expansion.
///
/// `${PIPESTATUS[0]}` and `${PIPESTATUS[@]}` are allowed because every element of a
/// numeric array is a number; any other subscript stays unresolved, since `${arr[0]}` is
/// one element of a value this module does not model. A transform is never a number:
/// `${#X}` is a length and `${!X}` is another variable's value, and neither is in [`NUMERIC`]
/// under that spelling.
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
            // **Only the expansions that ARE the value of one name.** `${#X}`, `${X%.*}`,
            // `${!X}` and `${arr[0]}` are other expressions of it — see [`plain`], which is
            // the one place that decides, so a transform added to the grammar cannot
            // silently become a value here.
            let text = n.source.get(u.span.start..u.span.end).unwrap_or("");
            // **What the text states comes first**: `X=/tmp; cat $X` is about `/tmp`, and no
            // rule about numbers may overrule a value somebody wrote down.
            if plain(text, name)
                && let Some(value) = value_at(&timeline, name, u.span.start)
                && let Some(word) = as_one_word(&value)
            {
                splices.push((
                    u.span.start,
                    u.span.end,
                    format!("`${name}` is `{word}`"),
                    word.to_string(),
                ));
                continue;
            }
            // Then the shell's own numbers — see [`NUMERIC`] for the measurement and for
            // what the claim is (and is not).
            if numeric_text(text) {
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
                ));
            }
        }
        if splices.is_empty() {
            break;
        }
        // **One splice per span, and no two splices may touch the same byte.** A `for` body
        // is walked once per pass with the SAME byte offsets — that is what the unrolling in
        // `shell` does — so a `$?` inside a loop body is reported once per iteration against
        // one place in the text; and a nested construct is reported at both the outer and the
        // inner span. Applying two splices to one place replaces the value AND the bytes
        // after it: MEASURED 2026-10-26 on lab2x1, commands where `echo "rc=$?"` became
        // `echo "rc=0` with the closing quote eaten and the rest of the command failed to
        // parse. Keeping the FIRST of an overlapping run is the safe choice: the dropped
        // one stays unresolved, the text is re-read next round, and nothing is claimed about
        // bytes two splices disagree over.
        splices.sort_by_key(|(start, _, _, _)| *start);
        let mut disjoint: Vec<(usize, usize, String, String)> = Vec::new();
        for s in splices {
            if disjoint.last().is_some_and(|k| s.0 < k.1) {
                continue;
            }
            disjoint.push(s);
        }
        let mut next = text.clone();
        for (start, end, _, word) in disjoint.iter().rev() {
            if *end <= next.len() && start < end {
                next.replace_range(*start..*end, word);
            }
        }
        // **A substitution may never make a command LESS parseable than it was.** A span is
        // a claim about bytes, and a wrong one replaces the value AND its neighbour — which
        // is how `echo "rc=$?"` once became `echo "rc=0` and the rest of the line stopped
        // parsing. The whole round is abandoned rather than applied: the command keeps its
        // constructs and stays unread, which is the fail-closed direction and is the same
        // answer the parser would have given. MEASURED 2026-10-26: 36 commands on lab2x1
        // still arrive here, and every one of them stays unread.
        if !n.partial && shell::normalise(&next).partial {
            break;
        }
        text = next;
        for (_, _, note, _) in disjoint {
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
