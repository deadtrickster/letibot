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

    /// One line for the startup banner, naming the things that are off.
    ///
    /// A daemon that does not say "spill is unset" is a daemon whose operator finds
    /// out when a 40 MB grep result lands in the prompt.
    pub fn disclosures(&self) -> Vec<String> {
        let mut out = Vec::new();
        match &self.spill {
            SpillPolicy::Unset => out.push(
                "spill: UNSET — no inline budget is configured, so no tool result will \
                 ever spill and every payload reaches the model whole (D6: unset is a \
                 genuine no-op). Pass --spill-inline BYTES to enable it."
                    .into(),
            ),
            SpillPolicy::Inline(n) => {
                out.push(format!("spill: inline budget {n} bytes, all tools"));
                if self.spill_storage == SpillStorage::Memory {
                    out.push(
                        "spill store: MEMORY — a spilled payload does not survive a \
                         restart, so `read_spill` fails after one. Pass --spill-dir."
                            .into(),
                    );
                }
            }
        }
        if self.store.is_none() {
            out.push(
                "store: MEMORY — the transcript is not persisted; the session ends with \
                 the process. Pass --store PATH."
                    .into(),
            );
        }
        out.push(
            "retrieval: INERT — ask_code and ask_corpus return NotRun, not Abstained. \
             No MCP server is running anywhere (T16.6), so nothing was searched; \
             saying `the corpus does not cover this` would be a claim about a corpus \
             nobody queried."
                .into(),
        );
        out.push(
            "adjudication: NONE — M1 is read-only tools, which never prompt (clause 4). \
             There is no boundary and no human in the loop."
                .into(),
        );
        out
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
            c.disclosures().iter().any(|d| d.contains("spill: UNSET")),
            "a daemon that does not disclose an unset budget is the defect"
        );
    }

    #[test]
    fn every_thing_that_is_off_is_disclosed() {
        let c = Config::for_this_box("/tmp");
        let all = c.disclosures().join("\n");
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
