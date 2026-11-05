//! `/prune`: structural eviction on its own, §4's first rung with no summary above it. The oldest
//! tool-call bodies leave the conversation as stubs, every row is carried, and each body is kept in
//! the spill store under the id `recall` opens. No model anywhere: a re-prefill is its entire cost
//! (`docs/compaction.md` §6).

use letibot_transcript::{SystemOrigin, TranscriptItem};
use letibot_turn::CompactionOutcome;

use super::wall::Spilled;
use super::{ForkReport, ForkTail, Harness, HarnessError};

/// What a prune did: the fork, the units whose bodies left, and what the re-seated prompt gained
/// and lost. The units are the map, so the reply and the base cannot disagree about an id.
#[derive(Debug, Clone)]
pub struct PruneReport {
    pub fork: ForkReport,
    pub units: Vec<letibot_turn::evict::Unit>,
    pub gained: Vec<String>,
    pub lost: Vec<String>,
}

impl Harness {
    /// Prune the conversation: reduce it with the same evicter a summary turn uses, then fork onto
    /// the reduced rows - a fork, not an edit, so the ledger's hash chain is never broken and the
    /// old transcript stays whole in the store. Refuses when nothing would leave, because the fork
    /// costs a cold re-prefill and a prune that changed nothing is that cost for nothing.
    pub fn prune(&mut self) -> Result<PruneReport, HarnessError> {
        if self.store.is_none() {
            return Err(HarnessError::Setup(
                "pruning forks the conversation onto a copy whose oldest bodies are stubs, and \
                 a fork needs a store to write it to; this session has none. Nothing was \
                 changed."
                    .into(),
            ));
        }
        let items: Vec<TranscriptItem> = self.session.items.clone();
        let rows = self.plan_rows();
        // Where the bodies go: the session's own spill store, the one `recall` reads - the same
        // door a compaction's eviction uses, cloned out because the fork needs `self` back.
        let runtime = self.runtime.clone();
        let reduced = letibot_turn::evict::reduce(&items, &rows, &Spilled(&*runtime.spiller.store));
        if reduced.units.is_empty() {
            return Err(HarnessError::Setup(format!(
                "nothing to prune: no finished call's body of {} bytes or more that a later \
                 call does not supersede, no answered subagent, and nothing the plan still \
                 names. The conversation is as it was.",
                letibot_turn::evict::BODY_FLOOR
            )));
        }
        let gone: usize = reduced.units.iter().map(|u| u.gone).sum();
        let n = reduced.units.len();
        // The store holds the bodies, the base carries their map, and `recall` answers from the
        // same ids - one call, so no id the base prints can be one the tool refuses.
        self.note_evicted(&reduced.units);
        let map = reduced.map();
        let lead = TranscriptItem::System {
            text: format!(
                "[pruned] {n} tool-call bodies ({gone} bytes) left this conversation as stubs; \
                 nothing was summarised, and every call, argument and outcome is where it was. \
                 Each body is in the store under the id its line below names, and `recall \
                 id=<id>` returns it exactly. The old transcript is still whole in the store \
                 (`transcript` tool).\n\n{map}"
            ),
            origin: SystemOrigin::Update,
        };
        // No summary turn ran, so the outcome is the empty one and the fork's own note is the
        // lead - the same record `/clear` writes, for the same reason.
        let outcome = CompactionOutcome {
            turn_id: format!("{}#prune", self.transcript_id),
            summary: String::new(),
            tool_calls: 0,
            truncated: false,
            cached_tokens: 0,
            reusable: 0,
            generated_tokens: 0,
            budget: None,
            fidelity: None,
        };
        // Already paying a fresh transcript's render, so it carries the seated prompt like every
        // other fork (`reseat_target`'s own rule).
        let reseat = self.reseat_target()?;
        let before = self.measure_resident().tokens;
        self.compacting = true;
        let fork = self.fork_onto(
            &outcome,
            reseat.as_ref().map(|(p, _)| p),
            reseat.as_ref().map(|(_, id)| id.as_str()),
            ForkTail {
                items: &reduced.items,
                split: None,
                because: "",
            },
            Some(lead),
        );
        self.compacting = false;
        let fork = fork?;
        let (mut gained, mut lost) = self.adopt_reseat(reseat);
        gained.sort();
        lost.sort();
        // A prune reduces the base, so the controller hears it as the fold it is: a session the
        // ladder stood down is re-armed by the room this made, not by a summary it never ran.
        let base = self.measure_resident();
        if self.cfg.wall.folded(
            crate::wall::Fold::Ordinary,
            &self.transcript_id,
            before,
            base.tokens,
        ) {
            self.persist_stuck();
        }
        if self.provider.is_none() {
            self.observe_wall(base.tokens);
        }
        self.publish_settings();
        Ok(PruneReport {
            fork,
            units: reduced.units,
            gained,
            lost,
        })
    }
}

/// The verb's own answer: the numbers an operator can check against the context line, and where
/// the bodies went. Long enough to name the resource, short enough to sit in one row.
pub fn prune_reply(r: &PruneReport) -> String {
    let gone: usize = r.units.iter().map(|u| u.gone).sum();
    format!(
        "pruned: {} unit(s), {} bytes of body no longer carried; each is in the spill store \
         under the id the new base names and `recall` opens it. {} → {} tokens on {}; nothing \
         was summarised and nothing destroyed - the old transcript is {}.",
        r.units.len(),
        gone,
        r.fork.was_tokens,
        r.fork.base_tokens,
        r.fork.transcript_id,
        r.fork.parent_id
    )
}
