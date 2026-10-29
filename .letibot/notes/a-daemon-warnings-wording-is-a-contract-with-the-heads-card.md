<!-- abstract: MEASURED 2026-10-29: the head reads a hub warning's detail BY ITS OPENING (`review_card::warning_review` matches the literal `refused it, and the complaints reached nobody (`), so rewording a daemon warning silently degrades the operator's card to a generic line — and `notices.rs`'s bless-and-compare fixture cannot see it, because it only compares the text it just wrote. -->

# A warning's wording is a contract with the head's card, and re-blessing the fixture does not check it

MEASURED 2026-10-29, branch `agent/a-refusal-reaches-a-live-session`, changing
`mergequeue::notices::refusal_undelivered`'s sentence.

## The mechanism

`crates/tui/src/ui/transcript/review_card.rs`'s `warning_review(detail)` matches a hub warning's
detail by a **literal prefix**, not by its code:

```rust
if r.starts_with("refused it, and the complaints reached nobody (") {
    return Some(Review::new(Verdict::RefusedToldNobody, branch, None));
}
```

I moved the evidence out of those brackets (`reached nobody — every session … was tried: …`) and the
operator's card stopped saying `refused, told nobody`: the reader answered `None`, the row fell back
to a generic `! refusal_undelivered · the review of … · click for details`, and
`crates/tui/src/app/tests/reviewcard.rs` went red twice — `the_readers_read_every_shape_the_queue_writes`
(which panics by printing the detail it could not read) and
`every_review_warning_draws_as_one_line_and_opens_whole` (which compares the drawn LINE).

## Why the harness-side fixture test does not catch it

`notices.rs`'s `the_tuis_review_card_fixtures_are_what_the_queue_writes` is bless-and-compare over
`crates/tui/src/app/tests/fixtures/*.txt`: with `LETIBOT_BLESS_REVIEW_CARD=1` it WRITES, otherwise it
asserts equality against what is on disk. So it fails once, the fix looks like *re-bless*, and after
the re-bless it is green **on text no reader in this tree has parsed**. The text is a daemon's; the
reader is the head's, and only the head's own test parses it.

## What to do

- Changing any `notices.rs` sentence that a card reads ⇒ re-bless **and** run
  `cargo test -p letibot-tui --lib reviewcard`.
- The safe edit keeps the brackets where the reader expects them: the evidence went inside
  `reached nobody (every session they could have gone to was tried: …)`, and the sentence now says so
  in its own doc comment.
- The general shape: a warning's `code` is not what the head keys on — the *first words of the
  detail* are. Grep `crates/tui/src/ui/transcript/*_card.rs` for the code before rewording its text.
