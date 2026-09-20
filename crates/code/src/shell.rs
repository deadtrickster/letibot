//! Layer 2 of `docs/boundary-and-adjudication.md` §4: **normalise a shell command
//! through the grammar, and name what the grammar cannot resolve.**
//!
//! # The vulnerability class this exists for
//!
//! Every surveyed harness that gates on a shell command decides on a *string the
//! shell will reinterpret*. `crates/tools/src/exec/predicate.rs` concedes it in its
//! own words — *"not a shell parser… `eval`, here-docs, aliases, `$(...)` nesting
//! past one level are unhandled"* — and calls itself a diagnosis rather than the
//! mechanism.
//!
//! This is not a local worry. **GuardFall** (Adversa AI, 2026-06-30) named it as a
//! structural class and measured it against eleven popular open-source coding
//! agents: **ten of eleven** were bypassable, because the agent inspects raw
//! command text while the shell performs quote removal, expansion and argument
//! construction *after* that inspection. The one that held up reads the command the
//! way bash will before deciding, and keeps a hard refuse-list underneath. That is
//! this module plus `letibot_tools::intent`.
//!
//! # The rule that makes it honest, and it is the whole point
//!
//! A grammar buys a real decomposition and it buys a real **limit**. `eval "$X"`,
//! `$CMD --now`, `cat $FILE` and `<(...)` are not commands whose meaning is
//! *hidden*; they are commands whose meaning **does not exist yet**. So:
//!
//! > A construct the grammar cannot resolve is reported as
//! > [`Unresolved`] — naming the construct, what decision it denies, and its
//! > position — and is **never** classified as safe.
//!
//! [`Normalised::is_resolved`] is the single predicate a gate reads, and
//! `letibot_tools::adjudicate` maps `false` to `NotRun`: *nobody could decide*.
//! Never `Admit`, and never `Denied` either, because a construct nobody could read
//! is not a decision anybody made. A normaliser that answered "looks fine" for text
//! it could not parse would be the empty-haystack bug with a parse tree attached.
//!
//! What that costs is stated rather than hidden: `cat $FILE` is refused, and the
//! refusal carries the fix (`docs/tool-design-brief.md` §3, *errors carry the fix*)
//! — resolve the variable and pass the literal path. The alternative is deciding
//! about a target nobody can name, which is the theatre.
//!
//! # What it deliberately does NOT do
//!
//! It does not out-parse the shell, and it holds **no table of programs**. Which
//! token sits in the command-name position is grammar; what that token *means* —
//! that `sudo` wraps the next word, that `cat` surfaces its input, that `scp` leaves
//! the box — is a table about this host's software, and a table is policy. Policy
//! lives one crate up, in `letibot_tools::intent`, where the deny-list and the
//! §3 flow rule live with it. This crate keeps the property the rest of it has: it
//! knows about source text, and nothing about tools, turns or transcripts.
//!
//! # Reading the output
//!
//! ```
//! use letibot_code::shell;
//! let n = shell::normalise("cargo test 2>&1 | tee out.log");
//! assert!(n.is_resolved());
//! assert_eq!(n.binaries(), vec!["cargo", "tee"]);
//!
//! let n = shell::normalise("eval \"$CMD\"");
//! assert!(!n.is_resolved());
//! assert_eq!(n.unresolved[0].decides.as_str(), "argument");
//! ```

use std::fmt;

use rano::syntax::{Lang, Node, Stream};

// ---------------------------------------------------------------------------
// Positions
// ---------------------------------------------------------------------------

/// Where something is in the command text.
///
/// Byte offsets *and* line/column: the offsets are what a caller slices with, and
/// the line/column is what a refusal prints. A refusal that says "unresolvable
/// construct" without saying where is a refusal nobody can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    /// 1-based.
    pub line: usize,
    /// 0-based, in bytes from the start of the line.
    pub column: usize,
}

impl Span {
    fn of(node: &Node) -> Span {
        Span {
            start: node.start,
            end: node.end,
            line: node.start_point.row + 1,
            column: node.start_point.column,
        }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{} (bytes {}..{})", self.line, self.column, self.start, self.end)
    }
}

// ---------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------

/// One argument, command name or redirection target, resolved as far as the
/// grammar resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Word {
    /// The bytes are known. Quotes are removed and backslash escapes are applied,
    /// because that is what the shell will pass to `execve` — deciding on `"a b"`
    /// when the program receives `a b` is the GuardFall mistake in miniature.
    ///
    /// A leading `~` is left as written. Tilde expansion needs `$HOME`, which is
    /// not in the text; the caller that knows it expands it, and
    /// `letibot_tools::intent` does.
    Literal(String),
    /// A pathname pattern. The **pattern** is known and the **file set is not**,
    /// and those are different facts.
    ///
    /// Not [`Unresolved`]: a glob does not change which program runs, and refusing
    /// every `ls *.rs` would make the gate useless without making it safer. What it
    /// does change is *breadth*, which is why it is its own variant rather than a
    /// literal — a caller deciding about `rm -rf *` must be able to see that the
    /// target is a set nobody enumerated.
    Glob(String),
    /// A bash array literal: `args=( a b c )`. Every element is resolved on its own
    /// terms, so an array holding one expansion is not wholly unknown.
    Array(Vec<Word>),
    /// The grammar saw it and cannot say what it is. The index selects the entry in
    /// [`Normalised::unresolved`], so a caller reading an argument list never has
    /// to guess why a word is missing.
    Unresolved(usize),
}

impl Word {
    /// The bytes, when they are known. `None` for a glob (a pattern is not a value)
    /// and for an unresolved word.
    pub fn literal(&self) -> Option<&str> {
        match self {
            Word::Literal(s) => Some(s),
            _ => None,
        }
    }

    pub fn is_resolved(&self) -> bool {
        match self {
            Word::Unresolved(_) => false,
            Word::Array(parts) => parts.iter().all(Word::is_resolved),
            _ => true,
        }
    }

    /// The pattern or the value, for a caller that wants the text either way and
    /// says so. Used by the intent layer to decide *which region* a glob reaches
    /// into, which a pattern answers even though a file set does not.
    pub fn text(&self) -> Option<&str> {
        match self {
            Word::Literal(s) | Word::Glob(s) => Some(s),
            Word::Array(_) | Word::Unresolved(_) => None,
        }
    }

    /// The words this one stands for: itself, or an array's elements. So a caller
    /// scanning arguments for a path cannot miss the ones inside an array.
    pub fn flatten(&self) -> Vec<&Word> {
        match self {
            Word::Array(parts) => parts.iter().flat_map(Word::flatten).collect(),
            w => vec![w],
        }
    }
}

// ---------------------------------------------------------------------------
// The honest limit
// ---------------------------------------------------------------------------

/// The construct the grammar could not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Construct {
    /// `$X`, `${X}`, `${X:-default}`. The value is not in the text.
    ///
    /// `${X:-default}` is included on purpose: the default is only used when `X` is
    /// unset, so the resolved value is one of two and the grammar cannot say which.
    ParameterExpansion { name: String },
    /// `$(…)` or a backquote. **The command inside is resolved** and appears in
    /// [`Normalised::stages`] with [`Context::CommandSubstitution`]; what is
    /// unresolvable is the *value* it produces, which is what the enclosing word
    /// becomes. That split is the thing `predicate.rs` could not do.
    CommandSubstitutionValue,
    /// `<(…)` or `>(…)`. The inner command is resolved and listed; the *path* the
    /// shell invents for it (`/dev/fd/63`) is invented at runtime.
    ProcessSubstitutionPath,
    /// `$((…))`.
    ArithmeticExpansion,
    /// A here-document whose delimiter was unquoted, so the shell expands the body.
    /// A `<<'EOF'` body is literal and is resolved.
    ExpandedHeredoc,
    /// `$'…'`. The grammar hands over the bytes but not their decoded value, and
    /// reporting an undecoded `\n` as the literal would be a wrong claim about what
    /// the program receives.
    AnsiCEscapes,
    /// `{a,b}.txt`, `{1..5}`. **One** word in the text, **several** arguments after
    /// the shell is done with it.
    ///
    /// Unlike the rest of this list, brace expansion is decidable from the text
    /// alone and a later version could enumerate it. It is named rather than guessed
    /// because the grammar hands it over as ordinary concatenated words, so the
    /// merged literal reads as a filename called `{a,b}.txt` — a path that does not
    /// exist, which is to say it reads as **harmless**. That is the wrong direction
    /// to be wrong in, and it is GuardFall's class exactly: the string inspected is
    /// not the arguments delivered.
    BraceExpansion,
    /// The parse contains an `ERROR` or `MISSING` node: the grammar could not read
    /// this text.
    ///
    /// This is the most important one and the least obvious. tree-sitter *recovers*
    /// — it hands back a plausible tree rather than nothing — so every claim made
    /// about the recovered region is a guess. Measured here on
    /// `cat >&2 <<< 'here string'`, which this grammar cannot parse: the recovery
    /// turns the here-string into `< 'here string'`, i.e. **a read of a file named
    /// `here string`**. A normaliser that trusted that would report a data flow that
    /// does not exist. So a parse error is unresolvable for the whole command.
    ParseError,
}

impl Construct {
    pub fn as_str(&self) -> &'static str {
        match self {
            Construct::ParameterExpansion { .. } => "parameter_expansion",
            Construct::CommandSubstitutionValue => "command_substitution_value",
            Construct::ProcessSubstitutionPath => "process_substitution_path",
            Construct::ArithmeticExpansion => "arithmetic_expansion",
            Construct::ExpandedHeredoc => "expanded_heredoc",
            Construct::AnsiCEscapes => "ansi_c_escapes",
            Construct::BraceExpansion => "brace_expansion",
            Construct::ParseError => "parse_error",
        }
    }
}

