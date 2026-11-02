# byobu's chrome - what letibot took from it, and what it left

byobu looks like a chrome system and is not one. `byobu` is a shell script that `exec`s tmux with a profile,
and that profile is five `source-file` lines; `byobu-janitor` seeds them on first run. Everything that makes
the bar work - the pty per pane, the VT parser, the status line's rendering and truncation, the mouse and its
hit-testing, the window list - is tmux's. What byobu owns is the **content** of the two halves of the bar, a
palette, a directory of shell functions invoked through `#()`, and a keybinding file.

That is the finding this study is built on: **you can own the chrome without owning the terminal**, as long as
the thing you are decorating hands you a string to fill and a set of bindings to override. tmux's version of
that arrangement is coarse - one string per half, one cadence, character truncation - and this document is
about which of its ideas survive into letibot's own frame.

## 1. The findings that still govern a drawing

### 1.1 A span is reserved, a cap is the bug

byobu's whole width model is two caps: `status-left-length = w/4` and `status-right-length = 3w/4`, recomputed
only when the session width changes. `w/4 + 3w/4 = w`, so the caps sum to exactly the terminal's width and there
is **no** width at which the window list is guaranteed a column: the left half takes `min(its width, its cap)`,
then the right, and the list gets `width - left - right` with no reservation and no minimum - 4 columns at width
120, 5 at 60, zero at 40 and 80. Within each half truncation eats the tail, so the first things to vanish are
the last items of the list: on a stock right half the clock, then the date, then the disk.

Two rules come out of that, and both are in this head:

- **Reserve a span; do not cap a half.** A cap says "you may use up to", which is a promise about nothing. A
  reserved span says "these columns are yours", and everything else is laid out around it. The composer's box
  is the same shape: the left legend takes what it needs (up to `inner - 4`), the right legend gets
  `inner - left_cols - 3` and is truncated into it, and the fill takes the remainder.
- **Give up a whole segment before truncating any.** byobu can only cut characters, so a narrow bar shows
  `<ba>` - half a segment, which means nothing. A segment is the smallest unit that carries a meaning, so it is
  the unit of loss. In this head that is the fit ladder's rung: the queue standings, the waits-on-you count and
  a plugin pane's badge each go whole, and the words that remain are cut from their right end only because the
  edge truncates that way - which is why the leading word has to carry the fact.

### 1.2 The renderer writes the action into the frame

The most transferable idea in byobu's stack is tmux's `#[range=window|#{window_index} …]`. The status
renderer emits a range marker around each window as it draws, the mouse binding is `switch-client -t =`, and
tmux resolves `=` to the window the event landed on. **Nothing recomputes a column.** The binding does not
know the layout; the frame does, and the frame is where the answer is read back from.

letibot had the opposite: three sites each re-derived the renderer's row arithmetic by hand and each guarded
itself with a "did the frame actually draw that row" flag. The equivalent of the range marker is
`crates/tui/src/app/hits.rs`: `compose_screen` records `Hit { rect, target, stops }` for every clickable
thing it draws, with the scroll already applied, and a click is answered from that record alone
(`App::hit_at`, later records on top). A new clickable segment costs no arithmetic, and "did the frame draw
that row" is answered structurally instead of per site.

The same shape covers things a terminal cannot report at all: the head takes the mouse, so a link's cells are
read back off the finished row (`App::draw_links` over `app::pointer::pieces`) and recorded as a `Target`
rather than left to the terminal's own OSC 8 handling.

### 1.3 Colour is a role's, not a segment's

byobu's four palette variables are used for the bar as a whole and, separately, each segment prints its own
`#[fg=…,bg=…]` escapes: a measured right half is 996 characters of which 950 are style escapes. Every segment
therefore makes its own colour decisions, across thirty-odd scripts.

