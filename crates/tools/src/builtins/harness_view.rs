//! `harness`: what the session can see about the harness it runs inside.
//! Read-only, and not as a placeholder: a tool that could change the harness from inside a turn
//! would let a model widen its own gate. It does not carry the transcript the model already has.

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
    /// What a head has on screen right now, as `(cols, rows, lines)`, or `None` when no head
    /// answered in time. Asked of the heads because only the process that drew the bytes knows the
    /// width, the scroll position and which folds are open.
    fn screen(&self) -> Option<(usize, usize, Vec<String>)>;
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

/// **The warnings whose detail is written for the operator and withheld from the model** - a line
/// that quotes the model's own words or its numbers back. Spelled here because this crate sits
/// below the daemon that names them.
const WITHHELD: &[&str] = &["context_claim_repeated"];

/// What the view says in place of a withheld detail: the fact, and the reason there are no words.
const WITHHELD_SAID: &str = "(withheld: this line quotes you and the fill back)";

impl Tool for HarnessView {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "harness",
            "What this session is running inside, read-only. `what`: `status` (the \
             startup disclosures — mode, role, seated tools, backend, adjudicator, and \
             what each one means), `warnings` (the `!` lines on this session's log, \
             which is what the operator sees in their terminal), `turn` (is a turn \
             running, how many rounds, which model), `heads` (who is attached), `screen` (WHAT THE OPERATOR IS LOOKING AT — the \
             rows a head has drawn, colour codes and all, at its real terminal size). \
             Use it instead of asking the operator to describe their screen. It reports; it \
             cannot change anything.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "what": {
                        "type": "string",
                        "enum": ["status", "warnings", "turn", "heads", "screen"],
                        "description": "Which facts to report. Defaults to `status`."
                    },
                    "limit": {"type": "integer", "description": "For `warnings`: how many, newest last."}
                }
            }),
            Access::Read,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let what = args
            .get("what")
            .and_then(|v| v.as_str())
            .unwrap_or("status");
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
                    // The model reads that the thing happened; the operator keeps the words and the
                    // numbers, which are theirs to size the session by.
                    let detail = match WITHHELD.contains(&code.as_str()) {
                        true => WITHHELD_SAID,
                        false => detail.as_str(),
                    };
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
            "screen" => match self.facts.screen() {
                Some((cols, rows_n, lines)) => {
                    let mut out = format!(
                        "the operator's terminal, {cols}x{rows_n}, exactly as a head drew \
                         it. Escape codes are included — this is the rendering, not a \
                         description of it:\n\n"
                    );
                    for l in lines {
                        out.push_str(&l);
                        out.push('\n');
                    }
                    Invocation::ok(out)
                }
                // Not "the screen is empty". Nobody drew.
                None => Invocation::failed(
                    "no head drew a screen",
                    "either nothing is attached — a one-shot or a daemon nobody has \
                     opened a window on — or no head answered in time. Nothing is on \
                     your screen that this could show, and `harness what=heads` says \
                     which of the two it is.",
                ),
            },
            other => Invocation::failed(
                format!("`harness` has no `{other}`"),
                "the five are `status`, `warnings`, `turn`, `heads` and `screen`. \
                 Nothing was read.",
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
                (
                    "flowy".into(),
                    "NO SEAT".into(),
                    "nothing is behind it".into(),
                ),
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
        fn screen(&self) -> Option<(usize, usize, Vec<String>)> {
            Some((
                80,
                2,
                vec!["\u{1b}[32mgreen\u{1b}[0m".into(), "plain".into()],
            ))
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
        fn screen(&self) -> Option<(usize, usize, Vec<String>)> {
            None
        }
    }

    /// A log with the one warning whose detail is written for the OPERATOR alone.
    struct Repeating;
    impl HarnessFacts for Repeating {
        fn disclosures(&self) -> Vec<(String, String, String)> {
            Vec::new()
        }
        fn warnings(&self) -> Vec<(String, String, u64)> {
            vec![(
                "context_claim_repeated".into(),
                "the model said its context is full again after being told it is not: \"Hmm — **my \
                 *context* ✓** ✓ ✓\" (context 21% full — 212,408 of 1,000,000 tokens). It was told \
                 once this turn; this one is for you, not a second notice."
                    .into(),
                4,
            )]
        }
        fn turn(&self) -> Option<(bool, usize, String)> {
            None
        }
        fn heads(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn screen(&self) -> Option<(usize, usize, Vec<String>)> {
            None
        }
    }

    fn ask(facts: Arc<dyn HarnessFacts>, args: &str) -> crate::ToolResult {
        let mut reg = crate::runtime::Registry::new();
        reg.register(Box::new(HarnessView::new(facts))).unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let b = crate::backend::HostBackend::new(d.path()).unwrap();
        let rt = crate::runtime::ToolRuntime::new(reg, Box::new(b));
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

    /// The five views: a warning's age rather than a timestamp, an empty log reported as empty
    /// rather than as fine, and a screen handed back as bytes rather than a description of itself.
    #[test]
    fn the_five_views_report_what_the_operator_sees() {
        let f: Arc<dyn HarnessFacts> = Arc::new(Fake);

        let status = ask(f.clone(), r#"{"what":"status"}"#);
        assert!(
            status.payload.contains("writes allowed"),
            "{}",
            status.payload
        );
        assert!(
            status.payload.contains("flowy (NO SEAT)"),
            "{}",
            status.payload
        );

        // Newest last, and aged: the `!` lines as the terminal shows them.
        let w = ask(f.clone(), r#"{"what":"warnings"}"#);
        assert!(
            w.payload.contains("! auto_compact (4s ago)"),
            "{}",
            w.payload
        );
        assert!(
            w.payload.find("old").unwrap() < w.payload.find("auto_compact").unwrap(),
            "oldest first, newest last: {}",
            w.payload
        );

        let t = ask(f.clone(), r#"{"what":"turn"}"#);
        assert!(
            t.payload.contains("RUNNING after 3 round(s)"),
            "{}",
            t.payload
        );

        let h = ask(f.clone(), r#"{"what":"heads"}"#);
        assert!(h.payload.contains("tui: dead"), "{}", h.payload);

        // The rows verbatim, escape codes and all. A view that rendered the text
        // and dropped the SGR would pass a test that looked for the words, and
        // the words are not what was asked for — the colour is.
        let s = ask(f.clone(), r#"{"what":"screen"}"#);
        assert!(s.payload.contains("80x2"), "the head's size: {}", s.payload);
        assert!(
            s.payload.contains("\u{1b}[32mgreen\u{1b}[0m"),
            "the SGR reached the model: {:?}",
            s.payload
        );
        assert!(s.payload.contains("plain"), "{}", s.payload);

        // `Access::Read`, so it never reaches the gate: asking what you are running
        // inside must not need permission, and a session with no adjudicator must still
        // be able to discover that it has none.
        assert_eq!(
            HarnessView::new(Arc::new(Fake)).schema().access,
            crate::schema::Access::Read
        );
    }

    /// Empty is reported as empty, never as fine. And no head is not an empty screen: the two look
    /// identical from the daemon, so a model told the screen is empty would explain a blank screen
    /// nobody has.
    #[test]
    fn no_head_is_never_reported_as_a_blank_screen() {
        let s = ask(Arc::new(Empty), r#"{"what":"screen"}"#);
        assert!(
            !matches!(s.outcome, letibot_transcript::ToolOutcome::Ok),
            "an absent head answered as a screen: {}",
            s.payload
        );
        let said = s.payload.to_lowercase();
        assert!(said.contains("no head"), "{}", s.payload);
        assert!(
            !said.contains("empty screen") && !said.contains("blank"),
            "the absence was dressed up as a screen: {}",
            s.payload
        );
        // And it says how to tell the two apart rather than leaving the model to guess
        // which one it hit.
        assert!(said.contains("heads"), "{}", s.payload);
    }

    /// **R93.A2: the operator's repeated-claim warning is not read out to the model.** Its detail
    /// quotes the model's own sentence and its fill back, which is the habit the check exists to
    /// stop, so the view says the code, the age and that the detail was withheld.
    #[test]
    fn the_repeated_context_claim_is_withheld_from_the_model() {
        let w = ask(Arc::new(Repeating), r#"{"what":"warnings"}"#);
        assert!(
            w.payload.contains("! context_claim_repeated (4s ago)"),
            "the warning is still said: {}",
            w.payload
        );
        assert!(
            !w.payload.contains("**my *context* ✓**"),
            "not one word of the quote: {}",
            w.payload
        );
        assert!(
            !w.payload.contains("212,408") && !w.payload.contains("% full"),
            "and none of the numbers: {}",
            w.payload
        );
        assert!(
            w.payload.contains("withheld"),
            "and it says why nothing is there: {}",
            w.payload
        );
    }

    #[test]
    fn nothing_recorded_is_said_as_nothing_recorded() {
        let e: Arc<dyn HarnessFacts> = Arc::new(Empty);
        let w = ask(e.clone(), r#"{"what":"warnings"}"#);
        assert!(
            w.payload.contains("not a claim that nothing is wrong"),
            "{}",
            w.payload
        );
        let t = ask(e.clone(), r#"{"what":"turn"}"#);
        assert!(t.payload.contains("no turn has run"), "{}", t.payload);
        let h = ask(e.clone(), r#"{"what":"heads"}"#);
        assert!(h.payload.contains("refuse with `not_run`"), "{}", h.payload);
        let s = ask(e, r#"{"what":"status"}"#);
        assert!(s.payload.contains("did not supply them"), "{}", s.payload);
    }
}
