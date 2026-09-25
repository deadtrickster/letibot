//! `decisions` — what the gate decided, and what the oracle said about it.
//!
//! The third tool in the same family as `transcript` and `digest`, and it exists
//! for the same reason. The operator, 2026-09-19, watching a session try to work
//! out why an oracle's reply never reached a tool card:
//!
//! ```text
//! bash {"command": "cd letibot; sqlite3 -line
//!   /home/dead/.local/share/letibot/sessions.db \"SELECT request_id, turn_id,
//!   tool, verdict, verdict_by, verdict_basis, oracle_ms FROM adjudication
//!   WHERE request_id='adj-s-1789639478142928813-0225';\" …"}
//! ```
//!
//! Their rule, stated plainly: *"the model shouldn't derive the storage, its
//! format, or what path it"* is at. That call derives all three — a hardcoded
//! `~/.local/share` path, a table name, and eight column names written from
//! memory against a schema with thirty of them. Get one wrong and sqlite answers
//! `no such column`, which reads to a model as *the fact is not recorded*.
//!
//! So every argument here is a **name a person would use**, and none of them is a
//! column:
//!
//! | argument | the question it answers |
//! |---|---|
//! | `request` | what happened to this one decision, in full |
//! | `tool` | every decision about `bash`, or `write` |
//! | `verdict` | `allow`, `deny`, `ask`, `unavailable` |
//! | `by` | who decided: `operator`, `oracle`, `boundary`, `gate` |
//! | `asked` | was a person actually put in front of it |
//! | `last` | the newest N, which is the window the warm start reads |
//! | `session` | another session, by id or part of its title |
//!
//! # Why `verdict_basis` is the field this was built for
//!
//! The corpus keeps layer B in parts: `verdict`, `verdict_by`, `verdict_basis`
//! and `oracle_ms` are separate columns precisely so that nobody has to re-parse
//! prose to recover them. `verdict_basis` is **the oracle's own sentence** — the
//! reasoning it gave for admitting or refusing — and it is the one thing that
//! exists nowhere else once the decision has settled: `SettledDecision` on the
//! wire carries the DECIDER's basis, not the advisor's, so a head that draws an
//! `oracle:` line from it is drawing the operator's own words under the oracle's
//! name.
//!
//! That is a real bug being chased in another session as this lands. This tool
//! does not fix it. It makes the difference between the two bases legible, which
//! is the step that was costing a `sqlite3` call.
//!
//! # Read-only, and a miss is a report
//!
//! Nothing here rules on anything: labelling a decision is the operator's, through
//! `/gate ok|grant|revoke`. A search that matches nothing comes back with the
//! counts and the values that ARE present in each field it filtered on — because
//! the common miss is a verdict word spelled the way the enum spells it rather
//! than the way the corpus stores it, and a bare "no rows" sends a model back to
//! sqlite.

use std::sync::Arc;

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// One decision, as a reader sees it. A flattened view of the corpus row: the
/// columns a person asks about, under the names they ask about them by.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecisionRow {
    pub request_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub decided_ms: i64,
    /// Layer A's normalised reading of the call — never the raw command text,
    /// for the same reason the oracle is not shown it.
    pub action: String,
    pub tool: String,
    /// Where the gate was standing. The same call admits under `allow-all` and
    /// asks under `always-ask`, so a row without it is not interpretable.
    pub mode: String,
    pub tier: String,
    pub effect: String,
    /// `allow`, `deny`, `ask`, `unavailable` — as the corpus stores it.
    pub verdict: String,
    /// Who decided: `operator`, `oracle`, `boundary:…`, `gate:…`.
    pub verdict_by: String,
    /// **The decider's own sentence.** For an oracle row this is the guard
    /// model's reasoning, and it exists nowhere else after the decision settles.
    pub verdict_basis: String,
    /// Calibrated P(allow) where the oracle returned logprobs.
    pub p_allow: Option<f64>,
    pub oracle_ms: Option<i64>,
    pub oracle_model: String,
    /// True when a person was actually put in front of this call and answered.
    /// Different from `verdict_by = operator`: the gate can record an operator
    /// decision that came from a standing rule nobody was asked about.
    pub asked: bool,
    /// The operator's later ruling on the decision, when they gave one.
    pub operator_kind: String,
    pub operator_note: String,
    /// The exact bytes the oracle was shown. `None` when no oracle was consulted
    /// — which is a fact about the row, not a missing value.
    pub shown: Option<String>,
}

