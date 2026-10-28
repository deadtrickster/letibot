<!-- abstract: MEASURED 2026-10-27: Ghostty forwards ⌘C as `CSI 99;9u`; rano's `Mods::from_bits(b & 7)` and `kitty_key`'s legacy round-trip both drop the super bit, so letibot's key map sees a bare `c` — no letibot-side fix exists (`raw_input` is lossy too); two rano lines fix it and the head needs none. -->

# A Cmd chord reaches letibot as its plain letter, and nothing here can tell

MEASURED 2026-10-27, branch `agent/tui-status-and-copy`, from the operator's report
*"Cmd-C with a mouse selection copies AND types a `c`"* (a selection copied mid-prompt
corrupts the prompt).

## The numbers

Run through this tree's own map (`app::keys::input`), with the pinned rano `bf3fa48`:

```text
decode(b"\x1b[99;9u")       -> [Key::Char('c')]   // ⌘C
decode(b"\x1b[99;13u")      -> [Key::CtrlC]       // ⌘⌃C — an INTERRUPT, not a copy
decode(b"\x1b[99;11u")      -> []                 // ⌘⌥C (alt has its own arms)
legacy_bytes(b"\x1b[99;9u") -> [99]               // the byte 'c'
Mods::from_bits(8).bits()   -> 0                  // the super bit is already gone
```

Ghostty (the operator's terminal, `/Applications/Ghostty.app`) forwards ⌘C to the
application as `CSI 99;9u` — modifier `9` is `1 + 8`, bit 8 is super — because the head
pushes the kitty keyboard's disambiguate flag (`CSI > 1 u`, rano `term/terminal.rs`).
Two places in rano then lose it:

* `Mods::from_bits(b & 7)` (`src/term/event.rs`) keeps shift/alt/ctrl only;
* `kitty_key` (`src/term/decode.rs`) re-encodes the character as its LEGACY bytes and
  decodes those, and `kitty_legacy` re-emits alt and ctrl only.

So the head receives `Key::Char('c')` with empty mods — indistinguishable from a typed
`c`. **There is no letibot-side fix**: the one door to the bytes, `Terminal::raw_input`,
runs them through `legacy_bytes` and answers `[99]` for the same sequence.

## The fix is two rano lines, and the head needs none

Verified against a scratch copy of the pinned rev, `[patch]`ed into a worktree and then
reverted (nothing landed):

1. `event.rs`: `Mods(b & 7)` → `Mods(b)` (and a `pub const SUPER: Mods = Mods(8);`).
2. `decode.rs`, in `kitty_key`, before the `kitty_legacy` round-trip: when `bits & !7 != 0`
   and the code is a printable char, `return Some(key(KeyCode::Char(ch), Mods::from_bits(bits)))`.

With those, `CSI 99;9u` and `CSI 99;13u` both decode to NOTHING, and every existing
decode test still passes. letibot's `key()` needs no change: its last character arm
(`KeyCode::Char(_) => return None`) already swallows any modifier it does not bind.

## The rake

* **`Mods::from_bits(8)` is `Mods::NONE`**, so a SUPER key cannot even be *written* in a
  letibot test — `key_of(KeyEvent::new(Char('c'), Mods::from_bits(8)))` is a plain `c`.
  Any test claiming to press ⌘C at this level is measuring nothing.
* `crates/tui/src/app/keys/input.rs`'s
  `a_cmd_chord_arrives_as_its_plain_character_because_rano_drops_the_super_bit` asserts
  today's decode **on purpose**; when rano keeps the bit, its first assertion fails and
  that is the instruction to invert it, not a regression.
* **Do not "fix" this by swallowing typing while a selection is held** — a drag's release
  copies and then holds the frame (`selection_held`), and the operator types under it.
  `app/tests/editor.rs`'s `a_selection_does_not_swallow_what_is_typed` is there to fail if
  somebody tries.
* The copy the operator saw was never ours: Ghostty copies the selection, and so does this
  head's drag-release (`App::copy_selection`).
