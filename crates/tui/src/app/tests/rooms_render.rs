//! What a message is drawn as: a block - header, body, quote, pictures, dividers - and what the
//! layer decided before the renderer saw it. Proved here: `ui::chat` draws a `Stream` and nothing
//! else, and `app::chat_adapter` is the one place a room's `Msg` is read.

use super::rooms::{history, msg as wire_msg, reply, rooms_reply};
use super::*;
use crate::app::rooms::{Attach, ChatRef, Msg};
use crate::ui::chat::colour::paint;
use crate::ui::chat::{Block, ChatCtx, Picture, Pictures, chat_blocks, hue};
use crate::ui::render::{RenderConfig, row_strings, visible_width};
use letibot_sessionlog::pane::{Cell, Divider, Mention, Role, Stream, StreamEntry};
use std::collections::BTreeMap;

/// A fixed afternoon. Every instant here is derived from it and so is the clock the adapter is
/// handed, so no assertion depends on when the suite runs.
const TS: u64 = 1_793_735_794_000;

fn message(seq: u64, author: &str, text: &str) -> Msg {
    Msg {
        seq,
        ts: TS + seq * 60_000,
        author: author.into(),
        kind: "session".into(),
        to: None,
        thread: None,
        refs: Vec::new(),
        attachments: Vec::new(),
        text: text.into(),
    }
}

/// The same message, carrying one 1280x720 picture the body names.
fn pictured(seq: u64, text: &str) -> Msg {
    let mut m = message(seq, "s-a", text);
    m.attachments = vec![Attach {
        id: "abc123".into(),
        mime: "image/png".into(),
        name: "shot.png".into(),
        bytes: 4096,
        w: Some(1280),
        h: Some(720),
    }];
    m
}

fn names(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(h, n)| (h.to_string(), n.to_string()))
        .collect()
}

fn chan() -> crate::app::rooms::Chan {
    crate::app::rooms::Chan::Room("host".into())
}

fn view(msgs: Vec<Msg>) -> crate::app::rooms::RoomsView {
    let mut v = crate::app::rooms::RoomsView {
        heard: true,
        ..Default::default()
    };
    v.merge("host", msgs);
    v
}

/// One channel as the layer answers it: the adapter's stream, drawn against a clock two minutes on.
fn stream(msgs: Vec<Msg>, named: &[(&str, &str)]) -> Stream {
    let v = view(msgs);
    crate::app::chat_adapter::stream(&v, &chan(), &names(named), "operator", TS + 120_000)
}

/// A message entry, the dividers aside: what a test about messages is about.
fn entries(s: &Stream) -> Vec<&StreamEntry> {
    s.entries.iter().filter(|e| !e.is_divider()).collect()
}

fn drawing(images: bool) -> RenderConfig {
    RenderConfig {
        color: false,
        images,
        ..RenderConfig::default()
    }
}

/// No picture in the cache, which is P4's stub and every test that is not about pictures.
fn absent(_: &str) -> Option<Picture> {
    None
}

fn known(_: &str) -> Option<Picture> {
    Some((7, 1280, 720))
}

fn blobs() -> BTreeMap<String, crate::ui::chat::Blob> {
    let v = view(vec![pictured(1, "")]);
    crate::app::chat_adapter::blobs(&v, &chan())
}

fn drawn(
    s: &Stream,
    width: usize,
    cfg: &RenderConfig,
    cache: Pictures<'_>,
    blobs: &BTreeMap<String, crate::ui::chat::Blob>,
) -> Vec<Block> {
    let ctx = ChatCtx {
        width,
        cfg,
        picture: cache,
        blobs,
        selected: None,
        highlight: None,
    };
    chat_blocks(s, &ctx)
}

fn blocks(s: &Stream, width: usize, cfg: &RenderConfig) -> Vec<Block> {
    drawn(s, width, cfg, &absent, &BTreeMap::new())
}

fn rows(blocks: &[Block], cfg: &RenderConfig) -> Vec<String> {
    blocks
        .iter()
        .flat_map(|b| row_strings(&b.lines, cfg.palette()))
        .collect()
}