/// What the corpus holds, for `what=counts`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionCounts {
    pub total: u64,
    /// A person was put in front of it and answered.
    pub decided_by_operator: u64,
    /// An oracle was actually consulted and gave a verdict.
    pub measured: u64,
    /// The operator ruled against what the model would have done.
    pub disagreements: u64,
}

/// What one `decisions` call asked for. Predicates AND, like `transcript`'s.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionQuery {
    /// One decision by its request id. Every other predicate is ignored when set.
    pub request: Option<String>,
    /// `None` is this session. Otherwise an id, part of a title, or `any`.
    pub session: Option<String>,
    pub tool: Option<String>,
    /// `allow`, `deny`, `ask`, `unavailable`.
    pub verdict: Option<String>,
    /// `operator`, `oracle`, `boundary`, `gate` — matched as a prefix, because
    /// the stored value is `boundary:flow` and the question is about boundaries.
    pub by: Option<String>,
    /// `Some(true)`: only decisions a person was actually asked.
    pub asked: Option<bool>,
    /// Only decisions the operator later ruled against the gate on.
    pub disagreements: bool,
    /// The newest N.
    pub last: Option<u32>,
    pub limit: usize,
    pub offset: usize,
}

/// What a search found, with the values present in each filtered field so a miss
/// can name them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecisionHits {
    pub rows: Vec<DecisionRow>,
    pub session_id: String,
    pub scanned: usize,
    pub matched: usize,
    /// Distinct `verdict` values in scope, for the miss report.
    pub verdicts_present: Vec<String>,
    /// Distinct `tool` values in scope.
    pub tools_present: Vec<String>,
    /// Distinct `verdict_by` values in scope.
    pub deciders_present: Vec<String>,
}

/// The corpus, as this tool needs it. Implemented by the daemon over `Store`.
pub trait DecisionSource: Send + Sync {
    fn find(&self, q: &DecisionQuery) -> Result<DecisionHits, String>;
    fn counts(&self, session: Option<&str>) -> Result<DecisionCounts, String>;
}

/// No corpus behind it: refuse by name. "No decisions recorded" and "nowhere to
/// look" are different facts, and only one of them is about the gate.
pub struct NoDecisions;

impl DecisionSource for NoDecisions {
    fn find(&self, _q: &DecisionQuery) -> Result<DecisionHits, String> {
        Err(
            "this session has no store, so no decision was written down. The gate \
             still ran — a decision the corpus did not record is not a decision that \
             did not happen."
                .into(),
        )
    }
    fn counts(&self, _session: Option<&str>) -> Result<DecisionCounts, String> {
        Err("this session has no store, so the corpus is empty by construction.".into())
    }
}

pub struct DecisionsTool {
    src: Arc<dyn DecisionSource>,
}

impl DecisionsTool {
    pub fn new(src: Arc<dyn DecisionSource>) -> Self {
        DecisionsTool { src }
    }
}

const DEFAULT_LIMIT: usize = 12;
const MAX_LIMIT: usize = 200;
/// Per-field excerpt for a listed row. The full text of one decision is what
/// `request=` is for.
const EXCERPT: usize = 400;

