//! OpenAI's own protocol: `POST /v1/responses`, streamed.
//!
//! Every field below is off the API reference's OpenAPI document
//! (`openai/openai-openapi`, `openapi.yaml`, read 2026-11) - the create request
//! (`CreateResponse`) and its `input` item list (`InputItem`), the SSE events, and
//! `ResponseUsage`. The parts a `messages` API does not have at all:
//!
//! - `input` is a list of ITEMS, not turns: a message, a `function_call`, a
//!   `function_call_output`, and a `reasoning` item each stand on their own, and a
//!   tool result names the call it answers by `call_id` rather than by position.
//! - A thought comes back as a `reasoning` item carrying `encrypted_content`, which
//!   only the account that issued it can read. `store: false` means OpenAI keeps
//!   nothing, so unless letibot hands the item back the next turn has no memory of
//!   why the call was made: that continuity is the reason this protocol is here.
//!
//! ```text
//! include: ["reasoning.encrypted_content"]   "Includes an encrypted version of reasoning
//!                                             tokens in reasoning item outputs. This enables
//!                                             reasoning items to be used in multi-turn
//!                                             conversations when using the Responses API
//!                                             statelessly (like when the store parameter is
//!                                             set to false …)"
//! ```
//!
//! Where the record lives is the one decision this module makes that the transcript
//! does not: the row's `ReasoningField` keeps an Anthropic signature, which any key for
//! that door can replay, but this ciphertext is readable by one account only, so it is
//! not put on the row (`Completion::thoughts` stays empty). This client keeps what it
//! issued, in THE INSTANCE, paired
//! back to a row by the summary text the row was written from - see [`Issued`] and
//! [`pairing`]. It is written nowhere else, so it dies with the provider: after a restart,
//! or after `/models` switched this session to another door, those thoughts are simply not
//! replayed (the transcript still has their summary) rather than replayed as a ciphertext
//! another key would have to read. That is the smaller loss, and it is also why a
//! `Marks`-style session record would be wrong here: it would outlive the account and the
//! model that issued the item.
//!
//! Two shapes are deliberate and measured elsewhere in this crate: a picture follows
//! the refusal ladder `messages.rs` documents (send it, hear the refusal, name it from
//! then on), and a request that would END on an instruction is said as the operator's
//! turn instead.

use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use letibot_backend::{
    BackendCaps, BackendError, Completion, Delta, Finish, MessagesBackend, Meter, StreamFlow,
    TurnCost, TurnRequest,
};
use letibot_transcript::{ToolCall, TranscriptItem, UserPart};
use serde_json::{Value, json};

use crate::keys::Credentials;
use crate::messages::{Eyes, Withheld, not_sent_said, picture_fate};
use crate::presets::Preset;

/// How long the provider may take to send its first byte. The headers go out with the
/// first event, so a door that says nothing is caught here rather than at the answer's
/// budget.
pub const DEFAULT_FIRST_BYTE: Duration = Duration::from_secs(60);

/// The budget for the whole stream once bytes are arriving: a long answer is progress.
pub const DEFAULT_ANSWER_TIMEOUT: Duration = Duration::from_secs(600);

/// The most reasoning records one conversation keeps here. A record is one round's
/// ciphertext, so this is the recent tail: an older thought is replayed as nothing
/// rather than as a wrong blob, and the trim is from the front, which is the turn
/// compaction drops first anyway.
pub const LEDGER_MAX: usize = 32;

/// One reasoning item this client issued, and what pairs it back to a transcript row.
///
/// ```text
/// {"type": "response.output_item.done", "item": {"type": "reasoning", "id": "rs_07…",
///   "encrypted_content": "gAAAAAB…", "summary": [{"type": "summary_text", "text": "…"}]}}
/// ```
///
/// The summary is the key because it is the only part of the item the transcript keeps:
/// the engine writes `done.reasoning` - the summary deltas, joined - into one row, so a
/// row and the record that answers it say the same words.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Issued {
    summary: String,
    item_id: String,
    encrypted: String,
}

/// One provider, one model, one key, over the Responses protocol.
pub struct ResponsesProvider {
    preset: &'static Preset,
    model: String,
    url: String,
    creds: Credentials,
    agent: ureq::Agent,
    /// The two deadlines `agent` was built from, kept so setting one never resets the other.
    first_byte: Duration,
    answer: Duration,
    /// Ask the provider to think, where the operator has not said otherwise per request.
    pub thinking: bool,
    /// `--effort`, in this protocol's words: `reasoning.effort`. `None` leaves the
    /// model's own default, because a guess here is billed.
    pub effort: Option<String>,
    /// `temperature` and `top_p`, passed through; see [`ResponsesProvider::body_for`].
    pub sampling: Value,
    /// The catalogue this provider looks a model up in: the box's by default, and a
    /// caller's own when a test must not read the machine's files.
    catalogue: std::sync::Arc<crate::catalogue::Catalogue>,
    /// Where a fact the operator must read is said: the daemon's hub, one crate up.
    notes: Option<Notes>,
    /// The reasoning items this client issued and must carry back; see the module docs.
    issued: std::sync::Mutex<Vec<Issued>>,
}

/// Where a provider says something a person has to read. The same closure shape the
/// chat-completions client takes, so the daemon hands both the same one.
pub type Notes = crate::openai::Notes;