/// Every span of a block, as `(text, the roles it means)`.
fn spans(b: &Block) -> Vec<(String, Vec<rano::style::Role>)> {
    b.lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| (s.content.clone(), s.style.roles().collect()))
        .collect()
}

fn painted(b: &Block, needle: &str, want: rano::style::Role) -> bool {
    spans(b)
        .iter()
        .any(|(text, roles)| text.contains(needle) && roles.contains(&want))
}

fn row_with<'a>(rows: &'a [String], needle: &str) -> &'a str {
    rows.iter()
        .find(|r| r.contains(needle))
        .unwrap_or_else(|| panic!("no row has {needle:?}:\n{}", rows.join("\n")))
}

/// The block an entry drew, by its id: a stream's first block is usually the day's rule.
fn entry_block<'a>(drew: &'a [Block], id: &str) -> &'a Block {
    drew.iter()
        .find(|b| b.id == id)
        .unwrap_or_else(|| panic!("no block is entry {id:?}"))
}

/// A fence is drawn by rano's grammar - the language named, the tokens coloured - and a line
/// longer than the column is cut, never wrapped: a wrapped code line is code that no longer runs.
#[test]
fn a_code_fence_draws_highlighted_and_unwrapped() {
    let long = "x".repeat(200);
    let body = format!(
        "the run:\n\n```rust\nfn main() {{ let answer: u32 = 42; }}\n```\n\n```text\n{long}\n```\n"
    );
    let s = stream(vec![message(1, "s-a", &body)], &[]);
    let cfg = drawing(false);
    let drawn = blocks(&s, 80, &cfg);
    let out = rows(&drawn, &cfg);
    assert!(
        out.iter().any(|r| r.contains("┌─ rust")),
        "the fence names its grammar: {}",
        out.join("\n")
    );
    assert!(
        painted(entry_block(&drawn, "1"), "fn", rano::style::Role::Keyword)
            || painted(entry_block(&drawn, "1"), "u32", rano::style::Role::TypeName)
            || painted(entry_block(&drawn, "1"), "42", rano::style::Role::NumberLit),
        "a fence is coloured by its grammar: {:?}",
        spans(entry_block(&drawn, "1"))
    );
    let held = out.iter().filter(|r| r.contains("xxxx")).count();
    assert_eq!(held, 1, "a long code line is one row: {}", out.join("\n"));
    for r in &out {
        assert!(
            visible_width(r) <= 80,
            "{} columns: {r:?}",
            visible_width(r)
        );
    }
}

/// Inline markdown is styled, not spelled: the asterisks are markers, and a reader who sees them is
/// reading the wire instead of the answer.
#[test]
fn inline_markdown_is_styled_not_shown_as_asterisks() {
    let s = stream(
        vec![message(1, "s-a", "this is **bold**, *italic* and `code`")],
        &[],
    );
    let cfg = drawing(false);
    let drawn = blocks(&s, 80, &cfg);
    let out = rows(&drawn, &cfg);
    let body = row_with(&out, "this is");
    assert!(!body.contains('*'), "{body:?}");
    assert!(!body.contains('`'), "{body:?}");
    let bold = entry_block(&drawn, "1")
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .any(|s| s.content.contains("bold") && s.style.attrs.contains(rano::style::Attrs::BOLD));
    assert!(bold, "bold is bold: {:?}", spans(entry_block(&drawn, "1")));
    assert!(
        painted(entry_block(&drawn, "1"), "code", rano::style::Role::Code),
        "a code span is code: {:?}",
        spans(entry_block(&drawn, "1"))
    );
}

/// Two messages from one speaker inside ten minutes are one run: the second drops its header and
/// keeps only its own time, so a scan down the column finds the speakers, not every message.
#[test]
fn two_messages_from_one_author_within_ten_minutes_share_a_header() {
    let s = stream(
        vec![
            message(1, "s-a", "who has the disk?"),
            message(2, "s-a", "the second one"),
        ],
        &[("s-a", "gatekeeper")],
    );
    let said = entries(&s);
    assert_eq!(said.len(), 2, "two messages: {s:?}");
    assert!(!said[0].head.is_empty(), "the run opens with a header");
    assert!(
        said[1].head.is_empty(),
        "a continuation has no header: {:?}",
        said[1].head
    );
    assert!(
        said[1].time.is_some(),
        "a continuation keeps its own time, and the header above does not carry it"
    );
    let cfg = drawing(false);
    let out = rows(&blocks(&s, 80, &cfg), &cfg);
    assert_eq!(
        out.iter().filter(|r| r.contains("gatekeeper")).count(),
        1,
        "one header for the run: {}",
        out.join("\n")
    );
    // The continuation's time rides in the gutter of its first row.
    let second = row_with(&out, "the second one");
    assert!(
        second
            .chars()
            .take(5)
            .all(|c| c.is_ascii_digit() || c == ':'),
        "the gutter holds a time: {second:?}"
    );
}

