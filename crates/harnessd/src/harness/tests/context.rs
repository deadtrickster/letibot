//! The context wall: the provider's count, compaction, and the backstop.

use super::*;

/// A ledger and its items, built row by row with tokens of a given count — the
/// shape `Session::append_items` keeps (one row per item), with no vocabulary.
struct Wall {
    ledger: letibot_tokencore::ledger::TokenLedger,
    items: Vec<TranscriptItem>,
}

impl Wall {
    fn new(prefix: usize) -> Wall {
        Wall {
            ledger: letibot_tokencore::ledger::TokenLedger::new("t-wall", &vec![1; prefix])
                .expect("a ledger"),
            items: Vec::new(),
        }
    }
    fn push(&mut self, item: TranscriptItem, tokens: usize) -> &mut Wall {
        let id = format!("t-wall.{}", self.items.len());
        self.ledger
            .append(&id, &vec![2; tokens])
            .expect("an append");
        self.items.push(item);
        self
    }
    fn sent(&mut self, tokens: usize) -> &mut Wall {
        self.push(
            TranscriptItem::System {
                text: "sent".into(),
                origin: SystemOrigin::Update,
            },
            tokens,
        )
    }
    fn thought(&mut self, tokens: usize) -> &mut Wall {
        self.push(
            TranscriptItem::Reasoning {
                text: "thinking".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            },
            tokens,
        )
    }
    fn len(&self) -> u64 {
        self.ledger.len() as u64
    }
    fn anchor(&self, provider: u64) -> super::WallAnchor {
        super::WallAnchor {
            transcript: "t-wall".into(),
            ledger: self.len(),
            provider,
            measured: true,
        }
    }
}

fn deepseek_window() -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.context_window = Some(1_000_000);
    cfg.auto_compact = true;
    cfg
}

/// **R72: a reasoning-heavy history is judged at the window's REAL edge.**
///
/// The provider counted 880k at its last round, over a ledger ~3.4× that (the
/// reasoning it is not sent). Then a round fails inside its own reasoning — R7's
/// *"the turn spent its whole output on reasoning"* — which commits 200k ledger
/// tokens of thought and measures nothing, and a 12k tool result lands. The
/// provider will be sent 892k of a 1M window whose wall is at 937.5k: room.
///
/// The ratio reading — `ledger ÷ (ledger, provider)` as learned at the round —
/// counted the 200k of unsent thought at nearly a third of its size and the
/// session past the wall (941.7k against 937.5k), compacting a conversation with
/// 108k of its window free.
#[test]
fn a_reasoning_heavy_history_meets_the_wall_at_the_providers_count() {
    let mut w = Wall::new(20_000);
    // ~3.4 ledger tokens per provider token: 880k sent, ~2.1M of thought.
    for _ in 0..100 {
        w.sent(8_600).thought(21_400);
    }
    let anchor = w.anchor(880_000);
    let at_round = w.len();
    w.thought(200_000).sent(12_000);

    let (_, now) = super::roll_resident(&anchor, "t-wall", &w.ledger, &w.items, true);
    assert_eq!(
        now.tokens, 892_000,
        "the provider's 880k + the 12k it will be sent"
    );
    assert_eq!(now.measured, Some(880_000));
    assert_eq!(now.since, 12_000);

    let mut cfg = deepseek_window();
    cfg.ledger_scale = Some((w.len(), now.tokens));
    assert!(
        !cfg.should_compact(w.len()),
        "892k of 1M is not the wall (937.5k); compacting here is 27% early"
    );
    assert_eq!(
        cfg.shown_tokens(w.len()),
        892_000,
        "the line prints the same count"
    );
    // The old reading, for the record: the round's ratio applied to today's ledger.
    let old = (w.len() as u128 * 880_000 / at_round as u128) as u64;
    assert!(
        old > 937_500,
        "the ratio reading put it past the wall: {old}"
    );

    // And at the real edge it fires: a 50k tool result takes it to 942k.
    w.sent(50_000);
    let (_, now) = super::roll_resident(&anchor, "t-wall", &w.ledger, &w.items, true);
    assert_eq!(now.tokens, 942_000);
    cfg.ledger_scale = Some((w.len(), now.tokens));
    assert!(
        cfg.should_compact(w.len()),
        "942k + 62.5k reserve is past 1M"
    );
}