impl ResponsesProvider {
    pub fn new(preset: &'static Preset, model: Option<&str>, creds: Credentials) -> Self {
        let url = creds.url.clone().unwrap_or_else(|| preset.url.to_string());
        let catalogue = crate::catalogue::Catalogue::load();
        ResponsesProvider {
            preset,
            model: model
                .map(str::to_string)
                .unwrap_or_else(|| preset.default_model(&catalogue)),
            url,
            creds,
            catalogue,
            agent: Self::agent(DEFAULT_FIRST_BYTE, DEFAULT_ANSWER_TIMEOUT),
            first_byte: DEFAULT_FIRST_BYTE,
            answer: DEFAULT_ANSWER_TIMEOUT,
            thinking: false,
            effort: None,
            sampling: json!({}),
            notes: None,
            issued: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The HTTP agent, with the two numbers that decide when a door that has said nothing
    /// is a failure rather than a hang.
    fn agent(first_byte: Duration, answer: Duration) -> ureq::Agent {
        let config = ureq::Agent::config_builder()
            // A 4xx/5xx is an answer to read, not an error to unwrap.
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(first_byte))
            .timeout_global(None)
            .timeout_recv_body(Some(answer))
            .build();
        ureq::Agent::new_with_config(config)
    }

    /// How long this provider may take to answer at all, over [`DEFAULT_ANSWER_TIMEOUT`].
    pub fn with_response_timeout(mut self, answer: Duration) -> Self {
        self.answer = answer;
        self.agent = Self::agent(self.first_byte.min(answer), answer);
        self
    }

    /// How long this provider may stay silent before its first byte.
    pub fn with_first_byte(mut self, first_byte: Duration) -> Self {
        self.first_byte = first_byte;
        self.agent = Self::agent(first_byte.min(self.answer), self.answer);
        self
    }

    /// The first-byte and answer deadlines in force.
    pub fn deadlines(&self) -> (Duration, Duration) {
        (self.first_byte.min(self.answer), self.answer)
    }

    /// The operator's `--effort`, as `reasoning.effort`. A word the API does not document
    /// is dropped rather than sent: an unknown enum value is a 400 on every turn, and the
    /// model's own default is the honest answer to a word this build cannot read.
    pub fn with_effort(mut self, effort: Option<&str>) -> Self {
        self.effort = effort.and_then(effort_word).map(str::to_string);
        self
    }

    /// The catalogue this provider reads, instead of the box's.
    pub fn with_catalogue(
        mut self,
        catalogue: std::sync::Arc<crate::catalogue::Catalogue>,
    ) -> Self {
        self.catalogue = catalogue;
        self
    }

    /// Install the way this provider tells a person something - see [`Notes`].
    pub fn with_notes(mut self, notes: Notes) -> Self {
        self.notes = Some(notes);
        self
    }

    /// What this model gets pictures as: a provider that already refused one, then the
    /// catalogue's `modalities.input`, and otherwise the pictures are SENT. The same three
    /// answers as the chat-completions client, off the same table, because it is a fact
    /// about the model and not about the protocol.
    fn eyes(&self) -> Eyes<'_> {
        if crate::messages::known_text_only(self.preset.name, &self.model) {
            return Eyes::TextOnly(&self.model);
        }
        match self
            .catalogue
            .takes_images(self.preset.catalogue_id, &self.model)
        {
            Some(true) => Eyes::Sees,
            Some(false) => Eyes::TextOnly(&self.model),
            None => Eyes::Unknown(&self.model),
        }
    }

    /// The reasoning records this client holds, oldest first.
    fn ledger(&self) -> Vec<Issued> {
        self.issued
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Keep the records one answered round issued, and forget the oldest beyond [`LEDGER_MAX`].
    fn keep(&self, fresh: Vec<Issued>) {
        if fresh.is_empty() {
            return;
        }
        let mut held = self.issued.lock().unwrap_or_else(|e| e.into_inner());
        held.extend(fresh);
        let over = held.len().saturating_sub(LEDGER_MAX);
        held.drain(..over);
    }

    /// The request body, for a test to look at.
    pub fn body(&self, req: &TurnRequest<'_>) -> Result<Value, BackendError> {
        self.body_for(req, self.eyes(), Withheld::nothing())
    }

    /// The same body, with what the model gets pictures as given rather than decided: the
    /// retry after a refusal is the one caller.
    fn body_for(
        &self,
        req: &TurnRequest<'_>,
        eyes: Eyes<'_>,
        withheld: Withheld<'_>,
    ) -> Result<Value, BackendError> {
        let tools = tools(req.tools_json).map_err(BackendError::Malformed)?;
        let ledger = self.ledger();
        let (instructions, input) = input_items(req, eyes, withheld, &ledger);
        let mut body = json!({
            "model": self.model,
            "input": Value::Array(input),
            "stream": true,
            // The record is letibot's, so the account keeps none of it: `store` defaults to
            // `true`, which would put every turn's conversation in OpenAI's retention for
            // thirty days whether or not this daemon ever asked for it again.
            "store": false,
            // What makes a reasoning item replayable at all. Without it the item carries an
            // id this build cannot read back, which is a 400 on the next turn (see below).
            "include": ["reasoning.encrypted_content"],
            // Obfuscation pads delta events with random bytes to even out their size. This
            // reader parses by field name and would only pay for the padding.
            "stream_options": {"include_obfuscation": false},
        });
        if let Some(system) = instructions {
            body["instructions"] = Value::String(system);
        }
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
        // `reasoning.effort` is `none|minimal|low|medium|high|xhigh|max`, and `none` is how a
        // cheap round asks for no thinking at all. `summary: auto` is what puts
        // `response.reasoning_summary_text.delta` on the stream: without it a reasoning model
        // thinks where this build cannot see, and the transcript row that pairs an encrypted
        // item back would never be written.
        //
        // An explicit per-request answer wins, and otherwise the operator's own switch does: a
        // session started with `--thinking`, or with an `--effort` this API understands, asked to
        // think. Absent on every other turn, because `reasoning` on a model that does not reason
        // is a 400 on every turn rather than a setting nobody used.
        let thinking = match req.thinking {
            Some(asked) => asked,
            None => self.thinking || self.effort.is_some(),
        };
        if thinking {
            let mut reasoning = json!({"summary": "auto"});
            if let Some(effort) = &self.effort {
                reasoning["effort"] = Value::String(effort.clone());
            }
            body["reasoning"] = reasoning;
        } else if req.thinking == Some(false) {
            body["reasoning"] = json!({"effort": "none"});
        }
        // The operator's sampling, in this protocol's names. `temperature` and `top_p` are the
        // two it documents (`ModelResponseProperties`); a chat-completions `max_tokens` is
        // `max_output_tokens` here, and the rest (`seed`, the penalties) are fields this API
        // does not have, so they are not sent rather than sent to be refused.
        if let Some(obj) = self.sampling.as_object() {
            for (k, v) in obj {
                let field = match k.as_str() {
                    "temperature" | "top_p" => k.as_str(),
                    "max_tokens" => "max_output_tokens",
                    _ => continue,
                };
                body[field] = v.clone();
            }
        }
        // The request's cap is a ceiling the operator's sampling cannot lift: a cheap turn's
        // whole point is that it cannot run long.
        if let Some(n) = req.max_output_tokens {
            let set = body["max_output_tokens"].as_u64().unwrap_or(u64::MAX);
            body["max_output_tokens"] = json!(set.min(u64::from(n)));
        }
        Ok(body)
    }
}

/// `--effort` in the words `ReasoningEffort` documents, or `None` for a word this build
/// cannot map. The daemon's own flag offers four of the seven, so the rest are here for a
/// config file that spells one.
fn effort_word(s: &str) -> Option<&'static str> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "none" => "none",
        "minimal" => "minimal",
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "xhigh" => "xhigh",
        "max" => "max",
        _ => return None,
    })
}

