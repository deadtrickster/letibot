//! What a daemon needs to know before it can open a session.
//!
//! Every field is a fact the daemon cannot invent. There is no `Default` for the
//! whole struct on purpose: a default endpoint, a default model and a default
//! workspace root together describe a session against somebody else's box, and the
//! failure would be a running daemon rather than an error.

use std::path::PathBuf;

use letibot_turn::Endpoint;
use serde_json::{Value, json};

use crate::dialect::Dialect;

/// §8.3 clause 5's budget, as configuration.
///
/// **`Unset` is the default and it is correct**, per D6: an unset budget is a
/// genuine no-op, not a hidden constant. Nothing spills until somebody configures
/// one. That is a session-config gap rather than a code one, it is invisible unless
/// said, and `harnessd --spill-inline N` is where it stops being invisible.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SpillPolicy {
    /// No budget. Every payload reaches the model whole. The daemon says so at
    /// startup rather than letting a reader assume spill is on.
    #[default]
    Unset,
    /// One inline ceiling for every tool, in bytes.
    Inline(usize),
}

/// Where a spilled payload is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpillStorage {
    /// In this process. Dies with the daemon, which makes `read_spill` a lie
    /// across a restart — so it is only the default while the budget is `Unset`
    /// and nothing can spill.
    Memory,
    /// Under a directory, one subdirectory per session.
    Dir(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub dialect: Dialect,
    /// The alias the server reports. Recorded in `turn_metrics` and in the store;
    /// several models may share one dialect.
    pub model: String,
    pub endpoint: Endpoint,
    /// The GGUF the vocabulary is read from. For a split model, the first shard.
    pub vocab_gguf: PathBuf,
    /// The root every read-only tool is confined to.
    pub workspace: PathBuf,
    pub socket: PathBuf,
    /// `None` keeps the transcript in memory only — usable, and honest about it.
    pub store: Option<PathBuf>,
    pub session_id: String,
    /// A human name for the session, or empty.
    ///
    /// Empty is the default and a head then shows the id. Deliberately **not**
    /// derived from the first prompt: a title guessed from content is a title that
    /// changes under you, and a session picker whose rows rename themselves is a
    /// picker you cannot learn.
    pub title: String,
    pub owner: String,
    /// The bootstrap system prompt. Part of the stable prefix; nothing volatile
    /// belongs in it (§5.2, and the operator paid for that rule).
    pub system: String,
    /// `low` / `medium` / `high` / `xhigh`, interpreted per dialect. It is prefix
    /// bytes, so changing it mid-session re-prefills everything.
    pub effort: Option<String>,
    pub sampling: Value,
    pub spill: SpillPolicy,
    pub spill_storage: SpillStorage,
    /// How many times one user turn may go round the tool loop before the daemon
    /// stops and says so. Not a token cap — §5.7 removed those — a *loop* bound, so
    /// a model that calls `read` on the same file forever is a reported failure
    /// rather than a session that never returns.
    pub max_tool_rounds: usize,
}

/// The system prompt M1 ships.
///
/// Two rules from §5.2 are visible in it, and both cost the operator real time:
/// it says how to use the tools and never what the data contains, and it **states
/// the answer language** — oracle's prompt did not, and a Chinese-trained model
/// drifted into Chinese mid-sentence under Russian input.
///
/// Nothing volatile: no timestamp, no cwd listing, no git branch. Adding one line
/// to an `<env>` block once cost a full cold re-prefill of a 179k conversation.
pub const DEFAULT_SYSTEM: &str = "You are a careful software engineering assistant working in a \
checked-out source tree.\n\n\
Answer in English unless the user writes in another language, in which case answer in theirs.\n\n\
You have read-only tools. Use them for questions about **this tree** — its files, their contents, \
where something is defined — rather than guessing: a file you have not read is a file you do not \
know. Do not call a tool for a question about the world, about a definition, or about arithmetic; \
answer those directly. When a tool reports that it found nothing, say so — do not fill the gap \
from memory.\n\n\
Be direct. Prefer the shortest answer that is complete.";

