<!-- abstract: MEASURED 2026-10-29: rano's fences_in required a delimiter at the start of a line's content, so `— ```path:60-97` was prose whose *closing* ``` opened a block that never closed — one odd fence drew a whole reply as code. Fixed on rano `fix/markdown-fences` (32ed5246); the citation filling is in letibot, in three places, and both scanners still need the delimiter at line start. -->

# A fence a model put after prose swallowed the rest of the reply

MEASURED 2026-10-29, from the operator's screenshot of a real reply in the head, fixed on rano
`fix/markdown-fences` (pushed `32ed5246dc47f645f46a7c71fc8dbb021b3d44f9`, cut from `bf3fa48`).

## The three symptoms, and they are ONE defect

The reply contained

```text
— ```/Users/dead/Projects/firecode/firecode/scripts/vsock-exec.py:60-97
```
```

(an em-dash and a space before the opening run). Drawn before the fix:

```text
|— ```/Users/…/vsock-exec.py:60-97        <- label as TEXT, with its backticks
|
|┌─                                       <- the CLOSING ``` opened a block
|│ ps -eo … → (nothing) cat /proc/loadavg → 0.00 …   <- two lines joined by a space
|│ **The smoking gun**                    <- and every word after it is code
|└─ (still writing…)
```

So: a fence whose info string is not a language is drawn as text with a stray run; a fence's
newlines are collapsed; and after it inline markup stops being parsed. All three are one cause.

## The mechanism, exactly

`fences_in` (`rano/src/markdown/parse.rs`) took a line's content after `split_container` and
required the **first byte** to be the run. `— ```x` fails that, so:

- the opening run stayed in a paragraph — and an unmatched ``` in a paragraph is literal text, so
  the label was drawn with its backticks;
- the paragraph's two source lines were joined by a space (`joined_runs`), which is what a
  paragraph does and what a code block must not;
- the **closing** ``` on the next line then opened a fence with no close, which runs to EOF, so
  everything after it was code — `**bold**` included.

A *second*, independent leak of the same class: ` ```path:60-97``` ` (a run at line start with a
second run after it) put the trailing run in the block's TITLE — `┌─ /w/a.py:60-97``` `.

## The fix, and the two rules that were rejected first

`opening_run` now opens a fence at a run that **starts a word** after prose, when the document
closes it, and the label stops at the next run of the fence's own character. `Fence::open` became
the *delimiter's* offset (it was the line's start) so `mask` blanks from there and the words in
front of the fence stay prose for the block grammar.

Two earlier cuts failed on an existing test, and both failures are the useful part:

- **"must be closed later" alone is too weak.** `a ``` b` in a paragraph, with a ```rust block
  three paragraphs below, "closes" at that block's closing line — the paragraph swallowed two
  blocks. Fixed by `swallows_a_fence`: a mid-line run is a fence only when no line between it and
  its closing delimiter opens a fence of its own.
- **The guard is "not glued to a word", not "preceded by a space".** `Here is the code:```rust`
  swallowed the same paragraphs with a colon in place of the space; `a```b` stays text, because a
  fence would have to eat the sentence around it.
- **"a second run on the line means no fence at all" poisons instead of leaking.** With that
  rule, ` ```path``` ` became a paragraph and the *next* ``` opened an unclosed block. So a run at
  line start always opens a fence, and a trailing run at least as long as it closes the fence on
  the same line — ` ```a/b.go:2-3``` ` is an empty citation.

## And an empty fence drew N+1 rows

`CodePaint::lines` split `""` into one empty line, so an empty fence drew `┌─` / `│ ` / `└─`.
`N` lines in, `N` rows out — the citation the harness convention writes is empty, and the extra
row made it look like a block with one blank line in it.

## Where `path:lines` is actually filled — LETIBOT, in three places

A renderer must not read files, and rano does not. The chain:

1. `crates/harnessd/src/citations.rs` — `cited_fences` finds empty fences whose label parses
   (`parse_label`), and `snapshot` reads the lines through the session's own `read` when the reply
   LANDS, capped at `MAX_CITED_LINES`.
2. The lines are stored beside the reply (`Store::put_citations`, keyed by the reply's text) and
   sent to the head as `Citation`s.
3. `crates/tui/src/ui/transcript/cite.rs` — `fill` puts them into the fence **before** rano lexes
   it, adding `· changed since the reply` when the file on disk no longer matches.
4. rano's `citation()` only recognises the label and numbers the rows; it never reads a file.

**The gap this left:** both letibot scanners test `line.trim_start().starts_with("```")`, so a
fence written after prose is still not filled — the head draws it as an empty block labelled with
the path (which is what rano can do) and the lines never arrive. Fixing that is a change in those
two scanners, not in the renderer, and it was not made here.

## The rake

**"It is not a fence, so it is text" is not a safe reading for a model's reply.** A delimiter the
parser declines to open does not disappear: it stays as literal backticks in a paragraph, and its
*partner* — the delimiter the model meant as the close — opens a block that runs to the end of the
document. Any rule about fence recognition has to say what happens to BOTH runs, and the test that
catches a regression is "does a paragraph after the odd fence still parse".
