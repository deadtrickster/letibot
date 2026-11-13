//! Transcript items to the API's messages, by each provider's documented behaviour.
//!
//! A system item becomes a system message; a user item joins its text parts and SENDS an
//! image until a model refuses it ([`Eyes`]); reasoning rides on its assistant message; an
//! assistant with calls and no text has `content: null`; a tool result folds its outcome
//! into the content, so a refusal reads differently from an answer.
//!
//! Two shapes are not conversions. A result carrying its own author is one the OPERATOR ran,
//! with no proposing assistant row above it, so it is carried as a user turn ([`person_ran`]).
//! A picture goes through [`picture_fate`]: a model that takes text only, bytes that are not a
//! COMPLETE image, or bytes a provider already refused become a `not sent` placeholder naming
//! why, because the transcript is append-only and one bad picture refuses every turn after it.
//!
//! `reasoning_content` is where this file was wrong twice, and the shape of the mistake is
//! worth keeping: a refusal names a field the API would accept as a remedy, not the shape
//! that provokes it.
//!
//! ```text
//! compaction FAILED: http 400: The `reasoning_content` in the thinking mode must
//! be passed back to the API. (request_id: 5f6e40f5-…)
//! ```
//!
//! The field that refusal names is one the API accepts as a remedy, and it is not the rule
//! the guide states: whether `reasoning_content` goes back does not depend on `tools`, and
//! omitting it produces no 400 in any of these shapes.
//!
//! ```text
//! no tools, assistant without reasoning_content                 200
//! tools,    assistant without reasoning_content                 200
//! tools,    assistant with tool_calls, no reasoning_content     200
//! tools,    assistant with tool_calls, with reasoning_content   200
//! no tools, assistant with tool_calls, no reasoning_content     200
//! ```
//!
//! So the echo is this harness's own switch for how much thinking to send, not a contract
//! being obeyed: it keeps half a million tokens off a request that does not need them, and a
//! choice is not evidence about the API.
//!
//! The trigger is a TRAILING SYSTEM MESSAGE plus `tools`. The five shapes above all end on a
//! user message, so the cause sat outside the sampled space: only `run_compaction` appends an
//! instruction after the conversation, which is why compaction alone failed while short turns
//! kept working. Size is refuted too.
//!
//! ```text
//! 976,097 provider tokens   200   finish: length (16 of 16 output tokens, all reasoning)
//! 1,248,039 provider tokens 400   "maximum context length is 1048576 tokens. However, you
//!                                  requested 1248055 tokens (1248039 in the messages, 16 in
//!                                  the completion)"  — in 5.6 s
//! ```
//!
//! So a structurally clean request past the window gets the honest complaint, naming exact
//! counts, in seconds. The rule that follows is what the daemon does, not what it believes: a
//! refused request logs its own SHAPE once, so the next occurrence is read off the record
//! rather than argued from a hypothesis.
//!
//! Nothing asserted the request SHAPE before, which is why no test caught it: a head near its
//! limit cannot free context with the call that is being rejected, and short turns keep
//! working until a conversation has run for hours.

use letibot_transcript::{TranscriptItem, UserPart};
use serde_json::{Value, json};

/// Whether the model this request is going to takes pictures: the one fact about the model
/// that changes the bytes [`convert`] produces.
///
/// A parameter rather than a lookup, for the same reason `echo_reasoning` is: the caller is
/// where the request is assembled and where the provider is known, and a conversion that read
/// the catalogue itself would be a second place for the answer to come from.
///
/// A part is SENT unless something says not to, because a model given a picture describes it,
/// while a model given a placeholder has quietly turned a chart into a sentence about a
/// chart. Exactly two things say not to: the catalogue's `modalities.input` naming text and
/// not image, or a provider refusing an `image_url` with an error naming a content type and
/// text, which [`mark_text_only`] records for the rest of that session's requests. That refusal
/// outranks the table, because it is a measurement.
///
/// ```text
/// http 400: messages.content.type is invalid, allowed values: ['text']
/// ```
///
/// The two mistakes are not the same size, so the answer to the larger one is not to stop
/// sending pictures to everybody: send them, hear the refusal, say so, and send that model
/// placeholders from then on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eyes<'a> {
    /// The catalogue says this model takes image input.
    Sees,
    /// This model takes text only: the catalogue said so, or a provider did.
    TextOnly(&'a str),
    /// Nothing here knows: the catalogue does not carry this model and it has not refused a
    /// picture. Sent, because the alternative is describing a picture the model could have
    /// seen.
    Unknown(&'a str),
}

impl Eyes<'_> {
    /// Whether an image part becomes `image_url` or a placeholder.
    pub fn takes_images(&self) -> bool {
        !matches!(self, Eyes::TextOnly(_))
    }

    /// Why this picture is not being sent, in the words the placeholder uses, or `None` when
    /// it is sent. Deterministic per model on purpose: these bytes are the prompt prefix, and a
    /// reason that varied between requests would re-prefill every cached turn.
    fn why_not(&self) -> Option<String> {
        match self {
            Eyes::TextOnly(model) => Some(format!("{model} takes text only")),
            Eyes::Sees | Eyes::Unknown(_) => None,
        }
    }
}

/// The most of a provider's refusal a placeholder carries: a placeholder is prompt text, and a
/// refusal a page long is truncated rather than pasted into every turn of the conversation.
const REFUSAL_SAID_MAX: usize = 200;

