<!-- abstract: A digit on a card used to answer, so the first character of a line somebody was typing could stop the daemon; a digit now marks and the answer is Enter, and the empty-composer guard cannot cover that first keystroke. -->

# A digit on a card is an answer, and the empty-composer guard cannot cover the first character

The quit card's digit arm answered: `2` left the head or stopped the daemon with every job and subagent
it had running. The reseat card's `1` sent `/reseat` and forked the conversation. Both fired on the
first character of a line somebody was typing ("2 minutes", "1. check the logs"), because
`self.editor.text().is_empty()` is true for exactly that one keystroke.

The empty-composer guard cannot help: it asks the composer BEFORE the key is applied, so the keystroke
that turns an empty composer into a line is the keystroke it waves through. Any guard of the shape
"only when nothing is typed" has this hole for the first character.

A digit now MARKS on every numbered card, and the answer is the deliberate Enter - which the guard does
cover, because by then the composer has words in it.

Rake for the next card: a drawn row number is a promise about the digit. If a card numbers its rows and
the digit does not move the mark, either the numbers are decoration (say so) or the key is wrong. Grep
`digit_row(` and ask of each card whether one keystroke there can do something the operator cannot take
back.
