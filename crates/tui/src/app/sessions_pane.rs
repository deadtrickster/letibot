//! The session list, drawn from a plugin: the built-in `sessions` layer decides the groups, names,
//! counts and keys, and the head draws its answer through rano's one tree. The head runs the layer
//! in process, because the list is how a person gets out of a broken session.
//!
//! There is ONE drawing. While the head's stack is being built the pane says so in one line
//! ([`letibot_plugin::sessions::BUILDING`]) rather than drawing a list of its own that then changes
//! shape under the reader; the words both sides say live in `letibot_plugin::sessions`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_plugin::pane::{Act as RowAct, Feed, Input};
use letibot_plugin::sessions as sdk;
use letibot_plugins::{Refusal, Refusals, Stack};
use letibot_sessionlog::pane as pv;
use rano::agent::pane::PaneLines;
use rano::agent::tree_list::{
    Action as TreeAction, Cell as TreeCell, Fold, Row as TreeRow, TreeList,
};
use serde_json::{Value, json};

use crate::app::*;
use crate::ui::render::{trim_to, visible_width};

/// The hook this pane answers under.
pub(crate) const HOOK: &str = "pane_sessions";

/// One row the list drew, as the keys read it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Listed {
    /// The row's id: a session's, or a group's name. It is the key of the fold state.
    pub(crate) id: String,
    /// The session this row names, as an index into [`App::sessions`].
    pub(crate) session: Option<usize>,
    /// The number `/switch N` takes to reach this row.
    pub(crate) number: Option<usize>,
    /// What Enter does on it, performed by the head.
    pub(crate) enter: Option<RowAct>,
    /// Whether it folds, and which way it stands now.
    pub(crate) open: Option<Fold>,
    /// The column this row is DRAWN at - what `←` and `→` walk by, so a key that steps out of a
    /// subtree lands where the eye already is.
    pub(crate) depth: usize,
}

impl Listed {
    /// Whether this row's own rows are drawn under it now.
    pub(crate) fn is_open(&self) -> bool {
        matches!(self.open, Some(Fold::Open))
    }

    /// Whether a fold would put rows under it - a closed row, and not a leaf or a `fixed` one.
    pub(crate) fn folds_open(&self) -> bool {
        matches!(self.open, Some(Fold::Closed))
    }
}

/// The list, as drawn: the rows the keys act on, and the lines already rendered.
#[derive(Debug, Clone, Default)]
pub(crate) struct List {
    pub(crate) rows: Vec<Listed>,
    pub(crate) content: PaneLines,
}

impl List {
    /// The row the cursor is on, or `None` for a list with no stops in it.
    pub(crate) fn selected(&self, sel: usize) -> Option<&Listed> {
        if self.rows.is_empty() {
            return None;
        }
        self.rows.get(sel.min(self.rows.len() - 1))
    }
}

/// The head's stack for its panes, built once and kept: discovery reads the disk, and a list
/// drawn every frame cannot afford it. One stack serves every hook - the cache, the state and the
/// acts are keyed by hook, because two panes sharing one `state` would swap answers.
pub(crate) struct Host {
    stack: Stack,
    /// The last answer, by the input that asked for it. The list is drawn once per frame and read
    /// several times within it (the bar, the click map, the keys), so this is the difference
    /// between a pane that costs microseconds and one that costs a compile.
    cache: Mutex<Cached>,
    /// What each hook's layer answered with last time, handed back to it (state
    /// belongs to the host). Keyed by hook, so the queue pane's answer and the session list's
    /// cannot overwrite each other.
    state: Mutex<std::collections::BTreeMap<String, Value>>,
    /// Which word links where, as the `link_` layers last answered: `None` is a word they were
    /// asked about and did not know, which is cached too - a frame asks once per word, not once
    /// per frame (`docs/plugins-plan.md` \u{00a7}4.6).
    links: Mutex<std::collections::BTreeMap<String, Option<String>>>,
    /// The `card_` answer for each row, by `hook`, row id and the hash of what the layer was asked
    /// about: a card is asked for ONCE and drawn on every frame after it, so a layer sees a row once
    /// per head rather than once per frame (X22) - and an edited row, which is the same id with other
    /// words, is asked about again. Bounded, and evicted least-recently-used: [`CardCache`].
    /// A `None` is a row the layer drew nothing for, which is also not worth asking twice.
    cards: Mutex<CardCache>,
    /// How many asks reached the layer: a miss is one call, so this is what *asked once per row*
    /// means, and a test reads it rather than believing the cache.
    #[cfg(test)]
    asked: std::sync::atomic::AtomicUsize,
    /// The same count for `pane_` answers, which is the measure of an idle frame.
    #[cfg(test)]
    list_asked: std::sync::atomic::AtomicUsize,
    /// What a layer owes the person, said once each: a failure, a trap, or an answer this head will
    /// not draw. Fail down LOUDLY (`docs/plugins-plan.md`): the list the person sees may be the
    /// built-in's or the head's own, and either way the reason their layer is not drawing it is news.
    refusals: Refusals,
    /// The folder this host was built for; a head that moves to another one gets another stack.
    workspace: PathBuf,
    /// When this host next looks for a rebuilt layer, on the head's clock. The daemon's
    /// `plugin load` cannot refresh a pane the HEAD draws, so the head watches its own scopes with
    /// `Stack::poll_changes` and a rebuild redraws with no restart. See `Host::reload_if_due`.
    next_poll: u64,
}

/// How often the head looks for a rebuilt layer - the daemon's own `POLL_EVERY`, for the same
/// reason: a stat per layer is free, and two seconds is the longest a person waits after saving.
const POLL_MS: u64 = 2_000;

#[derive(Default)]
struct Cached {
    /// `"<hook>\n<the input's JSON>"` - the hook is part of the key because one host answers two
    /// panes, and two panes' inputs can be byte-identical while their answers are not. Nothing that
    /// moves on its own belongs in it: the clock left it with the age buckets (see
    /// `App::sessions_feed`), so an idle frame asks once and not once per frame.
    key: String,
    answer: Option<Value>,
    /// The size the last ask was made for, so a test can prove the pane is asked about the height
    /// it is drawn at rather than a number this file made up.
    #[cfg(test)]
    size: Option<(u32, u32)>,
}

/// What one call came back with. The failure a layer met is not here: [`Host::ask`] keeps it for
/// [`App::take_sessions_failure`], because the one that has to say it is a caller with a screen.
pub(crate) struct Answered {
    pub(crate) view: Option<pv::View>,
    /// The answer as the layer wrote it: a `command_` or `complete_` hook's own shape, which is
    /// not a view.
    pub(crate) answer: Option<Value>,
    /// The fuel the answering layer's module burned on this call - `None` when nothing answered,
    /// or when the answer came from the cache and nothing was measured.
    pub(crate) fuel: Option<u64>,
}

impl Host {
    /// One `command_` hook through this host's stack, so the layer approved for the pane is the
    /// one that answers the command; the hook is built from the verb (`todo` -> `command_todo`).
    /// The answer is raw - the CALLER validates an `act` - and a failure is kept for its screen.
    pub(crate) fn command(
        &self,
        verb: &str,
        input: &serde_json::Value,
    ) -> letibot_plugins::Resolved {
        let hook = format!("command_{verb}");
        let resolved = self.stack.call(&hook, input);
        if let Some(failure) = resolved.failures.first() {
            self.refuse(
                failure,
                format!(
                    "a plugin layer could not answer `/{verb}`, so it was answered from below it: \
                     {failure}"
                ),
            );
        }
        resolved
    }

    /// Queue a layer's failure with the sentence this caller wants read. The identity is the
    /// failure's own - layer, hook, reason - so a pane asked on every frame says this once.
    fn refuse(&self, failure: &letibot_plugins::Failure, said: String) {
        self.refusals.push(Refusal::of(failure), said);
    }

    /// What a layer owes the person, without taking it - the queue pane's refusal sentence reads it
    /// on every draw, and taking it would eat the notice the picker owes its reader.
    pub(crate) fn peek_problem(&self) -> Option<String> {
        self.refusals.peek()
    }