impl Tool for DecisionsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "decisions",
            "WHAT THE GATE DECIDED, and what the guard model said about it. Read-only. \
             Every permission this session asked about is recorded with its verdict, \
             who decided, their stated reason, and whether a person was actually \
             asked. Predicates AND: `request` (one decision in full, including the \
             exact brief the oracle was shown), `tool`, `verdict` \
             (allow/deny/ask/unavailable), `by` (operator/oracle/boundary/gate), \
             `asked`, `last`, `session`. Use it instead of guessing why a call was \
             refused, or asking the operator what they approved.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "string",
                        "description": "One decision by request id, in full — every field, plus the brief the oracle saw. Other predicates are ignored."
                    },
                    "tool": {"type": "string", "description": "Decisions about one tool: bash, write, edit, web_fetch…"},
                    "verdict": {"type": "string", "description": "allow, deny, ask, unavailable."},
                    "by": {
                        "type": "string",
                        "description": "Who decided: operator, oracle, boundary, gate. Matched as a prefix, so `boundary` finds boundary:flow and boundary:always_ask."
                    },
                    "asked": {"type": "boolean", "description": "true: only decisions a person was actually put in front of."},
                    "disagreements": {"type": "boolean", "description": "true: only decisions the operator later ruled against the gate on."},
                    "last": {"type": "integer", "description": "Only the newest N decisions."},
                    "session": {"type": "string", "description": "Another session by id or part of its title, or `any` for the whole corpus. Defaults to this one."},
                    "limit": {"type": "integer", "description": "How many rows. Default 12."},
                    "offset": {"type": "integer", "description": "Skip this many, to continue a previous call."},
                    "what": {
                        "type": "string",
                        "enum": ["rows", "counts"],
                        "description": "`rows` (the default) lists decisions; `counts` reports how many, how many a person answered, how many an oracle measured, and how many the operator overruled."
                    }
                }
            }),
            Access::Read,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        match args.get("what").and_then(|v| v.as_str()).unwrap_or("rows") {
            "counts" => self.counts(args),
            "rows" => self.rows(args),
            other => Invocation::failed(
                format!("`{other}` is not something decisions reports"),
                "There are two: `rows` (the default) and `counts`.".to_string(),
            ),
        }
    }
}

impl DecisionsTool {
    fn counts(&self, args: &Value) -> Invocation {
        let session = args.get("session").and_then(|v| v.as_str());
        match self.src.counts(session) {
            Err(e) => Invocation::failed(e, String::new()),
            Ok(c) if c.total == 0 => Invocation::ok(
                "no decisions recorded. Either nothing has been gated here yet, or \
                 this seat's tools are all read-only — a read never reaches the gate."
                    .to_string(),
            ),
            Ok(c) => Invocation::ok(format!(
                "{} decision(s) recorded.\n\n  \
                 {} a person was actually asked and answered\n  \
                 {} an oracle was consulted and gave a verdict\n  \
                 {} the operator later ruled AGAINST what the gate did\n\n\
                 The first two are independent: a decision can be measured by an \
                 oracle and answered by a person, or settled by a standing rule with \
                 neither.\n",
                c.total, c.decided_by_operator, c.measured, c.disagreements
            )),
        }
    }

