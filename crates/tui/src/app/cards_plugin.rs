//! The head's cards, asked of the `cards` layer when a layer on disk answers `card_`, and drawn from
//! the row's facts and the shared readers otherwise: one ask per transcript row, kept by its id. Both
//! are `letibot_plugin::card`'s own functions, so which of the two drew a line is invisible.

use super::sessions_pane::Host;
use super::*;
use crate::ui::transcript::line_cards::Card;
use letibot_plugin::card as plugin_card;
use letibot_transcript::TranscriptItem;
use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// How many card answers one head keeps: a transcript window is bounded, and a session that has read
/// more rows than this re-asks for the ones it draws again - one call per row, once.
pub(crate) const MAX_CARDS: usize = 4096;

/// The answers a `card_` layer gave, evicted least-recently-used.
///
/// A row's id alone is not a key: an edited row is the same id with different words, and a card
/// cached under the id would draw the old sentence for the rest of the session. The key carries a
/// hash of the input the layer was asked with, so an edit is a miss and a redrawn frame is not.
#[derive(Default)]
pub(crate) struct CardCache {
    /// The answer's place in [`CardCache::by_age`], by [`CardCache::key`].
    at: BTreeMap<String, u64>,
    /// The same answers in the order they were last asked for, so the front is the one to evict.
    by_age: BTreeMap<u64, (String, Option<serde_json::Value>)>,
    tick: u64,
}

impl CardCache {
    /// What a card is asked about: the hook, the row, and the input's own bytes - so a row drawn
    /// again the same way is the same question, and one word changed is a different one.
    pub(crate) fn key(hook: &str, id: &str, input: &serde_json::Value) -> String {
        let mut hasher = DefaultHasher::new();
        hook.hash(&mut hasher);
        id.hash(&mut hasher);
        input.to_string().hash(&mut hasher);
        format!("{hook}\n{id}\n{:016x}", hasher.finish())
    }

    /// The answer kept for this key, marked as the most recently used. `Some(None)` is a row the
    /// layer drew nothing for, which is an answer too: asking again would get the same nothing.
    pub(crate) fn get(&mut self, key: &str) -> Option<Option<serde_json::Value>> {
        let old = self.at.get(key).copied()?;
        let (_, answer) = self.by_age.remove(&old)?;
        self.tick += 1;
        self.by_age
            .insert(self.tick, (key.to_string(), answer.clone()));
        self.at.insert(key.to_string(), self.tick);
        Some(answer)
    }

    /// Keep one answer, dropping the least recently used when the cap is reached.
    pub(crate) fn insert(&mut self, key: String, answer: Option<serde_json::Value>) {
        self.tick += 1;
        if let Some(old) = self.at.insert(key.clone(), self.tick) {
            self.by_age.remove(&old);
        }
        self.by_age.insert(self.tick, (key, answer));
        while self.by_age.len() > MAX_CARDS {
            let Some((_, (gone, _))) = self.by_age.pop_first() else {
                break;
            };
            self.at.remove(&gone);
        }
    }

    /// How many answers are kept - a test's measure of the bound, and nothing else's.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.at.len()
    }
}

/// One row, as the layer is asked about it: what it is, its words, and the facts it carries.
struct Row<'a> {
    item: &'a TranscriptItem,
    id: &'a str,
}

/// The cards for one row, and the layer to ask. Cheap to build - a frame builds one per row - and
/// the answer behind it is kept in the head's [`Host`] by row id and the input's own bytes, so a row
/// drawn on a hundred frames costs one call and an edited one costs another.
pub(crate) struct Cards<'a> {
    host: Option<&'a Host>,
    id: &'a str,
    row: Option<Row<'a>>,
}

