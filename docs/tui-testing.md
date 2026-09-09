# Seeing what the TUI actually renders

A terminal UI is the one part of this system you cannot verify by reading. This is
how to look at it from an agent, a script or CI, with no terminal of your own.

The technique is not ours. `~/Projects/rano` — a ratatui/crossterm nano clone on this
box — used it to settle a bug that no amount of reading would have: **two tmux
sessions, the same binary, byte-identical input, one variable changed.** Its
`TODO.md` records the result, and the shape is the reusable part.

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

## The controlled-pair form, which is the stronger one

Two sessions, identical except for the thing under test, captured and diffed. `rano`
used it to prove an LSP fault was workspace-path-dependent — same binary, same cursor
position, one path differing, junk in one and correct members in the other.

For a UI the same shape answers: does this change alter anything other than what I
meant it to. Capture before, change one thing, capture after, diff. Anything in the
diff you cannot explain is a regression you have not noticed yet.