/// The key a picture's refusal is recorded against: a hash of the reference, so two reads of the
/// same file are one picture and one lesson. FNV-1a and not a digest, because a collision costs a
/// good picture withheld beside a bad one; the length is folded in so a prefix cannot collide.
pub fn picture_key(data_ref: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data_ref.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}:{}", data_ref.len())
}

/// Record that a provider refused these BYTES, with the words it refused them in, and `true` when
/// it is new. The words go in the placeholder a later request carries, so the model is told why it
/// has no picture and the reason is the provider's rather than this tree's guess.
///
/// Not the same table as the text-only one, which is a fact about the MODEL: a model may see
/// perfectly well and refuse these bytes, and a mark against the model would withhold every other
/// picture for a fault none of them has.
///
/// ```text
/// http 400: .messages[1861].image[0]: You have uploaded an unsupported image. Please make sure
/// your image is valid and has one of the following formats: webp, png, jpeg, and gif.
/// ```
///
/// The table belongs to the SESSION whose marks are in force on this thread (see
/// [`crate::marks::enter`]), not to the process. With none in force nothing is remembered and the
/// refusal is this call's alone.
pub fn mark_bad_picture(data_ref: &str, why: &str) -> bool {
    let key = picture_key(data_ref);
    let mut said = why.trim().replace(['\n', '\r'], " ");
    if said.chars().count() > REFUSAL_SAID_MAX {
        said = said.chars().take(REFUSAL_SAID_MAX).collect::<String>() + "…";
    }
    crate::marks::update(|m| m.mark_picture(&key, &said)).unwrap_or(false)
}

/// Why this picture is not sent, when this session has already learned that it is not: the
/// provider's own words, or `None` when it goes.
pub fn known_bad_picture(data_ref: &str) -> Option<String> {
    let key = picture_key(data_ref);
    crate::marks::read(|m| m.picture_why(&key).map(String::from)).flatten()
}

/// Is this refusal about the PICTURE rather than about the model? It does not say this model takes
/// text, it says these bytes are not an image, and the remedies differ: one withholds every
/// picture from a model, the other withholds one. The matcher reads what the sentence names.
pub fn picture_refusal(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    let names_picture = b.contains("image") || b.contains("picture") || b.contains("media");
    if !names_picture {
        return false;
    }
    const COMPLAINTS: [&str; 12] = [
        "unsupported",
        "invalid",
        "not valid",
        "not a valid",
        "could not",
        "cannot",
        "unable",
        "corrupt",
        "malformed",
        "format",
        "upload",
        "decode",
    ];
    COMPLAINTS.iter().any(|c| b.contains(c))
}

/// The pictures a request carries, as the references they are: the two carriers, [`UserPart::Image`]
/// and a tool result's [`Media`](letibot_transcript::media::Media), in the order a retry would
/// withhold them.
pub fn pictures<'a>(items: impl IntoIterator<Item = &'a TranscriptItem>) -> Vec<&'a str> {
    let mut out = Vec::new();
    for item in items {
        match item {
            TranscriptItem::User { parts, .. } => {
                for p in parts {
                    if let UserPart::Image { data_ref, .. } = p {
                        out.push(data_ref.as_str());
                    }
                }
            }
            TranscriptItem::ToolResult { media: Some(m), .. } => out.push(m.data_ref.as_str()),
            _ => {}
        }
    }
    out
}

/// The pictures a request must not send, beyond what this session has already learned. A retry is
/// the only caller, and it is a parameter rather than a record because a retry that also fails
/// proves nothing about the pictures; the caller commits the marks once it answers.
#[derive(Debug, Clone, Copy, Default)]
pub struct Withheld<'a> {
    /// `(picture_key, the provider's words)` for pictures a retry is trying without.
    trying: &'a [(String, String)],
}

impl<'a> Withheld<'a> {
    /// Nothing beyond the marks this session already holds.
    pub fn nothing() -> Self {
        Withheld { trying: &[] }
    }

    /// A retry: these pictures, and the words that made it try.
    pub fn trying(pairs: &'a [(String, String)]) -> Self {
        Withheld { trying: pairs }
    }

    /// Why this picture is not sent for a reason this request knows about - `None` when it goes.
    fn why(&self, data_ref: &str) -> Option<String> {
        let key = picture_key(data_ref);
        if let Some((_, why)) = self.trying.iter().find(|(k, _)| *k == key) {
            return Some(refused_said(why));
        }
        known_bad_picture(data_ref).map(|why| refused_said(&why))
    }
}

/// Why a picture is withheld, when the reason is the bytes themselves - the sentence a
/// placeholder carries for a file that sniffs as an image and holds none.
pub const NOT_A_PICTURE: &str = "these bytes are not a complete image";

/// The placeholder's reason for a picture a provider refused: the provider's words, attributed.
fn refused_said(why: &str) -> String {
    format!("the provider refused these bytes: {why}")
}

/// The key a model's refusal is recorded against: `provider/model`, because one model name behind
/// two doors is two models.
fn text_only_key(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
}

/// Record that this model refused a picture. `true` when it is new, which is what makes the
/// notice to the operator once, and `false` when it was already known - or when no session has
/// entered its marks on this thread, where nothing is remembered at all.
pub fn mark_text_only(provider: &str, model: &str) -> bool {
    let key = text_only_key(provider, model);
    crate::marks::update(|m| m.mark_model(&key)).unwrap_or(false)
}

