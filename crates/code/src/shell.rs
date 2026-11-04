//! Layer 2 of `docs/boundary-and-adjudication.md`: normalise a shell command
//! through the grammar, and name what the grammar cannot resolve.
//!
//! # The vulnerability class this exists for
//!
//! Every surveyed harness that gates on a shell command decides on a *string the
//! shell will reinterpret*. `crates/tools/src/exec/predicate.rs` concedes it in its
//! own words - *"not a shell parser… `eval`, here-docs, aliases, `$(...)` nesting
//! past one level are unhandled"* - and calls itself a diagnosis rather than the
//! mechanism.
//!
//! This is not a local worry. GuardFall (Adversa AI) named it as a
//! structural class and measured it against eleven popular open-source coding
//! agents: ten of eleven were bypassable, because the agent inspects raw
//! command text while the shell performs quote removal, expansion and argument
//! construction *after* that inspection. The one that held up reads the command the
//! way bash will before deciding, and keeps a hard refuse-list underneath. That is
//! this module plus `letibot_tools::intent`.
//!
//! # The rule that makes it honest, and it is the whole point
//!
//! A grammar buys a real decomposition and it buys a real limit. `eval "$X"`,
//! `$CMD --now`, `cat $FILE` and `<(...)` are not commands whose meaning is
//! *hidden*; they are commands whose meaning does not exist yet. So:
//!
//! > A construct the grammar cannot resolve is reported as
//! > [`Unresolved`] - naming the construct, what decision it denies, and its
//! > position - and is never classified as safe.
//!
//! [`Normalised::is_resolved`] is the single predicate a gate reads, and
//! `letibot_tools::adjudicate` maps `false` to `NotRun`: *nobody could decide*.
//! Never `Admit`, and never `Denied` either, because a construct nobody could read
//! is not a decision anybody made. A normaliser that answered "looks fine" for text
//! it could not parse would be the empty-haystack bug with a parse tree attached.
//!
//! What that costs is stated rather than hidden: `cat $FILE` is refused, and the
//! refusal carries the fix
//! - resolve the variable and pass the literal path. The alternative is deciding
//! about a target nobody can name, which is the theatre.
//!
//! # What it deliberately does NOT do
//!
//! It does not out-parse the shell, and it holds no table of programs. Which
//! token sits in the command-name position is grammar; what that token *means*  -
//! that `sudo` wraps the next word, that `cat` surfaces its input, that `scp` leaves
//! the box - is a table about this host's software, and a table is policy. Policy
//! lives one crate up, in `letibot_tools::intent`, where the deny-list and the
//! flow rule live with it. This crate keeps the property the rest of it has: it
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

use letibot_transcript::text::clip_chars;
use rano::syntax::{Lang, Node, Stream};

// ---------------------------------------------------------------------------
// Positions
// ---------------------------------------------------------------------------

/// Where something is in the command text: byte offsets *and* line/column, because the offsets are
/// what a caller slices with and the line/column is what a refusal prints. A refusal that says
/// "unresolvable construct" without saying where is a refusal nobody can act on.
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
        write!(
            f,
            "{}:{} (bytes {}..{})",
            self.line, self.column, self.start, self.end
        )
    }
}

// ---------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------

/// One argument, command name or redirection target, resolved as far as the
/// grammar resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Word {
    /// The bytes are known: quotes are removed and backslash escapes applied, because that is what
    /// the shell passes to `execve` - deciding on `"a b"` when the program receives `a b` is the
    /// GuardFall mistake in miniature. A leading `~` stays as written: `$HOME` is not in the text.
    Literal(String),
    /// A pathname pattern: the pattern is known and the file set is not, and those are different
    /// facts. Not [`Unresolved`] - a glob does not change which program runs - but it changes
    /// breadth, which is why it is its own variant: `rm -rf *` must show a set nobody enumerated.
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
    /// `$X`, `${X}`, `${X:-default}`: the value is not in the text. The default form is included on
    /// purpose - it is used only when `X` is unset, so the resolved value is one of two and the
    /// grammar cannot say which.
    ParameterExpansion { name: String },
    /// `$( ... )` or a backquote: the command inside is resolved and appears in
    /// [`Normalised::stages`] with [`Context::CommandSubstitution`], and what is unresolvable is the
    /// value it produces, which is what the enclosing word becomes.
    CommandSubstitutionValue,
    /// `<(…)` or `>(…)`. The inner command is resolved and listed; the *path* the
    /// shell invents for it (`/dev/fd/63`) is invented at runtime.
    ProcessSubstitutionPath,
    /// `$((…))`.
    ArithmeticExpansion,
    /// A here-document whose body the parse did not hand back at all, so whether it is a script
    /// is unknown. A body the shell expands is [`Construct::ExpandedHeredoc`] instead.
    UnreadHeredoc,
    /// A here-document whose delimiter was unquoted, so the shell expands the body.
    /// A `<<'EOF'` body is literal and is resolved.
    ExpandedHeredoc,
    /// `$'…'`. The grammar hands over the bytes but not their decoded value, and
    /// reporting an undecoded `\n` as the literal would be a wrong claim about what
    /// the program receives.
    AnsiCEscapes,
    /// `{a,b}.txt`, `{1..5}`: one word in the text and several arguments after the shell is done.
    /// Decidable from the text alone, but named rather than guessed because the grammar hands it
    /// over as concatenated words, so the merged literal reads as a harmless filename.
    BraceExpansion,
    /// A whole command where a WORD was expected: `time ( ... )`, `! ( ... )`. The grammar hangs the
    /// compound off the `command` node in the argument position and its commands RUN, but how a
    /// compound's value relates to this stage's argv cannot be said.
    CompoundArgument,
    /// The parse contains an `ERROR` or `MISSING` node: the grammar could not read this text. The
    /// least obvious one - tree-sitter recovers, handing back a plausible tree, so every claim
    /// about the recovered region is a guess, and the whole command is unresolvable.
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
            Construct::UnreadHeredoc => "unread_heredoc",
            Construct::AnsiCEscapes => "ansi_c_escapes",
            Construct::BraceExpansion => "brace_expansion",
            Construct::CompoundArgument => "compound_argument",
            Construct::ParseError => "parse_error",
        }
    }
}

/// Which decision the unresolvable construct denies, graded because the three are not equally bad:
/// `Program` means nobody knows what will run, while `Argument` and `RedirectTarget` mean the
/// program is known and its target is not.
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
    /// The part of the word that IS known, when the unresolvable construct is concatenated with
    /// literal text: `/etc/$NAME` knows `/etc/`. Decision-relevant rather than cosmetic, since
    /// `~/.ssh/$KEY`'s known prefix already places it in the secret store.
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
    /// `>&`, `&>`, `<&` - duplicate or close a descriptor.
    Duplicate,
    /// `<<`, `<<-`
    HereDoc,
    /// `<<<` - a here-string. Feeds one word to stdin.
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
        matches!(
            self,
            RedirectOp::Write | RedirectOp::Append | RedirectOp::ReadWrite
        )
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
    /// the shell will expand the body - and then the body's own expansions are
    /// separately unresolvable.
    HereDoc {
        literal: bool,
        body: String,
    },
    /// A here-string's word: it is stdin, not a file. Reporting it as `RedirectTarget::File` would
    /// invent a read of a file named after the text, which is what this grammar's error recovery
    /// does with `<<<` when another redirection is present.
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

/// Where a stage sits in the command's structure: a list, because the contexts nest and the nesting
/// is what a caller needs. A `rm -rf /` inside a function body inside a subshell is not the same
/// fact as one at the top level.
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
    /// Inside `$(…)` or a backquote. The stage runs.
    CommandSubstitution,
    /// Inside `<(…)` or `>(…)`. The stage runs.
    ProcessSubstitution,
    /// The body of a function definition. The stage does not run unless the
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

