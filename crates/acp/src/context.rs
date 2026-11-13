//! An ACP prompt as the two things a letibot prompt is: the operator's words, and the editor
//! context the daemon renders after them.
//!
//! ACP carries context two ways (`docs/protocol/v1/content.mdx`): a `resource` block with the text
//! embedded, which is what a client that has the buffer sends, and a `resource_link`, which is a
//! reference the agent is expected to read itself. Both name their place by URI, and neither spec
//! says a URI may carry lines - so the lines ride a fragment, `#L41-L47`, the spelling GitHub's
//! permalinks and opencode's `@file:41-47` both settle on.
//!
//! The range is rano's `SendEvent` shape: a column-1 end is the start of the line AFTER the last
//! line named, which is exactly how `harnessd::editor_context` reads it when it prints
//! `article.md:41-47`.

use letibot_sessionlog::protocol::{ContextPos, ContextRange, ContextRef, EditorContext};

use crate::acp::ContentBlock;

/// What `docs/rano-integration.md` §4.2 calls the origin: who captured it, said on the block's
/// first line, so the model reads "from the editor (acp)".
pub const ORIGIN: &str = "acp";

/// Split a prompt into the words and what it pointed at.
///
/// `None` for the context when the prompt named no place, because an empty [`EditorContext`] is a
/// block the daemon would render for nothing.
pub fn split(prompt: &[ContentBlock]) -> (String, Option<EditorContext>) {
    let mut words: Vec<&str> = Vec::new();
    let mut refs: Vec<ContextRef> = Vec::new();
    for block in prompt {
        match block {
            ContentBlock::Text { text } => words.push(text),
            ContentBlock::Resource { resource, meta } => {
                let (file, range) = place(&resource.uri);
                refs.push(ContextRef {
                    range,
                    lang: lang_of(&file),
                    text: resource.text.clone().unwrap_or_default(),
                    unsaved: unsaved(meta.as_ref()),
                    title: String::new(),
                    cursor: None,
                    file,
                });
            }
            ContentBlock::ResourceLink {
                uri,
                name,
                title,
                mime_type,
            } => {
                let (file, range) = place(uri);
                refs.push(ContextRef {
                    file,
                    range,
                    cursor: None,
                    text: String::new(),
                    unsaved: false,
                    lang: mime_type
                        .clone()
                        .unwrap_or_else(|| lang_of(name))
                        .trim_start_matches("text/")
                        .to_string(),
                    title: title.clone().unwrap_or_default(),
                });
            }
            // An image or audio block is refused by the capability this adapter does not
            // advertise, so a client that sent one anyway gets the words and nothing else.
            ContentBlock::Image { .. } | ContentBlock::Audio { .. } | ContentBlock::Other => {}
        }
    }
    let context = (!refs.is_empty()).then(|| EditorContext {
        origin: ORIGIN.into(),
        refs,
        open: Vec::new(),
        ask: None,
        outcomes: Vec::new(),
    });
    (words.join("\n\n"), context)
}

/// The file a URI names, and the lines its fragment names.
///
/// A `file:` URI becomes a path; anything else is kept whole, because [`ContextRef::file`] already
/// holds a URL a browser captured and the daemon prints it relative to nothing.
pub fn place(uri: &str) -> (String, Option<ContextRange>) {
    let (base, fragment) = match uri.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (uri, None),
    };
    let file = match base.strip_prefix("file://") {
        // `file:///a/b` is a path from the root; `file://host/a` names a host this adapter cannot
        // reach, and the authority is kept rather than silently dropped.
        Some(rest) => match rest.strip_prefix('/') {
            Some(path) => format!("/{}", percent_decode(path)),
            None => percent_decode(rest),
        },
        None => percent_decode(base),
    };
    (file, fragment.and_then(lines))
}

/// A file and lines as the URI that names them, which is the inverse of [`place`] - the one place
/// the fragment's spelling exists.
pub fn uri_for(path: &str, start: u32, end: u32) -> String {
    let path = percent_encode(path);
    if end > start {
        format!("file://{path}#L{start}-L{end}")
    } else {
        format!("file://{path}#L{start}")
    }
}

