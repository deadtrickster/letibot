<!-- abstract: A fence a model put after prose used to draw the whole rest of the reply as code, because the closing run opened a block that never closed; a run that starts a word after prose now opens a fence when the document closes it. -->

# A fence a model put after prose swallowed the rest of the reply

rano's `fences_in` required a delimiter at the first byte of a line's content, so a reply line like
`- ```path:60-97` (a dash and a space before the run) was prose. The consequences compound: the opening
run stays in a paragraph and is drawn as literal backticks, the paragraph's lines are joined by a space,
and then the CLOSING run opens a fence that never closes and runs to the end of the document - every
word after it is code.

The fix: a run which starts a word after prose opens a fence when the document closes it, and the label
stops at the next run of the fence's own character. "Must be closed later" alone is too weak - `a ``` b`
closes at the next block's closer and swallows it - and the guard is "not glued to a word", not
"preceded by a space": `Here is the code:```rust` swallows the same paragraphs with a colon.

The rake: "it is not a fence, so it is text" is not a safe reading for a model's reply. A delimiter the
parser declines to open stays as literal backticks, and its partner opens a block, so any rule about
fence recognition has to say what happens to BOTH runs.
