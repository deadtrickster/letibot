//! **A link on the frame is a target** — the operator, 2026-10-30: *"why links are not clickable
//! and ghostty doesnt detect them either?"*.
//!
//! The answer this file pins: the head takes the mouse (`rano::term::Mouse::Any`), so Ghostty
//! forwards the click instead of opening the link — and a link the head drew had no target, so
//! the click did nothing. Here it opens, the URL is the one the frame drew, the copy path is
//! still the visible text, and a row with no link records nothing.

use super::*;
use std::path::PathBuf;

fn replied(text: &str) -> App {
    let mut a = app();
    a.wiring.workspace = "/w".into();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("r.1", "assistant"),
    )));
    a.record_item(
        "r.1",
        TranscriptItem::Assistant {
            text: text.into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    a.set_features(rano::term::Features::ALL);
    a
}

/// The `Hit` of the link whose URL is `url`, on the frame just drawn. A `file://` URL carries
/// this machine's name, so a test may name only its tail.
fn link_hit(a: &App, url: &str) -> Hit {
    a.hits
        .iter()
        .rev()
        .find(|h| {
            h.target == Target::Link
                && h.stops.iter().any(|(_, k)| {
                    a.link_targets
                        .get(*k)
                        .is_some_and(|u| u == url || u.ends_with(url))
                })
        })
        .cloned()
        .unwrap_or_else(|| panic!("no link for {url:?} — the frame drew {:?}", a.link_targets))
}

/// The cells a hit covers, read off the frame the way the operator sees them.
fn cells_under(frame: &[String], hit: &Hit) -> String {
    crate::app::keyhints::cells(&frame[hit.rect.y])[hit.rect.x..hit.rect.x + hit.rect.w]
        .iter()
        .filter(|c| **c != '\0')
        .collect()
}

fn click(a: &mut App, hit: &Hit) -> Option<Action> {
    a.key(Key::Click {
        x: (hit.rect.x + 1) as u16,
        y: hit.rect.y as u16,
    })
}

/// A workspace on disk, removed when the test ends.
struct Workspace(PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn workspace(tag: &str) -> Workspace {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-links-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let text: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    std::fs::write(dir.join("src/main.rs"), text).unwrap();
    Workspace(dir)
}

/// **A row with a URL in it records a hit, and a click opens that exact URL** — the hit covers
/// the URL's own cells and nothing else, so the prose around it is still the conversation's.
#[test]
fn a_click_on_a_link_opens_the_url_the_frame_drew() {
    let mut a = replied("Read https://example.com/x/guide first.");
    let frame = a.screen(80, 20);
    let hit = link_hit(&a, "https://example.com/x/guide");
    assert_eq!(hit.rect.h, 1);
    assert_eq!(cells_under(&frame, &hit), "https://example.com/x/guide");
    assert_eq!(
        click(&mut a, &hit),
        Some(Action::OpenUrl("https://example.com/x/guide".into())),
        "a click on a link opens the URL the frame drew"
    );

    // **And a click beside it is not the link's** — the prose is text, selected and not clicked.
    let mut b = replied("Read https://example.com/x/guide first.");
    b.screen(80, 20);
    let hit = link_hit(&b, "https://example.com/x/guide");
    assert_eq!(
        b.key(Key::Click {
            x: (hit.rect.x - 1) as u16,
            y: hit.rect.y as u16
        }),
        None,
        "the words in front of a link are not the link"
    );

    // **The pointer finds it without a click** — the head cannot leave detection to the terminal
    // (it takes the mouse), so the link lights its own cells under the pointer.
    let mut c = replied("Read https://example.com/x/guide first.");
    c.screen(80, 20);
    let hit = link_hit(&c, "https://example.com/x/guide");
    c.hover_at((hit.rect.x + 1) as u16, hit.rect.y as u16);
    let lit = c
        .hovered
        .clone()
        .expect("the link lights under the pointer");
    assert_eq!(
        lit.rows,
        vec![(hit.rect.y, Some((hit.rect.x, hit.rect.x + hit.rect.w)))]
    );
}

/// **The copy path still copies the words** — a selection over a linked row carries the URL's
/// text and none of the escape bytes that made it a link.
#[test]
fn a_rows_copied_text_carries_no_escape_bytes() {
    let mut a = replied("Read https://example.com/x/guide first.");
    let frame = a.screen(80, 20);
    let hit = link_hit(&a, "https://example.com/x/guide");
    let y = hit.rect.y as u16;
    assert!(frame[hit.rect.y].contains('\u{1b}'), "the row is linked");
    a.press_at(0, y);
    a.drag_to(79, y);
    a.release();
    let copied = a.clipboard_out.clone().expect("the selection was copied");
    assert!(!copied.contains('\u{1b}'), "{copied:?}");
    assert!(
        copied.contains("https://example.com/x/guide"),
        "the URL's own text is what was copied: {copied:?}"
    );
    assert!(
        copied.contains("Read ") && copied.contains(" first."),
        "and the words around it: {copied:?}"
    );
}

/// **A wrapped link is one well-formed pair per row** — rano reopens at a break what it closed
/// at the end of the last row, so no row ends with an opener whose closer is on the next row,
/// and each row's pair covers exactly the cells that row shows.
#[test]
fn a_wrapped_link_is_one_well_formed_pair_per_row() {
    const URL: &str = "https://example.com/g";
    let mut a = replied(
        "[a label long enough to wrap across the width of a narrow terminal](https://example.com/g)",
    );
    let frame = a.screen(30, 20);
    let rows: Vec<&String> = frame.iter().filter(|l| l.contains("\u{1b}]8;")).collect();
    assert!(rows.len() >= 3, "the label wraps: {frame:#?}");
    for row in &rows {
        // One opener and one closer, in that order, and nothing left open.
        assert_eq!(
            row.matches("\u{1b}]8;;").count(),
            2,
            "one pair per row: {row:?}"
        );
        let spans = crate::app::osc8_spans(row);
        assert_eq!(spans.len(), 1, "{row:?}");
        let (from, to, url) = &spans[0];
        assert_eq!(url, URL, "{row:?}");
        assert_eq!(
            to - from,
            crate::ui::render::visible_width(row),
            "the pair covers the whole visible row: {row:?}"
        );
    }
    // And each row is a target, all of them the same URL.
    let hits: Vec<&Hit> = a.hits.iter().filter(|h| h.target == Target::Link).collect();
    assert_eq!(hits.len(), rows.len(), "{:?}", a.link_targets);
    assert!(
        a.link_targets.iter().all(|u| u == URL),
        "{:?}",
        a.link_targets
    );
}

/// **A row with no link records no link target** — the assertion `ghlinks`/`standing` keep from
/// the other side, here as the hit map's own.
#[test]
fn a_row_with_no_link_records_no_link_target() {
    let mut a = replied("Nothing to see here, just words and 12:30.");
    let frame = a.screen(80, 20);
    assert!(!frame.iter().any(|l| l.contains("\u{1b}]8;")), "{frame:#?}");
    assert!(a.link_targets.is_empty(), "{:?}", a.link_targets);
    assert!(!a.hits.iter().any(|h| h.target == Target::Link));
}

/// **A URL in the composer is text, not a link** — a click in the middle of one the operator is
/// typing belongs to the caret, so the chrome is where a link stops being a target.
#[test]
fn a_url_in_the_composer_is_text_and_not_a_link() {
    let mut a = app();
    a.set_features(rano::term::Features::ALL);
    typed(&mut a, "look at https://example.com/x");
    let frame = a.screen(80, 20);
    assert!(
        frame.iter().any(|l| l.contains("https://example.com/x")),
        "the composer draws the URL: {frame:#?}"
    );
    assert!(a.link_targets.is_empty(), "{:?}", a.link_targets);
    assert!(!a.hits.iter().any(|h| h.target == Target::Link));
}

/// **`path:line` in a reply is a link, and it opens the file at the line** — through the editor
/// pane this head already opens files with, which is the door an edit row and a note use.
#[test]
fn a_path_and_line_in_a_reply_opens_the_file_there() {
    let ws = workspace("pathline");
    let mut a = app();
    a.wiring.workspace = ws.0.to_string_lossy().into_owned();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("r.1", "assistant"),
    )));
    a.record_item(
        "r.1",
        TranscriptItem::Assistant {
            text: "the bug is in src/main.rs:12 — look".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    a.set_features(rano::term::Features::ALL);
    let frame = a.screen(100, 20);
    let hit = link_hit(&a, "/src/main.rs:12");
    assert_eq!(cells_under(&frame, &hit), "src/main.rs:12");
    let url = a.link_targets[hit.stops[0].1].clone();
    let (path, line) = crate::app::links::file_target(&url).expect("a file URL");
    assert!(path.ends_with("/src/main.rs"), "{path}");
    assert_eq!(line, 12);
    assert_eq!(click(&mut a, &hit), None, "a file is not handed to `open`");
    assert!(a.edit_pane.is_some(), "the file opened in the pane");
    assert_eq!(
        a.edit_pane.as_ref().map(|p| p.focused),
        Some(true),
        "and the pane has the keyboard"
    );
}

/// **A `file://` URL is read back into a path and a line** — rano's `uri_path` decoded, and this
/// head's `:N` taken off the end.
#[test]
fn a_file_url_is_read_back_into_a_path_and_a_line() {
    let (p, l) = crate::app::links::file_target("file://host/w/my%20file.txt:7").unwrap();
    assert_eq!(p, "/w/my file.txt");
    assert_eq!(l, 7);
    let (p, l) = crate::app::links::file_target("file://host/w/src/main.rs").unwrap();
    assert_eq!(p, "/w/src/main.rs");
    assert_eq!(l, 1, "no line is the first line");
    assert_eq!(crate::app::links::file_target("https://x/y"), None);
}

/// **A URL with a port is a URL, not a path** — `link_path_lines` runs after `link_urls`, so
/// `https://example.com/x:8080` is one link and not a file called `example.com/x`.
#[test]
fn a_url_with_a_port_is_not_read_as_a_path_and_line() {
    let mut a = replied("at https://example.com/x:8080 ok");
    let frame = a.screen(80, 20);
    assert_eq!(
        a.link_targets,
        vec!["https://example.com/x:8080".to_string()],
        "{frame:#?}"
    );
}
