//! What a layer says about the *inside* of a body: a token that reads as a name, the search's
//! needle, and whether this terminal takes a hyperlink. The markdown itself is never edited - the
//! spans rano drew are split and repainted, which is the whole point of a mention being a token.

use std::collections::BTreeMap;

use letibot_sessionlog::pane::Mention;
use rano::render::{Span, Style};
use rano::style::Role as Paint;

use crate::ui::chat::colour;

/// What one body is drawn against, beside its markdown.
pub(super) struct Marks<'a> {
    /// Token (`@s-1793…`) -> how it reads.
    pub mentions: &'a BTreeMap<String, Mention>,
    /// The search's needle: its hits are picked out.
    pub highlight: Option<&'a str>,
    /// The terminal was told it speaks OSC 8, so a link in the markdown stays a link.
    pub links: bool,
}

/// The characters a handle is made of, as the room parses a mention: a `@` and a `:` join a
/// bridge's parts, so `cc@folder:name` is one token and `@name.` is a token and a full stop.
fn name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'@' | b'*')
}

/// One span as the spans that draw it: every mention token rewritten to the name the layer gave it
/// and painted in its hue, every hit of the needle picked out, and no hyperlink at all when the
/// terminal never said it speaks them.
pub(super) fn spans(sp: &Span, m: &Marks<'_>) -> Vec<Span> {
    let mut base = sp.style.clone();
    if !m.links {
        base.link = None;
    }
    let mut out = Vec::new();
    let content = sp.content.as_str();
    let mut at = 0usize;
    let mut plain = String::new();
    while at < content.len() {
        let Some((token, mention, end)) = token_at(content, at, m.mentions) else {
            let ch = content[at..].chars().next().unwrap_or(' ');
            plain.push(ch);
            at += ch.len_utf8();
            continue;
        };
        out.extend(picked(&plain, &base, m.highlight));
        plain.clear();
        let text = mention.text.clone().unwrap_or_else(|| token.to_string());
        // A mention of the reader is the one thing a body may not be quiet about: it is painted
        // the way this head paints *needs a person*, not the way that speaker usually speaks.
        let paint = match (mention.me, mention.hue) {
            (true, _) => Paint::Attention,
            (false, hue) => colour::paint(hue.unwrap_or_else(|| colour::hue(token))),
        };
        out.push(Span::styled(text, base.patch(&Style::of(paint))));
        at = end;
    }
    out.extend(picked(&plain, &base, m.highlight));
    out
}

/// The mention token that starts at `at`, when one does: the longest match, on word boundaries, so
/// `@s-abc` is not found inside `@s-abcd` and `mail@host` is not a mention.
fn token_at<'a>(
    content: &'a str,
    at: usize,
    mentions: &'a BTreeMap<String, Mention>,
) -> Option<(&'a str, &'a Mention, usize)> {
    let boundary = at == 0 || !name_byte(content.as_bytes()[at - 1]);
    if !boundary || content.as_bytes()[at] != b'@' {
        return None;
    }
    let mut best: Option<(&str, &Mention, usize)> = None;
    for (token, mention) in mentions {
        let end = at + token.len();
        if !content.get(at..end).is_some_and(|s| s == token) {
            continue;
        }
        let after = content.as_bytes().get(end).is_none_or(|b| !name_byte(*b));
        if after && best.is_none_or(|(t, _, _)| token.len() > t.len()) {
            best = Some((token, mention, end));
        }
    }
    best
}

/// `text` with every hit of the needle painted the way this head paints a match.
fn picked(text: &str, base: &Style, needle: Option<&str>) -> Vec<Span> {
    let Some(needle) = needle.filter(|n| !n.is_empty()) else {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(text, base.clone())]
        };
    };
    let mark = base.patch(&Style::of(Paint::Match));
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut at = 0usize;
    while at < text.len() {
        let end = at + needle.len();
        let hit = text
            .get(at..end)
            .is_some_and(|s| s.eq_ignore_ascii_case(needle));
        if hit {
            if !plain.is_empty() {
                out.push(Span::styled(std::mem::take(&mut plain), base.clone()));
            }
            out.push(Span::styled(text[at..end].to_string(), mark.clone()));
            at = end;
            continue;
        }
        let ch = text[at..].chars().next().unwrap_or(' ');
        plain.push(ch);
        at += ch.len_utf8();
    }
    if !plain.is_empty() {
        out.push(Span::styled(plain, base.clone()));
    }
    out
}
