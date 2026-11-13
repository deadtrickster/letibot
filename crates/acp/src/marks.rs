//! What a reply's line-addressed marks become in ACP - the one place that mapping lives.
//!
//! The daemon already publishes them: `SessionEvent::Citations` carries `{label, path, start, end,
//! lines}` per fence the model wrote as `path:start-end`, and `ToolFinished::edits` carries both
//! sides of every region a call changed. `docs/rano-integration.md` §5.1 turns the first into three
//! kinds (L7: a `cite`, a `note` and a `suggest`, the last two with a body). ACP has no editor mark
//! at all, so a mark is not a construct of its own here: it rides something the client already
//! draws, which is why this module is a mapping and not an event.
//!
//! **What L7 adds, and the one line it changes.** `Citation` gains `kind: CitationKind` and
//! `body: String`. [`cited`] then grows two arms: a `Note` becomes an `agent_message_chunk` whose
//! text is the body (ACP has no diagnostic line either), and a `Suggest` becomes
//! [`crate::acp::ToolCallContent::Diff`] - the same construct [`edit`] builds, with `lines` as
//! `oldText` and `body` as `newText`, inside a `tool_call_update` for a call this adapter invents.
//! A `Cite` stays what it is below. Nothing in `session.rs` changes except passing the kind.

use letibot_transcript::ToolEditExcerpt;

use crate::acp::{ContentBlock, Diff, SessionUpdate, ToolCallContent};
use crate::context::uri_for;

/// One fence the model addressed at a place, as the event carries it today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cited<'a> {
    /// The model's own label for the fence.
    pub label: &'a str,
    pub path: &'a str,
    /// 1-based and inclusive, as `Citations` spells them.
    pub start: u32,
    pub end: u32,
}

/// The update a citation becomes: a link to the place it names, with the lines in the fragment.
///
/// A chunk rather than a tool call, because the reply is what pointed at the file and the links
/// arrive where the prose did. A client that follows it lands on those lines; one that draws links
/// as chips gets the model's label as the title.
pub fn cited(citation: &Cited<'_>) -> Option<SessionUpdate> {
    if citation.path.is_empty() {
        return None;
    }
    let name = std::path::Path::new(citation.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(citation.path)
        .to_string();
    Some(SessionUpdate::AgentMessageChunk {
        content: ContentBlock::ResourceLink {
            uri: uri_for(citation.path, citation.start, citation.end),
            name,
            title: Some(citation.label.to_string()).filter(|l| !l.is_empty()),
            mime_type: None,
        },
    })
}

/// One changed region as ACP's `diff`.
///
/// The excerpt is the region that differs, not the whole file: `ToolEditExcerpt`'s `before` and
/// `after` are what the call changed, bounded, and a client draws exactly that as the hunk. A file
/// the call created has no `oldText` at all, which is the schema's spelling of "there was nothing
/// here".
pub fn edit(excerpt: &ToolEditExcerpt, workspace: &str) -> ToolCallContent {
    let path = absolute(workspace, &excerpt.path);
    Diff {
        path,
        old_text: (!excerpt.created).then(|| excerpt.before.clone()),
        new_text: excerpt.after.clone(),
    }
    .into()
}

/// The path a session's own names resolve against: an excerpt's path is relative to the session
/// root, and ACP wants the absolute one a client can open.
pub fn absolute(workspace: &str, path: &str) -> String {
    if path.starts_with('/') || path.is_empty() || workspace.is_empty() {
        return path.to_string();
    }
    std::path::Path::new(workspace)
        .join(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn an_excerpt(created: bool) -> ToolEditExcerpt {
        ToolEditExcerpt {
            path: "docs/article.md".into(),
            created,
            before_start: 41,
            after_start: 41,
            before_lines: 3,
            after_lines: 4,
            truncated: false,
            before: "1865\n".into(),
            after: "1866\nand the gap\n".into(),
        }
    }

    /// A citation is drawn where the client can follow it: the absolute path in the URI and the
    /// model's own label as the title.
    #[test]
    fn a_citation_is_a_link_to_the_lines_it_names() {
        let update = cited(&Cited {
            label: "article.md:41-47",
            path: "/ws/docs/article.md",
            start: 41,
            end: 47,
        })
        .expect("a path is a place");
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["sessionUpdate"], "agent_message_chunk");
        assert_eq!(json["content"]["type"], "resource_link");
        assert_eq!(json["content"]["uri"], "file:///ws/docs/article.md#L41-L47");
        assert_eq!(json["content"]["name"], "article.md");
        assert_eq!(json["content"]["title"], "article.md:41-47");
        assert!(
            cited(&Cited {
                label: "",
                path: "",
                start: 1,
                end: 1
            })
            .is_none()
        );
    }

    /// The diff is the excerpt's two sides under the schema's names, with the path the client can
    /// open - and a created file has no old side.
    #[test]
    fn an_excerpt_is_a_diff_and_a_created_file_has_no_old_text() {
        let diff = edit(&an_excerpt(false), "/ws").into_diff();
        assert_eq!(diff.path, "/ws/docs/article.md");
        assert_eq!(diff.old_text.as_deref(), Some("1865\n"));
        assert_eq!(diff.new_text, "1866\nand the gap\n");
        let created = edit(&an_excerpt(true), "/ws").into_diff();
        assert_eq!(created.old_text, None);
        assert_eq!(
            serde_json::to_value(ToolCallContent::Diff(created)).unwrap(),
            json!({
                "type": "diff",
                "path": "/ws/docs/article.md",
                "oldText": null,
                "newText": "1866\nand the gap\n",
            })
        );
    }
}
