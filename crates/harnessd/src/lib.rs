//! `harnessd`: the daemon that assembles the parts into a working harness.
//!
//! ```text
//!         ┌──────────────────────── harnessd ────────────────────────┐
//!         │                                                          │
//!  head ──┼─ sessionlog::serve ── Registry ─ next_command ─ worker ───┼── llama.cpp
//!         │                     Hub per session               │      │   /completion
//!         │                     LogSink            Harness per session│
//!         │                   ToolLogSink        engine · tools       │
//!         │                                       ledger · store      │
//!         └──────────────────────────────────────────────────────────┘
//! ```
//!
//! A `Hub` per session and a `Harness` per `Hub` ([`sessions`]), served over a Unix
//! socket with `letibot-sessionlog`'s protocol; the transcript, the token ledger and
//! the token blobs persist to SQLite, where append-only is a trigger rather than a
//! convention. What is off is printed at startup by `Config::disclosures`: a
//! capability that is off and silent cannot be told from one that is on and broken.

/// stderr is the daemon's log. A full disk or a closed reader must not panic the thread that
/// printed: a dead signal thread leaves a daemon no SIGTERM stops.
macro_rules! eprintln {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($t)*);
    }};
}

pub mod activity;
pub mod answers;
pub mod attachments;
pub mod backfill;
pub mod bangrun;
pub mod calibrate;
pub mod changelog_scope;
pub mod child_todos;
pub mod citations;
pub mod cli;
pub mod close_settle;
pub mod compaction_relevance;
pub mod config;
pub mod context_claim;
pub mod corpus;
pub mod daemon;
pub mod decision_source;
pub mod delivery;
pub mod dialect;
pub mod done_offer;
pub mod editor_context;
/// The box's one embedder: `[embeddings]`, the model loaded once, and a tier per caller.
pub mod embeddings;
pub mod etalon;
pub mod etalon_map;
pub mod etalon_oracle;
pub mod exe_stamp;
pub mod facts;
pub mod footprint;
pub mod github;
pub mod harness;
pub mod httphead;
pub mod instruments;
pub mod jobwatch;
pub mod leases;
// The seat's config layer, re-exported rather than re-implemented: which file a leticode
// reads is a leticode decision, and it lives in `crates/leticode-config` outside the
// daemon. This binding keeps every `crate::leticode_config::…` path below compiling.
pub use leticode_config;
pub mod m1;
pub mod memwatch;
pub mod mergequeue;
pub mod modes;
pub mod nag_pause;
pub mod notes_offer;
pub mod notes_scope;
pub mod offer_rows;
pub mod operator_ask;
pub mod oracle;
pub mod perimeter;
pub mod plugins;
pub mod pr_watch;
pub mod progress;
pub mod prompt;
pub mod recall_index;
pub mod recall_offer;
pub mod restart;
pub mod room;
pub mod selfverbs;
pub mod sessions;
pub mod skills_offer;
pub mod slash;
pub mod standing_notes;
pub mod store_floor;
pub mod subagent_record;
pub mod sudo;
pub mod suggest;
pub mod supervise;
pub mod tasks;
pub mod term;
pub mod transcript_source;
pub mod turn_tail;
pub mod vm_reap;
pub mod waits_offer;
pub mod wall;
// One process per session's transcript: the claim in `letibot_tokencore::store::writers`
// that refuses a second daemon's open, and the sentence it is refused with.
pub mod writer;

pub use answers::{Answers, HeadAdjudicator};
pub use config::{Config, SpillPolicy, SpillStorage};
pub use daemon::Daemon;
pub use dialect::{Dialect, Wiring};
pub use harness::{Harness, HarnessError, HubSteering, Parts, Reply};
pub use modes::ModeStore;
pub use sessions::{Outcome, OutcomeSaid, Sessions};
pub use tasks::{TaskEntry, TaskJournal};
pub use term::Terminals;

/// The vocabulary GGUF this build can LOAD, for a test: `None` with the skip announced.
pub fn vocab_gguf() -> Option<std::path::PathBuf> {
    #[cfg(feature = "local")]
    {
        letibot_llama::apparatus::present_gguf()
    }
    #[cfg(not(feature = "local"))]
    {
        letibot_tokencore::apparatus::absent(NO_LLAMA);
        None
    }
}

/// [`vocab_gguf`] for a named file: it must LOAD here, not merely exist. For a test that pins its
/// own GGUF rather than the box's default one.
pub fn vocab_file(path: &std::path::Path) -> Option<()> {
    #[cfg(feature = "local")]
    {
        letibot_llama::apparatus::present_file(path)
    }
    #[cfg(not(feature = "local"))]
    {
        let _ = path;
        letibot_tokencore::apparatus::absent(NO_LLAMA);
        None
    }
}

/// What a build without `local` says about the vocabulary it cannot load.
#[cfg(not(feature = "local"))]
const NO_LLAMA: &str = "llama.cpp: this harnessd was built without the `local` feature, so a \
                       vocabulary GGUF cannot be loaded";
