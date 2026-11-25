//! Transcript items to the API's messages. A result the operator ran becomes a user turn
//! ([`person_ran`]); a picture that cannot go becomes a placeholder ([`picture_fate`]). Echoing
//! `reasoning_content` is this harness's choice of how much thinking to send, not an API contract.

use letibot_transcript::{TranscriptItem, UserPart};
use serde_json::{Value, json};

/// Whether the target model takes pictures. A parameter so the catalogue is read in one place.
/// Pictures are sent unless the catalogue says text-only or a provider refused an `image_url`
/// ([`mark_text_only`]); the refusal outranks the table, and a placeholder loses the picture.
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

/// What one request may carry besides words: the pictures ([`Eyes`]) and whether a PDF goes as a
/// DOCUMENT. Two facts rather than one, because they are independent - most models see and take no
/// document - and because a document is the one part that has a text fallback worth using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Senses<'a> {
    pub eyes: Eyes<'a>,
    /// `Some(why)` when a PDF part is sent as its EXTRACTED TEXT instead of as a document: the
    /// catalogue lists no `pdf` input for this model, a provider refused one, or this door cannot
    /// carry a file part beside a picture. `None` is the catalogue's yes, and nothing else is.
    /// Deterministic, like [`Eyes::why_not`], because it is prompt prefix too.
    pub pdf_why: Option<&'a str>,
}

impl<'a> Senses<'a> {
    /// Pictures as `eyes` says, and no documents: the shape every caller that has no catalogue
    /// answer starts from. Absent is a no, so a document falls back to its text rather than to a
    /// 400 that refuses every later turn.
    pub fn of(eyes: Eyes<'a>) -> Self {
        Senses {
            eyes,
            pdf_why: Some("the catalogue lists no pdf input for this model"),
        }
    }

    /// Pictures as `eyes` says, and the catalogue says PDFs are taken.
    pub fn taking_pdfs(eyes: Eyes<'a>) -> Self {
        Senses {
            eyes,
            pdf_why: None,
        }
    }

    /// Whether a PDF part becomes a document at this door.
    pub fn takes_pdfs(&self) -> bool {
        self.pdf_why.is_none()
    }
}

/// The most of a provider's refusal a placeholder carries: a placeholder is prompt text, and a
/// refusal a page long is truncated rather than pasted into every turn of the conversation.
const REFUSAL_SAID_MAX: usize = 200;

/// What one door may carry for one model, read from the catalogue in ONE place: four protocols
/// deciding it four ways is four chances to send a part a door refuses, and the refusal costs every
/// later turn of the conversation.
///
/// Pictures follow [`Eyes`]; a PDF needs the catalogue to list `pdf` and no provider to have refused
/// one, and `carries_picture` is what keeps this door's own rule about mixing the two (Z.ai's GLM
/// sends file parts OR image parts, never both).
pub fn senses_for<'a>(
    catalogue: &crate::catalogue::Catalogue,
    preset: &crate::presets::Preset,
    model: &'a str,
    carries_picture: bool,
) -> Senses<'a> {
    let eyes = if known_text_only(preset.name, model) {
        Eyes::TextOnly(model)
    } else {
        match catalogue.takes_images(preset.catalogue_id, model) {
            Some(true) => Eyes::Sees,
            Some(false) => Eyes::TextOnly(model),
            None => Eyes::Unknown(model),
        }
    };
    let pdf_why = if known_no_pdf(preset.name, model) {
        Some("the provider refused a PDF for this model")
    } else if !preset.compat.pdf_beside_images && carries_picture {
        Some("this door sends a PDF or an image, never both in one request")
    } else {
        match catalogue.takes_pdfs(preset.catalogue_id, model) {
            Some(true) => None,
            Some(false) => Some("the catalogue lists no pdf input for this model"),
            None => Some("no catalogue carries this model, so a PDF goes as its text"),
        }
    };
    Senses { eyes, pdf_why }
}

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

/// Record that a provider refused these bytes, with its words, and `true` when new. A separate table
/// from text-only, because a model that sees may still refuse one image. Scoped to the session
/// whose marks are in force ([`crate::marks::enter`]); with none, nothing is remembered.
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

/// Record that this model refused a PDF. Its own set, because a model that sees fine and takes no
/// document is the common case, and marking it text-only would withhold every picture too.
pub fn mark_no_pdf(provider: &str, model: &str) -> bool {
    let key = text_only_key(provider, model);
    crate::marks::update(|m| m.mark_document(&key)).unwrap_or(false)
}

