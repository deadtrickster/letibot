<!-- abstract: Every PNG fixture was a 33-byte header with a zeroed CRC, and one reached the operator's paste directory; a fixture's size is part of a protocol test, and completeness is a chunk walk. -->

# A picture fixture that was not a picture - and what turning it into one broke

A session died and no restart revived it. `save_clipboard_image` named its file from `head_runtime_dir()`, a test's `App` had `session_id: ""`, so the paste landed in the operator's own paste directory; the bytes were the fixture `png()` - signature, IHDR, a zeroed CRC, no IDAT, no IEND, 33 bytes; `Media::of` sniffed the magic, `read` stored it, and the provider refused the upload on every turn, because the row is append-only.

Rake 1: a fixture's size is part of what a protocol test asserts. `rano::term::graphics::image_upload` cuts the base64 at 4096 bytes, so the old fixture was one chunk with `m=0`, and a real 200×100 PNG is ~27 chunks with `m=1` first. The fixture is now 8×4, which is what the test is about.

Rake 2: completeness is a chunk walk, not a prefix check. `complete_image` (`crates/transcript/src/media.rs`) needs PNG's `IEND` to terminate the walk, JPEG's `EOI`, GIF's `0x3b`, a WEBP chunk that fits - and it refuses a chunk whose length runs past the file.

Rake 3: `attachments_dir()` resolves a test directory under `#[cfg(test)]`, which covers `cargo test --lib` and not `crates/tui/tests/*`, which compile the lib without it. A `cfg(test)` guard is a guard on unit tests.
