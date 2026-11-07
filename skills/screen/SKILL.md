---
name: screen
description: Run a program (the letibot head included) on a terminal screen and look at it as text, a style map or a picture, to check a UI change
when_to_use: a change touches what the terminal UI draws or how it responds to keys and mouse, and you want to see it, or a program's full-screen output must be checked
keywords: screenshot, screen, terminal, tui, ui, pty, render, visual, look, status line
---

# screen

The `screen` tool runs a command on a pty of a size you choose, inside this seat, and lets you
type at it and look. Use it for UI work. It is not for scripts: `bash` runs those.

## The loop

1. `start {cmd, cols, rows}`. No `cmd` starts the letibot head on a throwaway session
   (`$LETIBOT_HEAD`, else `letibot-tui`). To see your change, build it first and give the built
   binary as `cmd`. Pick the size the change is about: 100x30 is the default, 80x24 is the
   classic, and a narrow 50 columns shows wrapping bugs.
2. `read {id}` waits until output stops (300 ms quiet, at most 3 s) and returns rows with their
   numbers, the cursor, and a style map. A program that animates never goes quiet: the result
   says output is still arriving. Take it as a frame, or ask again with a larger `idle_ms`.
3. `keys {id, keys}` types. Text is literal. `<Enter>`, `<Esc>`, `<Tab>`, `<BTab>`, `<Up>`,
   `<Down>`, `<Left>`, `<Right>`, `<Home>`, `<End>`, `<PgUp>`, `<PgDn>`, `<Del>`, `<F1>` to
   `<F12>` are keys. `<C-c>` is Ctrl, `<M-x>` is Alt, `<S-Tab>` Shift, and they combine
   (`<C-M-Left>`). `<<` is a literal `<`. A newline in text is Enter.
4. `mouse {id, x, y, button}` clicks at a cell, counted from 0 as `read` shows the rows.
   `move: true` moves, `scroll: "up"` or `"down"` turns the wheel. The program must have turned
   mouse reporting on, or the call says so and sends nothing.
5. `shot {id}` returns a PNG of the screen on the same grid. If the picture did not arrive this
   model cannot see pictures: say so and keep to `read`.
6. `stop {id}` when done. Screens also end with the session, and only a few run at once.

## What to check

Read the text first, because it is exact: the status line, the border, the truncation `…`, the
wrapped row. The style map says what a picture would: `bold`, `faint`, `inverse`, and `fg=` or
`bg=` per run of columns, so a selected row is a run marked `inverse`, not a guess. Use `shot` for
what text cannot say: alignment, blocks, whether a colour reads against its background.

After a change, check the same screen before and after, at two sizes, and after a resize-sensitive
key. Send `<C-c>` or the program's quit key, then `read` to see it end. A program that waits for
a terminal answer (a cursor-position query, an image probe) gets none here, and the head's own
timeouts apply.

## Limits

- Colours are the sixteen palette slots, bold, faint and inverse. Truecolour and 256-colour
  requests are not reproduced, italic and underline are not drawn.
- The picture uses a built-in bitmap face: ASCII, box and block characters, braille, a few
  symbols. Any other glyph is a hollow box, so read the text for what it says.
- The program runs in this seat's boundary: the same files, network and limits as `bash`.