/// Whether the stage runs when the command runs. "This command contains `rm -rf /`" and "this
/// command runs `rm -rf /`" are different claims, and a refusal that makes the second when only
/// the first is true will be argued with - correctly.
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
    /// `VAR=1 cmd` - the assignments that apply to this stage only.
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
    /// The program's name with any directory stripped: `/usr/bin/cat` becomes `cat`. `None` when the
    /// program is unresolved, which the caller must handle rather than default - that is the whole limit.
    pub fn program_name(&self) -> Option<&str> {
        let p = self.program.literal()?;
        Some(p.rsplit('/').next().unwrap_or(p))
    }

    /// Whether this program name can be shadowed by the shell that runs it: a bare name is resolved
    /// through aliases, functions and `PATH`, none of which are in the text, so a parser can be
    /// right about the text and wrong about its meaning. `false` for a name holding a `/`.
    pub fn program_is_shadowable(&self) -> Option<bool> {
        self.program.literal().map(|p| !p.contains('/'))
    }

    /// Files this stage writes through a redirection, which is the half that is
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

    /// Whether this stage's stdout reaches the caller, and therefore becomes a tool result and a
    /// row: a stage piped onward or redirected to a file does not surface it here. False for a
    /// `> file`, false mid-pipeline, true for the last stage of a pipeline with no redirection.
    pub fn stdout_surfaces(&self) -> bool {
        if self.pipe_out {
            return false;
        }
        !self.redirects.iter().any(|r| {
            r.op.writes()
                && matches!(r.fd, None | Some(1))
                && matches!(r.target, RedirectTarget::File(_))
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
    /// Read this before reading anything else. Empty means the grammar resolved
    /// the whole command.
    pub unresolved: Vec<Unresolved>,
    /// The parse contained an `ERROR` or `MISSING` node. Also recorded as an [`Unresolved`] with
    /// [`Construct::ParseError`], so a caller reading only one of the two still fails closed: one
    /// mechanism for a safety property ships broken the first time somebody refactors it.
    pub partial: bool,
    /// Bytes examined. The denominator: "0
    /// stages" over 0 bytes and "0 stages" over 4 KiB are different facts, and an
    /// empty command must not read as a clean one.
    pub bytes: usize,
    /// The loop variables an unrolled `for` has bound, so the body reads `$f` as the value the
    /// shell gives it on that pass - see [`unrolled_for`]. A state of the WALK rather than a fact
    /// about the command, which is why it is private and empty when [`normalise`] returns.
    binds: Vec<(String, String)>,
}

impl Normalised {
    /// The predicate a gate reads: true only when nothing in the command needs a runtime value and
    /// the grammar read all of it. `cat $FILE` is not resolved - the program is known and the target
    /// is not, and the target is what the decision is about.
    pub fn is_resolved(&self) -> bool {
        self.unresolved.is_empty() && !self.partial
    }

    /// Every program that will run, in source order, skipping the unresolved ones  -
    /// so a caller must consult [`Normalised::is_resolved`] first or it is reading a
    /// list with silent holes. [`Normalised::unresolved_programs`] is the other half.
    pub fn binaries(&self) -> Vec<&str> {
        self.stages
            .iter()
            .filter_map(|s| s.program_name())
            .collect()
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
        self.stages
            .iter()
            .flat_map(|s| s.redirect_writes())
            .collect()
    }

    /// Files read through a redirection, anywhere in the command.
    pub fn redirect_reads(&self) -> Vec<&Word> {
        self.stages
            .iter()
            .flat_map(|s| s.redirect_reads())
            .collect()
    }

    /// The unresolvable constructs, for a refusal body: one explanation per distinct reason, with
    /// the places it applies listed under it, because the refusal has to carry the fix and the fix
    /// is always the same shape - resolve it in the caller and pass the literal.
    pub fn unresolved_report(&self) -> String {
        if self.unresolved.is_empty() {
            return String::new();
        }
        // Grouped by the sentence itself rather than by `construct`, because two kinds can share a
        // reason and one kind can give different ones: the text is what a reader is spared, so the
        // text is the key. First appearance first, which is how somebody reads their own command.
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

/// How many positions one reason lists before it counts the rest: four is enough to show this is
/// not a one-off and few enough that a variable used thirty times does not print thirty lines. The
/// count is the disclosure - a refusal may summarise, but never silently.
const MAX_PLACES_SHOWN: usize = 4;

/// Parse `source` as bash and normalise it. Never fails and never panics on input: an unparseable
/// command comes back with `partial` set and a [`Construct::ParseError`], a result saying nobody
/// could read it rather than an error saying the normaliser broke.
pub fn normalise(source: &str) -> Normalised {
    let mut out = Normalised {
        source: source.to_string(),
        stages: Vec::new(),
        assignments: Vec::new(),
        unresolved: Vec::new(),
        partial: false,
        bytes: source.len(),
        binds: Vec::new(),
    };

    let mut stream = Stream::new(Lang::Bash);
    stream.push(source);

    let Some(root) = stream.root() else {
        out.partial = true;
        out.unresolved.push(Unresolved {
            construct: Construct::ParseError,
            decides: Decides::Structure,
            span: Span {
                start: 0,
                end: source.len(),
                line: 1,
                column: 0,
            },
            text: clip(source, 200),
            known_prefix: None,
            stage: None,
            why: "the bash grammar returned no tree at all, so nothing here has been \
                  read and no claim about this command is available"
                .into(),
        });
        return out;
    };

    // `root.has_error` with nothing left to name means every error it left is one the text
    // vouches for, which is the read-write redirect: the command reads.
    if let Some(bad) = first_error(&root, source, None) {
        out.partial = true;
        out.unresolved.push(Unresolved {
            construct: Construct::ParseError,
            decides: Decides::Structure,
            span: Span::of(bad),
            text: clip(src(bad, source), 120),
            known_prefix: None,
            stage: None,
            why: unreadable_why(bad, source),
        });
    }

    let mut ctx: Vec<Context> = Vec::new();
    walk(&root, source, &mut ctx, &mut out);
    out
}

/// What the grammar left behind, or `None` when every error in it is a shape whose recovery the
/// text around it verifies. The first region it could not read is what the refusal names: "somewhere
/// in these 4 KiB" is not a position.
fn first_error<'a>(node: &'a Node, source: &str, parent: Option<&'a Node>) -> Option<&'a Node> {
    // tree-sitter's `is_error()` is a node KIND here - rano's `Node` is plain data and
    // names it rather than carrying a flag for it.
    if node.kind == "ERROR" || node.is_missing {
        return (!recovered_read_write(node, source, parent)).then_some(node);
    }
    if !node.has_error {
        return None;
    }
    node.children
        .iter()
        .find_map(|c| first_error(c, source, Some(node)))
}

/// `3<>file`: the `<` this grammar has a token for, then the `>` recovered as an ERROR inside the
/// same `file_redirect`. Excused only when that redirect is whole - one destination, no second word
/// the recovery swallowed - because a credited recovery that lost a command is worse than a refusal.
fn recovered_read_write(node: &Node, source: &str, parent: Option<&Node>) -> bool {
    let whole = parent.is_some_and(|p| {
        p.kind == "file_redirect"
            && p.children
                .iter()
                .filter(|c| c.field.as_deref() == Some("destination"))
                .count()
                == 1
    });
    whole
        && node.kind == "ERROR"
        && src(node, source) == ">"
        && source.get(..node.start).is_some_and(|s| s.ends_with('<'))
}

/// `<>file` with no descriptor: the `<` is the error and `>file` is a second redirect beside it, so
/// the one redirect the shell opens is not in the tree and the command is refused.
fn read_write_without_descriptor(node: &Node, source: &str) -> bool {
    src(node, source) == "<" && source.get(node.end..).is_some_and(|s| s.starts_with('>'))
}

/// A here-document operator followed by another operator on its line: the grammar hangs the rest of
/// the line off the redirect, so a pipeline's `|` arrives inside the heredoc's `2>&1`.
fn heredoc_then_operator(node: &Node, source: &str) -> bool {
    let operator = matches!(src(node, source), "|" | "|&" | "&&" | "||" | ";" | "&");
    let line_start = source
        .get(..node.start)
        .and_then(|s| s.rfind('\n'))
        .map_or(0, |i| i + 1);
    operator
        && source
            .get(line_start..node.start)
            .is_some_and(|s| s.contains("<<"))
}

/// `%(…)`: this grammar takes the `(` for a subshell and lists its contents as commands that run.
/// The error may hold the `%`, end at it, or wrap the whole command around it.
fn percent_bracket(node: &Node, source: &str) -> bool {
    let text = src(node, source);
    let opens = source.get(node.end..).is_some_and(|s| s.starts_with('('));
    text.contains("%(")
        || (text.ends_with('%') && opens)
        || (source.get(..node.start).is_some_and(|s| s.ends_with('%')) && opens)
}

/// The sentence a refusal carries for the region the grammar could not read. Three of its shapes are
/// the grammar's own limits rather than text nobody wrote, and each has a writing that reads, so the
/// refusal names the shape and that writing instead of calling the whole tree a guess.
fn unreadable_why(node: &Node, source: &str) -> String {
    if read_write_without_descriptor(node, source) {
        return "the shell reads `<>` as ONE read-write redirect and this grammar has no \
                operator for it: with no descriptor the recovery is two redirects (`<` and \
                `>file`) where the shell opens one. Write the descriptor you mean in front of \
                the operator - `3<>file` is one redirect on fd 3 and it reads"
            .into();
    }
    if percent_bracket(node, source) {
        return "the shell refuses this text too - `(` after an unquoted `%` is a syntax error \
                - and this grammar recovers it as a subshell, so what is inside the brackets \
                would be listed as a command that runs. Quote the word (`--format='%(refname)'`) \
                and both the shell and this read it"
            .into();
    }
    if heredoc_then_operator(node, source) {
        return "this grammar loses the rest of a command's line when a here-document operator \
                is followed by another redirect and then a pipe: `cat <<'EOF' 2>&1 | tail` \
                came back with `| tail` inside the `2>&1`, so the pipeline is a guess. Write \
                the redirect before the `<<` - `cat 2>&1 <<'EOF' | tail` is the same command \
                - and it reads"
            .into();
    }
    "the bash grammar could not read this. tree-sitter RECOVERS rather than failing, so the \
     tree it produced here is a guess and every claim drawn from it would be one too — \
     measured: `<<<` recovers as a read of a file whose name is the here-string's text"
        .into()
}

fn src<'a>(node: &Node, source: &'a str) -> &'a str {
    source.get(node.start..node.end).unwrap_or("")
}

/// The child in field `name`: by field rather than by kind or position, because the grammar states
/// which child a field names, and matching kinds in the order they appear guesses at a fact the
/// grammar already has.
fn child_field<'a>(node: &'a Node, name: &str) -> Option<&'a Node> {
    node.children
        .iter()
        .find(|c| c.field.as_deref() == Some(name))
}

/// A span's label: the source, trimmed, at most `n` characters and never inside one.
fn clip(s: &str, n: usize) -> String {
    clip_chars(s.trim(), n)
}