    fn rows(&self, args: &Value) -> Invocation {
        let s = |k: &str| {
            args.get(k)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let q = DecisionQuery {
            request: s("request"),
            session: s("session"),
            tool: s("tool"),
            verdict: s("verdict"),
            by: s("by"),
            asked: args.get("asked").and_then(|v| v.as_bool()),
            disagreements: args
                .get("disagreements")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            last: args.get("last").and_then(|v| v.as_u64()).map(|v| v as u32),
            limit: args
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(DEFAULT_LIMIT)
                .clamp(1, MAX_LIMIT),
            offset: args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        };

        let hits = match self.src.find(&q) {
            Ok(h) => h,
            Err(e) => return Invocation::failed(e, String::new()),
        };

        // One decision asked for by id gets everything, including the brief. That
        // is the call this tool replaced, and half a row is what sent somebody to
        // sqlite in the first place.
        if let Some(want) = &q.request {
            return match hits.rows.first() {
                Some(r) => Invocation::ok(render_full(r)),
                None => Invocation::ok(format!(
                    "no decision `{want}` in this corpus. {} decision(s) were searched. \
                     A request id is minted per gated call and looks like \
                     `adj-<session>-<n>`; `decisions last=20` lists recent ones with \
                     their ids.",
                    hits.scanned
                )),
            };
        }

        if hits.matched == 0 {
            return Invocation::ok(miss(&q, &hits));
        }

        let mut out = format!(
            "{} decision(s) matched{}, {} shown. {} searched in session {}.\n\n",
            hits.matched,
            describe(&q),
            hits.rows.len(),
            hits.scanned,
            hits.session_id,
        );
        for r in &hits.rows {
            out.push_str(&render_short(r));
            out.push('\n');
        }
        let shown = q.offset + hits.rows.len();
        if hits.matched > shown {
            out.push_str(&format!(
                "\n{} more. `offset={shown}` reads the next page; `request=<id>` reads \
                 one in full, with the brief the oracle was shown.\n",
                hits.matched - shown
            ));
        }
        Invocation::ok(out)
    }
}

/// One decision, in full. Every field, and the two bases kept apart — which is
/// the distinction the whole tool was built around.
fn render_full(r: &DecisionRow) -> String {
    let mut out = format!(
        "decision {}\n\n  \
         when      {} ms\n  \
         session   {}\n  \
         turn      {}\n  \
         tool      {}\n  \
         action    {}\n  \
         mode      {}\n  \
         tier      {}\n  \
         effect    {}\n\n",
        r.request_id,
        r.decided_ms,
        r.session_id,
        r.turn_id,
        if r.tool.is_empty() {
            "(unrecorded)"
        } else {
            &r.tool
        },
        r.action,
        if r.mode.is_empty() {
            "(unrecorded)"
        } else {
            &r.mode
        },
        r.tier,
        r.effect,
    );
    out.push_str(&format!(
        "  verdict   {}\n  by        {}\n",
        if r.verdict.is_empty() {
            "(none recorded)"
        } else {
            &r.verdict
        },
        if r.verdict_by.is_empty() {
            "(none recorded)"
        } else {
            &r.verdict_by
        },
    ));
    if !r.verdict_basis.is_empty() {
        out.push_str(&format!("  because   {}\n", r.verdict_basis));
    }
    match (r.p_allow, r.oracle_ms) {
        (Some(p), Some(ms)) => {
            out.push_str(&format!("  P(allow)  {p:.3}, in {ms} ms\n"));
        }
        (None, Some(ms)) => out.push_str(&format!(
            "  oracle    answered in {ms} ms, no logprobs — so `allow` here is a \
             string, not a confidence\n"
        )),
        _ => {}
    }
    if !r.oracle_model.is_empty() {
        out.push_str(&format!("  oracle    {}\n", r.oracle_model));
    }
    out.push_str(&format!(
        "  asked     {}\n",
        if r.asked {
            "yes — a person was put in front of this and answered"
        } else {
            "no — nobody was asked; a rule, a boundary or an oracle settled it"
        }
    ));
    if !r.operator_kind.is_empty() {
        out.push_str(&format!(
            "\n  the operator later ruled: {}",
            r.operator_kind
        ));
        if !r.operator_note.is_empty() {
            out.push_str(&format!(" — {}", r.operator_note));
        }
        out.push('\n');
    }
    match &r.shown {
        Some(brief) => out.push_str(&format!(
            "\nthe brief the oracle was shown, exactly:\n\n{brief}\n"
        )),
        // Not a missing value: it is the fact that layer B never ran.
        None => out.push_str(
            "\nNo oracle was consulted for this one, so there is no brief. `because` \
             above is then whoever DID decide — a boundary rule, the gate, or the \
             operator — and not a model's reasoning.\n",
        ),
    }
    out
}

/// One decision, on two lines.
fn render_short(r: &DecisionRow) -> String {
    let head = format!(
        "  {}  {}  {}  by {}{}",
        r.request_id,
        if r.verdict.is_empty() {
            "?"
        } else {
            &r.verdict
        },
        if r.tool.is_empty() {
            "(tool unrecorded)"
        } else {
            &r.tool
        },
        if r.verdict_by.is_empty() {
            "?"
        } else {
            &r.verdict_by
        },
        if r.asked { "  [person asked]" } else { "" },
    );
    let why = if r.verdict_basis.is_empty() {
        "      (no reason recorded)".to_string()
    } else {
        format!("      {}", excerpt(&r.verdict_basis, EXCERPT))
    };
    format!("{head}\n{why}\n")
}

fn describe(q: &DecisionQuery) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = &q.tool {
        parts.push(format!("tool `{t}`"));
    }
    if let Some(v) = &q.verdict {
        parts.push(format!("verdict `{v}`"));
    }
    if let Some(b) = &q.by {
        parts.push(format!("decided by `{b}`"));
    }
    match q.asked {
        Some(true) => parts.push("where a person was asked".into()),
        Some(false) => parts.push("where nobody was asked".into()),
        None => {}
    }
    if q.disagreements {
        parts.push("the operator overruled".into());
    }
    if let Some(n) = q.last {
        parts.push(format!("in the last {n}"));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(" for {}", parts.join(", "))
}

/// Clause 1: the miss names what IS in each field it filtered on, because the
/// common miss is a word spelled the way an enum spells it rather than the way
/// the corpus stores it — and a bare "no rows" sends a model back to sqlite.
fn miss(q: &DecisionQuery, hits: &DecisionHits) -> String {
    let mut out = format!(
        "no decision matched{}. {} were searched in session {}, so the corpus was \
         read — this is an empty result, not a failure.\n",
        describe(q),
        hits.scanned,
        hits.session_id,
    );
    if hits.scanned == 0 {
        out.push_str(
            "\nThe corpus has no rows for this session at all. A read-only seat never \
             reaches the gate, so a session that only read files records nothing here. \
             `session=any` searches every session.\n",
        );
        return out;
    }
    let mut named = false;
    if q.verdict.is_some() && !hits.verdicts_present.is_empty() {
        out.push_str(&format!(
            "\nThe verdicts actually in scope are: {}.\n",
            hits.verdicts_present.join(", ")
        ));
        named = true;
    }
    if q.tool.is_some() && !hits.tools_present.is_empty() {
        out.push_str(&format!(
            "\nThe tools with decisions here are: {}.\n",
            hits.tools_present.join(", ")
        ));
        named = true;
    }
    if q.by.is_some() && !hits.deciders_present.is_empty() {
        out.push_str(&format!(
            "\nWho actually decided things here: {}. `by` matches a prefix, so `gate` \
             finds `gate:timeout`.\n",
            hits.deciders_present.join(", ")
        ));
        named = true;
    }
    if !named {
        out.push_str(&format!(
            "\nIn scope there are {} verdict(s) ({}), {} tool(s) ({}) and {} \
             decider(s) ({}).\n",
            hits.verdicts_present.len(),
            hits.verdicts_present.join(", "),
            hits.tools_present.len(),
            hits.tools_present.join(", "),
            hits.deciders_present.len(),
            hits.deciders_present.join(", "),
        ));
    }
    if q.session.is_none() {
        out.push_str(
            "\nThis searched only the current session. `session=any` searches the \
             whole corpus.\n",
        );
    }
    out
}

fn excerpt(text: &str, cap: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= cap {
        return t.to_string();
    }
    let head: String = t.chars().take(cap).collect();
    format!("{head}… (`request=<id>` for the whole of it)")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Vec<DecisionRow>);