The rule here is the opposite and it is already built: a caller names a `Role` and only `Palette` knows what
it looks like, so a segment cannot decide a colour. See `docs/ratatui-survey.md` for the palette's own rules
(roles not colours, the ANSI 16 and not the cube, reverse video for the user's block).

### 1.4 Two clocks, and the second one is a per-segment staleness

byobu has two cadences that are not synchronised: tmux redraws the bar at `status-interval 1`, and each segment
re-runs on its own **prime** interval (`load_average` 2 s, `memory` 13 s, `release` 599 s, `whoami` 86029 s)
because prime intervals "decrease collisions". The bar is sampled at 1 Hz and each segment is stale by up to its
own prime. The shape - a segment declares how stale it may be, and only what is due is recomputed - is worth
taking and is **not** taken for the status line, whose rows are cheap to compose whole. It is taken where a
recomputation is not cheap: a plugin pane's `every_s` says when the host should ask the layer again, and the
head's own records (`hist_marks`, `note_upto`) are incremental so a draw does not re-walk the history.

## 2. What governs a key

**The chrome's mouse and a program's mouse are separated by where the event landed, not by who wants it.**
tmux asks the outer terminal for mouse reporting when its own `mouse` is on (`?1000h ?1002h ?1006h`, and never
`?1003h`); a pane that wants the mouse says so on its own pty, and tmux records that as a format variable
(`mouse_any_flag`). The default bindings then branch: a `Pane` binding forwards with `send-keys -M` when the
pane asked, and a `Status*` binding never reaches a pane at all. There is no ambiguity to resolve, because the
location decides before the binding is looked up.

This head is the chrome and the program in one process, so it asks for the mouse itself (`Mouse::Any`, all
motion and SGR coordinates) and separates the two by what it recorded on the frame: a wheel notch arrives with
its position and scrolls the surface under the pointer, read off the last frame's record. What it cannot do is
give the terminal's own selection back, and that trade is stated where the decoder is: clicks are decoded and
**selecting text stays the terminal's own Shift+drag**, which mouse tracking does not take away. tmux spends the
plain drag on copy-mode and gives back Shift the same way, so a head that wants the drag for its own chrome
must document Shift the same way.

Two smaller facts a chrome must live with: **tmux re-asserts its modes** on attach and on a window switch,
which is why a program that turns the mouse off cannot keep it off; and a pane that asks for the mouse gets the
drag too, so a program's selection and the chrome's are exclusive for the unmodified gesture.

## 3. Is the pane a tmux client?

**No, and two facts narrow it before any measurement.**

**Attaching a real client means giving up the chrome.** tmux draws the whole screen including the bottom row,
and the only way to put anything there is `status-format` with `#()` commands - byobu's arrangement, i.e. the
chrome becomes shell scripts again, which is the opposite of what the role table and the row diff exist for.

**A control-mode client gives no screen.** `%output pane-id value` is the program's bytes, not a grid, and it
has no frame boundary; `capture-pane -p` is a snapshot of the composited grid. So the choice is not "tmux
instead of `crates/vt`" - a control-mode pane must be rendered by us either way, and the emulator stays in both
worlds.

What tmux would replace is the **session layer**, which is exactly the unfinished half of the shell: streaming
(`%output` is a stream), `SIGWINCH` (`refresh-client -C`), scrollback (`capture-pane -S -`), a registry of
running sessions, and the input path (`send-keys`, literal and `-H`). Against it: a control-mode client cannot
forward a mouse (`send-keys -M` answers "no mouse target"), a tmux server is a second long-lived process tree
whose lifetime has to be expressed like the session cgroup's, and the dependency has a version floor.

**The one measurement that would decide it.** One private server (`TMUX_TMPDIR` under the test's scratch dir,
`-L letibot-spike -f /dev/null`), one pane running a full-screen program, and two readers of the same pane: ours
(a control client, `refresh-client -C <cols>x<rows>` set to the rectangle the head would give the pane,
octal-decoding every `%output` payload into one `letibot_vt::Screen`) and tmux's (`capture-pane -p -J` at the
same rectangle). The pass condition is **cell for cell** agreement at two sizes with a resize between them,
normalised only by the two known differences: `capture-pane` strips trailing whitespace, and `crates/vt` keeps
characters and four attributes with the colour slots compared as slots. A missing row, a differently wrapped
row, a cursor in the wrong cell, or a screen that diverges only after the resize is a fail. If it passes, the
pane can be a tmux session with the emulator unchanged; if it fails, the failure names the sequences
`crates/vt` is missing, which is worth having either way. What it does not settle is whether a second
long-lived tmux server belongs in the product at all - that is a policy call about process lifetimes, not a
fidelity one.

**`capture-pane` as an oracle, and its limits.** For a model the right invocation is `capture-pane -p -J`:
plain text, logical lines, no escapes, "exactly the visible contents" or the history with `-S`. It is a snapshot
and not a stream, so it can say what the screen ended up looking like and never how much was written to get
there - which is why the head counts its own writes. It is the pane's screen and not the terminal's, so the
status row and the composer are not in it; it has no cursor and no notion of a frame. `docs/tui-testing.md` uses
it as the way to read a head, for exactly those reasons.

## 4. What this study did not verify

Whether byobu's `mouse.tmux.enable`/`disable` inversion (sourcing `enable` sets `mouse off` and says "Mouse:
ON") is a bug or a convention: both files and their messages were read, but not byobu's history or changelog.
The exact cost of a byobu status tick on the operator's own machine - the figures taken were for the stock
segment list under a scratch `HOME`. Anything about byobu's *screen* backend, which is dead on a tmux backend.
`capture-pane` fidelity for every class of program: a shell, a wrapped line and a colour prompt were measured,
and `mc`, `top`, `vim` and a 256-colour program were not. The alternate screen under `capture-pane -a`: the
attempt to put a pane on it by printing the escape at a shell failed, so the flag's semantics are from the man
page and were not observed working. And whether Shift+drag still selects in the operator's terminal
specifically: the mechanism is measured, the gesture was not driven.
