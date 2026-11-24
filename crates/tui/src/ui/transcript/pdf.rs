//! A document in the conversation, as a card of pages: the header that names it and says which page
//! the reader is on, the page itself as a picture the terminal fills in, and the text a page falls
//! back to when there is no picture to draw.
//!
//! The card draws what the app holds (`app::pdf`) and records where it put its rows, because the
//! page turn needs to know which lines are the card's. It renders nothing: a page comes from the
//! renderer on another thread, and until it lands the card says `rendering p N…`.

use crate::ui::render::{RenderConfig, bytes_human, row, trim_to, wrap};
use crate::ui::*;
use letibot_transcript::UserPart;
use letibot_transcript::media::Media;
use rano::render::{Line, Span};
use rano::style::Role;

/// What a document card starts with: a page, so the card is not read as another paragraph of the
/// row above it. The same column the tool cards' glyphs use.
pub(crate) const GLYPH: &str = "▤";

/// How far the page is set in from the row's own text: the pictures under a prompt sit at the same
/// step, and a page at the left edge reads as part of the words.
const INSET: &str = "  ";

/// The header: `▤ name · p N/M · size`. `M` is `…` until a render has counted the pages - a count
/// this head does not have is not one it prints.
pub(crate) fn header(
    name: &str,
    page: u32,
    pages: Option<u32>,
    bytes: usize,
    w: usize,
    cfg: &RenderConfig,
) -> String {
    let of = match pages {
        Some(n) => (n.max(1)).to_string(),
        None => "…".to_string(),
    };
    let line = Line::new(vec![
        Span::role(format!("{GLYPH} "), Role::Faint),
        Span::role(name.to_string(), Role::Strong),
        Span::role(
            format!(" · p {}/{} · {}", page + 1, of, bytes_human(bytes as u64)),
            Role::Faint,
        ),
    ]);
    row(&rano::render::truncate(&line, w), cfg.palette())
}

/// The cards one prompt's document parts draw, in part order - each under the ones before it, so
/// two documents attached together are two cards and not one. `base` is how many rows of the prompt
/// are already drawn: the card's place inside its own row, which the page turn reads back.
pub(crate) fn user_cards(
    item_id: &str,
    parts: &[UserPart],
    base: usize,
    ctx: &ItemCtx<'_>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (k, p) in parts.iter().enumerate() {
        let UserPart::Document {
            filename, data_ref, ..
        } = p
        else {
            continue;
        };
        let at = base + out.len();
        out.extend(card(item_id, k, filename, ref_len(data_ref), at, ctx));
    }
    out
}

/// The card a tool result's document draws: `name` is the file the call read, and the size is the
/// row's own measurement of it.
pub(crate) fn tool_card(
    item_id: &str,
    media: &Media,
    name: &str,
    at: usize,
    ctx: &ItemCtx<'_>,
) -> Vec<String> {
    card(item_id, 0, name, media.bytes, at, ctx)
}

/// One card: the header, then the page - as a picture where the terminal draws one, as its text
/// where it does not or where the rasterizer refused, and as one row saying so while it comes.
///
/// `at` is how many rows of this row are already drawn, which is what places the card inside its own
/// row: the page turn reads it back, and nothing else knows where the card starts.
fn card(
    item_id: &str,
    part: usize,
    name: &str,
    bytes: usize,
    at: usize,
    ctx: &ItemCtx<'_>,
) -> Vec<String> {
    let cfg = ctx.cfg;
    let held = ctx.pdf.card(item_id, part);
    let page = held.map(|c| c.page).unwrap_or(0);
    let pages = held.and_then(|c| c.pages);
    let w = cfg.width.max(20);
    let mut out = vec![header(name, page, pages, bytes, w, cfg)];
    // A verbosity that hides the head's working shows the header alone: the card still says which
    // document and which page, and costs the conversation one row rather than a screenful.
    if !ctx.vis.hides_the_working() {
        out.extend(body(item_id, part, page, w, ctx));
    }
    ctx.pdf.place(
        item_id,
        part,
        crate::app::pdf::Where {
            at,
            lines: out.len(),
            page,
        },
    );
    out
}

/// The page itself, as the rows the card draws under its header.
fn body(item_id: &str, part: usize, page: u32, w: usize, ctx: &ItemCtx<'_>) -> Vec<String> {
    let held = ctx.pdf.card(item_id, part);
    // The picture first: a page the terminal holds bytes for is drawn wherever it can be, and the
    // text is the fallback rather than the other way round.
    if let (true, Some(shot)) = (ctx.cfg.images, held.and_then(|c| c.shot(page))) {
        let box_cols = rano::term::graphics::image_box(w);
        let (cols, rows) =
            rano::term::graphics::image_cells(Some(shot.px.0), Some(shot.px.1), box_cols);
        let id = crate::app::pdf::page_id(item_id, part, page);
        return rano::term::graphics::image_rows(id, cols, rows)
            .into_iter()
            .map(|r| format!("{INSET}{r}"))
            .collect();
    }
    if let Some(text) = held.and_then(|c| c.text_of(page)) {
        return text_rows(text, w, ctx.cfg);
    }
    match held.and_then(|c| c.failure(page)) {
        Some(why) => vec![faint(&format!("{INSET}{why}"), w, ctx.cfg)],
        // Nothing yet: one row that says the page is on its way. The app starts the render from the
        // clock, so this row is drawn on the frame before it and replaced by the frame it lands on.
        None => vec![faint(
            &format!("{INSET}rendering p {}…", page + 1),
            w,
            ctx.cfg,
        )],
    }
}

