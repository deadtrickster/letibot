//! **Every decision this operator's boxes ever made, read again by the parser and the
//! gate as they stand now.**
//!
//! # Why this exists
//!
//! The gate reads a shell command into stages, intents, regions, a shape and a grant
//! key, and the oracle is shown *that reading* rather than the command. So the thing a
//! change here improves or breaks is not a test — it is every decision already taken.
//! The corpus is those decisions, exported read-only from each host's store
//! (`~/letibot-corpus/*.json` on the box that holds it; this module takes the path on
//! the command line and nothing in the tree points at that directory).
//!
//! The operator's own framing, 2026-10-26: *"we have neglected the command parsing and
//! adjudication for too long"*, and *"this permission thing is more important actually
//! because the oracle always had trouble figuring out the intent"*. This is the
//! instrument that makes "better" a number instead of an opinion.
//!
//! # What it reports, and what each column is FOR
//!
//! * **resolved then vs now** — `then` is the record's own words: a baseline whose
//!   summary ends with *"the meaning was never resolved"* was not read. It is the only
//!   durable trace of that fact (`Baseline::summary` puts it there for exactly this
//!   reason), and it is what the corpus can be trusted about.
//! * **unresolved by construct**, from the parser's own
//!   [`Unresolved::construct`](letibot_code::shell::Unresolved) — never from a regex
//!   over the command text. Two numbers per construct: the commands it appears in, and
//!   the commands where it is the **only** reason. The second is what fixing that one
//!   construct buys; the first is only how common it is.
//! * **shapes**, distinct, over every command and over the commands a person answered.
//!   A shape is the approval key, so *"743 distinct shapes among 794 human asks"* is the
//!   measurement the arity folding in `shell::shape` was written for.
//! * **what a standing approval would now settle** — the per-session replay: in time
//!   order, every human answer is recorded as a session grant AND as an approved shape
//!   under the NEW reading, and each later human ask is asked whether that record would
//!   have settled it. The corpus does not carry which rung a person chose (only
//!   `selected`), so this is a **counterfactual with a stated assumption**: every past
//!   approval at its strongest. It is the upper bound of what approval memory can save,
//!   and the floor (nothing carried forward) is the plain count of human asks.
//! * **the oracle-brief diff** — for a decision where a model was consulted, whether the
//!   intent set or the region set the gate computes now differs from the one recorded in
//!   the `baseline` column, which is the text the oracle was shown. A row here is a
//!   decision that WAS taken on a different reading from the one the gate would give it
//!   today.
//!
//! # What it deliberately does not do
//!
//! It does not decide anything, does not ask the oracle (no live test, no model), and
//! does not read scripts (`scripts: &[]`): a corpus is a record of decisions, not the
//! tree those decisions were taken in, and a sweep that opened files would measure this
//! box instead of the corpus.
//!
//! # `then`'s regions were placed against a workspace this file does not record
//!
//! `--home` and `--workspace` are the caller's, because a corpus row carries the
//! command and not the directory it ran in. The **intent** comparison does not depend on
//! them (intents are read from programs and arguments); the **region** comparison does,
//! and only for a path that is either relative or equal to the workspace prefix. So the
//! report prints both counts apart rather than one "differs" number that hides which
//! half moved.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::BufReader;
use std::path::Path;

use serde::Deserialize;

use letibot_code::shell::{Construct, Context, Normalised, Unresolved};

use crate::adjudicate::{ActionClass, NO_PROGRAM, Tier, shape_of};
use crate::grant::{Coverage, Grant, Written};
use crate::intent::{Baseline, Intent, Surroundings};
use crate::mode::Mode;
use crate::schema::Access;

/// The sentence [`Baseline::summary`] appends to a reading the grammar did not resolve.
///
/// Read rather than recomputed, because this is what the record says — the fact a
/// corpus is evidence for.
const UNREAD: &str = "the meaning was never resolved";