/// `L41-L47`, `L41-47` or `L41`, as a range whose end is the start of the line after the last one.
fn lines(fragment: &str) -> Option<ContextRange> {
    let (first, last) = match fragment.strip_prefix('L')?.split_once(['-', ',']) {
        Some((first, last)) => (first, last.trim_start_matches('L')),
        None => (fragment.trim_start_matches('L'), ""),
    };
    let start: u32 = first.parse().ok()?;
    if start == 0 {
        return None;
    }
    let last: u32 = match last {
        "" => start,
        word => word.parse().ok()?,
    };
    // A client that names them backwards is read forwards: the point is the lines it means.
    let (start, last) = (start.min(last), start.max(last));
    Some(ContextRange {
        start: ContextPos {
            line: start,
            column: 1,
        },
        end: ContextPos {
            line: last + 1,
            column: 1,
        },
    })
}

/// The fence language the daemon prints, which the head reads off the path's extension. A MIME
/// type is no better: `text/markdown` is not the word a fence is labelled with.
fn lang_of(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string()
}

/// Whether the client says the text it sent is its buffer rather than the file on disk. ACP has no
/// such bit, so a client that cares adds `_meta.letibot.unsaved`; absent is the disk's text.
fn unsaved(meta: Option<&serde_json::Value>) -> bool {
    meta.and_then(|m| m.get("letibot"))
        .and_then(|m| m.get("unsaved"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = bytes.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(&String::from_utf8_lossy(hex), 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The characters a URI may not carry bare. A path with a space is the case that matters: the
/// fragment parser would still work, but a client reading the URI back would split it wrong.
fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The fragment is the whole of "a ranged resource": it becomes the range the daemon prints as
    /// `article.md:41-47`, and the end is the line AFTER the last one named.
    #[test]
    fn a_line_fragment_becomes_the_range_the_daemon_prints() {
        let (file, range) = place("file:///ws/docs/article.md#L41-L47");
        assert_eq!(file, "/ws/docs/article.md");
        assert_eq!(
            range,
            Some(ContextRange {
                start: ContextPos {
                    line: 41,
                    column: 1
                },
                end: ContextPos {
                    line: 48,
                    column: 1
                },
            })
        );
        let (_, one) = place("file:///ws/a.md#L7");
        assert_eq!(
            one.map(|r| (r.start.line, r.end.line)),
            Some((7, 8)),
            "one line is a range of one"
        );
        assert_eq!(
            place("file:///ws/a.md").1,
            None,
            "no fragment is the whole file"
        );
    }

    /// A URL the browser captured is not a path, and it keeps its lines: one place, two origins.
    #[test]
    fn a_url_keeps_its_scheme_and_a_space_survives_the_round_trip() {
        let (file, range) = place("https://example.com/a%20post#L3-L4");
        assert_eq!(file, "https://example.com/a post");
        assert_eq!(range.map(|r| r.start.line), Some(3));
        assert_eq!(
            place(&uri_for("/ws/my notes.md", 41, 47)),
            (
                "/ws/my notes.md".to_string(),
                place("file:///ws/x.md#L41-L47").1
            ),
            "a URI the adapter writes is one it reads back"
        );
    }

    /// The words are the operator's and the refs are in the order they were attached; an unsaved
    /// buffer is the client's `_meta`, and a bare link is a place with no text.
    #[test]
    fn a_prompt_splits_into_words_and_the_places_it_pointed_at() {
        let prompt = vec![
            ContentBlock::Text {
                text: "check this".into(),
            },
            ContentBlock::Resource {
                resource: crate::acp::EmbeddedResource {
                    uri: "file:///ws/docs/article.md#L41-L47".into(),
                    mime_type: None,
                    text: Some("41 the cable\n".into()),
                    blob: None,
                },
                meta: Some(json!({"letibot": {"unsaved": true}})),
            },
            ContentBlock::ResourceLink {
                uri: "file:///ws/notes.md".into(),
                name: "notes.md".into(),
                title: None,
                mime_type: None,
            },
        ];
        let (text, context) = split(&prompt);
        assert_eq!(text, "check this");
        let context = context.expect("two places");
        assert_eq!(context.origin, "acp");
        assert_eq!(context.refs.len(), 2);
        assert_eq!(context.refs[0].file, "/ws/docs/article.md");
        assert_eq!(context.refs[0].text, "41 the cable\n");
        assert!(context.refs[0].unsaved);
        assert_eq!(context.refs[0].lang, "md");
        assert_eq!(context.refs[1].range, None);
        assert_eq!(context.refs[1].text, "");
        assert_eq!(context.refs[1].lang, "md");
        assert!(context.ask.is_none());
    }

    /// A prompt of prose alone is the whole of what the daemon renders: no block for nothing.
    #[test]
    fn words_with_no_place_carry_no_context() {
        let (text, context) = split(&[ContentBlock::Text {
            text: "just a question".into(),
        }]);
        assert_eq!(text, "just a question");
        assert!(context.is_none());
    }
}
