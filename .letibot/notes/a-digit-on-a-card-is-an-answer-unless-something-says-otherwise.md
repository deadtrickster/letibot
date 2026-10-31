<!-- abstract: MEASURED 2026-10-31 (R90): the quit card's `2` stopped the daemon and every running job, and the reseat card's `1` forked the conversation, on the FIRST CHARACTER of a line being typed — while the bar under the quit card said "1/2 or ↑↓ then enter". The empty-composer guard cannot help: it reads the composer before the key. -->

# A digit on a card is an answer, and the empty-composer guard cannot cover the first character

MEASURED 2026-10-31, branch `agent/the-keys-and-the-prompt-reviews`, auditing R90
(`TODO.md:376`) against the tree: R90's own fixes (commits `fee16209`, `d70d70f5`,
…) had left two of the six cards answering on the digit while the decision card
marks with it (`R90.2`: "a row number MARKS the row; Enter answers it").

## What was actually wrong

* `key_quit_card`'s digit arm answered: `2` left the head **or stopped the daemon,
  with every job and subagent it had running**.
* `key_reseat_card`'s digit arm chose: `1` sent `/reseat` and forked the
  conversation.

Both fire on the **first character of a line somebody is typing** — "2 minutes",
"1. check the logs" — because `self.editor.text().is_empty()` is true for exactly
that one keystroke. **R90.30's empty-composer guard is not a defence**: it asks the
composer *before* the key is applied, so the keystroke that turns an empty composer
into a line is the keystroke the guard waves through. Any future guard of the shape
"only when nothing is typed" has this hole for the first character.

And the help already disagreed: the quit card's bar is rano's `HintMode::QuitCard`
tail, `"1/2 or ↑↓ then enter · esc stays"` (`rano src/agent/hint_bar.rs:71`) — the
digit was never advertised as the answer.

## What the rule is now

A digit **marks** on every numbered card, and the answer is the deliberate Enter
(the one the guard does cover, since by then the composer has words in it). The
reseat card's hint names the digits it honours.

## The rake for the next card

**A drawn row number is a promise about the digit.** If a card numbers its rows and
the digit does not move the mark, either the numbers are decoration (say so) or the
key is wrong. Grep `digit_row(` for the cards that take one, and ask of each
whether one keystroke there can do something the operator cannot take back.

`crates/tui/src/app/tests/cardkeys.rs::a_digit_on_the_quit_card_marks_the_row_and_stops_nothing`
and `reseatcard.rs::a_digit_on_the_reseat_card_marks_the_row_and_reseats_nothing`
pin it; two older tests (`settings.rs`, `screen.rs`) had pinned the destructive
behaviour as if it were the design.