/// One decision, the fields this sweep reads. Everything else in the export is ignored,
/// so an older or newer export still sweeps.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Row {
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub decided_ms: u64,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub verdict_by: String,
    /// Layer A's one-line reading, as the card and the corpus row carried it.
    #[serde(default)]
    pub baseline: Option<String>,
    /// The approval key the decision was recorded under; `None` for a non-command.
    #[serde(default)]
    pub shape: Option<String>,
    /// The call the gate was handed, as JSON. `command` in it is the shell command.
    #[serde(default)]
    pub arguments_json: String,
}

impl Row {
    /// The shell command, when this row has one.
    pub fn command(&self) -> Option<String> {
        let args: serde_json::Value = serde_json::from_str(&self.arguments_json).ok()?;
        Some(args.get("command")?.as_str()?.to_string())
    }

    fn a_human_answered(&self) -> bool {
        self.verdict_by.starts_with("human:")
    }

    fn an_oracle_answered(&self) -> bool {
        self.verdict_by.starts_with("model oracle")
    }
}

/// What today's parser and gate make of one command.
pub struct Reading {
    pub baseline: Baseline,
    /// Every distinct construct the grammar could not resolve, by its own name.
    pub constructs: Vec<&'static str>,
    /// The same constructs, one step finer, for choosing what to fix: the parameter's
    /// name (`parameter_expansion:$PWD`) or the program inside a substitution
    /// (`command_substitution_value:git`). The NAME and the PROGRAM only — never an
    /// operand, so a report can be read aloud without repeating a session.
    pub detail: Vec<String>,
    pub shape: Option<String>,
    pub class: ActionClass,
    pub program: String,
    pub access: Access,
}

impl Reading {
    pub fn of(cmd: &str, env: &Surroundings) -> Reading {
        let baseline = Baseline::of_command_with(cmd, env, &[]);
        let mut detail: Vec<String> = Vec::new();
        if let Some(n) = &baseline.command {
            for u in &n.unresolved {
                let key = match &u.construct {
                    Construct::ParameterExpansion { name } if !name.is_empty() => {
                        format!("parameter_expansion:${name}")
                    }
                    // The substitution's *value* is what nobody can know, and WHICH
                    // command produces it is what decides whether anything can be said
                    // about it: `$(git rev-parse …)` is a directory, `$(date)` is not.
                    Construct::CommandSubstitutionValue | Construct::ProcessSubstitutionPath => {
                        let inner: Vec<&str> = n
                            .stages
                            .iter()
                            .filter(|s| {
                                s.context.iter().any(|c| {
                                    matches!(
                                        c,
                                        Context::CommandSubstitution | Context::ProcessSubstitution
                                    )
                                }) && s.span.start >= u.span.start
                                    && s.span.end <= u.span.end
                            })
                            .filter_map(|s| s.program_name())
                            .collect();
                        match inner.first() {
                            Some(p) => format!("{}:{p}", u.construct.as_str()),
                            None => u.construct.as_str().to_string(),
                        }
                    }
                    other => other.as_str().to_string(),
                };
                detail.push(key);
            }
            detail.sort();
            detail.dedup();
        }
        let mut constructs: Vec<&'static str> = baseline
            .command
            .as_ref()
            .map(|n| n.unresolved.iter().map(|u| u.construct.as_str()).collect())
            .unwrap_or_default();
        constructs.sort();
        constructs.dedup();
        // The gate's own order (`AdjudicatedGate::judged_access`), asked of the two
        // predicates rather than restated: a shell that only reads is judged a read.
        let access = if baseline.reads_only() {
            Access::Read
        } else if baseline.writes_only() {
            Access::Write
        } else {
            Access::Exec
        };
        Reading {
            shape: shape_of(&baseline),
            class: ActionClass::host(access, true, false),
            program: baseline
                .grant_program
                .clone()
                .unwrap_or_else(|| NO_PROGRAM.to_string()),
            access,
            baseline,
            constructs,
            detail,
        }
    }

    pub fn resolved(&self) -> bool {
        self.baseline.resolved
    }

    fn destroys(&self) -> bool {
        self.baseline.intents.contains(&Intent::Destroy)
    }
}