impl<'a> Cards<'a> {
    /// No layer and no row: the readers draw. What a pane outside the conversation (a subagent's
    /// rows) and a test get, and what the head draws with when its stack has no cards layer.
    pub(crate) fn readers_only() -> Cards<'static> {
        Cards {
            host: None,
            id: "",
            row: None,
        }
    }

    /// The layer, and the row it is asked about.
    pub(crate) fn row(host: Option<&'a Host>, it: &'a SnapshotItem) -> Cards<'a> {
        Cards {
            host,
            id: &it.item_id,
            row: it.item.as_ref().map(|item| Row {
                item,
                id: &it.item_id,
            }),
        }
    }

    /// The layer, asked about one call by the id it is drawn under - a row this head has already
    /// read back, or a call whose row it is deciding whether to open.
    pub(crate) fn call(host: Option<&'a Host>, id: &'a str) -> Cards<'a> {
        Cards {
            host,
            id,
            row: None,
        }
    }

    /// The card for each part of the row, in the order the head draws them: the layer's answer where it
    /// gives one, the row's typed facts for the part they are about, and the readers for everything
    /// else. Never empty for a part: a notice no reader knows draws the generic line.
    pub(crate) fn lines(&self, parts: &[&str]) -> Vec<Card> {
        let answered = self.answered(parts);
        parts
            .iter()
            .enumerate()
            .map(|(k, part)| match answered.as_ref().and_then(|a| a.get(k)) {
                Some(Some(lines)) => card_of(lines),
                // The facts describe the delivery the row opens with, so they are the first part's.
                _ if k == 0 => self.facts().unwrap_or_else(|| self.readers(part)),
                _ => self.readers(part),
            })
            .collect()
    }

    /// The card the row's own facts draw, when it carries any - what survives a `notice_` layer
    /// rewording the sentence, and drawn by the SDK's one facts renderer so a layer above answers
    /// the same card this head would.
    fn facts(&self) -> Option<Card> {
        let row = self.row.as_ref()?;
        let TranscriptItem::Notice {
            facts: Some(facts), ..
        } = row.item
        else {
            return None;
        };
        plugin_card::facts::read(facts).map(|l| card_of(&l))
    }

    /// The verb a tool row is drawn as: the layer's answer when it names one of the seven, and the
    /// head's own rule - `notes` hands back a change, so it is drawn as the `edit` or `write` it is
    /// - otherwise.
    pub(crate) fn verb(
        &self,
        name: &str,
        edit: Option<&letibot_transcript::ToolEditExcerpt>,
    ) -> String {
        match self.asked(plugin_card::CALL, 0, call_input(self.id, name, edit)) {
            Some(out) => out,
            None => crate::ui::drawn_as(name, edit).to_string(),
        }
    }

    /// The head's own fallback for one part: the readers, the head's own stale reader (the one shape
    /// they do not carry), and the generic line for everything else.
    fn readers(&self, part: &str) -> Card {
        if let Some(lines) = plugin_card::readers::read(part) {
            return card_of(&lines);
        }
        if let Some(card) = crate::ui::transcript::stale_card::stale_card(part) {
            return card;
        }
        card_of(&plugin_card::readers::generic(part))
    }

    /// One hook asked once per row, and the verb it named when the ask was `card_call`. The input is
    /// built even on a hit: it IS the question, so the cache cannot know the row changed without it.
    fn asked(&self, hook: &str, parts: usize, input: Option<serde_json::Value>) -> Option<String> {
        let host = self.host?;
        let answer = host.card_answer(hook, self.id, input?)?;
        let out = self.read(hook, &answer)?;
        out.cards(parts).ok().and_then(|cards| cards.verb)
    }

    /// The layer's cards for this row, one per part, or `None` for a head with no layer or an answer
    /// the contract refuses - which is the readers' cue, said once.
    pub(crate) fn answered(&self, parts: &[&str]) -> Option<Vec<Option<plugin_card::Lines>>> {
        let host = self.host?;
        let row = self.row.as_ref()?;
        let input = row_input(row, parts)?;
        let answer = host.card_answer(plugin_card::ITEM, self.id, input)?;
        let out = self.read(plugin_card::ITEM, &answer)?;
        match out.cards(parts.len()) {
            Ok(cards) => Some(cards.cards),
            Err(why) => {
                self.complain(
                    plugin_card::ITEM,
                    format!(
                        "a card layer drew nothing this head will draw, so the row is drawn by \
                         its own readers: {why}"
                    ),
                );
                None
            }
        }
    }

    /// The answer off the wire, with a refusal said once - the sentence a person reads when their
    /// layer answered a card this head will not draw.
    fn read(&self, hook: &str, answer: &serde_json::Value) -> Option<plugin_card::Out> {
        let out = match plugin_card::read(answer) {
            Ok(out) => out,
            Err(why) => {
                self.complain(
                    hook,
                    format!("a card layer answered something this head cannot read: {why}"),
                );
                return None;
            }
        };
        Some(out)
    }

    /// A refusal is queued under the hook it answered: the head still draws the row, the person
    /// learns why it is not their layer's line, and the next frame says nothing new.
    fn complain(&self, hook: &str, why: String) {
        if let Some(host) = self.host {
            host.complain(hook, why);
        }
    }
}

