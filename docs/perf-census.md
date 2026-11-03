# Perf census: one daemon, many sessions

`crates/harnessd/examples/perf_census.rs` runs one in-process daemon (`Daemon::serve` + `Sessions`), opens N sessions, and streams one fake turn per session: 32 text chunks, one `read` tool call with arguments in two pieces, then an answer. The model is a loopback stub speaking the SSE frames `letibot-provider` parses, so there is no network. Subscribers are real `HeadClient`s on the real unix socket. Daemon and bench share a process, so a subscriber's own decode is inside "CPU".

## Rerun

```sh
scripts/rrun perf-NAME $PWD --host lab2x1 -- \
  'CARGO_TARGET_DIR=/data/letibot-runs-target/perf cargo run --offline --release -p letibot-harnessd --example perf_census -- 1,10,50 1 > perf.txt 2>&1; echo $?' \
  'grep -E "^ +[0-9]+ +[0-9]+ |threads|WAL|store |fan-out|marginal" perf.txt; false'
```

Args: session counts, then subscribers per session (0 is the control). The second step is `false` only so rrun keeps the output tail.

## What is measured

- RSS: `/proc/self/status` after all sessions opened, and the peak. Marginal per session is the slope between runs.
- CPU: `getrusage` over the streaming phase, per event.
- Emit-to-subscriber latency: a probe warning published on a session's hub, timed to the moment a subscriber has the frame (fan-out clone, queue, pump JSON, socket, client parse).
- Fan-out per subscriber: one session, S = 1, 2, 4, 8 subscribers, CPU per event, least-squares slope.
- Store write volume: db + WAL growth over the streaming phase, with `wal_autocheckpoint = 0` so the WAL size is the volume written. A WAL frame is 4120 bytes.

lab2x1 was shared and loaded (load average 20 to 36) during every run. Treat differences under about 20% as noise.

## Numbers (lab2x1, release)

Before is `perf-before1` (both fixes reverted), after is `perf-after1`, same load window.

| sessions | RSS open KiB | RSS peak KiB | CPU ms | events | us/event | p50 us | p99 us | WAL frames/turn |
|---|---|---|---|---|---|---|---|---|
| 1 (after) | 214536 | 217452 | 7.1 | 157 | 45.3 | 22 | 657 | 15.0 |
| 10 before | 239920 | 251828 | 40.2 | 662 | 60.8 | 24 | 127 | 16.7 |
| 10 after | 237864 | 249944 | 45.7 | 659 | 69.3 | 25 | 196 | 16.5 |
| 50 before | 321916 | 374640 | 191.5 | 2899 | 66.1 | 28 | 447 | 17.1 |
| 50 after | 320312 | 373388 | 198.8 | 2900 | 68.6 | 28 | 453 | 17.1 |

Marginal per session: RSS 2.16 to 2.19 MiB, CPU 3.8 ms per turn, 56 events per turn, about 5 threads (9 at one session, 62 at ten, 257 at fifty; the bench adds a reader thread and a client per subscriber).

Fan-out, CPU per extra subscriber per event (fit slope), runs at 10 and 50 sessions plus the 1-session sweep:

- before: 5.15, 7.51, 7.81 (earlier runs of the same build: 6.6, 7.0, 7.2)
- after: 6.00, 6.18, 5.85, 6.38 (earlier runs: 6.4, 3.4)

CPU per event with 8 subscribers: about 52 us before, about 45 us after. The fit is linear, so no superlinear cost with subscribers. The gain is within noise at the 20% level; it is a direction, not a proof.

## Hotspots

1. Store write volume. 15 to 17 WAL frames (60 to 70 KiB) per turn of about 4 rows, at `synchronous = FULL`. A turn commits per prompt, per round's rows, per round's context row, and per tool-result batch. Fixed in part: `Store::append_items_with_context` carries the round's context UPDATE in the rows' last commit, so a round costs one commit (one fsync) instead of two, asserted by `the_context_row_rides_the_rows_commit` (counts commits with a commit hook). WAL bytes did not change (17.1 frames before and after): the session row page is dirtied once either way. The fsync saving is not visible to this bench, which has no fsync counter. Not done, and the way to cut bytes: write the user row, the round's rows and the tool results of one turn in fewer transactions, and look at why a 4-row commit dirties about 3 pages (token blob and the indexes on `transcript_item`).
2. Socket fan-out per subscriber, 6 to 8 us CPU per event. The head pump wrote and flushed each event as its own frame and cloned each envelope to wrap it in `ServerFrame::Event`. Fixed: `FrameWriter::write_all` sends a queued batch in one write (`a_batch_of_frames_is_one_write_and_reads_back_frame_by_frame`), and `EventOut` serialises the envelope by reference (`the_borrowed_event_frame_is_the_owned_one` pins the bytes to the owned frame's).

Not fixed, described: RSS is 2.2 MiB per session on top of a 214 MiB floor with zero load. Five threads per session (about 3 of them the bench's) each get a malloc arena. Check `MALLOC_ARENA_MAX` or a shared pump before suspecting the harness. CPU per turn (3.8 ms) and p50 latency (22 to 28 us) are small. p99 latency (127 to 650 us) tracks host load.