/// Which decision the unresolvable construct denies.
///
/// Graded because the three are not equally bad and a report that flattened them
/// would lose the interesting half. `Program` means nobody knows *what will run*;
/// `Argument` and `RedirectTarget` mean the program is known and its target is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decides {
    /// The command name itself: `$CMD --now`, `"$(which x)" -y`. Nobody knows what
    /// binary this is.
    Program,
    /// An argument to a known program.
    Argument,
    /// A redirection's target file.
    RedirectTarget,
    /// The value assigned to a variable.
    Assignment,
    /// The shape of the command, not one of its parts. Only [`Construct::ParseError`].
    Structure,
}

impl Decides {
    pub fn as_str(&self) -> &'static str {
        match self {
            Decides::Program => "program",
            Decides::Argument => "argument",
            Decides::RedirectTarget => "redirect_target",
            Decides::Assignment => "assignment",
            Decides::Structure => "structure",
        }
    }
}

/// One construct the grammar resolved as far as it goes and no further.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    pub construct: Construct,
    pub decides: Decides,
    pub span: Span,
    /// The text as written, clipped.
    pub text: String,
    /// The part of the word that *is* known, when the unresolvable construct is
    /// concatenated with literal text: `/etc/$NAME` knows `/etc/`.
    ///
    /// Decision-relevant rather than cosmetic — `~/.ssh/$KEY` is unresolvable and
    /// its known prefix is already enough to place it in the secret store — so a
    /// caller can be strict about the prefix without pretending to know the whole.
    pub known_prefix: Option<String>,
    /// Which stage it belongs to, when it belongs to one.
    pub stage: Option<usize>,
    /// One sentence naming what cannot be decided and what would fix it.
    pub why: String,
}

// ---------------------------------------------------------------------------
// Redirections
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectOp {
    /// `<`
    Read,
    /// `>`, `>|`
    Write,
    /// `>>`
    Append,
    /// `<>`
    ReadWrite,
    /// `>&`, `&>`, `<&` — duplicate or close a descriptor.
    Duplicate,
    /// `<<`, `<<-`
    HereDoc,
    /// `<<<` — a here-string. Feeds one word to stdin.
    HereString,
}

impl RedirectOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            RedirectOp::Read => "read",
            RedirectOp::Write => "write",
            RedirectOp::Append => "append",
            RedirectOp::ReadWrite => "read_write",
            RedirectOp::Duplicate => "duplicate",
            RedirectOp::HereDoc => "heredoc",
            RedirectOp::HereString => "herestring",
        }
    }

    pub fn writes(self) -> bool {
        matches!(self, RedirectOp::Write | RedirectOp::Append | RedirectOp::ReadWrite)
    }

    pub fn reads(self) -> bool {
        matches!(
            self,
            RedirectOp::Read | RedirectOp::ReadWrite | RedirectOp::HereDoc | RedirectOp::HereString
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectTarget {
    File(Word),
    /// `2>&1`: the destination is another descriptor, not a file. Nothing leaves the
    /// process, which is why this must not be reported as a write to a file called
    /// `1`.
    Descriptor(u32),
    /// A here-document. `literal` is false when the delimiter was unquoted, i.e.
    /// the shell will expand the body — and then the body's own expansions are
    /// separately unresolvable.
    HereDoc { literal: bool, body: String },
    /// A here-string's word. It is stdin, not a file — reporting it as
    /// `RedirectTarget::File` would invent a read of a file named after the text,
    /// which is precisely what this grammar's error recovery does with `<<<` when it
    /// appears alongside another redirection.
    HereString(Word),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub op: RedirectOp,
    /// The descriptor being redirected, when it was written: `2` in `2>err`.
    pub fd: Option<u32>,
    pub target: RedirectTarget,
    pub span: Span,
}

// ---------------------------------------------------------------------------
// Stages
// ---------------------------------------------------------------------------

/// Where a stage sits in the command's structure.
///
/// A list rather than a single value, because the contexts nest and the nesting is
/// what a caller needs: a `rm -rf /` inside a function body inside a subshell is not
/// the same fact as one at the top level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    /// A stage of a `|` pipeline.
    Pipeline,
    /// `( … )`
    Subshell,
    /// `{ …; }`
    Group,
    /// The right side of `&&`.
    AndThen,
    /// The right side of `||`.
    OrElse,
    /// `! …`
    Negated,
    /// The body of a `for`, `while` or `until`.
    Loop,
    /// A branch of an `if` or a `case`, or the condition of one.
    Conditional,
    /// Inside `[ … ]` or `[[ … ]]`.
    Test,
    /// Inside `$(…)` or a backquote. **The stage runs.**
    CommandSubstitution,
    /// Inside `<(…)` or `>(…)`. The stage runs.
    ProcessSubstitution,
    /// The body of a function definition. The stage does **not** run unless the
    /// function is called.
    FunctionBody(String),
    /// `… &`
    Background,
}

impl Context {
    pub fn as_str(&self) -> &'static str {
        match self {
            Context::Pipeline => "pipeline",
            Context::Subshell => "subshell",
            Context::Group => "group",
            Context::AndThen => "and_then",
            Context::OrElse => "or_else",
            Context::Negated => "negated",
            Context::Loop => "loop",
            Context::Conditional => "conditional",
            Context::Test => "test",
            Context::CommandSubstitution => "command_substitution",
            Context::ProcessSubstitution => "process_substitution",
            Context::FunctionBody(_) => "function_body",
            Context::Background => "background",
        }
    }
}

/// Whether the stage runs when the command runs.
///
/// Stated because "this command contains `rm -rf /`" and "this command runs
/// `rm -rf /`" are different claims, and a refusal that makes the second when only
/// the first is true is a refusal that will be argued with — correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Certainty {
    /// Reached unconditionally.
    Always,
    /// Guarded: a branch, a loop that may not iterate, the right side of `&&`.
    Conditional,
    /// In a function body. It runs only if something calls it, and whether anything
    /// does is a question about the whole script rather than about this stage.
    OnlyIfCalled,
}

impl Certainty {
    pub fn as_str(&self) -> &'static str {
        match self {
            Certainty::Always => "always",
            Certainty::Conditional => "conditional",
            Certainty::OnlyIfCalled => "only_if_called",
        }
    }
}

/// A `VAR=value` prefix or standalone assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub name: String,
    pub value: Word,
    pub span: Span,
}

/// One simple command: a program, its arguments, its redirections, and where it
/// sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub index: usize,
    /// The command-name token. [`Word::Unresolved`] here is the case that matters
    /// most: nobody knows what program this is.
    pub program: Word,
    pub argv: Vec<Word>,
    /// `VAR=1 cmd` — the assignments that apply to this stage only.
    pub assignments: Vec<Assignment>,
    pub redirects: Vec<Redirect>,
    pub context: Vec<Context>,
    pub certainty: Certainty,
    /// stdin comes from a pipe.
    pub pipe_in: bool,
    /// stdout goes to a pipe.
    pub pipe_out: bool,
    pub span: Span,
}

impl Stage {
    /// The program's name with any directory stripped: `/usr/bin/cat` → `cat`.
    ///
    /// `None` when the program is unresolved, which the caller must handle rather
    /// than default — that is the whole limit.
    pub fn program_name(&self) -> Option<&str> {
        let p = self.program.literal()?;
        Some(p.rsplit('/').next().unwrap_or(p))
    }

    /// Whether this program name can be **shadowed** by the shell that runs it: a
    /// bare name is resolved through aliases, shell functions and `PATH`, none of
    /// which are in the command text.
    ///
    /// This is the limit of what a grammar can promise, and it is a real defeat
    /// rather than a theoretical one. A surveyed harness with the best command parser
    /// of its group runs the model's command through an interactive login shell that
    /// replays `declare -f` and `alias -p` and re-enables `expand_aliases`, then
    /// `eval`s it — so `alias ls='rm -rf ~'` turns an entry on its always-safe list
    /// into an `rm`. The parser was not wrong about the text; **the text did not mean
    /// what the parser read**, because the shell held state the parser never
    /// consulted.
    ///
    /// So this crate reports the property and refuses to decide it: whether a bare
    /// name means what it says is a fact about the *execution environment*, which
    /// lives one crate up. `false` for a program given with a `/` in it, which no
    /// alias and no function name can shadow. `None` when the program is unresolved,
    /// which is already worse.
    pub fn program_is_shadowable(&self) -> Option<bool> {
        self.program.literal().map(|p| !p.contains('/'))
    }

    /// Files this stage writes **through a redirection**, which is the half that is
    /// visible without a table of programs.
    pub fn redirect_writes(&self) -> Vec<&Word> {
        self.redirects
            .iter()
            .filter(|r| r.op.writes())
            .filter_map(|r| match &r.target {
                RedirectTarget::File(w) => Some(w),
                _ => None,
            })
            .collect()
    }

    /// Files this stage reads through a redirection.
    pub fn redirect_reads(&self) -> Vec<&Word> {
        self.redirects
            .iter()
            .filter(|r| r.op.reads())
            .filter_map(|r| match &r.target {
                RedirectTarget::File(w) => Some(w),
                _ => None,
            })
            .collect()
    }

    /// Whether this stage's stdout reaches the caller — i.e. becomes a tool result,
    /// and therefore the transcript.
    ///
    /// §3's invariant is about *where bytes end up*, and this is the structural half
    /// of that question: a stage whose stdout is piped onward or redirected to a
    /// file does not surface it here. False for a `> file`, false mid-pipeline, true
    /// for the last stage of a pipeline with no output redirection.
    pub fn stdout_surfaces(&self) -> bool {
        if self.pipe_out {
            return false;
        }
        !self.redirects.iter().any(|r| {
            r.op.writes() && matches!(r.fd, None | Some(1)) && matches!(r.target, RedirectTarget::File(_))
        })
    }
}

// ---------------------------------------------------------------------------
// The whole thing
// ---------------------------------------------------------------------------

