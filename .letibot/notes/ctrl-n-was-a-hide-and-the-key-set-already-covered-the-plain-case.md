<!-- abstract: MEASURED 2026-10-30: ctrl-n retired by note identity (`code|ts|hash(detail)`), and that key set ALREADY hid the plain live case, a resync, a reattach and 70 notes — the wall comes back only for a copy whose identity differs, which is why the fix is a per-kind watermark and not a bigger list. -->

# `ctrl-n` was a hide, and the key set already covered every plain case

MEASURED 2026-10-30, branch `agent/ctrl-n-is-a-watermark-not-a-hide` (worktree of the same
name, `main` at `18bfbce5`), `cargo test --offline -p letibot-tui --lib` with four throwaway
tests I then deleted.

## What did NOT reproduce, before any change

The operator's *"i do ctrl-n to hide but the very next time model_endpoint_retry happens im
presented with the wall of all endpoint_retry notices ever happened"* does not come back
through `App::notes` on any path I could build:

| path | result before the change |
|---|---|
| three `Warning`s, `ctrl-n`, a fourth | green — exactly the fourth is drawn |
| … with a `ServerFrame::Resync` in between (which replaces `self.notes` wholesale) | green |
| … with a second `Hello` (a reattach) in between | green |
| … at 70 notes, i.e. past `file_note`'s 64-entry cap, with and without a resync | green |

The reason is `ui/transcript/window.rs`'s draw filter: `!dismissed.contains(&note_key(..))`,
and `dismissed` is a **set of identities** (`w|code|ts|fnv1a(detail)`), persisted in
`~/.config/letibot/head.toml`. A copy whose `(code, ts, detail)` is unchanged is the same
key and stays hidden — including a snapshot's re-plant of it.

## So where the wall does come from

The set is a statement about **copies**, and every way the copy can differ is a way back in:

* the words differ — a re-said retry, a `[fallback]` re-worded on the way past, a
  **scrubbed replay** (`Hub::attach` runs `scrub_replay` over a resumed gap, `hub.rs:1270`,
  so a secret in the detail changes the hash and the key);
* the key was **evicted**: `retire`/`merge_retired` drop the oldest past `RETIRED_CAP` (512);
* `refresh_retired` **replaces** `dismissed` from the shared file, so a key this head added
  in memory (from the mark) is dropped and the wall it cleared comes back on the next draw.

The one *retained* list behind it is never trimmed: `App::notes` (cap 64) and the daemon's
`view.warnings` (bounded at 128, re-sent in every snapshot — `sessionlog/src/view.rs:866`).
A hide over an untrimmed source is exactly the shape the brief calls shape 1.

## What the fix is, and the two asymmetries worth remembering

`App::read_marks: Vec<(code, newest ts)>` — a watermark **per kind** (per kind is what keeps
a new kind visible: no row, so nothing of it can be older than the mark).

* the DRAW reads `ts < mark`: a notice in the mark's own millisecond is news. Drawing one
  line costs nothing; hiding one loses it for ever.
* `hide_read` records `ts <= mark` into `dismissed`: the copies at that instant *were* on the
  screen when the press happened, and without this the newest line was hidden only by its key
  — which a `head.toml` reload brings back. MEASURED: with `<=` only at the draw, the reload
  test drew the wall again.
* `ts == 0` is never watermarked (`unreadable_frame`, `log_gap` happen on the socket): two
  notes with no clock cannot be ordered, so a mark of 0 would hide the second one for ever.

Proof that the mark is doing work: neutering `already_read`/`covered_by_mark` to `false` turns
`a_notice_of_a_kind_the_reader_has_read_past_is_not_news_in_other_words` and
`the_watermark_survives_the_key_set_being_reloaded_from_the_file` red, and leaves the plain
three-then-a-fourth test green — which is the measurement above, restated.