/// **R72: a restart whose last append was a notice comes back at the provider's
/// count, and does not compact.**
///
/// MEASURED 2026-10-11, leticl's `s-1791151788256943467` after a daemon restart:
/// `compacting: 3454266 of 1000000 tokens resident` → `compacted: 75408` — a
/// session the provider counted at ~740k, because the stored pair's ledger no
/// longer EQUALLED the rebuilt one (something was appended after the last round),
/// so the window went unscaled and the raw ledger was compared with it.
#[test]
fn a_restart_after_a_notice_recovers_the_providers_count_and_does_not_compact() {
    let mut w = Wall::new(20_000);
    for _ in 0..100 {
        w.sent(7_200).thought(26_900);
    }
    // The last round: its prompt was this long, and the provider counted 740k.
    let (stored_ledger, stored_provider) = (w.len(), 740_000u64);
    // Then the round's reply (thought and a short answer) and a notice.
    w.thought(40_000).sent(1_500).sent(600);

    let anchor =
        super::recovered_scale(Some(stored_provider), Some(stored_ledger), w.len() as usize)
            .expect("a later append does not throw the count away");
    let anchor = super::WallAnchor {
        transcript: "t-wall".into(),
        ledger: anchor.0,
        provider: anchor.1,
        measured: true,
    };
    let (_, now) = super::roll_resident(&anchor, "t-wall", &w.ledger, &w.items, true);
    assert_eq!(now.tokens, 742_100, "740k + the answer and the notice");

    let mut cfg = deepseek_window();
    cfg.provider = Some(crate::config::ProviderConfig {
        name: "deepseek".into(),
        ..Default::default()
    });
    cfg.ledger_scale = Some((w.len(), now.tokens));
    assert!(
        w.len() > 3_400_000,
        "the ledger is the 3.45M the log printed"
    );
    assert!(
        !cfg.should_compact(w.len()),
        "742k of 1M must not compact on the first idle pass after a restart"
    );
    assert!(!cfg.tokens_are_unscaled());
}

/// **A fork re-bases the count on the new base**, rather than applying the
/// history's ratio to a summary whose reasoning share is nothing like it; and a
/// local session counts every row, because its prompt IS the ledger.
#[test]
fn a_fork_rebases_the_count_and_a_local_session_counts_the_ledger() {
    let mut w = Wall::new(5_000);
    w.sent(1_000).thought(4_000).sent(2_000);
    let stale = super::WallAnchor {
        transcript: "t-before-the-fork".into(),
        ledger: 3_000_000,
        provider: 900_000,
        measured: true,
    };
    let (rebased, now) = super::roll_resident(&stale, "t-wall", &w.ledger, &w.items, true);
    assert_eq!(
        now.tokens, 8_000,
        "prefix + what is sent, the thought left out"
    );
    assert_eq!(now.measured, None, "nothing measured the new base yet");
    assert_eq!(rebased.ledger, w.len());
    assert!(!rebased.measured);
    // The next roll adds only what came after the re-base.
    w.sent(500);
    let (_, next) = super::roll_resident(&rebased, "t-wall", &w.ledger, &w.items, true);
    assert_eq!(next.tokens, 8_500);

    // Local: the ledger is the prompt, reasoning and all.
    let anchor = super::WallAnchor {
        transcript: "t-wall".into(),
        ledger: u64::from(w.ledger.prefix_len()),
        provider: 5_000,
        measured: true,
    };
    let (_, local) = super::roll_resident(&anchor, "t-wall", &w.ledger, &w.items, false);
    assert_eq!(local.tokens, w.len(), "ratio 1 by construction");
}

/// The closing line names its unit and says when the after-count is an estimate.
#[test]
fn the_compacted_line_is_one_unit_said_once() {
    let base = super::Resident {
        tokens: 75_408,
        measured: None,
        since: 0,
    };
    let said = super::compacted_line(937_686, base, "deepseek tokens", true);
    assert!(
        said.starts_with("compacted: 937686 → 75408 deepseek tokens resident"),
        "{said}"
    );
    assert!(said.contains("the next round measures it"), "{said}");
    assert_eq!(
        super::compacted_line(240_000, base, "tokens", false),
        "compacted: 240000 → 75408 tokens resident."
    );
}