/// A command, normalised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalised {
    /// The text as given.
    pub source: String,
    /// Every simple command, in source order, including those inside
    /// substitutions, loops and function bodies.
    pub stages: Vec<Stage>,
    /// Assignments that are statements of their own rather than a stage's prefix.
    pub assignments: Vec<Assignment>,
    /// **Read this before reading anything else.** Empty means the grammar resolved
    /// the whole command.
    pub unresolved: Vec<Unresolved>,
    /// The parse contained an `ERROR` or `MISSING` node.
    ///
    /// Also recorded as an [`Unresolved`] with [`Construct::ParseError`], so a
    /// caller that only looks at one of the two still fails closed. Two mechanisms
    /// for one safety property, for the reason `adjudicate.rs` gives about the
    /// backend: a property with one mechanism ships broken the first time somebody
    /// refactors the mechanism.
    pub partial: bool,
    /// Bytes examined. The denominator (`docs/tool-design-brief.md` §2.2): "0
    /// stages" over 0 bytes and "0 stages" over 4 KiB are different facts, and an
    /// empty command must not read as a clean one.
    pub bytes: usize,
}

impl Normalised {
    /// **The predicate a gate reads.** True only when nothing in the command needs
    /// a runtime value and the grammar read all of it.
    ///
    /// Deliberately conjunctive and deliberately strict. `cat $FILE` is not
    /// resolved: the program is known and the target is not, and the target is what
    /// the decision is about.
    pub fn is_resolved(&self) -> bool {
        self.unresolved.is_empty() && !self.partial
    }

    /// Every program that will run, in source order, skipping the unresolved ones —
    /// so a caller must consult [`Normalised::is_resolved`] first or it is reading a
    /// list with silent holes. [`Normalised::unresolved_programs`] is the other half.
    pub fn binaries(&self) -> Vec<&str> {
        self.stages.iter().filter_map(|s| s.program_name()).collect()
    }

    /// The stages whose program nobody can name.
    pub fn unresolved_programs(&self) -> Vec<&Stage> {
        self.stages
            .iter()
            .filter(|s| !s.program.is_resolved())
            .collect()
    }

    /// Files written through a redirection, anywhere in the command.
    pub fn redirect_writes(&self) -> Vec<&Word> {
        self.stages.iter().flat_map(|s| s.redirect_writes()).collect()
    }

    /// Files read through a redirection, anywhere in the command.
    pub fn redirect_reads(&self) -> Vec<&Word> {
        self.stages.iter().flat_map(|s| s.redirect_reads()).collect()
    }

    /// The unresolvable constructs, for a refusal body — **one explanation per
    /// distinct reason**, with the places it applies listed under it.
    ///
    /// The refusal has to carry the fix (`docs/tool-design-brief.md` §3), and the
    /// fix for an unresolvable construct is always the same shape: resolve it in the
    /// caller and pass the literal.
    ///
    /// # Why this groups
    ///
    /// It used to print two lines per entry — the position, then the `why` —
    /// and the `why` is per CONSTRUCT KIND, not per occurrence. So a command
    /// using one variable five times printed the same 220-character sentence
    /// five times: 1,776 bytes to say one thing. The operator, 2026-09-17,
    /// looking at a worse one: *"the explanation lines are repeated over and
    /// over, also 66 lines???"*
    ///
    /// Sixty-six lines is a refusal nobody reads, and a refusal nobody reads is
    /// the same as a refusal that did not say why — which is the thing this
    /// whole report exists to avoid. So identical reasons collapse: the
    /// sentence once, then the positions it covers, and past
    /// [`MAX_PLACES_SHOWN`] a count rather than a list.
    pub fn unresolved_report(&self) -> String {
        if self.unresolved.is_empty() {
            return String::new();
        }
        // Grouped by the sentence itself rather than by `construct`, because two
        // kinds can share a reason and one kind can give different ones — the
        // text is what a reader is being spared, so the text is the key. Order
        // is first appearance: the earliest position in the command comes first,
        // which is the order somebody reads their own command in.
        let mut order: Vec<&str> = Vec::new();
        let mut by_why: std::collections::HashMap<&str, Vec<&Unresolved>> = Default::default();
        for u in &self.unresolved {
            let at = by_why.entry(u.why.as_str()).or_default();
            if at.is_empty() {
                order.push(u.why.as_str());
            }
            at.push(u);
        }

        let mut s = String::new();
        for why in order {
            let places = &by_why[why];
            let first = places[0];
            s.push_str(&format!("  {}: {why}\n", first.construct.as_str()));
            for u in places.iter().take(MAX_PLACES_SHOWN) {
                s.push_str(&format!(
                    "      at {} decides the {}: {:?}{}\n",
                    u.span,
                    u.decides.as_str(),
                    u.text,
                    u.known_prefix
                        .as_ref()
                        .map(|p| format!(" (known prefix {p:?})"))
                        .unwrap_or_default(),
                ));
            }
            if places.len() > MAX_PLACES_SHOWN {
                s.push_str(&format!(
                    "      … and {} more place(s) with the same cause\n",
                    places.len() - MAX_PLACES_SHOWN
                ));
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Normalisation
// ---------------------------------------------------------------------------

/// How many positions one reason lists before it counts the rest.
///
/// Four: enough that a reader sees this is not a one-off, few enough that a
/// command using a variable thirty times does not print thirty lines. The count
/// is the disclosure — the same rule the spill and the miss report already keep,
/// that a refusal may summarise but may never be silent about what it summarised.
const MAX_PLACES_SHOWN: usize = 4;

/// Parse `source` as bash and normalise it.
///
/// Never fails and never panics on input: an unparseable command comes back with
/// `partial` set and a [`Construct::ParseError`] entry, which is a *result* saying
/// nobody could read it rather than an error saying the normaliser broke.
pub fn normalise(source: &str) -> Normalised {
    let mut out = Normalised {
        source: source.to_string(),
        stages: Vec::new(),
        assignments: Vec::new(),
        unresolved: Vec::new(),
        partial: false,
        bytes: source.len(),
    };

    let mut stream = Stream::new(Lang::Bash);
    stream.push(source);

    let Some(root) = stream.root() else {
        out.partial = true;
        out.unresolved.push(Unresolved {
            construct: Construct::ParseError,
            decides: Decides::Structure,
            span: Span { start: 0, end: source.len(), line: 1, column: 0 },
            text: clip(source, 200),
            known_prefix: None,
            stage: None,
            why: "the bash grammar returned no tree at all, so nothing here has been \
                  read and no claim about this command is available"
                .into(),
        });
        return out;
    };

    if root.has_error {
        out.partial = true;
        // Name the first bad region rather than the whole command: "somewhere in
        // these 4 KiB" is not a position.
        let (span, text) = first_error(&root, source)
            .map(|n| (Span::of(n), clip(src(n, source), 120)))
            .unwrap_or_else(|| {
                (
                    Span { start: 0, end: source.len(), line: 1, column: 0 },
                    clip(source, 120),
                )
            });
        out.unresolved.push(Unresolved {
            construct: Construct::ParseError,
            decides: Decides::Structure,
            span,
            text,
            known_prefix: None,
            stage: None,
            why: "the bash grammar could not read this. tree-sitter RECOVERS rather \
                  than failing, so the tree it produced here is a guess and every \
                  claim drawn from it would be one too — measured: `<<<` recovers as \
                  a read of a file whose name is the here-string's text"
                .into(),
        });
    }

    let mut ctx: Vec<Context> = Vec::new();
    walk(&root, source, &mut ctx, &mut out);
    out
}

fn first_error<'a>(node: &'a Node, _src: &str) -> Option<&'a Node> {
    // tree-sitter's `is_error()` is a node KIND here — rano's `Node` is plain data and
    // names it rather than carrying a flag for it.
    if node.kind == "ERROR" || node.is_missing {
        return Some(node);
    }
    if !node.has_error {
        return None;
    }
    node.children.iter().find_map(|c| first_error(c, _src))
}

fn src<'a>(node: &Node, source: &'a str) -> &'a str {
    source.get(node.start..node.end).unwrap_or("")
}

/// The child in field `name`, by **field** rather than by kind or position — the same
/// helper `crate::outline` uses, for the same reason: the grammar states which child a
/// field names, and matching kinds in the order they appear is guessing at a fact the
/// grammar already has.
fn child_field<'a>(node: &'a Node, name: &str) -> Option<&'a Node> {
    node.children
        .iter()
        .find(|c| c.field.as_deref() == Some(name))
}

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        return s.to_string();
    }
    format!("{}…", s.chars().take(n).collect::<String>())
}

fn certainty_of(ctx: &[Context]) -> Certainty {
    if ctx.iter().any(|c| matches!(c, Context::FunctionBody(_))) {
        return Certainty::OnlyIfCalled;
    }
    if ctx.iter().any(|c| {
        matches!(
            c,
            Context::Loop | Context::Conditional | Context::AndThen | Context::OrElse | Context::Test
        )
    }) {
        return Certainty::Conditional;
    }
    Certainty::Always
}