/// The two sets a baseline's summary spells, for comparison with today's reading.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recorded {
    pub intents: BTreeSet<String>,
    pub regions: Option<BTreeSet<String>>,
}

/// `ask — intents [read_file unknown] over [host_other]` → the two bracketed sets.
///
/// The summary's own grammar, parsed the way a reader parses it. `None` when the text is
/// not a baseline summary at all — an older row, or a non-command — which the caller
/// counts rather than silently treating as agreement.
pub fn recorded(text: &str) -> Option<Recorded> {
    const INTENTS: &str = " — intents [";
    let start = text.find(INTENTS)? + INTENTS.len();
    let end = start + text[start..].find(']')?;
    let intents = text[start..end]
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let rest = &text[end + 1..];
    const REGIONS: &str = " over [";
    let regions = rest.find(REGIONS).and_then(|i| {
        let s = i + REGIONS.len();
        let e = s + rest[s..].find(']')?;
        Some(
            rest[s..e]
                .split_whitespace()
                .map(str::to_string)
                .collect::<BTreeSet<_>>(),
        )
    });
    Some(Recorded { intents, regions })
}

/// Today's reading, in the same shape, so a sample can print the two side by side.
fn now_words(now: &Reading) -> Recorded {
    Recorded {
        intents: now
            .baseline
            .intents
            .iter()
            .map(|i| i.as_str().to_string())
            .collect(),
        regions: Some(
            now.baseline
                .regions
                .iter()
                .map(|r| r.as_str().to_string())
                .collect(),
        ),
    }
}

fn words(r: Option<&Recorded>) -> String {
    let Some(r) = r else {
        return "—".into();
    };
    let join = |s: &BTreeSet<String>| s.iter().cloned().collect::<Vec<_>>().join(" ");
    format!(
        "intents {} over {}",
        join(&r.intents),
        r.regions.as_ref().map(join).unwrap_or_else(|| "—".into())
    )
}

/// Whether the RECORD says the command was never read.
///
/// Three spellings, and all three are in the corpus because the gate changed one:
///
/// * `the meaning was never resolved` — [`Baseline::summary`]'s marker, written since an
///   unresolvable construct stopped being a refusal (2026-10-02);
/// * `not_run` as the summary's verdict token — every row before that date, where the
///   refusal WAS the verdict and the summary had no reason to repeat it;
/// * `boundary:normaliser`, the verdict-by of the `NotRun` arm in
///   `AdjudicatedGate::admit`, which is one place and says exactly this.
///
/// MEASURED 2026-10-26: read as the marker alone, 514 rows on lab2x1 looked like a
/// REGRESSION — *resolved then, unresolved now* — and every one of them was a `not_run`
/// row from before the marker existed. A count whose denominator is a rule that changed
/// half way through the data is not a measurement.
fn unread(r: &Row) -> bool {
    if r.verdict_by == "boundary:normaliser" {
        return true;
    }
    match r.baseline.as_deref() {
        None => false,
        Some(t) => t.contains(UNREAD) || t.starts_with("not_run"),
    }
}