/// A day boundary ends a run whatever the clock says, and draws the rule a reader orients by.
#[test]
fn a_day_boundary_breaks_a_run_and_draws_a_separator() {
    let mut later = message(2, "s-a", "the next day");
    later.ts = TS + 2 * 24 * 60 * 60 * 1000;
    let s = stream(
        vec![message(1, "s-a", "today's news"), later],
        &[("s-a", "gatekeeper")],
    );
    let days: Vec<&StreamEntry> = s
        .entries
        .iter()
        .filter(|e| e.divider == Some(Divider::Today))
        .collect();
    assert_eq!(days.len(), 2, "one rule per day the window touches");
    assert!(
        days.iter().all(|d| !d.text.is_empty()),
        "a divider says which day it is"
    );
    assert!(
        entries(&s).iter().all(|e| !e.head.is_empty()),
        "two days are two runs, so the header comes back"
    );
    let cfg = drawing(false);
    let out = rows(&blocks(&s, 80, &cfg), &cfg);
    assert_eq!(
        out.iter().filter(|r| r.contains("── ")).count(),
        2,
        "two rules: {}",
        out.join("\n")
    );
}

/// The colour is the handle's, not the name's: a session that is renamed, or a message with
/// somebody new above it, keeps the colour it was read in.
#[test]
fn the_same_handle_always_gets_the_same_colour() {
    assert_eq!(hue("s-1793"), hue("s-1793"));
    assert_eq!(hue("s-1793"), hue("S-1793"), "a handle is case-insensitive");
    assert!(hue("s-1793") < 8);
    // Two names for one handle are two rows in one colour, and the operator is not a hue at all.
    let one = stream(vec![message(1, "s-1793", "hi")], &[("s-1793", "one")]);
    let two = stream(vec![message(1, "s-1793", "hi")], &[("s-1793", "two")]);
    let cell = |s: &Stream| entries(s).last().expect("an entry").head[0].clone();
    assert_eq!(cell(&one).role, Some(Role::Hue(hue("s-1793"))));
    assert_eq!(cell(&one).role, cell(&two).role, "a rename keeps the hue");
    let me = stream(vec![message(1, "operator", "hi")], &[]);
    assert_eq!(cell(&me).role, Some(Role::Strong));
    // A hue is a colour a reader can tell from the next one, and the hash spreads them.
    let hues: std::collections::BTreeSet<u8> = ["s-1", "s-2", "s-3", "s-4", "cc@letibot"]
        .iter()
        .map(|h| hue(h))
        .collect();
    assert!(hues.len() > 1, "the hash spreads: {hues:?}");
}

/// A session's mention is its name: the coordinator stores `@s-<id>`, and that is a key, not
/// something an operator reads.
#[test]
fn a_mention_of_a_session_id_draws_its_name() {
    let s = stream(
        vec![message(1, "s-b", "ping @s-1793 about the disk")],
        &[("s-1793", "fix-room-push")],
    );
    let cfg = drawing(false);
    let out = rows(&blocks(&s, 80, &cfg), &cfg);
    let row = row_with(&out, "about the disk");
    assert!(row.contains("@fix-room-push"), "{row:?}");
    assert!(!row.contains("@s-1793"), "the id is not drawn: {row:?}");
}

