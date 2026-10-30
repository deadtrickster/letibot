<!-- abstract: MEASURED 2026-10-31: `plan_cut` is `Hopeless` when the prompt leaves less than WRITE_ROOM+CUT_MARGIN (24 576) of the window, and the prompt is a fact about `$HOME` (6 709 empty / 7 669 Mac, 6 717 empty / ~8 209 lab2x1) — so every fixture window must be measured, not 32 768. -->

# A fixture's window is the box's prompt, and the prompt is the HOME's

MEASURED 2026-10-31, branch `agent/a-fixture-sized-for-one-tokenizer`
(`crates/harnessd/tests/wall_refusal.rs`), from `a_refusal_on_a_stuck_session_still_folds`
being green on the Mac (5/5) and red on lab2x1's gate.

## The threshold, exactly

`letibot_turn::compaction::plan_cut` opens with

```rust
if prefix_tokens + WRITE_ROOM + CUT_MARGIN >= window { return OverrunPlan::Hopeless { .. } }
```

`WRITE_ROOM` is 16 384 and `CUT_MARGIN` 8 192 (`CUT_MARGIN` is private — a test cannot
import it), so **a two-half fold needs the prompt to be under `window − 24 576`**: 8 192 of a
32 768 window, a quarter. `Hopeless` comes back as `HarnessError::Setup`, the compaction is
swallowed (`compaction FAILED: this session's PROMPT is N token(s) of a M token window …`),
nothing forks, and the wall error the fold was asked to answer stands — the caller sees
`Err(ContextWall { rounds: 0, resident, window })` and nothing about the fold.

## The prompt is message zero, and message zero is the HOME

`Config::compose_for_seat` puts the standing-notes section (AGENTS.md, the seat's
instruction files, `<config>/notes/*.md`, `<workspace>/.letibot/notes/`, budget 2 000 tokens)
INTO the system prompt, so the prefix is `system + tools_json` and its size is a fact about
the box's home. Same binary, same vocabulary (`qwen3.8-flash-next` shard 1 of 6, the same
file on both boxes):

| box | home | prompt (tokens) | at a 32 768 window |
|---|---|---|---|
| macOS | real (`~/.claude/CLAUDE.md`, 8 531 B → indexed, 708 tokens) | 7 669 | 7 669 + 24 576 = 32 245 ✓ |
| macOS | empty | 6 709 | ✓ |
| lab2x1 | empty (a placed session's `$HOME`) | 6 717 | ✓ |
| lab2x1 | the gate's real home | ~8 209 | 32 785 ✗ `Hopeless` |

Two things to keep: **the vocabulary is not the variable** — with an empty home the two
boxes agree to 8 tokens (the per-session scratch-dir path length); and
**`Sessions::open_first` is 217 tokens bigger than `Harness::open`** (`open_first` sets
`cfg.seat_prompt = Some(default)`), which is why a bare-harness test passed on the gate while
the `Sessions`-door test next to it failed.

## How to measure it, and how to build a window from it

```rust
// Through the DAEMON's door, or you measure 217 tokens less than the session gets.
let mut probe = cfg.clone();                  // same workspace, same seat
probe.store = Some(scratch.join("sessions.db"));
probe.session_id = "probe".into();
let registry = Registry::new();
registry.create("probe", "", Sessions::wiring(&probe)).expect("…");
let sessions = Sessions::open_first(&parts, probe, registry).expect("the probe opens");
sessions.harness_of("probe").unwrap().prefix_tokens().len() as u64
```

Two window shapes, both derived:

* **a fold must exist**: `prompt + CUT_ROOM + cfg.headroom()` (the reserve is
  `headroom_of(window) = (w/16).max(2048).min(w/4)`);
* **a wall by size**: a BAND — above `prompt + CUT_ROOM` (or the fold is `Hopeless`) and
  below `prompt + the history` (or the history never overruns it). The middle of it is
  `prompt + CUT_ROOM + (history − CUT_ROOM)/2`, where `history` is the fixture's own content
  counted with the session's vocabulary (`vocab.tokenize_text(...)`), not a guess.

## Two rakes

* **A seeded stuck pair must be the window's.** `Harness::restore_auto_compact` clears and
  refuses a pair whose `measured > window` ("a provider counts nothing past its own window"),
  so `set_auto_compact_stood_down(id, Some((43_000, 31_000)))` restores nothing once the
  window moves. Derive both numbers from the window.
* **A scripted provider count is not free.** Making a canned server report a count at the
  wall (to keep a `Stuck` session stuck across a round) sets `cfg.ledger_scale = (ledger,
  count)`; `planning_window()` converts the window by that ratio, so a count 5× the ledger
  collapses the window in ledger units and `plan_cut` goes `Hopeless`. A fixture cannot lie
  its way past `plan_cut`; it must measure.

## The reproduction recipe, on any box

A synthetic home is enough, and it is faster than waiting for a gate: create
`$HOME/.claude/CLAUDE.md` plus **six small** `$HOME/.config/letibot/notes/*.md` (~250 tokens
each — small, so each arrives VERBATIM and spends the 2 000-token budget; 96 KB of large
notes added only 575 tokens because each was indexed), then run the test binary with
`HOME=<that dir>` and `LETIBOT_VOCAB_GGUF=<absolute path>` (the default GGUF is found under
`$HOME`). On the Mac that took the prompt 6 709 → 9 283 and turned the four size-shaped
tests in `wall_refusal.rs` red at `aa6c1b8d`.
