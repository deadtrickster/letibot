<!-- abstract: MEASURED 2026-11-01: `ended_in` was computed for the single-handle door only, so every lost child that did not end on an answer printed one sentence in the list; the caveat belongs at the START of a tool payload (a spill keeps the head half, a notice keeps `first_line`); and the class must be asked BY HANDLE, because the list carries only the children a record was DUE for. -->

# A second-hand answer must say so, and the caveat belongs at the head of the payload

**MEASURED 2026-11-01**, branch `agent/the-lost-child-says-how-far`, on top of
`agent/the-lost-child-is-still-listed` (`01b3ab3c`) — the follow-up its reviewer asked for.

## 1. A fact computed for one door is a fact the other door lacks

`subagent_record::ended_in` (now `EndedIn::said`) said exactly where a recordless child stopped —
`the result of `bash`, so the turn went on from there`, `a call it had just asked for (`bash`) and
never got an answer to`, `nothing: it wrote no row of its own at all` — and only
`a_child_with_no_record`'s **Failed** sentence printed it. The list row carried a bool
(`ChildName.lost`) and printed ONE fixed sentence for every child that did not end on an assistant
row, so a parent could not tell a child that died mid-`bash` from one that never opened.

The fix is a shape worth copying: **the class travels on the row** (`ChildName.lost:
Option<LostChild>` — `Answered` | `Stopped(<clause>)`), built by one function
(`subagent_record::how_far`) over the same predicate that decides a text IS an answer
(`its_answer`). Two doors, one judgement, and `EndedIn` grew a second rendering: `said()` (the
paragraph, byte-identical to what the answer door printed) and `brief()` (a clause that reads on
from *"it stopped "*).

Rake: **a bool that marks a row "second-hand" is not the class.** If a list row and a detail row
about one thing both have to say how far it got, the class belongs on the row, not in whichever
door happens to compute it.

## 2. The class must be asked BY HANDLE, and where a caveat goes is the truncation's decision

**By handle, because the two reads are windowed differently.** `ChildName.lost` is built from
`children_without_runs`, which is the children a record was **due** for; `look` /
`child_without_run` answer for any child. So a child from before the store's first record — 517 of
one session's 573 on the operator's box — is *not in the list*, and reading the caveat's class off
the list left exactly that class's answers bare. `TaskRunner::lost(handle)` is the keyed question
the answer door asks instead, and the integration test stages a pre-record child with an answer to
pin it.

A child with no record's last `Assistant` row is returned as `Done { answer: text }` — and
`SubagentRecord::settled`'s own comment says why that is not an answer: *during a shutdown an
interrupted turn comes back as an ANSWER — whatever the child had written when it stopped — so no
status here can be trusted to be the child's last word*. The caveat now leads the payload, for two
measured reasons:

* `Spiller::apply` splits the budget **head/tail** (`head = ceil(budget/2)`), so the head half is
  what a spilled payload keeps;
* a child's settlement **notice** is `first_line(&answer)` (`jobwatch.rs:1879`), so the first line
  is the only line a notice ever carries.

So a sentence a reader must not miss goes FIRST, in the payload — not in a headline and not at the
end. The caveat is one `const` (`task::NOT_A_FINISHED_ANSWER`) used by the list row and the answer,
because two renderings of one fact drift.

## 3. Staging a child's last row in an integration test

`unrecorded_of` reads the child's newest transcript's highest-`seq` row, so a test that wants a
lost child that "died mid-`bash`" stages one:

```rust
let prefix_id = store.put_stable_prefix(&StablePrefixRecord { /* 4 tokens */ }).unwrap();
store.put_transcript(&format!("t-{id}"), id, &prefix_id).unwrap();
store.append_item(&format!("t-{id}"), 0, &item, &LedgerRow {
    item_id: "a".into(), tok_offset: 4, tok_len: 1, h_k: [1; 32],
}, &[9]).unwrap();
```

`tok_offset` for `seq` 0 must equal **the prefix's token count** (`assert_anchored`), and
`put_session` stamps `created_at` with the clock — so a child that must be *older* than the store's
first `subagent_run` needs an `UPDATE session SET created_at`.

## 4. Why the class went on `ChildName` and not on `TaskStatus::Done`

`ChildName` is built in 6 places; `TaskStatus::Done` in 51. A new field on `Done` would have been a
51-site change for a fact only two doors read.

Numbers, `/data/letibot-target-lostchild2`: `-p letibot-tools --lib` 1328 passed;
`-p letibot-harnessd --lib` 961 passed, 2 ignored; `--test restart_restore` 21 passed.
