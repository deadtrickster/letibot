<!-- abstract: MEASURED 2026-10-29 (answering the queue-pane review): `Host::ask` caches on the serialised `Input`, so any feed carrying a live clock misses the cache on every frame — the queue feed's clock is quantised to the second for that reason; plus `Input::folded`'s doc contradicted both layers, and a `Says::Text` on a list surface is a lie the moment a key means two things. -->

# A clock in a plugin feed is a wasm call per frame, and three other seams the queue pane found

MEASURED 2026-10-29 on the operator's Mac, branch `agent/answer-the-queue-plugin-refusal`
(answering the review of `agent/queue-pane-as-a-plugin`), `crates/tui`.

## 1. The host's cache is the INPUT, so a millisecond clock in a feed defeats it

`app::sessions_pane::Host::ask` keys its one-slot cache on
`format!("{hook}\n{}", serde_json::to_string(input))` — the whole input, feeds included. The queue
feed carried `now_ms` plus an age per row, so the key changed on every tick and every draw was one
wasm call **and one serialisation of the entire queue snapshot** (every review, every landing, every
finished row).

The fix is a quantum, not a flag: `CLOCK_QUANTUM_MS = 1_000` in `app::queue_pane` rounds the feed's
clock down before anything is derived from it. A row's finest fact is the layer's `age()`, which
prints `{}s`, and `(now - x) / 1000` is constant across a second's bucket — so no drawn frame can
differ, and the cache hits for the rest of the second. Measured by
`the_feeds_clock_moves_once_a_second_and_not_once_a_frame` (a scripted layer records every input:
two frames inside one second are ONE call, the frame after the tick is the second).

**Rake:** excluding the clock from the key instead would freeze every age until something else
changed — the cache would answer a *previous* second's drawing, which is a wrong frame rather than a
cheap one. Quantise the number the pane draws at, and nothing finer.

## 2. `Input::folded`'s doc was the opposite of both implementations

The SDK said *"`true` for a row the person has folded away"*. `plugins/builtin/sessions` reads
`Some(true)` as OPEN (`Folded::get`'s own doc says so), `plugins/builtin/queue` draws the history when
`folded["finished"] == true`, and both host maps (`App::picker_folds`, `App::queue_folds`) store
*open*. The doc is fixed; had it been the one anybody implemented against, the fold glyph would have
been inverted for a day and the tests would have agreed with each other.

## 3. A `Says::Text` on a list surface is a lie as soon as a key means two things

`QUEUE`'s Enter binding said `opens the row: the ask, the verdict, …` and the pane's history row
carries `{"fold": true}` — so the bar promised to open a row over the one row Enter folds. The seam
is `Says::Row` → `App::row_says`, which the picker/todos/subagents/jobs already use (the same defect
`.letibot/notes/the-four-edges-of-ranos-tree-list.md` records for the picker's group row). Cost of
the fix: one arm in `row_says` and a `pub(crate)` question for it to ask (`App::queue_row_folds`).
Note that `Says::Row` returning `None` DROPS the binding from the bar, so a surface that switches to
it must answer `Some` for every row it can be on, not only the interesting one.

## 4. A fold map nobody writes looks exactly like a fold map that works

`App::queue_folds` was declared, initialised, cloned into `Input::folded`, and written by **no
production path** — the branch's own test set it by hand and asserted the rendering, so the key the
row advertised (`enter opens it`) did nothing and every finished row was unreachable. A whole-tree
`grep` for the field is what found it: four hits, one of them a test.

**The general shape:** a host-side state map that only a test writes is a seam that is *drawn* and
not *connected*, and the test that sets it by hand is the test that certifies it. When a plugin row
carries an act, the question to ask is which production path performs it — here the answer was none,
for `Act::Fold`, on that surface.