/// Whether this session has already been refused a picture for this model.
pub fn known_text_only(provider: &str, model: &str) -> bool {
    let key = text_only_key(provider, model);
    crate::marks::read(|m| m.has_model(&key)).unwrap_or(false)
}

/// Is this refusal the one that means "not this kind of content"? The wording differs by
/// vendor while the fact does not:
///
/// ```text
/// messages.content.type is invalid, allowed values: ['text']            (Zhipu)
/// Invalid 'messages[1].content[2].type': 'image_url' is not supported   (OpenAI-shaped)
/// unknown variant `image_url`, expected `text`                          (serde, behind a gateway)
/// ```
///
/// So the matcher reads what the sentence NAMES rather than one vendor's punctuation:
/// `image_url` anywhere, or a content type complaint naming text. It is only consulted for a
/// 400 on a request that carried a picture, which keeps it from reading a schema complaint
/// about something else as this one.
pub fn text_only_refusal(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    // A part type the model does not take, named outright. This is the shape every
    // OpenAI-compatible validator produces for a content array it cannot deserialize.
    if b.contains("image_url") || b.contains("image url") {
        return true;
    }
    // A content TYPE complaint that also names text - Zhipu's spelling, the
    // space-separated English of the same sentence, and the serde wording.
    let content_type = b.contains("content.type")
        || b.contains("content type")
        || b.contains("content_type")
        || (b.contains("content") && b.contains("type"));
    if content_type && b.contains("text") {
        return true;
    }
    // "text only" / "only text" / "text-only" about a content, an image or a modality.
    let says_text_only =
        b.contains("text only") || b.contains("only text") || b.contains("text-only");
    says_text_only && (b.contains("content") || b.contains("image") || b.contains("modalit"))
}

/// Does this request carry a picture at all? The guard on the refusal path: a 400 naming a content
/// type on a request with no picture is not this refusal, and retrying it would send the same
/// bytes again. An iterator, so items and tail are one walk without copying the conversation.
pub fn carries_pictures<'a>(items: impl IntoIterator<Item = &'a TranscriptItem>) -> bool {
    !pictures(items).is_empty()
}

/// What one picture becomes: sent, or the reason it is not. The order of the three reasons is
/// their scope: the model, then the bytes, then what a provider said about this picture.
///
/// `pub(crate)` because every protocol in this crate sends pictures and must answer the same way:
/// two answers would be one model sent an image by one door and a placeholder by another.
pub(crate) fn picture_fate(
    data_ref: &str,
    eyes: Eyes<'_>,
    withheld: Withheld<'_>,
) -> Option<String> {
    if !eyes.takes_images() {
        return Some(eyes.why_not().unwrap_or_default());
    }
    if !letibot_transcript::media::data_ref_is_image(data_ref) {
        return Some(NOT_A_PICTURE.to_string());
    }
    withheld.why(data_ref)
}

/// One picture, said as one text part, for a model that will not be given it. It carries no name,
/// so it is named by its position among its message's images. The size is the DECODED size,
/// because base64 is 4/3 of it and that ratio is an encoding detail.
pub(crate) fn not_sent_said(nth: usize, mime: &str, bytes: Option<usize>, why: &str) -> String {
    let size = match bytes {
        Some(n) => size_said(n),
        None => "size unknown".to_string(),
    };
    format!("[image {nth} · {mime} · {size} — not sent: {why}]")
}

/// The decoded length of a `data:<mime>;base64,<payload>` reference.
pub(crate) fn data_ref_bytes(data_ref: &str) -> Option<usize> {
    let payload = data_ref.split_once(";base64,")?.1;
    if payload.len() % 4 != 0 {
        return None;
    }
    let padding = payload.bytes().rev().take_while(|b| *b == b'=').count();
    Some(payload.len() / 4 * 3 - padding)
}

/// A byte count a person reads: `128 KiB`, `1.4 MiB`. Whole kilobytes, because the
/// placeholder is about how much was withheld rather than about the file.
fn size_said(bytes: usize) -> String {
    const K: usize = 1024;
    if bytes >= K * K {
        format!("{:.1} MiB", bytes as f64 / (K * K) as f64)
    } else if bytes >= K {
        format!("{} KiB", bytes / K)
    } else {
        format!("{bytes} B")
    }
}

/// The `messages` array. `system` is prepended when non-empty and no `System` item leads the
/// transcript. `echo_reasoning` is this harness's own choice about how much to send: the caller
/// computes it, because the caller knows whether `tools` is about to go.
pub fn convert(
    system: &str,
    items: &[TranscriptItem],
    echo_reasoning: bool,
    eyes: Eyes<'_>,
    withheld: Withheld<'_>,
) -> Vec<Value> {
    convert_with_tail(system, items, &[], echo_reasoning, eyes, withheld)
}

