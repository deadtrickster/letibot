//! `harness` — what the session can see about the harness it is running inside.
//!
//! **Read-only, and the first tool whose subject is letibot itself.**
//!
//! The operator's ask, 2026-09-15: *"we need to give models ability to peek into
//! letibot itself, readonly for now, very tyresome to describe what i see and
//! where on the terminal."* Until now a model working in a letibot session could
//! read the whole filesystem and not one fact about its own session — so every
//! banner line, every `!` warning and every "which mode am I in" had to be typed
//! out by hand by the person watching the terminal.
//!
//! What it answers, and each is a fact the daemon already holds:
//!
//! | `what` | from |
//! |---|---|
//! | `status` | the disclosures — mode, role, seated tools, backend, adjudicator |
//! | `warnings` | the `!` lines on this session's log, newest last |
//! | `turn` | whether a turn is running, its rounds, the model |
//! | `heads` | who is attached right now |
//!
//! # Why read-only is not a placeholder
//!
//! A tool that could CHANGE the harness from inside a turn would let a model
//! widen its own gate — the thing every layer below this refuses. The mode, the
//! permission list and the adjudicator are the operator's, and a session
//! reporting them is a different act from a session setting them. If a write
//! surface is ever wanted it belongs behind the adjudicator like any other, and
//! not in this file.
//!
//! # What it deliberately does not carry
//!
//! The transcript. A model asking to re-read its own conversation would be
//! spending context to recover context it already has, and the rows it cannot
//! see are the ones compaction deliberately replaced.

use std::sync::Arc;

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// What the harness will tell a session about itself. Supplied by the daemon,
/// which owns the hub and the config; this crate holds only the trait so that
/// `letibot-tools` keeps depending on nothing above it.
pub trait HarnessFacts: Send + Sync {
    /// One `(subject, state, detail)` per disclosure, as the banner shows them.
    fn disclosures(&self) -> Vec<(String, String, String)>;
    /// `(code, detail, age_seconds)` per warning, oldest first.
    fn warnings(&self) -> Vec<(String, String, u64)>;
    /// `(running, rounds, model)` for the turn in flight, if any.
    fn turn(&self) -> Option<(bool, usize, String)>;
    /// `(kind, identity)` per attached head.
    fn heads(&self) -> Vec<(String, String)>;
}

pub struct HarnessView {
    facts: Arc<dyn HarnessFacts>,
}

impl HarnessView {
    pub fn new(facts: Arc<dyn HarnessFacts>) -> Self {
        HarnessView { facts }
    }
}

const MAX_WARNINGS: usize = 20;