impl Config {
    /// A session against the box this repository is developed on.
    ///
    /// Named `for_this_box` rather than `default` because that is what it is: the
    /// endpoint, the model alias and the GGUF path are this machine's, and a
    /// `Default` impl would let them travel silently.
    pub fn for_this_box(workspace: impl Into<PathBuf>) -> Config {
        Config {
            dialect: Dialect::Qwen,
            model: "qwen-3.8-flash-next".into(),
            endpoint: Endpoint::new("127.0.0.1", 8080),
            vocab_gguf: PathBuf::from(
                "/home/dead/models/qwen3.8-flash-next/\
                 Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf",
            ),
            workspace: workspace.into(),
            socket: letibot_sessionlog::server::default_socket_path(),
            store: None,
            session_id: format!("s-{}", now_ns()),
            title: String::new(),
            owner: std::env::var("USER").unwrap_or_else(|_| "operator".into()),
            system: DEFAULT_SYSTEM.into(),
            effort: None,
            // Deterministic by default: a harness whose own measurements move
            // between runs cannot tell a regression from a sample.
            sampling: json!({"temperature": 0.0, "top_k": 1, "seed": 7}),
            spill: SpillPolicy::Unset,
            spill_storage: SpillStorage::Memory,
            max_tool_rounds: 12,
        }
    }

    /// The things that are off, and why.
    ///
    /// A daemon that does not say "spill is unset" is a daemon whose operator finds
    /// out when a 40 MB grep result lands in the prompt.
    ///
    /// Takes [`GateWiring`] rather than deciding for itself, because the config does
    /// not know which tools were seated or how the backend was opened, and a
    /// disclosure that guesses is the thing this list exists to prevent.
    pub fn disclosures(&self, wiring: &GateWiring) -> Vec<Disclosure> {
        let mut out = Vec::new();
        match &self.spill {
            SpillPolicy::Unset => out.push(Disclosure::off(
                "spill",
                "UNSET",
                "no inline budget is configured, so no tool result will ever spill and \
                 every payload reaches the model whole (D6: unset is a genuine no-op). \
                 Pass --spill-inline BYTES to enable it.",
            )),
            SpillPolicy::Inline(n) => {
                out.push(Disclosure::on(
                    "spill",
                    format!("inline budget {n} bytes, all tools"),
                ));
                if self.spill_storage == SpillStorage::Memory {
                    out.push(Disclosure::off(
                        "spill store",
                        "MEMORY",
                        "a spilled payload does not survive a restart, so `read_spill` \
                         fails after one. Pass --spill-dir.",
                    ));
                }
            }
        }
        if self.store.is_none() {
            out.push(Disclosure::off(
                "store",
                "MEMORY",
                "the transcript is not persisted; the session ends with the process. \
                 Pass --store PATH.",
            ));
        }
        out.push(Disclosure::off(
            "retrieval",
            "INERT",
            "ask_code and ask_corpus return NotRun, not Abstained. No MCP server is \
             running anywhere (T16.6), so nothing was searched; saying `the corpus does \
             not cover this` would be a claim about a corpus nobody queried.",
        ));
        let (state, detail, active) = letibot_tools::adjudicate::startup_disclosure(
            &wiring.adjudicator,
            wiring.backend_writable,
            wiring.has_write_tools,
        );
        out.push(Disclosure {
            subject: "adjudication".into(),
            state: state.into(),
            detail,
            active,
        });
        out
    }
}

/// What the session actually wired, read from it rather than asserted about it.
///
/// This type exists because of a defect it now makes unwriteable. The adjudication
/// disclosure was a hard-coded sentence — *"M1 is read-only tools, which never
/// prompt (clause 4). There is no boundary and no human in the loop."* It was true
/// when it was written and **nothing checked it**. The moment a session seats a
/// role containing `write` or `edit`, that banner tells the operator there is no
/// boundary while there is one, and that nothing can prompt while everything can.
///
/// A banner whose job is to say what is off, and which says it from memory instead
/// of from the wiring, is worse than no banner: it is trusted. Every field here is
/// read at open time — [`ExecBackend::is_writable`], [`Gate::describe`], and the
/// registry's own schemas — so the sentence cannot drift away from the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateWiring {
    /// How the adjudicator identifies itself, or a string beginning `none`.
    pub adjudicator: String,
    /// Whether the execution backend was opened with a writable constructor.
    pub backend_writable: bool,
    /// Whether any seated tool declares `Access::Write`.
    pub has_write_tools: bool,
}

impl GateWiring {
    /// The read-only wiring: no adjudicator, no writable backend, no write tools.
    /// Used by callers that have not opened a session yet.
    pub fn read_only() -> GateWiring {
        GateWiring {
            adjudicator: "none (no adjudicator attached)".into(),
            backend_writable: false,
            has_write_tools: false,
        }
    }
}