/// The same conversion, with items the request carries and the transcript does not
/// (`TurnRequest::tail`). The tail goes through the SAME walk, because a tail with its own
/// converter could disagree with the transcript's, and it goes last as an unrefused shape.
pub fn convert_with_tail(
    system: &str,
    items: &[TranscriptItem],
    tail: &[TranscriptItem],
    echo_reasoning: bool,
    eyes: Eyes<'_>,
    withheld: Withheld<'_>,
) -> Vec<Value> {
    let mut out = Vec::with_capacity(items.len() + tail.len() + 1);
    let leads_with_system = matches!(items.first(), Some(TranscriptItem::System { .. }));
    if !system.trim().is_empty() && !leads_with_system {
        out.push(json!({"role": "system", "content": system}));
    }
    // The open turn's reasoning, held for the assistant it belongs to and ACROSS intervening
    // items: a steering message can land in between, and dropping the reasoning there is exactly
    // the 400 this prevents.
    let mut pending_reasoning: Option<String> = None;
    // Results that carry their own author, by `call_id`: the operator's `!` line and their own
    // door calls have no model proposing anything, so nothing above them answers them. Collected
    // in this walk and consulted by `pair_tool_calls`, which is where the old code dropped it.
    let mut own_author: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in items.iter().chain(tail.iter()) {
        match item {
            TranscriptItem::System { text, .. } => {
                out.push(json!({"role": "system", "content": text}));
            }
            // A delivery reaches the model where a notice has always reached it - as a user
            // turn whose words are the harness's own, said as such in the text. A dialect that
            // cannot take a mid-history system message would refuse the other shape.
            TranscriptItem::Notice { text, .. } => {
                out.push(json!({"role": "user", "content": text}));
            }
            TranscriptItem::User { parts, .. } => {
                // An image part is SENT to a model that takes pictures and NAMED to one that does
                // not. The shape for a model that sees is a content ARRAY; a text-only message keeps
                // the plain string, because an array where a string was re-prefills the cache.
                let mut content: Vec<Value> = Vec::new();
                let mut images = 0usize;
                for p in parts {
                    match p {
                        UserPart::Text { text } => {
                            content.push(json!({"type": "text", "text": text}))
                        }
                        UserPart::Image {
                            media_type,
                            data_ref,
                        } => {
                            images += 1;
                            // A picture that will not be sent is NAMED instead, with the reason
                            // [`picture_fate`] gives: the model takes text only, the bytes are not
                            // an image, or a provider refused these bytes.
                            match picture_fate(data_ref, eyes, withheld) {
                                None => content.push(
                                    json!({"type": "image_url", "image_url": {"url": data_ref}}),
                                ),
                                Some(why) => content.push(json!({
                                    "type": "text",
                                    "text": not_sent_said(
                                        images,
                                        media_type,
                                        data_ref_bytes(data_ref),
                                        &why,
                                    ),
                                })),
                            }
                        }
                        UserPart::FileRef { path, .. } => {
                            content.push(json!({"type": "text", "text": path}))
                        }
                    }
                }
                let only_text = content
                    .iter()
                    .all(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"));
                if only_text {
                    let text = content
                        .iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    out.push(json!({"role": "user", "content": text}));
                } else {
                    out.push(json!({"role": "user", "content": Value::Array(content)}));
                }
            }
            TranscriptItem::Reasoning { text, .. } => {
                if echo_reasoning && !text.is_empty() {
                    match &mut pending_reasoning {
                        // Joined, not replaced: a truncated reasoning row and its
                        // continuation are two items and one thought.
                        Some(prev) => {
                            prev.push_str("\n\n");
                            prev.push_str(text);
                        }
                        None => pending_reasoning = Some(text.clone()),
                    }
                }
            }
            TranscriptItem::SegmentMark { .. } | TranscriptItem::Offer { .. } => {}
            TranscriptItem::Assistant {
                text, tool_calls, ..
            } => {
                let mut m = json!({"role": "assistant"});
                // The reasoning this assistant produced, if the request must carry it. Set before
                // `content` so the field order reads the way the API documents it, though order is
                // not what it checks.
                if let Some(r) = pending_reasoning.take() {
                    m["reasoning_content"] = Value::String(r);
                }
                if text.is_empty() && !tool_calls.is_empty() {
                    m["content"] = Value::Null;
                } else {
                    m["content"] = json!(text);
                }
                if !tool_calls.is_empty() {
                    m["tool_calls"] = Value::Array(
                        tool_calls
                            .iter()
                            .map(|c| {
                                json!({
                                    "id": c.id,
                                    "type": "function",
                                    "function": {"name": c.name, "arguments": c.arguments}
                                })
                            })
                            .collect(),
                    );
                }
                out.push(m);
            }
            TranscriptItem::ToolResult {
                call_id,
                // The payload names its own outcome, so neither is needed here.
                name: _,
                outcome: _,
                payload,
                // Display-only; the prompt never sees it.
                edits: _,
                media,
                origin,
            } => {
                if matches!(
                    origin,
                    Some(letibot_transcript::CallOrigin::Operator { .. })
                ) {
                    own_author.insert(call_id.clone());
                }
                // The payload already names its own outcome - `render` wraps a failure, a timeout
                // and a backgrounding in their own envelope - so a second prefix here said it twice.
                // An empty string is refused by some servers of this API (Kimi's): a command that
                // printed nothing says so instead.
                let content = match payload.trim().is_empty() {
                    true => "(no output)".to_string(),
                    false => payload.clone(),
                };
                out.push(json!({"role": "tool", "tool_call_id": call_id, "content": content}));
                // When the call read a picture, the picture follows as its own user message, the
                // shape every vision API accepts. A model that does not see gets the words and one
                // plain string instead, so the prefix is not an array where a string was.
                if let Some(m) = media {
                    // The same predicate as a head's own picture: the row carrying the bytes that
                    // were refused is history, so the renderer is where it stops being sent.
                    match picture_fate(&m.data_ref, eyes, withheld) {
                        None => out.push(json!({
                            "role": "user",
                            "content": [
                                {"type": "text", "text": "Attached media from tool result:"},
                                {"type": "image_url", "image_url": {"url": m.data_ref}},
                            ]
                        })),
                        Some(why) => out.push(json!({
                            "role": "user",
                            "content": format!(
                                "Attached media from tool result: {}",
                                not_sent_said(1, &m.mime, Some(m.bytes), &why)
                            ),
                        })),
                    }
                }
            }
        }
    }
    pair_tool_calls(out, &own_author)
}

/// Every `tool_calls` gets its `tool` message, or the provider refuses the whole conversation.
///
/// OpenAI requires one `tool` message per `tool_call_id` after an assistant message carrying
/// calls. A local dialect does not: it renders a call that never came back as text and the
/// conversation carries on, so a transcript that is fine on qwen is one a provider will not
/// accept. Measured the first time a long local conversation was pointed at deepseek:
///
/// ```text
/// provider refused (400): An assistant message with 'tool_calls' must be
/// followed by tool messages responding to each 'tool_call_id'.
/// ```
///
/// The gap is filled rather than the request being sent to be refused: the synthesised message
/// says no result was recorded, which is better information than the provider's refusal and
/// better than a fabricated success. A `tool` message whose id answers nothing is dropped for
/// the same reason in reverse, since providers reject those too.
///
/// One orphan is carried instead of dropped: a result the OPERATOR ran, marked
/// `CallOrigin::Operator`. Those are calls no model proposed, so a `tool` message would be
/// refused for having nothing above it to answer; it becomes the person's own turn instead.
/// See [`person_ran`].
fn pair_tool_calls(rows: Vec<Value>, own_author: &std::collections::HashSet<String>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    // Ids opened by the assistant message being answered right now, in order, so
    // the fillers go in the order the calls were made.
    let mut owed: Vec<String> = Vec::new();
    // Rows recorded while calls were still running - a room note, a monitor firing - wait here
    // until the results land, so a note between a call and its result never orphans the result.
    let mut held: Vec<Value> = Vec::new();

    for row in rows {
        let mut row = row;
        let role = row.get("role").and_then(|r| r.as_str()).map(str::to_string);
        if role.as_deref() == Some("tool") {
            let id = row
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if let Some(at) = owed.iter().position(|o| *o == id) {
                owed.remove(at);
                out.push(row);
                if owed.is_empty() {
                    out.append(&mut held);
                }
                continue;
            }
            // An orphan has no assistant message above it opening this id: dropped, because a
            // provider refuses it, unless it carries its own author and becomes the person's turn.
            if !own_author.contains(&id) {
                continue;
            }
            row = person_ran(row);
        }
        if role.as_deref() != Some("assistant") && !owed.is_empty() {
            held.push(row);
            continue;
        }
        // The next assistant message closes the answering window, so whatever is still owed is
        // owed for ever and is filled here, then the held rows follow in their order.
        for id in std::mem::take(&mut owed) {
            out.push(unanswered(&id));
        }
        out.append(&mut held);
        if role.as_deref() == Some("assistant") {
            owed = row
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .map(|cs| {
                    cs.iter()
                        .filter_map(|c| c.get("id").and_then(|i| i.as_str()))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
        }
        out.push(row);
    }
    // The transcript can simply end on an unanswered call - an interrupted turn does exactly
    // that - and the provider counts it the same way.
    for id in owed {
        out.push(unanswered(&id));
    }
    out.append(&mut held);
    trailing_system_becomes_user(&mut out);
    out
}

/// A request may not END on a system message, and that is the trigger this file spent two
/// wrong theories looking for. One axis at a time, everything else held:
///
/// ```text
///                                                   no tools   with tools
/// system at the head, ends with a user message        200         200
/// ends with ONE system message                        200         400
/// ends with TWO system messages                       200         400
/// systems interleaved mid-conversation, ends system   200         400
/// the same, reasoning_content echoed on assistants    200         200
/// ```
///
/// So the refusal needs a trailing system message AND `tools`, and echoing
/// `reasoning_content` is one way out of it. Every shape the module header tested ended with a
/// user message, which is why size, structure and the echo were measured away while the cause
/// stayed hidden.
///
/// The error text is literally true and completely misleading: it names a field the API would
/// accept as a remedy, not the shape that provoked it, and three readers in a row chased the
/// field.
///
/// The translation lives here rather than in the transcript because the record is right:
/// `compaction::run_compaction` appends its instruction after the cached history rather than
/// rewriting anything, and the local dialect path renders it that way and reuses the prefix.
/// Changing the stored item would make the transcript lie about what the daemon did in order to
/// satisfy one provider's validator.
///
/// This function is the layer that already translates the record into one provider's dialect,
/// so the translation belongs here, on this transport only. Not gated on the vendor's name: a
/// request that ends on an instruction reads better as a user turn in every case, and the
/// narrower rule is the one that rots when the next shape appears.
fn trailing_system_becomes_user(out: &mut [Value]) {
    // A lone system message is the system PROMPT and stays one: translating it would send a
    // conversation whose only turn is the operator saying the prompt. The trigger is a system
    // message arriving AFTER a conversation.
    if out.len() < 2 {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("system")
    {
        last["role"] = Value::String("user".into());
    }
}

/// A result the OPERATOR ran, carried as the person's own turn: `CallOrigin::Operator` marks the
/// row, the daemon ran it with no model proposing anything, so no `tool_calls` entry answers it
/// and a `tool` message would be refused. A user message is what the local dialects render too.
fn person_ran(row: Value) -> Value {
    let content = row.get("content").cloned().unwrap_or(Value::Null);
    json!({"role": "user", "content": content})
}

fn unanswered(id: &str) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": id,
        "content": "[no result was recorded for this call: the turn ended before it \
                    finished, or it was never run]",
    })
}

/// Tool schemas as the API wants them. The harness hands over its dialect's rendering, either the
/// wrapped shape already or the bare `{name, description, parameters}`; the bare one is wrapped,
/// and anything else is refused rather than sent as a string the provider would 400 on.
pub fn tools(tools_json: &[String]) -> Result<Vec<Value>, String> {
    tools_json
        .iter()
        .map(|t| {
            let v: Value =
                serde_json::from_str(t).map_err(|e| format!("a tool schema is not JSON: {e}"))?;
            if v.get("function").is_some() {
                return Ok(v);
            }
            if v.get("name").is_some() {
                return Ok(json!({"type": "function", "function": v}));
            }
            Err(format!(
                "a tool schema has neither `function` nor `name`: {}",
                t.chars().take(80).collect::<String>()
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod pairing_tests {
    use super::*;
    use letibot_transcript::{ToolCall, ToolOutcome, TranscriptItem, UserPart};

    /// [`super::convert`] for a model that SEES - see the sibling module's helper.
    fn convert(system: &str, items: &[TranscriptItem], echo_reasoning: bool) -> Vec<Value> {
        super::convert(
            system,
            items,
            echo_reasoning,
            Eyes::Sees,
            Withheld::nothing(),
        )
    }

    fn user(t: &str) -> TranscriptItem {
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text { text: t.into() }],
        }
    }

    fn calls(ids: &[&str]) -> TranscriptItem {
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: ids
                .iter()
                .map(|id| ToolCall {
                    id: (*id).into(),
                    name: "bash".into(),
                    arguments: "{}".into(),
                })
                .collect(),
            truncated: false,
        }
    }

    fn result(id: &str) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: id.into(),
            name: "bash".into(),
            outcome: ToolOutcome::Ok,
            payload: "done".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        }
    }

    /// The same row with the operator's own mark on it: `CallOrigin::Operator` is what tells an
    /// orphan with an author from an orphan without one.
    fn ran_by_the_operator(id: &str, payload: &str) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: id.into(),
            name: "bash".into(),
            outcome: ToolOutcome::Ok,
            payload: payload.into(),
            edits: Vec::new(),
            origin: Some(letibot_transcript::CallOrigin::Operator { who: "dead".into() }),
            media: None,
        }
    }

    /// The operator's `!` line and its output reached the transcript and the screen, and the model
    /// was handed the line and nothing else: no proposing assistant row exists, so the result was
    /// dropped as an orphan rather than carried.
    #[test]
    fn the_operators_own_result_reaches_the_model_with_no_proposal_above_it() {
        let m = convert(
            "sys",
            &[
                user("! ls -la"),
                ran_by_the_operator("bang-1", "total 8\ndrwxr-xr-x 2 dead dead"),
            ],
            false,
        );
        assert_paired(&m);
        assert!(
            m.iter().any(|r| r["content"]
                .as_str()
                .is_some_and(|c| c.contains("drwxr-xr-x"))),
            "the command's output is not in the request at all: {m:?}"
        );
        // And it is the person's turn, not the model's: a `tool` message would answer a `tool_calls`
        // entry that does not exist, and a fabricated assistant call would put a call in the trail
        // that no model made.
        let carried = m
            .iter()
            .find(|r| {
                r["content"]
                    .as_str()
                    .is_some_and(|c| c.contains("drwxr-xr-x"))
            })
            .expect("just asserted");
        assert_eq!(carried["role"], "user");
        assert!(
            !m.iter().any(|r| r["role"] == "tool"),
            "there is no call above it for a tool message to answer: {m:?}"
        );
        // The operator's own line is still there, and still theirs.
        assert!(m.iter().any(|r| r["content"] == "! ls -la"));
    }

    /// The control the change needs: an OPERATOR-origin result that DOES answer a call is
    /// untouched. Keying the fix on `origin` alone would have made it a user turn and lost the
    /// pairing; keying it on no proposal above it leaves the `tool` message it always was.
    #[test]
    fn an_operators_result_that_answers_a_call_is_still_a_tool_message() {
        let m = convert(
            "sys",
            &[
                user("go"),
                calls(&["op-1"]),
                ran_by_the_operator("op-1", "the page"),
            ],
            false,
        );
        assert_paired(&m);
        let answered = m
            .iter()
            .find(|r| r["tool_call_id"] == "op-1")
            .expect("the result answers the call the model proposed");
        assert_eq!(answered["role"], "tool");
        assert!(answered["content"].as_str().unwrap().contains("the page"));
        assert_eq!(m.len(), 4, "system + three, nothing added: {m:?}");
    }

    /// The question exchange's shape: the result answers the call and the person's two rows
    /// follow it, in whichever order the session wrote them.
    #[test]
    fn the_question_exchange_pairs_in_either_order() {
        let asked = TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text {
                text: "asked: which database?".into(),
            }],
        };
        let answered = TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![UserPart::Text {
                text: "sqlite".into(),
            }],
        };

        // As the session writes it. The result answers the call, and the two rows about
        // the person follow it.
        let m = convert(
            "sys",
            &[
                user("go"),
                calls(&["q-1"]),
                ran_by_the_operator("q-1", "chose option 1: sqlite"),
                asked.clone(),
                answered.clone(),
            ],
            false,
        );
        assert_paired(&m);
        let result = m
            .iter()
            .find(|r| r["tool_call_id"] == "q-1")
            .expect("the result answers the call the model proposed");
        assert_eq!(result["role"], "tool");
        assert!(
            result["content"]
                .as_str()
                .unwrap()
                .contains("chose option 1"),
            "the answer the model reads is the result's payload: {result}"
        );
        assert!(
            !m.iter().any(|r| r["content"]
                .as_str()
                .is_some_and(|c| c.contains("no result was recorded"))),
            "the call was reported unanswered beside its own answer: {m:?}"
        );
        assert!(
            m.iter()
                .any(|r| r["role"] == "user" && r["content"] == "sqlite"),
            "the person's own words must reach the model as a turn of theirs: {m:?}"
        );

        // The other order - the person's half written first - reaches the provider the same
        // way: the rows between a call and its result wait until the result has answered it.
        let swapped = convert(
            "sys",
            &[
                user("go"),
                calls(&["q-1"]),
                asked,
                answered,
                ran_by_the_operator("q-1", "chose option 1: sqlite"),
            ],
            false,
        );
        assert_paired(&swapped);
        assert!(
            !swapped.iter().any(|r| r["content"]
                .as_str()
                .is_some_and(|c| c.contains("no result was recorded"))),
            "a row between a call and its result orphaned the result: {swapped:?}"
        );
    }

    /// A note recorded while calls were running - a room message, a monitor firing - sits
    /// between the calls and their results. Every result still answers its call, and the note
    /// follows them.
    #[test]
    fn a_note_between_calls_and_their_results_waits_for_the_results() {
        let m = convert(
            "sys",
            &[
                user("go"),
                calls(&["c-1", "c-2"]),
                user("room: a message for everyone"),
                result("c-1"),
                result("c-2"),
            ],
            false,
        );
        assert_paired(&m);
        assert!(
            !m.iter().any(|r| r["content"]
                .as_str()
                .is_some_and(|c| c.contains("no result was recorded"))),
            "{m:?}"
        );
        let at = |want: &dyn Fn(&Value) -> bool| m.iter().position(want).unwrap();
        let note = at(&|r| r["content"] == "room: a message for everyone");
        let last = at(&|r| r["tool_call_id"] == "c-2");
        assert!(note > last, "the note goes after the results: {m:?}");
    }

    /// A call that never got a result is still filled, and a note held behind it is not lost.
    #[test]
    fn a_held_note_survives_a_call_that_never_answered() {
        let m = convert(
            "sys",
            &[
                user("go"),
                calls(&["c-1"]),
                user("room: a note"),
                calls(&["c-2"]),
                result("c-2"),
            ],
            false,
        );
        assert_paired(&m);
        assert!(m.iter().any(|r| r["content"] == "room: a note"), "{m:?}");
        assert!(
            m.iter().any(|r| r["tool_call_id"] == "c-1"
                && r["content"]
                    .as_str()
                    .is_some_and(|c| c.contains("no result was recorded"))),
            "{m:?}"
        );
    }

    /// Every id an assistant message opens is answered by the time the next non-tool message
    /// starts: the provider's own rule, asserted over whatever `messages` produced.
    fn assert_paired(rows: &[Value]) {
        let mut owed: Vec<String> = Vec::new();
        for r in rows {
            let role = r["role"].as_str().unwrap_or("");
            if role == "tool" {
                let id = r["tool_call_id"].as_str().unwrap_or("").to_string();
                let at = owed
                    .iter()
                    .position(|o| *o == id)
                    .unwrap_or_else(|| panic!("a tool message answers nothing: {id}"));
                owed.remove(at);
                continue;
            }
            assert!(owed.is_empty(), "unanswered tool_call_id(s): {owed:?}");
            if let Some(cs) = r.get("tool_calls").and_then(|v| v.as_array()) {
                owed = cs
                    .iter()
                    .map(|c| c["id"].as_str().unwrap_or("").to_string())
                    .collect();
            }
        }
        assert!(owed.is_empty(), "unanswered at the end: {owed:?}");
    }

    /// The 400 this exists for. A turn interrupted between the call and its result leaves an
    /// assistant row with `tool_calls` and no `tool` message after it, which a local dialect draws
    /// as text and a provider refuses whole. The row is history, so it is filled, not reported.
    #[test]
    fn an_interrupted_call_is_answered_rather_than_left_dangling() {
        let m = convert(
            "sys",
            &[user("go"), calls(&["c1"]), user("never mind, carry on")],
            false,
        );
        assert_paired(&m);
        let filler = m
            .iter()
            .find(|r| r["role"] == "tool")
            .expect("the gap was filled");
        assert_eq!(filler["tool_call_id"], "c1");
        assert!(
            filler["content"]
                .as_str()
                .unwrap()
                .contains("no result was recorded"),
            "it says what is true rather than faking a success: {filler}"
        );
        // And it goes BEFORE the user turn that followed, not at the end.
        let at = m.iter().position(|r| r["role"] == "tool").unwrap();
        let next = m
            .iter()
            .position(|r| r["content"] == "never mind, carry on")
            .unwrap();
        assert!(at < next, "the filler answers the call it belongs to");
    }

    /// A transcript can simply END on an unanswered call, which an interrupted turn does, and the
    /// provider counts it the same way.
    #[test]
    fn a_transcript_that_ends_mid_call_is_still_valid() {
        let m = convert("sys", &[user("go"), calls(&["c1", "c2"])], false);
        assert_paired(&m);
        assert_eq!(m.iter().filter(|r| r["role"] == "tool").count(), 2);
    }

    /// Some answered, some not: only the gaps are filled, and the real results keep their
    /// content.
    #[test]
    fn only_the_missing_half_of_a_fan_out_is_filled() {
        let m = convert(
            "sys",
            &[
                user("go"),
                calls(&["c1", "c2", "c3"]),
                result("c2"),
                user("next"),
            ],
            false,
        );
        assert_paired(&m);
        let tools: Vec<&Value> = m.iter().filter(|r| r["role"] == "tool").collect();
        assert_eq!(tools.len(), 3);
        let real = tools.iter().find(|r| r["tool_call_id"] == "c2").unwrap();
        assert!(real["content"].as_str().unwrap().contains("done"));
    }

    /// The mirror image: a `tool` message answering nothing. Providers reject those too, and
    /// nothing downstream reads it. The operator's own rows are the exception: an orphan is
    /// dropped when nothing can vouch for it, and carried as the person's turn when it can.
    #[test]
    fn an_orphan_result_is_dropped_rather_than_sent() {
        let m = convert("sys", &[user("go"), result("ghost"), user("next")], false);
        assert_paired(&m);
        assert!(
            !m.iter().any(|r| r["role"] == "tool"),
            "the orphan went: {m:?}"
        );
    }

    /// The ordinary conversation is untouched: this must cost a well-formed transcript nothing.
    #[test]
    fn a_well_formed_conversation_is_unchanged() {
        let items = [user("go"), calls(&["c1"]), result("c1"), user("thanks")];
        let m = convert("sys", &items, false);
        assert_paired(&m);
        assert_eq!(m.len(), 5, "system + four, nothing added: {m:?}");
        assert!(m[3]["content"].as_str().unwrap().contains("done"));
    }

    // --- `reasoning_content`, the contract this file gets wrong at its peril ----------

    /// A request carrying `tools` must pass every previous turn's reasoning back. This is the
    /// assertion whose absence let a head wedge at 940k of 999k tokens: nothing checked the
    /// request SHAPE, so nothing could tell a dropped field from one the model tolerated.
    #[test]
    fn a_request_with_tools_carries_each_turns_reasoning_on_its_assistant() {
        let items = vec![
            user("what is the date?"),
            TranscriptItem::Reasoning {
                text: "I should call get_date first.".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            },
            calls(&["c1"]),
            result("c1"),
            TranscriptItem::Reasoning {
                text: "Now I can answer.".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            },
            TranscriptItem::Assistant {
                text: "It is the 19th.".into(),
                tool_calls: vec![],
                truncated: false,
            },
        ];
        let m = convert("sys", &items, true);
        let assistants: Vec<&Value> = m.iter().filter(|r| r["role"] == "assistant").collect();
        assert_eq!(assistants.len(), 2, "two assistant turns: {m:#?}");
        for a in &assistants {
            assert!(
                a["reasoning_content"].is_string(),
                "every assistant whose turn had reasoning must carry it when the \
                 request has tools, or DeepSeek answers 400: {a:#?}"
            );
        }
        assert_eq!(
            assistants[0]["reasoning_content"], "I should call get_date first.",
            "the reasoning attaches to the assistant it belongs to"
        );
        assert_eq!(assistants[1]["reasoning_content"], "Now I can answer.");
    }

    /// And a request WITHOUT tools does not carry it: DeepSeek ignores the field without `tools`,
    /// and adding it anyway would be a change to a prefix that buys nothing.
    #[test]
    fn a_request_without_tools_does_not_carry_it() {
        let items = vec![
            TranscriptItem::Reasoning {
                text: "thinking".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            },
            TranscriptItem::Assistant {
                text: "answer".into(),
                tool_calls: vec![],
                truncated: false,
            },
        ];
        let m = convert("sys", &items, false);
        assert!(
            m.iter().all(|r| r.get("reasoning_content").is_none()),
            "without tools the field is not sent: {m:#?}"
        );
    }

    /// A split reasoning row is one thought in two items, so it travels joined - and
    /// nothing is dropped, which is the whole requirement.
    #[test]
    fn a_truncated_reasoning_row_and_its_continuation_travel_together() {
        let items = vec![
            TranscriptItem::Reasoning {
                text: "first half".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: true,
            },
            TranscriptItem::Reasoning {
                text: "second half".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            },
            TranscriptItem::Assistant {
                text: "answer".into(),
                tool_calls: vec![],
                truncated: false,
            },
        ];
        let m = convert("sys", &items, true);
        let a = m.iter().find(|r| r["role"] == "assistant").unwrap();
        let r = a["reasoning_content"].as_str().unwrap();
        assert!(
            r.contains("first half") && r.contains("second half"),
            "{r:?}"
        );
    }

    /// An assistant with no reasoning before it gets no field - the field means
    /// "this turn reasoned", not "the request has tools".
    #[test]
    fn an_assistant_that_did_not_reason_carries_no_field() {
        let items = vec![user("hi"), calls(&["c1"])];
        let m = convert("sys", &items, true);
        let a = m.iter().find(|r| r["role"] == "assistant").unwrap();
        assert!(a.get("reasoning_content").is_none(), "{a:#?}");
    }
}
