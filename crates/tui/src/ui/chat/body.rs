//! A body: markdown through rano at the column the gutter leaves, with the pictures it names drawn
//! where it names them. Nothing here reads a message - it reads the entry a layer turned it into.

use letibot_ui::text::without_control_lines;
use rano::markdown::{lex, render_bounded};
use rano::render::{Line, Span, Style, truncate};
use rano::style::Role as Paint;

use crate::ui::chat::{Blob, Draw, md_options, mention};

/// A picture taller than this is most of a chat's screen, so it is drawn at this many rows and the
/// top of it is what the reader sees.
const PICTURE_ROWS: u32 = 12;

/// One piece of a body, in the order the reader meets them.
enum Piece {
    /// Markdown to draw.
    Text(String),
    /// A blob the message carries, drawn at the row its reference sat on.
    Picture { id: String, alt: String },
}

/// The rows a body draws: its markdown, and between two runs of it the pictures the message names.
pub(super) fn lines(d: &Draw<'_>) -> Vec<Line> {
    let Some(text) = &d.e.body else {
        return Vec::new();
    };
    // Somebody else's bytes: a control character is an instruction to the operator's terminal, so
    // it goes before the lexer and every row, width and measurement below sees one string.
    let text = without_control_lines(text);
    let mut out = Vec::new();
    for piece in pieces(&text, &d.e.pictures) {
        match piece {
            Piece::Text(t) => {
                let opts = md_options(d.ctx.cfg, d.ctx.cfg.budget.body_lines);
                for block in lex(&t) {
                    let rows = render_bounded(&block, d.body_cols(), &opts);
                    out.extend(rows.iter().map(|l| fitted(l, d)));
                }
            }
            Piece::Picture { id, alt } => out.extend(picture(&id, &alt, d)),
        }
    }
    out
}

/// One row as this body draws it: under the gutter, with the layer's marks applied, cut to the
/// column. A row rano drew with nothing on it stays a row: an empty line inside a message is a
/// line the author wrote.
fn fitted(l: &Line, d: &Draw<'_>) -> Line {
    let mut spans = vec![Span::raw(" ".repeat(d.gutter()))];
    let marks = mention::Marks {
        mentions: d.mentions,
        highlight: d.ctx.highlight,
        links: d.ctx.cfg.links.is_some(),
    };
    for sp in &l.spans {
        spans.extend(mention::spans(sp, &marks));
    }
    let mut line = Line::new(spans);
    line.style = Style::of(crate::ui::severity::role(d.e.role));
    truncate(&line, d.cols)
}

/// The body's pieces: every `![alt](blob:ID)` the text names, and after the words the pictures it
/// does not name - a message that carries a picture still carries it when the text forgot to.
fn pieces(text: &str, pictures: &[String]) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let mut named: Vec<&str> = Vec::new();
    let mut from = 0usize;
    let mut at = 0usize;
    while let Some(open) = text[at..].find("![") {
        let open = at + open;
        let Some((alt, id, end)) = reference(text, open) else {
            at = open + 2;
            continue;
        };
        out.push(Piece::Text(text[from..open].to_string()));
        out.push(Piece::Picture {
            id: id.to_string(),
            alt,
        });
        named.push(id);
        from = end;
        at = end;
    }
    out.push(Piece::Text(text[from..].to_string()));
    for id in pictures {
        if !named.contains(&id.as_str()) {
            out.push(Piece::Picture {
                id: id.clone(),
                alt: String::new(),
            });
        }
    }
    out.retain(|p| !matches!(p, Piece::Text(t) if t.is_empty()));
    out
}

/// The `![alt](blob:ID)` starting at `open`, as its alt text, its blob id and where it ends. `None`
/// for anything else - a URL, a picture split across a paragraph, an unclosed bracket - which the
/// markdown renderer keeps as the text it is.
fn reference(text: &str, open: usize) -> Option<(String, &str, usize)> {
    let rest = text.get(open + 2..)?;
    let close = rest.find("](")?;
    if rest[..close].contains('\n') {
        return None;
    }
    let after = rest.get(close + 2..)?;
    let end = after.find(')')?;
    let target = after[..end].split(' ').next().unwrap_or("").trim();
    let id = target.strip_prefix("blob:")?;
    if id.is_empty() || id.contains('\n') {
        return None;
    }
    Some((
        rest[..close].trim().to_string(),
        id,
        open + 2 + close + 2 + end + 1,
    ))
}

/// A picture as the rows it takes: the terminal's own placeholders where it speaks kitty, and one
/// row naming it everywhere else.
fn picture(id: &str, alt: &str, d: &Draw<'_>) -> Vec<Line> {
    let gutter = Span::raw(" ".repeat(d.gutter()));
    let blob = d.ctx.blobs.get(id);
    let drawn = (d.ctx.picture)(id);
    if let (Some((image, w, h)), Some(Blob { picture: true, .. })) = (drawn, blob)
        && d.ctx.cfg.images
    {
        let (cols, rows) = rano::term::graphics::image_cells(
            Some(w),
            Some(h),
            rano::term::graphics::image_box(d.body_cols()),
        );
        return rano::term::graphics::image_lines(image, cols, rows.min(PICTURE_ROWS))
            .into_iter()
            .map(|mut l| {
                l.spans.insert(0, gutter.clone());
                l
            })
            .collect();
    }
    // The size is the wire's measurement of the image's own header, and the cache's when the bytes
    // are here - the same two numbers. A picture nobody measured draws without one: absent is not
    // 0x0, and a row that said so would be a measurement nobody made.
    let size = drawn
        .map(|(_, w, h)| (w, h))
        .or_else(|| blob.and_then(|b| b.size))
        .map(|(w, h)| format!("{w}x{h} "))
        .unwrap_or_default();
    let word = match blob {
        Some(b) if b.picture => "picture",
        Some(_) => "file",
        None => "picture",
    };
    let name = match blob.map(|b| b.name.as_str()).filter(|n| !n.is_empty()) {
        Some(name) => name.to_string(),
        None if !alt.is_empty() => alt.to_string(),
        None => id.chars().take(8).collect(),
    };
    let row = Line::new(vec![
        gutter,
        Span::role(format!("[{word} {size}{name} - enter opens]"), Paint::Faint),
    ]);
    vec![truncate(&row, d.cols)]
}