/// A page as text, wrapped to the card and bounded: `pdftotext` gives a page's lines, and a page with
/// more than [`crate::app::pdf::TEXT_ROWS`] of them is one where the tail is not worth the frame.
fn text_rows(text: &str, w: usize, cfg: &RenderConfig) -> Vec<String> {
    let room = w.saturating_sub(INSET.len()).max(20);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            if out.last().is_some_and(|l: &String| !l.is_empty()) {
                out.push(String::new());
            }
            continue;
        }
        for piece in wrap(line, room) {
            if out.len() >= crate::app::pdf::TEXT_ROWS {
                out.push(faint(
                    &format!("{INSET}… the rest of this page is text, not drawn"),
                    w,
                    cfg,
                ));
                return out;
            }
            out.push(format!("{INSET}{piece}"));
        }
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

/// One faint row, cut to the width.
fn faint(text: &str, w: usize, cfg: &RenderConfig) -> String {
    let line = Line::new(vec![Span::role(
        trim_to(text, w.saturating_sub(1)),
        Role::Faint,
    )]);
    row(&line, cfg.palette())
}

/// The bytes a `data:` reference carries, from the base64's own arithmetic and no decode: the card's
/// size line is a number the row already states and decoding a megabyte to count it is a number this
/// head paid for.
pub(crate) fn ref_len(data_ref: &str) -> usize {
    let b64 = letibot_transcript::media::wire_payload(data_ref);
    let pad = b64.ends_with('=') as usize + b64.ends_with("==") as usize;
    (b64.len() / 4) * 3 - pad.min(2)
}

/// The file a tool result's document is, as the card's header names it: the call's target, tail
/// only when it is a path, and the payload's own first line when the row has no target.
pub(crate) fn tool_name(target: &str, payload: &str) -> String {
    let target = target.trim();
    if !target.is_empty() {
        if let Some(tail) = target.rsplit('/').next().filter(|t| !t.is_empty()) {
            return tail.to_string();
        }
        return target.to_string();
    }
    let first = payload.lines().next().unwrap_or("").trim();
    let named = first.split(" — ").next().unwrap_or(first).trim();
    if named.is_empty() {
        "document".to_string()
    } else {
        named.rsplit('/').next().unwrap_or(named).to_string()
    }
}

/// The header as a test reads it: the colourless form of the row the card draws.
#[cfg(test)]
pub(crate) fn header_plain(name: &str, page: u32, pages: Option<u32>, bytes: usize) -> String {
    header(
        name,
        page,
        pages,
        bytes,
        usize::MAX,
        &RenderConfig {
            color: false,
            ..RenderConfig::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header states the three facts the card is: what the document is, which page of how many,
    /// and how big. A page count nobody has is `…`, never a number this head guessed.
    #[test]
    fn the_header_names_the_document_the_page_and_the_size() {
        assert_eq!(
            header_plain("memo.pdf", 0, Some(3), 12 * 1024),
            "▤ memo.pdf · p 1/3 · 12.0 KB"
        );
        assert_eq!(
            header_plain("memo.pdf", 1, None, 900),
            "▤ memo.pdf · p 2/… · 900 B"
        );
        // And it is cut to the frame, because a long name must not push the edge out.
        let cfg = RenderConfig {
            width: 40,
            color: false,
            ..RenderConfig::default()
        };
        let cut = header(&"a".repeat(200), 0, Some(1), 10, 40, &cfg);
        assert_eq!(crate::ui::render::visible_width(&cut), 40, "{cut:?}");
    }

    /// The size a reference states is the file's, not the base64's.
    #[test]
    fn a_reference_states_the_files_own_size() {
        let bytes = letibot_transcript::testing::pdf("hello");
        let media = letibot_transcript::media::Media::document("a.pdf", &bytes).expect("a pdf");
        assert_eq!(media.bytes, bytes.len());
        assert_eq!(ref_len(&media.data_ref), bytes.len());
    }

    /// A tool result's document is named by the call's target, and by the payload's own first line
    /// when the row has no target - the path is the tail, because the row's header already shows it.
    #[test]
    fn a_tool_results_document_is_named_by_what_the_call_read() {
        assert_eq!(tool_name("docs/memo.pdf", "whatever"), "memo.pdf");
        assert_eq!(tool_name("memo.pdf", "whatever"), "memo.pdf");
        assert_eq!(
            tool_name(
                "",
                "/w/docs/memo.pdf — document application/pdf · 12 KiB\nThe document is"
            ),
            "memo.pdf"
        );
        assert_eq!(tool_name("", ""), "document");
    }

    /// A page's text is wrapped to the card's width and set in; a page with more rows than the bound
    /// says so in one row rather than drawing the rest.
    #[test]
    fn a_pages_text_is_wrapped_in_and_bounded() {
        let cfg = RenderConfig {
            width: 40,
            color: false,
            ..RenderConfig::default()
        };
        let page = "the first line\n\nthe second line is long enough to wrap at forty columns\n";
        let rows = text_rows(page, 40, &cfg);
        assert_eq!(rows[0], "  the first line");
        assert_eq!(rows[1], "", "a paragraph break is kept");
        assert!(rows[2].starts_with("  the second line"), "{rows:?}");
        assert!(
            rows.iter()
                .all(|r| crate::ui::render::visible_width(r) <= 40),
            "{rows:?}"
        );
        // A page of a thousand lines stops at the bound, with the row that says why.
        let long = "a line\n".repeat(1000);
        let many = text_rows(&long, 40, &cfg);
        assert_eq!(many.len(), crate::app::pdf::TEXT_ROWS + 1, "{}", many.len());
        assert!(
            many.last().unwrap().contains("the rest of this page"),
            "{many:?}"
        );
    }
}
