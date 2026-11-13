//! The model card: two levels. The first is the model answering now, the ones used recently and one
//! row per provider; a provider opens onto its own models. A query at either level is one ranked
//! list across every provider, because a typed name asks "where is it", not "which provider".

use super::*;

/// One model as the daemon offers it, with the door its switch name is spelled by: two doors that
/// serve one catalogue carry the same models, and the card draws each of them once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Model {
    /// The switch name the daemon published, `door/model`, or a bare `local`/fleet name.
    pub(crate) name: String,
    /// The daemon's own line about it: the window and the two rates.
    pub(crate) facts: String,
    /// The door this name is spelled with, empty for a bare name.
    pub(crate) door: String,
    /// The model without its door: what the card draws, and what two doors share.
    pub(crate) model: String,
}

/// One provider: what a level-one row opens, and the models behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Provider {
    /// What `model_open` holds while this provider's models are drawn.
    pub(crate) key: String,
    /// The name the row is labelled with: the first door that serves it.
    pub(crate) label: String,
    /// Every door that serves these models, in the daemon's order. Empty for `local` and the
    /// declared fleet, which are an address rather than a provider.
    pub(crate) doors: Vec<String>,
    /// This box can take a model of it: it holds a key for one door, or needs none.
    pub(crate) ready: bool,
    /// Nothing has to be authenticated for it: `local` and the fleet.
    pub(crate) keyless: bool,
    /// The models, each once, whoever's door offered it.
    pub(crate) models: Vec<Model>,
}

/// What the card as a whole holds, gathered once per frame so the drawing and the cursor read one
/// answer: a second pass over the settings rows could disagree with the rows already drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ModelCard {
    /// One row per provider, the ones this box can take first.
    pub(crate) providers: Vec<Provider>,
    /// The model answering now, as the daemon spelled it, when it is one of the rows.
    pub(crate) now: Option<String>,
    /// The models used before it, newest first, at most [`RECENT_MODELS`].
    pub(crate) recent: Vec<String>,
}

/// The key of the one level-one row every provider this box holds no key for folds into. A door
/// name never starts with a space, so no provider is spelled this way.
pub(crate) const NOT_CONNECTED: &str = " not connected";

/// How many models the recent group holds: the last few choices and no more, so the group never
/// costs the providers their room on a 45-line screen.
pub(crate) const RECENT_MODELS: usize = 5;

/// One line of the card. A model's or a provider's line carries its index in the card's takeable
/// rows, so the cursor counts exactly what is drawn and a click lands on the row it named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Line {
    /// A heading: `now`, `recent`, `providers`, or the provider whose models are open.
    Group(String),
    /// The labels of the model columns under them.
    Columns,
    /// A model, by its index in the takeable rows.
    Model(usize),
    /// A provider, by its index in the takeable rows.
    Provider(usize),
}

/// What Enter takes on the model card: one row of the list the cursor counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Take {
    /// A model: taking it switches this session, from the next turn.
    Model {
        /// The switch name, spelled with the provider's active door.
        name: String,
        facts: String,
        /// The characters of the name the query matched, for the highlight. Empty without one.
        at: Vec<usize>,
    },
    /// A provider: Enter opens it, or asks for the key that would.
    Provider {
        key: String,
        /// What the row is labelled: the door the models are reached by, which is the word a
        /// reader types and the one the cursor is pointed at in the tests.
        label: String,
        /// The switch a keyless box would make taking it: the key card's choice.
        model: String,
    },
}

impl Take {
    /// The name the row answers to, which is what seeds a cursor on the model that is in force.
    pub(crate) fn name(&self) -> &str {
        match self {
            Take::Model { name, .. } => name,
            Take::Provider { label, .. } => label,
        }
    }
}