    /// One `link_` question for a frame: the words no layer has answered yet, asked of each
    /// `link_` hook in stack order with the still-unknown words only, and cached by word. The
    /// answer is what a head may draw - a layer that traps or answers a shape the contract refuses
    /// is said once and leaves the words as text, which is what they were before it existed.
    pub(crate) fn links(
        &self,
        words: &[String],
        repo: &letibot_plugin::link::Repo,
    ) -> std::collections::BTreeMap<String, Option<String>> {
        let mut cached = self.links.lock().map(|l| l.clone()).unwrap_or_default();
        let mut unknown: Vec<String> = Vec::new();
        for word in words {
            if !cached.contains_key(word) && !unknown.contains(word) {
                unknown.push(word.clone());
            }
        }
        for hook in self.link_hooks() {
            if unknown.is_empty() {
                break;
            }
            unknown.truncate(letibot_plugin::link::MAX_WORDS);
            let input = letibot_plugin::link::Input::new(unknown.clone(), repo.clone());
            let Ok(input) = serde_json::to_value(&input) else {
                break;
            };
            let resolved = self.stack.call(&hook, &input);
            if let Some(failure) = resolved.failures.first() {
                self.refuse(
                    failure,
                    format!(
                        "a plugin layer could not answer `{hook}`, so the words it would have \
                         linked are text: {failure}"
                    ),
                );
            }
            let Some(answer) = resolved.answer else {
                continue;
            };
            let by = resolved.by.clone().unwrap_or_default();
            let urls = match serde_json::from_value::<letibot_plugin::link::Out>(answer.clone()) {
                Ok(out) => match out.checked(&unknown) {
                    Ok(urls) => urls,
                    Err(why) => {
                        let said = format!(
                            "the plugin layer `{by}` answered `{hook}` with something a head will \
                             not draw: {why}"
                        );
                        self.refusals
                            .push(Refusal::new(&by, Some(&hook), why), said);
                        continue;
                    }
                },
                Err(why) => {
                    let said = format!(
                        "the plugin layer `{by}` answered `{hook}` with something that is not a \
                         link answer: {why}"
                    );
                    self.refusals
                        .push(Refusal::new(&by, Some(&hook), why.to_string()), said);
                    continue;
                }
            };
            for (word, url) in unknown.iter().zip(urls) {
                cached.insert(word.clone(), url);
            }
            // Only what this hook did not answer is left for the layer below it.
            unknown.retain(|w| !cached.contains_key(w));
        }
        if let Ok(mut known) = self.links.lock()
            && !cached.is_empty()
        {
            *known = cached.clone();
        }
        cached
    }

    /// The `link_<name>` hooks this stack exports, topmost layer first: a kind whose name is the
    /// layer's own, so there is no single export to ask.
    fn link_hooks(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for layer in self.stack.layers() {
            if layer.disabled || layer.manifest.contract != letibot_plugins::CONTRACT {
                continue;
            }
            for hook in &layer.manifest.hooks {
                if hook.starts_with(letibot_plugin::link::PREFIX)
                    && letibot_plugins::is_hook_kind(hook)
                    && !out.contains(hook)
                {
                    out.push(hook.clone());
                }
            }
        }
        out
    }

    /// Does any layer on disk answer `link_`? The head reads manifests and no modules, so a session
    /// with no such layer keeps the head it had instead of compiling a stack on its first frame.
    pub(crate) fn any_link_layer(session: &str, workspace: &Path) -> bool {
        Self::any_layer_answering(session, workspace, letibot_plugin::link::PREFIX)
    }

    /// Does any layer on disk answer `card_`? The same question as `any_link_layer`'s and for the
    /// same reason: the head's own readers draw what the built-in layer draws, so only a layer a
    /// person wrote is worth compiling a stack on the first frame a row is drawn.
    pub(crate) fn any_card_layer(session: &str, workspace: &Path) -> bool {
        Self::any_layer_answering(session, workspace, letibot_plugin::card::PREFIX)
    }

    /// Whether a placed layer's manifest names a hook under `prefix` - no module is loaded to ask.
    fn any_layer_answering(session: &str, workspace: &Path, prefix: &str) -> bool {
        let roots = [
            session_plugins_dir(session),
            Some(letibot_plugins::paths::project_plugin_dir(workspace)),
            letibot_plugins::paths::user_plugin_dir(),
        ];
        roots.iter().flatten().any(|root| {
            let Ok(entries) = std::fs::read_dir(root) else {
                return false;
            };
            entries.flatten().any(|e| {
                let dir = e.path();
                let Ok(manifest) = letibot_plugins::Manifest::read(&dir) else {
                    return false;
                };
                manifest.hooks.iter().any(|h| h.starts_with(prefix))
            })
        })
    }

    /// The failures owed, taken - the seam [`App::take_sessions_failure`] reads through, and the one
    /// a test uses to see that a broken layer was SEEN rather than skipped.
    #[cfg(test)]
    fn take_problem(&self) -> Option<String> {
        let owed = self.refusals.drain();
        (!owed.is_empty()).then(|| owed.join(" · "))
    }

    /// A host with exactly this stack - what a test stands a broken one in with. The stack's own
    /// refusals come with it, because a layer that would not load is news the person is owed and
    /// this host is the last place that can say it.
    #[cfg(test)]
    pub(crate) fn with_stack(stack: Stack, workspace: PathBuf) -> Host {
        let refusals = Refusals::new();
        if let Some(said) = would_not_load(&stack) {
            refusals.push_said(said);
        }
        Host {
            stack,
            cache: Mutex::new(Cached::default()),
            state: Mutex::new(std::collections::BTreeMap::new()),
            links: Mutex::new(std::collections::BTreeMap::new()),
            cards: Mutex::new(CardCache::default()),
            #[cfg(test)]
            asked: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            list_asked: std::sync::atomic::AtomicUsize::new(0),
            refusals,
            workspace,
            next_poll: 0,
        }
    }

    /// A host for this session's folder, with the binary's own layers at the bottom. One door for
    /// the head, and crate-visible because a test stands the real build in for a folder it cannot
    /// create. Admission is `letibot_plugins::admit`'s - the daemon's and the bridge's path too -
    /// and the trust book's pin is the head's to TAKE: the daemon's stack reads the same file at
    /// its next poll, so one answer is in force in both processes.
    pub(crate) fn build(session: &str, workspace: &Path) -> Host {
        let session_dir = session_plugins_dir(session);
        let admitted = letibot_plugins::admit(
            letibot_plugins::Scopes {
                session: session_dir.as_deref(),
                workspace,
                user: user_config_dir().as_deref(),
            },
            Arc::new(RanoLoader),
            Some(letibot_plugins::Trust::open_default()),
        );
        let mut broke = admitted.sentences();
        let mut stack = admitted.stack;
        // The built-in scope at the bottom of the stack, and a failure here is SAYABLE: the head
        // still falls back to its own list, but a binary whose own layer is broken is a defect and
        // the person is owed the sentence. Recorded once, at build time - it is never asked.
        if let Some(builtin) = install_builtins(&mut stack) {
            broke.push(builtin);
        }
        // Admission's news has no failure record this host can key on, so each sentence is its own
        // identity: the same layers failing the same way on a rebuild is the same news.
        let refusals = Refusals::new();
        for said in broke {
            refusals.push_said(said);
        }
        Host {
            stack,
            cache: Mutex::new(Cached::default()),
            state: Mutex::new(std::collections::BTreeMap::new()),
            links: Mutex::new(std::collections::BTreeMap::new()),
            cards: Mutex::new(CardCache::default()),
            #[cfg(test)]
            asked: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            list_asked: std::sync::atomic::AtomicUsize::new(0),
            refusals,
            workspace: workspace.to_path_buf(),
            next_poll: 0,
        }
    }