/// **Why a command with a `parse_error` is unread**, in the four shapes a sweep can tell apart
/// without running a shell.
///
/// This is §4i.4's census, as a function: the table there is this one run over every `bash`
/// row on each host, so a reader can re-derive the numbers rather than take them.
///
/// The split that matters is the FIRST arm. `partial == false` means the grammar read the
/// whole tree — there is no `ERROR` node and no `MISSING` one — and the command was called a
/// parse error by the resolver's own fall-through for a node kind it does not name. That is
/// this crate's bug and not the grammar's, and MEASURED 2026-10-27 it was 133 commands on
/// lab2x1 and 25 on the mac, every one of them a `$` that ends a word (`x$$`, `run=^$`).
///
/// The other three arms are lexical, because a census that ran `bash -n` would be an
/// instrument that spawns a shell over the operator's own commands:
///
/// * **`<>`** — this grammar's `file_redirect` has `<`, `>`, `>>`, `<&`, `>&`, `&>` and `&>>`
///   and no read-write redirection, so `exec 3<>/dev/tcp/h/p` recovers as an `ERROR` on one
///   of the two characters. The error node often IS that character, which is why the window
///   is widened by one byte either side before looking for the pair.
/// * **a here-document** — a `<<` earlier on the SAME line as the error. The grammar nests
///   what follows the operator inside `heredoc_redirect`, and a second redirect there
///   (`<<'EOF' 2>&1 | tail`) is what it cannot take.
/// * **the rest**: a command bash itself would refuse (an unbalanced quote, Lisp in a
///   single-quoted argument), a `%(…)` in a word, and a pair of prose backticks in an
///   unquoted here-document body. Measured one by one on lab2x1 through `bash -n`, and §4i.4
///   says which way each falls.
pub fn parse_cause(n: &Normalised, u: &Unresolved) -> &'static str {
    if !n.partial {
        return "a token the resolver does not name";
    }
    let src = &n.source;
    let window = src
        .get(u.span.start.saturating_sub(1)..(u.span.end + 1).min(src.len()))
        .unwrap_or("");
    if window.contains("<>") {
        return "a read-write redirect `<>`";
    }
    let line_start = src[..u.span.start.min(src.len())]
        .rfind('\n')
        .map_or(0, |i| i + 1);
    if src
        .get(line_start..u.span.start)
        .unwrap_or("")
        .contains("<<")
    {
        return "a here-document followed by an operator on its line";
    }
    "the text is not a command this grammar reads"
}

/// One session's approvals, replayed in the order they were given.
#[derive(Default)]
struct Session {
    grants: Vec<Grant>,
    shapes: HashMap<String, ActionClass>,
}

/// One armed approval: what the answer to a card records.
fn as_grant(r: &Reading) -> Grant {
    Grant {
        written: Written::Enumerated {
            patterns: vec![format!("{} ({})", r.program, r.class)],
        },
        coverage: vec![Coverage {
            program: r.program.clone(),
            class: r.class,
            intents: r.baseline.intents.clone(),
        }],
        why: "replayed: a person answered this".into(),
    }
}

/// Whether a point admits the call unasked — the gate's clause 2, before any grant.
fn mode_admits(mode: &str, r: &Reading) -> bool {
    // **The exec exception, and its one missing half.** Clause 2 is guarded by
    // `exec_follows_mode`, which the daemon sets only for an UNCONFINED session
    // (`cfg.unconfined`) and a corpus row does not record. The confined default is
    // modelled — a shell asks at every point — so this can under-count what a point
    // would settle and never over-count it.
    if r.access == Access::Exec {
        return false;
    }
    // The store spells the operator's own consent beside the name
    // (`allow-all (this box, consented)`); the name is the part before ` (`.
    let name = mode.split(" (").next().unwrap_or(mode);
    let Ok(mode) = Mode::parse(name) else {
        return false;
    };
    mode.admits_unasked(&r.baseline.tier, r.access)
}

#[derive(Debug, Default, Clone)]
pub struct Constructs {
    /// Commands the construct appears in.
    pub appears: usize,
    /// Commands where it is the ONLY reason the command is unresolved — what
    /// resolving this construct alone would recover.
    pub alone: usize,
}

/// The whole report for one corpus file.
#[derive(Debug, Default)]
pub struct Report {
    pub rows: usize,
    pub other_tools: usize,
    pub no_command: usize,
    pub bash: usize,
    /// Rows whose `baseline` column is absent, so `then` is not known.
    pub then_unknown: usize,
    pub then_resolved: usize,
    pub now_resolved: usize,
    /// Unresolved then, resolved now. The change this sweep is measured by.
    pub fixed: usize,
    /// Resolved then, unresolved now — a regression, and it must be zero.
    pub broken: usize,
    pub constructs: BTreeMap<&'static str, Constructs>,
    /// The `parse_error` commands, by cause — [`parse_cause`]. One per command, so a command
    /// with two errors counts once, under its first.
    pub parse_causes: BTreeMap<&'static str, usize>,
    /// The same, one step finer (`parameter_expansion:$HOME`,
    /// `command_substitution_value:git`) — the table a fix is chosen from, because
    /// *"parameter expansion"* is not one thing to fix.
    pub details: BTreeMap<String, Constructs>,
    /// Commands still unresolved now, counted once each however many constructs.
    pub unresolved_now: usize,
    pub shapes_then: BTreeSet<String>,
    pub shapes_now: BTreeSet<String>,
    /// Commands whose shape carried `<?>`, then and now.
    pub shapes_with_hole_then: usize,
    pub shapes_with_hole_now: usize,