/// Tool schemas as this API wants them: flat, where a chat-completions tool nests the
/// definition under `function`.
///
/// ```text
/// {"type": "function", "name": "read", "description": "…",
///  "parameters": {"type": "object", …}, "strict": false}
/// ```
///
/// `strict` is required by the schema and is `false` here: strict mode demands that every
/// object in `parameters` set `additionalProperties: false`, and the harness's schemas do
/// not, so `true` would be refused for reasons that have nothing to do with the call.
///
/// The harness hands over its dialect's rendering, either `{type, function: {...}}` already
/// or the bare `{name, description, parameters}`; a schema with neither is refused rather
/// than sent as something the provider would 400 on.
pub fn tools(tools_json: &[String]) -> Result<Vec<Value>, String> {
    tools_json
        .iter()
        .map(|t| {
            let v: Value =
                serde_json::from_str(t).map_err(|e| format!("a tool schema is not JSON: {e}"))?;
            let def = match v.get("function") {
                Some(f) => f.clone(),
                None if v.get("name").is_some() => v.clone(),
                None => {
                    return Err(format!(
                        "a tool schema has neither `function` nor `name`: {}",
                        t.chars().take(80).collect::<String>()
                    ));
                }
            };
            Ok(json!({
                "type": "function",
                "name": def.get("name").cloned().unwrap_or(Value::Null),
                "description": def.get("description").cloned().unwrap_or(Value::Null),
                "parameters": def.get("parameters").cloned().unwrap_or(Value::Null),
                "strict": false,
            }))
        })
        .collect()
}

/// One input item on its way to the wire, tagged with what the pairing pass needs: which
/// call it answers, if any.
struct Input {
    item: Value,
    /// The `call_id` this item answers (`function_call_output` only).
    answers: Option<String>,
    /// An output answering a call no model proposed - the operator ran it themselves.
    own_author: bool,
}

/// The `instructions` string and the `input` array, from the transcript.
///
/// `instructions` is the top-level prompt (`CreateResponse.instructions`: "A system (or
/// developer) message inserted into the model's context"), so a request whose transcript
/// LEADS with a `System` row keeps that row as a message instead and leaves this empty -
/// the same rule `messages::convert` applies to its system message, for the same reason:
/// the prefix cache is keyed on the bytes, and a prompt that moved between the two places
/// re-prefills every turn.
fn input_items(
    req: &TurnRequest<'_>,
    eyes: Eyes<'_>,
    withheld: Withheld<'_>,
    ledger: &[Issued],
) -> (Option<String>, Vec<Value>) {
    let leads_with_system = matches!(req.items.first(), Some(TranscriptItem::System { .. }));
    let instructions = if leads_with_system || req.system.trim().is_empty() {
        None
    } else {
        Some(req.system.to_string())
    };
    let mut rows: Vec<Input> = Vec::with_capacity(req.items.len() + req.tail.len());
    // The cursor is what keeps a record from answering two rows: a row may only pair with a
    // record at or after the one the previous row took, because both are in stream order.
    let mut cursor = 0usize;
    // The reasoning this round made, held for the message or calls it belongs to and across
    // intervening items, exactly as `messages::convert` holds it: the item must sit before
    // what it produced, and a steering row can land in between.
    let mut held: Vec<Value> = Vec::new();
    for item in req.items.iter().chain(req.tail.iter()) {
        match item {
            TranscriptItem::System { text, .. } => {
                rows.push(plain(json!({"role": "developer", "content": text})));
            }
            // A delivery reaches the model where a notice has always reached it: as the
            // harness's own words in a user turn.
            TranscriptItem::Notice { text, .. } => {
                rows.push(plain(json!({"role": "user", "content": text})));
            }
            TranscriptItem::User { parts, .. } => {
                rows.push(plain(user_message(parts, eyes, withheld)));
            }
            TranscriptItem::Reasoning { text, .. } => {
                for record in pairing(text, ledger, &mut cursor) {
                    held.push(reasoning_item(record));
                }
            }
            // Display-only: a segment mark delimits a run and an offer is what the operator
            // was shown. Neither is a token.
            TranscriptItem::SegmentMark { .. } | TranscriptItem::Offer { .. } => {}
            TranscriptItem::Assistant {
                text, tool_calls, ..
            } => {
                rows.extend(held.drain(..).map(plain));
                if !text.is_empty() {
                    // `output_text` is the part type an assistant message carries on the way
                    // back in (`InputContent`), and a plain string is the other accepted shape;
                    // the array is the one that says the role produced text.
                    rows.push(plain(json!({
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}],
                    })));
                }
                for call in tool_calls {
                    rows.push(plain(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": call.arguments,
                    })));
                }
            }
            TranscriptItem::ToolResult {
                call_id,
                // The payload names its own outcome, so neither is needed here.
                name: _,
                outcome: _,
                payload,
                // Display-only; the prompt never sees it.
                edits: _,
                origin,
                media,
            } => {
                let own_author = matches!(
                    origin,
                    Some(letibot_transcript::CallOrigin::Operator { .. })
                );
                // A picture a tool read rides the OUTPUT of the call that read it: this API's
                // `function_call_output.output` is text or a content list, so the two need no
                // second message to be attached to (unlike chat completions, which has no
                // such field and appends a user turn).
                let output = match media {
                    Some(m) => match picture_fate(&m.data_ref, eyes, withheld) {
                        None => json!([
                            {"type": "input_text", "text": payload},
                            {"type": "input_image", "image_url": m.data_ref},
                        ]),
                        Some(why) => json!(format!(
                            "{payload}\n\nAttached media from tool result: {}",
                            not_sent_said(1, &m.mime, Some(m.bytes), &why)
                        )),
                    },
                    None => json!(payload),
                };
                rows.push(Input {
                    item: json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output,
                    }),
                    answers: Some(call_id.clone()),
                    own_author,
                });
            }
        }
    }
    // A trailing reasoning item whose assistant never came is not an item: it would be a
    // thought with nothing it produced, and the next request carries the transcript again.
    rows.extend(held.drain(..).map(plain));
    let mut out = pair_outputs(rows);
    trailing_developer_becomes_user(&mut out);
    (instructions, out)
}

