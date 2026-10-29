<!-- abstract: MEASURED 2026-10-29: the one-line citation fence (` ```a/b.go:2-3``` `) was never filled — the daemon glued the closing run onto the label and then looked for a separate closer, and its scan consumed to the end of the reply; the head had a SECOND scanner, keyed on the whole label while the daemon keyed the first word. -->

# A one-line citation fence was never filled, and the two halves scanned differently — MEASURED 2026-10-29, branch `agent/a-one-line-citation-is-filled` (worktree of the same name, `main` at `e2fe4611`)

The convention: an empty fence labelled `path:start-end` is filled with those lines. Two
spellings are in use — the empty block (opener, blank line, closer) and the one-liner
(```` ```a/b.go:2-3``` ```` alone on its line) — and only the block worked.

## What the old scanner did, measured on the three inputs

`crates/harnessd/src/citations.rs::cited_fences` at `e2fe4611`:

| input | before | after |
|---|---|---|
| prose, the one-liner, prose | `[]` | one `Cited{path, 60, 97}` |
| prose, an empty block, prose | one `Cited` | one `Cited` |
| an unclosed fence above a well-formed block | `[]` | the well-formed block's `Cited` |

Two causes, and the second is the worse one:

1. **The closing run was glued onto the label.** `label = t[fence.len()..].trim()` on a one-liner
   gives `/path:60-97````, and the `for body in lines.by_ref()` loop then looked for a *separate*
   closer, found none, and `closed` stayed false.
2. **The scan consumed to the end of the reply.** `lines.by_ref()` had no stopping rule, so an
   unclosed fence swallowed everything after it and could take a later block's closer for its
   own — which is why the third row was `[]`: the first fence ate the ```` ``` ```` that belonged
   to the well-formed block below it.

## And the head had a second scanner that disagreed twice

`crates/tui/src/ui/transcript/cite.rs::fill` did not share the daemon's rule. It had the same
one-liner blind spot, and it matched a fence to a citation by the **whole label**
(`cits[k].label == label`) while the daemon keys a citation by the label's **first word**
(`parse_label` stores `word`). So a label that says more than the range
(```` ```src/main.rs:7 the entry point\n``` ````) was cited by the daemon and never filled by the
head. Also its own parsing was `rano::markdown::citation`, a third opinion on what a label means.

## Where the rule lives now

`crates/transcript/src/citation.rs` — `fence_at(&lines, i) -> Option<Fence>` and
`range(label) -> Option<Range>`, both used by the daemon and by the head. `Fence` carries
`run`, `label`, `empty`, `closed` and `end` (the block's last line), so a consumer skips a block
whole and never rescans its body.

**The unclosed rule, chosen and pinned**: a fence that is never closed ends on the line before the
next line that *begins* a fence, or at the end of the text, and reports `closed: false`. That is
what makes the third row above work — the unclosed fence cannot take the next block's closer, and
the next block is still there to be read on its own terms.

## Rakes

- **A test whose citation label is an ABSOLUTE path can read the operator's disk.** The real
  reply cites `/Users/dead/Projects/firecode/.../vsock-exec.py:60-97`, and that file exists on
  this box, so `fill`'s drift check compared against it and appended `changed since the reply`.
  The head test asserts the fill's parts (label line, lines, prose after) rather than the exact
  bytes, and says why.
- A `~~~` fence and a ```` ``` ```` fence are the same rule; the run that opened a block is the
  run that closes it, and `block()` re-emits the model's own run.
