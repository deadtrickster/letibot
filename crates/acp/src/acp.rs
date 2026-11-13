//! The subset of ACP v1 this adapter speaks, typed so a renamed field is a compile error.
//!
//! Read from `zed-industries/agent-client-protocol` at `c392d7e`, schema `v1.31.0`
//! (`schema/v1/schema.json`) and `docs/protocol/v1/*.mdx`. `PROTOCOL_VERSION` is that schema's
//! integer version, which ACP bumps only for a breaking change (`schema/v1/ProtocolVersion`).
//! Only what is used is here: an unknown field from a newer client is ignored, and an unknown
//! content block becomes [`ContentBlock::Other`] rather than refusing the prompt.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The ACP version this adapter answers. `1` is the stable schema; `2` is `2.0.0-alpha`.
///
/// [Initialization](https://agentclientprotocol.com/protocol/v1/initialization)
pub const PROTOCOL_VERSION: u16 = 1;

/// JSON-RPC codes ACP names itself (`$defs/ErrorCode`), plus the transport's own.
pub mod code {
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL: i32 = -32603;
    /// ACP's own range starts at -32000; `-32000` is what a session that cannot be opened uses.
    pub const SESSION: i32 = -32000;
}

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

/// `initialize`'s params. `clientCapabilities` and `clientInfo` are ignored: this adapter uses no
/// client capability, and reads its own limits from the daemon.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    pub protocol_version: u16,
}

/// `initialize`'s result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u16,
    pub agent_capabilities: AgentCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<Implementation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// `session/load` is not offered: the daemon's snapshot is not a conversation replay, and a
    /// client that was promised one would draw a history it was never given.
    pub load_session: bool,
    pub prompt_capabilities: PromptCapabilities,
    pub mcp_capabilities: McpCapabilities,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    pub image: bool,
    pub audio: bool,
    /// ACP says `resource` blocks are the preferred way to include context, and this adapter
    /// turns them into the editor context the daemon already renders (L4).
    pub embedded_context: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpCapabilities {
    pub http: bool,
    pub sse: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Implementation {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub version: String,
}

// ---------------------------------------------------------------------------
// session/new, session/prompt, session/cancel
// ---------------------------------------------------------------------------

/// `session/new`'s params. `mcpServers` is required by the schema and unused here: a leticode
/// session's tools are the daemon's, seated by its config and not by a client.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionRequest {
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    pub session_id: String,
    pub prompt: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelNotification {
    pub session_id: String,
}

/// Why a turn ended. `cancelled` MUST be answered after a `session/cancel`, whatever the turn was
/// doing ([Cancellation](https://agentclientprotocol.com/protocol/v1/prompt-turn#cancellation)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

// ---------------------------------------------------------------------------
// content blocks
// ---------------------------------------------------------------------------

/// One piece of a prompt or of the agent's output. ACP's `ContentBlock`, cut to the variants that
/// can reach a letibot turn: an image or audio block needs a capability this adapter does not
/// advertise, and an unknown one from a newer client is skipped rather than refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// A whole resource, or the piece of one the client captured. `uri` may carry a `#L10-14`
    /// fragment naming the lines - see [`crate::context::place`].
    Resource {
        resource: EmbeddedResource,
        #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
    },
    /// A reference to something the agent can read itself, with the same optional fragment.
    ResourceLink {
        uri: String,
        #[serde(default)]
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Audio {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(other)]
    Other,
}

/// A resource's text or bytes. Only one of `text` and `blob` is set; a blob has no place in a
/// prompt here, because the daemon's attachment door wants a path and a client's bytes would have
/// to be written somewhere first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedResource {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

// ---------------------------------------------------------------------------
// session/update
// ---------------------------------------------------------------------------

/// The `session/update` notification's params.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotification {
    pub session_id: String,
    pub update: SessionUpdate,
}

