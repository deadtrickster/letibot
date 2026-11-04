<!-- abstract: `Policy::after` reads the error's `Retry-After`, so a fake provider that scripts one makes the ladder sleep a minute per rung; and `Config::embedder` is an Arc-shared slot the box fills on a thread, so an old fixture name no longer compiles and a session opened early scores lexically. -->

# A `Retry-After` in a fake provider, and the embedder slot that replaced three config fields

`crates/http/src/retry.rs`'s `Policy::after` takes the failure as well as the rung: the wait is
`max(rung, Retry-After)` capped at `LONGEST_WAIT` (60 s). A fake provider that answers

    HTTP/1.1 429 ... Retry-After: 90

against a daemon with the default `cfg.http_retries` of 5 makes that turn sleep 5 × 60 s, not
1+2+4+8+16 = 31 s. `crates/harnessd/tests/provider_trouble.rs` sets `http_retries = 0` for this reason;
a new fixture about a rate limit must do the same or script a `Retry-After` of 0 or 1. The honest
figure is `Ladder::waited()`, the sum of the waits actually returned, and `ending()` reports it
("tried 6 times over 5m 00s").

`crates/harnessd/src/config.rs`'s `EmbedderSlot` replaced the three plain fields `embedder`,
`embedder_fault` and `embedder_fetch_note`. It is `Arc`-shared and interior-mutable because
`read_embeddings` moves a first-use fetch onto a thread (`may_fetch`: only
`Mode::Internal { model: None }` with `download` set). Two consequences for a fixture:

- `cfg.embedder.get()` is `None` right after `read_embeddings` on a fetching box, and a session opened
  at that moment scores lexically for its whole life: `cfg.tier()` is read once, at open.
- the fault and the fetch note are read at a session's open, so a fetch that fails thirty seconds in
  is said by the NEXT session opened. `set_fault` / `set_fetch_note` are how a test says either one
  without a network, and `open_the_embedder(settings, data, closure)` is the seam that proves the
  timing.