/// A request may not END on an instruction, and this is the shape that cost this tree hours:
/// the compaction appends its instruction AFTER the conversation, so the request ends on a
/// system message - which a provider with tools refuses with an error that names a field
/// instead of the shape. The record is right and the translation belongs on the transport, so
/// the last message is the operator's turn.
///
/// The lone developer message is the system PROMPT and stays one: translating it would send a
/// conversation whose only turn is the operator saying the prompt.
fn trailing_developer_becomes_user(out: &mut [Value]) {
    if out.len() < 2 {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("developer")
    {
        last["role"] = Value::String("user".into());
    }
}

/// One row that is not a tool result and answers nothing.
fn plain(item: Value) -> Input {
    Input {
        item,
        answers: None,
        own_author: false,
    }
}

/// A user row: a string when it is words, a content list when it carries a picture.
///
/// `input_text` and `input_image` are the two part types, and `image_url` on an image part
/// takes a URL - a `data:` URL, which is what a `UserPart::Image` holds. A model that takes
/// text only, or bytes a provider already refused, are NAMED instead, by the same rules the
/// chat-completions client follows: the placeholder is the tree's one answer to a picture
/// that cannot be sent, and it carries the reason rather than a blank.
fn user_message(parts: &[UserPart], eyes: Eyes<'_>, withheld: Withheld<'_>) -> Value {
    let mut content: Vec<Value> = Vec::new();
    let mut images = 0usize;
    for part in parts {
        match part {
            UserPart::Text { text } => content.push(json!({"type": "input_text", "text": text})),
            UserPart::Image {
                media_type,
                data_ref,
            } => {
                images += 1;
                match picture_fate(data_ref, eyes, withheld) {
                    None => content.push(json!({"type": "input_image", "image_url": data_ref})),
                    Some(why) => content.push(json!({
                        "type": "input_text",
                        "text": not_sent_said(
                            images,
                            media_type,
                            crate::messages::data_ref_bytes(data_ref),
                            &why,
                        ),
                    })),
                }
            }
            UserPart::FileRef { path, .. } => {
                content.push(json!({"type": "input_text", "text": path}))
            }
        }
    }
    // A message of words is a STRING: an array where a string was re-prefills the cache.
    let only_text = content
        .iter()
        .all(|p| p.get("type").and_then(Value::as_str) == Some("input_text"));
    if only_text {
        let text = content
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n");
        return json!({"role": "user", "content": text});
    }
    json!({"role": "user", "content": Value::Array(content)})
}

/// The `reasoning` input item, as the API issues it and therefore the only shape it takes
/// back: `type`, the `id` it minted, the summary it wrote, and the ciphertext.
fn reasoning_item(record: &Issued) -> Value {
    json!({
        "type": "reasoning",
        "id": record.item_id,
        "encrypted_content": record.encrypted,
        "summary": [{"type": "summary_text", "text": record.summary}],
    })
}

/// The records that answer one reasoning row, and move the cursor past them.
///
/// A row's text is the summary the stream carried, so it equals one record's `summary` -
/// or, when a round issued two reasoning items, the concatenation of both. The search
/// starts at the cursor and skips records that do not match: those belong to rows this
/// request no longer carries (compaction dropped them, or the round was never committed),
/// and skipping them is what stops one stale record from hiding every thought after it.
/// Nothing is ever paired against its order: a record that does not spell the row's text
/// is not used at all.
fn pairing<'a>(text: &str, ledger: &'a [Issued], cursor: &mut usize) -> Vec<&'a Issued> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut at = *cursor;
    while at < ledger.len() && !rest.is_empty() {
        let record = &ledger[at];
        at += 1;
        if record.summary.is_empty() || !rest.starts_with(record.summary.as_str()) {
            // Only before anything has paired: once a run has started, its next member is
            // the very next record, and a mismatch means the row says more than it issued.
            if out.is_empty() {
                continue;
            }
            break;
        }
        rest = &rest[record.summary.len()..];
        out.push(record);
    }
    if !out.is_empty() {
        *cursor = at;
    }
    out
}

/// Every `function_call` gets its `function_call_output`, and an output that answers
/// nothing is dropped unless the operator ran it.
///
/// The two shapes are the ones `messages.rs` fills for chat completions, for the same
/// reason: an output the provider will not accept is the smaller loss than a result the
/// model never sees. A row recorded WHILE the calls were running - a room note, a steering
/// message - waits behind the results rather than orphaning them, which is also what the
/// chat-completions path measured.
///
/// What a call may not do is go unanswered: an interrupted turn leaves exactly that, and the
/// gap is filled where its window closed rather than the request being sent to be refused.
fn pair_outputs(rows: Vec<Input>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    // The calls opened and not yet answered, in the order they were made.
    let mut owed: Vec<String> = Vec::new();
    // Rows recorded while calls were still running, held until the results land.
    let mut held: Vec<Value> = Vec::new();
    for row in rows {
        if let Some(id) = &row.answers {
            if let Some(at) = owed.iter().position(|o| o == id) {
                owed.remove(at);
                out.push(row.item);
                if owed.is_empty() {
                    out.append(&mut held);
                }
            } else if row.own_author {
                // No model proposed this call, so an output would answer nothing: the
                // person's own turn is what their `!` line actually was.
                let content = row.item.get("output").cloned().unwrap_or(Value::Null);
                let person = json!({"role": "user", "content": content});
                if owed.is_empty() {
                    out.push(person);
                } else {
                    held.push(person);
                }
            }
            continue;
        }
        let opens = row.item.get("type").and_then(Value::as_str) == Some("function_call");
        // A row between a call and its result waits; an assistant message is what closes the
        // answering window, since nothing after it can be answering these calls.
        if !opens && !owed.is_empty() && !closes_the_window(&row.item) {
            held.push(row.item);
            continue;
        }
        for id in std::mem::take(&mut owed) {
            out.push(unanswered(&id));
        }
        out.append(&mut held);
        if opens && let Some(id) = row.item.get("call_id").and_then(Value::as_str) {
            owed.push(id.to_string());
        }
        out.push(row.item);
    }
    for id in owed {
        out.push(unanswered(&id));
    }
    out.append(&mut held);
    out
}