/// One update. The `sessionUpdate` tag is the schema's discriminant
/// ([SessionUpdate](https://agentclientprotocol.com/protocol/v1/prompt-turn#session-updates)).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "sessionUpdate",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SessionUpdate {
    /// A chunk of the reply. `messageId` is omitted: the daemon's rows are the message
    /// boundaries and a chunk stream is one message per turn.
    AgentMessageChunk { content: ContentBlock },
    /// A chunk of the model's reasoning, which the daemon publishes as its own delta target.
    AgentThoughtChunk { content: ContentBlock },
    /// A call starting.
    ToolCall {
        tool_call_id: String,
        title: String,
        kind: ToolKind,
        status: ToolCallStatus,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        locations: Vec<ToolCallLocation>,
    },
    /// A call's status or its result. Only the changed fields are sent.
    ToolCallUpdate {
        tool_call_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<ToolCallStatus>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<Vec<ToolCallContent>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        locations: Option<Vec<ToolCallLocation>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallLocation {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// What a call produced. A diff is the whole reason this adapter has this type: a tool's changed
/// lines are what ACP clients draw in their editor
/// ([Diffs](https://agentclientprotocol.com/protocol/v1/tool-calls#diffs)).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCallContent {
    Diff(Diff),
}

impl From<Diff> for ToolCallContent {
    fn from(diff: Diff) -> Self {
        ToolCallContent::Diff(diff)
    }
}

impl ToolCallContent {
    /// The diff this content is. One variant today; a second one added here would break this on
    /// purpose, because every caller wants the two sides.
    pub fn into_diff(self) -> Diff {
        match self {
            ToolCallContent::Diff(diff) => diff,
        }
    }
}

/// A file's two sides. `oldText` is `None` for a file the call created, which the schema reads as
/// "there was nothing here" ([`Diff`](https://agentclientprotocol.com/protocol/v1/tool-calls#diffs)).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diff {
    pub path: String,
    pub old_text: Option<String>,
    pub new_text: String,
}

// ---------------------------------------------------------------------------
// session/request_permission
// ---------------------------------------------------------------------------

/// A question the agent asks the client's person, and the one client-side method this adapter
/// calls: a letibot decision is a tool permission, so it is asked where the person is looking.
///
/// [Requesting permission](https://agentclientprotocol.com/protocol/v1/tool-calls#requesting-permission)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionRequest {
    pub session_id: String,
    pub tool_call: ToolCallUpdateRequest,
    pub options: Vec<PermissionOption>,
}

/// The `toolCall` a permission request carries: the schema's `ToolCallUpdate` without a status
/// change, which is how the client names the call in its own log.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallUpdateRequest {
    pub tool_call_id: String,
    pub title: String,
    pub kind: ToolKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: PermissionOptionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// `session/request_permission`'s result. `cancelled` is what a client MUST answer for every
/// pending request once it cancels the turn; the adapter turns it into an interrupt.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionResponse {
    pub outcome: PermissionOutcome,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PermissionOutcome {
    Cancelled,
    Selected {
        #[serde(rename = "optionId")]
        option_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The tags are the contract: a client reads `sessionUpdate`, `toolCallId` and `diff` by those
    /// exact names, so a rename is proved here rather than in a script.
    #[test]
    fn a_tool_call_update_carries_a_diff_under_the_schemas_names() {
        let update = SessionUpdate::ToolCallUpdate {
            tool_call_id: "call-1".into(),
            status: Some(ToolCallStatus::Completed),
            content: Some(vec![ToolCallContent::Diff(Diff {
                path: "/w/a.md".into(),
                old_text: Some("old\n".into()),
                new_text: "new\n".into(),
            })]),
            locations: None,
        };
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "completed",
                "content": [{
                    "type": "diff",
                    "path": "/w/a.md",
                    "oldText": "old\n",
                    "newText": "new\n",
                }],
            })
        );
    }

    /// A block from a newer client is skipped, not a refusal: the same tolerance the head's own
    /// client has, for the same reason.
    #[test]
    fn an_unknown_content_block_does_not_refuse_the_prompt() {
        let blocks: Vec<ContentBlock> = serde_json::from_value(json!([
            {"type": "text", "text": "hello"},
            {"type": "video", "uri": "file:///a.mp4"},
            {"type": "resource_link", "uri": "file:///a.md", "name": "a.md"},
        ]))
        .expect("the prompt parses");
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[1], ContentBlock::Other);
        assert!(matches!(blocks[2], ContentBlock::ResourceLink { .. }));
    }

    /// `cancelled` and `selected` are two different shapes of one field, and the client sends both.
    #[test]
    fn a_permission_answer_is_read_as_the_schema_spells_it() {
        let cancelled: RequestPermissionResponse =
            serde_json::from_value(json!({"outcome": {"outcome": "cancelled"}})).unwrap();
        assert!(matches!(cancelled.outcome, PermissionOutcome::Cancelled));
        let selected: RequestPermissionResponse =
            serde_json::from_value(json!({"outcome": {"outcome": "selected", "optionId": "a"}}))
                .unwrap();
        match selected.outcome {
            PermissionOutcome::Selected { option_id } => assert_eq!(option_id, "a"),
            other => panic!("expected a selected outcome, got {other:?}"),
        }
    }
}