/// One thing the operator has to know about this session before they trust an
/// answer from it.
///
/// Structured rather than a sentence because the banner had become a wall: five
/// paragraphs of correct prose, in which the two words that decide whether you can
/// trust the answer — the subject, and whether it is on — were buried mid-line.
/// The prose is not the problem and none of it is cut; what it needed was a shape
/// that can be *scanned*, with the sentence still under it for whoever wants to
/// know why. [`Display`](std::fmt::Display) still renders the original one-line
/// form, which is what a log line and `letibot-m1`'s header want.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disclosure {
    /// `spill`, `store`, `retrieval`, `adjudication`.
    pub subject: String,
    /// The state, in one word. `UNSET`, `MEMORY`, `INERT`, `NONE` — or empty when
    /// the thing is configured and there is nothing alarming to name.
    pub state: String,
    /// What it means, in full. Never abbreviated for the banner.
    pub detail: String,
    /// Whether the subject is doing anything. `false` is the case that has to be
    /// impossible to miss.
    pub active: bool,
}

impl Disclosure {
    /// Public because the daemon has disclosures the config cannot compute — the
    /// count of stored sessions needs the store open — and a second Disclosure
    /// constructor in the binary is a second way for the banner to be shaped.
    pub fn off(subject: &str, state: &str, detail: &str) -> Disclosure {
        Disclosure {
            subject: subject.into(),
            state: state.into(),
            detail: detail.into(),
            active: false,
        }
    }

    fn on(subject: &str, detail: impl Into<String>) -> Disclosure {
        Disclosure {
            subject: subject.into(),
            state: String::new(),
            detail: detail.into(),
            active: true,
        }
    }
}

impl std::fmt::Display for Disclosure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.state.is_empty() {
            write!(f, "{}: {}", self.subject, self.detail)
        } else {
            write!(f, "{}: {} — {}", self.subject, self.state, self.detail)
        }
    }
}

pub fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_no_spill_and_it_says_so() {
        let c = Config::for_this_box("/tmp");
        assert_eq!(c.spill, SpillPolicy::Unset);
        assert!(
            c.disclosures(&GateWiring::read_only())
                .iter()
                .any(|d| d.to_string().contains("spill: UNSET")),
            "a daemon that does not disclose an unset budget is the defect"
        );
    }

    /// The defect this replaced: the adjudication line was a sentence asserting
    /// "M1 is read-only tools, which never prompt", printed regardless of what the
    /// session had seated. Nothing here checks that a *particular* wording is
    /// produced — what it checks is that the wording MOVES when the wiring does.
    /// A banner that says the same thing about a read-only session and a session
    /// with `write` and `edit` in it is not a disclosure.
    #[test]
    fn the_adjudication_line_reads_the_wiring_rather_than_asserting_it() {
        let c = Config::for_this_box("/tmp");
        let line = |w: &GateWiring| {
            c.disclosures(w)
                .into_iter()
                .find(|d| d.subject == "adjudication")
                .expect("adjudication is always disclosed")
        };

        let ro = line(&GateWiring::read_only());
        assert!(!ro.active);
        assert!(
            ro.detail.contains("read-only"),
            "a read-only session should say so: {}",
            ro.detail
        );

        // Write tools seated, nobody to ask. This is the case the old sentence got
        // exactly backwards: it reported no boundary while the gate was refusing
        // every call.
        let unattended = GateWiring {
            adjudicator: "none (no adjudicator attached)".into(),
            backend_writable: true,
            has_write_tools: true,
        };
        let u = line(&unattended);
        assert_ne!(
            u.detail, ro.detail,
            "seating write tools must change the disclosure; it did not"
        );
        assert!(
            u.detail.contains("WRITE TOOLS"),
            "an operator must be able to see write tools are present: {}",
            u.detail
        );
        assert!(!u.active, "no adjudicator is not an active boundary");

        // Fully wired: an admitted call reaches the disk, and that is a third
        // distinct thing to say.
        let wired = GateWiring {
            adjudicator: "console adjudicator".into(),
            backend_writable: true,
            has_write_tools: true,
        };
        let w = line(&wired);
        assert!(w.active, "an attached adjudicator over a writable backend is on");
        assert_ne!(w.detail, u.detail);
    }

    #[test]
    fn every_thing_that_is_off_is_disclosed() {
        let c = Config::for_this_box("/tmp");
        let all = c
            .disclosures(&GateWiring::read_only())
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in ["spill", "store", "retrieval", "adjudication"] {
            assert!(all.contains(expected), "{expected} is not disclosed:\n{all}");
        }
    }

    #[test]
    fn the_system_prompt_carries_nothing_volatile() {
        // §5.2 rule 1, as a test rather than as a habit.
        for volatile in ["/home/", "20", "branch", "commit"] {
            assert!(
                !DEFAULT_SYSTEM.contains(volatile),
                "the stable prefix contains {volatile:?}, which will change and re-prefill"
            );
        }
        assert!(DEFAULT_SYSTEM.contains("Answer in English"), "§5.2: state the language");
    }
}