/// Descend, building stages. `ctx` is the nesting as it stands at this node.
fn walk(node: &Node, source: &str, ctx: &mut Vec<Context>, out: &mut Normalised) {
    match node.kind.as_str() {
        "command" | "declaration_command" | "unset_command" => {
            stage(node, source, ctx, out, false, false);
        }
        "pipeline" => {
            // The pipe flags are a property of position in the pipeline, and they
            // decide `Stage::stdout_surfaces` — which is the structural half of
            // §3's "may never enter the transcript".
            let members: Vec<&Node> = kids(node)
                .into_iter()
                .filter(|c| c.kind != "|" && c.kind != "|&")
                .collect();
            let last = members.len().saturating_sub(1);
            ctx.push(Context::Pipeline);
            for (i, m) in members.iter().enumerate() {
                pipe_member(*m, source, ctx, out, i > 0, i < last);
            }
            ctx.pop();
        }
        "redirected_statement" => {
            redirected(node, source, ctx, out, false, false);
        }
        "subshell" => push_walk(node, source, ctx, out, Context::Subshell),
        "compound_statement" => push_walk(node, source, ctx, out, Context::Group),
        "list" => {
            // `a && b` / `a || b`: the left side runs unconditionally, the right
            // side is guarded. Recording that asymmetry is the difference between
            // "this command runs rm" and "this command runs rm if the first part
            // failed".
            let op = kids(node)
                .into_iter()
                .find(|c| c.kind.as_str() == "&&" || c.kind.as_str() == "||")
                .map(|c| c.kind.to_string());
            let kids = kids(node);
            let mut seen_op = false;
            for k in kids {
                if k.kind == "&&" || k.kind == "||" {
                    seen_op = true;
                    continue;
                }
                if seen_op {
                    ctx.push(if op.as_deref() == Some("||") {
                        Context::OrElse
                    } else {
                        Context::AndThen
                    });
                    walk(k, source, ctx, out);
                    ctx.pop();
                } else {
                    walk(k, source, ctx, out);
                }
            }
        }
        "for_statement" | "c_style_for_statement" | "while_statement" | "until_statement" => {
            push_walk(node, source, ctx, out, Context::Loop)
        }
        "if_statement" | "elif_clause" | "else_clause" | "case_statement" | "case_item" => {
            push_walk(node, source, ctx, out, Context::Conditional)
        }
        "test_command" => {
            // No stage: `[ -f x ]` is a stat, not an action, and inventing a
            // program called `[` would put a phantom binary in `binaries()`. The
            // descent continues so a `$(…)` inside the test is still found.
            push_walk(node, source, ctx, out, Context::Test)
        }
        "command_substitution" => push_walk(node, source, ctx, out, Context::CommandSubstitution),
        "process_substitution" => push_walk(node, source, ctx, out, Context::ProcessSubstitution),
        "function_definition" => {
            let name = child_field(node, "name")
                .map(|n| src(n, source).to_string())
                .unwrap_or_else(|| "<anonymous>".into());
            push_walk(node, source, ctx, out, Context::FunctionBody(name))
        }
        "variable_assignment" => {
            // Reached here only as a statement of its own; a stage's prefix
            // assignments are consumed by `stage`.
            let a = assignment(node, source, out, None);
            out.assignments.push(a);
            scan_substitutions(node, source, ctx, out);
        }
        _ => {
            for child in &node.children {
                walk(child, source, ctx, out);
            }
        }
    }
}

fn push_walk(node: &Node, source: &str, ctx: &mut Vec<Context>, out: &mut Normalised, c: Context) {
    ctx.push(c);
    for child in &node.children {
        walk(child, source, ctx, out);
    }
    ctx.pop();
}

/// One member of a pipeline: a command, or a redirected command, or something
/// compound (a `while` loop reading from the pipe is real bash).
fn pipe_member(
    node: &Node,
    source: &str,
    ctx: &mut Vec<Context>,
    out: &mut Normalised,
    pipe_in: bool,
    pipe_out: bool,
) {
    match node.kind.as_str() {
        "command" | "declaration_command" | "unset_command" => {
            stage(node, source, ctx, out, pipe_in, pipe_out);
        }
        "redirected_statement" => redirected(node, source, ctx, out, pipe_in, pipe_out),
        _ => walk(node, source, ctx, out),
    }
}

/// A command with redirections attached. The redirections belong to the command,
/// not to the statement, which is why they cannot be discovered by a generic walk.
///
/// **Which command.** The grammar hoists a trailing redirection above the whole
/// list or pipeline it ends: `cd /x && python3 - <<'PY'` is
/// `redirected_statement(list(cd, python3), heredoc)`, and `a | b > f` is
/// `redirected_statement(pipeline(a, b), > f)`. The shell gives that redirection
/// to the LAST command — `python3` reads the here-document, `b` writes `f` — and
/// this used to give it to the first. Measured on the operator's corpus
/// (2026-09-17): 5,675 here-documents were read as `cd`'s stdin, 217 as `.`'s,
/// 214 as `timeout`'s. A compound body (`{ a; b; } > f`, `while …; done < f`) has
/// no single owner and every stage of the body gets the fact, as before.
///
/// **What follows the operator.** The grammar also nests whatever follows a
/// here-document operator on the same line INSIDE the `heredoc_redirect`:
/// `cat <<'EOF' | bash` puts `pipeline(| bash)` there, `cat > f <<'EOF' && chmod
/// +x f` puts `&& command(chmod …)` there. Those are commands and they used to be
/// dropped — `cat <<'EOF' | sudo bash` normalised to a `cat`. See
/// [`heredoc_tail`].
fn redirected(
    node: &Node,
    source: &str,
    ctx: &mut Vec<Context>,
    out: &mut Normalised,
    pipe_in: bool,
    pipe_out: bool,
) {
    let body = child_field(node, "body");
    let before = out.stages.len();
    let depth = ctx.len();
    let body_kind = body.map(|b| b.kind.as_str()).unwrap_or("");
    match body_kind {
        "command" | "declaration_command" | "unset_command" => {
            stage(body.unwrap(), source, ctx, out, pipe_in, pipe_out);
        }
        _ => {
            if let Some(b) = body {
                walk(b, source, ctx, out);
            }
        }
    }
    // Collect the redirects, then attach them to the stage(s) the body produced.
    // `cat <<EOF > f` nests the `file_redirect` inside the `heredoc_redirect`, so
    // this recurses rather than scanning one level.
    let mut reds = Vec::new();
    let mut tails: Vec<&Node> = Vec::new();
    for child in &node.children {
        if child.id == body.map(|b| b.id).unwrap_or(usize::MAX) {
            continue;
        }
        collect_redirects(child, source, out, &mut reds);
        if child.kind.as_str() == "heredoc_redirect" {
            // The substitutions inside the body are scanned here; the commands
            // that follow the operator are walked after the owner is known, so
            // they are not walked twice.
            for k in kids(child) {
                if !is_continuation(k) {
                    scan_substitutions(k, source, ctx, out);
                }
            }
            tails.push(child);
        } else {
            scan_substitutions(child, source, ctx, out);
        }
    }
    // The stages the body itself produced, not the ones inside a `$(…)` in one
    // of its arguments: those ran to make a word and the redirection is not
    // theirs.
    let own: Vec<usize> = (before..out.stages.len())
        .filter(|&i| {
            !out.stages[i].context[depth.min(out.stages[i].context.len())..]
                .iter()
                .any(|c| matches!(c, Context::CommandSubstitution | Context::ProcessSubstitution))
        })
        .collect();
    let single_owner = matches!(
        body_kind,
        "command" | "declaration_command" | "unset_command" | "pipeline" | "list" | "redirected_statement"
    );
    let owner = if single_owner { own.last().copied() } else { None };
    match owner {
        Some(i) => out.stages[i].redirects.extend(reds),
        None => {
            // A redirected compound statement (`{ …; } > f`). There is no single
            // stage to own the redirection; the fact is not dropped, it is
            // recorded against every stage of the body, which is what the shell
            // does to their shared stdout.
            for i in &own {
                out.stages[*i].redirects.extend(reds.iter().cloned());
            }
        }
    }
    for t in tails {
        heredoc_tail(t, source, ctx, out, owner);
    }
}

/// A named child of a `heredoc_redirect` that is a command rather than a part of
/// the here-document: what followed the `<<` operator on its line.
fn is_continuation(k: &Node) -> bool {
    k.named
        && !matches!(k.kind.as_str(),
            "heredoc_start" | "heredoc_body" | "heredoc_end" | "file_redirect" | "herestring_redirect"
        )
}

/// Walk the commands the grammar nested inside a `heredoc_redirect`, joined to
/// the owner by the operator token that precedes them. `| bash` makes the owner's
/// stdout a pipe and `bash` a pipe member; `&& chmod` is a guarded sibling.
fn heredoc_tail(
    node: &Node,
    source: &str,
    ctx: &mut Vec<Context>,
    out: &mut Normalised,
    owner: Option<usize>,
) {
    let mut op: Option<String> = None;
    for k in &node.children {
        if !k.named {
            if matches!(k.kind.as_str(), "|" | "|&" | "&&" | "||" | ";" | "&") {
                op = Some(k.kind.to_string());
            }
            continue;
        }
        if is_continuation(k) {
            continuation(k, op.take().as_deref(), source, ctx, out, owner);
        }
    }
}

/// The owner of a here-document turns out to feed a pipe the grammar hid after
/// the operator: its stdout is the pipe, and it is a pipeline member like the
/// stages it feeds.
fn feeds_pipe(s: &mut Stage) {
    s.pipe_out = true;
    if !s.context.contains(&Context::Pipeline) {
        s.context.push(Context::Pipeline);
    }
}

fn continuation(
    node: &Node,
    op: Option<&str>,
    source: &str,
    ctx: &mut Vec<Context>,
    out: &mut Normalised,
    owner: Option<usize>,
) {
    let piped = matches!(op, Some("|") | Some("|&"));
    if node.kind.as_str() == "pipeline" {
        // `pipeline(| bash)` or `pipeline(| pipeline(bash | wc))`: the leading
        // token says the owner feeds it, and a lone inner pipeline is the real
        // one.
        let leading = node
            .children
            .first()
            .is_some_and(|c| matches!(c.kind.as_str(), "|" | "|&"))
            || piped;
        let members: Vec<&Node> = kids(node)
            .into_iter()
            .filter(|c| c.kind != "|" && c.kind != "|&")
            .collect();
        if members.len() == 1 && members[0].kind == "pipeline" {
            continuation(members[0], Some("|"), source, ctx, out, owner);
            return;
        }
        if leading && let Some(o) = owner {
            feeds_pipe(&mut out.stages[o]);
        }
        let last = members.len().saturating_sub(1);
        ctx.push(Context::Pipeline);
        for (i, m) in members.iter().enumerate() {
            pipe_member(*m, source, ctx, out, i > 0 || leading, i < last);
        }
        ctx.pop();
        return;
    }
    match op {
        Some("|") | Some("|&") => {
            if let Some(o) = owner {
                feeds_pipe(&mut out.stages[o]);
            }
            ctx.push(Context::Pipeline);
            pipe_member(node, source, ctx, out, true, false);
            ctx.pop();
        }
        Some("&&") => {
            ctx.push(Context::AndThen);
            walk(node, source, ctx, out);
            ctx.pop();
        }
        Some("||") => {
            ctx.push(Context::OrElse);
            walk(node, source, ctx, out);
            ctx.pop();
        }
        _ => walk(node, source, ctx, out),
    }
}

