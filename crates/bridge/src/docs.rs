//! The deep documents, served as MCP resources: embedded at build time and read on demand, so the
//! contract behind a guide line costs nothing until a client asks for it.

use serde_json::{Value, json};

/// The type of every document here: all three are markdown from `docs/`.
pub const MIME: &str = "text/markdown";

/// One document this server publishes at a `letibot://` uri.
#[derive(Clone, Copy)]
pub struct Resource {
    pub uri: &'static str,
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub text: &'static str,
}

/// What `resources/list` answers, and the list the guide is held against.
pub const RESOURCES: [Resource; 3] = [
    Resource {
        uri: "letibot://rooms",
        name: "rooms",
        title: "The room: one host's chat, and one queue per tag",
        description: "docs/room.md: the room's contract, its kinds, its asks and its wire.",
        text: include_str!("../../../docs/room.md"),
    },
    Resource {
        uri: "letibot://peering",
        name: "peering",
        title: "Peering: two boxes' rooms, one LAN",
        description: "docs/peering.md: how two coordinators link, and the cookie they prove.",
        text: include_str!("../../../docs/peering.md"),
    },
    Resource {
        uri: "letibot://leticode",
        name: "leticode",
        title: "leticode: the seat on letibot's primitives",
        description: "docs/leticode.md: what a leticode session is, and what it may do.",
        text: include_str!("../../../docs/leticode.md"),
    },
];

/// `resources/list`. A resource has no schema and no arguments, which is the whole difference
/// from a tool: a client reads it when a question is about the contract.
pub fn list() -> Value {
    let resources: Vec<Value> = RESOURCES
        .iter()
        .map(|r| {
            json!({
                "uri": r.uri,
                "name": r.name,
                "title": r.title,
                "description": r.description,
                "mimeType": MIME,
            })
        })
        .collect();
    json!({ "resources": resources })
}

/// `resources/read`: the document, or `None` for a uri this build does not publish. The MCP shape
/// is one `contents` entry per uri, carrying the whole text.
pub fn read(uri: &str) -> Option<Value> {
    let resource = RESOURCES.iter().find(|r| r.uri == uri)?;
    Some(json!({
        "contents": [{
            "uri": resource.uri,
            "mimeType": MIME,
            "text": resource.text,
        }]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every document is embedded and is the file `docs/` holds: a resource that shipped an empty
    /// string would answer a client with silence and no error.
    #[test]
    fn every_document_is_embedded_whole() {
        for resource in RESOURCES {
            assert!(resource.text.len() > 2000, "{}", resource.uri);
            assert!(
                resource.text.starts_with("# "),
                "{} is not the markdown from docs/",
                resource.uri
            );
        }
    }

    /// A uri this build does not publish is answered by name, not with an empty document.
    #[test]
    fn an_unknown_uri_is_not_a_document() {
        assert!(read("letibot://rooms").is_some());
        assert!(read("letibot://nope").is_none());
    }
}
