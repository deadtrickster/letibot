<!-- abstract: `Mouse::Any` turns off the terminal's own link detection and SGR has no Command bit, so a head that reports the mouse must open its own links - and a bare URL linkified after the wrap is linked truncated. -->

# A link is not clickable because the head took the mouse - and where the escapes are wrong

`crates/tui/src/head.rs` sets `mouse: rano::term::Mouse::Any` (`CSI ?1003h` and `?1006h`). Every motion goes to the head, so the terminal never hovers a link, and SGR has no bit for Command, so a cmd+click arrives as a plain click. A TUI that reports the mouse must open its own links; nothing here did until `Action::OpenUrl` and `open_in_editor`.

`App::draw_links` (`crates/tui/src/ui/screen.rs`) runs the writer passes and then reads the frame back for OSC 8 pairs (`app::links::osc8_spans`), recording one `Target::Link` over exactly the cells it covers. The escape and the hit target are then one fact, so no renderer can link something the pointer map does not know.

Rake: a pass that adds OSC 8 to finished rows is right about the bytes and blind about the pointer, so the same function must record the hit. A bare `https://` in prose is still linked after the wrap, which truncates the URL on row one and leaves the continuation dead text; that fix belongs before the wrap, inside rano's markdown renderer.