/// The one row that ends the window a run of calls opened: an assistant message is the model
/// speaking again, and no later output can answer a call it has moved past.
fn closes_the_window(item: &Value) -> bool {
    item.get("role").and_then(Value::as_str) == Some("assistant")
}

fn unanswered(call_id: &str) -> Value {
    json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": "[no result was recorded for this call: the turn ended before it \
                   finished, or it was never run]",
    })
}

/// The SHAPE of a request that was refused: what was in it and how long each item is,
/// never the content. A log that quoted the conversation would be a second copy of the
/// transcript, and every diagnosis this is for - a field the provider will not take, an
/// item it will not accept in that position - is answerable from the shape.
fn shape(body: &Value, bytes: usize) -> String {
    let mut out = String::new();
    let n_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    out.push_str(&format!(
        "    {bytes} bytes, model={} tools={n_tools} stream={} store={} top-level-keys=[{}]",
        body.get("model").and_then(Value::as_str).unwrap_or("?"),
        body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        body.get("store").and_then(Value::as_bool).unwrap_or(true),
        body.as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>().join(" "))
            .unwrap_or_default(),
    ));
    let none: Vec<Value> = Vec::new();
    let input = body.get("input").and_then(Value::as_array).unwrap_or(&none);
    out.push_str(&format!("\n    {} input item(s):", input.len()));
    for (i, item) in input.iter().enumerate() {
        out.push_str(&format!("\n      [{i}] {}", one_item(item)));
    }
    out
}

/// One item of a [`shape`] dump. An absent key and a null one are different answers, and a
/// character count is what says a field was carried without saying what it said.
fn one_item(item: &Value) -> String {
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| item.get("role").and_then(Value::as_str))
        .unwrap_or("?");
    let mut bits = vec![kind.to_string()];
    if let Some(role) = item.get("role").and_then(Value::as_str)
        && item.get("type").is_some()
    {
        bits.push(format!("role={role}"));
    }
    for key in ["content", "output"] {
        bits.push(match item.get(key) {
            None => format!("{key}=absent"),
            Some(Value::Null) => format!("{key}=null"),
            Some(Value::String(s)) => format!("{key}={}ch", s.chars().count()),
            Some(Value::Array(a)) => format!("{key}=array({})", a.len()),
            Some(_) => format!("{key}=?"),
        });
    }
    for key in ["id", "call_id"] {
        match item.get(key).and_then(Value::as_str) {
            None => bits.push(format!("{key}=absent")),
            Some(id) => bits.push(format!("{key}={}", id.chars().take(10).collect::<String>())),
        }
    }
    if let Some(name) = item.get("name").and_then(Value::as_str) {
        bits.push(format!("name={name}"));
    }
    for key in ["encrypted_content", "arguments"] {
        if let Some(s) = item.get(key).and_then(Value::as_str) {
            bits.push(format!("{key}={}ch", s.chars().count()));
        }
    }
    bits.join(" ")
}

/// The io failure under a transport error, when there is one: a deadline must stay a
/// deadline for a caller that branches on it, and a sentence is what it must not have to read.
fn io_kind(err: &ureq::Error) -> Option<std::io::ErrorKind> {
    match err {
        ureq::Error::Timeout(_) => Some(std::io::ErrorKind::TimedOut),
        ureq::Error::Io(e) => Some(e.kind()),
        _ => None,
    }
}

/// Whether an interrupted send is asked again. One retry covers the daemon's own Ctrl-C,
/// and a loop that never gives up re-posts the whole history once per interruption.
fn ask_again(err: &ureq::Error, retried: &mut u32) -> bool {
    if let ureq::Error::Io(e) = err
        && e.kind() == std::io::ErrorKind::Interrupted
        && *retried == 0
    {
        *retried = 1;
        return true;
    }
    false
}

/// Is this refusal the one that means "not this kind of content" on THIS protocol? The
/// shared matcher reads a chat-completions refusal (`image_url`, a content type that
/// names text); this API's validator names the input part type instead.
///
/// ```text
/// Invalid value: 'input_image'. Supported values are: 'input_text'.
/// ```
fn text_only_refusal(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    let names_a_part = b.contains("input_image") || b.contains("input_image_content");
    let refuses_it = [
        "invalid",
        "not supported",
        "unsupported",
        "does not support",
    ]
    .iter()
    .any(|w| b.contains(w));
    crate::messages::text_only_refusal(body) || (names_a_part && refuses_it)
}

/// The refusal a stream carries, which has no status of its own: the response's is `200`
/// because the headers went out before the first event.
///
/// ```text
/// {"type": "error", "code": "rate_limit_exceeded", "param": null, "message": "…"}
/// {"type": "response.failed", "response": {"error": {"code": "server_error", …}}}
/// ```
///
/// The code is turned into the status this tree's retry policy reads, because that is the
/// only vocabulary there is: `rate_limit_exceeded` is a 429 (wait and ask again),
/// `server_error` a 500 (the same), and anything else a 400 (asking again gets the same
/// answer). The status the wire sent is 200 in all three cases and says nothing.
fn in_band_failure(err: &Value, status: u16, retry_after: Option<u64>) -> BackendError {
    let code = err.get("code").and_then(Value::as_str).unwrap_or("");
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| err.to_string());
    // The provider's own code goes in the sentence, for the reason the chat-completions
    // client keeps it: it is the one machine-readable word in a refusal, and a quota is
    // read off it (`insufficient_quota` is in the retry policy's money list).
    let said = if code.is_empty() || message.contains(code) {
        message
    } else {
        format!("{message} ({code})")
    };
    let status = match code {
        "rate_limit_exceeded" => 429,
        "server_error" => 500,
        _ => status.max(400),
    };
    BackendError::Refused {
        status,
        body: said,
        retry_after,
    }
}

