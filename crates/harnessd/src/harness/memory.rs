//! What the model is told about memory where it chooses a call's cap.
//!
//! `bash`'s `memory` argument describes the policy and is rendered once, at open, because a schema
//! is the session's stable prefix. What is free moves every second, so it rides the round's tail.

use letibot_tools::exec::memory::{Holds, Meminfo, MemoryPolicy};
use letibot_transcript::{Speaker, TranscriptItem, UserPart};

/// The round's memory line, as the model reads it. One item, because a line the harness says is
/// one thing and the renderer builds a turn out of it.
pub(super) fn memory_tail(policy: &MemoryPolicy, mem: &Meminfo, holds: Holds) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Speaker::Agent,
        parts: vec![UserPart::Text {
            text: format!(
                "[memory - the harness's own line, not the operator's] {} This line is composed \
                 for this round and is gone from the next one.",
                mem.round_clause(policy, holds)
            ),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn mem(available_gib: u64) -> Meminfo {
        Meminfo {
            total: 31 * GIB,
            available: available_gib * GIB,
            swap_free: 8 * GIB,
        }
    }

    fn text(item: &TranscriptItem) -> String {
        match item {
            TranscriptItem::User { speaker, parts } => {
                assert_eq!(*speaker, Speaker::Agent, "never drawn as the operator's");
                match &parts[..] {
                    [UserPart::Text { text }] => text.clone(),
                    other => panic!("one text part, got {other:?}"),
                }
            }
            other => panic!("a user item, got {other:?}"),
        }
    }

    /// The numbers the model needs are in the line it reads, and it says the line is the
    /// harness's rather than the operator's and that it will not be there next round.
    #[test]
    fn the_round_carries_what_is_free_where_the_cap_is_chosen() {
        let policy = MemoryPolicy::default();
        let holds = Holds {
            scopes: 3,
            promised: 2 * GIB,
            peaks: 300 << 20,
        };
        let line = text(&memory_tail(&policy, &mem(12), holds));
        assert!(
            line.starts_with("[memory - the harness's own line"),
            "{line}"
        );
        assert!(line.contains("12 GiB of 31 GiB is free"), "{line}");
        assert!(line.contains("is free (swap 8 GiB)"), "{line}");
        assert!(line.contains("an ask up to 6 GiB is granted"), "{line}");
        assert!(line.contains("hold 300 MiB of 2 GiB promised"), "{line}");
        assert!(line.contains("gone from the next one"), "{line}");
    }

    /// A box with no scope tree - macOS, or a daemon that could not delegate - says the box's own
    /// numbers and no holds, rather than claiming nothing is running.
    #[test]
    fn a_box_with_no_scopes_says_only_the_boxs_numbers() {
        let line = text(&memory_tail(
            &MemoryPolicy::default(),
            &mem(12),
            Holds::default(),
        ));
        assert!(!line.contains("scope(s)"), "{line}");
        assert!(line.contains("12 GiB of 31 GiB is free"), "{line}");
    }
}