fn collect_redirects(node: &Node, source: &str, out: &mut Normalised, into: &mut Vec<Redirect>) {
    match node.kind.as_str() {
        "file_redirect" => {
            if let Some(r) = file_redirect(node, source, out) {
                into.push(r);
            }
        }
        "herestring_redirect" => {
            let w = kids(node)
                .into_iter()
                .find(|c| c.named)
                .map(|c| word_of(c, source, out, Decides::RedirectTarget, Some(out.stages.len().saturating_sub(1))))
                .unwrap_or(Word::Literal(String::new()));
            into.push(Redirect {
                op: RedirectOp::HereString,
                fd: None,
                target: RedirectTarget::HereString(w),
                span: Span::of(node),
            });
        }
        "heredoc_redirect" => {
            into.push(heredoc_redirect(node, source, out));
            // `<<EOF > f` puts the file redirect inside the heredoc redirect.
            for child in &node.children {
                if child.kind.as_str() == "file_redirect"
                    && let Some(r) = file_redirect(child, source, out) {
                        into.push(r);
                    }
            }
        }
        _ => {
            for child in &node.children {
                collect_redirects(child, source, out, into);
            }
        }
    }
}

fn redirect_op(node: &Node) -> Option<RedirectOp> {
    for child in &node.children {
        let op = match child.kind.as_str() {
            "<" => RedirectOp::Read,
            ">" | ">|" => RedirectOp::Write,
            ">>" => RedirectOp::Append,
            "<>" => RedirectOp::ReadWrite,
            ">&" | "<&" | "&>" | "&>>" | ">&-" | "<&-" => RedirectOp::Duplicate,
            "<<" | "<<-" => RedirectOp::HereDoc,
            _ => continue,
        };
        // `&>` and `&>>` duplicate *and* write to a file; the file is what matters,
        // and calling it a duplicate would hide a write.
        if matches!(child.kind.as_str(), "&>" | "&>>") {
            return Some(if child.kind.as_str() == "&>>" {
                RedirectOp::Append
            } else {
                RedirectOp::Write
            });
        }
        return Some(op);
    }
    None
}

fn file_redirect(node: &Node, source: &str, out: &mut Normalised) -> Option<Redirect> {
    let op = redirect_op(node)?;
    let fd = child_field(node, "descriptor")
        .and_then(|n| src(n, source).parse::<u32>().ok());
    let dest = child_field(node, "destination");
    let target = match dest {
        Some(d) if op == RedirectOp::Duplicate && d.kind == "number" => {
            RedirectTarget::Descriptor(src(d, source).parse().unwrap_or(0))
        }
        Some(d) => RedirectTarget::File(word_of(d, source, out, Decides::RedirectTarget, None)),
        None => return None,
    };
    Some(Redirect {
        op,
        fd,
        target,
        span: Span::of(node),
    })
}

fn heredoc_redirect(node: &Node, source: &str, out: &mut Normalised) -> Redirect {
    let delim = kids(node)
        .into_iter()
        .find(|c| c.kind.as_str() == "heredoc_start")
        .map(|c| src(c, source).to_string())
        .unwrap_or_default();
    // A quoted delimiter (`<<'EOF'`, `<<"EOF"`) means the body is literal. An
    // unquoted one means the shell expands it, and then the body's expansions are
    // themselves unresolvable.
    let literal = delim.starts_with('\'') || delim.starts_with('"');
    let body_node = kids(node)
        .into_iter()
        .find(|c| c.kind.as_str() == "heredoc_body");
    let body = body_node.map(|b| src(b, source).to_string()).unwrap_or_default();
    if let Some(b) = body_node
        && !literal
        && kids(b).iter().any(|c| c.kind != "heredoc_content")
    {
        out.unresolved.push(Unresolved {
            construct: Construct::ExpandedHeredoc,
            decides: Decides::RedirectTarget,
            span: Span::of(b),
            text: clip(&body, 120),
            known_prefix: None,
            stage: Some(out.stages.len()),
            why: "the here-document's delimiter is unquoted, so the shell expands the \
                  body before the program reads it and the bytes it receives are not \
                  the bytes here. Quote the delimiter (`<<'EOF'`) to make the body \
                  literal"
                .into(),
        });
    }
    Redirect {
        op: RedirectOp::HereDoc,
        fd: None,
        target: RedirectTarget::HereDoc { literal, body },
        span: Span::of(node),
    }
}

/// Every child, **anonymous tokens included**.
///
/// The name this had — `named_children` — was wrong, and the wrongness cost a bug the
/// day it was "fixed": it was written as `node.children(&mut cursor).collect()`, which
/// is every child, and a reader taking the name at its word filtered on `Node::named`
/// and silently lost the `&&`, `|` and `;` this module decides *by*. An operator token
/// is exactly the kind of child tree-sitter leaves anonymous, so "named children" is
/// the wrong set for half its callers — see the `&&` asymmetry at `list`, which is a
/// permission decision.
fn kids(node: &Node) -> Vec<&Node> {
    node.children.iter().collect()
}

fn stage(
    node: &Node,
    source: &str,
    ctx: &mut Vec<Context>,
    out: &mut Normalised,
    pipe_in: bool,
    pipe_out: bool,
) {
    let index = out.stages.len();
    // Reserve the slot so nested substitutions get a higher index and the
    // `Unresolved::stage` back-references stay correct.
    out.stages.push(Stage {
        index,
        program: Word::Literal(String::new()),
        argv: Vec::new(),
        assignments: Vec::new(),
        redirects: Vec::new(),
        context: ctx.clone(),
        certainty: certainty_of(ctx),
        pipe_in,
        pipe_out,
        span: Span::of(node),
    });

    let mut program: Option<Word> = None;
    let mut argv = Vec::new();
    let mut assignments = Vec::new();
    let mut tails: Vec<&Node> = Vec::new();

    // `declaration_command` and `unset_command` name themselves with an anonymous
    // keyword child rather than a `command_name`.
    if matches!(node.kind.as_str(), "declaration_command" | "unset_command")
        && let Some(kw) = kids(node).into_iter().find(|c| !c.named)
    {
        program = Some(Word::Literal(src(kw, source).to_string()));
    }

    for child in kids(node) {
        match child.kind.as_str() {
            "command_name" => {
                // `command_name` wraps one word, which may itself be an expansion:
                // `$X --now` has an unresolvable PROGRAM, and that is the gravest
                // grade of unresolvable there is.
                let inner = kids(child).into_iter().next().unwrap_or(child);
                program = Some(word_of(inner, source, out, Decides::Program, Some(index)));
            }
            "variable_assignment" => {
                assignments.push(assignment(child, source, out, Some(index)));
            }
            "file_redirect" | "heredoc_redirect" | "herestring_redirect" => {
                // A redirection can hang off the command node itself rather than a
                // `redirected_statement` in some shapes; take it either way.
                let mut reds = Vec::new();
                collect_redirects(child, source, out, &mut reds);
                out.stages[index].redirects.extend(reds);
                if child.kind.as_str() == "heredoc_redirect" {
                    tails.push(child);
                }
            }
            _ if !child.named => {}
            _ => {
                argv.push(word_of(child, source, out, Decides::Argument, Some(index)));
            }
        }
    }

    let s = &mut out.stages[index];
    s.program = program.unwrap_or(Word::Literal(String::new()));
    s.argv = argv;
    s.assignments = assignments;

    // The commands inside `$(…)` and `<(…)` RUN. Their *value* is unresolvable and
    // is already recorded as such; the commands themselves are ordinary stages and
    // are listed here. This is the split `predicate.rs` names as its own limit —
    // *"`$(...)` nesting past one level are unhandled"* — and it is the reason a
    // grammar was worth the trouble: `kill $(pgrep -f x)` has two programs in it,
    // not one string.
    scan_substitutions(node, source, ctx, out);
    for t in tails {
        heredoc_tail(t, source, ctx, out, Some(index));
    }
}

/// Walk the substitutions hanging under `node`, without re-entering `node` itself.
fn scan_substitutions(node: &Node, source: &str, ctx: &mut Vec<Context>, out: &mut Normalised) {
    for child in kids(node) {
        match child.kind.as_str() {
            "command_substitution" | "process_substitution" => walk(child, source, ctx, out),
            // The commands nested after a here-document operator are walked by
            // `heredoc_tail`, substitutions and all; only the document itself is
            // scanned here.
            "heredoc_redirect" => {
                for k in kids(child) {
                    if !is_continuation(k) {
                        scan_substitutions(k, source, ctx, out);
                    }
                }
            }
            _ => scan_substitutions(child, source, ctx, out),
        }
    }
}

fn assignment(node: &Node, source: &str, out: &mut Normalised, stage: Option<usize>) -> Assignment {
    let name = child_field(node, "name")
        .map(|n| src(n, source).to_string())
        .unwrap_or_default();
    let value = match child_field(node, "value") {
        Some(v) => word_of(v, source, out, Decides::Assignment, stage),
        // `VAR=` with an empty value is a real assignment to the empty string.
        None => Word::Literal(String::new()),
    };
    Assignment {
        name,
        value,
        span: Span::of(node),
    }
}