/// A message that names the reader says so in its header, and the token itself is the one mention
/// no speaker's hue may quiet.
#[test]
fn a_mention_of_the_operator_marks_the_header() {
    let s = stream(
        vec![message(
            1,
            "s-b",
            "@operator can you look at the push frame?",
        )],
        &[],
    );
    assert_eq!(
        entries(&s)[0].head[0],
        Cell::role("!", Role::Warning),
        "the header carries the mark"
    );
    assert_eq!(
        s.mentions.get("@operator").map(|m| m.me),
        Some(true),
        "the token is the reader's own"
    );
    let cfg = drawing(false);
    let drawn = blocks(&s, 80, &cfg);
    let out = rows(&drawn, &cfg);
    assert!(
        row_with(&out, "s-b").contains('!'),
        "the mark is on the header, not on the body: {}",
        out.join("\n")
    );
    // The mark is `warning`: a message that names the reader is worth knowing and is not a
    // decision they owe. (The token itself is rano's mention renderer's, which draws a mention of
    // the reader in the attention register, and that is not this head's to move.)
    assert!(
        painted(entry_block(&drawn, "1"), "!", rano::style::Role::Warning),
        "the mark is news, not a demand: {:?}",
        spans(entry_block(&drawn, "1"))
    );
}

/// The unread divider falls before the first message the reader had not seen when they opened the
/// channel, and nowhere else: it is the one row whose place is a fact about the reader.
#[test]
fn the_unread_divider_sits_before_the_first_unseen_message() {
    let mut v = view(vec![
        message(1, "s-a", "before"),
        message(2, "s-a", "after"),
        message(3, "s-b", "later"),
    ]);
    v.opened_seen.insert("host".into(), 1);
    let s = crate::app::chat_adapter::stream(&v, &chan(), &names(&[]), "operator", TS);
    let at = s
        .entries
        .iter()
        .position(|e| e.divider == Some(Divider::Unread))
        .expect("the divider is drawn");
    assert_eq!(at, 2, "after the message the reader had seen: {s:?}");
    assert_eq!(s.entries[at].text, "unread");
    assert_eq!(s.entries[at + 1].id.as_deref(), Some("2"));
    let cfg = drawing(false);
    let out = rows(&blocks(&s, 80, &cfg), &cfg);
    assert!(
        out.iter().any(|r| r.contains("── unread ")),
        "{}",
        out.join("\n")
    );
}

/// A reply says what it answers: the number, the speaker and as much of the line as a quote has
/// room for - above the answer, because it is read first.
#[test]
fn a_reply_draws_a_quote_of_the_message_it_answers() {
    let mut answer = message(5, "s-b", "no, the other one");
    answer.thread = Some(3);
    let s = stream(
        vec![message(3, "s-a", "which disk?"), answer],
        &[("s-a", "gatekeeper")],
    );
    let quote = entries(&s)
        .last()
        .and_then(|e| e.quote.clone())
        .expect("the answer quotes what it answers");
    assert!(quote.contains("↳ 3"), "{quote:?}");
    assert!(quote.contains("gatekeeper"), "{quote:?}");
    assert!(quote.contains("which disk?"), "{quote:?}");
    let cfg = drawing(false);
    let out = rows(&blocks(&s, 80, &cfg), &cfg);
    let at = |needle: &str| {
        out.iter()
            .position(|r| r.contains(needle))
            .unwrap_or(usize::MAX)
    };
    assert!(at("↳ 3") < at("no, the other one"), "{}", out.join("\n"));
}

/// A terminal that speaks no pictures protocol still says the message carried one, what it was
/// called and that it is worth opening - one row, never a hole and never a zero.
#[test]
fn a_picture_without_a_protocol_is_one_row_naming_it() {
    let v = view(vec![pictured(1, "here it is ![shot](blob:abc123)")]);
    let s = crate::app::chat_adapter::stream(&v, &chan(), &names(&[]), "operator", TS);
    let cfg = drawing(false);
    assert!(!cfg.images, "the terminal speaks no pictures");
    // P4's cache is the stub on this build: the bytes are not here, and the row still says the
    // size, because the wire carried the store's measurement of the picture's own header.
    let drew = drawn(&s, 80, &cfg, &absent, &blobs());
    let out = rows(&drew, &cfg);
    let row = row_with(&out, "picture");
    assert!(row.contains("1280x720"), "{row:?}");
    assert!(row.contains("shot.png"), "{row:?}");
    assert!(row.contains("enter opens"), "{row:?}");
    assert!(
        !out.join("\n").contains("blob:abc123"),
        "the reference is the picture, not a word in it: {}",
        out.join("\n")
    );
    assert_eq!(
        out.iter().filter(|r| r.contains("[picture")).count(),
        1,
        "one row: {}",
        out.join("\n")
    );
    // And with the bytes here the same row is drawn: one fact, one spelling.
    let held = rows(&drawn(&s, 80, &cfg, &known, &blobs()), &cfg);
    assert_eq!(row_with(&held, "picture"), row);
}