/// The card a layer's answer draws: its parts joined the way every card's line is, the accented
/// part in its register, and the title as the popup's top edge.
fn card_of(lines: &plugin_card::Lines) -> Card {
    let line = lines.parts.join(" · ");
    let accented = lines
        .parts
        .get(lines.accent)
        .cloned()
        .unwrap_or_else(|| line.clone());
    Card::new(
        line,
        0,
        &accented,
        crate::ui::panes::plugin::role(lines.role),
        lines.title.clone(),
    )
}

/// What the layer is told about one row: what it is, its words, and the typed facts it was delivered
/// with. The kind word is the contract's, so a row of a kind this build has no word for is asked as
/// a notice - which is what every other row of the conversation is.
fn row_input(row: &Row<'_>, parts: &[&str]) -> Option<serde_json::Value> {
    let text = letibot_sessionlog::view::body_of(row.item);
    let mut item = plugin_card::Item::new(
        row.id,
        match row.item {
            TranscriptItem::ToolResult { .. } => plugin_card::KIND_CALL,
            _ => plugin_card::KIND_NOTICE,
        },
        text,
    );
    if let TranscriptItem::Notice { facts, .. } = row.item {
        item.facts = facts.clone();
    }
    if let TranscriptItem::ToolResult {
        name,
        edits,
        outcome,
        ..
    } = row.item
    {
        let mut call = plugin_card::Call::new(name.clone(), changed(edits));
        call.outcome = Some(crate::ui::transcript::call::outcome_word(outcome).to_string());
        item.call = Some(call);
    }
    let parts = parts.iter().map(|p| (*p).to_string()).collect();
    serde_json::to_value(plugin_card::Input::new(item, parts)).ok()
}

/// The ask about one call, for the verb alone: the name and the change it handed back.
fn call_input(
    id: &str,
    name: &str,
    edit: Option<&letibot_transcript::ToolEditExcerpt>,
) -> Option<serde_json::Value> {
    let mut item = plugin_card::Item::new(id, plugin_card::KIND_CALL, String::new());
    item.call = Some(plugin_card::Call::new(
        name,
        edit.map(|e| {
            vec![plugin_card::Changed {
                path: e.path.clone(),
                created: e.created,
            }]
        })
        .unwrap_or_default(),
    ));
    serde_json::to_value(plugin_card::Input::new(item, Vec::new())).ok()
}

impl App {
    /// Whether a layer on disk answers `card_`, asked at most once a minute - the rule the links'
    /// layers are read by: the head's own readers draw what the built-in layer draws, so only a layer
    /// a person wrote is worth compiling a stack for.
    pub(crate) fn note_card_layers(&mut self) {
        const EVERY_MS: u64 = 60_000;
        if self
            .card_layers
            .is_some_and(|_| self.now_ms.saturating_sub(self.card_layers_at) < EVERY_MS)
        {
            return;
        }
        let workspace = self.workspace_path();
        self.card_layers = Some(Host::any_card_layer(&self.session_id, &workspace));
        self.card_layers_at = self.now_ms;
    }

    /// Whether a `card_` layer is on disk to ask.
    pub(crate) fn card_layers_on(&self) -> bool {
        self.card_layers == Some(true)
    }
}

/// The files a row's call changed, in the row's order.
fn changed(edits: &[letibot_transcript::ToolEditExcerpt]) -> Vec<plugin_card::Changed> {
    edits
        .iter()
        .map(|e| plugin_card::Changed {
            path: e.path.clone(),
            created: e.created,
        })
        .collect()
}