    // --- the commands a person answered ------------------------------------
    pub human: usize,
    pub human_shapes_then: BTreeSet<String>,
    pub human_shapes_now: BTreeSet<String>,
    pub human_shape_unknown_then: usize,
    /// A session grant recorded earlier in the same session would cover it.
    pub human_by_grant: usize,
    /// An approved shape, same class, non-destructive.
    pub human_by_shape: usize,
    /// The point would admit it unasked under today's reading.
    pub human_by_mode: usize,
    /// Any of the three: the person need not have been asked.
    pub human_settled: usize,

    // --- the decisions a model was consulted on ----------------------------
    pub oracle: usize,
    pub oracle_unparsed: usize,
    pub oracle_intents_differ: usize,
    pub oracle_regions_differ: usize,
    pub oracle_either: usize,
    pub oracle_then_unresolved: usize,
    pub oracle_then_unresolved_now_read: usize,
    /// Rows whose session stream was not in `decided_ms` order, so the per-session
    /// replay is not the order they were given in. Reported, never hidden.
    pub out_of_order: usize,
    /// `LETIBOT_CORPUS_SHOW=<n>`: up to `n` samples of a regression and of each
    /// construct's leftovers, by request id and by the two readings' WORDS. Never the
    /// command: a sample is here to classify a finding, not to reproduce the operator's
    /// session.
    pub notes: Vec<String>,
}

impl Report {
    fn row(&mut self, r: &Row, env: &Surroundings) -> Option<Reading> {
        self.rows += 1;
        if r.tool != "bash" {
            self.other_tools += 1;
            return None;
        }
        let Some(cmd) = r.command() else {
            self.no_command += 1;
            return None;
        };
        self.bash += 1;
        Some(Reading::of(&cmd, env))
    }
}

