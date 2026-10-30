//! **A link on the frame is a target** — the operator, 2026-10-30: *"why links are not clickable
//! and ghostty doesnt detect them either?"*.
//!
//! # Why the terminal cannot answer this one
//!
//! The head takes the mouse: `rano::term::Mouse::Any` is `CSI ?1003h` and `CSI ?1006h`
//! (`head.rs`), so every motion and every click is reported to this program and Ghostty's own
//! link detection never runs — its hover underline, and its `cmd`+click, which the SGR protocol
//! cannot even carry (the Command key has no modifier bit, so a forwarded `cmd`+click arrives as
//! a plain click). A link drawn well is therefore still not clickable unless THIS program makes
//! it one. So it does: [`crate::app::hits::Target::Link`] is recorded for every link the finished
//! frame draws, and a click on one opens it.
//!
//! # What is a link, and who decides
//!
//! The frame is read back for `OSC 8` pairs ([`osc8_spans`]) — the same shape as
//! `App::scan_hints`, which reads the key hints back off the frame — so a link is whatever the
//! frame drew as one, whichever renderer drew it: rano's markdown label, a `file_url` span from
//! `ui::render::path_span`, the `http(s)` pass and the repository's own words in
//! `ui::screen::App::draw_links`.
//!
//! The one detection this file adds is `path:line`, and it exists because rano's `link_words`
//! cannot see one: its word is `[alnum /#-_.]`, so `src/main.rs:120` arrives as the word
//! `src/main.rs` with `:120` beside it as a word of its own. The head does the split — the half
//! only it knows, exactly as `ui::render::path_span` says — and rano's `file_url` still decides
//! what URL a path is. **The generalisation rano would need to own this instead is one character
//! (`:` in `link_words`'s word), and it was left alone: rano is pinned by four manifests and a
//! rev bump is its own step.**

use super::*;

/// **Every `OSC 8` link on a finished row, as the columns it covers** — `(from, to, url)`, half
/// open, 0-based, in the row's own cells (a link is zero columns, so the escapes do not count).
///
/// The escape walk is `app::pointer`'s — one walker for the head's frames, not a third.
pub(crate) fn osc8_spans(row: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut open: Option<(String, usize)> = None;
    let mut col = 0usize;
    for p in pointer::pieces(row) {
        match p {
            pointer::Piece::Escape(e) => {
                let Some(uri) = osc8_uri(e) else { continue };
                // The end of whatever was open is where the new sequence starts, so a link's
                // span is exactly the cells between its opener and its closer.
                if let Some((url, from)) = open.take()
                    && col > from
                {
                    out.push((from, col, url));
                }
                if !uri.is_empty() {
                    open = Some((uri.to_string(), col));
                }
            }
            pointer::Piece::Cell(_, w) => col += w,
        }
    }
    // An opener whose closer the row never got — a cut row, which `rano::width::text::truncate`
    // closes, so this is the belt under that braces. The span is the rest of the row.
    if let Some((url, from)) = open
        && col > from
    {
        out.push((from, col, url));
    }
    out
}

/// The URL an `OSC 8` sequence names, or `None` when the sequence is some other escape. An empty
/// URL is the closer.
fn osc8_uri(seq: &str) -> Option<&str> {
    let body = seq.strip_prefix("\x1b]8;")?;
    let body = body
        .strip_suffix("\x1b\\")
        .or_else(|| body.strip_suffix('\x07'))
        .unwrap_or(body);
    body.split_once(';').map(|(_, uri)| uri)
}

