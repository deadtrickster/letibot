//! What a reply's line-addressed marks (`SessionEvent::Citations`, `ToolFinished::edits`) become in
//! ACP, the one place that mapping lives. ACP has no editor mark, so a mark rides something the
//! client already draws.

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

/// The update a citation becomes: a link to the place, with the lines in the fragment. A chunk
/// rather than a tool call, because the reply pointed at the file and the links arrive with the
/// prose.
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

/// One changed region as ACP's `diff`: the bounded `before` and `after` of what the call changed,
/// not the whole file. A created file has no `oldText`, the schema's spelling of "nothing was
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
            note: String::new(),
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
