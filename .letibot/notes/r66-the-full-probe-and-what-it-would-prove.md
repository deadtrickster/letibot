<!-- abstract: What a session's harness holds is measured by `Footprint` against `max(1 MiB, window × 16 + 144 KiB)`, and the scale that does not fit in CI is an `--ignored` `footprint_plateau` run whose verdicts are ratios. -->

# R66's full probe: what to run, what to read, and which number proves which answer

`crates/harnessd/src/footprint.rs` counts what a session's HARNESS holds - `items_text_bytes`
(`TranscriptItem::bytes` over `Session::items`), `ledger_region_bytes` (the token region's mapped
bytes), `trail_bytes` - against `ceiling_for(window)` (`footprint.rs:40`): `max(1 MiB, window × 16 +
trail ceiling)`. `window × 16` is 8 bytes of text a token plus 8 of region a token, doubled for
capacity, so this box's ordinary 32 768-token session is held to the 1 MiB floor.

The scale that does not fit in CI is behind `--ignored`:

```sh
R66_SESSIONS=50 R66_TURNS=200 R66_TURN_BYTES=1048576 \
  cargo test --release --offline -p letibot-harnessd --test footprint_plateau -- --ignored --nocapture
```

It needs ~15 GiB, most of it `RssShmem` rather than heap. Read the verdicts as ratios: heap growth over
bytes appended near 1 means the daemon holds the work and nothing else. Two cautions: a memfd's pages
are `RssShmem`, so `lsof` and `RssAnon` will not show the region; and the probe drives
`Harness::system_update`, which does not consult the wall, so it measures the unbounded case.
