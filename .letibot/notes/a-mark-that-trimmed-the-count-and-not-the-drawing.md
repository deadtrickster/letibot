<!-- abstract: A byte offset that trimmed the reasoning count and not the renderer drew a landed thought twice; the pane's buffer is now the unlanded text only, and the count and the wrap read one width. -->

# A mark that trimmed the count and not the drawing - and the count's width

With the fold open at `thinking=open`, one landed thought drew twice: a `Thought` row and a `Thinking…` pane, same body. `TurnPane::reasoned_upto` was a byte offset into the round's reasoning and only the count honoured it; the renderer drew the whole buffer. A mark that trims a number and not the thing the number is about is worse than no mark, and the disagreement is invisible while the fold is shut.

`TurnPane::reasoning` is now the thinking no row holds, and both the count and the fold read that string. `reasoned_rows` indexes `appended`, and `hand_over_reasoning` (`crates/tui/src/app/turn.rs`) is the one door both a landing row and a snapshot take.

The walk is a prefix, not an equality, because one round can produce `[Reasoning, Assistant, Reasoning]`. A mismatch stops the walk instead of clearing, so the failure is a duplicate and never a hole. `IncrementalMarkdown` never re-lexes a settled block, so the remainder is re-seated rather than sliced.

`reasoning_body_width` is now one definition used by the wrap and by the count. A thought written as two paragraphs counts its blank separator, so a test comparing the number with the `┃` rows needs text with no blank lines in it.