    /// Pick up a layer that was rebuilt on disk, at most every `POLL_MS` - the live-iteration loop
    /// for a pane the head draws. No shadow gate: the head gates nothing it discovers, and the
    /// gate a change must pass is the daemon's `plugin load`. The cache goes with the reload.
    fn reload_if_due(&mut self, now_ms: u64) {
        if now_ms != 0 && now_ms < self.next_poll {
            return;
        }
        self.next_poll = now_ms.saturating_add(POLL_MS);
        // The book before the modules: a pin taken by another process (or one whose bytes
        // moved) decides a layer without any module having changed, and a pane drawn from a layer
        // that stopped being pinned should say so on this head's own clock rather than a daemon's.
        self.stack.refresh_trust();
        let candidates = self.stack.poll_changes();
        if candidates.is_empty() {
            return;
        }
        let mut applied = false;
        for cand in candidates {
            if self.stack.apply(cand).is_ok() {
                applied = true;
            }
        }
        if applied {
            if let Ok(mut cache) = self.cache.lock() {
                cache.key.clear();
                cache.answer = None;
            }
            // A rebuilt layer is a NEW layer answering: whatever `state` the old one left behind
            // was its own, and handing it to a different module is handing it an answer to a
            // question it never asked. The words it links are asked again for the same reason.
            if let Ok(mut state) = self.state.lock() {
                state.clear();
            }
            if let Ok(mut links) = self.links.lock() {
                links.clear();
            }
            // And every card is drawn again: a card is kept by row id and the question asked, and
            // the row's answer was the OLD module's.
            if let Ok(mut cards) = self.cards.lock() {
                *cards = CardCache::default();
            }
        }
    }

    /// The layers a person could pin, with what the book says about each - `/plugin`'s listing and
    /// the answer `/plugin trust NAME` reports. A built-in is not in it: a list of things that need
    /// no decision hides the ones that do.
    pub(crate) fn placed(&self) -> Vec<letibot_plugins::Layer> {
        self.stack.placed().into_iter().cloned().collect()
    }

    /// Pin a layer's bytes as they are now. The pin is taken through the head's own book, so the
    /// file is written once and the daemon reads it, and this stack is re-decided immediately so
    /// the pane does not wait for a poll to agree.
    pub(crate) fn pin(&mut self, name: &str) -> Result<(letibot_plugins::Pin, String), String> {
        let layer = self.stack.layer_named(name).cloned().ok_or_else(|| {
            let names: Vec<String> = self
                .stack
                .placed()
                .iter()
                .map(|l| l.manifest.name.clone())
                .collect();
            format!(
                "no placed layer `{name}` — this session's stack has [{}]",
                names.join(", ")
            )
        })?;
        if layer.scope == letibot_plugins::Scope::BuiltIn {
            return Err(format!(
                "`{name}` is a built-in — it is part of this binary, and D17 runs the built-ins \
                 freely: there is nothing to pin"
            ));
        }
        let trust = self.stack.trust().cloned().ok_or_else(|| {
            "this head has no trust book open, so nothing could be recorded".to_string()
        })?;
        let pin = trust.pin(&layer.manifest.name, &layer.dir)?;
        self.stack.refresh_trust();
        // Who wrote them, when a tool did - the third arm, reported beside the hash so the
        // person pinning knows whether these bytes are the model's or their own.
        let wrote = trust
            .writes_for(&layer.dir)
            .into_iter()
            .map(|w| format!("{} (session {})", w.path, w.session))
            .collect::<Vec<_>>();
        Ok((pin, wrote.join("; ")))
    }

    /// Take a pin back - `/plugin untrust NAME`.
    pub(crate) fn unpin(&mut self, name: &str) -> Result<letibot_plugins::Pin, String> {
        let trust = self
            .stack
            .trust()
            .cloned()
            .ok_or_else(|| "this head has no trust book open".to_string())?;
        let pin = trust
            .unpin(name)
            .or_else(|| {
                self.stack
                    .layer_named(name)
                    .and_then(|l| trust.unpin(&l.dir.display().to_string()))
            })
            .ok_or_else(|| format!("nothing is pinned as `{name}`"))?;
        self.stack.refresh_trust();
        Ok(pin)
    }

    /// The layer a pane's hook would ask. A head with a `queue` layer that does not read the queue
    /// feed is not that pane's: it may answer, but it would draw rows from data it never asked
    /// for, so the hook and the feed are one predicate.
    pub(crate) fn reader(&self, hook: &str, feed: Feed) -> Option<&letibot_plugins::Layer> {
        self.stack.layers().iter().find(|l| {
            !l.disabled
                && l.manifest.contract == letibot_plugins::CONTRACT
                && l.manifest.hooks.iter().any(|h| h == hook)
                && l.manifest.feeds.contains(&feed)
        })
    }

    /// The acts the layer answering `hook` declared - what a view may ask the head to perform.
    pub(crate) fn declared(&self, hook: &str, feed: Feed) -> Vec<String> {
        self.reader(hook, feed)
            .map(|l| l.manifest.actions.clone())
            .unwrap_or_default()
    }

    /// Ask `hook`, one call per distinct (hook, input). The failure sentence names the pane the
    /// caller is drawing, because one host answers two: the wrong name sends the reader to the
    /// wrong place.
    pub(crate) fn ask(&self, hook: &str, pane: &str, input: &Input) -> Answered {
        let key = format!(
            "{hook}\n{}",
            serde_json::to_string(input).unwrap_or_default()
        );
        if let Ok(cache) = self.cache.lock()
            && cache.key == key
        {
            return Answered {
                view: cache
                    .answer
                    .as_ref()
                    .and_then(|a| serde_json::from_value(a.clone()).ok()),
                answer: cache.answer.clone(),
                // A cache hit measured nothing: a caller that wants the fuel asks [`Host::call`],
                // which is uncached for exactly that reason.
                fuel: None,
            };
        }
        #[cfg(test)]
        self.list_asked
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let value = serde_json::to_value(input).unwrap_or(Value::Null);
        let resolved = self.stack.call(hook, &value);
        // Fail down, loudly: a layer that traps is skipped by the stack and
        // the one below answers; the sentence is kept for the person whose layer it was.
        let view = resolved
            .answer
            .as_ref()
            .and_then(|a| serde_json::from_value::<pv::View>(a.clone()).ok());
        if let Ok(mut cache) = self.cache.lock() {
            cache.key = key;
            cache.answer = resolved.answer.clone();
            #[cfg(test)]
            {
                cache.size = Some((input.size.w, input.size.h));
            }
        }
        if let Some(failure) = resolved.failures.first() {
            self.refuse(
                failure,
                format!(
                    "a plugin layer could not draw {pane}, so it is drawn from below it: {failure}"
                ),
            );
        }
        Answered {
            view,
            answer: resolved.answer.clone(),
            fuel: resolved.by.as_deref().and_then(|id| self.stack.fuel(id)),
        }
    }

    /// One hook call through this host's stack, UNCACHED, with the fuel the answering module burned.
    /// A pane whose feed changes at its own pace (the chat, asked once per generation) or a hook
    /// that answers a keystroke (`complete_`) would spend the one-slot cache above on every frame.
    pub(crate) fn call(&self, hook: &str, pane: &str, input: &Value) -> Answered {
        let resolved = self.stack.call(hook, input);
        let view = resolved
            .answer
            .as_ref()
            .and_then(|a| serde_json::from_value::<pv::View>(a.clone()).ok());
        if let Some(failure) = resolved.failures.first() {
            self.refuse(
                failure,
                format!(
                    "a plugin layer could not draw {pane}, so it is drawn from below it: {failure}"
                ),
            );
        }
        Answered {
            view,
            fuel: resolved.by.as_deref().and_then(|id| self.stack.fuel(id)),
            answer: resolved.answer,
        }
    }

    /// The `card_` answer for one row, asked at most once per thing it was asked about: the key is
    /// the hook, the row's id and the hash of `input`, so a row drawn again the same way costs one
    /// call however many frames draw it (X22), and a row that was edited - the same id with other
    /// words - is asked about again rather than drawn from a card about what it no longer is.
    ///
    /// The cache is bounded and evicts the least recently used ([`CardCache`]): a long session holds
    /// more rows than a map should, and asking for one again is one call per row.
    pub(crate) fn card_answer(&self, hook: &str, id: &str, input: Value) -> Option<Value> {
        let key = CardCache::key(hook, id, &input);
        if let Some(known) = self.cards.lock().ok().and_then(|mut c| c.get(&key)) {
            return known;
        }
        #[cfg(test)]
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let resolved = self.stack.call(hook, &input);
        if let Some(failure) = resolved.failures.first() {
            self.refuse(
                failure,
                format!(
                    "a plugin layer could not draw this row's card, so the head's own readers did: \
                     {failure}"
                ),
            );
        }
        if let Ok(mut cards) = self.cards.lock() {
            cards.insert(key, resolved.answer.clone());
        }
        resolved.answer
    }