impl App {
    /// The doors as the daemon published them: `(door, catalogue)` in its own order. Empty for a
    /// daemon older than the row, and every provider is then its own group.
    pub(crate) fn model_doors(&self) -> Vec<(String, String)> {
        self.settings
            .iter()
            .find(|r| r.key == letibot_sessionlog::protocol::MODEL_DOORS_KEY)
            .map(|r| {
                r.choices
                    .iter()
                    .filter_map(|c| {
                        let (door, catalogue) = c.split_once(' ')?;
                        Some((door.trim().to_string(), catalogue.trim().to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The models the store's sessions were last moved to, newest first - the card's recent group.
    pub(crate) fn recent_models_setting(&self) -> Vec<String> {
        self.settings
            .iter()
            .find(|r| r.key == letibot_sessionlog::protocol::MODEL_RECENT_KEY)
            .map(|r| {
                r.value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The card, gathered from the daemon's rows: one provider per catalogue, every model once, and
    /// the ready ones first - a row that works is worth more than one that asks for a key.
    pub(crate) fn model_card(&self) -> ModelCard {
        let doors = self.model_doors();
        let catalogue = |door: &str| {
            doors
                .iter()
                .find(|(d, _)| d == door)
                .map(|(_, c)| c.clone())
        };
        let mut providers: Vec<Provider> = Vec::new();
        for (name, facts) in self.model_choices() {
            let (door, model) = match name.split_once('/') {
                Some((d, m)) => (d.to_string(), m.to_string()),
                None => (String::new(), name.clone()),
            };
            // A door no row names is its own group: a card that dropped it would lose a model the
            // daemon can switch to.
            let key = match door.is_empty() {
                true => model.clone(),
                false => catalogue(&door).unwrap_or_else(|| door.clone()),
            };
            let at = match providers.iter().position(|p| p.key == key) {
                Some(at) => at,
                None => {
                    providers.push(Provider {
                        key: key.clone(),
                        label: if door.is_empty() {
                            model.clone()
                        } else {
                            door.clone()
                        },
                        doors: Vec::new(),
                        ready: false,
                        keyless: false,
                        models: Vec::new(),
                    });
                    providers.len() - 1
                }
            };
            let p = &mut providers[at];
            if !door.is_empty() && !p.doors.contains(&door) {
                p.doors.push(door.clone());
            }
            // The same model behind two doors is one row: which door answers is the card's chosen
            // door and not the daemon's list order.
            if p.models.iter().any(|m| m.model == model) {
                continue;
            }
            p.models.push(Model {
                name,
                facts,
                door,
                model,
            });
        }
        let keyless = self.keyless_choices();
        for p in &mut providers {
            p.keyless = p.doors.is_empty() && keyless.iter().any(|k| k == &p.label);
            p.ready = if p.doors.is_empty() {
                self.choice_ready(&p.label)
            } else {
                p.doors.iter().any(|d| self.choice_ready(d))
            };
        }
        // Ready first, and the daemon's own order under that: a provider this box can reach is the
        // one a reader is looking for, and one it cannot is a row about a key.
        providers.sort_by_key(|p| !p.ready);
        let now = self.pick_current();
        let card = ModelCard {
            providers,
            now: None,
            recent: Vec::new(),
        };
        let named = |card: &ModelCard, name: &str| -> Option<String> {
            // The daemon spells what answers in several ways: `local`, `preset/model`, and
            // `name (model at url)` for a declared fleet block. The first word names a row in every
            // case, which is the same reading `choice_ready` makes of the same spellings.
            let head = name.split_whitespace().next().unwrap_or(name);
            let is =
                |m: &Model| m.name == name || m.model == name || m.name == head || m.model == head;
            let p = card
                .providers
                .iter()
                .find(|p| p.models.iter().any(is) || p.doors.iter().any(|d| d == name))?;
            let m = p.models.iter().find(|m| is(m)).or(p.models.first())?;
            Some(m.name.clone())
        };
        let now = named(&card, &now);
        let mut recent: Vec<String> = Vec::new();
        for name in self.recent_models_setting() {
            let Some(model) = named(&card, &name) else {
                continue;
            };
            if Some(&model) == now.as_ref() || recent.contains(&model) {
                continue;
            }
            recent.push(model);
            if recent.len() == RECENT_MODELS {
                break;
            }
        }
        ModelCard {
            providers: card.providers,
            now,
            recent,
        }
    }

    /// The door a provider's models are spelled with, as an index into its `doors`. The choice a
    /// reader made with Tab when they made one, else the first door this box holds a key for, else
    /// the first - so the name a row switches by is one that can be taken.
    pub(crate) fn model_door_at(&self, p: &Provider) -> usize {
        if let Some(chosen) = self.model_door.get(&p.key) {
            return (*chosen).min(p.doors.len().saturating_sub(1));
        }
        p.doors
            .iter()
            .position(|d| self.choice_ready(d))
            .unwrap_or(0)
    }

    /// A model's switch name, spelled with the door its provider is on.
    pub(crate) fn model_switch(&self, p: &Provider, m: &Model) -> String {
        match p.doors.is_empty() {
            true => m.model.clone(),
            false => format!("{}/{}", p.doors[self.model_door_at(p)], m.model),
        }
    }

    /// The card's lines and the rows its cursor counts, built together: a heading costs a line and
    /// takes no cursor, and one pass is what keeps a click and Enter counting the same rows.
    pub(crate) fn model_card_rows(&self) -> (ModelCard, Vec<Line>, Vec<Take>) {
        let card = self.model_card();
        let mut lines: Vec<Line> = Vec::new();
        let mut takes: Vec<Take> = Vec::new();
        let query = self.model_query.trim().to_lowercase();
        if !query.is_empty() {
            let hits = ranked(&card, &query, self);
            if hits.is_empty() {
                // Said on the card and not only in the notice: a list that is empty because of a
                // line typed on it is a different fact from a daemon that named no models.
                lines.push(Line::Group(format!(
                    "nothing here matches {:?} — esc clears the line",
                    self.model_query.trim()
                )));
            }
            for hit in hits {
                lines.push(Line::Model(takes.len()));
                takes.push(Take::Model {
                    name: self.model_switch(hit.provider, hit.model),
                    facts: hit.model.facts.clone(),
                    at: hit.at,
                });
            }
            return (card, lines, takes);
        }
        let unkeyed: Vec<&Provider> = match self.keys_row_present() {
            true => card.providers.iter().filter(|p| !p.ready).collect(),
            false => Vec::new(),
        };
        if self.model_open.as_deref() == Some(NOT_CONNECTED) {
            lines.push(Line::Group(format!(
                "not connected · {} provider{} · enter asks for a key",
                unkeyed.len(),
                plural(unkeyed.len())
            )));
            for p in &unkeyed {
                lines.push(Line::Provider(takes.len()));
                takes.push(self.provider_take(p));
            }
            return (card, lines, takes);
        }
        if let Some(p) = self
            .model_open
            .as_ref()
            .and_then(|key| card.providers.iter().find(|p| &p.key == key))
        {
            lines.push(Line::Group(open_heading(self, p)));
            lines.push(Line::Columns);
            for m in &p.models {
                lines.push(Line::Model(takes.len()));
                takes.push(Take::Model {
                    name: self.model_switch(p, m),
                    facts: m.facts.clone(),
                    at: Vec::new(),
                });
            }
            return (card, lines, takes);
        }
        // Level one: what answers, what was used, and a row per provider.
        for (heading, names) in [
            ("now", card.now.iter().collect::<Vec<_>>()),
            ("recent", card.recent.iter().collect::<Vec<_>>()),
        ] {
            if names.is_empty() {
                continue;
            }
            lines.push(Line::Group(heading.to_string()));
            for name in names {
                let Some((p, m)) = card
                    .providers
                    .iter()
                    .find_map(|p| p.models.iter().find(|m| &m.name == name).map(|m| (p, m)))
                else {
                    continue;
                };
                lines.push(Line::Model(takes.len()));
                takes.push(Take::Model {
                    name: self.model_switch(p, m),
                    facts: m.facts.clone(),
                    at: Vec::new(),
                });
            }
        }
        if !card.providers.is_empty() {
            lines.push(Line::Group("providers".to_string()));
        }
        // Keyed first, and the unkeyed ones behind a single row: a row per provider this box cannot
        // take is a list of keys nobody asked about.
        for p in card.providers.iter().filter(|p| !unkeyed.contains(p)) {
            lines.push(Line::Provider(takes.len()));
            takes.push(self.provider_take(p));
        }
        if !unkeyed.is_empty() {
            lines.push(Line::Provider(takes.len()));
            takes.push(Take::Provider {
                key: NOT_CONNECTED.to_string(),
                label: "not connected".to_string(),
                model: String::new(),
            });
        }
        (card, lines, takes)
    }

    fn provider_take(&self, p: &Provider) -> Take {
        Take::Provider {
            key: p.key.clone(),
            label: p.label.clone(),
            model: p
                .models
                .first()
                .map(|m| self.model_switch(p, m))
                .unwrap_or_default(),
        }
    }

    /// The card's rows and what the cursor counts, without the snapshot the drawing path wants.
    pub(crate) fn model_rows(&self) -> (Vec<Line>, Vec<Take>) {
        let (_, lines, takes) = self.model_card_rows();
        (lines, takes)
    }

    /// Enter on the model card: the row the cursor is on, whatever kind it is. A provider this box
    /// cannot take asks for the key through the row's own switch, so the card and the key card agree
    /// on what is being taken.
    pub(crate) fn model_card_enter(&mut self) -> Option<Action> {
        let (_, takes) = self.model_rows();
        // The settings answer can land while the cursor is at a row this answer no longer has; the
        // last row is nearer what the reader was looking at than doing nothing on Enter.
        let at = self.mode_sel.min(takes.len().saturating_sub(1));
        match takes.get(at).cloned() {
            Some(Take::Model { name, .. }) => self.take_pick(name),
            Some(Take::Provider { key, .. }) if key == NOT_CONNECTED => {
                self.model_open = Some(key);
                self.mode_sel = 0;
                self.pick_unseeded = false;
                None
            }
            Some(Take::Provider { key, model, .. }) => {
                // A provider the daemon did not describe - no keys row at all - opens anyway: the
                // head cannot tell, and taking its first model is not better than showing them.
                let opens = self
                    .model_card()
                    .providers
                    .iter()
                    .find(|p| p.key == key)
                    .is_some_and(|p| p.ready || !self.keys_row_present());
                if opens {
                    self.model_open = Some(key.clone());
                    self.mode_sel = 0;
                    self.pick_unseeded = false;
                    return None;
                }
                self.take_pick(model)
            }
            None => None,
        }
    }

    /// The provider whose models are open, when the daemon still offers it.
    pub(crate) fn model_open_provider(&self) -> Option<Provider> {
        let key = self.model_open.clone()?;
        self.model_card()
            .providers
            .into_iter()
            .find(|p| p.key == key)
    }

    /// Cycle the door the open provider's models are spelled with: two doors of one catalogue serve
    /// the same models, and which wire answers is the operator's call.
    pub(crate) fn model_next_door(&mut self) {
        let Some(p) = self.model_open_provider() else {
            return;
        };
        if p.doors.len() < 2 {
            return;
        }
        let now = self.model_door_at(&p);
        self.model_door
            .insert(p.key.clone(), (now + 1) % p.doors.len());
    }

    /// Esc on the model card, one step at a time: the query first, then the open provider, and only
    /// then the card. It is the shape a reader who typed something expects, and the card is the one
    /// surface here that Esc can leave in two different ways.
    pub(crate) fn model_card_back(&mut self) {
        if !self.model_query.is_empty() {
            self.model_query.clear();
            self.mode_sel = 0;
        } else if self.model_open.is_some() {
            self.model_open = None;
            self.mode_sel = 0;
        } else {
            self.pick = None;
        }
        self.pick_unseeded = false;
    }
}

/// The heading of an open provider: its name, whether this box can take it, and the doors it is
/// reachable by - the only line that says a second door exists.
fn open_heading(a: &App, p: &Provider) -> String {
    let state = if p.keyless {
        "this box's own".to_string()
    } else if p.ready {
        "keyed".to_string()
    } else {
        "not connected".to_string()
    };
    let doors = match p.doors.len() {
        0 | 1 => String::new(),
        n => {
            let at = a.model_door_at(p);
            let others: Vec<&str> = p
                .doors
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != at)
                .map(|(_, d)| d.as_str())
                .collect();
            format!(
                " · {n} doors, on {} · tab takes {}",
                p.doors[at],
                others.join(" or ")
            )
        }
    };
    format!(
        "{} · {state} · {} model{}{doors}",
        p.label,
        p.models.len(),
        plural(p.models.len())
    )
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A model a query matched, with the characters of its name that were the match - what the card
/// highlights, so a reader sees why a row is in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelHit<'a> {
    pub(crate) provider: &'a Provider,
    pub(crate) model: &'a Model,
    /// Character positions in `model.name`, in order.
    pub(crate) at: Vec<usize>,
}

/// The models a query matches, best first, each once. A hit's tier decides first, then whether the
/// box can take it, then how short its name is: `pro` is `deepseek-v4-pro` before it is
/// `lyria-3-pro-preview`, and never a scattering of letters above a name that contains the word.
fn ranked<'a>(card: &'a ModelCard, query: &str, a: &App) -> Vec<ModelHit<'a>> {
    let mut hits: Vec<(u8, bool, usize, &Provider, &Model)> = Vec::new();
    for p in &card.providers {
        for m in &p.models {
            let Some(tier) = rank(&m.name, query) else {
                continue;
            };
            let ready = p.ready && a.choice_ready(&a.model_switch(p, m));
            hits.push((tier, !ready, m.name.chars().count(), p, m));
        }
    }
    // A scattered match is a fallback, never a companion: once one row contains what was typed, the
    // subsequence rows are noise, and the complaint that opened this change was exactly that noise.
    if hits.iter().any(|h| h.0 < SUBSEQUENCE) {
        hits.retain(|h| h.0 < SUBSEQUENCE);
    }
    hits.sort_by(|x, y| (x.0, x.1, x.2, &x.4.name).cmp(&(y.0, y.1, y.2, &y.4.name)));
    hits.into_iter()
        .map(|(_, _, _, provider, model)| ModelHit {
            provider,
            model,
            at: matched(&model.name, query),
        })
        .collect()
}

/// The tier a subsequence match sits at, and the last one: see [`rank`].
pub(crate) const SUBSEQUENCE: u8 = 4;

/// How well one model's name answers what was typed, lowest first: the whole name or the model part
/// alone, a prefix of either, the start of a word inside it, anywhere inside it, and last its
/// letters in order. `None` is no match at all.
pub(crate) fn rank(name: &str, query: &str) -> Option<u8> {
    let (name, query) = (name.to_lowercase(), query.to_lowercase());
    let model = name.rsplit('/').next().unwrap_or(&name);
    if name == query || model == query {
        return Some(0);
    }
    if name.starts_with(&query) || model.starts_with(&query) {
        return Some(1);
    }
    if word_start(&name, &query) {
        return Some(2);
    }
    if name.contains(&query) {
        return Some(3);
    }
    subsequence(&name, &query).then_some(SUBSEQUENCE)
}

/// Whether what was typed begins a word of the name: `pro` starts the `pro` of `deepseek-v4-pro`,
/// and `deep` does not start the `deep` of `deepseek` by this test - a prefix already answered it.
fn word_start(name: &str, query: &str) -> bool {
    let mut from = 0;
    while let Some(at) = name[from..].find(query) {
        let at = from + at;
        let before = name[..at].chars().next_back();
        if before.is_none_or(|c| !c.is_alphanumeric()) {
            return true;
        }
        from = at + query.len().max(1);
    }
    false
}

/// The query's letters in order inside the name, which is how `dflash` reaches
/// `deepseek/deepseek-v4-flash`.
fn subsequence(name: &str, query: &str) -> bool {
    let mut letters = name.chars();
    query.chars().all(|c| letters.any(|x| x == c))
}

/// The characters of a name that the query matched, for the highlight: the contiguous run for a real
/// match, and the scattered ones for a subsequence.
pub(crate) fn matched(name: &str, query: &str) -> Vec<usize> {
    let (lower, want) = (name.to_lowercase(), query.to_lowercase());
    if want.is_empty() {
        return Vec::new();
    }
    if let Some(at) = lower.find(&want) {
        return (at..at + want.len())
            .map(|b| lower[..b].chars().count())
            .collect();
    }
    let mut out = Vec::new();
    let mut chars = lower.chars().enumerate();
    for c in want.chars() {
        for (i, x) in chars.by_ref() {
            if x == c {
                out.push(i);
                break;
            }
        }
    }
    out
}

/// The daemon's fact line as the card's columns, `(window, in, out)`. `None` for a line it did not
/// write that way - `local`'s "no key, no meter" - which is drawn whole rather than mangled into
/// columns that would say the wrong thing about it.
pub(crate) fn columns(facts: &str) -> Option<(String, String, String)> {
    let (window, rest) = facts.split_once(" window · ")?;
    if window.trim().is_empty() {
        return None;
    }
    if rest == "price unlisted" {
        return Some((window.trim().to_string(), "—".into(), "—".into()));
    }
    let rest = rest.strip_suffix(" per M")?;
    let (input, output) = rest.split_once(" in / ")?;
    let output = output.strip_suffix(" out")?;
    Some((
        window.trim().to_string(),
        input.trim().to_string(),
        output.trim().to_string(),
    ))
}

/// A provider's price as one column: the cheapest and dearest rate of what it offers, which is the
/// one number a person compares two providers by. A provider whose models the catalogue prices
/// nowhere says so; the daemon's own server, which has no catalogue row at all, has no meter.
pub(crate) fn price_range(p: &Provider) -> String {
    let mut rates: Vec<(f64, String)> = Vec::new();
    let mut listed = false;
    for m in &p.models {
        let Some((_, input, _)) = columns(&m.facts) else {
            continue;
        };
        listed = true;
        if let Ok(rate) = input.trim_start_matches('$').parse::<f64>() {
            rates.push((rate, input));
        }
    }
    rates.sort_by(|x, y| x.0.total_cmp(&y.0));
    match (rates.first(), rates.last()) {
        (Some(lo), Some(hi)) if lo.1 == hi.1 => format!("{} /M", lo.1),
        (Some(lo), Some(hi)) => format!("{}–{} /M", lo.1, hi.1),
        _ if listed => "price unlisted".to_string(),
        _ => "no meter".to_string(),
    }
}