/// Sweep one corpus file. `env` is the caller's home/workspace assumption, and it must
/// carry a declared shell (`with_pinned_shell`) — the daemon's own gate does, and a
/// sweep under `ShellTrust::Unknown` would measure that instead of the parser.
pub fn sweep(path: &Path, env: &Surroundings) -> Result<Report, String> {
    let mut rep = Report::default();
    let mut sessions: HashMap<String, Session> = HashMap::new();
    let mut last_ms: HashMap<String, u64> = HashMap::new();
    // See [`Report::notes`]: samples are opt-in because a sweep's default output is a
    // table of counts, and a reader who wants to classify one finding asks for it.
    let show: usize = std::env::var("LETIBOT_CORPUS_SHOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut shown_broken = 0usize;
    let mut shown_construct: HashMap<&'static str, usize> = HashMap::new();
    each_row(path, |r| {
        let Some(now) = rep.row(r, env) else {
            return;
        };

        // `then`, in the record's own words.
        match &r.baseline {
            None => rep.then_unknown += 1,
            Some(_) if unread(r) => {}
            Some(_) => rep.then_resolved += 1,
        }
        let then_unresolved = unread(r);
        if now.resolved() {
            rep.now_resolved += 1;
        }
        if then_unresolved && now.resolved() {
            rep.fixed += 1;
        }
        if !then_unresolved && r.baseline.is_some() && !now.resolved() {
            rep.broken += 1;
            if shown_broken < show {
                shown_broken += 1;
                rep.notes.push(format!(
                    "BROKEN {}: then [{}] now [{}], shape `{}`",
                    r.request_id,
                    words(recorded(r.baseline.as_deref().unwrap_or_default()).as_ref()),
                    words(Some(&now_words(&now))),
                    now.shape.as_deref().unwrap_or("—"),
                ));
            }
        }
        if !now.resolved() {
            rep.unresolved_now += 1;
            let alone = now.constructs.len() == 1;
            for c in &now.constructs {
                let e = rep.constructs.entry(c).or_default();
                e.appears += 1;
                if alone {
                    e.alone += 1;
                }
            }
            let alone = now.detail.len() == 1;
            for d in &now.detail {
                let e = rep.details.entry(d.clone()).or_default();
                e.appears += 1;
                if alone {
                    e.alone += 1;
                }
            }
            if let Some(first) = now.constructs.first() {
                let n = shown_construct.entry(first).or_default();
                if *n < show {
                    *n += 1;
                    rep.notes.push(format!(
                        "UNRESOLVED {} {first}: [{}] shape `{}`",
                        r.request_id,
                        now.constructs.join(" "),
                        now.shape.as_deref().unwrap_or("—"),
                    ));
                }
            }
            // The parse errors, by cause — once per command, under its first, because a
            // command is one decision however many errors the recovery put in it.
            if now.constructs.contains(&Construct::ParseError.as_str())
                && let Some(n) = &now.baseline.command
                && let Some(u) = n
                    .unresolved
                    .iter()
                    .find(|u| u.construct == Construct::ParseError)
            {
                *rep.parse_causes.entry(parse_cause(n, u)).or_default() += 1;
            }
        }

        // Shapes: the approval key, over every command and over the ones a person saw.
        if let Some(s) = &r.shape {
            if s.contains("<?>") {
                rep.shapes_with_hole_then += 1;
            }
            rep.shapes_then.insert(s.clone());
        }
        if let Some(s) = &now.shape {
            if s.contains("<?>") {
                rep.shapes_with_hole_now += 1;
            }
            rep.shapes_now.insert(s.clone());
        }

        let session = sessions.entry(r.session_id.clone()).or_default();
        match last_ms.get(&r.session_id) {
            Some(prev) if r.decided_ms < *prev => rep.out_of_order += 1,
            _ => {
                last_ms.insert(r.session_id.clone(), r.decided_ms);
            }
        }

        // The per-session replay: what a standing approval would have settled.
        if r.a_human_answered() {
            rep.human += 1;
            match &r.shape {
                None => rep.human_shape_unknown_then += 1,
                Some(s) => {
                    rep.human_shapes_then.insert(s.clone());
                }
            }
            if let Some(s) = &now.shape {
                rep.human_shapes_now.insert(s.clone());
            }
            let by_mode = mode_admits(&r.mode, &now);
            let by_shape = !by_mode
                && matches!(now.baseline.tier, Tier::MayApprove)
                && !now.destroys()
                && now
                    .shape
                    .as_ref()
                    .and_then(|s| session.shapes.get(s))
                    .is_some_and(|c| *c == now.class);
            let by_grant = !by_mode
                && !by_shape
                && session.grants.iter().any(|g| {
                    g.covers(
                        &now.baseline.tier,
                        now.resolved(),
                        &now.program,
                        now.class,
                        &now.baseline.intents,
                    )
                    .is_ok()
                });
            if by_mode {
                rep.human_by_mode += 1;
            }
            if by_shape {
                rep.human_by_shape += 1;
            }
            if by_grant {
                rep.human_by_grant += 1;
            }
            if by_mode || by_shape || by_grant {
                rep.human_settled += 1;
            }
            // The answer, recorded at its strongest: both a grant and a shape. See the
            // module note — the export does not carry which rung was chosen.
            session.grants.push(as_grant(&now));
            if let (Some(shape), Tier::MayApprove) = (&now.shape, &now.baseline.tier)
                && !now.destroys()
            {
                session.shapes.insert(shape.clone(), now.class);
            }
        }

        // The oracle's brief: the reading it was shown against the one there is now.
        if r.an_oracle_answered() {
            rep.oracle += 1;
            if then_unresolved {
                rep.oracle_then_unresolved += 1;
                if now.resolved() {
                    rep.oracle_then_unresolved_now_read += 1;
                }
            }
            let Some(rec) = r.baseline.as_deref().and_then(recorded) else {
                rep.oracle_unparsed += 1;
                return;
            };
            let today = now_words(&now);
            let intents: BTreeSet<String> = today.intents;
            let regions: BTreeSet<String> = today.regions.unwrap_or_default();
            let i_differ = rec.intents != intents;
            let r_differ = rec.regions.as_ref().is_some_and(|x| *x != regions);
            if i_differ {
                rep.oracle_intents_differ += 1;
            }
            if r_differ {
                rep.oracle_regions_differ += 1;
            }
            if i_differ || r_differ {
                rep.oracle_either += 1;
            }
        }
    })?;
    Ok(rep)
}

/// Fold every element of the top-level JSON array, one row at a time.
///
/// Streamed rather than parsed into a `Vec`: the largest corpus file is 184 MiB of
/// decisions, and a sweep that held all of it would make the measurement's own memory
/// the first thing an operator has to provision — while what the report needs is one
/// row at a time anyway.
fn each_row(path: &Path, mut f: impl FnMut(&Row)) -> Result<(), String> {
    struct Seq<F>(F);
    impl<'de, F: FnMut(&Row)> serde::de::Visitor<'de> for Seq<F> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an array of gate decisions")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            mut self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            while let Some(row) = seq.next_element::<Row>()? {
                (self.0)(&row);
            }
            Ok(())
        }
    }
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let reader = BufReader::with_capacity(1 << 20, file);
    let mut de = serde_json::Deserializer::from_reader(reader);
    use serde::de::Deserializer as _;
    de.deserialize_seq(Seq(&mut f))
        .map_err(|e| format!("{}: {e}", path.display()))
}