    /// How many asks reached the layer - a test's measure of *once per row*.
    #[cfg(test)]
    pub(crate) fn cards_asked(&self) -> usize {
        self.asked.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many `pane_` asks were NOT served from the cache: the measure of *an idle frame does not
    /// re-run the guest*.
    #[cfg(test)]
    pub(crate) fn list_asks(&self) -> usize {
        self.list_asked.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The size the last `pane_` ask was made for: `(w, h)`.
    #[cfg(test)]
    pub(crate) fn asked_size(&self) -> Option<(u32, u32)> {
        self.cache.lock().ok().and_then(|c| c.size)
    }

    /// Say why a row was drawn the head's way rather than a layer's: queued under the hook it
    /// answered, so a frame that draws the row again does not say it again.
    pub(crate) fn complain(&self, hook: &str, why: String) {
        self.refusals
            .push(Refusal::new("", Some(hook), why.clone()), why);
    }

    /// The state a hook's layer answered with last time - per hook, because two panes share
    /// this host and handing one pane's `state` back to the other would be handing it an answer to
    /// a question it never asked.
    pub(crate) fn state_of(&self, hook: &str) -> Option<Value> {
        self.state.lock().ok().and_then(|s| s.get(hook).cloned())
    }

    /// Remember what a hook's layer answered with, for its next call.
    pub(crate) fn remember(&self, hook: &str, state: Option<Value>) {
        if let Ok(mut slot) = self.state.lock() {
            match state {
                Some(v) => {
                    slot.insert(hook.to_string(), v);
                }
                None => {
                    slot.remove(hook);
                }
            }
        }
    }
}

/// Put the binary's own layers at the bottom of `stack`, and answer one sentence naming the layer
/// and the reason when one will not go; `None` when the whole table installed. The sentence names
/// no particular question, because it now reaches the session list and a slash command both.
fn install_builtins(stack: &mut Stack) -> Option<String> {
    let failed = letibot_builtins::install(stack);
    if failed.is_empty() {
        return None;
    }
    Some(format!(
        "this binary's built-in plugin layer could not be loaded — {} — so whatever it answers is \
         answered by the head itself",
        failed.join("; ")
    ))
}

/// The layers that never got as far as a call, in one sentence. A manifest this host refuses
/// at load has no call to fail in, so the host that built the stack is the only one that can say
/// it. A built-in is left out: [`install_builtins`] already said that one. `admit` says this for
/// the host the head builds; this is the door a test's hand-made stack comes through.
#[cfg(test)]
fn would_not_load(stack: &Stack) -> Option<String> {
    let said: Vec<String> = stack
        .failures()
        .iter()
        .filter(|f| f.scope != letibot_plugins::Scope::BuiltIn)
        .map(|f| format!("the plugin layer {f}"))
        .collect();
    (!said.is_empty()).then(|| said.join("; "))
}

/// The session's own plugin directory, where a session-scope layer lives - under the data
/// directory the daemon resolves, so the two processes read one scope.
fn session_plugins_dir(session: &str) -> Option<PathBuf> {
    let data = letibot_plugins::paths::data_dir()?;
    Some(letibot_plugins::paths::session_plugin_dir(&data, session))
}

/// `$XDG_CONFIG_HOME/letibot`, whose `plugins/` is the user scope - from the one module that works
/// this out, because a head and a daemon that disagree about it read two different stacks.
fn user_config_dir() -> Option<PathBuf> {
    letibot_plugins::paths::config_dir()
}

/// A stack being built on a thread of its own. Discovery reads three directories and the binary's
/// own thirteen built-ins compile, which is seconds on the drawing thread and a freeze on the first
/// picture; the frame that asks for a host draws the head's own list and a later frame draws the
/// layer's, when this has something on it.
pub(crate) struct HostBuild {
    /// The folder the build is for. A head that has moved on since discards the answer.
    pub(crate) workspace: PathBuf,
    pub(crate) settled: std::sync::mpsc::Receiver<Host>,
}

impl App {
    /// The head's plugin stack, for this session's folder - one door, so the picker and a slash
    /// command ask the SAME stack, and a layer installed is a layer asked. A head that moves to
    /// another folder gets another stack: layers belong to a project.
    ///
    /// A stack the head has not got yet is built OFF the drawing thread: the answer is `None` for
    /// the frames it takes, which is the head's own list, and the layer's answer arrives with the
    /// next frame after the thread is done. See [`HostBuild`].
    pub(crate) fn plugin_host(&self) -> std::cell::Ref<'_, Option<Host>> {
        self.collect_host_build();
        let workspace = self.workspace_path();
        let stale = self
            .sessions_host
            .borrow()
            .as_ref()
            .map(|h| h.workspace != workspace)
            .unwrap_or(true);
        if stale {
            // A host for another folder is not this folder's, and a build for one is left to land
            // and be dropped: its workspace is in the answer, so nothing stale can be seated.
            *self.sessions_host.borrow_mut() = None;
            // A test has no second frame, so it asks for the stack where it stands. A head never
            // does: the frame that waited on thirteen compiles is the freeze this is here to end.
            #[cfg(test)]
            {
                if self.host_inline.get() {
                    *self.sessions_host.borrow_mut() =
                        Some(Host::build(&self.session_id, &workspace));
                    return self.sessions_host.borrow();
                }
            }
            let building = self
                .host_build
                .borrow()
                .as_ref()
                .is_some_and(|b| b.workspace == workspace);
            if !building && self.host_build.borrow().is_none() {
                self.start_host_build(&workspace);
            }
        } else if let Some(host) = self.sessions_host.borrow_mut().as_mut() {
            // A layer rebuilt on disk is a pane that redraws - the live loop, and the
            // reason this pane is a plugin at all. See [`Host::reload_if_due`].
            host.reload_if_due(self.now_ms);
        }
        self.sessions_host.borrow()
    }

    /// Start this folder's stack on a thread of its own. A thread this process cannot start is not
    /// a reason to lose the layer: the frame builds it where it stands and pays what the thread
    /// would have taken.
    fn start_host_build(&self, workspace: &Path) {
        let session = self.session_id.clone();
        let folder = workspace.to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("plugin-host".into())
            .spawn(move || {
                let _ = tx.send(Host::build(&session, &folder));
            });
        match spawned {
            Ok(_) => {
                *self.host_build.borrow_mut() = Some(HostBuild {
                    workspace: workspace.to_path_buf(),
                    settled: rx,
                });
            }
            Err(_) => {
                *self.sessions_host.borrow_mut() = Some(Host::build(&self.session_id, workspace));
            }
        }
    }

    /// Take the stack a thread finished, if it has: one whose folder is still this head's becomes
    /// the host, and one built for a folder the head has left is dropped. A thread that died
    /// without answering lets the build go, so the next frame asks again rather than leaving the
    /// pane on the head's list for the rest of the session.
    fn collect_host_build(&self) {
        let seen = {
            let held = self.host_build.borrow();
            match held.as_ref().map(|b| b.settled.try_recv()) {
                None | Some(Err(std::sync::mpsc::TryRecvError::Empty)) => return,
                Some(Ok(host)) => Some(host),
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => None,
            }
        };
        match seen {
            Some(host) => self.seat_host_build(host),
            None => {
                self.host_build.borrow_mut().take();
            }
        }
    }

    /// Seat a finished build, and let go of the build: the two are one act, because a stack whose
    /// folder the head has left is dropped here rather than seated as this one's.
    fn seat_host_build(&self, host: Host) {
        let built = self.host_build.borrow_mut().take();
        if built.is_some_and(|b| b.workspace == host.workspace) {
            *self.sessions_host.borrow_mut() = Some(host);
        }
    }

    /// Wait for the build in flight, and seat it. A test's door: the drawing path never waits, and
    /// a test that has to see what the layer draws cannot spin frames until a compile finishes.
    #[cfg(test)]
    pub(crate) fn wait_for_host_build(&self, timeout: std::time::Duration) -> bool {
        let arrived = self
            .host_build
            .borrow()
            .as_ref()
            .and_then(|b| b.settled.recv_timeout(timeout).ok());
        let Some(host) = arrived else {
            return false;
        };
        self.seat_host_build(host);
        true
    }

    /// The session list, from the plugin - `None` when there is no layer to ask, or when its
    /// answer is not a list this head will draw (and that is SAID, because the pane no longer has a
    /// second drawing to hide behind).
    pub(crate) fn sessions_list(&self, w: usize, h: usize) -> Option<List> {
        let slot = self.plugin_host();
        let host = slot.as_ref()?;
        host.reader(HOOK, Feed::Sessions)?;
        let declared = host.declared(HOOK, Feed::Sessions);
        let state = host.state_of(HOOK);
        let input = self.sessions_input(state, w, h);
        let answered = host.ask(HOOK, "the session list", &input);
        if let Some(answer) = answered.view.as_ref() {
            host.remember(HOOK, answer.state.clone());
        }
        let view = answered.view?;
        // The host's rules, and the acts the layer declared (C3).
        if let Err(why) = view.validate_for(&declared) {
            host.complain(
                HOOK,
                format!("the session list's layer answered a view this head will not draw: {why}"),
            );
            return None;
        }
        // A view with no tree is valid as a document and still not a list this pane can draw.
        let Some((mut rows, empty)) = tree_of(&view, &self.picker_folds, self.picker_sel) else {
            host.complain(
                HOOK,
                "the session list's layer answered a view this head will not draw: it holds no \
                 tree block"
                    .to_string(),
            );
            return None;
        };
        let width = w.max(20);
        fit_facts(&mut rows, width);
        // Where the cursor stands in the list, in the title: the window is drawn around the cursor,
        // so a page key has nowhere else to say how far down the list a reader has got.
        let stops = rows.iter().filter(|r| r.id.is_some()).count();
        let title = match stops {
            0 => sdk::TITLE.to_string(),
            n => sdk::counter(self.picker_sel.min(n - 1) + 1, n),
        };
        let list = TreeList {
            title,
            rows,
            selected: Some(self.picker_sel),
            empty,
            fold_column: true,
            // The full id sits at column 6, under the mark and the fold column.
            detail_column: 6,
            margin: 2,
            hint: sdk::HINT.iter().map(|h| (*h).to_string()).collect(),
            notes: vec![sdk::NOTE.to_string()],
        };
        Some(List {
            rows: listed(&view, self),
            content: list.content(width),
        })
    }

    /// Everything a layer owes the person, taken: one sentence per distinct refusal, joined into the
    /// one line the head has for it. A caller with a screen says it at the moment it happened; the
    /// frame itself says whatever is left, so a refusal met while drawing is not a silence.
    pub(crate) fn take_sessions_failure(&mut self) -> Option<String> {
        let owed = self.sessions_host.get_mut().as_ref()?.refusals.drain();
        (!owed.is_empty()).then(|| owed.join(" · "))
    }

    /// The same, onto the notice line - the door a frame uses, because a refusal kept for a pane
    /// nobody opened is a refusal the person never read.
    pub(crate) fn say_refusals(&mut self) {
        if let Some(said) = self.take_sessions_failure() {
            self.say(&said);
        }
    }

    /// The folder a session's layers are found under - the one it is wired to, or this process's
    /// own directory when the daemon has not said yet. Public in the crate because a test stands
    /// a host in for this one, and has to build it for the folder the head will ask about.
    pub(crate) fn workspace_path(&self) -> PathBuf {
        match self.sessions.first().map(|s| s.wiring.workspace.clone()) {
            Some(w) if !w.is_empty() => PathBuf::from(w),
            _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    /// Take the fleet reader the binary supplies.
    pub fn set_fleet_reader(&mut self, reader: letibot_sessionlog::FleetReader) {
        self.fleet_reader = Some(reader);
    }

    /// Read the other daemons' packages in the background while the picker is open, never from a
    /// paint: a reading connects to sockets. One at a time, [`FLEET_REFRESH_MS`] apart.
    pub fn refresh_fleet(&mut self) {
        if let Some(rx) = &self.fleet_job {
            match rx.try_recv() {
                Ok(read) => {
                    self.fleet = read.rows;
                    self.fleet_mine = read.mine;
                    self.rooms_sync_sessions();
                    self.fleet_unlisted = read.unlisted;
                    self.fleet_job = None;
                    self.fleet_read = self.now_ms;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.fleet_job = None;
                    self.fleet_read = self.now_ms;
                }
            }
            return;
        }
        let Some(reader) = self.fleet_reader.clone() else {
            return;
        };
        if !(self.is_open(Surface::Picker) || self.sidebar.on || self.is_open(Surface::Rooms))
            || self.now_ms.saturating_sub(self.fleet_read) < FLEET_REFRESH_MS
                && self.fleet_read != 0
        {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        if std::thread::Builder::new()
            .name("fleet".into())
            .spawn(move || {
                let _ = tx.send(reader());
            })
            .is_ok()
        {
            self.fleet_job = Some(rx);
        } else {
            self.fleet_read = self.now_ms;
        }
    }

    /// What the pane is asked with - the one feed it declared, the fold state, and the size it is
    /// DRAWN at. A layer is handed the height its own rectangle gives it, not a number this file
    /// chose: a pane that drew to a made-up height drew to somebody else's screen.
    fn sessions_input(&self, state: Option<Value>, w: usize, h: usize) -> Input {
        let mut feeds = BTreeMap::new();
        feeds.insert(Feed::Sessions.name().to_string(), self.sessions_feed());
        Input {
            v: letibot_plugin::pane::V,
            state,
            look: None,
            event: self.sessions_event.borrow().clone(),
            size: letibot_plugin::pane::Size {
                w: w as u32,
                h: h as u32,
            },
            feeds,
            folded: self.picker_folds.clone(),
            below: None,
            ..Input::default()
        }
    }

    /// The daemon's session list as the feed spells it, one object per session. `project` and
    /// `system` are computed HERE: they are facts about the whole list, and a guest that
    /// recomputed them would be a second opinion about where a row goes. The layer says what.
    ///
    /// **No clock.** An age is bucketed here (`sdk::age_word`) and the bucket is what a row gets.
    /// A raw `now` made every frame a different input, so the one-slot cache never hit and the
    /// guest was re-run on every idle frame.
    pub(crate) fn sessions_feed(&self) -> Value {
        let place = self.placements();
        let projects = self.projects(&place);
        let now = letibot_sessionlog::registry::now_ms();
        let name_of = |key: &str| {
            projects
                .iter()
                .find(|p| p.key == key)
                .map(|p| p.name.clone())
                .unwrap_or_default()
        };
        let kept: Vec<(usize, &_)> = self
            .sessions
            .iter()
            .enumerate()
            // A removed session is not in the feed at all, so the layer's rows, its counts and
            // its numbering agree with the head's own walk by construction: one predicate
            // (`App::removed_chain`), one drawing.
            .filter(|(i, _)| !self.removed_chain(*i))
            .collect();
        // Capped before the rows are built: the feed's cost to a guest is its size. Whatever the
        // cap leaves out is COUNTED and said, because a list that stops at 250 and says nothing
        // reads as a daemon that holds 250.
        let total = kept.len();
        let kept = letibot_plugin::pane::cap_session_rows(kept, |(_, s)| {
            (
                s.live || s.status.running || s.status.heads > 0 || s.session_id == self.session_id,
                s.moved_ms,
            )
        });
        let more = total.saturating_sub(kept.len());
        let sessions: Vec<Value> = kept
            .into_iter()
            .map(|(i, s)| {
                json!({
                    "id": s.session_id,
                    "title": s.title,
                    "slug": s.slug,
                    "parent": s.parent_session_id,
                    // The origin's wire word - the same one the store holds.
                    "origin": s.origin.as_str(),
                    "live": s.live,
                    "running": s.status.running,
                    "heads": s.status.heads,
                    "held": s.status.held_rooms,
                    "items": s.status.items,
                    "stored": s.stored_items,
                    "role": s.role,
                    "model": s.wiring.model,
                    "workspace": s.wiring.workspace,
                    "waits_on": s.waits_on_operator,
                    "nags_paused": s.nags_paused.is_some(),
                    // When it last moved, so a pane can say *is that one stuck?* - the one fact
                    // a row of a session list is read for. Off the daemon's own brief, which reads
                    // the store's row (`SessionBrief::moved_ms`).
                    "moved_ms": s.moved_ms,
                    // The same fact as one word, and a word that only moves when its bucket does:
                    // this is the field a row draws, and it is why an idle frame asks nothing.
                    "age": (s.moved_ms > 0)
                        .then(|| sdk::age_word(now.saturating_sub(s.moved_ms))),
                    // Where it sits: the project's key and the name to draw, and the `system`
                    // group when it is one of the harness's own agents.
                    "project": place[i].project,
                    "project_name": name_of(&place[i].project),
                    // Which top-level group the project is drawn in, decided here for the same
                    // reason as `project`.
                    "bucket": self.bucket_of(&place, &place[i].project, now).word(),
                    "system": place[i].system.map(|k| k.title()),
                })
            })
            .collect();
        let fleet: Vec<Value> = self
            .fleet
            .iter()
            .map(|f| {
                json!({
                    "id": f.id,
                    "name": fleet_name(f),
                    "facts": fleet_facts(f),
                    "under": fleet_under(f, &self.home),
                    "running": f.state == letibot_sessionlog::FleetState::Running,
                })
            })
            .collect();
        json!({
            "sessions": sessions,
            "fleet": fleet,
            "unlisted": (self.fleet_unlisted > 0)
                .then(|| letibot_sessionlog::fleet::unlisted_words(self.fleet_unlisted)),
            // The sessions the cap left out, for the layer's stated "N more" line.
            "more": more,
            "here": self.session_id,
            // The one home the head shortens against - the same value the layer uses, so a
            // plugin's rows and the daemon's cannot disagree about what `~` means.
            "home": self.home,
        })
    }

    /// What Enter does on a row, in the head's words - `switch` and `fold` are the head's own
    /// verbs, so the bar reads them here rather than being told them.
    pub(crate) fn list_says(&self, row: &Listed) -> Option<&'static str> {
        match &row.enter {
            None => None,
            Some(RowAct::Switch { .. }) => Some("switches"),
            Some(RowAct::Event { .. }) => Some("acts on it"),
            // A queue verb is not this list's to perform. The queue pane's keys are the ones
            // that act on an entry, and a session list row carrying `approve` is a layer reaching
            // for a key this pane does not have - said, not silently done.
            Some(RowAct::Verb { .. }) => {
                Some("asks for a queue verb, which this list has no key for")
            }
            Some(RowAct::Fold { .. }) => match row.open {
                Some(Fold::Open) => Some("folds it away"),
                Some(Fold::Closed) => Some("unfolds it"),
                // A fixed row folds nothing, so it offers no key.
                Some(Fold::Fixed) | None => None,
            },
        }
    }

    /// Perform a row's act. The three verbs C3 gives a pane, and nothing else.
    pub(crate) fn perform(&mut self, id: &str, act: &RowAct) -> Option<Action> {
        match act {
            RowAct::Switch { session } => self.switch_to(session.clone()),
            RowAct::Fold { .. } => {
                // The new state is the opposite of what is DRAWN, not of what the fold map holds: a
                // node closed by DEFAULT has no entry until somebody opens one, so reading the map
                // alone turns the first click into a second "open". Drawn knows the two exceptions.
                let open = !matches!(self.drawn_open(id), Some(Fold::Open));
                self.set_row_fold(id, open);
                None
            }
            RowAct::Event { event } => {
                *self.sessions_event.borrow_mut() = Some(letibot_plugin::pane::Event {
                    item: id.to_string(),
                    action: event.clone(),
                });
                None
            }
            // Said rather than done: the queue's verbs belong to the queue's pane, whose
            // cursor names the entry its keys act on. A session list has no such cursor, so a
            // row here naming an entry is a row asking for a key that is not on this screen.
            RowAct::Verb { verb, .. } => {
                let said = format!(
                    "`{id}` asks for `{verb}`, which is the queue pane's verb — the session list \
                     has no key for it. Nothing was sent."
                );
                self.say(&said);
                None
            }
        }
    }

    /// The list's rows, for the keys - the layer's answer, as one shape.
    pub(crate) fn picker_rows(&self) -> Vec<Listed> {
        self.picker_list(self.picker_width(), self.picker_height())
            .rows
    }

    /// The list being drawn - ONE drawing, and one door, because the drawing, the keys, the click
    /// map and `/switch N` must all be reading the same list. The size is recorded here, because
    /// the keys ask about the pane the picture was drawn in: a key that asked about another height
    /// would miss the guest's cache and re-run it on every press.
    pub(crate) fn picker_list(&self, w: usize, h: usize) -> List {
        self.picker_size.set((w, h));
        match self.sessions_list(w, h) {
            Some(list) => list,
            // Building or unanswered: one line about the LIST, never a second list of it.
            None if self.host_build.borrow().is_some() => self.saying(sdk::BUILDING, w),
            None => self.saying(sdk::UNANSWERED, w),
        }
    }

    /// The pane as one line, in the list's own frame - title, hint and note - so the pane does not
    /// jump when the layer's answer arrives.
    fn saying(&self, line: &str, w: usize) -> List {
        let tree = TreeList {
            title: sdk::TITLE.to_string(),
            rows: Vec::new(),
            selected: None,
            empty: line.to_string(),
            fold_column: true,
            detail_column: 6,
            margin: 2,
            hint: sdk::HINT.iter().map(|h| (*h).to_string()).collect(),
            notes: vec![sdk::NOTE.to_string()],
        };
        List {
            rows: Vec::new(),
            content: tree.content(w.max(20)),
        }
    }

    /// The width the list was last DRAWN at - the pane's own column, which is the sidebar's width
    /// away from the frame's. A key and the picture it moves are one list, not two renderings.
    pub(crate) fn picker_width(&self) -> usize {
        let (w, _) = self.picker_size.get();
        if w > 0 { w } else { self.cfg.width.max(20) }
    }

    /// The height the list was last DRAWN at - the pane's own rectangle, which is the height the
    /// layer is handed. The keys ask about the same one: a key that asked about another height
    /// would send the guest a different input and re-run it on every press.
    pub(crate) fn picker_height(&self) -> usize {
        let (_, h) = self.picker_size.get();
        if h > 0 { h } else { self.screen_rows.max(1) }
    }

    /// The row under the cursor, from whichever list is drawn (`picker_rows`).
    pub(crate) fn picker_row(&self) -> Option<Listed> {
        let rows = self.picker_rows();
        if rows.is_empty() {
            return None;
        }
        rows.get(self.picker_sel.min(rows.len() - 1)).cloned()
    }

    /// How a row of the drawn list stands - `Some(Fold::Open)`/`Closed` for a row that folds,
    /// `None` for one that does not. Read by [`App::perform`] so a fold flips what the screen says
    /// rather than what the map happens to hold.
    fn drawn_open(&self, id: &str) -> Option<Fold> {
        self.picker_rows()
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.open)
    }

    /// Open or close a row that folds - one door for Enter, the arrows and a click, so a fold is
    /// one act however it was asked for. The state travels back to the layer in the pane's input,
    /// which is where a fold lives: a layer remembers nothing, and the head keeps no tree.
    pub(crate) fn set_row_fold(&mut self, id: &str, open: bool) {
        self.picker_folds.insert(id.to_string(), open);
    }
}

/// Milliseconds between two readings of the other daemons.
const FLEET_REFRESH_MS: u64 = 5_000;

/// A fleet row's name, facts and faint line. The feed carries these words so the plugin draws
/// the head's sentences and cannot differ.
fn fleet_name(f: &letibot_sessionlog::FleetRow) -> String {
    if f.title.is_empty() {
        sdk::short_id(&f.id)
    } else {
        f.title.clone()
    }
}

fn fleet_facts(f: &letibot_sessionlog::FleetRow) -> String {
    let mut facts = vec![f.state.word()];
    if let Some(p) = &f.placement {
        facts.push(p.clone());
    }
    if !f.model.is_empty() {
        facts.push(f.model.clone());
    }
    facts.join(" · ")
}

fn fleet_under(f: &letibot_sessionlog::FleetRow, home: &str) -> String {
    let ws = match f.workspace.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => f.workspace.clone(),
    };
    format!("{}  {ws}", f.id)
}

/// The tree block a sessions view answers with, and its empty sentence. A row's faint detail line is
/// its identity - the id, the path, the model - and it is drawn for the row under the cursor and
/// for no other: an identity line under every session doubled the list for facts a person reads one
/// row at a time.
fn tree_of(
    view: &pv::View,
    folds: &BTreeMap<String, bool>,
    cursor: usize,
) -> Option<(Vec<TreeRow>, String)> {
    let tree = view.panels.iter().find_map(|p| {
        p.shown().iter().find_map(|b| match b {
            pv::Block::Tree(t) => Some(t),
            _ => None,
        })
    })?;
    let walked: Vec<(usize, &pv::Row)> = pv::rows_deep(&tree.rows);
    // rano's cursor is `selected.min(stops - 1)`, and the head's is clamped the same way: a row
    // folded or removed under the cursor can leave it past the end of the list.
    let stops = walked.iter().filter(|(_, r)| !r.id().is_empty()).count();
    let at = cursor.min(stops.saturating_sub(1));
    let mut rows = Vec::new();
    let mut stop = 0;
    for (base, row) in walked {
        let id = row.id().to_string();
        let picked = !id.is_empty() && stop == at;
        if !id.is_empty() {
            stop += 1;
        }
        // The host's fold state wins over the plugin's, which is what makes a fold instant: the
        // press changes this map and the next draw, and the layer remembers nothing. Three rows it
        // does not reach: a leaf, a `fixed` group, and one the map was never asked about.
        let open = match (row.open, folds.get(&id)) {
            (Some(pv::Fold::Fixed), _) => Some(Fold::Fixed),
            (Some(_), Some(true)) => Some(Fold::Open),
            (Some(_), Some(false)) => Some(Fold::Closed),
            (Some(f), None) => Some(fold_of(f)),
            (None, _) => None,
        };
        rows.push(TreeRow {
            id: (!id.is_empty()).then(|| id.clone()),
            depth: drawn_depth(base, row),
            number: row.number,
            open,
            role: row.role.map(|r| crate::ui::severity::role(Some(r))),
            cells: row.cells.iter().map(cell_of).collect(),
            facts: row.facts.iter().map(cell_of).collect(),
            // A group's line explains itself always, and there are four of those. A session's faint
            // line is its identity, drawn for the cursor's row alone: an id and a path under every row
            // doubled the list for facts read one row at a time. A group folds and takes no number.
            detail: if picked || (row.open.is_some() && row.number.is_none()) {
                row.under.clone()
            } else {
                Vec::new()
            },
            detail_column: None,
            actions: row
                .actions
                .iter()
                .filter_map(|a| {
                    a.letter()
                        .map(|c| TreeAction::new(c.to_string(), a.label.clone()))
                })
                .chain(row.enter.as_ref().and_then(|a| enter_action(a, open)))
                .collect(),
            children: Vec::new(),
        });
    }
    Some((rows, tree.empty.clone()))
}

/// Room for a row's facts. rano draws the right-hand column only when both halves fit and otherwise
/// draws the left alone (`rano::agent::pane::split_row`) - so a long title silently cost a row its
/// age, the fact the row is read for. The name gives way instead.
fn fit_facts(rows: &mut [TreeRow], w: usize) {
    // Cutting one row can leave another's arithmetic alone, so the measurement is taken again after
    // the cuts; two rounds is the common case, and four is the bound a frame will pay.
    for _ in 0..4 {
        let left = left_widths(rows);
        let mut cut = false;
        for (i, row) in rows.iter_mut().enumerate() {
            let Some(&lw) = left.get(i) else {
                continue;
            };
            let facts = facts_width(&row.facts);
            if facts == 0 {
                continue;
            }
            let over = (lw + facts + 2).saturating_sub(w);
            if over > 0 {
                cut |= shorten(&mut row.cells, over);
            }
        }
        if !cut {
            return;
        }
    }
}

/// The width the left half is MEASURED at - wider than any pane, so a name longer than the screen is
/// measured whole instead of being reported as the screen's width.
const PROBE_W: usize = 4096;

/// What rano draws on the left of each row, measured by rano: the same rows with no facts and no
/// detail line. The indent, the cursor mark, the fold column and the number field are rano's
/// arithmetic, and a second copy of it here would be a second opinion about where a name starts.
fn left_widths(rows: &[TreeRow]) -> Vec<usize> {
    let probe = TreeList {
        title: String::new(),
        rows: rows
            .iter()
            .map(|r| TreeRow {
                facts: Vec::new(),
                detail: Vec::new(),
                ..r.clone()
            })
            .collect(),
        selected: None,
        empty: String::new(),
        fold_column: true,
        detail_column: 0,
        margin: 0,
        hint: Vec::new(),
        notes: Vec::new(),
    };
    probe
        .content(PROBE_W)
        .lines
        .iter()
        .map(|l| l.width())
        .collect()
}

/// The columns a row's facts take, their ` · ` separators included.
fn facts_width(facts: &[TreeCell]) -> usize {
    let text: usize = facts.iter().map(|c| visible_width(&c.text)).sum();
    text + 2 * facts.len().saturating_sub(1)
}

/// Cut `over` columns out of a row's widest cells, so the facts keep their room. `false` when there
/// was nothing left to cut - which is how a row with no name stops the loop.
fn shorten(cells: &mut [TreeCell], over: usize) -> bool {
    let mut left = over;
    let mut cut = false;
    while left > 0 {
        let widest = cells
            .iter()
            .enumerate()
            .map(|(i, c)| (i, visible_width(&c.text)))
            .max_by_key(|&(_, width)| width);
        let Some((i, width)) = widest.filter(|&(_, width)| width > 0) else {
            break;
        };
        let take = left.min(width);
        let cut_text = trim_to(&cells[i].text, width - take);
        cells[i].text = cut_text;
        left -= take;
        cut = true;
    }
    cut
}

/// The column a row is drawn at. A row with children is NESTED and its depth is the walk's own; a
/// row without is a flat list's, and says its depth itself. One rule for the drawing and the keys,
/// so a `→` walks where the eye already reads the indent.
fn drawn_depth(base: usize, row: &pv::Row) -> usize {
    if row.children.is_empty() {
        row.depth
    } else {
        base
    }
}

/// The Enter action a row carries, in the words the head's own list uses - so a plugin's list and
/// the head's read alike.
fn enter_action(act: &RowAct, open: Option<Fold>) -> Option<TreeAction> {
    let words = match act {
        RowAct::Switch { .. } => "switches",
        RowAct::Event { .. } => "acts on it",
        // A row asking for a queue verb draws no Enter key in this list - see `list_says`.
        RowAct::Verb { .. } => return None,
        RowAct::Fold { .. } => match open {
            Some(Fold::Open) => "folds it away",
            Some(Fold::Closed) => "unfolds it",
            Some(Fold::Fixed) | None => return None,
        },
    };
    Some(TreeAction::new("enter", words))
}

fn fold_of(f: pv::Fold) -> Fold {
    match f {
        pv::Fold::Open => Fold::Open,
        pv::Fold::Closed => Fold::Closed,
        pv::Fold::Fixed => Fold::Fixed,
    }
}

fn cell_of(c: &pv::Cell) -> TreeCell {
    match c.role {
        Some(r) => TreeCell::role(c.text.clone(), crate::ui::severity::role(Some(r))),
        None => TreeCell::new(c.text.clone()),
    }
}

/// The rows as the keys read them: the list's stops, in drawing order.
fn listed(view: &pv::View, app: &App) -> Vec<Listed> {
    let mut out = Vec::new();
    let Some(tree) = view.panels.iter().find_map(|p| {
        p.shown().iter().find_map(|b| match b {
            pv::Block::Tree(t) => Some(t),
            _ => None,
        })
    }) else {
        return out;
    };
    for (base, row) in pv::rows_deep(&tree.rows) {
        let id = row.id().to_string();
        if id.is_empty() {
            continue;
        }
        let open = match (row.open, app.picker_folds.get(&id)) {
            (Some(pv::Fold::Fixed), _) => Some(Fold::Fixed),
            (Some(_), Some(true)) => Some(Fold::Open),
            (Some(_), Some(false)) => Some(Fold::Closed),
            (Some(f), None) => Some(fold_of(f)),
            (None, _) => None,
        };
        out.push(Listed {
            depth: drawn_depth(base, row),
            session: app.sessions.iter().position(|s| s.session_id == id),
            number: row.number,
            enter: row.enter.clone(),
            open,
            id,
        });
    }
    out
}

/// The one `Loader` this binary uses, from `letibot-plugins-rano` - the daemon's and the bridge's
/// too: a compiled module is kept per process, so a workspace switch or a second session's stack
/// compiles nothing that has been compiled before. The type is a unit; there is nothing to build.
pub(crate) use letibot_plugins_rano::RanoLoader;

#[cfg(test)]
mod tests {
    // The engine's seam, for the one test that stands a refusing loader in: `RanoLoader` is the
    // only other `Loader` this crate names.
    use letibot_plugins::{Capability, Loader, Module};