/// What the deltas and the terminal events built up.
#[derive(Default)]
struct Accumulator {
    text: String,
    /// The summary text, which is the whole of what the transcript keeps of a thought.
    reasoning: String,
    /// How much of `reasoning` a record has already claimed; see [`Accumulator::record`].
    claimed: usize,
    calls: Vec<Call>,
    finish: Option<Finish>,
    usage: Option<Value>,
    /// The model the provider says it used, read off the stream. Not the model we asked
    /// for: several names are accepted and some are aliases, and the price follows the one
    /// that actually ran.
    model: Option<String>,
    /// The reasoning items this round issued, in the order they were done.
    issued: Vec<Issued>,
    /// The provider said the response was over (completed, incomplete or failed).
    ended: bool,
    /// The failure an in-band event carried, if any.
    failure: Option<BackendError>,
}

/// One function call the stream is building. `output_index` is the API's own slot, which
/// is what a delta names.
#[derive(Default)]
struct Call {
    call_id: String,
    name: String,
    arguments: String,
}

impl Accumulator {
    /// One SSE event → the deltas it carries, in order.
    fn absorb(&mut self, event: &Value) -> Vec<Delta> {
        let mut out = Vec::new();
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        if kind.starts_with("response.") {
            let response = event.get("response");
            if let Some(model) = response
                .and_then(|r| r.get("model"))
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
            {
                self.model = Some(model.to_string());
            }
            if let Some(usage) = response
                .and_then(|r| r.get("usage"))
                .filter(|u| !u.is_null())
            {
                self.usage = Some(usage.clone());
            }
        }
        match kind {
            // `response.output_text.delta` is the answer as it is written.
            "response.output_text.delta" => {
                if let Some(t) = event.get("delta").and_then(Value::as_str)
                    && !t.is_empty()
                {
                    self.text.push_str(t);
                    out.push(Delta::Text(t.to_string()));
                }
            }
            // `response.reasoning_summary_text.delta` is the thought as it is summarised:
            // the API's only streamed form of it (`reasoning.summary: auto`), and what the
            // transcript row says.
            "response.reasoning_summary_text.delta" => {
                if let Some(t) = event.get("delta").and_then(Value::as_str)
                    && !t.is_empty()
                {
                    self.reasoning.push_str(t);
                    out.push(Delta::Reasoning(t.to_string()));
                }
            }
            // A call's first sight: the item is opened with its name and `call_id`, and the
            // arguments follow as their own deltas. This is the only place the name and id
            // appear on the stream, so it is where a delta's `index` learns them.
            "response.output_item.added" => {
                let Some(item) = event.get("item") else {
                    return out;
                };
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let index = output_index(event);
                    let slot = self.slot(index);
                    if let Some(id) = item.get("call_id").and_then(Value::as_str) {
                        slot.call_id = id.to_string();
                    }
                    if let Some(name) = item.get("name").and_then(Value::as_str) {
                        slot.name = name.to_string();
                    }
                    out.push(Delta::ToolCall {
                        index,
                        id: (!slot.call_id.is_empty()).then(|| slot.call_id.clone()),
                        name: (!slot.name.is_empty()).then(|| slot.name.clone()),
                        arguments: String::new(),
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(t) = event.get("delta").and_then(Value::as_str)
                    && !t.is_empty()
                {
                    let index = output_index(event);
                    self.slot(index).arguments.push_str(t);
                    out.push(Delta::ToolCall {
                        index,
                        id: None,
                        name: None,
                        arguments: t.to_string(),
                    });
                }
            }
            // The complete argument string, which is the truth even if a delta was missed.
            "response.function_call_arguments.done" => {
                if let Some(args) = event.get("arguments").and_then(Value::as_str) {
                    let index = output_index(event);
                    self.slot(index).arguments = args.to_string();
                }
            }
            // The only place `encrypted_content` is complete: the API says an item in
            // `response.output_item.added` "may be incomplete", and to use the done event.
            "response.output_item.done" => self.item_done(event),
            "response.completed" => {
                self.ended = true;
                // The whole output list, in case an item's own `done` event was not read.
                self.harvest(event);
                if self.finish.is_none() {
                    self.finish = Some(self.stopped_on());
                }
            }
            // `incomplete_details.reason`: `max_output_tokens` is the request's own cap, which
            // is a length stop and not a failure; anything else (`content_filter`, `steered`)
            // is kept verbatim.
            "response.incomplete" => {
                self.ended = true;
                self.harvest(event);
                let reason = event
                    .get("response")
                    .and_then(|r| r.get("incomplete_details"))
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("incomplete");
                self.finish = Some(match reason {
                    "max_output_tokens" => Finish::Length,
                    other => Finish::Other(other.to_string()),
                });
            }
            "response.failed" => {
                self.ended = true;
                if let Some(err) = event.get("response").and_then(|r| r.get("error")) {
                    self.failure = Some(in_band_failure(err, 500, None));
                }
            }
            "error" => {
                self.ended = true;
                self.failure = Some(in_band_failure(event, 400, None));
            }
            _ => {}
        }
        out
    }

    /// One output item, finished. A reasoning item is the whole point of this module: it
    /// is claimed with the summary text it just wrote, so the transcript row that says the
    /// same words can find it again on the next turn.
    fn item_done(&mut self, event: &Value) {
        let Some(item) = event.get("item") else {
            return;
        };
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "message" => {
                // A provider that sent no deltas still has the text here.
                if self.text.is_empty()
                    && let Some(text) = message_text(item)
                {
                    self.text = text;
                }
            }
            "function_call" => {
                let index = output_index(event);
                let slot = self.slot(index);
                if let Some(id) = item.get("call_id").and_then(Value::as_str) {
                    slot.call_id = id.to_string();
                }
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    slot.name = name.to_string();
                }
                if let Some(args) = item.get("arguments").and_then(Value::as_str) {
                    slot.arguments = args.to_string();
                }
            }
            "reasoning" => self.record(item),
            _ => {}
        }
    }