impl Report {
    /// Fold another file's report into this one, for the total across hosts. Counts add
    /// and the sets union, so a total is the sweep of the concatenated corpora.
    pub fn merge(&mut self, o: Report) {
        self.rows += o.rows;
        self.other_tools += o.other_tools;
        self.no_command += o.no_command;
        self.bash += o.bash;
        self.then_unknown += o.then_unknown;
        self.then_resolved += o.then_resolved;
        self.now_resolved += o.now_resolved;
        self.fixed += o.fixed;
        self.broken += o.broken;
        for (name, c) in o.constructs {
            let e = self.constructs.entry(name).or_default();
            e.appears += c.appears;
            e.alone += c.alone;
        }
        for (name, n) in o.parse_causes {
            *self.parse_causes.entry(name).or_default() += n;
        }
        self.unresolved_now += o.unresolved_now;
        for (name, c) in o.details {
            let e = self.details.entry(name).or_default();
            e.appears += c.appears;
            e.alone += c.alone;
        }
        self.shapes_then.extend(o.shapes_then);
        self.shapes_now.extend(o.shapes_now);
        self.shapes_with_hole_then += o.shapes_with_hole_then;
        self.shapes_with_hole_now += o.shapes_with_hole_now;
        self.human += o.human;
        self.human_shapes_then.extend(o.human_shapes_then);
        self.human_shapes_now.extend(o.human_shapes_now);
        self.human_shape_unknown_then += o.human_shape_unknown_then;
        self.human_by_grant += o.human_by_grant;
        self.human_by_shape += o.human_by_shape;
        self.human_by_mode += o.human_by_mode;
        self.human_settled += o.human_settled;
        self.oracle += o.oracle;
        self.oracle_unparsed += o.oracle_unparsed;
        self.oracle_intents_differ += o.oracle_intents_differ;
        self.oracle_regions_differ += o.oracle_regions_differ;
        self.oracle_either += o.oracle_either;
        self.oracle_then_unresolved += o.oracle_then_unresolved;
        self.oracle_then_unresolved_now_read += o.oracle_then_unresolved_now_read;
        self.out_of_order += o.out_of_order;
        self.notes.extend(o.notes);
    }