    use super::*;

    /// The head's own reading of `XDG_CONFIG_HOME=`, which used to be its own answer: the variable
    /// set but empty joined `letibot/` onto nothing, so the head read a stack relative to its cwd
    /// while the daemon read `~/.config/letibot`. Both scopes now come from `letibot_plugins::paths`.
    #[test]
    fn an_empty_xdg_config_home_is_the_users_plugin_dir() {
        static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _held = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "letibot-head-xdg-{}-{}",
            std::process::id(),
            letibot_metrics::now_ms()
        ));
        std::fs::create_dir_all(&home).expect("a temp home");
        let before: Vec<(&str, Option<std::ffi::OsString>)> =
            ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "HOME"]
                .into_iter()
                .map(|k| (k, std::env::var_os(k)))
                .collect();
        // SAFETY: the module's only test that touches the environment, under the lock above.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", "");
            std::env::set_var("XDG_DATA_HOME", "");
            std::env::set_var("HOME", &home);
        }
        let config = user_config_dir();
        let session = session_plugins_dir("s-1");
        for (key, value) in before {
            // SAFETY: as above, and restored before the assertions.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
        assert_eq!(config, Some(home.join(".config").join("letibot")));
        assert_eq!(
            session,
            Some(
                home.join(".local/share/letibot")
                    .join("sessions")
                    .join("s-1")
                    .join("plugins")
            )
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The built-in layer answers through rano's engine, in this process - the head's half of
    /// `docs/plugins-plan.md`: the same module the daemon carries, loaded by the same
    /// engine, asked by the head that draws it.
    #[test]
    fn the_builtin_sessions_layer_loads_and_answers_in_the_head() {
        let tmp =
            std::env::temp_dir().join(format!("letibot-head-sessions-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("a temp dir");
        let host = Host::build("s-none", &tmp);
        assert!(
            host.stack.failures().is_empty(),
            "nothing failed to load: {:?}",
            host.stack.failures()
        );
        let layer = host
            .reader(HOOK, Feed::Sessions)
            .expect("the built-in sessions layer is found");
        assert_eq!(layer.id, "b-sessions");
        let input = Input {
            v: letibot_plugin::pane::V,
            feeds: [(
                "sessions".to_string(),
                json!({"sessions": [{"id": "s-1", "title": "one"}], "here": "s-1", "home": "/home/x"}),
            )]
            .into_iter()
            .collect(),
            ..Input::default()
        };
        let answered = host.ask(HOOK, "the session list", &input);
        assert_eq!(
            host.take_problem(),
            None,
            "the built-in answered, and nothing failed"
        );
        let view = answered.view.expect("the built-in answered");
        assert_eq!(view.title, "sessions in this daemon");
        assert_eq!(
            host.take_problem(),
            None,
            "every built-in installed, so there is nothing to say"
        );
    }

    /// The built-in `todo` layer is installed and answers the head's command, which is the one a
    /// person actually types. `install_builtins` is under test as much as the module: the table is
    /// generated at build time, so a layer in the tree and not in the binary fails HERE, named.
    #[test]
    fn the_builtin_todo_layer_is_installed_and_answers_the_heads_command() {
        let tmp = std::env::temp_dir().join(format!("letibot-head-todo-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("a temp dir");
        let host = Host::build("s-none", &tmp);
        assert!(
            host.stack.failures().is_empty(),
            "nothing failed to load: {:?}",
            host.stack.failures()
        );
        let layer = host
            .stack
            .layers()
            .iter()
            .find(|l| l.id == "b-todo")
            .expect("the built-in `todo` layer is in the stack");
        assert!(
            layer.manifest.hooks.iter().any(|h| h == "command_todo"),
            "and it declares the hook the head asks: {:?}",
            layer.manifest.hooks
        );
        assert_eq!(
            layer.manifest.actions,
            vec!["add_todo".to_string()],
            "the one act the head performs, declared where a reviewer reads it"
        );
        assert!(
            layer.manifest.capabilities.is_empty(),
            "it writes nothing itself — the head is what writes"
        );

        let resolved = host.command(
            "todo",
            &json!({"line": "title:\"x\" body:\"y\"", "feeds": {}}),
        );
        assert_eq!(
            resolved.by.as_deref(),
            Some("b-todo"),
            "the answer is this binary's own layer: {:?}",
            resolved.failures
        );
        assert_eq!(
            resolved
                .answer
                .as_ref()
                .and_then(|a| a.pointer("/act/args/content"))
                .and_then(Value::as_str),
            Some("x — y"),
            "and it parsed the line"
        );
        assert_eq!(
            resolved
                .answer
                .as_ref()
                .and_then(|a| a.pointer("/act/name"))
                .and_then(Value::as_str),
            Some("add_todo")
        );
        assert_eq!(host.take_problem(), None, "and nothing was owed a sentence");
    }

    /// A built-in that will not load is SAID, not silently skipped: the pane says no layer answered
    /// and the sentence names the layer, because a binary whose own layer is broken is a defect. A
    /// build cannot ship an empty table - the build script stops with one sentence.
    #[test]
    fn a_builtin_that_cannot_load_is_a_sentence_and_not_a_silence() {
        struct Refuses;
        impl Loader for Refuses {
            fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
                Err(format!("no engine here: {}", wasm.display()))
            }
        }
        let mut stack = Stack::discover(None, None, None, Arc::new(Refuses));
        let said = install_builtins(&mut stack).expect("a refusal is a sentence");
        assert!(said.contains("built-in plugin layer"), "{said}");
        assert!(said.contains("sessions"), "the layer is named: {said}");
        assert!(
            said.contains("answered by the head itself"),
            "and the fallback is named: {said}"
        );
        // And a stack whose built-ins DID install says nothing at all.
        let mut good = Stack::discover(None, None, None, Arc::new(RanoLoader));
        assert_eq!(install_builtins(&mut good), None);
    }

    /// A store that has held thousands of sessions hands the head's list a capped feed, and the
    /// built-in layers answer it inside the hook's fuel budget (they trapped past ~1000 rows).
    #[test]
    fn the_builtin_layers_draw_a_full_session_feed_inside_their_fuel() {
        use letibot_plugin::pane::SESSION_FEED_ROWS;
        let rows: Vec<Value> = (0..SESSION_FEED_ROWS)
            .map(|i| {
                json!({"id": format!("s-{i:04}-aaaaaaaa"), "title": format!("session number {i}"),
                       "slug": "x", "parent": null, "origin": "user", "live": i % 3 == 0,
                       "running": i % 7 == 0, "heads": i % 2, "items": 10, "stored": 10,
                       "model": "glm", "workspace": format!("/p/repo/worktrees/w{}", i % 40),
                       "waits_on": 0, "nags_paused": false, "moved_ms": 1_000 + i,
                       "project": "/p/repo", "project_name": "repo", "bucket": "recent",
                       "system": null, "repo": "/p/repo"})
            })
            .collect();
        let input = json!({"v": 1, "feeds": {"sessions": {"sessions": rows,
            "here": "s-0001-aaaaaaaa", "home": "/h", "now": 9_000_000}}});
        for (name, hook, needle) in [("irc", "pane_nicks", "nicks"), ("sessions", HOOK, "")] {
            let wasm = letibot_builtins::table()
                .iter()
                .find(|b| b.name == name)
                .expect("the layer is built in")
                .wasm;
            let module = RanoLoader
                .load_bytes(name, wasm, &[])
                .expect("the module loads");
            let answered = module
                .call(hook, &input.to_string())
                .unwrap_or_else(|why| panic!("{name} trapped: {why}"));
            assert!(
                answered.is_some_and(|a| a.contains(needle)),
                "{name} draws the pane"
            );
            assert!(
                module.fuel() < 45_000_000,
                "{name} burned {} of 50M",
                module.fuel()
            );
        }
    }
}