    /// Keep one reasoning item, if it can be carried at all.
    fn record(&mut self, item: &Value) {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            return;
        };
        // Without the ciphertext an item is an id this build cannot read back: replaying it
        // is a 400, and the alternative is the summary alone, which the transcript already
        // has. So an item the account did not encrypt is not a record.
        let Some(encrypted) = item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
        else {
            return;
        };
        if self.issued.iter().any(|r| r.item_id == id) {
            return;
        }
        // The row this record answers is the text streamed SINCE the last record: a round
        // that issued two items writes one row of both summaries joined, and the pairing
        // walk re-joins them the same way.
        let summary = self.reasoning[self.claimed.min(self.reasoning.len())..].to_string();
        self.claimed = self.reasoning.len();
        self.issued.push(Issued {
            summary,
            item_id: id.to_string(),
            encrypted: encrypted.to_string(),
        });
    }

    /// The same harvest for a terminal event's `response.output` list.
    fn harvest(&mut self, event: &Value) {
        let Some(items) = event
            .get("response")
            .and_then(|r| r.get("output"))
            .and_then(Value::as_array)
        else {
            return;
        };
        for item in items {
            if item.get("type").and_then(Value::as_str) == Some("reasoning") {
                self.record(item);
            }
        }
    }

    /// Why the model stopped, off the output it produced: a response that asked for a
    /// call stopped on the calls, and one that only spoke stopped.
    fn stopped_on(&self) -> Finish {
        if self.calls.iter().any(|c| !c.name.is_empty()) {
            Finish::ToolCalls
        } else {
            Finish::Stop
        }
    }

    fn slot(&mut self, index: usize) -> &mut Call {
        while self.calls.len() <= index {
            self.calls.push(Call::default());
        }
        &mut self.calls[index]
    }

    /// The answer, and the reasoning records it issued for the caller to keep.
    fn finish(
        self,
        model: &str,
        creds: &Credentials,
        wall_ms: u64,
        preset: &'static crate::presets::Preset,
        catalogue: &crate::catalogue::Catalogue,
    ) -> (Completion, Vec<Issued>) {
        let u = self.usage.as_ref();
        let get = |k: &str| {
            u.and_then(|u| u.get(k))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        let prompt_tokens = get("input_tokens");
        let generated_tokens = get("output_tokens");
        let cached_tokens = u
            .and_then(|u| u.get("input_tokens_details"))
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        // `output_tokens_details.reasoning_tokens` is inside the generated count and inside
        // the price, so it is not a separate figure here: `raw_usage` keeps the whole object
        // for the metrics row that reads it.
        let billed = self.model.as_deref().unwrap_or(model);
        let micros_usd = creds
            .prices
            .get(billed)
            .copied()
            .or_else(|| {
                catalogue
                    .model(preset.catalogue_id, billed)
                    .and_then(|m| m.prices)
            })
            .map(|p| p.micros(prompt_tokens, cached_tokens, generated_tokens));
        let tool_calls = self
            .calls
            .into_iter()
            .filter(|c| !c.name.is_empty())
            .map(|c| ToolCall {
                // A call the model named is echoed back by that name - `function_call_output`
                // answers by `call_id`, so losing it would lose the pairing. One it did not
                // name gets an id no other turn holds.
                id: if c.call_id.is_empty() {
                    letibot_transcript::mint_call_id()
                } else {
                    c.call_id
                },
                name: c.name,
                arguments: c.arguments,
            })
            .collect();
        let finish = self.finish.unwrap_or(Finish::Stop);
        let completion = Completion {
            text: self.text,
            reasoning: self.reasoning,
            tool_calls,
            finish,
            cost: TurnCost {
                meter: Meter::Metered,
                prompt_tokens,
                cached_tokens,
                generated_tokens,
                wall_ms,
                micros_usd,
            },
            raw_usage: u.map(|u| u.to_string()),
            thoughts: Vec::new(),
        };
        (completion, self.issued)
    }
}

fn output_index(event: &Value) -> usize {
    event
        .get("output_index")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize
}

/// The text of a completed `message` item, its `output_text` parts joined.
fn message_text(item: &Value) -> Option<String> {
    let parts = item.get("content")?.as_array()?;
    let text: String = parts
        .iter()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect();
    (!text.is_empty()).then_some(text)
}

impl MessagesBackend for ResponsesProvider {
    fn caps(&self) -> BackendCaps {
        BackendCaps::METERED_API
    }