    impl DecisionSource for Fake {
        fn find(&self, q: &DecisionQuery) -> Result<DecisionHits, String> {
            let rows: Vec<DecisionRow> = self
                .0
                .iter()
                .filter(|r| q.request.as_ref().is_none_or(|w| &r.request_id == w))
                .filter(|r| q.tool.as_ref().is_none_or(|t| &r.tool == t))
                .filter(|r| q.verdict.as_ref().is_none_or(|v| &r.verdict == v))
                .filter(|r| q.by.as_ref().is_none_or(|b| r.verdict_by.starts_with(b)))
                .filter(|r| q.asked.is_none_or(|a| r.asked == a))
                .cloned()
                .collect();
            let matched = rows.len();
            let mut verdicts: Vec<String> = self.0.iter().map(|r| r.verdict.clone()).collect();
            verdicts.sort();
            verdicts.dedup();
            let mut tools: Vec<String> = self.0.iter().map(|r| r.tool.clone()).collect();
            tools.sort();
            tools.dedup();
            let mut by: Vec<String> = self.0.iter().map(|r| r.verdict_by.clone()).collect();
            by.sort();
            by.dedup();
            Ok(DecisionHits {
                rows: rows.into_iter().skip(q.offset).take(q.limit).collect(),
                session_id: "s1".into(),
                scanned: self.0.len(),
                matched,
                verdicts_present: verdicts,
                tools_present: tools,
                deciders_present: by,
            })
        }
        fn counts(&self, _s: Option<&str>) -> Result<DecisionCounts, String> {
            Ok(DecisionCounts {
                total: self.0.len() as u64,
                decided_by_operator: self.0.iter().filter(|r| r.asked).count() as u64,
                measured: self.0.iter().filter(|r| r.oracle_ms.is_some()).count() as u64,
                disagreements: 1,
            })
        }
    }

