<!-- abstract: Rounding a daemon notice's closing words is a second contract with the head: `line_cards::split_card` splits a row by `SESSION.ends`, so a new tail made the emergency card swallow the notice after it - after both fixture tests were re-blessed green. -->

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

### And a session NOTICE is split by its CLOSING words, which is a second contract

R93.F47 shortened `mergequeue::notices::TOOL_WAYS`, the paragraph every `health` notice ends with.
Both bless-and-compare fixture tests went green after a re-bless, `notice_tip` still parsed the
fixture, and one test then failed that neither the fixtures nor the readers name:
`app::tests::redcard::an_emergency_beside_a_job_completion_keeps_both_compact`, because the job
line had vanished. The cause is `crates/tui/src/ui/transcript/red_card.rs`'s
`NOTICE_END = "the operator, whose decision it is."` - `line_cards::split_card` splits a row of
notices after the words a kind lists in `ends`, and with the old closing gone the whole row became
ONE notice, so the emergency card ate the completion behind it.

So a notice's ENDING is a contract exactly as its opening is, and it fails as *extra text in a card*
rather than as a missing card. `SESSION.ends` is a slice for that reason: keep the old closing
alongside the new one, because rows already in a store end with it, and pin both
(`a_notice_splits_after_the_closing_it_was_written_with`). The lesson generalises: after rewording a
daemon sentence, run the head's card tests by name - `cargo test -p letibot-tui --lib redcard` -
before believing the fixture tests.
