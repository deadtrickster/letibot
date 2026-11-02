<!-- abstract: `ctrl-n` retires a note by identity, and that key set already covered the plain live case, a resync and a reattach - the wall comes back only for a copy whose identity differs, so the fix is a per-kind watermark. -->

# `ctrl-n` was a hide, and the key set already covered every plain case

`ctrl-n` retires a note by IDENTITY (`w|code|ts|fnv1a(detail)`, persisted in
`~/.config/letibot/head.toml`) and the draw filter is `!dismissed.contains(&note_key(..))`, so the
plain cases were covered before any change: three warnings, `ctrl-n`, then a fourth draws exactly the
fourth - and so does the same with a `Resync`, a second `Hello`, or 70 notes.

The wall comes back only for a copy whose identity DIFFERS. Three ways: the words differ (a re-said
retry, a `[fallback]` re-worded, a scrubbed replay - `Hub::attach` runs `scrub_replay` over a resumed
gap, so a secret in the detail changes the hash); the key was evicted past `RETIRED_CAP`; or
`refresh_retired` replaced `dismissed` from the shared file. Behind all of it the source is never
trimmed - `App::notes` caps at 64 and the daemon's `view.warnings` at 128, re-sent in every snapshot.

The fix is `App::read_marks: Vec<(code, newest ts)>`, a watermark PER KIND, because per kind is what
keeps a new kind visible: no row for it, so nothing of it can be older than the mark. The draw reads
`ts < mark`, so a notice in the mark's own millisecond is news; `hide_read` records `ts <= mark` into
`dismissed`, because the copies at that instant were on the screen; and `ts == 0` is never watermarked,
since two notes with no clock cannot be ordered.