/// Resolve one word node as far as the grammar resolves it, recording any
/// unresolvable construct as a side effect.
fn word_of(
    node: &Node,
    source: &str,
    out: &mut Normalised,
    decides: Decides,
    stage: Option<usize>,
) -> Word {
    // `VAR=( a b "$c" )`. The grammar gives an `array` node whose children are
    // ordinary words, so each element is resolved on its own terms — an array
    // holding one expansion is not wholly unknown, and flattening it into one
    // opaque blob would lose the elements that ARE known.
    if node.kind.as_str() == "array" {
        let parts: Vec<Word> = kids(node)
            .into_iter()
            .filter(|c| c.named)
            .map(|c| word_of(c, source, out, decides, stage))
            .collect();
        return Word::Array(parts);
    }
    match resolve(node, source) {
        Resolved::Literal(s) => Word::Literal(s),
        Resolved::Glob(s) => Word::Glob(s),
        Resolved::Blocked { construct, prefix, at } => {
            let idx = out.unresolved.len();
            let why = why_of(&construct, decides);
            out.unresolved.push(Unresolved {
                construct,
                decides,
                span: Span::of(at),
                text: clip(src(node, source), 120),
                known_prefix: prefix.filter(|p| !p.is_empty()),
                stage,
                why,
            });
            Word::Unresolved(idx)
        }
    }
}

fn why_of(construct: &Construct, decides: Decides) -> String {
    let what = match decides {
        Decides::Program => "which program will run",
        Decides::Argument => "what the program will be given",
        Decides::RedirectTarget => "which file the output goes to",
        Decides::Assignment => "what the variable will hold",
        Decides::Structure => "the shape of the command",
    };
    match construct {
        Construct::ParameterExpansion { name } => format!(
            "`${name}` decides {what}, and its value is not in this text — it comes \
             from the environment or an earlier command. Substitute the value and \
             pass the literal, or pass it as a separate argument the harness can see."
        ),
        Construct::CommandSubstitutionValue => format!(
            "a command substitution decides {what}. The command inside IS resolved \
             and is listed as its own stage, but its OUTPUT does not exist yet. Run \
             it first and pass the result."
        ),
        Construct::ProcessSubstitutionPath => format!(
            "a process substitution decides {what}: the shell invents a `/dev/fd/N` \
             path at runtime. The inner command is listed as its own stage; the path \
             cannot be named before the shell makes it."
        ),
        Construct::ArithmeticExpansion => {
            format!("an arithmetic expansion decides {what}, and it may read variables")
        }
        Construct::ExpandedHeredoc => format!(
            "the here-document body is expanded by the shell, so it decides {what} \
             with bytes that are not these. Quote the delimiter to make it literal."
        ),
        Construct::AnsiCEscapes => format!(
            "a `$'…'` string decides {what}, and its escapes are decoded by the shell \
             — the bytes the program receives are not the bytes written here."
        ),
        Construct::BraceExpansion => format!(
            "a brace expansion decides {what}: this is one word here and SEVERAL \
             arguments after the shell expands it. Write the arguments out."
        ),
        Construct::ParseError => {
            format!("the grammar could not read this, so {what} is a guess")
        }
    }
}

enum Resolved<'t> {
    Literal(String),
    Glob(String),
    Blocked {
        construct: Construct,
        /// The literal part before the unresolvable one, when there is one.
        prefix: Option<String>,
        /// The node it was found at. Borrowed from the parse, which the caller owns.
        at: &'t Node,
    },
}

fn resolve<'t>(node: &'t Node, source: &str) -> Resolved<'t> {
    match node.kind.as_str() {
        "word" => {
            let (text, glob) = unescape(src(node, source));
            if has_brace_expansion(&text) {
                return Resolved::Blocked {
                    construct: Construct::BraceExpansion,
                    prefix: None,
                    at: node,
                };
            }
            if glob { Resolved::Glob(text) } else { Resolved::Literal(text) }
        }
        // `'…'`: no expansion of any kind, and no globbing either.
        "raw_string" => {
            let t = src(node, source);
            Resolved::Literal(t.trim_start_matches('\'').trim_end_matches('\'').to_string())
        }
        "number" | "test_operator" | "regex" => Resolved::Literal(src(node, source).to_string()),
        "ansi_c_string" => Resolved::Blocked {
            construct: Construct::AnsiCEscapes,
            prefix: None,
            at: node,
        },
        "string" | "translated_string" => {
            // A double-quoted string is literal only if every part of it is.
            let mut lit = String::new();
            for c in kids(node) {
                match c.kind.as_str() {
                    "\"" | "$" => {}
                    "string_content" => lit.push_str(src(c, source)),
                    "escape_sequence" => lit.push_str(src(c, source)),
                    other => {
                        let construct = construct_of(other);
                        return match construct {
                            Some(construct) => Resolved::Blocked {
                                construct: named(construct, c, source),
                                prefix: Some(lit),
                                at: c,
                            },
                            // An unknown named child inside a string. Fail closed:
                            // an unrecognised construct is not a literal.
                            None => Resolved::Blocked {
                                construct: Construct::ParseError,
                                prefix: Some(lit),
                                at: c,
                            },
                        };
                    }
                }
            }
            // Quoted: globbing does not apply.
            Resolved::Literal(lit)
        }
        "concatenation" => {
            let mut lit = String::new();
            let mut glob = false;
            for c in kids(node) {
                match resolve(c, source) {
                    Resolved::Literal(s) => lit.push_str(&s),
                    Resolved::Glob(s) => {
                        lit.push_str(&s);
                        glob = true;
                    }
                    Resolved::Blocked { construct, prefix, at } => {
                        let mut p = lit;
                        if let Some(inner) = prefix {
                            p.push_str(&inner);
                        }
                        return Resolved::Blocked {
                            construct,
                            prefix: Some(p),
                            at,
                        };
                    }
                }
            }
            // The braces of `{a,b}.txt` arrive as three separate `word` children, so
            // this is the only place the pattern is visible.
            if has_brace_expansion(&lit) {
                return Resolved::Blocked {
                    construct: Construct::BraceExpansion,
                    prefix: None,
                    at: node,
                };
            }
            if glob { Resolved::Glob(lit) } else { Resolved::Literal(lit) }
        }
        other => match construct_of(other) {
            Some(construct) => Resolved::Blocked {
                construct: named(construct, node, source),
                prefix: None,
                at: node,
            },
            // An unrecognised node in a word position. This is the arm that decides
            // whether a grammar bump quietly widens the gate: a kind this code has
            // never seen becomes UNRESOLVED, not a literal of its own text.
            None => Resolved::Blocked {
                construct: Construct::ParseError,
                prefix: None,
                at: node,
            },
        },
    }
}

/// The construct a node kind stands for, or `None` for "this code does not know
/// this kind" — which is not the same as "this kind is harmless".
fn construct_of(kind: &str) -> Option<Construct> {
    Some(match kind {
        "simple_expansion" | "expansion" => Construct::ParameterExpansion { name: String::new() },
        "command_substitution" => Construct::CommandSubstitutionValue,
        "process_substitution" => Construct::ProcessSubstitutionPath,
        "arithmetic_expansion" => Construct::ArithmeticExpansion,
        "string_expansion" => Construct::AnsiCEscapes,
        "ERROR" => Construct::ParseError,
        _ => return None,
    })
}

/// Fill in the variable name for a parameter expansion, so a refusal can say
/// `$HOME` rather than "a parameter".
fn named(construct: Construct, node: &Node, source: &str) -> Construct {
    match construct {
        Construct::ParameterExpansion { .. } => {
            let name = kids(node)
                .into_iter()
                .find(|c| c.kind.as_str() == "variable_name" || c.kind.as_str() == "special_variable_name")
                .map(|c| src(c, source).to_string())
                .unwrap_or_else(|| src(node, source).trim_start_matches('$').to_string());
            Construct::ParameterExpansion { name }
        }
        other => other,
    }
}