    fn oracle_row() -> DecisionRow {
        DecisionRow {
            request_id: "adj-s1-0225".into(),
            session_id: "s1".into(),
            turn_id: "t9".into(),
            decided_ms: 1_700,
            action: "read a file under the project".into(),
            tool: "bash".into(),
            mode: "automode-edits".into(),
            tier: "MayApprove".into(),
            effect: "read".into(),
            verdict: "allow".into(),
            verdict_by: "oracle:glm".into(),
            verdict_basis: "the operator asked for the man page sweep in this turn".into(),
            p_allow: Some(0.94),
            oracle_ms: Some(2_100),
            oracle_model: "glm-5.3-flash".into(),
            asked: false,
            operator_kind: String::new(),
            operator_note: String::new(),
            shown: Some("=== the brief ===\ncommand: ar t libfoo.a".into()),
        }
    }

    fn human_row() -> DecisionRow {
        DecisionRow {
            request_id: "adj-s1-0226".into(),
            tool: "write".into(),
            verdict: "deny".into(),
            verdict_by: "human:dead".into(),
            verdict_basis: "dead chose `deny` at the head".into(),
            asked: true,
            shown: None,
            session_id: "s1".into(),
            ..Default::default()
        }
    }

    fn ask(rows: Vec<DecisionRow>, args: Value) -> crate::ToolResult {
        let mut reg = crate::runtime::Registry::new();
        reg.register(Box::new(DecisionsTool::new(Arc::new(Fake(rows)))))
            .unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let b = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = crate::runtime::ToolRuntime::new(reg, Box::new(b));
        rt.invoke(
            "t",
            &letibot_transcript::ToolCall {
                id: "c".into(),
                name: "decisions".into(),
                arguments: args.to_string(),
            },
            &mut crate::NullToolSink,
        )
    }

    /// The call this tool replaced, as one argument — and it returns every field
    /// that sqlite3 query selected by name, plus the brief it did not think to ask
    /// for.
    #[test]
    fn one_request_comes_back_whole_including_the_brief() {
        let out = ask(
            vec![oracle_row(), human_row()],
            serde_json::json!({"request": "adj-s1-0225"}),
        )
        .payload;
        assert!(out.contains("adj-s1-0225"), "{out}");
        assert!(out.contains("t9"), "the turn: {out}");
        assert!(out.contains("bash"), "the tool: {out}");
        assert!(out.contains("allow"), "the verdict: {out}");
        assert!(out.contains("oracle:glm"), "who: {out}");
        // The field this was built for.
        assert!(out.contains("man page sweep"), "the oracle's reason: {out}");
        assert!(out.contains("2100 ms"), "the latency: {out}");
        assert!(out.contains("0.940"), "the confidence: {out}");
        assert!(out.contains("ar t libfoo.a"), "the brief: {out}");
    }