fn certainty_of(ctx: &[Context]) -> Certainty {
    if ctx.iter().any(|c| matches!(c, Context::FunctionBody(_))) {
        return Certainty::OnlyIfCalled;
    }
    if ctx.iter().any(|c| {
        matches!(
            c,
            Context::Loop
                | Context::Conditional
                | Context::AndThen
                | Context::OrElse
                | Context::Test
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
            // decide `Stage::stdout_surfaces` - which is the structural half of
            // The "may never enter the transcript".
            let members: Vec<&Node> = kids(node)
                .into_iter()
                .filter(|c| c.kind != "|" && c.kind != "|&")
                .collect();
            let last = members.len().saturating_sub(1);
            ctx.push(Context::Pipeline);
            for (i, m) in members.iter().enumerate() {
                pipe_member(m, source, ctx, out, i > 0, i < last);
            }
            ctx.pop();
        }
        "redirected_statement" => {
            redirected(node, source, ctx, out, false, false);
        }
        "subshell" => push_walk(node, source, ctx, out, Context::Subshell),
        "compound_statement" => push_walk(node, source, ctx, out, Context::Group),
        "list" => {
            // `a && b` / `a || b`: the left side runs unconditionally and the right side is guarded.
            // Recording that asymmetry is the difference between "this command runs rm" and "this
            // command runs rm if the first part failed".
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
        "for_statement" => {
            // A `for` over a list the command spells out is N commands, not one command
            // whose variable nobody can know - see [`unrolled_for`].
            if !unrolled_for(node, source, ctx, out) {
                push_walk(node, source, ctx, out, Context::Loop);
            }
        }
        "c_style_for_statement" | "while_statement" | "until_statement" => {
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
            scan_substitutions(node, source, ctx, out, &[]);
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

/// A `for` whose list is in the text is that many commands, not one unreadable one: `$f` on a pass
/// is the next word of a list written above, so the body is walked once per value. A glob, an
/// expansion, a value that would split, or more than `MOST_ITERATIONS` values stays unresolved.
fn unrolled_for(node: &Node, source: &str, ctx: &mut Vec<Context>, out: &mut Normalised) -> bool {
    const MOST_ITERATIONS: usize = 32;
    let Some(var) = child_field(node, "variable").map(|n| src(n, source).to_string()) else {
        return false;
    };
    let Some(body) = child_field(node, "body") else {
        return false;
    };
    let mut values: Vec<String> = Vec::new();
    for child in kids(node) {
        if child.field.as_deref() != Some("value") {
            continue;
        }
        // A brace list in the list position is the words bash makes of it, so
        // `for i in {1..3}` is three passes like `for i in 1 2 3`.
        let words = match brace_words(child, source) {
            Some(words) => words,
            None => match resolve(child, source, &out.binds) {
                Resolved::Literal(v) => vec![Word::Literal(v)],
                _ => return false,
            },
        };
        for w in words {
            // A brace alternative the shell would glob is a file set, not a value.
            let Word::Literal(v) = w else {
                return false;
            };
            if !one_word(&v) {
                return false;
            }
            values.push(v);
        }
        if values.len() > MOST_ITERATIONS {
            return false;
        }
    }
    if values.is_empty() || values.len() > MOST_ITERATIONS {
        return false;
    }
    for v in values {
        out.binds.push((var.clone(), v));
        push_walk(body, source, ctx, out, Context::Loop);
        out.binds.pop();
    }
    true
}

/// Whether a bound loop value can stand for ONE word in the body: whitespace would be field
/// splitting and a glob character pathname expansion, both of which a single [`Word::Literal`]
/// cannot say, so such a loop is left unresolved rather than read as something narrower.
fn one_word(v: &str) -> bool {
    !v.is_empty()
        && !v
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '*' | '?' | '['))
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

/// A command with redirections attached: they belong to the command, not the statement, so no
/// generic walk finds them. The grammar hoists a trailing redirection above the whole list and the
/// shell gives it to the LAST command; a compound body has none, and every stage gets the fact.
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
                    scan_substitutions(k, source, ctx, out, &[]);
                }
            }
            tails.push(child);
        } else {
            scan_substitutions(child, source, ctx, out, &[]);
        }
    }
    // The stages the body itself produced, not the ones inside a `$(…)` in one
    // of its arguments: those ran to make a word and the redirection is not
    // theirs.
    let own: Vec<usize> = (before..out.stages.len())
        .filter(|&i| {
            !out.stages[i].context[depth.min(out.stages[i].context.len())..]
                .iter()
                .any(|c| {
                    matches!(
                        c,
                        Context::CommandSubstitution | Context::ProcessSubstitution
                    )
                })
        })
        .collect();
    let single_owner = matches!(
        body_kind,
        "command"
            | "declaration_command"
            | "unset_command"
            | "pipeline"
            | "list"
            | "redirected_statement"
    );
    let owner = if single_owner {
        own.last().copied()
    } else {
        None
    };
    match owner {
        Some(i) => out.stages[i].redirects.extend(reds),
        None => {
            // A redirected compound statement (`{ ...; } > f`): there is no single stage to own the
            // redirection, so the fact is recorded against every stage of the body, which is what
            // the shell does to their shared stdout.
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
        && !matches!(
            k.kind.as_str(),
            "heredoc_start"
                | "heredoc_body"
                | "heredoc_end"
                | "file_redirect"
                | "herestring_redirect"
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
            pipe_member(m, source, ctx, out, i > 0 || leading, i < last);
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
                .map(|c| {
                    word_of(
                        c,
                        source,
                        out,
                        Decides::RedirectTarget,
                        Some(out.stages.len().saturating_sub(1)),
                    )
                })
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
                    && let Some(r) = file_redirect(child, source, out)
                {
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
    let mut op = redirect_op(node)?;
    // `3<>file`: this grammar has no read-write token, so the `>` arrives as an ERROR beside the
    // `<` and the operator is read from the text the two characters spell.
    if op == RedirectOp::Read
        && kids(node)
            .into_iter()
            .any(|c| recovered_read_write(c, source, Some(node)))
    {
        op = RedirectOp::ReadWrite;
    }
    let fd = child_field(node, "descriptor").and_then(|n| src(n, source).parse::<u32>().ok());
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
    // The grammar hands the body back as an argument word, or inside a sibling redirect or a
    // pipeline, whenever something on the line comes between the delimiter and the body: an empty
    // `heredoc_body` is "not in this node", not "nothing", and the text is read instead.
    let body = match body_node.map(|b| src(b, source)).filter(|b| !b.is_empty()) {
        Some(b) => Some(b.to_string()),
        None => heredoc_body_from_text(node, source),
    };
    let body = match body {
        // `<<-` strips the leading tabs of every line, and the grammar strips only the first
        // line's, so what it hands back is not the bytes the program reads.
        Some(b) if heredoc_strips_tabs(node) => strip_heredoc_tabs(&b),
        Some(b) => b,
        // A body this parse did not hand back at all: nothing was read, and an empty body would
        // claim a program was fed nothing.
        None => {
            out.unresolved.push(Unresolved {
                construct: Construct::UnreadHeredoc,
                decides: Decides::RedirectTarget,
                span: Span::of(node),
                text: clip(src(node, source), 120),
                known_prefix: None,
                stage: Some(out.stages.len()),
                why: "the here-document's body is not in this parse, so the bytes the program \
                      reads are unknown. Write the body as a file, or pass it on stdin of a \
                      command whose text this can read"
                    .into(),
            });
            String::new()
        }
    };
    if !literal && (heredoc_expansion_child(body_node) || heredoc_expands(&body)) {
        out.unresolved.push(Unresolved {
            construct: Construct::ExpandedHeredoc,
            decides: Decides::RedirectTarget,
            span: body_node.map(Span::of).unwrap_or_else(|| Span::of(node)),
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

/// A here-document's body read out of its redirect's own text: everything between the first
/// newline of the redirect and the line that ends the document.
fn heredoc_body_from_text(node: &Node, source: &str) -> Option<String> {
    let text = src(node, source);
    let from = node.start + text.find('\n')? + 1;
    let end = kids(node)
        .into_iter()
        .find(|c| c.kind.as_str() == "heredoc_end")?;
    source.get(from..end.start).map(str::to_string)
}

/// Whether the redirect is `<<-`, which the shell reads as "strip the leading tabs". The token is
/// the only tell that survives a descriptor: `3<<-'EOF'` does not start with `<<-`.
fn heredoc_strips_tabs(node: &Node) -> bool {
    kids(node).into_iter().any(|c| c.kind.as_str() == "<<-")
}

/// `<<-` strips every line's leading tabs and the grammar strips one line's, so the body it hands
/// back still carries tabs the program never sees.
fn strip_heredoc_tabs(body: &str) -> String {
    body.split_inclusive('\n')
        .map(|l| l.trim_start_matches('\t'))
        .collect()
}

/// Whether the grammar put something in the body other than literal content: an expansion, a
/// substitution. It is the half of "the shell expands this" the tree states on its own.
fn heredoc_expansion_child(body: Option<&Node>) -> bool {
    body.is_some_and(|b| kids(b).iter().any(|c| c.kind != "heredoc_content"))
}

/// What an unquoted here-document body holds that the shell will act on: `$`, a backquote, or a
/// backslash before either of those or itself. The grammar surfaces most as child nodes, but a
/// backquote arrives inside `heredoc_content`, so the text is read as well as the tree.
fn heredoc_expands(body: &str) -> bool {
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'$' | b'`' => return true,
            b'\\' => {
                if matches!(b.get(i + 1), Some(b'$') | Some(b'`') | Some(b'\\')) {
                    return true;
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Every child, anonymous tokens included. An operator token is exactly the kind of child
/// tree-sitter leaves anonymous, so a caller filtering on `named` silently loses the `&&`, `|` and
/// `;` this module decides by - see the `&&` asymmetry at `list`, which is a permission decision.
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
    // The compounds this stage has already walked, so the substitution scan below does not
    // descend into them a second time.
    let mut nested: Vec<usize> = Vec::new();

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
            _ if !child.named && child.kind != "$" => {}
            // A whole command where a WORD was expected: `time ( ... )` hangs a `subshell` off the
            // `command` node with no field, and its commands RUN, so they are walked and listed
            // here. The argument position stays unread: a compound's value is not this stage's argv.
            "subshell" | "compound_statement" | "list" | "pipeline" => {
                walk(child, source, ctx, out);
                nested.push(child.id);
                let idx = out.unresolved.len();
                out.unresolved.push(Unresolved {
                    construct: Construct::CompoundArgument,
                    decides: Decides::Argument,
                    span: Span::of(child),
                    text: clip(src(child, source), 120),
                    known_prefix: None,
                    stage: Some(index),
                    why: why_of(&Construct::CompoundArgument, Decides::Argument),
                });
                argv.push(Word::Unresolved(idx));
            }
            _ => {
                argv.extend(argument(child, source, out, index));
            }
        }
    }

    // A brace expansion the grammar split at a space: `echo {a b,c}` arrives as `{a` and `b,c}`,
    // and reading each as a literal claims two arguments where bash delivers three. Only the PAIR
    // is recognisable, because a lone `M-}` and a half-typed `ls {corpus` are literals to bash.
    let texts: Vec<Option<String>> = argv.iter().map(|w| w.text().map(str::to_string)).collect();
    if texts
        .iter()
        .any(|t| t.as_deref().is_some_and(opens_unclosed))
        && texts
            .iter()
            .any(|t| t.as_deref().is_some_and(closes_unopened))
    {
        for (i, t) in texts.iter().enumerate() {
            let Some(t) = t
                .as_deref()
                .filter(|t| opens_unclosed(t) || closes_unopened(t))
            else {
                continue;
            };
            let idx = out.unresolved.len();
            out.unresolved.push(Unresolved {
                construct: Construct::BraceExpansion,
                decides: Decides::Argument,
                span: Span::of(node),
                text: clip(t, 120),
                known_prefix: None,
                stage: Some(index),
                why: why_of(&Construct::BraceExpansion, Decides::Argument),
            });
            argv[i] = Word::Unresolved(idx);
        }
    }

    let s = &mut out.stages[index];
    s.program = program.unwrap_or(Word::Literal(String::new()));
    s.argv = argv;
    s.assignments = assignments;

    // The commands inside `$( ... )` and `<$( ... )` RUN: their value is unresolvable and already
    // recorded as such, but the commands themselves are ordinary stages and are listed here. It is
    // why a grammar was worth the trouble - `kill $(pgrep -f x)` has two programs in it.
    scan_substitutions(node, source, ctx, out, &nested);
    for t in tails {
        heredoc_tail(t, source, ctx, out, Some(index));
    }
}

/// Walk the substitutions hanging under `node`, without re-entering `node` and without descending
/// into `skip`, which is a compound [`stage`] has already walked: `time ( ... )` hangs a subshell
/// in the argument position, and the generic recursion would reach its substitutions twice.
fn scan_substitutions(
    node: &Node,
    source: &str,
    ctx: &mut Vec<Context>,
    out: &mut Normalised,
    skip: &[usize],
) {
    for child in kids(node) {
        if skip.contains(&child.id) {
            continue;
        }
        match child.kind.as_str() {
            "command_substitution" | "process_substitution" => walk(child, source, ctx, out),
            // The commands nested after a here-document operator are walked by
            // `heredoc_tail`, substitutions and all; only the document itself is
            // scanned here.
            "heredoc_redirect" => {
                for k in kids(child) {
                    if !is_continuation(k) {
                        scan_substitutions(k, source, ctx, out, skip);
                    }
                }
            }
            _ => scan_substitutions(child, source, ctx, out, skip),
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

/// One argument, or the several a brace expansion makes of it: a [`Stage`]'s `argv` is the list the
/// program is handed, so a word the shell turns into four arguments arrives as four. A brace in the
/// command-name position stays unread, because one `Stage` has one program and `{a,b}/x` runs two.
fn argument(node: &Node, source: &str, out: &mut Normalised, stage: usize) -> Vec<Word> {
    match brace_words(node, source) {
        Some(words) => words,
        None => vec![word_of(node, source, out, Decides::Argument, Some(stage))],
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
    // `VAR=( a b "$c" )`: the grammar gives an `array` node whose children are ordinary words, so
    // each element resolves on its own terms. Flattening it into one opaque blob would lose the
    // elements that ARE known.
    if node.kind.as_str() == "array" {
        let parts: Vec<Word> = kids(node)
            .into_iter()
            .filter(|c| c.named)
            .map(|c| word_of(c, source, out, decides, stage))
            .collect();
        return Word::Array(parts);
    }
    match resolve(node, source, &out.binds) {
        Resolved::Literal(s) => Word::Literal(s),
        Resolved::Glob(s) => Word::Glob(s),
        Resolved::Blocked {
            construct,
            prefix,
            at,
        } => {
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
        Construct::UnreadHeredoc => {
            format!("the here-document's body is not in this parse, so {what} was never read")
        }
        Construct::AnsiCEscapes => format!(
            "a `$'…'` string decides {what}, and its escapes are decoded by the shell \
             — the bytes the program receives are not the bytes written here."
        ),
        Construct::BraceExpansion => format!(
            "a brace expansion decides {what}: this is one word here and SEVERAL \
             arguments after the shell expands it. Write the arguments out."
        ),
        Construct::CompoundArgument => format!(
            "a whole command sits where {what} was expected — `time ( … )`, `! ( … )`. Its \
             own commands RUN and are listed as their own stages; run them one at a time \
             rather than nesting one command inside another's argument list."
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

fn resolve<'t>(node: &'t Node, source: &str, binds: &[(String, String)]) -> Resolved<'t> {
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
            if glob {
                Resolved::Glob(text)
            } else {
                Resolved::Literal(text)
            }
        }
        // `'…'`: no expansion of any kind, and no globbing either.
        "raw_string" => {
            let t = src(node, source);
            Resolved::Literal(
                t.trim_start_matches('\'')
                    .trim_end_matches('\'')
                    .to_string(),
            )
        }
        "number" | "test_operator" | "regex" => Resolved::Literal(src(node, source).to_string()),
        // A name, not a value: `export SKIP_SLT` and `declare -x FOO` put a bare `variable_name` in
        // the ARGUMENT position. A name cannot be a path or an expansion, so a literal is exactly
        // what it is - and without an arm here it reads as an unrecognised node kind.
        "variable_name" => Resolved::Literal(src(node, source).to_string()),
        // A `$` that ENDS a word is an anonymous token here: the grammar wraps `$?`, `$#` and `$$`
        // in a `simple_expansion` when something follows, but a `$` in the last position of a word
        // (`echo x$$`) comes back as the token itself - bash's own pid, not a parse error.
        "$" => match src(node, source) {
            // A lone `$` is a literal dollar to the shell: `echo x$` prints `x$`.
            "$" => Resolved::Literal("$".to_string()),
            // `$$` is a pid, so it goes to the resolver as the shell's own number rather
            // than being read as a value (`intent/parameters.rs`'s `NUMERIC`).
            "$$" => Resolved::Blocked {
                construct: Construct::ParameterExpansion {
                    name: "$".to_string(),
                },
                prefix: None,
                at: node,
            },
            // Nothing else is known to arrive as this token, and an unknown spelling is
            // not a literal.
            _ => Resolved::Blocked {
                construct: Construct::ParseError,
                prefix: None,
                at: node,
            },
        },
        // `$'...'`: the escapes are the shell's, so the word the program receives is the DECODED
        // bytes, not the text between the quotes. Reporting it undecoded would be a claim about a
        // command nobody wrote - `$'a\tb'` is one argument holding a tab.
        "ansi_c_string" => match decode_ansi_c(src(node, source)) {
            Some(s) => Resolved::Literal(s),
            // The bytes are not a string this can carry (a lone `\xff`, a NUL, a code point
            // past `U+10FFFF`, an escape bash refuses to finish): stay unread rather than
            // guess what the program receives.
            None => Resolved::Blocked {
                construct: Construct::AnsiCEscapes,
                prefix: None,
                at: node,
            },
        },
        "string" | "translated_string" => {
            // A double-quoted string is literal only if every part of it is.
            let mut lit = String::new();
            for c in kids(node) {
                match c.kind.as_str() {
                    "\"" => {}
                    // A `$` the grammar hands back alone has nothing to expand: `"x$"` is `x$`.
                    "$" => lit.push('$'),
                    "string_content" | "escape_sequence" => {
                        lit.push_str(&double_quoted(src(c, source)))
                    }
                    other => {
                        // A loop variable the walk has bound: `for f in a b` makes `$f`
                        // THIS value on this pass, inside quotes as much as outside.
                        if let Some(v) = bound(c, source, binds) {
                            lit.push_str(&v);
                            continue;
                        }
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
        // A loop variable this walk bound (`for f in a b; do …`). The value is not a
        // hole: it is the word the list spelled for this pass.
        "simple_expansion" | "expansion" => match bound(node, source, binds) {
            Some(v) => Resolved::Literal(v),
            None => Resolved::Blocked {
                construct: named(
                    Construct::ParameterExpansion {
                        name: String::new(),
                    },
                    node,
                    source,
                ),
                prefix: None,
                at: node,
            },
        },
        "concatenation" => {
            let mut lit = String::new();
            // The same text with every QUOTED character blanked, so the brace check below
            // keeps the one fact the merged literal loses: a brace inside quotes is not a
            // brace expansion.
            let mut bare = String::new();
            let mut glob = false;
            for c in kids(node) {
                let quoted = matches!(
                    c.kind.as_str(),
                    "raw_string" | "string" | "translated_string" | "ansi_c_string"
                );
                let part = match resolve(c, source, binds) {
                    Resolved::Literal(s) => s,
                    Resolved::Glob(s) => {
                        glob = true;
                        s
                    }
                    Resolved::Blocked {
                        construct,
                        prefix,
                        at,
                    } => {
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
                };
                if quoted {
                    bare.extend(part.chars().map(|_| '\u{1}'));
                } else {
                    bare.push_str(&part);
                }
                lit.push_str(&part);
            }
            // The braces of `{a,b}.txt` arrive as three separate `word` children, so this is the
            // only place the pattern is visible - and it is the pattern WITHOUT the quoted parts
            // that decides, or a `gh api graphql query='{repository(...)}'` reads as an expansion.
            if has_brace_expansion(&bare) {
                return Resolved::Blocked {
                    construct: Construct::BraceExpansion,
                    prefix: None,
                    at: node,
                };
            }
            if glob {
                Resolved::Glob(lit)
            } else {
                Resolved::Literal(lit)
            }
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

/// The value a bound loop variable gives this expansion, when the expansion is exactly that
/// variable and nothing is done to it. The name is read from the TEXT because the grammar's
/// `variable_name` cannot tell `${f}` from `${#f}`: a transform then simply does not match.
fn bound(node: &Node, source: &str, binds: &[(String, String)]) -> Option<String> {
    if binds.is_empty() {
        return None;
    }
    let text = src(node, source);
    let name = match text.strip_prefix("${").and_then(|t| t.strip_suffix('}')) {
        Some(inner) => inner,
        None => text.strip_prefix('$')?,
    };
    binds
        .iter()
        .rev()
        .find(|(b, _)| b == name)
        .map(|(_, v)| v.clone())
        .filter(|v| one_word(v))
}

/// The construct a node kind stands for, or `None` for "this code does not know
/// this kind" - which is not the same as "this kind is harmless".
fn construct_of(kind: &str) -> Option<Construct> {
    Some(match kind {
        "simple_expansion" | "expansion" => Construct::ParameterExpansion {
            name: String::new(),
        },
        "command_substitution" => Construct::CommandSubstitutionValue,
        "process_substitution" => Construct::ProcessSubstitutionPath,
        "arithmetic_expansion" => Construct::ArithmeticExpansion,
        // `{1..3}` arrives as its OWN node kind: without an arm it falls through to `parse_error`,
        // which says the grammar could not read a tree with no error in it, and names the wrong
        // construct for a word left unread because it has too many values.
        "brace_expression" => Construct::BraceExpansion,
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
                .find(|c| {
                    c.kind.as_str() == "variable_name" || c.kind.as_str() == "special_variable_name"
                })
                .map(|c| src(c, source).to_string())
                .unwrap_or_else(|| src(node, source).trim_start_matches('$').to_string());
            Construct::ParameterExpansion { name }
        }
        other => other,
    }
}

/// Whether this text opens a brace it never closes: one side of a brace expansion the grammar split
/// at a space. Neither half is suspicious on its own - `M-}` is a tmux key name and `ls {corpus` is
/// a half-typed line, and bash hands both on as literals - so the check needs the PAIR; see [`stage`].
fn opens_unclosed(s: &str) -> bool {
    s.matches('{').count() > s.matches('}').count()
}

/// The other half: closes a brace it never opened.
fn closes_unopened(s: &str) -> bool {
    s.matches('}').count() > s.matches('{').count()
}

/// Does this text contain a brace expansion the shell turns into several words? Bash needs a comma
/// or a `..` range inside the braces, which separates `{a,b}.txt` from `find ... -exec rm {} \;`:
/// permissive would refuse every `find -exec`, the other way would call two deletions one.
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

/// How many words one word may become: the same 32 as [`unrolled_for`]'s passes, because a thousand
/// arguments is a script and this is the reading of one command. Above it the word stays unread,
/// which costs a question rather than an answer.
const MOST_WORDS: usize = 32;

/// A brace expansion, expanded into bash's words, or `None` when the text does not decide it.
/// Refused: a brace inside quotes, an alternative holding an expansion or whitespace, too many
/// words, or an empty result.
fn brace_words(node: &Node, source: &str) -> Option<Vec<Word>> {
    let (text, quoted) = brace_source(node, source)?;
    // No brace, no brace expansion: a word with none is read by [`resolve`], which knows a
    // quoted `*` is a pattern only when the quotes are not there.
    if !has_brace_expansion(&text) {
        return None;
    }
    // A quoted part INSIDE a brace pair is an alternative whose bytes this does not model,
    // so that word stays unread; beside the braces it is literal text the shell keeps.
    let mut depth = 0usize;
    for (c, in_quotes) in text.chars().zip(&quoted) {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if *in_quotes && depth > 0 {
            return None;
        }
    }
    let mut out = Vec::new();
    for w in expand_braces(&text)? {
        // An empty alternative makes no word: measured, `echo {a,,b}` is two arguments,
        // `echo {a,}` one, and `echo {,}` none at all.
        if w.is_empty() {
            continue;
        }
        if w.chars().any(char::is_whitespace) {
            return None;
        }
        out.push(if w.chars().any(|c| matches!(c, '*' | '?' | '[')) {
            // Pathname expansion runs over each word the braces produced, so `{*,x}` is a
            // pattern and a name. A pattern is not a value - the same distinction
            // [`Word::Glob`] already makes for `*.rs`.
            Word::Glob(w)
        } else {
            Word::Literal(w)
        });
    }
    if out.is_empty() { None } else { Some(out) }
}

/// The text a word holds once quotes are removed, and which characters came from a quoted part,
/// or `None` when the shell does not decide it. A brace inside a quoted part is literal; any
/// expansion, backslash or substitution leaves the word undecided.
fn brace_source(node: &Node, source: &str) -> Option<(String, Vec<bool>)> {
    match node.kind.as_str() {
        "raw_string" => {
            let t = src(node, source);
            let inner = t.trim_start_matches('\'').trim_end_matches('\'');
            (!inner.contains(['{', '}'])).then(|| quoted(inner))
        }
        "string" | "translated_string" => {
            let mut lit = String::new();
            for c in kids(node) {
                match c.kind.as_str() {
                    "\"" => {}
                    "$" => lit.push('$'),
                    "string_content" | "escape_sequence" => {
                        lit.push_str(&double_quoted(src(c, source)))
                    }
                    _ => return None,
                }
            }
            (!lit.contains(['{', '}'])).then(|| quoted(&lit))
        }
        "concatenation" => {
            let mut text = String::new();
            let mut in_quotes = Vec::new();
            for c in kids(node) {
                let (part, flags) = brace_source(c, source)?;
                text.push_str(&part);
                in_quotes.extend(flags);
            }
            Some((text, in_quotes))
        }
        "word" | "number" | "brace_expression" => {
            let t = src(node, source);
            (!t.contains(['\'', '"', '\\', '$', '`']))
                .then(|| (t.to_string(), vec![false; t.chars().count()]))
        }
        _ => None,
    }
}

/// The bytes one double-quoted run delivers: a backslash goes away before `$`, a backquote, `"`
/// and itself, a backslash-newline is a line continuation, and before anything else it stays.
fn double_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some(kept @ ('$' | '`' | '"' | '\\')) => out.push(kept),
            Some('\n') => {}
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// One quoted part, and one flag per character: every character of a quoted run is quoted.
fn quoted(s: &str) -> (String, Vec<bool>) {
    (s.to_string(), vec![true; s.chars().count()])
}

/// What one brace pair is, as far as the text decides. The difference between the last two arms is
/// the safety property: `{a}` and `{1..a}` are not expansions and a word made of one is a literal,
/// while `{1..100}` IS one, and calling it literal would claim the program receives the braces.
enum Brace {
    /// Not an expansion at all: `{a}`, `{}`, `{1..a}`, `{1..3..x}`.
    Literal,
    /// The words the shell makes of it.
    Words(Vec<String>),
    /// An expansion over the bound. The word stays unread.
    TooMany,
}

/// The words bash makes of one word: find the first brace pair a comma or range can expand, replace
/// it with each alternative in turn - leftmost outermost, rightmost varying fastest - and expand
/// each result again, because an alternative may hold another pair. `None` means the text did not decide.
fn expand_braces(text: &str) -> Option<Vec<String>> {
    const MOST_DEPTH: usize = 8;
    fn at(text: &str, depth: usize) -> Option<Vec<String>> {
        if depth > MOST_DEPTH {
            return None;
        }
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] != b'{' {
                i += 1;
                continue;
            }
            let Some(close) = matching_brace(b, i) else {
                i += 1;
                continue;
            };
            let alts = match alternatives(&text[i + 1..close]) {
                Brace::Words(w) => w,
                Brace::TooMany => return None,
                // `{a}`, `{}` and `{1..a}` are not expansions, and bash keeps looking to the
                // right: `{{a,b}}` is `{a}` and `{b}`, because the inner pair is the one that
                // expands.
                Brace::Literal => {
                    i += 1;
                    continue;
                }
            };
            let (pre, post) = (&text[..i], &text[close + 1..]);
            let mut out: Vec<String> = Vec::new();
            for alt in alts {
                out.extend(at(&format!("{pre}{alt}{post}"), depth + 1)?);
                if out.len() > MOST_WORDS {
                    return None;
                }
            }
            return Some(out);
        }
        Some(vec![text.to_string()])
    }
    at(text, 0)
}

/// The index of the `}` that closes the `{` at `open`, counting nesting.
fn matching_brace(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (j, c) in b.iter().enumerate().skip(open) {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
    }
    None
}

/// What one brace pair expands to: the alternatives of a comma list, or the words of a
/// range.
fn alternatives(inner: &str) -> Brace {
    let b = inner.as_bytes();
    let mut parts: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut comma = false;
    for (i, c) in b.iter().enumerate() {
        match c {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            // A comma inside a NESTED pair belongs to the nested expansion, which the
            // recursion reaches; `{a,{b,c}}` is three words and not a two-way split.
            b',' if depth == 0 => {
                comma = true;
                parts.push(inner[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    if comma {
        parts.push(inner[start..].to_string());
        return Brace::Words(parts);
    }
    brace_range(inner)
}

/// `{1..3}`, `{-2..2..2}`, `{01..10}`, `{a..e..2}`: bash's own arithmetic, including its zero
/// padding, where an endpoint written with a leading zero pads every word to the wider endpoint.
/// Neither a mixed pair (`{1..a}`) nor a step that is not a number is a range at all.
fn brace_range(inner: &str) -> Brace {
    let mut parts = inner.split("..");
    let (Some(lo), Some(hi)) = (parts.next(), parts.next()) else {
        return Brace::Literal;
    };
    let step = match parts.next() {
        None => 1i64,
        Some(t) => match t.parse::<i64>() {
            Ok(n) => n,
            // `{1..3..x}` is not a range: measured, bash hands it through as characters.
            Err(_) => return Brace::Literal,
        },
    };
    if parts.next().is_some() {
        return Brace::Literal;
    }
    // Bash takes the MAGNITUDE of the step and the direction from the endpoints: measured,
    // `{1..3..-1}` is `1 2 3` and `{a..e..-1}` is `a b c d e`.
    let step = step.unsigned_abs().max(1);
    let (Ok(a), Ok(b)) = (lo.parse::<i64>(), hi.parse::<i64>()) else {
        return char_range(lo, hi, step);
    };
    // The padding is bash's, and it counts the sign. Measured: `{-01..3}` is `-01 000 001
    // 002 003` - the width is the wider endpoint INCLUDING its minus sign - while `{8..10}`
    // is `8 9 10` and `{001..3}` is `001 002 003`.
    let width = lo.len().max(hi.len());
    let leading_zero = |s: &str| {
        let d = s.trim_start_matches('-');
        d.len() > 1 && d.starts_with('0')
    };
    let pad = leading_zero(lo) || leading_zero(hi);
    let mut out = Vec::new();
    let mut n = a;
    loop {
        out.push(if pad {
            format!("{n:0width$}")
        } else {
            n.to_string()
        });
        if out.len() > MOST_WORDS {
            return Brace::TooMany;
        }
        if n == b {
            return Brace::Words(out);
        }
        // i128 so a step near u64::MAX or an endpoint near i64::MAX cannot overflow.
        let next = if a <= b {
            i128::from(n) + i128::from(step)
        } else {
            i128::from(n) - i128::from(step)
        };
        if (a <= b && next > i128::from(b)) || (a > b && next < i128::from(b)) {
            return Brace::Words(out);
        }
        n = next as i64;
    }
}

/// `{a..e}`, `{a..e..2}`: both endpoints ONE character, and bash walks the code points
/// between them. `{1..a}` is not a range - measured, bash leaves it literal.
fn char_range(lo: &str, hi: &str, step: u64) -> Brace {
    let (mut a, mut b) = (
        lo.chars().collect::<Vec<_>>(),
        hi.chars().collect::<Vec<_>>(),
    );
    if a.len() != 1 || b.len() != 1 {
        return Brace::Literal;
    }
    let (start, end) = (a.remove(0) as u32, b.remove(0) as u32);
    // A digit endpoint means the pair is MIXED (`{1..a}`), which bash leaves literal.
    if matches!(start, 0x30..=0x39) || matches!(end, 0x30..=0x39) {
        return Brace::Literal;
    }
    let mut out = Vec::new();
    let mut n = start;
    loop {
        match char::from_u32(n) {
            Some(c) => out.push(c.to_string()),
            None => return Brace::Literal,
        }
        if out.len() > MOST_WORDS {
            return Brace::TooMany;
        }
        if n == end {
            return Brace::Words(out);
        }
        let step = u32::try_from(step).unwrap_or(u32::MAX);
        if start <= end {
            match n.checked_add(step) {
                Some(next) if next <= end => n = next,
                _ => return Brace::Words(out),
            }
        } else {
            n = n.saturating_sub(step);
            if n < end {
                return Brace::Words(out);
            }
        }
    }
}

/// The bytes `$'...'` delivers, with bash's own escape decoding, or `None` when they are not a
/// string this can carry: a lone `\xff`, a NUL, a code point past `U+10FFFF` and a surrogate have
/// no `Word` to be, and the undecoded text or a replacement character would be a worse claim.
fn decode_ansi_c(text: &str) -> Option<String> {
    let inner = text.strip_prefix("$'")?.strip_suffix('\'')?;
    let mut out: Vec<u8> = Vec::with_capacity(inner.len());
    let mut it = inner.chars().peekable();
    let hex = |it: &mut std::iter::Peekable<std::str::Chars<'_>>, most: usize| -> Option<u32> {
        let mut n = 0u32;
        let mut digits = 0usize;
        while digits < most {
            let Some(d) = it.peek().and_then(|c| c.to_digit(16)) else {
                break;
            };
            it.next();
            n = n.checked_mul(16)?.checked_add(d)?;
            digits += 1;
        }
        (digits > 0).then_some(n)
    };
    while let Some(c) = it.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        let Some(e) = it.next() else {
            // A trailing backslash cannot happen inside `$'…'` (the quote follows), but a
            // guess about one is not worth making.
            return None;
        };
        let simple = match e {
            'a' => Some(0x07),
            'b' => Some(0x08),
            'e' | 'E' => Some(0x1b),
            'f' => Some(0x0c),
            'n' => Some(0x0a),
            'r' => Some(0x0d),
            't' => Some(0x09),
            'v' => Some(0x0b),
            '\\' => Some(b'\\'),
            '\'' => Some(b'\''),
            '"' => Some(b'"'),
            '?' => Some(b'?'),
            _ => None,
        };
        if let Some(b) = simple {
            out.push(b);
            continue;
        }
        match e {
            // A hex BYTE. One or two digits, and `\x` with none keeps the backslash.
            'x' => match hex(&mut it, 2) {
                Some(n) if n <= 0xff => out.push(n as u8),
                Some(_) => return None,
                None => out.extend_from_slice(b"\\x"),
            },
            // A hex CODE POINT, decoded to UTF-8 - `\u00e9` is two bytes, not one.
            'u' | 'U' => match hex(&mut it, if e == 'u' { 4 } else { 8 }) {
                Some(n) => {
                    let ch = char::from_u32(n)?;
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                None => out.extend_from_slice(if e == 'u' { b"\\u" } else { b"\\U" }),
            },
            // Up to three OCTAL digits: `\0123` is a newline and a `3`.
            '0'..='7' => {
                let mut n = e.to_digit(8)?;
                for _ in 0..2 {
                    let Some(d) = it.peek().and_then(|c| c.to_digit(8)) else {
                        break;
                    };
                    it.next();
                    n = n * 8 + d;
                }
                if n > 0xff {
                    return None;
                }
                out.push(n as u8);
            }
            // A control character: `\cZ` is `0x1a`, and `\c?` is DEL, which is bash's own
            // exception rather than arithmetic.
            'c' => match it.next() {
                Some('?') => out.push(0x7f),
                Some(x) if (x as u32) < 0x80 => out.push((x as u8) & 0x1f),
                Some(_) => return None,
                None => out.extend_from_slice(b"\\c"),
            },
            // Measured: an escape bash does not know keeps its backslash AND the character.
            other => {
                out.push(b'\\');
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    if out.contains(&0) {
        // `$'\0'` is a NUL, and a NUL cannot be an argv word: bash itself delivers the
        // empty string. Neither is a claim this can make.
        return None;
    }
    String::from_utf8(out).ok()
}

/// Remove backslash escapes and report whether an unescaped glob metacharacter survived. Both
/// halves matter: `e\ f.txt` is one argument whose bytes are `e f.txt`, and `\*` is a literal
/// asterisk, so calling every escaped star a glob would read `rm \*` as a wildcard delete.
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

    /// A `for` over a list the command spells is that many passes, not one hole: the shape from the
    /// corpus is `for f in <literal> <literal>; do grep ... $f; done`, with the literals written here.
    #[test]
    fn a_for_over_a_literal_list_is_unrolled_into_its_passes() {
        let x = n("for f in /w/a.rs /w/b.rs; do grep -n x $f; done");
        assert!(x.is_resolved(), "{:?}", x.unresolved);
        assert_eq!(x.binaries(), vec!["grep", "grep"]);
        assert_eq!(
            x.stages[0].argv,
            vec!["-n".into(), "x".into(), "/w/a.rs".into()]
                .into_iter()
                .map(Word::Literal)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            x.stages[1].argv.last(),
            Some(&Word::Literal("/w/b.rs".into())),
            "the second pass carries the second value"
        );
        // A loop body is a loop body: nothing here is promoted to "always runs".
        assert!(
            x.stages
                .iter()
                .all(|s| s.certainty == Certainty::Conditional),
            "{:?}",
            x.stages.iter().map(|s| s.certainty).collect::<Vec<_>>()
        );
        // And the passes are in the shape, because they are what will run.
        assert_eq!(shape(&x), "grep -n <args> ; grep -n <args>");

        // A bound value is a word of the body wherever it appears - inside a
        // concatenation and inside double quotes as much as on its own.
        let x = n("for f in /w/a /w/b; do cp $f $f.bak; echo \"done $f\"; done");
        assert!(x.is_resolved(), "{:?}", x.unresolved);
        assert_eq!(
            x.stages[0].argv,
            vec!["/w/a".into(), "/w/a.bak".into()]
                .into_iter()
                .map(Word::Literal)
                .collect::<Vec<_>>()
        );
        // Four stages: two per pass, in pass order.
        assert_eq!(x.stages.len(), 4);
        assert_eq!(x.stages[3].argv[0], Word::Literal("done /w/b".into()));
    }

    #[test]
    fn a_brace_range_at_the_integer_edge_ends_instead_of_overflowing() {
        let words = |inner: &str| match brace_range(inner) {
            Brace::Words(w) => w,
            _ => panic!("{inner} is a range"),
        };
        assert_eq!(
            words("9223372036854775800..9223372036854775807..5"),
            vec!["9223372036854775800", "9223372036854775805"]
        );
        assert_eq!(words("1..3..9223372036854775807"), vec!["1"]);
        assert_eq!(words("a..e..4294967297"), vec!["a"]);
    }

    /// A `for` over a brace list is the passes the shell makes of it: a brace is decided by the
    /// text, so the body is walked once per word rather than left unread.
    #[test]
    fn a_for_over_a_brace_list_is_unrolled_into_the_words_it_makes() {
        let x = n("for i in {1..3}; do cat /w/f$i; done");
        assert!(x.is_resolved(), "{:?}", x.unresolved);
        let tails: Vec<Option<&str>> = x
            .stages
            .iter()
            .map(|s| s.argv.last().and_then(Word::literal))
            .collect();
        assert_eq!(tails, vec![Some("/w/f1"), Some("/w/f2"), Some("/w/f3")]);
        assert!(
            x.stages
                .iter()
                .all(|s| s.certainty == Certainty::Conditional),
            "a brace list does not make a loop body unconditional"
        );

        // A brace concatenated with text, and two brace words in one list.
        let x = n("for i in {a,b}.txt; do cat /w/$i; done");
        assert!(x.is_resolved(), "{:?}", x.unresolved);
        let tails: Vec<Option<&str>> = x
            .stages
            .iter()
            .map(|s| s.argv.last().and_then(Word::literal))
            .collect();
        assert_eq!(tails, vec![Some("/w/a.txt"), Some("/w/b.txt")]);
        let x = n("for i in {1..2} {8,9}; do echo $i; done");
        assert!(x.is_resolved(), "{:?}", x.unresolved);
        assert_eq!(x.stages.len(), 4);

        // Past the bound, and a brace alternative the shell would glob: unread, as before.
        assert!(!n("for i in {1..100}; do echo $i; done").is_resolved());
        assert!(!n("for i in {a,*}; do echo $i; done").is_resolved());
    }

    /// A list the command does not spell is not unrolled, and each of these is a real
    /// `for` from the same corpus.
    #[test]
    fn a_for_whose_list_is_not_in_the_text_stays_one_unread_pass() {
        for cmd in [
            // The file set is not in the text.
            "for f in *.rs; do grep -n x $f; done",
            // The value is not.
            "for f in $FILES; do grep -n x $f; done",
            "for f in $(ls); do grep -n x $f; done",
            // Two words from one: a single binding cannot stand for both.
            "for f in \"a b\"; do grep -n x $f; done",
        ] {
            let x = n(cmd);
            assert!(!x.is_resolved(), "{cmd} was unrolled: {:?}", x.stages);
        }
        // The body is still read, once - unrolling is the extra, not the reading.
        let x = n("for f in *.rs; do grep -n x $f; done");
        assert_eq!(x.binaries(), vec!["grep"]);
    }

    /// A transform of the loop variable is not the variable - the same rule the
    /// intent layer keeps for `${#X}` (the grammar reports the name
    /// `f` for all of `$f`, `${f}`, `${#f}` and `${!f}`).
    #[test]
    fn a_transform_of_a_loop_variable_is_not_the_variable() {
        for cmd in [
            "for f in /w/a; do echo ${#f}; done",
            "for f in /w/a; do echo ${f%.*}; done",
            "for f in /w/a; do echo ${!f}; done",
            "for f in /w/a; do echo ${f[0]}; done",
        ] {
            assert!(!n(cmd).is_resolved(), "{cmd} was read as the variable");
        }
        // The braced spelling of the value itself is the value.
        assert!(n("for f in /w/a; do echo ${f}; done").is_resolved());
    }

    /// A binding lives for one body. `$f` after the `done` is the empty string, which
    /// this module does not know and must not claim.
    #[test]
    fn a_loop_binding_does_not_leak_past_its_body() {
        let x = n("for f in /w/a; do echo $f; done; echo $f");
        assert!(!x.is_resolved());
        assert_eq!(x.unresolved.len(), 1);
        assert_eq!(x.unresolved[0].construct.as_str(), "parameter_expansion");
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
    fn a_double_quoted_part_is_the_bytes_bash_makes_of_it() {
        // Measured with `set --`: inside `"…"` a backslash goes before `$`, a backquote, `"` and
        // itself, a backslash-newline is a line continuation, every other backslash stays, and a
        // `$` with nothing to expand is a dollar.
        let x = n(r#"rm -rf "/w/$"{a,b}"#);
        assert_eq!(
            x.stages[0].argv[1..].to_vec(),
            vec![Word::Literal("/w/$a".into()), Word::Literal("/w/$b".into())]
        );
        let y = n("rm -rf \"a\\$b\" \"c\\\\d\" \"e\\\"f\" \"g\\nh\" \"i\\\nj\"");
        assert_eq!(
            y.stages[0].argv[1..].to_vec(),
            vec![
                Word::Literal("a$b".into()),
                Word::Literal("c\\d".into()),
                Word::Literal("e\"f".into()),
                Word::Literal("g\\nh".into()),
                Word::Literal("ij".into()),
            ]
        );
    }

    #[test]
    fn a_command_assembled_at_runtime_is_unresolved_at_the_program_position() {
        let x = n("X=$(cat f); $X --now");
        assert!(!x.is_resolved());
        // The inner `cat f` IS resolved and listed - this is what `predicate.rs`
        // could not do - while the value it produces is not.
        assert!(x.binaries().contains(&"cat"));
        let p = x.unresolved_programs();
        assert_eq!(p.len(), 1);
        assert!(x.unresolved.iter().any(|u| u.decides == Decides::Program
            && matches!(&u.construct, Construct::ParameterExpansion { name } if name == "X")));
        // And the assignment's value is unresolvable for a different reason.
        assert!(x.unresolved.iter().any(|u| u.decides == Decides::Assignment
            && u.construct == Construct::CommandSubstitutionValue));
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
        // Why `partial` is fatal: tree-sitter-bash 0.25 cannot parse `<<<`, and its RECOVERY reads it
        // as `< 'here string'` - a read of a file named `here string`. A normaliser reporting that
        // data flow would be confidently wrong.
        let x = n("cat >&2 <<< 'here string'");
        assert!(x.partial);
        assert!(!x.is_resolved());
        assert_eq!(x.unresolved[0].construct, Construct::ParseError);
        assert_eq!(x.unresolved[0].decides, Decides::Structure);
        assert!(
            !x.unresolved[0].why.contains("here-document operator"),
            "a here-string is not the here-document shape: {}",
            x.unresolved[0].why
        );
    }

    /// `<>` is one operator this grammar has no token for: the `>` is recovered as an ERROR between
    /// the descriptor and the destination, both of which land where bash puts them, so the one
    /// redirect is read instead of the whole command refused.
    #[test]
    fn a_read_write_redirect_is_read_rather_than_refused() {
        let x = n("exec 3<>/dev/tcp/127.0.0.1/5432");
        assert!(x.is_resolved(), "{:?}", x.unresolved);
        assert_eq!(x.binaries(), vec!["exec"]);
        let r = &x.stages[0].redirects;
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].op, RedirectOp::ReadWrite);
        assert_eq!(r[0].fd, Some(3));
        assert_eq!(
            r[0].target,
            RedirectTarget::File(Word::Literal("/dev/tcp/127.0.0.1/5432".into()))
        );

        // The one command in this tree that writes the operator: `fleet`'s port probe.
        let probe = n(
            "if (exec 3<>/dev/tcp/127.0.0.1/5432) 2>/dev/null; then exec 3>&-; \
                       echo yes; else echo no; fi",
        );
        assert!(probe.is_resolved(), "{:?}", probe.unresolved);
        assert_eq!(probe.binaries(), vec!["exec", "exec", "echo", "echo"]);
    }

    /// Without a descriptor the recovered tree holds TWO redirects (`<` and `>file`) where the shell
    /// opens one, so the redirect is not read - and the refusal names the operator and says where a
    /// descriptor makes it read.
    #[test]
    fn a_read_write_redirect_with_no_descriptor_names_the_writing_that_reads() {
        for cmd in ["cmd <>file", "cmd 0<>file"] {
            let x = n(cmd);
            assert!(x.partial, "{cmd}");
            assert_eq!(x.unresolved[0].construct, Construct::ParseError);
            let why = &x.unresolved[0].why;
            assert!(why.contains("<>"), "{cmd}: {why}");
            assert!(why.contains("3<>file"), "{cmd}: {why}");
        }

        let written = n("cmd 3<>file");
        assert!(written.is_resolved(), "{:?}", written.unresolved);
        assert_eq!(written.stages[0].redirects[0].op, RedirectOp::ReadWrite);
        assert_eq!(written.stages[0].redirects[0].fd, Some(3));

        // And a recovery that swallowed a word behind the destination is not credited: `exec 3<>file
        // cat`'s tree has `cat` as a second destination, so the `exec` of a command is refused.
        let swallowed = n("exec 3<>file cat");
        assert!(swallowed.partial, "{:?}", swallowed.unresolved);
    }

    /// `%(…)` in a word is not a command bash will run either, so the refusal says what the shell
    /// says and which writing both the shell and the gate read.
    #[test]
    fn a_percent_before_a_bracket_is_refused_as_the_text_bash_refuses() {
        for cmd in [
            "git for-each-ref --format=%(refname)",
            "echo a%(x)b",
            "a=%(x)",
        ] {
            let x = n(cmd);
            assert!(x.partial, "{cmd}");
            let why = &x.unresolved[0].why;
            assert!(why.contains("%("), "{cmd}: {why}");
            assert!(why.contains("quote"), "{cmd}: {why}");
        }
        let written = n("git for-each-ref --format='%(refname)'");
        assert!(written.is_resolved(), "{:?}", written.unresolved);
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

        // The shell expands a backquote in an unquoted body too, and the grammar hands that
        // back as body text rather than as a node: the bytes `cat` reads are not these.
        let back = n("cat <<EOF\n`date` x\nEOF");
        assert!(
            !back.is_resolved(),
            "a backquote in the body was read as text"
        );
        assert_eq!(back.unresolved[0].construct, Construct::ExpandedHeredoc);
        // A backslash only counts before what it would quote, so prose keeps its `\|`.
        assert!(!n("cat <<EOF\n\\$x\nEOF").is_resolved());
        assert!(n("cat <<EOF\n\\| x\nEOF").is_resolved());
        // Quoted, none of it expands.
        assert!(n("cat <<'EOF'\n`date` $x \\$y\nEOF").is_resolved());
        // The grammar hands a body whose first byte is a backslash back as an argument word, so
        // the bytes are read from the text: quoted, an empty body would claim a write of nothing.
        let q = n("cat > /w/f <<'EOF'\n\\x\nEOF");
        assert!(q.is_resolved(), "{:?}", q.unresolved);
        assert!(
            q.stages[0].redirects.iter().any(|r| matches!(
                &r.target,
                RedirectTarget::HereDoc { literal: true, body } if body == "\\x\n"
            )),
            "the body was lost: {:?}",
            q.stages[0].redirects
        );
    }

    #[test]
    fn a_here_document_body_a_sibling_swallowed_is_read_from_the_text() {
        // Measured: a redirect or a pipeline between the delimiter and the body leaves
        // `heredoc_body` empty and the body's own text inside that sibling, so an empty body is
        // "not in this node" - and an empty body is a script nobody read, admitted as no code.
        for src in [
            "cat <<'EOF' > /w/f\n\\x\nEOF",
            "cat <<'EOF' | sh\n\\x\nEOF",
            "cat <<'EOF' 2>&1\n\\x\nEOF",
        ] {
            let x = n(src);
            assert!(
                x.stages.iter().flat_map(|s| &s.redirects).any(|r| matches!(
                    &r.target,
                    RedirectTarget::HereDoc { body, .. } if body == "\\x\n"
                )),
                "the body was lost in {src:?}: {:?}",
                x.stages
            );
        }
    }

    /// A here-document operator followed by another redirect and then a pipe is the grammar
    /// misplacing the pipeline: `| tail` arrives inside the `2>&1` redirect and the text holds no
    /// second stage. The rewrite keeps the redirect on the same command and reads.
    #[test]
    fn a_here_document_followed_by_a_redirect_and_a_pipe_names_the_writing_that_reads() {
        for src in [
            "cat <<'EOF' 2>&1 | tail -3\nx\nEOF",
            "cat <<'EOF' 2>/dev/null | tail -3\nx\nEOF",
        ] {
            let x = n(src);
            assert!(x.partial, "{src}");
            let why = &x.unresolved[0].why;
            assert!(why.contains("here-document operator"), "{src}: {why}");
            assert!(why.contains("before the `<<`"), "{src}: {why}");
        }

        // What the refusal recommends: the same redirect, written before the operator.
        let written = n("cat 2>&1 <<'EOF' | tail -3\nx\nEOF");
        assert!(written.is_resolved(), "{:?}", written.unresolved);
        assert_eq!(written.binaries(), vec!["cat", "tail"]);
        assert!(written.stages[0].pipe_out && written.stages[1].pipe_in);
        assert_eq!(
            written.stages[0].redirects[0].op,
            RedirectOp::Duplicate,
            "{:?}",
            written.stages[0].redirects
        );
    }

    #[test]
    fn a_dash_here_document_hands_over_no_tabs() {
        // `<<-` strips the leading tabs of every line, terminator line included; the grammar
        // strips only the first line's and keeps the rest, so the body here is not the shell's.
        let x = n("cat 3<<-'EOF'\n\t\t\\x\n\t\ty\n\tEOF");
        assert!(
            x.stages.iter().flat_map(|s| &s.redirects).any(|r| matches!(
                &r.target,
                RedirectTarget::HereDoc { literal: true, body } if body == "\\x\ny\n"
            )),
            "the tabs were kept: {:?}",
            x.stages[0].redirects
        );
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
        let rm = x
            .stages
            .iter()
            .find(|s| s.program_name() == Some("rm"))
            .unwrap();
        assert_eq!(rm.certainty, Certainty::OnlyIfCalled);
        let ls = x
            .stages
            .iter()
            .find(|s| s.program_name() == Some("ls"))
            .unwrap();
        assert_eq!(ls.certainty, Certainty::Always);
    }

    #[test]
    fn the_guarded_side_of_a_list_is_conditional() {
        let x = n("test -f x && rm x");
        let rm = x
            .stages
            .iter()
            .find(|s| s.program_name() == Some("rm"))
            .unwrap();
        assert_eq!(rm.certainty, Certainty::Conditional);
        assert!(rm.context.contains(&Context::AndThen));
    }

    #[test]
    fn a_prefix_assignment_belongs_to_its_stage_and_a_statement_one_does_not() {
        let x = n("VAR=1 env; Y=2");
        let env = x
            .stages
            .iter()
            .find(|s| s.program_name() == Some("env"))
            .unwrap();
        assert_eq!(env.assignments.len(), 1);
        assert_eq!(env.assignments[0].name, "VAR");
        assert_eq!(x.assignments.len(), 1);
        assert_eq!(x.assignments[0].name, "Y");
    }

    #[test]
    fn a_subshell_and_a_loop_are_recorded_as_context_not_flattened_away() {
        let x = n("(cd /x && make) ; for f in *.rs; do wc -l $f; done");
        let make = x
            .stages
            .iter()
            .find(|s| s.program_name() == Some("make"))
            .unwrap();
        assert!(make.context.contains(&Context::Subshell));
        let wc = x
            .stages
            .iter()
            .find(|s| s.program_name() == Some("wc"))
            .unwrap();
        assert!(wc.context.contains(&Context::Loop));
        // `$f` is unresolvable even though a human can see the loop's list.
        assert!(!x.is_resolved());
    }

    #[test]
    fn an_empty_command_carries_its_denominator_and_does_not_read_as_clean() {
        let x = n("");
        assert_eq!(x.bytes, 0);
        assert!(x.stages.is_empty());
        // `is_resolved` is true - there is nothing unresolvable in nothing - and
        // that is why a caller must look at `stages`/`bytes` too. A zero with
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

    /// A run of operands is one hole and flags are kept, because the arity of an operand list is
    /// not part of what kind of command this is. A flag IS: `sed -n` prints and `sed -i` rewrites,
    /// so flags and their order stay in the key, and an unreadable word stays its own token.
    #[test]
    fn a_run_of_operands_is_one_hole_and_flags_are_kept() {
        let one = shape(&n("cargo test --offline -p letibot-tools"));
        assert_eq!(one, "cargo <args> --offline -p <args>");
        assert_eq!(
            one,
            shape(&n("cargo test --offline -p letibot-tools another-filter")),
            "an extra filter made a new shape"
        );
        // The brief's own example: `--lib -- <arg> <arg>` and `--lib -- <arg> <arg>
        // <arg>` were two shapes, so re-running tests with one more filter asked again.
        assert_eq!(
            shape(&n("cargo test --offline --lib -- a b")),
            shape(&n("cargo test --offline --lib -- a b c")),
            "one more filter after `--` made a new shape"
        );
        // The keeper, and the reason it is the keeper: a flag changes the kind of call.
        assert_ne!(shape(&n("sed -n 1p f")), shape(&n("sed -i 1p f")));
        assert_ne!(shape(&n("grep -A 3 x f")), shape(&n("grep x -A 3 f")));
        // An unreadable word is never pooled with an approved spelling, and never
        // swallows the operands on either side of it.
        assert_eq!(shape(&n("cat a $X b")), "cat <args> <?> <args>");
        // A count flag stays a count, as it did before the run was collapsed.
        assert_eq!(shape(&n("head -26 f")), "head -<n> <args>");
    }

    /// The words stage 0 hands its program, for a test that is about an expansion.
    fn argv(cmd: &str) -> Vec<String> {
        n(cmd)
            .stages
            .first()
            .map(|s| {
                s.argv
                    .iter()
                    .map(|w| match w {
                        Word::Literal(l) => l.clone(),
                        Word::Glob(g) => format!("{g}*"),
                        other => format!("{other:?}"),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A brace expansion is the words bash makes of it, in bash's order, and every expectation below
    /// is what `printf '<%s>' <the word>` printed rather than what the manual says: an empty
    /// alternative makes no word, a nested pair is the one that expands, and zero padding counts the sign.
    #[test]
    fn a_brace_expansion_is_the_words_the_shell_makes() {
        assert_eq!(argv("echo {a,b}"), ["a", "b"]);
        assert_eq!(argv("echo {1..3}"), ["1", "2", "3"]);
        assert_eq!(argv("echo {a..e}"), ["a", "b", "c", "d", "e"]);
        // Prefix and suffix land on every word.
        assert_eq!(argv("echo f{1,2}.rs"), ["f1.rs", "f2.rs"]);
        // Leftmost outermost, rightmost fastest.
        assert_eq!(
            argv("echo x{1,2}{3,4}"),
            ["x13", "x14", "x23", "x24"],
            "the order is bash's product order"
        );
        // A nested pair expands inside its alternative.
        assert_eq!(
            argv("echo pre{a,{b,c}}post"),
            ["preapost", "prebpost", "precpost"]
        );
        // `{{a,b}}` is `{a}` and `{b}`: the inner pair is the one bash expands.
        assert_eq!(argv("echo {{a,b}}"), ["{a}", "{b}"]);
        // Ranges: descending, stepped, zero-padded, and signed.
        assert_eq!(argv("echo {5..1}"), ["5", "4", "3", "2", "1"]);
        assert_eq!(argv("echo {a..e..2}"), ["a", "c", "e"]);
        assert_eq!(argv("echo {01..03..2}"), ["01", "03"]);
        assert_eq!(argv("echo {001..3}"), ["001", "002", "003"]);
        assert_eq!(
            argv("echo {-01..3}"),
            ["-01", "000", "001", "002", "003"],
            "the padding width counts the minus sign"
        );
        // A step's SIGN is dropped and its magnitude kept: measured, both of these ascend.
        assert_eq!(argv("echo {1..3..-1}"), ["1", "2", "3"]);
        // The empty alternative makes no word, and a comma pair can still be one word.
        assert_eq!(argv("echo {a,}"), ["a"]);
        assert_eq!(argv("echo {a,,b}"), ["a", "b"]);
        assert_eq!(argv("echo a{,}b"), ["ab", "ab"]);
        // A glob the braces produce is a PATTERN, exactly as `*.rs` is.
        assert_eq!(argv("echo {*,x}"), ["**", "x"]);
        // And the whole thing is a reading, not a hole.
        assert!(n("echo f{1,2}.rs").is_resolved());
        assert_eq!(shape(&n("rm -f f{1,2}.rs")), "rm -f <args>");
    }

    /// The braces bash leaves alone stay one literal word - and each of these is a word
    /// that really occurs (a `find -exec` terminator, a `{}` in a commit message).
    #[test]
    fn a_brace_the_shell_leaves_alone_stays_one_literal_word() {
        assert_eq!(argv("echo {a}"), ["{a}"]);
        assert_eq!(argv("echo {}"), ["{}"]);
        // A quoted brace is not expanded, so it is one literal - and never refused.
        assert_eq!(argv("echo \"{a,b}\""), ["{a,b}"]);
        assert_eq!(argv("echo '{a,b}'"), ["{a,b}"]);
        // A brace with a quoted literal beside it is still bare, so it expands: the quote
        // is not an expansion and the brace is outside it.
        assert_eq!(argv("echo \"x\"{a,b}"), ["xa", "xb"]);
        assert_eq!(argv("echo {a,b}\"x\""), ["ax", "bx"]);
        assert_eq!(argv("echo '/w/'{1,2}.txt"), ["/w/1.txt", "/w/2.txt"]);
        // Which brace is which cannot be said when the quotes hold one of them, and a
        // backslash or an expansion beside a brace leaves the word unread as it did.
        for cmd in ["echo \"{a,b}\"{c,d}", "echo a\\{b,c\\}", "echo \"$X\"{a,b}"] {
            assert!(!n(cmd).is_resolved(), "{cmd} was read: {:?}", n(cmd).stages);
        }
        // A mixed pair and a step that is not a number are not ranges.
        assert_eq!(argv("echo {1..a}"), ["{1..a}"]);
        assert_eq!(argv("echo {1..3..x}"), ["{1..3..x}"]);
        // No closing brace, no expansion.
        assert_eq!(argv("echo a{b,c"), ["a{b,c"]);
        // The `find -exec` terminator, which is why `{}` had to stay literal.
        assert!(n("find . -name x -exec rm -f {} ;").is_resolved());
        // `${a,b}` is a parameter expansion and not a brace expansion: the `$` decides it.
        assert!(!n("echo ${a,b}").is_resolved());
        // Neither half of a split brace is suspicious on its own: `M-}` is a tmux key name and
        // `ls {corpus` is a half-typed line, and bash hands both on as literals. Refusing either
        // alone leaves commands readable and then unreadable.
        assert_eq!(
            argv("tmux send-keys -t nav M-}"),
            ["send-keys", "-t", "nav", "M-}"]
        );
        assert_eq!(argv("ls {corpus"), ["{corpus"]);
        // Both halves in ONE command is the wreckage of `{a b,c}`: bash delivers three
        // words and the grammar delivers two, so neither may be called a literal.
        assert!(!n("echo {a b,c}").is_resolved());
        // A brace inside quotes is not an expansion, and the merge must not forget it: a
        // `gh api graphql query='{repository(...)}'` is one concatenation of `query=` and a quoted
        // string, and read on the MERGED text its braces look like an expansion.
        assert_eq!(
            argv("gh api graphql query='{repository(owner:\"x\", name:\"y\")}' -f a=b"),
            [
                "api",
                "graphql",
                "query={repository(owner:\"x\", name:\"y\")}",
                "-f",
                "a=b"
            ]
        );
        // A quoted `{` is not a brace: measured, `echo x'{'a,b'}'` prints ONE word `x{a,b}`.
        assert_eq!(argv("echo x'{'a,b'}'"), ["x{a,b}"]);
        // A quoted ALTERNATIVE inside unquoted braces is still an expansion (measured:
        // `echo {a,'b'}` is two words), which this refuses rather than expands - an
        // alternative holding a quote is a word whose bytes this does not model.
        assert!(!n("echo {a,'b'}").is_resolved());
    }

    /// A whole command in the argument position is walked, not swallowed: `time ( ... )` hangs a
    /// `subshell` off the `command` node with no field, and reading it as one opaque argument meant
    /// `time (cargo test ...)` produced no stage for `cargo`.
    #[test]
    fn a_compound_in_the_argument_position_is_walked_and_named() {
        let x = n("time (timeout 900 cargo test -p x 2>&1 | tail -3); echo done");
        assert!(!x.is_resolved(), "a compound argument was read as a word");
        // The commands inside RUN, so they are stages.
        for p in ["timeout", "tail", "echo"] {
            assert!(
                x.binaries().contains(&p),
                "{p} was not walked: {:?}",
                x.binaries()
            );
        }
        // And the refusal says what is wrong rather than calling a whole tree a parse error.
        assert!(
            x.unresolved
                .iter()
                .any(|u| u.construct == Construct::CompoundArgument),
            "{:?}",
            x.unresolved
        );
    }

    /// A brace expansion the text does not decide stays unread - the direction that
    /// costs a question rather than giving a wrong answer.
    #[test]
    fn a_brace_no_longer_readable_than_it_is_stays_unread() {
        for cmd in [
            // An alternative whose value is not in the text.
            "echo {$a,$b}",
            "echo {a,$(x)}",
            // Field splitting would make more words than the braces did.
            "echo {a b,c}",
            // More words than the bound, and the bound is the point: without it the word
            // would have to be called a literal, which claims the program gets `{1..100}`.
            "echo {1..100}",
            "echo f{1..33}",
            // An expansion that makes no word at all: `echo {,}` is `echo` with nothing.
            "echo {,}",
        ] {
            assert!(!n(cmd).is_resolved(), "{cmd} was read: {:?}", argv(cmd));
        }
        // 32 values is inside the bound and 33 is not.
        assert!(n("echo f{1..32}").is_resolved());
        // The refusal names the construct it is about, not "the grammar could not read this".
        assert!(
            n("echo {1..100}")
                .unresolved
                .iter()
                .any(|u| u.construct == Construct::BraceExpansion),
            "{:?}",
            n("echo {1..100}").unresolved
        );
    }

    /// `$'...'` is decoded into the bytes the program receives, one escape per row against bash 5.2
    /// and `od -tx1` of what the shell delivered: `\x` with no digits keeps its backslash, `\x4` is
    /// `0x04`, `\1011` is `A1`, and `\c?` is DEL rather than arithmetic.
    #[test]
    fn an_ansi_c_string_is_decoded_into_the_bytes_the_program_gets() {
        assert_eq!(argv("echo $'a\\tb'"), ["a\tb"]);
        assert_eq!(argv("echo $'a\\nb'"), ["a\nb"]);
        assert_eq!(argv("echo $'\\r'"), ["\r"]);
        assert_eq!(argv("echo $'\\a\\b\\f\\v'"), ["\u{7}\u{8}\u{c}\u{b}"]);
        assert_eq!(argv("echo $'\\e'"), ["\u{1b}"]);
        assert_eq!(argv("echo $'\\E'"), ["\u{1b}"]);
        assert_eq!(argv("echo $'\\\\'"), ["\\"]);
        assert_eq!(argv("echo $'\\''"), ["'"]);
        assert_eq!(argv("echo $'\\\"'"), ["\""]);
        // Hex: two digits, one digit, and none at all.
        assert_eq!(argv("echo $'\\x41'"), ["A"]);
        assert_eq!(argv("echo $'\\x4'"), ["\u{4}"]);
        assert_eq!(argv("echo $'\\x'"), ["\\x"]);
        // Octal, at most three digits: `\0123` is a newline and a `3`.
        assert_eq!(argv("echo $'\\101'"), ["A"]);
        assert_eq!(argv("echo $'\\0123'"), ["\n3"]);
        // A code point is decoded to UTF-8, and a byte is one byte.
        assert_eq!(argv("echo $'\\u00e9'"), ["é"]);
        assert_eq!(argv("echo $'\\U0001F600'"), ["😀"]);
        assert_eq!(argv("echo $'\\x41\\102'"), ["AB"]);
        // Control characters.
        assert_eq!(argv("echo $'\\cA'"), ["\u{1}"]);
        assert_eq!(argv("echo $'\\cZ'"), ["\u{1a}"]);
        assert_eq!(argv("echo $'\\c?'"), ["\u{7f}"]);
        // An escape bash does not know keeps its backslash AND the character.
        assert_eq!(argv("echo $'a\\zb'"), ["a\\zb"]);
        // A decoded quote is a QUOTE: the delimiter cannot reappear in the word, and a
        // decoded tab is a tab inside one argument rather than a field break.
        assert_eq!(argv("echo $'a\\'b'"), ["a'b"]);
        assert_eq!(n("echo pre$'\\t'post").stages[0].argv.len(), 1);
        // A `$'…'` next to ordinary text is still one word.
        assert_eq!(argv("echo pre$'\\t'post"), ["pre\tpost"]);
        assert!(n("echo $'a\\tb'").is_resolved());
    }

    /// An escape whose bytes are not a string this can carry stays unread. Each one is a real
    /// `$'...'` a shell accepts and none has a [`Word`] to be, and a replacement character or the
    /// undecoded text would both be wrong claims.
    #[test]
    fn an_ansi_c_string_that_is_not_a_string_stays_unread() {
        for cmd in [
            "echo $'\\xff'",     // a byte that is not UTF-8
            "echo $'\\0'",       // a NUL, which no argv can hold
            "echo $'\\U110000'", // past the last code point
        ] {
            let x = n(cmd);
            assert!(!x.is_resolved(), "{cmd} was read as {:?}", argv(cmd));
            assert!(
                x.unresolved
                    .iter()
                    .any(|u| u.construct == Construct::AnsiCEscapes),
                "{cmd}: {:?}",
                x.unresolved
            );
        }
    }

    /// A `$` at the end of a word is not a parse error. This grammar wraps `$?`, `$#` and even `$$`
    /// in a `simple_expansion` when something follows them, but a `$` in the LAST position of a
    /// word comes back as the bare token, and the tree is whole - bash's own pid, not a parse error.
    #[test]
    fn a_dollar_that_ends_a_word_is_the_pid_or_a_literal_and_not_a_parse_error() {
        for cmd in ["echo x$$", "echo a$$b", "echo $$", "echo x$ y$$"] {
            let x = n(cmd);
            assert!(!x.partial, "{cmd}");
            assert!(
                !x.unresolved
                    .iter()
                    .any(|u| u.construct == Construct::ParseError),
                "{cmd} was reported as a parse error: {:?}",
                x.unresolved
            );
        }
        // A bare `$` is a literal dollar to the shell - `--run=^$` is a regex, not a hole  -
        // and a `$$` is a pid, which the intent layer reads as one of the shell's numbers.
        assert_eq!(argv("echo x$"), ["x$"]);
        assert_eq!(argv("cargo nextest -E run=^$"), ["nextest", "-E", "run=^$"]);
        assert_eq!(n("echo $$").stages[0].argv.len(), 1);
        // The `$` is not silently dropped from the argument list either.
        assert_eq!(n("echo x$$").stages[0].argv.len(), 1);
    }
}

fn is_count_flag(t: &str) -> bool {
    t.len() > 1 && t.starts_with('-') && t[1..].chars().all(|c| c.is_ascii_digit())
}

/// The stable shape of a command, with its literals replaced by holes: the program and its FLAGS
/// are kept, because a flag is where a command changes kind (`sed -n` prints, `sed -i` rewrites),
/// and a shape folding those together would approve a write because a read was approved once.
pub fn shape(n: &Normalised) -> String {
    let mut out = String::new();
    for (i, st) in n.stages.iter().enumerate() {
        if i > 0 {
            out.push_str(if st.pipe_in { " | " } else { " ; " });
        }
        out.push_str(st.program.text().unwrap_or("<?>"));
        // Whether the previous word was an operand, so a RUN of them becomes one hole.
        let mut in_a_run = false;
        for w in &st.argv {
            // A COUNT is not a flag: `head -26`, `tail -5` - the dash is punctuation and the number
            // is an operand wearing it. Keeping them apart would make every `head -N` its own shape.
            let flag = match w.text() {
                Some(t) if is_count_flag(t) => Some("-<n>".to_string()),
                // A flag is part of the shape, because a flag is where a command
                // changes kind: `sed -n` prints and `sed -i` rewrites. `--opt=value`
                // keeps the option and holes the value - one token, two halves.
                Some(t) if t.starts_with('-') && t.len() > 1 => Some(match t.split_once('=') {
                    Some((opt, _)) => format!("{opt}=<v>"),
                    None => t.to_string(),
                }),
                // An operand, or a word nobody could read - both are holes, and the
                // caller below decides whether this one joins the run.
                _ => None,
            };
            match flag {
                Some(t) => {
                    out.push(' ');
                    out.push_str(&t);
                    in_a_run = false;
                }
                // Unresolved stays visible, and stays OUT of a run: a shape that hid it
                // would pool "we could not read this" with a spelling somebody has
                // approved.
                None if w.text().is_none() => {
                    out.push_str(" <?>");
                    in_a_run = false;
                }
                // Telling an operand from a flag's VALUE needs a per-program table of
                // which flags take a value, and buys nothing: `-A 22` and `-A 3` collapse
                // either way, and both are in the run after them.
                None => {
                    if !in_a_run {
                        out.push_str(" <args>");
                        in_a_run = true;
                    }
                }
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
    //! Which stage a hoisted redirection belongs to, and that the commands the grammar nests after a
    //! here-document operator are not lost: 5,675 here-documents read as `cd`'s stdin, and
    //! `cat <<'EOF' | bash` normalised to a `cat`.
    use super::*;

    fn heredoc_of(s: &Stage) -> Option<&str> {
        s.redirects.iter().find_map(|r| match &r.target {
            RedirectTarget::HereDoc { body, .. } => Some(body.as_str()),
            _ => None,
        })
    }

    fn programs(n: &Normalised) -> Vec<&str> {
        n.stages
            .iter()
            .map(|s| s.program_name().unwrap_or("?"))
            .collect()
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
        assert_eq!(
            n.stages[1].redirect_writes(),
            vec![&Word::Literal("f".into())]
        );
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
        assert_eq!(
            n.stages[0].redirect_writes(),
            vec![&Word::Literal("f.sh".into())]
        );
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
    //! A refusal nobody reads is a refusal that did not say why. The
    //! operator, on a `not_run` body: *"the explanation lines are
    //! repeated over and over, also 66 lines???"*
    use super::*;

    /// One variable, used five times, is one reason - not five copies of the
    /// same 220-character sentence.
    #[test]
    fn one_cause_is_explained_once_however_many_places_it_has() {
        let n = normalise("R=/tmp/x; grep q $R/a.rs $R/b.rs $R/c.rs; ls $R/d $R/e");
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
        // rather than dropped - a summary may never be silent about what it
        // summarised.
        assert!(report.contains("… and 1 more place(s)"), "{report}");
        assert!(
            report.lines().count() <= 7,
            "{} lines:\n{report}",
            report.lines().count()
        );
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
            report
                .lines()
                .filter(|l| l.starts_with("  ") && !l.starts_with("      "))
                .count(),
            2,
            "{report}"
        );
    }

    #[test]
    fn a_resolved_command_reports_nothing() {
        assert_eq!(normalise("ls /tmp").unresolved_report(), "");
    }
}
