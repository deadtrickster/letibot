//! `harnessd` (T17): the daemon that assembles the parts into a working harness.
//!
//! Nine crates built the pieces. This one closes the loop between two ends that
//! were already tested and had nothing between them —
//! `ToolRuntime::transcript_item` and `ToolLogSink` — and gives the result a
//! socket, a store and a shutdown.
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
//! # What it does
//!
//! * Opens **one session per `Hub`** over a content-addressed stable prefix, on the
//!   `/completion` token-array fallback (§5.6). No control channel: S0 is
//!   deliberately off the critical path.
//! * Runs §5.1's loop — render, tokenize, append, submit, stream, parse, execute,
//!   append, resubmit — with the append-only token ledger underneath it.
//! * Serves heads over a Unix socket, with the full §13.2 protocol (snapshot,
//!   resync, fan-out, scrub) because `letibot-sessionlog` already is that.
//! * Persists the transcript, the ledger rows and the token blobs to SQLite, where
//!   the append-only property is a database trigger rather than a convention.
//! * Acts on `finish_reason: length` (§5.7) and runs §18.1-I1's post-flight prefix
//!   assertion, gated on `BackendCaps::may_assert_structural_prefix()`.
//!
//! # What it does not do, and says so at startup
//!
//! Every one of these is printed by `Config::disclosures()` when the daemon comes
//! up, because a capability that is off and silent is indistinguishable from one
//! that is on and broken:
//!
//! * **Spill is off unless configured.** `NoBudget` is correct per D6 — an unset
//!   budget is a genuine no-op — so nothing spills until `--spill-inline` is given.
//! * **Retrieval is inert.** `ask_code` and `ask_corpus` abstain and nothing is
//!   behind them (T16.6, checked). No stub, because a stub that answered is
//!   precisely the failure §8.2 exists to prevent.
//! * **No adjudication.** Read-only tools never prompt (clause 4), so there is no
//!   boundary and no human in the loop. `Gate` is a parameter, not a constant, so
//!   W11 has a seam to absorb rather than a hard-coded `NoBoundary`.
//! * **No compaction, no fork, no subagents.** M2 and later.
//! * **No resume from the store.** The transcript, the ledger rows and the token
//!   blobs are all persisted and `Store::load_transcript` + `TokenLedger::restore`
//!   would rebuild them — what is missing is a constructor for
//!   `letibot_turn::Session` from a restored ledger, and the alternative (replaying
//!   the items through `append_items`) **re-renders the assistant rows**, which the
//!   engine deliberately never does: those rows were cut from the ids the server
//!   streamed, and re-rendering them reintroduces exactly the renderer
//!   non-determinism the hash chain exists to catch, during recovery, when nobody
//!   is looking. So a stored session is listed and refused with that reason, not
//!   resumed approximately. See [`sessions`].
//!
//! # What used to be here and is not
//!
//! **"One session per process"** was a disclosure in this list. It no longer is:
//! [`sessions::Sessions`] holds a `Harness` per `Hub` and
//! `letibot_sessionlog::registry` holds a `Hub` per session, addressable over the
//! socket. The per-session guarantees the old note was protecting are unchanged and
//! are now structural rather than incidental — see [`sessions`]'s header for which
//! of them is per session and why it cannot be otherwise.
//!
//! # One known gap it works around rather than hides
//!
//! * **T13.1** — `TranscriptAppended` carries no content and `EventSink` has no
//!   channel for one, so a head cannot rebuild a conversation from the log alone.
//!   `Hub::record_item` is the out-of-band path and [`harness`] wires it, reading
//!   the item id off the event rather than recomputing it. The event is not
//!   widened here: that is a contract change and not this strand's call.
//!
//! **T13.5 was the second of these and is now closed.** `DeltaTarget` had no channel for
//! tool-call argument text, so the arguments streamed to a head as `Text` while
//! the committed row put them in a `ToolCall` — a head showed the raw
//! `<function=…>` block scrolling past and then a tidy row. The note here used to
//! say it was "unfixed and visible", and that it would not be papered over by
//! filtering the deltas, which would make the live view and the stored view
//! disagree in the other direction. Both halves stand: it was not fixed by
//! filtering, it was fixed by giving the third channel a name
//! (`DeltaTarget::ToolCall`, `PROTOCOL_VERSION` 3), decided in the engine by
//! vocabulary id where the boundary still exists. The live view and the stored
//! view carry the same bytes, on channels that agree.

pub mod activity;
pub mod answers;
pub mod attachments;
pub mod backfill;
pub mod bangrun;
pub mod calibrate;
pub mod child_todos;
pub mod citations;
pub mod cli;
pub mod config;
pub mod context_claim;
pub mod corpus;
pub mod daemon;
pub mod decision_source;
pub mod delivery;
pub mod dialect;
pub mod etalon;
pub mod etalon_map;
pub mod etalon_oracle;
pub mod facts;
pub mod harness;
pub mod httphead;
pub mod instruments;
pub mod jobwatch;
pub mod leases;
// **The seat's config layer, re-exported rather than re-implemented.** It used to be a
// module of this crate; it is a crate of its own now (`crates/leticode-config`) because
// which file a leticode reads is a leticode decision and not a runtime one. This binding
// is what keeps every `crate::leticode_config::…` path below unchanged — the move was a
// move, not a rename — while the two names it answers to live in one place outside the
// daemon.
pub use leticode_config;
pub mod m1;
pub mod memwatch;
pub mod mergequeue;
pub mod modes;
pub mod nag_pause;
pub mod notes_offer;
pub mod notes_scope;
pub mod operator_ask;
pub mod oracle;
pub mod plugins;
pub mod pr_watch;
pub mod progress;
pub mod prompt;
pub mod restart;
pub mod sessions;
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
pub mod wall;
// **One process per session's transcript** — the claim a second daemon's open is refused by
// (`letibot_tokencore::store::writers`), and the sentence it is refused with.
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

/// **The vocabulary GGUF, for a test — or `None` with the skip announced.**
///
/// `letibot_tokencore::apparatus::present_gguf` asks whether the FILE is there. A harnessd
/// built without `local` (no llama.cpp on the box — `--no-default-features`) cannot load it
/// even when it is, so the file's presence is not the apparatus: llama.cpp is part of it. This
/// asks both, and under `LETIBOT_REQUIRE_APPARATUS` a missing half is a failure, as always.
pub fn vocab_gguf() -> Option<std::path::PathBuf> {
    if !cfg!(feature = "local") {
        letibot_tokencore::apparatus::absent(
            "llama.cpp — this harnessd was built without the `local` feature, so a vocabulary \
             GGUF cannot be loaded",
        );
        return None;
    }
    letibot_tokencore::apparatus::present_gguf()
}

/// [`vocab_gguf`] for a named file: the file must be there AND this build must be able to load
/// it. For a test that pins its own GGUF rather than the box's default one.
pub fn vocab_file(path: &std::path::Path) -> Option<()> {
    if !cfg!(feature = "local") {
        letibot_tokencore::apparatus::absent(
            "llama.cpp — this harnessd was built without the `local` feature, so a vocabulary \
             GGUF cannot be loaded",
        );
        return None;
    }
    letibot_tokencore::apparatus::present(
        &format!("a vocabulary GGUF ({})", path.display()),
        path.is_file(),
    )
}