    /// A human decision has no brief, and that is a fact about it rather than a
    /// missing value — the row says layer B never ran.
    #[test]
    fn a_decision_with_no_oracle_says_so_rather_than_showing_an_empty_brief() {
        let out = ask(
            vec![human_row()],
            serde_json::json!({"request": "adj-s1-0226"}),
        )
        .payload;
        assert!(out.contains("No oracle was consulted"), "{out}");
        assert!(
            out.contains("not a model's reasoning"),
            "and warns that `because` is then somebody else's: {out}"
        );
        assert!(out.contains("a person was put in front of this"), "{out}");
    }

    #[test]
    fn predicates_and_together() {
        let rows = vec![oracle_row(), human_row()];
        let out = ask(rows.clone(), serde_json::json!({"by": "oracle"})).payload;
        assert!(out.contains("adj-s1-0225"), "{out}");
        assert!(!out.contains("adj-s1-0226"), "{out}");
        let out = ask(rows, serde_json::json!({"asked": true})).payload;
        assert!(out.contains("adj-s1-0226"), "{out}");
        assert!(out.contains("[person asked]"), "{out}");
    }

    /// The miss that matters: a verdict word spelled the enum's way. It comes back
    /// with the words the corpus actually uses, not a bare "no rows".
    #[test]
    fn a_verdict_that_matches_nothing_names_the_ones_that_do() {
        let out = ask(
            vec![oracle_row(), human_row()],
            serde_json::json!({"verdict": "Selected"}),
        )
        .payload;
        assert!(out.contains("no decision matched"), "{out}");
        assert!(out.contains("allow"), "{out}");
        assert!(out.contains("deny"), "{out}");
        assert!(out.contains("empty result, not a failure"), "{out}");
    }

    #[test]
    fn an_unknown_request_id_says_how_ids_are_shaped_and_how_to_list_them() {
        let out = ask(vec![oracle_row()], serde_json::json!({"request": "nope"})).payload;
        assert!(out.contains("no decision `nope`"), "{out}");
        assert!(out.contains("decisions last=20"), "{out}");
    }

    /// The four counts, and the sentence that stops them being read as a partition.
    #[test]
    fn counts_report_the_four_numbers_and_that_two_of_them_overlap() {
        let out = ask(
            vec![oracle_row(), human_row()],
            serde_json::json!({"what": "counts"}),
        )
        .payload;
        assert!(out.contains("2 decision(s) recorded"), "{out}");
        assert!(out.contains("1 a person was actually asked"), "{out}");
        assert!(out.contains("1 an oracle was consulted"), "{out}");
        assert!(out.contains("independent"), "{out}");
    }

    #[test]
    fn an_empty_corpus_says_why_it_might_be_empty() {
        let out = ask(vec![], serde_json::json!({})).payload;
        assert!(out.contains("no rows for this session"), "{out}");
        assert!(out.contains("read-only seat never"), "{out}");
        assert!(out.contains("session=any"), "{out}");
    }

    #[test]
    fn no_store_refuses_by_name() {
        let mut reg = crate::runtime::Registry::new();
        reg.register(Box::new(DecisionsTool::new(Arc::new(NoDecisions))))
            .unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let b = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = crate::runtime::ToolRuntime::new(reg, Box::new(b));
        let r = rt.invoke(
            "t",
            &letibot_transcript::ToolCall {
                id: "c".into(),
                name: "decisions".into(),
                arguments: "{}".into(),
            },
            &mut crate::NullToolSink,
        );
        match &r.outcome {
            letibot_transcript::ToolOutcome::Failed { reason } => {
                assert!(reason.contains("no store"), "{reason}");
                // And it does not let "unrecorded" be read as "did not happen".
                assert!(
                    reason.contains("not a decision that did not happen"),
                    "{reason}"
                );
            }
            other => panic!("{other:?}"),
        }
    }
}
