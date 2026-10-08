# Seeing what the TUI actually renders

A terminal UI is the one part of this system you cannot verify by reading. This is
how to look at it from an agent, a script or CI, with no terminal of your own.

The technique is not ours. `~/Projects/rano` — a ratatui/crossterm nano clone on this
box — used it to settle a bug that no amount of reading would have: **two tmux
sessions, the same binary, byte-identical input, one variable changed.** Its
`TODO.md` records the result, and the shape is the reusable part.

## The acceptance suite: this method, codified

`tests/acceptance/` is the method below turned into specs that run in CI, in Playwright's
shape: a driver (`lib.sh`) that starts the head in a private tmux server, acts with the keys
and SGR mouse reports a terminal sends (`press C-]`, `click "Edited notes.txt"`), locates by
on-screen text, and waits for the screen instead of sleeping (`wait_for`, `wait_gone`,
`settle`). A spec reads as steps; a failed step prints the screen it saw.

```bash
cargo build -p letibot-tui --bin letibot-tui && tests/acceptance/run.sh
LETIBOT_TUI=target/release/letibot-tui tests/acceptance/run.sh editor-pane
```

Its sessions are replays, so they need no daemon or model, and its fixtures are
**generated** by a unit test from the protocol's own types (`LETIBOT_BLESS=1` rewrites them),
so a protocol change fails a test instead of leaving a fixture the head no longer reads.
`--replay` reads input through the same routed path as a live session, so a spec reaches
everything the keyboard and mouse reach there, the editor pane included.

## The mechanism

```bash
S=letibot-shot-$$                       # distinctive name; you must clean it up
tmux new-session -d -s "$S" -x 100 -y 28 '~/bin/letibot'
sleep 3
tmux send-keys -t "$S" 'what crates are in this workspace?' Enter
sleep 20

tmux capture-pane -p    -t "$S"         # plain text — what a person sees
tmux capture-pane -p -e -t "$S"         # -e keeps SGR: colour, reverse video, bold

tmux kill-session -t "$S"
```

## Why each flag earns its place

- **`capture-pane -p` is a screenshot you can diff.** Same input, same size, before and
  after a change: the diff *is* the visual change. It can go in a test, which a
  description of the change cannot.
- **`-e` is not optional for cursors and colour.** A block cursor that is not being
  drawn looks **identical** to one that is, in a plain capture — the escape is the only
  evidence. Same for an accent bar's colour and for anything emitting escapes it
  should not.
- **`-x` / `-y` make narrow terminals testable** without a human resizing anything, and
  `tmux resize-window -t "$S" -x 60` mid-session exercises the resize path.
- **Headless**, so an agent with no tty can see its own output.

## Two limits, so it is not used for the wrong question

1. **A capture is the *rendered* pane, not the byte stream.** It cannot show you frames
   that were written and immediately overwritten, so it is the wrong instrument for
   repaint volume. That question is answered by counting bytes written to the terminal
   — which is how the 10 Hz full-repaint bug was found: 269 frames in 28 s, 221 of them
   byte-identical to the one before.
2. **Kill your sessions.** An orphaned tmux session holds a `harnessd` and its socket,
   and the next run then attaches to a daemon it did not start.

## What to put in front of it: a real session, out of the store

`letibot-tui --replay FILE.jsonl` needs no daemon, no socket and no model, and
`FILE.jsonl` is one `letibot_sessionlog::event::Envelope` per line. That makes the
input to a capture **data you choose**, which is what turns a screenshot into an
experiment: truncate the file with `head -n K` and the head renders the state it
was in at event K. A turn mid-round, a call that never came back, a screen with
nothing on it yet — all of them are a `head -n` away, with no timing to race.

The strongest fixture is not a synthetic one. Every session this box has ever run
is in `~/.local/share/letibot/sessions.db`, and the rows are transcript items:

```sql
select seq, item_id, kind, item_json from transcript_item
 where transcript_id = 's-…#t0' order by seq
```

Copy the file first — it is the operator's, and a `-wal` alongside it means a
daemon may be writing. `python3` has `sqlite3` built in; there is no `sqlite3`
binary on this machine. Turn each row into a `transcript_appended` plus a
`transcript_content` (which carries the whole `TranscriptItem`), wrap them in
`turn_started` / `prompt_progress` / `turn_finished`, and emit
`tool_call_proposed` / `tool_started` / `tool_finished` around the calls.

**Get the ordering right or the fixture tests a head nobody runs.** The engine
invokes every call in a round *before* appending any of the round's result rows
(`harnessd::harness`, the `for call in &calls` loop then one `append_items`), so a
`ToolFinished` always precedes its own row's `TranscriptAppended`. A fixture that
interleaves them the other way exercises a path the daemon never produces — and
in this tree the head reads that ordering to carry a call's duration onto its
settled card, so getting it backwards silently loses the field.

Two defects in the last visual pass were only visible against real data: call ids
are positional **within a round** (`call_0`, `call_1`, …), so a fourteen-round
turn out of the store has fourteen calls named `call_0` and any head that keys a
table on the id alone shows it. No synthetic fixture anybody was writing had two
rounds in it.

## Waiting: two loops, in that order

A replay paces itself (`VTIME=1` means each key poll costs 100 ms, so 600
envelopes take about a minute). Sleeping a fixed number of seconds either races
it or wastes a minute per capture. Poll instead: wait until the pane is
**non-empty**, then until two captures 1.5 s apart are **identical**. An empty
pane before startup is a boot window, not a finished render.

## The controlled-pair form, which is the stronger one

Two sessions, identical except for the thing under test, captured and diffed. `rano`
used it to prove an LSP fault was workspace-path-dependent — same binary, same cursor
position, one path differing, junk in one and correct members in the other.

For a UI the same shape answers: does this change alter anything other than what I
meant it to. Capture before, change one thing, capture after, diff. Anything in the
diff you cannot explain is a regression you have not noticed yet.