/// Whether this session has already been refused a PDF by this model.
pub fn known_no_pdf(provider: &str, model: &str) -> bool {
    let key = text_only_key(provider, model);
    crate::marks::read(|m| m.has_document(&key)).unwrap_or(false)
}

/// Whether this refusal means "not this kind of content". Vendors word it differently, so it
/// matches `image_url` anywhere or a content type complaint naming text, and is consulted only for
/// a 400 on a request that carried a picture.
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

/// What one picture becomes: sent, or the reason not, checked by scope: model, bytes, this
/// request's own budget, then what a provider said. `pub(crate)` so every protocol answers the same
/// way, and `dropped` is the budget's reason where a door has one ([`crate::budget::Budget`]).
pub(crate) fn picture_fate(
    data_ref: &str,
    eyes: Eyes<'_>,
    withheld: Withheld<'_>,
    dropped: Option<&str>,
) -> Option<String> {
    if !eyes.takes_images() {
        return Some(eyes.why_not().unwrap_or_default());
    }
    if !letibot_transcript::media::data_ref_is_image(data_ref) {
        return Some(NOT_A_PICTURE.to_string());
    }
    if let Some(why) = dropped {
        return Some(why.to_string());
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

/// The one line a PDF gets when it is not sent as a document. The text comes from the row's own
/// bytes at send time, so nothing about the stored row changes and a second turn reads the same
/// words: a placeholder whose text moved would re-prefill every cached turn.
pub(crate) fn document_said(nth: usize, mime: &str, data_ref: &str, why: &str) -> String {
    let bytes = data_ref_bytes(data_ref);
    let size = match bytes {
        Some(n) => size_said(n),
        None => "size unknown".to_string(),
    };
    let payload =
        letibot_transcript::media::decode_base64(letibot_transcript::media::wire_payload(data_ref));
    // The renderer's (rano's, registered by the daemon) or `pdftotext`'s: `document_text` is the
    // one door, because a prompt that moved between the two would re-prefill every cached turn.
    match letibot_transcript::media::document_text(&payload, &format!("document {nth}")) {
        Some(text) => format!("[document {nth} · {mime} · {size} — sent as text: {why}]\n{text}"),
        // Nothing here could read it: the model is told what it did not get, which is the one thing
        // it can act on. A file's name alone would read as the document's contents.
        None => format!(
            "[document {nth} · {mime} · {size} — not sent: {why}, and nothing on this box could \
             read a PDF (no renderer, no `pdftotext`), so its contents were not read]"
        ),
    }
}

/// A PDF as PICTURES, for a model that sees but takes no document: the same bytes it refused as a
/// file are what it can read page by page. The header says how many pages the document has and how
/// many of them are here, because a silent ten of forty would read as the whole thing. The parts are
/// the protocol's own (an `image_url`, an `inlineData`, an `input_image`), so only the references
/// and the sentence come from here.
pub(crate) fn document_pictures(
    nth: usize,
    mime: &str,
    data_ref: &str,
) -> Option<(String, Vec<String>)> {
    let payload =
        letibot_transcript::media::decode_base64(letibot_transcript::media::wire_payload(data_ref));
    let (pages, shots) =
        letibot_transcript::pdf::pages_as_png(&payload, letibot_transcript::pdf::PAGE_DPI)?;
    if shots.is_empty() {
        return None;
    }
    let said = format!(
        "[document {nth} · {mime} — sent as {} page picture(s) of {pages}: this model takes no PDF, \
         but it sees, so its pages are attached]",
        shots.len()
    );
    let refs = shots
        .into_iter()
        .map(|png| {
            format!(
                "data:image/png;base64,{}",
                letibot_transcript::media::encode_base64(&png)
            )
        })
        .collect();
    Some((said, refs))
}

/// Whether a document a door cannot take should go as page PICTURES instead of as its text.
/// The model must SEE, and the reason the file is not sent must be the door rather than the model's
/// own refusal of documents: a model that refused a PDF outright is one whose pictures of it would
/// earn the same 400 again.
pub(crate) fn document_as_pictures(senses: Senses<'_>) -> bool {
    senses.eyes.takes_images()
        && senses
            .pdf_why
            .is_some_and(|why| !why.contains("refused a PDF"))
}

/// Does this refusal name a DOCUMENT rather than the model or a picture? Consulted only for a 400
/// on a request that carried a PDF, so it can afford to be generous: a refusal that names the file
/// part at all is the document's.
pub fn document_refusal(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    [
        "pdf",
        "document",
        "file_data",
        "file_data",
        "file_url",
        "file part",
        "\"file\"",
    ]
    .iter()
    .any(|w| b.contains(w))
}

/// Does this request carry a PDF at all? The guard on the document retry, for the same reason
/// [`carries_pictures`] is the guard on the picture one.
pub fn carries_documents<'a>(items: impl IntoIterator<Item = &'a TranscriptItem>) -> bool {
    items.into_iter().any(|item| match item {
        TranscriptItem::User { parts, .. } => {
            parts.iter().any(|p| matches!(p, UserPart::Document { .. }))
        }
        TranscriptItem::ToolResult { media, .. } => media.as_ref().is_some_and(|m| m.is_document()),
        _ => false,
    })
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

/// The pictures a chat-completions request carries, in the order the walk meets them: a user part's
/// image, and a tool result's media unless it is a DOCUMENT (whose own fate is `document_said`).
/// Not [`pictures`], which counts a tool result's document as a picture - that is the question a
/// refusal retry asks, and it is a different one.
fn carried_pictures<'a>(items: impl IntoIterator<Item = &'a TranscriptItem>) -> Vec<&'a str> {
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
            TranscriptItem::ToolResult { media: Some(m), .. } if !m.is_document() => {
                out.push(m.data_ref.as_str())
            }
            _ => {}
        }
    }
    out
}

