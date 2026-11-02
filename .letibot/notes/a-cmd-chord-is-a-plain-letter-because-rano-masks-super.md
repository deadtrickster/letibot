<!-- abstract: A Cmd chord used to reach the head as its plain letter because rano masked the super bit; the bit is kept now and letibot binds the four chords it acts on, while rano's own apps and a `!term` pane still see a typed letter. -->

# A Cmd chord reached letibot as its plain letter — fixed in rano, and the three edges left

Ghostty forwards ⌘C as `CSI 99;9u` (modifier 9 is 1 + 8, and bit 8 is super) because the head asks for
the kitty keyboard. Two masks threw that away: `Mods::from_bits(b & 7)` made `Mods::from_bits(8)` equal
`Mods::NONE`, and `kitty_key` re-encoded the character as its legacy bytes, which carry alt and ctrl
only - so the chord arrived as a plain `c`.

Fixed in rano (`Mods::SUPER`, `from_bits` is `b & 15`) and used in
`crates/tui/src/app/keys/input.rs`: ⌘C is deliberately `None` (the terminal and the head's own
drag-release already copy the selection), ⌘V is `Key::CtrlV`, ⌘Z is `Undo`, ⌘⇧Z is `Redo`, and every
other ⌘ chord falls through to `None`. Pinned by
`a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter`.

Three edges are open. rano's own apps type the letter, because `Key::from_event` has no super field.
A `!term` pane's program still gets a typed `c`, because `legacy_bytes` is unchanged - a decision, not a
line. And ⌘Enter used to submit and is now nothing, a behaviour change somebody can feel.