/// Where the terminal speaks kitty, the picture is placeholder rows the terminal fills - never more
/// than a dozen, because a picture taller than that is somebody else's whole screen.
#[test]
fn a_picture_with_kitty_draws_placeholder_rows() {
    let v = view(vec![pictured(1, "![shot](blob:abc123)")]);
    let s = crate::app::chat_adapter::stream(&v, &chan(), &names(&[]), "operator", TS);
    let cfg = drawing(true);
    assert!(cfg.images, "this case is the kitty one");
    let out = rows(&drawn(&s, 80, &cfg, &known, &blobs()), &cfg);
    let marks: Vec<&String> = out.iter().filter(|r| r.contains('\u{10EEEE}')).collect();
    assert!(marks.len() >= 2, "a 1280x720 picture takes rows: {out:?}");
    assert!(marks.len() <= 12, "capped at twelve rows: {}", marks.len());
}

/// A URL on the frame is a link when the terminal was told it speaks them, and only then: a
/// sequence a terminal does not understand is noise on every row that carried it.
#[test]
fn a_link_is_an_osc8_link_when_links_are_on() {
    let mut a = app();
    a.set_features(rano::term::Features::ALL);
    a.wiring.workspace = "/w".into();
    a.clock(TS);
    a.command("rooms");
    a.take_actions();
    reply(&mut a, rooms_reply(&[("host", 2, 1, 0)]));
    reply(
        &mut a,
        history(
            "host",
            vec![wire_msg(
                1,
                "host",
                "s-a",
                "session",
                "read https://example.com/x and crates/tui/src/ui/chat/mod.rs:12",
            )],
        ),
    );
    let all = a.screen(100, 24).join("\n");
    assert!(
        all.contains("\u{1b}]8;;https://example.com/x"),
        "the URL is a link: {all:?}"
    );
    a.set_features(rano::term::Features::default());
    let plain = a.screen(100, 24).join("\n");
    assert!(
        !plain.contains("\u{1b}]8;;"),
        "no terminal was told it speaks links: {plain:?}"
    );
    assert!(plain.contains("example.com/x"), "{plain:?}");
}

/// A row is a row at eighty columns and at forty: everything a body can hold - an unbreakable word,
/// a table, a list, a fence, a URL and a picture that is only named - is cut to the column, because
/// a row wider than the frame scrolls the frame it was drawn in.
#[test]
fn every_block_fits_eighty_and_forty_columns() {
    let body = format!(
        "## a heading\n\n{} look at https://example.com/{}\n\n\
         | column one | column two |\n|---|---|\n| a long cell in the table | 42 |\n\n\
         - a list item that runs past the edge of any ordinary column\n\
         - a second one\n\n\
         > quoted prose that also runs on for a while\n\n\
         ```rust\nfn main() {{ println!(\"{}\"); }}\n```\n\n\
         ![wide](blob:abc123)",
        "unbreakable".repeat(12),
        "x".repeat(80),
        "y".repeat(120)
    );
    let mut system = message(2, "todo", "done: write the doc");
    system.kind = "system".into();
    system.refs = vec![ChatRef {
        kind: "todo".into(),
        id: "s-a/write the doc".into(),
    }];
    let mut v = view(vec![pictured(1, &body), system]);
    v.opened_seen.insert("host".into(), 0);
    let s = crate::app::chat_adapter::stream(&v, &chan(), &names(&[]), "operator", TS);
    for width in [80, 40] {
        for images in [false, true] {
            let cfg = drawing(images);
            let drew = drawn(&s, width, &cfg, &absent, &BTreeMap::new());
            for (n, b) in drew.iter().enumerate() {
                for r in row_strings(&b.lines, cfg.palette()) {
                    assert!(
                        visible_width(&r) <= width,
                        "block {n} at {width}: {} columns: {r:?}",
                        visible_width(&r)
                    );
                }
            }
        }
    }
}

