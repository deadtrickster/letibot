<!-- abstract: MEASURED 2026-10-27, FIXED 2026-10-29 (rano b2bd7d22 + letibot d5141919): rano now keeps super (`Mods(b & 15)`, `Mods::SUPER`) and reports a `CSI … u` carrying an unspellable modifier as itself; letibot binds ⌘C/⌘V/⌘Z/⌘⇧Z. What is still not fixed: rano's own `Key::from_event` drops super, and `legacy_bytes` still hands a `!term` pane a typed `c`. -->

# A Cmd chord reached letibot as its plain letter — fixed in rano, and the three edges left

MEASURED 2026-10-27 (branch `agent/tui-status-and-copy`) from the operator's report
*"Cmd-C with a mouse selection copies AND types a `c`"*; **FIXED 2026-10-29** on branch
`agent/cmd-chords-reach-the-app` — rano `fix/super-modifier` **b2bd7d2280971f762e3ccc5452ec3ea9829f2511**
(pushed, so the rev exists for a git dependency), letibot commit **d5141919**.

## What was wrong, in two places

Ghostty forwards ⌘C to the application as `CSI 99;9u` — modifier `9` is `1 + 8`, bit 8 is
super — because the head asks for the kitty keyboard (`CSI > 1 u`). Then:

* `Mods::from_bits(b & 7)` masked the bit off, so `Mods::from_bits(8)` **was** `Mods::NONE`;
* `kitty_key` re-encoded the character as its LEGACY bytes, and `kitty_legacy` re-emits alt and
  ctrl only — so `CSI 99;9u` came out as a plain `c`, indistinguishable from a typed letter.

There was no letibot-side fix: `Terminal::raw_input` runs the same bytes through `legacy_bytes`.

## What the fix is, now that it is in

`Mods` keeps bit 8 (`Mods::SUPER`, plus `sup()`); `from_bits` is `b & 15`. `kitty_key` has one
arm before the round trip: `bits & (8|16|32) != 0` → `key(KeyCode::Char(ch), mods)`, and `None`
for a control code point, a private-use code, or `mods.is_empty()` (hyper/meta have no bit, and a
letter whose only modifier cannot be carried must not become an unmodified letter). The lock bits
(64 caps, 128 num) are **not** in that mask on purpose: a caps-locked ⌘C is still ⌘C.

letibot's `key()` binds ⌘C → nothing, ⌘V → `Key::CtrlV`, ⌘Z → `Undo`, ⌘⇧Z → `Redo`, and every
other ⌘ chord falls through to `KeyCode::Char(_) => return None`.

## The three edges this does NOT close

* **rano's own apps still type the letter.** `Key::from_event` (`src/keymap.rs`) has
  `ctrl/alt/shift` fields and no super, so a super-carrying `Char` becomes a plain printable one
  — rano's editor/review/host would insert `c` on ⌘C. letibot does not use `Key::from_event`.
* **A `!term` pane's program still gets a typed `c`.** `legacy_bytes` is unchanged, so
  `CSI 99;9u` is rewritten to the byte `c` for a pane (`head::read_routed` hands `raw` to it).
  Fixing that is a `legacy_bytes`/`kitty_legacy` change and would break
  `a_pane_gets_legacy_bytes_under_the_kitty_keyboard`'s premise — a decision, not a line.
* **⌘Enter used to submit.** It decoded to a plain Enter (the bit was masked, then `\r`), and is
  now nothing at all — the same rule that fixed ⌘C, but it is a behaviour change somebody can
  feel.

## Rakes that still bite

* **A note or a test that writes `Mods::from_bits(8)` is measuring nothing until the pin moves.**
  It was `Mods::NONE` for the whole of 2026-10-27/28, so any test claiming to press ⌘C at this
  level asserted a plain `c` — including, on purpose, the old
  `a_cmd_chord_arrives_as_its_plain_character_because_rano_drops_the_super_bit`, whose first
  assertion was the instruction to invert it. It is now
  `a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter`.
* **Do not "fix" this by swallowing typing while a selection is held** — a drag's release copies
  and then holds the frame (`selection_held`), and the operator types under it.
  `app/tests/editor.rs`'s `a_selection_does_not_swallow_what_is_typed` is there to fail if
  somebody tries.
* The copy the operator saw was never ours: Ghostty copies the selection, and so does this head's
  drag-release (`App::copy_selection`). Silence on ⌘C is the whole of the head's job.
* **⌘V shares `Key::CtrlV`, so with a payload window open it CLOSES the window** instead of
  attaching — `keys/chords.rs`'s rule (*the clipboard first, unless a window is open*). And in a
  terminal that intercepts ⌘V itself (Ghostty's default `super+v=paste_from_clipboard`) the head
  never sees the chord at all: it gets an EMPTY bracketed paste, which reaches the same door
  (`App::paste_arrived`).
