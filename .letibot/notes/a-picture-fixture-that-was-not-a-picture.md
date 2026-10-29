<!-- abstract: MEASURED 2026-10-30 (branch agent/a-bad-picture-must-not-wedge): every PNG fixture in this tree was a 33-byte header with a zeroed CRC, and one of them was written into the operator's real paste dir by a tui unit test, read as an image, and refused by a provider on every turn for ever. Making the fixtures real broke a kitty-chunking assertion (a real 200×100 PNG is 27 chunks, so `m=0` is the LAST chunk, not the first). Plus: why completeness is a chunk WALK, and the two gaps left. -->

# A picture fixture that was not a picture — and what turning it into one broke

MEASURED 2026-10-30, branch `agent/a-bad-picture-must-not-wedge`, from one incident: a session
died and no restart could revive it.

## The chain, in the order it happened

1. `letibot-tui`'s `save_clipboard_image` took its directory from `head_runtime_dir()` and named
   the file `<session>-<n>.png`; `App::new` starts with `session_id: ""`, so a test's paste wrote
   `$TMPDIR/letibot-<uid>/attachments/-1.png` — the operator's own paste directory.
2. The bytes were the fixture `png()` in `app/tests/paste.rs` / `attachments.rs`: PNG signature +
   IHDR for 640×480 RGBA + **a zeroed CRC**, no IDAT, no IEND — 33 bytes.
3. `Media::of` sniffed the magic and read the IHDR, so `read` called it an image and stored it.
4. `messages.rs` rendered the tool result's `Media` as `image_url`, and the provider answered
   `http 400: .messages[1861].image[0]: You have uploaded an unsupported image.` — on every turn,
   because `transcript_item` is append-only.

## Rake 1 — making a fixture real changes the shape of the kitty upload

`rano::term::graphics::image_upload` cuts the base64 at **4096 bytes** per escape. The old 33-byte
fixture was one chunk, so `crates/tui/src/app/tests/attention.rs` asserted
`up.starts_with("\x1b_Ga=t,f=100,i={id},q=2,m=0;")`. With a real 200×100 PNG (~80 KB) the first
chunk is `m=1` and the whole transmit is one `Vec<u8>` of ~27 escapes — `ups.len()` is still 2, only
the `m=` moved. The fixture is now 8×4 (same 2:1 aspect, 200 bytes, one chunk), which is what that
test is actually about. **A fixture's SIZE is part of what a protocol test asserts.**

## Rake 2 — a completeness check cannot be a prefix check

`complete_image` (`crates/transcript/src/media.rs`) is a chunk **walk**, because every format states
its end at the END: PNG needs an `IHDR` first, an `IDAT` anywhere, and an `IEND` that terminates the
walk; JPEG needs `EOI`; GIF its `0x3b` trailer; WEBP a `VP8 `/`VP8L`/`VP8X` chunk that fits. The walk
also refuses a chunk whose length runs past the file, which is the truncated case a "has an IDAT and
an IEND" byte-search would miss.

Two more facts under it: `decode_base64` is **tolerant**, so a corrupt `data_ref` decodes short and
`data_ref_is_image` answers `false` — the right answer for bytes nobody can read; and the check
decodes the whole payload per render, because two of the four formats keep their verdict at the end.

## Rake 3 — a test directory that is only safe in a `--lib` build

`attachments_dir()` resolves a test-process directory under `#[cfg(test)]`, which covers
`cargo test -p letibot-tui --lib` — the incident's build. `crates/tui/tests/*` compile the lib
WITHOUT `cfg(test)`, so they get the production path; they cannot reach the write only because
`set_clipboard` is `cfg(test)` too and `default_clipboard()` hands them the real `SystemClipboard`.
**A `cfg(test)` guard is a guard on unit tests, not on every test.**

## What was left, on purpose

* `transcript::media::mark_delivered` still sets `delivered = true` for a picture the renderer
  withheld (a text-only model, or one a provider refused): the flag lives in `letibot-transcript`
  and the picture table lives in `letibot-provider`, so the predicate is not reachable from there.
  Pre-existing for the text-only path.
* `media_in_order` (the local dialect's `multimodal_data`) still sends whatever bytes a row holds.
  `Media::of` refuses incomplete ones at the source now, so this only matters for rows written
  before this build — and filtering one picture out of that list would desynchronise it from the
  dialect renderer's markers, which is worse.
* `read.rs`'s payload still says *"The image is attached to this result"*, which is a promise made
  before anything is sent (`delivered` is false at that moment). The sentence already carries *"if
  the attachment did not arrive, say so"*, and the right tense is not obvious.