/// A message is somebody else's bytes. A control character in one is an instruction to the
/// operator's terminal, so it is stripped before anything measures or draws the text.
#[test]
fn a_control_byte_in_a_message_never_reaches_the_terminal() {
    let s = stream(
        vec![message(1, "s-a", "clean\u{1b}[2J\u{7}line and\ttabbed")],
        &[],
    );
    let cfg = drawing(false);
    let out = rows(&blocks(&s, 80, &cfg), &cfg);
    assert!(out.join("\n").contains("clean"), "{}", out.join("\n"));
    for r in &out {
        assert!(
            !r.chars().any(char::is_control),
            "a control character was drawn: {r:?}"
        );
    }
}

/// A body is markdown through rano: its headings keep their level, its lists keep their bullets,
/// and none of it is the wrapped plain text the panes drew before this package.
#[test]
fn a_stream_entry_body_draws_through_rano_markdown() {
    let mut s = Stream::default();
    s.entries.push(StreamEntry {
        id: Some("1".into()),
        body: Some("# Findings\n\n- one\n- two\n\n> quoted\n".into()),
        ..StreamEntry::default()
    });
    let cfg = drawing(false);
    let drawn = blocks(&s, 80, &cfg);
    let out = rows(&drawn, &cfg);
    assert!(
        row_with(&out, "Findings").contains("# Findings"),
        "a heading keeps its level for the monochrome reader: {}",
        out.join("\n")
    );
    assert!(
        painted(
            entry_block(&drawn, "1"),
            "Findings",
            rano::style::Role::Heading
        ),
        "a heading is a heading: {:?}",
        spans(entry_block(&drawn, "1"))
    );
    assert!(
        row_with(&out, "one").contains("· one"),
        "{}",
        out.join("\n")
    );
    assert!(
        out.iter().any(|r| r.contains("quoted")),
        "{}",
        out.join("\n")
    );
}

/// A hue is a palette position, not a colour: the theme decides what it looks like, and both the
/// dark and the light table answer one that can be read on that background.
#[test]
fn a_hue_maps_to_a_theme_colour_on_dark_and_light() {
    use rano::style::{Hue, Look, Palette as Table};
    let (mut dark, mut light): (Vec<Look>, Vec<Look>) = (Vec::new(), Vec::new());
    for h in 0..8u8 {
        let p = paint(h);
        let (d, l) = (Table::Colour.look(p), Table::Light.look(p));
        // The eight foregrounds are slots 1..=6 - the terminal's own colours, never slot 0 or 7,
        // which are a background and its opposite on one of the two themes.
        for look in [d, l] {
            match look.fg {
                Some(Hue::Slot(n)) => assert!(
                    (1..=6).contains(&n),
                    "hue {h} is slot {n}, which vanishes on one background"
                ),
                other => panic!("hue {h} has no theme colour: {other:?}"),
            }
        }
        assert!(
            !dark.contains(&d) && !light.contains(&l),
            "two hues would read as one: {h}"
        );
        dark.push(d);
        light.push(l);
    }
}

/// A layer's mention token reads as the name it was given, in the hue it was given: the host
/// rewrites the token while drawing, so the markdown a layer handed over is never edited.
#[test]
fn a_mention_token_draws_as_its_name_in_its_hue() {
    let mut s = Stream::default();
    s.entries.push(StreamEntry {
        id: Some("1".into()),
        body: Some("ask @s-1793 about it".into()),
        ..StreamEntry::default()
    });
    s.mentions.insert(
        "@s-1793".into(),
        Mention {
            text: Some("@fix-room-push".into()),
            hue: Some(3),
            me: false,
        },
    );
    let cfg = drawing(false);
    let drawn = blocks(&s, 80, &cfg);
    let out = rows(&drawn, &cfg);
    let row = row_with(&out, "about it");
    assert!(row.contains("@fix-room-push"), "{row:?}");
    assert!(!row.contains("@s-1793"), "{row:?}");
    assert!(
        painted(entry_block(&drawn, "1"), "@fix-room-push", paint(3)),
        "the name is drawn in its hue: {:?}",
        spans(entry_block(&drawn, "1"))
    );
    // The role table itself: a pane role is a meaning the head already has a paint for.
    let role = |r| crate::ui::severity::role(r);
    assert_eq!(role(Some(Role::Hue(3))), paint(3));
    assert_eq!(role(None), rano::style::Role::Plain);
    assert_eq!(role(Some(Role::Faint)), rano::style::Role::Faint);
}