/// Remove backslash escapes and report whether an **unescaped** glob metacharacter
/// survived.
///
/// The escaping matters twice: `e\ f.txt` is one argument whose bytes are `e f.txt`,
/// and `\*` is a literal asterisk rather than a pattern. Getting the second wrong in
/// the safe direction would call every escaped star a glob; getting it wrong in the
/// unsafe direction would call `rm \*` a single-file delete when it is one, and
/// `rm *` a single-file delete when it is not.
/// Does this text contain a brace expansion the shell will turn into several words?
///
/// Bash needs a comma or a `..` range inside the braces, which is what separates
/// `{a,b}.txt` (two arguments) from `find … -exec rm {} \;` (one literal `{}`) and
/// from `git commit -m "{}"`. Getting that wrong in the permissive direction would
/// refuse every `find -exec`; getting it wrong in the other would call two deletions
/// one.
fn has_brace_expansion(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'{' => {
                let mut j = i + 1;
                let mut interesting = false;
                while j < b.len() && b[j] != b'}' {
                    if b[j] == b'\\' {
                        j += 1;
                    } else if b[j] == b',' || (b[j] == b'.' && b.get(j + 1) == Some(&b'.')) {
                        interesting = true;
                    }
                    j += 1;
                }
                if j < b.len() && interesting {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn unescape(s: &str) -> (String, bool) {
    let mut out = String::with_capacity(s.len());
    let mut glob = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '*' | '?' => {
                glob = true;
                out.push(c);
            }
            '[' => {
                glob = true;
                out.push(c);
            }
            c => out.push(c),
        }
    }
    (out, glob)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Normalised {
        normalise(s)
    }

    #[test]
    fn a_plain_pipeline_resolves_to_its_stages_and_their_redirections() {
        let x = n("cargo test 2>&1 | tee out.log");
        assert!(x.is_resolved(), "{:?}", x.unresolved);
        assert_eq!(x.binaries(), vec!["cargo", "tee"]);
        assert_eq!(x.stages[0].argv, vec![Word::Literal("test".into())]);
        // `2>&1` is a descriptor dup, NOT a write to a file named `1`.
        assert_eq!(
            x.stages[0].redirects[0].target,
            RedirectTarget::Descriptor(1)
        );
        assert_eq!(x.stages[0].redirects[0].fd, Some(2));
        assert!(x.stages[0].pipe_out && !x.stages[0].pipe_in);
        assert!(x.stages[1].pipe_in && !x.stages[1].pipe_out);
        // The last stage's stdout is what becomes a tool result.
        assert!(!x.stages[0].stdout_surfaces());
        assert!(x.stages[1].stdout_surfaces());
    }

    #[test]
    fn quote_removal_happens_before_the_decision_which_is_the_whole_point() {
        // GuardFall's class: the harness inspects `"c d.txt"` and the program
        // receives `c d.txt`. The normalised word is what `execve` gets.
        let x = n(r#"cat 'a b.txt' "plain" e\ f.txt"#);
        assert!(x.is_resolved(), "{:?}", x.unresolved);
        assert_eq!(
            x.stages[0].argv,
            vec![
                Word::Literal("a b.txt".into()),
                Word::Literal("plain".into()),
                Word::Literal("e f.txt".into()),
            ]
        );
    }

    #[test]
    fn a_command_assembled_at_runtime_is_unresolved_at_the_program_position() {
        let x = n("X=$(cat f); $X --now");
        assert!(!x.is_resolved());
        // The inner `cat f` IS resolved and listed — this is what `predicate.rs`
        // could not do — while the value it produces is not.
        assert!(x.binaries().contains(&"cat"));
        let p = x.unresolved_programs();
        assert_eq!(p.len(), 1);
        assert!(
            x.unresolved
                .iter()
                .any(|u| u.decides == Decides::Program
                    && matches!(&u.construct, Construct::ParameterExpansion { name } if name == "X"))
        );
        // And the assignment's value is unresolvable for a different reason.
        assert!(
            x.unresolved
                .iter()
                .any(|u| u.decides == Decides::Assignment
                    && u.construct == Construct::CommandSubstitutionValue)
        );
    }

    #[test]
    fn eval_of_a_variable_is_unresolved_and_says_so_at_a_position() {
        let x = n("eval \"$CMD\"");
        assert!(!x.is_resolved());
        assert_eq!(x.binaries(), vec!["eval"]);
        let u = &x.unresolved[0];
        assert_eq!(u.decides, Decides::Argument);
        assert_eq!(u.span.line, 1);
        assert!(u.why.contains("$CMD"), "{}", u.why);
        assert!(x.unresolved_report().contains("parameter_expansion"));
    }

    #[test]
    fn a_here_string_does_not_parse_and_the_recovered_tree_is_not_trusted() {
        // Measured, and the reason `partial` is fatal: tree-sitter-bash 0.25 cannot
        // parse `<<<`, and its RECOVERY reads it as `< 'here string'` — a read of a
        // file named `here string`. A normaliser that reported that data flow would
        // be confidently wrong.
        let x = n("cat >&2 <<< 'here string'");
        assert!(x.partial);
        assert!(!x.is_resolved());
        assert_eq!(x.unresolved[0].construct, Construct::ParseError);
        assert_eq!(x.unresolved[0].decides, Decides::Structure);
    }

    #[test]
    fn a_quoted_heredoc_is_literal_and_an_unquoted_one_is_not() {
        let lit = n("cat <<'EOF' > /tmp/x\nbody $notexpanded\nEOF");
        assert!(lit.is_resolved(), "{:?}", lit.unresolved);
        let r = &lit.stages[0].redirects;
        assert!(matches!(
            r[0].target,
            RedirectTarget::HereDoc { literal: true, .. }
        ));
        // The `> /tmp/x` nested inside the heredoc redirect is still a write.
        assert_eq!(lit.redirect_writes(), vec![&Word::Literal("/tmp/x".into())]);

        let exp = n("cat <<EOF\n$expanded\nEOF");
        assert!(!exp.is_resolved());
        assert_eq!(exp.unresolved[0].construct, Construct::ExpandedHeredoc);
    }

    #[test]
    fn a_glob_is_a_pattern_and_not_an_unresolvable() {
        // Refusing every `*.rs` would make the gate useless without making it safer.
        let x = n("wc -l *.rs");
        assert!(x.is_resolved());
        assert_eq!(x.stages[0].argv[1], Word::Glob("*.rs".into()));
        // But an escaped star is a literal star.
        let y = n(r"rm \*");
        assert_eq!(y.stages[0].argv[0], Word::Literal("*".into()));
    }

    #[test]
    fn a_known_prefix_survives_an_unresolvable_suffix() {
        // `~/.ssh/$KEY` cannot be resolved, and the prefix is already enough to
        // place it in the secret store. Losing it would lose the decision.
        let x = n("cat ~/.ssh/$KEY");
        assert!(!x.is_resolved());
        assert_eq!(
            x.unresolved[0].known_prefix.as_deref(),
            Some("~/.ssh/"),
            "{:?}",
            x.unresolved
        );
    }

    #[test]
    fn a_process_substitution_lists_the_inner_command_and_refuses_the_path() {
        let x = n("diff <(sort a) <(sort b)");
        assert!(!x.is_resolved());
        assert!(x.binaries().contains(&"sort"));
        assert_eq!(
            x.unresolved
                .iter()
                .filter(|u| u.construct == Construct::ProcessSubstitutionPath)
                .count(),
            2
        );
        assert!(
            x.stages
                .iter()
                .any(|s| s.context.contains(&Context::ProcessSubstitution))
        );
    }

    #[test]
    fn a_stage_in_a_function_body_is_not_claimed_to_run() {
        let x = n("nuke() { rm -rf /; }\nls");
        let rm = x.stages.iter().find(|s| s.program_name() == Some("rm")).unwrap();
        assert_eq!(rm.certainty, Certainty::OnlyIfCalled);
        let ls = x.stages.iter().find(|s| s.program_name() == Some("ls")).unwrap();
        assert_eq!(ls.certainty, Certainty::Always);
    }

    #[test]
    fn the_guarded_side_of_a_list_is_conditional() {
        let x = n("test -f x && rm x");
        let rm = x.stages.iter().find(|s| s.program_name() == Some("rm")).unwrap();
        assert_eq!(rm.certainty, Certainty::Conditional);
        assert!(rm.context.contains(&Context::AndThen));
    }

    #[test]
    fn a_prefix_assignment_belongs_to_its_stage_and_a_statement_one_does_not() {
        let x = n("VAR=1 env; Y=2");
        let env = x.stages.iter().find(|s| s.program_name() == Some("env")).unwrap();
        assert_eq!(env.assignments.len(), 1);
        assert_eq!(env.assignments[0].name, "VAR");
        assert_eq!(x.assignments.len(), 1);
        assert_eq!(x.assignments[0].name, "Y");
    }

    #[test]
    fn a_subshell_and_a_loop_are_recorded_as_context_not_flattened_away() {
        let x = n("(cd /x && make) ; for f in *.rs; do wc -l $f; done");
        let make = x.stages.iter().find(|s| s.program_name() == Some("make")).unwrap();
        assert!(make.context.contains(&Context::Subshell));
        let wc = x.stages.iter().find(|s| s.program_name() == Some("wc")).unwrap();
        assert!(wc.context.contains(&Context::Loop));
        // `$f` is unresolvable even though a human can see the loop's list.
        assert!(!x.is_resolved());
    }

    #[test]
    fn an_empty_command_carries_its_denominator_and_does_not_read_as_clean() {
        let x = n("");
        assert_eq!(x.bytes, 0);
        assert!(x.stages.is_empty());
        // `is_resolved` is true — there is nothing unresolvable in nothing — and
        // that is why a caller must look at `stages`/`bytes` too. §2.2: a zero with
        // no denominator is not a measurement.
        assert!(x.is_resolved());
    }

    #[test]
    fn a_declaration_and_an_unset_are_stages_with_their_keyword_as_the_program() {
        let x = n("export Z=3; unset Q");
        assert_eq!(x.binaries(), vec!["export", "unset"]);
        let exp = &x.stages[0];
        assert_eq!(exp.assignments[0].name, "Z");
    }

    #[test]
    fn a_write_redirect_stops_stdout_from_surfacing() {
        let a = n("printf hi > log");
        assert!(!a.stages[0].stdout_surfaces());
        // fd 2 to a file leaves stdout alone.
        let b = n("printf hi 2> log");
        assert!(b.stages[0].stdout_surfaces());
        let c = n("printf hi >> log");
        assert!(!c.stages[0].stdout_surfaces());
    }

    #[test]
    fn backquotes_are_command_substitution_too() {
        let x = n("echo `id -u`");
        assert!(!x.is_resolved());
        assert!(x.binaries().contains(&"id"));
        assert!(
            x.unresolved
                .iter()
                .any(|u| u.construct == Construct::CommandSubstitutionValue)
        );
    }

    #[test]
    fn an_expansion_with_a_default_is_still_unresolved() {
        // `${x:-def}` resolves to one of two values and the grammar cannot say
        // which, so claiming `def` would be a guess dressed as a fact.
        let x = n("cat ${x:-def}");
        assert!(!x.is_resolved());
        assert!(matches!(
            &x.unresolved[0].construct,
            Construct::ParameterExpansion { .. }
        ));
    }

    #[test]
    fn nested_substitution_past_one_level_is_still_seen() {
        // `predicate.rs` names this as its own limit: "$(...) nesting past one level
        // are unhandled".
        let x = n("echo $(dirname $(readlink -f /proc/self/exe))");
        assert!(x.binaries().contains(&"dirname"));
        assert!(x.binaries().contains(&"readlink"));
    }
}

/// **The stable shape of a command**, with its literals replaced by holes.
///
/// `cd /a/b && grep -n "struct CallRow" -A 22 crates/tui/src/app.rs` and the same
/// thing over another file at other line numbers are one shape and two commands.
/// The operator's observation, after answering the same question for the fifth
/// time:
///
/// > *"i wonder if it is possible to cache the approved/denied shapes somehow - i
/// > see this patterns like cd <path>; sed <lines> path or grep instead of sed.
/// > looks cachable especially on tree sitter level"*
///
/// # What is kept, and why exactly that
///
/// The program and its FLAGS, because a flag is where a command changes kind:
/// `sed -n` prints and `sed -i` rewrites the file, `grep -r` walks a tree and
/// `rm -f` stops asking. A shape that folded those together would approve a write
/// because a read of the same name was approved once, which is the drift this is
/// supposed to prevent rather than cause.
///
/// Everything else becomes a hole. Operands are `<arg>`, and a flag's VALUE is
/// `<v>` — `-A 22` and `-A 3` are the same shape, `-A` and `-B` are not.
/// Redirection operators are kept and their targets are holes, so `> <file>` and
/// `>> <file>` stay apart.
///
/// # What it is not
///
/// Not a security decision on its own and not a cache key yet: it is the label
/// under which the same question gets asked repeatedly, so the asking can be
/// counted before anything is built on it. An unresolved word is `<?>` and a shape
/// containing one is not the same shape as the resolved spelling, because the
/// thing that could not be read is exactly where a difference would hide.
fn is_count_flag(t: &str) -> bool {
    t.len() > 1 && t.starts_with('-') && t[1..].chars().all(|c| c.is_ascii_digit())
}

pub fn shape(n: &Normalised) -> String {
    let mut out = String::new();
    for (i, st) in n.stages.iter().enumerate() {
        if i > 0 {
            out.push_str(if st.pipe_in { " | " } else { " ; " });
        }
        out.push_str(st.program.text().unwrap_or("<?>"));
        for w in &st.argv {
            out.push(' ');
            match w.text() {
                // **A COUNT is not a flag.** `head -26`, `tail -5`, `head -30`:
                // the dash is punctuation and the number is an operand wearing it.
                // Keeping them apart would make every `head -N` its own shape,
                // which is most of what recurs.
                Some(t) if is_count_flag(t) => out.push_str("-<n>"),
                // A flag is part of the shape, because a flag is where a command
                // changes kind: `sed -n` prints and `sed -i` rewrites. `--opt=value`
                // keeps the option and holes the value — one token, two halves.
                Some(t) if t.starts_with('-') && t.len() > 1 => match t.split_once('=') {
                    Some((opt, _)) => out.push_str(&format!("{opt}=<v>")),
                    None => out.push_str(t),
                },
                // Everything else is a hole, flag value or operand alike. Telling
                // those two apart needs a per-program table of which flags take a
                // value, and buys nothing: `-A 22` and `-A 3` collapse either way,
                // and the arity of the stage keeps genuinely different calls apart.
                Some(_) => out.push_str("<arg>"),
                // Unresolved stays visible: a shape that hid it would pool "we could
                // not read this" with a spelling somebody has approved.
                None => out.push_str("<?>"),
            }
        }
        for r in &st.redirects {
            out.push(' ');
            out.push_str(r.op.as_str());
            out.push_str(match &r.target {
                RedirectTarget::File(_) => " <file>",
                RedirectTarget::Descriptor(_) => "&<fd>",
                RedirectTarget::HereDoc { .. } => " <<heredoc",
                _ => " <in>",
            });
        }
    }
    out
}


#[cfg(test)]
mod redirect_ownership {
    //! Which stage a hoisted redirection belongs to, and that the commands the
    //! grammar nests after a here-document operator are not lost. Measured on
    //! the operator's corpus 2026-09-17 (plan §4b): 5,675 here-documents read
    //! as `cd`'s stdin, and `cat <<'EOF' | bash` normalised to a `cat`.
    use super::*;

    fn heredoc_of(s: &Stage) -> Option<&str> {
        s.redirects.iter().find_map(|r| match &r.target {
            RedirectTarget::HereDoc { body, .. } => Some(body.as_str()),
            _ => None,
        })
    }

    fn programs(n: &Normalised) -> Vec<&str> {
        n.stages.iter().map(|s| s.program_name().unwrap_or("?")).collect()
    }

    #[test]
    fn a_trailing_heredoc_belongs_to_the_last_command_of_the_list() {
        let n = normalise("cd /x && python3 - <<'PY'\nprint(1)\nPY");
        assert_eq!(programs(&n), ["cd", "python3"]);
        assert_eq!(heredoc_of(&n.stages[0]), None, "`cd` reads nothing");
        assert_eq!(heredoc_of(&n.stages[1]), Some("print(1)\n"));
    }

    #[test]
    fn a_trailing_file_redirect_belongs_to_the_last_command_of_the_pipeline() {
        let n = normalise("a | b > f");
        assert_eq!(programs(&n), ["a", "b"]);
        assert!(n.stages[0].redirects.is_empty());
        assert_eq!(n.stages[1].redirect_writes(), vec![&Word::Literal("f".into())]);
        // The substitution inside the last member ran to make a word; the
        // redirection is not its.
        let n = normalise("a && b $(c) > f");
        assert_eq!(programs(&n), ["a", "b", "c"]);
        assert_eq!(n.stages[1].redirect_writes().len(), 1);
        assert!(n.stages[2].redirects.is_empty());
    }

    #[test]
    fn a_compound_body_gives_the_fact_to_every_stage_of_the_body_only() {
        let n = normalise("ls; { a; b; } > f");
        assert_eq!(programs(&n), ["ls", "a", "b"]);
        assert!(n.stages[0].redirects.is_empty(), "`ls` is before the group");
        assert_eq!(n.stages[1].redirect_writes().len(), 1);
        assert_eq!(n.stages[2].redirect_writes().len(), 1);
    }

    #[test]
    fn a_pipe_after_the_heredoc_operator_is_a_pipe_into_a_real_stage() {
        let n = normalise("cat <<'EOF' | bash\necho hi\nEOF");
        assert!(n.is_resolved(), "{:?}", n.unresolved);
        assert_eq!(programs(&n), ["cat", "bash"]);
        assert!(n.stages[0].pipe_out, "cat's stdout is the pipe");
        assert!(n.stages[1].pipe_in);
        assert_eq!(heredoc_of(&n.stages[0]), Some("echo hi\n"));

        let n = normalise("cat <<'EOF' | sudo bash | wc -l\nx\nEOF");
        assert_eq!(programs(&n), ["cat", "sudo", "wc"]);
        assert!(n.stages[1].pipe_in && n.stages[1].pipe_out);
        assert!(n.stages[2].pipe_in && !n.stages[2].pipe_out);
    }

    #[test]
    fn a_guarded_command_after_the_heredoc_operator_is_a_stage() {
        let n = normalise("cat > f.sh <<'EOF' && chmod +x f.sh\nbody\nEOF");
        assert!(n.is_resolved(), "{:?}", n.unresolved);
        assert_eq!(programs(&n), ["cat", "chmod"]);
        assert_eq!(n.stages[0].redirect_writes(), vec![&Word::Literal("f.sh".into())]);
        assert_eq!(heredoc_of(&n.stages[0]), Some("body\n"));
        assert!(n.stages[1].context.contains(&Context::AndThen));
        assert_eq!(n.stages[1].argv.len(), 2);
    }

    #[test]
    fn a_stage_after_the_heredoc_body_is_unchanged() {
        let n = normalise("cat <<'EOF' | bash\nx\nEOF\nls");
        assert_eq!(programs(&n), ["cat", "bash", "ls"]);
        assert!(!n.stages[2].pipe_in);
    }
}

#[cfg(test)]
mod unresolved_reporting {
    //! **A refusal nobody reads is a refusal that did not say why.** The
    //! operator, 2026-09-17, on a `not_run` body: *"the explanation lines are
    //! repeated over and over, also 66 lines???"*
    use super::*;

    /// One variable, used five times, is one reason — not five copies of the
    /// same 220-character sentence.
    #[test]
    fn one_cause_is_explained_once_however_many_places_it_has() {
        let n = normalise(
            "R=/tmp/x; grep q $R/a.rs $R/b.rs $R/c.rs; ls $R/d $R/e",
        );
        let report = n.unresolved_report();
        assert!(n.unresolved.len() >= 5, "{} entries", n.unresolved.len());

        // The sentence, once.
        let sentence = "Substitute the value and pass the literal";
        assert_eq!(
            report.matches(sentence).count(),
            1,
            "the explanation repeated:\n{report}"
        );
        // Every place still reachable, up to the cap, and the rest counted
        // rather than dropped — a summary may never be silent about what it
        // summarised.
        assert!(report.contains("… and 1 more place(s)"), "{report}");
        assert!(report.lines().count() <= 7, "{} lines:\n{report}", report.lines().count());
    }

    /// Two different causes keep two explanations: the grouping is by the
    /// sentence, not by the construct, because a reader is being spared the
    /// text and the text is what differs.
    #[test]
    fn different_causes_keep_their_own_explanations() {
        let n = normalise("cat $A && ls $(which foo)");
        let report = n.unresolved_report();
        assert!(report.contains("parameter_expansion"), "{report}");
        assert!(report.contains("command_substitution"), "{report}");
        // Two headers, one per cause.
        assert_eq!(
            report.lines().filter(|l| l.starts_with("  ") && !l.starts_with("      ")).count(),
            2,
            "{report}"
        );
    }

    #[test]
    fn a_resolved_command_reports_nothing() {
        assert_eq!(normalise("ls /tmp").unresolved_report(), "");
    }
}