    fn name(&self) -> &str {
        self.preset.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    /// The host the request goes to, derived from `self.url` itself, which is the very
    /// string the post uses, so this cannot name a host the request did not reach.
    fn authority(&self) -> String {
        self.url
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(&self.url)
            .split('/')
            .next()
            .unwrap_or(&self.url)
            .to_string()
    }

    fn complete(
        &self,
        req: &TurnRequest<'_>,
        on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
    ) -> Result<Completion, BackendError> {
        let eyes = self.eyes();
        let first = self.round(req, on_delta, eyes, Withheld::nothing());
        // A refusal is read off the wire, never assumed: the status is 400 and the request
        // ACTUALLY carried a picture. Two different refusals answer that shape and they are
        // two different facts, exactly as the chat-completions client reads them.
        let Err(BackendError::Refused {
            status: 400,
            body: why,
            ..
        }) = &first
        else {
            return first;
        };
        if !eyes.takes_images()
            || !crate::messages::carries_pictures(req.items.iter().chain(req.tail.iter()))
        {
            return first;
        }
        // The first refusal: the MODEL takes text only. Marked for the rest of this daemon,
        // said to the operator once, and the same request goes again with placeholders. One
        // retry, never a ladder: a second refusal is the answer.
        if text_only_refusal(why) {
            if crate::messages::mark_text_only(self.preset.name, &self.model) {
                let said = format!(
                    "{} refused an image; images are now sent to it as placeholders (the provider \
                     said: {why}). The request went again without them, and this daemon will not \
                     send this model another picture.",
                    self.model
                );
                // The daemon's log first: a session may have no head attached.
                eprintln!("  {said}");
                if let Some(notes) = &self.notes {
                    notes(said);
                }
            }
            return self.round(
                req,
                on_delta,
                Eyes::TextOnly(&self.model),
                Withheld::nothing(),
            );
        }
        // The second refusal is a different fact: the PICTURE is what it would not take.
        // Answering it with a text-only mark would withhold every picture this model was
        // sent, so the marks go against the pictures and nothing is recorded until the retry
        // answers.
        if !crate::messages::picture_refusal(why) {
            return first;
        }
        let pictures: Vec<String> =
            crate::messages::pictures(req.items.iter().chain(req.tail.iter()))
                .into_iter()
                .map(String::from)
                .collect();
        if pictures.is_empty() {
            return first;
        }
        let trying: Vec<(String, String)> = pictures
            .iter()
            .map(|d| (crate::messages::picture_key(d), why.clone()))
            .collect();
        let second = self.round(req, on_delta, eyes, Withheld::trying(&trying));
        let said = match &second {
            Ok(_) => {
                let mut fresh = 0;
                for d in &pictures {
                    if crate::messages::mark_bad_picture(d, why) {
                        fresh += 1;
                    }
                }
                format!(
                    "{} refused {} picture(s) (the provider said: {why}). The request went again \
                     with them named instead of sent and was answered; this daemon will not send \
                     those bytes again{}. The pictures themselves are unchanged on disk.",
                    self.model,
                    pictures.len(),
                    if fresh == 0 {
                        " — they were already known"
                    } else {
                        ""
                    }
                )
            }
            Err(BackendError::Refused { body, .. }) => format!(
                "{} refused a picture (the provider said: {why}), and refused the request again \
                 with every picture in it withheld (the provider said: {body}). Nothing has been \
                 recorded against the pictures: the second refusal may be about something else, \
                 and the next turn will try them again.",
                self.model
            ),
            Err(e) => format!(
                "{} refused a picture (the provider said: {why}), and the retry without pictures \
                 failed before the provider answered: {e}",
                self.model
            ),
        };
        eprintln!("  {said}");
        if let Some(notes) = &self.notes {
            notes(said);
        }
        second
    }
}

impl ResponsesProvider {
    /// One attempt at the wire: build the body for `eyes`, post it, stream the answer. A
    /// non-2xx status returns before a byte of the body is read as a stream, which is what
    /// makes the retry safe.
    fn round(
        &self,
        req: &TurnRequest<'_>,
        on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
        eyes: Eyes<'_>,
        withheld: Withheld<'_>,
    ) -> Result<Completion, BackendError> {
        let body = self.body_for(req, eyes, withheld)?;
        // Serialised once and kept, so a refusal can say how large the request was without
        // paying for a second pass over a body that may be megabytes.
        let payload = body.to_string();
        let started = Instant::now();
        // A signal is not the provider failing: the answer bound makes this wait a timed
        // read, and a timed read ends with `EINTR` when the daemon's own Ctrl-C arrives.
        let mut retried = 0u32;
        let resp = loop {
            match self
                .agent
                .post(&self.url)
                .header("Authorization", &format!("Bearer {}", self.creds.key))
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .send(payload.as_bytes())
            {
                Ok(resp) => break resp,
                Err(e) if ask_again(&e, &mut retried) => {}
                Err(e) => return Err(BackendError::unreachable(e.to_string(), io_kind(&e))),
            }
        };
        let status = resp.status().as_u16();
        // The provider's own `Retry-After`, read before the body consumes the response: the
        // only form of *when may I come back* a caller may sleep on. A 429 that carries none
        // still names a window in its prose, which `letibot_http::reset` reads.
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        if !(200..300).contains(&status) {
            let text = resp
                .into_body()
                .read_to_string()
                .unwrap_or_else(|e| format!("(unreadable body: {e})"));
            let parsed: Option<Value> = serde_json::from_str(&text).ok();
            let error = parsed.as_ref().and_then(|v| v.get("error"));
            let message = error
                .and_then(|e| {
                    e.get("message")
                        .or(Some(e))
                        .map(|m| m.as_str().map_or_else(|| m.to_string(), String::from))
                })
                .unwrap_or_else(|| text.clone());
            // The provider's own CODE, kept beside its message: it is the one machine-readable
            // word in a refusal, and a quota is read off it (`insufficient_quota`).
            let message = match error.and_then(|e| e.get("code")).and_then(Value::as_str) {
                Some(code) if !message.contains(code) => format!("{message} ({code})"),
                _ => message,
            };
            eprintln!("  refused request shape:\n{}", shape(&body, payload.len()));
            return Err(BackendError::Refused {
                status,
                body: message,
                retry_after,
            });
        }
        let reader = BufReader::new(resp.into_body().into_reader());
        let mut acc = Accumulator::default();
        let mut aborted = false;
        for line in reader.lines() {
            let line = line.map_err(|e| {
                BackendError::unreachable(format!("mid-stream: {e}"), Some(e.kind()))
            })?;
            // SSE: `event: <name>` then `data: <json>`, and the type is in the JSON as well.
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            let event: Value = serde_json::from_str(data)
                .map_err(|e| BackendError::Malformed(format!("{e} in event {data}")))?;
            for d in acc.absorb(&event) {
                if on_delta(&d) == StreamFlow::Stop {
                    aborted = true;
                    break;
                }
            }
            if aborted {
                break;
            }
            // A terminal event is the end of the answer; the socket closes without a `[DONE]`
            // on this protocol, so waiting for one would hang until the body budget.
            if acc.ended {
                break;
            }
        }
        if aborted {
            return Err(BackendError::Aborted);
        }
        // A half answer is not an answer: this API says so with a terminal event, and a
        // socket that closes cleanly after one is indistinguishable from a finished answer.
        if !acc.ended {
            return Err(BackendError::unreachable(
                format!(
                    "the stream closed without `response.completed`, `response.incomplete` or \
                     `response.failed` after {} character(s) of answer and {} tool call(s), so \
                     what arrived is a fragment",
                    acc.text.chars().count() + acc.reasoning.chars().count(),
                    acc.calls.len()
                ),
                None,
            ));
        }
        if let Some(failure) = acc.failure.take() {
            return Err(failure);
        }
        let wall_ms = started.elapsed().as_millis() as u64;
        let (done, issued) = acc.finish(
            &self.model,
            &self.creds,
            wall_ms,
            self.preset,
            &self.catalogue,
        );
        // Only a round that answered keeps its records: the engine commits nothing of a
        // failed one, so a record for a row that was never written would be a thought with
        // no text to find it by - and it would sit in front of the ones that do.
        self.keep(issued);
        Ok(done)
    }
}

#[cfg(test)]
mod tests;