/// **The wall's turn failure is the provider's count with its unit** (R72 follow-up,
/// defect A). MEASURED on a scratch deepseek session at a 40,000-token window: the
/// head drew *"── FAILED — stopped after 1 round(s) at the context wall: 56466 of
/// 41843 tokens resident"* while the log for the same wall said *"53978 of 40000
/// deepseek tokens (56466 of 41843 ledger tokens)"* — the ledger's pair, no unit,
/// and a window nobody configured.
#[test]
fn the_wall_failure_says_the_providers_count_and_names_its_unit() {
    let said = super::HarnessError::ContextWall {
        rounds: 1,
        resident: 53_978,
        window: 40_000,
        unit: "deepseek tokens".into(),
        ledger: (56_466, 41_843),
    }
    .to_string();
    assert!(
        said.starts_with(
            "stopped after 1 round(s) at the context wall: 53978 of 40000 deepseek \
                 tokens resident (56466 of 41843 ledger tokens) and the next round"
        ),
        "{said}"
    );
    // A local session: the ledger IS the count, so it is said once.
    let local = super::HarnessError::ContextWall {
        rounds: 2,
        resident: 31_000,
        window: 32_768,
        unit: "tokens".into(),
        ledger: (31_000, 32_768),
    }
    .to_string();
    assert!(
        local.starts_with(
            "stopped after 2 round(s) at the context wall: 31000 of 32768 tokens \
                 resident and the next round"
        ),
        "{local}"
    );
}

/// Round 0 stops only past the window; later rounds inside the reserve (defect D).
#[test]
fn round_zero_stops_past_the_window_and_later_rounds_inside_the_reserve() {
    assert!(!super::wall_stops_round(0, 39_000, 5_000, 2_500, 40_000));
    assert!(super::wall_stops_round(0, 40_001, 5_000, 2_500, 40_000));
    assert!(super::wall_stops_round(1, 37_500, 5_000, 2_500, 40_000));
    assert!(!super::wall_stops_round(1, 37_499, 5_000, 2_500, 40_000));
    // Message zero alone past the window: nothing a compaction can fix, so round 0
    // is left to the server rather than stopped into a compaction known to fail.
    assert!(!super::wall_stops_round(0, 5_600, 5_443, 128, 512));
}

/// **Infinity is `0`, and it is one line, so it gets one test.**
///
/// The operator: *"make 200 tool calls limit configurable and set it to
/// infinity"*. `0` reaching the loop as `0` would mean a turn that may take no
/// rounds at all — the exact opposite — and it is the kind of inversion that
/// looks right in a diff.
#[test]
fn a_zero_backstop_is_unbounded_and_every_other_number_is_itself() {
    assert_eq!(super::round_backstop(0), usize::MAX, "0 means no bound");
    assert_eq!(super::round_backstop(1), 1);
    assert_eq!(super::round_backstop(200), 200);
    // And the default is the unbounded one, which is the half of this the
    // operator actually asked for.
    assert_eq!(
        super::round_backstop(Config::for_this_box("/tmp").max_tool_rounds),
        usize::MAX
    );
}

/// **`next` is the offset to ask at, not a count, and it stops at the end.**
///
/// A pane pages by handing `next` straight back as the next request's offset,
/// so it has to be the offset the window ended at — and it has to be `None`
/// exactly when the ring holds nothing further, or the pane offers a page that
/// comes back empty and reads as the job's end.
#[test]
fn a_job_window_names_the_next_offset_until_the_ring_runs_out() {
    let slice = |from, to, produced, dropped, retained| letibot_tools::exec::OutputSlice {
        bytes: Vec::new(),
        from,
        to,
        produced,
        dropped,
        retained,
    };
    // A window in the middle of a long job: there is more, and `next` is where
    // this one ended.
    assert_eq!(next_job_offset(&slice(0, 16, 40, 0, 40)), Some(16));
    // The window has taken everything still held: no further page.
    assert_eq!(next_job_offset(&slice(0, 40, 40, 0, 40)), None);
    // A ring that dropped its front: the end of what is *held* is the end, even
    // though `produced` is past `to` — that part is gone, not waiting.
    assert_eq!(next_job_offset(&slice(992, 1000, 1000, 992, 8)), None);
    // And a window that stopped short of the retained tail still has a page.
    assert_eq!(next_job_offset(&slice(992, 996, 1000, 992, 8)), Some(996));
}