/// A header holds its facts at the right edge, and a fact that does not fit entire goes under the
/// body: a ref cut in half says nothing, and the speaker's name must not be what pays for it.
#[test]
fn a_fact_that_does_not_fit_the_header_goes_under_the_body() {
    let mut s = Stream::default();
    s.entries.push(StreamEntry {
        id: Some("1".into()),
        time: Some("14:02".into()),
        head: vec![Cell::role("gatekeeper", Role::Hue(0))],
        facts: vec![
            Cell::role("#212", Role::Faint),
            Cell::role("[todo s-a/write the doc]", Role::Faint),
        ],
        body: Some("merged".into()),
        ..StreamEntry::default()
    });
    let cfg = drawing(false);
    let out = rows(&blocks(&s, 36, &cfg), &cfg);
    let header = row_with(&out, "gatekeeper");
    assert!(header.contains("#212"), "{header}");
    assert!(
        !header.contains("[todo"),
        "the ref is not in the header: {header}"
    );
    assert!(
        out.iter().any(|r| r.contains("[todo s-a/write the doc]")),
        "the ref is whole, under the body: {}",
        out.join("\n")
    );
    for r in &out {
        assert!(visible_width(r) <= 36, "{}", visible_width(r));
    }
}

/// The cursor and the search are the pane's to move and the renderer's to draw: the entry under the
/// cursor is picked out whole, and every hit of the needle is painted as a match - which is what
/// P4's keys and P4's `/search` land on.
#[test]
fn the_entry_under_the_cursor_and_the_searches_hits_are_picked_out() {
    let s = stream(
        vec![
            message(1, "s-a", "the disk is full"),
            message(2, "s-b", "which disk?"),
        ],
        &[],
    );
    let cfg = drawing(false);
    let none = BTreeMap::new();
    let ctx = ChatCtx {
        width: 80,
        cfg: &cfg,
        picture: &absent,
        blobs: &none,
        selected: Some("2"),
        highlight: Some("disk"),
    };
    let drew = chat_blocks(&s, &ctx);
    let picked = |id: &str| {
        entry_block(&drew, id)
            .lines
            .first()
            .is_some_and(|l| l.style.roles().any(|r| r == rano::style::Role::Selected))
    };
    assert!(picked("2"), "the entry under the cursor is picked out");
    assert!(!picked("1"), "and no other one is");
    for id in ["1", "2"] {
        assert!(
            painted(entry_block(&drew, id), "disk", rano::style::Role::Match),
            "every hit of the needle is a match: {:?}",
            spans(entry_block(&drew, id))
        );
    }
}

/// The seam this package exists for: what a message *means* is the adapter's to decide, so the
/// renderer and the pane draw a stream and never read a `Msg`. A plugin answering the same block
/// gets the same drawing, and a field added to a message cannot start being read in two places.
#[test]
fn the_adapter_is_the_only_place_that_reads_msg() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect(&root.join("ui/chat"), &mut files);
    files.push(root.join("ui/panes/rooms.rs"));
    assert!(files.len() > 2, "the sources were found: {files:?}");
    for path in &files {
        let text = std::fs::read_to_string(path).expect("a source file");
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            // A comment is not code: this file says `Msg` in prose, and says nothing with it.
            if code.starts_with("//") {
                continue;
            }
            assert!(
                !line.contains("Msg"),
                "{}:{} reads a message: {line}",
                path.display(),
                n + 1
            );
        }
    }
    let adapter = std::fs::read_to_string(root.join("app/chat_adapter.rs")).expect("the adapter");
    assert!(
        adapter.contains("Msg"),
        "the adapter is where a message is read"
    );
}

fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).expect("a source directory") {
        let p = e.expect("an entry").path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}
