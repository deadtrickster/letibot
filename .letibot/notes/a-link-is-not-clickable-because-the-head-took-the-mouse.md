<!-- abstract: MEASURED 2026-10-30: `Mouse::Any` (`?1003h`/`?1006h`) means Ghostty forwards every click and strips Command (SGR has no super bit), so its own OSC 8 detection and cmd+click never run — a link the head drew had no target; also the `http(s)` pass runs after the wrap, so a bare URL broken across rows is linked as a TRUNCATED url on row one and dead text after. -->

# A link is not clickable because the head took the mouse — and where the escapes are wrong

MEASURED 2026-10-30, branch `agent/a-link-you-cannot-click`, from the operator's
*"why links are not clickable and ghostty doesnt detect them either?"*.

## 1. The terminal cannot open a link in this program, by construction

`crates/tui/src/head.rs:1238` sets `mouse: rano::term::Mouse::Any` — `CSI ?1003h`
(all motion) + `CSI ?1006h` (SGR). So:

* every motion is reported to the head, so **Ghostty's own hover detection never runs**
  (no underline, nothing to "detect");
* every click is forwarded, and **the SGR protocol has no bit for the macOS Command
  key**, so a `cmd`+click arrives as a plain click. Ghostty's own report of this
  (discussion #13073) says the same: with mouse reporting on, only `ctrl`+click and
  `shift`+`cmd`+click open a link, because Shift makes Ghostty keep the click.

**So a TUI that enables mouse reporting must open its own links.** Nothing in this
tree did: no `Action::OpenUrl`, no `open`/`xdg-open` anywhere (`app/attachments.rs` is
the only place that runs a desktop tool, and it only *reads* the clipboard).

## 2. The escapes: well formed, except a bare URL the wrap broke

Dumped from `App::screen` with `{:?}` at 80 columns: one row carries
`ESC]8;;URL ESC\` before the visible text and `ESC]8;; ESC\` after it — correct.

* A **markdown link's label that wraps** is already right: rano's span emitters
  reopen at a break what they closed, so each row gets its own pair around the cells
  it shows (measured: 3 rows at width 30, each `\x1b]8;;URL\x1b\\label-fragment\x1b]8;;\x1b\\`).
* A **bare `https://…` in prose** is NOT: the `http(s)` pass (`rano::term::links::link_urls`)
  runs over the finished, already-wrapped rows, so at width 40
  `https://example.com/a-very-long-url-that-…` became
  `\x1b]8;;https://example.com/a-very-long-url-\x1b\\https://example.com/a-very-long-url-…`
  — a **truncated URL** — and the continuation row was dead text. `truncate` closes an
  open link (rano's own test), so a width CUT is fine; a WRAP is not.

**Not fixed**: the honest fix is to linkify before the wrap, which is inside rano's
markdown renderer. A head-side fix would need a second URL grammar (which is what the
package brief forbade), and "does this URL run to the end of the row because it was cut
or because the line ends here" is not decidable from one row.

## 3. `path:line` is invisible to `link_words`

`link_words`'s word is `[alnum /#-_.]` — no `:` — so `src/main.rs:120` arrives as the
word `src/main.rs` and `:120` as a word of its own. The head does the split (its half,
as `ui::render::path_span` says) and `rano::term::links::file_url` decides the URL. The
generalisation rano would need to own it is one character in `link_words`'s word.

## The shape that landed

`App::draw_links` (`crates/tui/src/ui/screen.rs`) runs the three writer passes and then
**reads the frame back** for OSC 8 pairs (`app::links::osc8_spans`, over
`app::pointer::pieces` — the head's one escape walker), recording one `Target::Link`
per link over exactly the cells it covers. That is the `scan_hints` pattern: the escape
and the click target are the same fact, so no renderer can link something the hit map
does not know about. A click opens `file://` in the editor pane (`open_in_editor`, at
the `:N`) and everything else as `Action::OpenUrl`, run by the driver.

## The rake

**A frame's escape is a claim about a target, and the frame is the only place that
knows both.** A pass that adds OSC 8 to finished rows is fine for the bytes and blind
for the pointer — which is why the same function must record the hit. And when a
terminal feature is "handled by the terminal", check who owns the input: mouse
reporting turns the terminal's own link detection off, silently, and nothing in this
tree said so.
