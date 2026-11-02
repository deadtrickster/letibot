<!-- abstract: `Host::ask` caches on the serialised `Input`, so a feed carrying a live clock misses the cache on every frame; quantise the number the pane draws at rather than excluding the clock from the key. -->

# A clock in a plugin feed is a wasm call per frame, and three other seams the queue pane found

`app::sessions_pane::Host::ask` keys its one-slot cache on the whole serialised `Input`, feeds
included, so a feed carrying `now_ms` missed on every frame: one wasm call and one serialisation of the
entire queue snapshot per draw. The fix is a quantum, not a flag - `CLOCK_QUANTUM_MS = 1_000` in
`app::queue_pane` rounds the clock down before anything is derived from it, and a row's finest fact is
an age printed in whole seconds, so no drawn frame can differ and the cache hits for the rest of the
second (`the_feeds_clock_moves_once_a_second_and_not_once_a_frame`).

Do not exclude the clock from the key instead: that freezes every age until something else changes,
which is a wrong frame rather than a cheap one. Quantise the number the pane draws at.

A `Says::Text` on a list surface is a lie as soon as one key means two things - `QUEUE`'s Enter promised
to open a row over the one row it folds. The seam is `Says::Row` -> `App::row_says`, which the picker,
todos, subagents and jobs already use, and a `None` there DROPS the binding from the bar.
