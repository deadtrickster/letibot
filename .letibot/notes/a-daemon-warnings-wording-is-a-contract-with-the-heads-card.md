<!-- abstract: The head reads a hub warning's detail by its opening words, not by its code, so rewording a daemon warning silently degrades the operator's card - and the bless-and-compare fixture cannot see it, because it only compares the text it just wrote. -->

# A warning's wording is a contract with the head's card, and re-blessing the fixture does not check it

`crates/tui/src/ui/transcript/review_card.rs`'s `warning_review(detail)` matches a hub warning by a
LITERAL PREFIX of its detail, not by its code (`if r.starts_with("refused it, and the complaints
reached nobody (")`). Move the evidence out of those brackets and the operator's card stops saying
`refused, told nobody`: the reader answers `None` and the row falls back to a generic one-liner.

`notices.rs`'s `the_tuis_review_card_fixtures_are_what_the_queue_writes` cannot catch it: it is
bless-and-compare over `crates/tui/src/app/tests/fixtures/*.txt`, so it fails once, the fix looks like
re-bless, and after the re-bless it is green on text no reader in the tree has parsed. The text is a
daemon's; the reader is the head's.

So changing any `notices.rs` sentence a card reads means re-bless AND `cargo test -p letibot-tui --lib
reviewcard`, keeping the brackets where the reader expects them. A warning's `code` is not what the
head keys on - the first words of the detail are - so grep `crates/tui/src/ui/transcript/*_card.rs`
for the code before rewording its text.