/// The reason this request drops the picture it is looking at, if it does: the budget's sentence,
/// or `None` when this one is kept (or there is no budget at all).
fn budget_drop<'a>(kept: &[bool], said: Option<&'a str>, at: &mut usize) -> Option<&'a str> {
    let goes = kept.get(*at).copied().unwrap_or(true);
    *at += 1;
    match goes {
        true => None,
        false => said,
    }
}

/// The `messages` array. `system` is prepended when non-empty and no `System` item leads the
/// transcript. `echo_reasoning` is this harness's own choice about how much to send: the caller
/// computes it, because the caller knows whether `tools` is about to go.
pub fn convert(
    system: &str,
    items: &[TranscriptItem],
    echo_reasoning: bool,
    senses: Senses<'_>,
    withheld: Withheld<'_>,
) -> Vec<Value> {
    convert_with_tail(
        system,
        items,
        &[],
        echo_reasoning,
        senses,
        withheld,
        crate::budget::Budget::NONE,
    )
}

/// The same conversion, with items the request carries and the transcript does not
/// (`TurnRequest::tail`). The tail goes through the SAME walk, because a tail with its own
/// converter could disagree with the transcript's, and it goes last as an unrefused shape.
/// `budget` is how many pictures this door will carry: the newest that fit, the rest named.
pub fn convert_with_tail(
    system: &str,
    items: &[TranscriptItem],
    tail: &[TranscriptItem],
    echo_reasoning: bool,
    senses: Senses<'_>,
    withheld: Withheld<'_>,
    budget: crate::budget::Budget,
) -> Vec<Value> {
    // Which pictures this request can carry, decided ONCE and before the walk: the newest are the
    // ones kept, and the walk meets the oldest first.
    let carried = carried_pictures(items.iter().chain(tail.iter()));
    let sizes: Vec<Option<u64>> = carried
        .iter()
        .map(|d| data_ref_bytes(d).map(|n| n as u64))
        .collect();
    let kept = budget.keeps(&sizes);
    let kept_count = kept.iter().filter(|k| **k).count();
    let budget_said = (kept_count < kept.len()).then(|| budget.said(kept_count, kept.len()));
    let mut at_picture = 0usize;
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
                let mut documents = 0usize;
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
                            // an image, this request's budget kept only the newest, or a provider
                            // refused these bytes.
                            let dropped =
                                budget_drop(&kept, budget_said.as_deref(), &mut at_picture);
                            match picture_fate(data_ref, senses.eyes, withheld, dropped) {
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
                        // A document is a `file` part where the door takes one (Z.ai's shape for
                        // GLM, which is the chat-completions file part). A door that takes no file
                        // gets the pages as PICTURES when the model sees, and its text otherwise:
                        // the whole point of a fallback is that the model still reads what is in it.
                        UserPart::Document {
                            media_type,
                            data_ref,
                            filename,
                        } => {
                            documents += 1;
                            match senses.pdf_why {
                                None => content.push(file_part(filename, data_ref)),
                                Some(why) => {
                                    let pictures = document_as_pictures(senses)
                                        .then(|| document_pictures(documents, media_type, data_ref))
                                        .flatten();
                                    match pictures {
                                        Some((said, refs)) => {
                                            content.push(json!({"type": "text", "text": said}));
                                            for r in refs {
                                                content.push(json!({
                                                    "type": "image_url",
                                                    "image_url": {"url": r},
                                                }));
                                            }
                                        }
                                        None => content.push(json!({
                                            "type": "text",
                                            "text": document_said(
                                                documents, media_type, data_ref, why
                                            ),
                                        })),
                                    }
                                }
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
            item if !item.reaches_model() => {}
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
                ran_ms: _,
                decision: _,
                media,
                origin,
            } => {
                if matches!(
                    origin,
                    Some(letibot_transcript::CallOrigin::Operator { .. })
                ) {
                    own_author.insert(call_id.clone());
                }
                // The payload already names its outcome in its own envelope, so no prefix here.
                // Some servers (Kimi's) refuse an empty string, so empty output says so instead.
                let content = match payload.trim().is_empty() {
                    true => "(no output)".to_string(),
                    false => payload.clone(),
                };
                out.push(json!({"role": "tool", "tool_call_id": call_id, "content": content}));
                // When the call read a picture, the picture follows as its own user message, the
                // shape every vision API accepts. A model that does not see gets the words and one
                // plain string instead, so the prefix is not an array where a string was.
                if let Some(m) = media {
                    // A document a call read rides the same channel, with its own part shape: the
                    // request that carried it is the one that names the file.
                    if m.is_document() {
                        out.push(json!({
                            "role": "user",
                            "content": match senses.pdf_why {
                                None => json!([
                                    {"type": "text", "text": "Attached document from tool result:"},
                                    file_part("document.pdf", &m.data_ref),
                                ]),
                                Some(why) => json!(format!(
                                    "Attached document from tool result: {}",
                                    document_said(1, &m.mime, &m.data_ref, why)
                                )),
                            },
                        }));
                    } else {
                        // The same predicate as a head's own picture: the row carrying the bytes that
                        // were refused is history, so the renderer is where it stops being sent.
                        let dropped = budget_drop(&kept, budget_said.as_deref(), &mut at_picture);
                        match picture_fate(&m.data_ref, senses.eyes, withheld, dropped) {
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
    }
    pair_tool_calls(out, &own_author)
}

/// The chat-completions `file` part a PDF rides: the bytes inline with the name beside them, which
/// is Z.ai's documented shape (`file_data`, `filename`) and the one every OpenAI-compatible door
/// that takes a document reads - <https://docs.z.ai/api-reference/llm/chat-completion.md>.
pub(crate) fn file_part(filename: &str, data_ref: &str) -> Value {
    json!({
        "type": "file",
        "file": {"filename": filename, "file_data": data_ref},
    })
}

/// Every `tool_calls` gets its `tool` message, or the provider refuses the conversation. A missing
/// result is filled with a note that none was recorded; an orphan `tool` message is dropped,
/// unless the operator ran it, in which case it becomes the person's own turn ([`person_ran`]).
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

/// A request may not end on a system message: with `tools`, providers refuse it, naming
/// `reasoning_content` instead of the shape. The record stays right and the trailing instruction is
/// sent as a user turn on this transport, for every vendor.
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

/// Give every untyped `enum` node in a tool schema the `type` its values share. A node whose
/// values mix types is left alone: no single `type` would be true of it.
pub fn type_enums(v: &mut Value) {
    match v {
        Value::Object(o) => {
            if !o.contains_key("type")
                && let Some(Value::Array(vals)) = o.get("enum")
            {
                let kind = |x: &Value| match x {
                    Value::String(_) => Some("string"),
                    Value::Bool(_) => Some("boolean"),
                    Value::Number(n) if n.is_i64() || n.is_u64() => Some("integer"),
                    Value::Number(_) => Some("number"),
                    _ => None,
                };
                let mut kinds = vals.iter().map(kind);
                if let Some(Some(first)) = kinds.next()
                    && kinds.all(|k| k == Some(first))
                {
                    o.insert("type".into(), json!(first));
                }
            }
            o.values_mut().for_each(type_enums);
        }
        Value::Array(a) => a.iter_mut().for_each(type_enums),
        _ => {}
    }
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
            Senses::of(Eyes::Sees),
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
            ran_ms: None,
            decision: None,
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
            ran_ms: None,
            decision: None,
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