/// **`path:line` in a row, made a link** — `Some` only when the row changed, like rano's own
/// `link_words`, so the common row costs no copy.
///
/// A candidate is a run of [`path_char`]s, a `:`, and digits; the text before the colon must hold
/// a `/` or a `.` (prose is not a file listing — `and:12` is not a path), and
/// [`rano::term::links::file_url`] must accept it. The line rides on the URL as `:N`, which is
/// how this head reads it back ([`file_target`]). Text inside a link already drawn is left alone.
pub(crate) fn link_path_lines(row: &str, root: &str) -> Option<String> {
    let ps = pointer::pieces(row);
    let mut found: Vec<(usize, usize, String)> = Vec::new();
    let mut in_link = false;
    let mut i = 0;
    while i < ps.len() {
        match &ps[i] {
            pointer::Piece::Escape(e) => {
                if let Some(uri) = osc8_uri(e) {
                    in_link = !uri.is_empty();
                }
                i += 1;
            }
            pointer::Piece::Cell(c, _) if !in_link && path_char(*c) => {
                let start = i;
                let mut text = String::new();
                let mut j = i;
                while let Some(pointer::Piece::Cell(c, _)) = ps.get(j) {
                    if !path_char(*c) {
                        break;
                    }
                    text.push(*c);
                    j += 1;
                }
                let line = line_suffix(&ps, j);
                let end = j + line.as_ref().map_or(0, |(n, _)| *n);
                if let Some((_, digits)) = line
                    && (text.contains('/') || text.contains('.'))
                    && let Some(url) = rano::term::links::file_url(root, &text)
                {
                    found.push((start, end, format!("{url}:{digits}")));
                    i = end;
                    continue;
                }
                i = j.max(i + 1);
            }
            _ => i += 1,
        }
    }
    if found.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(row.len() + 64);
    let mut at = 0;
    for (a, z, url) in found {
        out.push_str(&render(&ps[at..a]));
        let text = render(&ps[a..z]);
        out.push_str(&format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\"));
        at = z;
    }
    out.push_str(&render(&ps[at..]));
    Some(out)
}

/// The `:N` at piece `j`, as `(how many pieces it is, the digits)`.
fn line_suffix(ps: &[pointer::Piece<'_>], j: usize) -> Option<(usize, String)> {
    match ps.get(j) {
        Some(pointer::Piece::Cell(':', _)) => {}
        _ => return None,
    }
    let mut digits = String::new();
    let mut k = j + 1;
    while let Some(pointer::Piece::Cell(c, _)) = ps.get(k) {
        if !c.is_ascii_digit() {
            break;
        }
        digits.push(*c);
        k += 1;
    }
    (!digits.is_empty()).then(|| (k - j, digits))
}

/// The characters a path in prose is made of — rano's `link_words` word without `#`, and without
/// `:`: the colon is the line's, which is the whole of what this pass is for.
fn path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '~' | '+')
}

fn render(ps: &[pointer::Piece<'_>]) -> String {
    let mut out = String::new();
    for p in ps {
        match p {
            pointer::Piece::Escape(e) => out.push_str(e),
            pointer::Piece::Cell(c, _) => out.push(*c),
        }
    }
    out
}

/// **The file a `file://` URL names, and the line it names** — `("…/src/main.rs", 120)`.
///
/// The URL is rano's (`file://<host><percent-encoded path>`) with this head's `:N` on the end
/// ([`link_path_lines`]). The host is not compared: the head that drew the link is this process,
/// so the path is this machine's — which is what `file_url`'s host exists to keep true across
/// `ssh`, where the head runs on the far end and its own paths are the near end's.
pub(crate) fn file_target(url: &str) -> Option<(String, usize)> {
    let rest = url.strip_prefix("file://")?;
    // `file://host/path` — the host has no `/` in it, so the first one starts the path.
    let path = &rest[rest.find('/')?..];
    let (path, line) = match path.rsplit_once(':') {
        Some((p, n)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => {
            (p, n.parse().unwrap_or(1))
        }
        _ => (path, 1),
    };
    Some((percent_decode(path), line))
}

/// rano's `uri_path` read back: the bytes that were `%XX` are those bytes again.
fn percent_decode(p: &str) -> String {
    let b = p.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = b
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl App {
    /// **A link, opened** — a `file://` link in the editor pane this head already opens files
    /// with (`App::open_in_editor`, at the line the URL names), and anything else handed to the
    /// platform's opener as [`Action::OpenUrl`], which is the driver's to run.
    ///
    /// The file arm is not a second opener: it is the door an edit row, a cited line and a
    /// standing note already come through. A web URL has no such door in this head — nothing in
    /// the tree opened one before this — so the click hands it to the one process that can.
    pub(crate) fn open_link(&mut self, url: &str) -> Option<Action> {
        if let Some((path, line)) = file_target(url) {
            self.open_in_editor(&FileRef {
                path,
                line,
                change: None,
            });
            return None;
        }
        Some(Action::OpenUrl(url.to_string()))
    }
}