impl Tool for HarnessView {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "harness",
            "What this session is running inside, read-only. `what`: `status` (the \
             startup disclosures — mode, role, seated tools, backend, adjudicator, and \
             what each one means), `warnings` (the `!` lines on this session's log, \
             which is what the operator sees in their terminal), `turn` (is a turn \
             running, how many rounds, which model), `heads` (who is attached). Use it \
             instead of asking the operator to describe their screen. It reports; it \
             cannot change anything.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "what": {
                        "type": "string",
                        "enum": ["status", "warnings", "turn", "heads"],
                        "description": "Which facts to report. Defaults to `status`."
                    },
                    "limit": {"type": "integer", "description": "For `warnings`: how many, newest last."}
                }
            }),
            Access::Read,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let what = args.get("what").and_then(|v| v.as_str()).unwrap_or("status");
        match what {
            "status" => {
                let d = self.facts.disclosures();
                if d.is_empty() {
                    return Invocation::ok(
                        "this harness reported no disclosures, which means the daemon did \
                         not supply them rather than that the session has none."
                            .to_string(),
                    );
                }
                let mut out = String::from("this session, as the operator's banner shows it:\n");
                for (subject, state, detail) in d {
                    let head = if state.is_empty() {
                        subject.clone()
                    } else {
                        format!("{subject} ({state})")
                    };
                    out.push_str(&format!("\n[{head}]\n{detail}\n"));
                }
                Invocation::ok(out)
            }
            "warnings" => {
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(MAX_WARNINGS)
                    .clamp(1, MAX_WARNINGS);
                let all = self.facts.warnings();
                let total = all.len();
                if total == 0 {
                    // Not "no problems": nothing has been recorded on this log.
                    return Invocation::ok(
                        "no warnings on this session's log. That is the log being empty, \
                         not a claim that nothing is wrong elsewhere."
                            .to_string(),
                    );
                }
                let shown: Vec<_> = all.into_iter().rev().take(limit).rev().collect();
                let mut out = format!(
                    "{} warning(s) on this session's log{}; these are the `!` lines in the \
                     operator's terminal:\n",
                    total,
                    if total > shown.len() {
                        format!(", newest {} shown", shown.len())
                    } else {
                        String::new()
                    }
                );
                for (code, detail, age) in shown {
                    out.push_str(&format!("\n! {code} ({}s ago)\n  {detail}\n", age));
                }
                Invocation::ok(out)
            }
            "turn" => match self.facts.turn() {
                Some((running, rounds, model)) => Invocation::ok(format!(
                    "turn: {} after {rounds} round(s), model {}",
                    if running { "RUNNING" } else { "finished" },
                    if model.is_empty() { "unnamed" } else { &model }
                )),
                None => Invocation::ok(
                    "no turn has run in this session yet, so there is nothing to report \
                     about one."
                        .to_string(),
                ),
            },
            "heads" => {
                let h = self.facts.heads();
                if h.is_empty() {
                    return Invocation::ok(
                        "no head is attached. Anything the gate has to ask will refuse \
                         with `not_run` — nobody decided — rather than wait."
                            .to_string(),
                    );
                }
                let mut out = format!("{} head(s) attached:\n", h.len());
                for (kind, identity) in h {
                    out.push_str(&format!("  {kind}: {identity}\n"));
                }
                Invocation::ok(out)
            }
            other => Invocation::failed(
                format!("`harness` has no `{other}`"),
                "the four are `status`, `warnings`, `turn` and `heads`. Nothing was read.",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    impl HarnessFacts for Fake {
        fn disclosures(&self) -> Vec<(String, String, String)> {
            vec![
                ("mode".into(), String::new(), "writes allowed".into()),
                ("flowy".into(), "NO SEAT".into(), "nothing is behind it".into()),
            ]
        }
        fn warnings(&self) -> Vec<(String, String, u64)> {
            vec![
                ("old".into(), "the first thing".into(), 900),
                ("auto_compact".into(), "compacted: 3271 tokens".into(), 4),
            ]
        }
        fn turn(&self) -> Option<(bool, usize, String)> {
            Some((true, 3, "glm-5.3-flash".into()))
        }
        fn heads(&self) -> Vec<(String, String)> {
            vec![("tui".into(), "dead".into())]
        }
    }

    struct Empty;
    impl HarnessFacts for Empty {
        fn disclosures(&self) -> Vec<(String, String, String)> {
            Vec::new()
        }
        fn warnings(&self) -> Vec<(String, String, u64)> {
            Vec::new()
        }
        fn turn(&self) -> Option<(bool, usize, String)> {
            None
        }
        fn heads(&self) -> Vec<(String, String)> {
            Vec::new()
        }
    }

    fn ask(facts: Arc<dyn HarnessFacts>, args: &str) -> crate::ToolResult {
        let mut reg = crate::runtime::Registry::new();
        reg.register(Box::new(HarnessView::new(facts))).unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let b = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = crate::runtime::ToolRuntime::new(reg, Box::new(b));
        rt.invoke(
            "t",
            &letibot_transcript::ToolCall {
                id: "c".into(),
                name: "harness".into(),
                arguments: args.into(),
            },
            &mut crate::NullToolSink,
        )
    }

    /// The four views, and the distinction that matters in each: a warning's AGE
    /// rather than a timestamp (a session's clock is hours stale by the time it
    /// reads this), and an empty log reported as empty rather than as "fine".
    #[test]
    fn the_four_views_report_what_the_operator_sees() {
        let f: Arc<dyn HarnessFacts> = Arc::new(Fake);

        let status = ask(f.clone(), r#"{"what":"status"}"#);
        assert!(status.payload.contains("writes allowed"), "{}", status.payload);
        assert!(status.payload.contains("flowy (NO SEAT)"), "{}", status.payload);

        // Newest last, and aged — the `!` lines as the terminal shows them.
        let w = ask(f.clone(), r#"{"what":"warnings"}"#);
        assert!(w.payload.contains("! auto_compact (4s ago)"), "{}", w.payload);
        assert!(
            w.payload.find("old").unwrap() < w.payload.find("auto_compact").unwrap(),
            "oldest first, newest last: {}",
            w.payload
        );

        let t = ask(f.clone(), r#"{"what":"turn"}"#);
        assert!(t.payload.contains("RUNNING after 3 round(s)"), "{}", t.payload);

        let h = ask(f.clone(), r#"{"what":"heads"}"#);
        assert!(h.payload.contains("tui: dead"), "{}", h.payload);

        // `Access::Read`, so it never reaches the gate (clause 4): asking what you
        // are running inside must not need permission, and a session with no
        // adjudicator must still be able to discover that it has none.
        assert_eq!(HarnessView::new(Arc::new(Fake)).schema().access, crate::schema::Access::Read);
    }

    /// **Empty is reported as empty, never as fine.** A model told "no warnings"
    /// must not read it as "nothing is wrong" — the same rule `grep` follows when
    /// it has opened zero files.
    #[test]
    fn nothing_recorded_is_said_as_nothing_recorded() {
        let e: Arc<dyn HarnessFacts> = Arc::new(Empty);
        let w = ask(e.clone(), r#"{"what":"warnings"}"#);
        assert!(w.payload.contains("not a claim that nothing is wrong"), "{}", w.payload);
        let t = ask(e.clone(), r#"{"what":"turn"}"#);
        assert!(t.payload.contains("no turn has run"), "{}", t.payload);
        let h = ask(e.clone(), r#"{"what":"heads"}"#);
        assert!(h.payload.contains("refuse with `not_run`"), "{}", h.payload);
        let s = ask(e, r#"{"what":"status"}"#);
        assert!(s.payload.contains("did not supply them"), "{}", s.payload);
    }
}