    /// The report, as the operator reads it. Numbers only: the corpus holds real
    /// commands from their sessions and not one of them is printed here.
    pub fn render(&self, label: &str) -> String {
        let mut out = String::new();
        let pct = |n: usize, d: usize| {
            if d == 0 {
                "—".to_string()
            } else {
                format!("{:.1}%", n as f64 * 100.0 / d as f64)
            }
        };
        out.push_str(&format!(
            "{label}: {} rows, {} bash commands ({} other tools, {} without a command)\n",
            self.rows, self.bash, self.other_tools, self.no_command
        ));
        out.push_str(&format!(
            "  resolved: then {} ({}) → now {} ({})   fixed {}   BROKEN {}   unrecorded {}\n",
            self.then_resolved,
            pct(self.then_resolved, self.bash - self.then_unknown),
            self.now_resolved,
            pct(self.now_resolved, self.bash),
            self.fixed,
            self.broken,
            self.then_unknown
        ));
        out.push_str(&format!(
            "  still unresolved now {} ({}); distinct shapes then {} → now {}\n",
            self.unresolved_now,
            pct(self.unresolved_now, self.bash),
            self.shapes_then.len(),
            self.shapes_now.len()
        ));
        if !self.constructs.is_empty() {
            out.push_str(&format!(
                "\n  {:<28} {:>8} {:>8}\n",
                "unresolved by construct", "appears", "alone"
            ));
            let mut v: Vec<_> = self.constructs.iter().collect();
            v.sort_by_key(|(_, c)| std::cmp::Reverse(c.alone));
            for (name, c) in v {
                out.push_str(&format!("  {:<28} {:>8} {:>8}\n", name, c.appears, c.alone));
            }
        }
        if !self.details.is_empty() {
            out.push_str(&format!(
                "\n  {:<38} {:>8} {:>8}\n",
                "…one step finer", "appears", "alone"
            ));
            let mut v: Vec<_> = self.details.iter().collect();
            v.sort_by_key(|(_, c)| std::cmp::Reverse(c.alone));
            for (name, c) in v.iter().take(16) {
                out.push_str(&format!("  {:<38} {:>8} {:>8}\n", name, c.appears, c.alone));
            }
        }
        if !self.parse_causes.is_empty() {
            out.push_str(&format!(
                "\n  {:<52} {:>8}\n",
                "parse errors by cause", "appears"
            ));
            let mut v: Vec<_> = self.parse_causes.iter().collect();
            v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
            for (name, n) in v {
                out.push_str(&format!("  {name:<52} {n:>8}\n"));
            }
        }
        out.push_str(&format!(
            "\n  human answers {}\n    distinct shapes then {} → now {}\n    \
             would now be settled by: session grant {} · approved shape {} · the point {} \
             · any {}\n    (shape not recorded then: {})\n",
            self.human,
            self.human_shapes_then.len(),
            self.human_shapes_now.len(),
            self.human_by_grant,
            self.human_by_shape,
            self.human_by_mode,
            self.human_settled,
            self.human_shape_unknown_then
        ));
        out.push_str(&format!(
            "  oracle answers {} ({} unparsed)\n    intents differ {} · regions differ {} · \
             either {}\n    unresolved then {} — of those read now {}\n",
            self.oracle,
            self.oracle_unparsed,
            self.oracle_intents_differ,
            self.oracle_regions_differ,
            self.oracle_either,
            self.oracle_then_unresolved,
            self.oracle_then_unresolved_now_read
        ));
        if self.out_of_order > 0 {
            out.push_str(&format!(
                "  NOTE: {} rows arrived out of `decided_ms` order, so the per-session \
                 replay is not exactly the order the answers were given\n",
                self.out_of_order
            ));
        }
        if !self.notes.is_empty() {
            out.push_str("\n  samples (LETIBOT_CORPUS_SHOW) — no command text:\n");
            for n in &self.notes {
                out.push_str(&format!("    {n}\n"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
