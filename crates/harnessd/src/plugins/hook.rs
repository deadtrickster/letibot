//! The hook dispatcher: one door per hook kind the daemon asks a session's stack, the ask that
//! runs a module outside the session lock, and the by-line a refusal is reported with.
//! What each kind's answer MEANS belongs to `letibot_plugin`; this is only the daemon's side.

use super::*;

impl PluginHost {
    /// Whether the layer answering `hook` runs freely: a built-in, or a layer whose bytes a person
    /// pinned. An `exec` look asks this before the gate.
    pub fn free_layer(&self, hook: &str) -> bool {
        self.answering(hook).is_some_and(|a| a.trusted)
    }

    /// Whether any layer of this session answers `hook` at all - what a caller asks before it pays
    /// to build an input nothing would read.
    pub fn answers(&self, hook: &str) -> bool {
        self.answering(hook).is_some()
    }

    /// Ask one hook of this session's stack, recording the input for the gate's replay. `None` when
    /// nothing here exports the hook, which keeps a session that patches nothing as cheap as it was.
    /// A layer that traps is skipped, the layer below answers, and the model is told.
    pub fn ask(
        &self,
        hub: &Arc<Hub>,
        hook: &str,
        input: &Value,
    ) -> Option<letibot_plugins::Resolved> {
        // The binary's own layer for a hook the daemon asks, before the stack is read: a session
        // whose project says nothing about reviews still gets the words this build carries.
        if daemon_asks(hook) {
            self.install_builtin_for(hub, hook);
        }
        // What the call needs, taken under the lock and no more. A module runs for as long as its
        // fuel lasts; a pane drawn or a tool adjudicated meanwhile must not queue behind it.
        let (stack, was) = {
            let mut inner = self.lock();
            let answering = inner.stack.layers().iter().any(|l| {
                !l.disabled
                    && l.manifest.contract == letibot_plugins::CONTRACT
                    && l.manifest.hooks.iter().any(|h| h == hook)
            });
            if !answering {
                return None;
            }
            inner.rec.record(hook, input);
            (Arc::clone(&inner.stack), inner.footprint())
        };
        let resolved = stack.call(hook, input);
        let mut inner = self.lock();
        say_failures(hub, &mut inner, &resolved.failures);
        // The layers changed while this answer was being made. A failure stays news - it happened -
        // but the answer belongs to a stack that is no longer in force, and is dropped the way any
        // late answer is: the caller falls back to what it does with no answer at all.
        if inner.footprint() != was {
            return None;
        }
        Some(resolved)
    }

    /// One hook called outside the lock [`PluginHost::ask`] describes: the layers are taken under
    /// it, the module runs without it, and the answer is applied only to the stack it came from.
    pub(super) fn call_unlocked(
        &self,
        hub: &Arc<Hub>,
        hook: &str,
        input: &Value,
    ) -> Option<letibot_plugins::Resolved> {
        let (stack, was) = {
            let mut inner = self.lock();
            inner.rec.record(hook, input);
            (Arc::clone(&inner.stack), inner.footprint())
        };
        let resolved = stack.call(hook, input);
        let mut inner = self.lock();
        say_failures(hub, &mut inner, &resolved.failures);
        (inner.footprint() == was).then_some(resolved)
    }

    /// One lifecycle event through this session's stack: the acts a layer asked for, checked against its
    /// manifest's `actions`, for the caller to perform. Nothing here can stop the event - there is no
    /// field for it in a [`letibot_plugin::lifecycle::Answer`] - and the event has already happened.
    pub fn lifecycle(&self, hub: &Arc<Hub>, event: &str, input: &Value) -> Lifecycle {
        let mut out = Lifecycle::default();
        let Some(resolved) = self.ask(hub, &letibot_plugin::lifecycle::hook(event), input) else {
            return out;
        };
        out.by = resolved.by.clone();
        let Some(answer) = resolved.answer else {
            return out;
        };
        let by = resolved.by.unwrap_or_default();
        let declared = self
            .layer(&by)
            .map(|l| l.manifest.actions)
            .unwrap_or_default();
        let parsed = match letibot_plugin::lifecycle::Answer::parse(answer.clone()) {
            Ok(parsed) => parsed,
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "{why}, and a lifecycle answer is `{{\"acts\": [{{\"verb\": …, \"args\": …}}]}}` \
                         and nothing else: {answer}"
                    ),
                );
                return out;
            }
        };
        // All or nothing: an answer naming an act the manifest did not declare is the layer's bug,
        // and it loses the whole answer rather than half of it - the same rule a pane view has.
        if let Err(why) = parsed.validate_for(&declared) {
            self.complain(hub, &by, &why);
            return out;
        }
        out.acts = parsed
            .acts
            .into_iter()
            .map(|a| (a.verb, a.args.unwrap_or(Value::Null)))
            .collect();
        out
    }

    /// One notice through the `notice` hook: the sentence to record, or `None` when a layer dropped
    /// it. Which kinds may be dropped is `letibot_plugin::notice`'s answer, not this function's, and
    /// a layer that drops one the model is owed is refused and told why.
    pub fn notice(&self, hub: &Arc<Hub>, kind: &str, text: &str, facts: Value) -> Option<String> {
        let input = letibot_plugin::notice::Notice::new(kind, text, facts);
        let input = match serde_json::to_value(&input) {
            Ok(input) => input,
            // A notice that cannot be serialised is a bug in the host, not a layer's, and a
            // sentence the model is owed is never lost to it.
            Err(_) => return Some(text.to_string()),
        };
        let Some(resolved) = self.ask(hub, &letibot_plugin::notice::hook(kind), &input) else {
            // Nothing in this session answers this kind: the harness's own sentence, unchanged.
            return Some(text.to_string());
        };
        let Some(answer) = resolved.answer else {
            return Some(text.to_string());
        };
        let by = resolved.by.clone().unwrap_or_default();
        let out: letibot_plugin::notice::NoticeOut = match serde_json::from_value(answer.clone()) {
            Ok(out) => out,
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "the layer `{by}` answered something that is not a notice, which is \
                         `{{\"text\": \"…\"}}` or `{{\"drop\": true}}` and nothing else: \
                         {why}. Its answer was {answer}, so the harness's own sentence stands"
                    ),
                );
                return Some(text.to_string());
            }
        };
        match out.said() {
            letibot_plugin::notice::Said::Unchanged => Some(text.to_string()),
            letibot_plugin::notice::Said::Text(said) => Some(said.to_string()),
            letibot_plugin::notice::Said::Drop => {
                if letibot_plugin::notice::droppable(kind) {
                    return None;
                }
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "the layer `{by}` asked for the `{kind}` notice to be dropped, and a `{kind}` \
                         notice may be reworded but not dropped — it is a fact the model is owed \
                         (docs/plugins-plan.md §4.3). The harness's own sentence stands"
                    ),
                );
                Some(text.to_string())
            }
        }
    }

    /// One nag decision through the `nag_` hook: which eligible row the plan-check names, in what words,
    /// and when it may come due again. `None` when nothing answers or the answer is refused, and then the
    /// harness's own check stands. The rules live in `letibot_plugin::nag::Out::decide`.
    pub fn nag(
        &self,
        hub: &Arc<Hub>,
        input: &letibot_plugin::nag::Input,
    ) -> Option<letibot_plugin::nag::Decision> {
        let asked = serde_json::to_value(input).ok()?;
        let resolved = self.ask(hub, letibot_plugin::nag::HOOK, &asked)?;
        let answer = resolved.answer?;
        let by = resolved.by.clone().unwrap_or_default();
        let out = match letibot_plugin::nag::read(&answer) {
            Ok(out) => out,
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "the layer `{by}` answered something that is not a nag decision, which is \
                         `{{\"name\": [\"<a row the check may speak about>\"], \"text\": \"…\", \
                         \"next_ms\": …}}` or `{{\"quiet\": true}}` and nothing else: {why}. \
                         The harness's own check stands"
                    ),
                );
                return None;
            }
        };
        match out.decide(input) {
            Ok(decision) => Some(decision),
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!("the layer `{by}` cannot decide this check: {why}"),
                );
                None
            }
        }
    }

    /// The turn-end decision through `steer_turn`, with the host's two rules applied: the continuation
    /// budget and an interrupted turn are refused here rather than trusted to the layer, and a refusal
    /// says so. `None` is the harness's own decision.
    pub fn steer(
        &self,
        hub: &Arc<Hub>,
        input: &letibot_plugin::steer::Input,
        here: &letibot_plugin::steer::Here,
    ) -> Option<letibot_plugin::steer::Decision> {
        use letibot_plugin::steer;
        let value = serde_json::to_value(input).ok()?;
        let resolved = self.ask(hub, steer::HOOK, &value)?;
        let answer = resolved.answer?;
        let by = resolved.by.clone().unwrap_or_default();
        let parsed = match steer::Answer::parse(answer.clone()) {
            Ok(parsed) => parsed,
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!("{why}. The harness's own decision stands"),
                );
                return None;
            }
        };
        match parsed.decide(here) {
            Ok(decision) => Some(decision),
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!("{why}. The harness's own decision stands"),
                );
                None
            }
        }
    }

    /// One compaction's template through `compact_summary`: the sections the record keeps, a layer's
    /// own sentence, and what it pins. `None` is the harness's own record - no layer, a trap, or an
    /// answer naming something this compaction did not offer.
    pub fn compact(
        &self,
        hub: &Arc<Hub>,
        input: &letibot_plugin::compact::Input,
    ) -> Option<letibot_plugin::compact::Answer> {
        use letibot_plugin::compact;
        let value = serde_json::to_value(input).ok()?;
        let resolved = self.ask(hub, compact::HOOK, &value)?;
        let answer = resolved.answer?;
        let by = resolved.by.clone().unwrap_or_default();
        let parsed = match compact::Answer::parse(answer.clone()) {
            Ok(parsed) => parsed,
            Err(why) => {
                self.complain(hub, &by, &format!("{why}. The harness's own record stands"));
                return None;
            }
        };
        if let Err(why) = parsed.validate(input) {
            self.complain(hub, &by, &format!("{why}. The harness's own record stands"));
            return None;
        }
        Some(parsed)
    }

    /// One sentence for this session's model, queued once: a failure said every call is noise the
    /// model learns to ignore. Answers whether this is the first time, which is also when the
    /// session's log is worth a line.
    pub(super) fn say_once(&self, said: String) -> bool {
        let mut inner = self.lock();
        if !inner.said.insert(said.clone()) {
            return false;
        }
        inner.notices.push(said);
        true
    }

    /// A layer's answer the host could not use, said once - to the model, which can fix the layer
    /// and to the session's log, so a person watching sees why nothing changed. Not through the
    /// `notice` hook: a layer that fails must not fail again about its own failure.
    fn complain(&self, hub: &Arc<Hub>, layer: &str, what: &str) {
        let said = format!("a plugin layer's answer was refused: {what}");
        if !self.say_once(said.clone()) {
            return;
        }
        hub.warn(
            "plugin_layer",
            if layer.is_empty() {
                said
            } else {
                format!("{layer}: {said}")
            },
        );
    }

    /// What this session's model has not been told yet, handed over and forgotten.
    pub fn take_notices(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().notices)
    }

    /// The layer that answers `hook`: the topmost enabled one that exports it, which is the one the
    /// stack asks first. A look a lower layer declared runs only when the upper one asked below
    /// and the upper one's capabilities are still the ones in force for the answer that came back.
    pub fn answering(&self, hook: &str) -> Option<Answering> {
        let inner = self.lock();
        inner
            .stack
            .layers()
            .iter()
            .find(|l| !l.disabled && l.manifest.hooks.iter().any(|h| h == hook))
            .map(|l| Answering {
                id: l.id.clone(),
                name: l.manifest.name.clone(),
                scope: l.scope,
                capabilities: l.manifest.capabilities.clone(),
                trusted: l.trusted,
            })
    }

    /// The capabilities the layer that answers `hook` declared - what a look runs under.
    pub fn capabilities_for(&self, hook: &str) -> Vec<Capability> {
        self.answering(hook)
            .map(|l| l.capabilities)
            .unwrap_or_default()
    }

    /// The layer `id`, as the sentence a model reads: its manifest name and scope, `pr-watch
    /// (project)`. `s3` is a layer's identity to every mechanism and nothing to a reader, and a
    /// layer nobody can find is answered by its id rather than an invented name.
    pub fn layer_sentence(&self, id: &str) -> String {
        let inner = self.lock();
        match inner.stack.layer(id) {
            Some(l) => format!("{} ({})", l.manifest.name, l.scope),
            None => id.to_string(),
        }
    }

    /// Whether a person vouched for this layer's bytes: a built-in, or a placed layer matching a
    /// recorded pin. Everything else - a session's own layer above all - is the governed agent's.
    fn vouched(&self, id: &str) -> bool {
        self.layer(id).is_some_and(|l| l.trusted)
    }

    /// A look the gate would not run, said once - per layer, program and reason, since the sentence
    /// names all three and is its own dedup key. A refusal repeated on every firing is how a model
    /// learns to skip them; one that changes is news again.
    pub fn look_refused(&self, hub: &Arc<Hub>, said: String) {
        {
            let mut inner = self.lock();
            if !inner.said.insert(said.clone()) {
                return;
            }
            inner.notices.push(said.clone());
        }
        hub.warn("plugin_look", said);
    }

    /// The last inputs this session's host asked `hook` with, newest last - what the gate's replay
    /// runs over. Public because the record is the session's, not this module's: a head, `plugin
    /// test`'s replay and a test all read the same ring.
    pub fn recorded(&self, hook: &str) -> Vec<Value> {
        self.lock().rec.inputs(hook).to_vec()
    }

    /// One result through the `on_tool_result` hook: the call, how it ended, and what the model
    /// would read. `None` when nothing answers, or the answer is a pass or a mask; an answer that is
    /// not the contract is refused and the call's own result stands.
    pub fn on_tool_result(
        &self,
        hub: &Arc<Hub>,
        call: &letibot_plugin::Call,
        outcome: &str,
        payload: &str,
    ) -> Option<Answered<letibot_plugin::tool_result::ToolResultOut>> {
        use letibot_plugin::tool_result::{ToolResultIn, ToolResultOut};
        let input = ToolResultIn::new(call.clone(), outcome, payload);
        let input = serde_json::to_value(&input).ok()?;
        let resolved = self.ask(hub, letibot_plugin::tool_result::HOOK, &input)?;
        let answer = resolved.answer?;
        let by = resolved.by.clone().unwrap_or_default();
        match serde_json::from_value::<ToolResultOut>(answer.clone()) {
            Ok(out) => Some(Answered { out, by }),
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "the layer `{by}` answered something that is not a tool result — \
                         `payload`, `append` and `arm` are the whole of it: {why}. Its answer \
                         was {answer}, so the call's own result stands"
                    ),
                );
                None
            }
        }
    }

    /// One call through the `on_tool_call` hook - before the gate, so what a layer rewrites is what
    /// the gate then adjudicates. `None` when nothing answers; an answer that is not the contract is
    /// refused and the call runs as proposed, because the gate still decides.
    pub fn on_tool_call(
        &self,
        hub: &Arc<Hub>,
        call: &letibot_plugin::Call,
        standing: &letibot_tools::runtime::Standing,
    ) -> Option<Answered<letibot_plugin::tool_call::ToolCallOut>> {
        use letibot_plugin::tool_call::{Session, ToolCallIn, ToolCallOut};
        let session = Session {
            session: standing.session.clone(),
            mode: standing.mode.clone(),
            // The host's own workspace, not the gate's string: this is the directory a layer is
            // written about, and it is the same one the looks and the panes resolve against.
            workspace: self.workspace.display().to_string(),
        };
        let input = ToolCallIn::new(call.clone(), session);
        let input = serde_json::to_value(&input).ok()?;
        let resolved = self.ask(hub, letibot_plugin::tool_call::HOOK, &input)?;
        // A guard that failed refuses the call: the layer's id and why go to the model.
        if let Some((by, why)) = resolved.refused {
            let said = format!(
                "plugin layer `{by}` guards this call and failed, so the call is refused: {why}"
            );
            self.complain(hub, &by, &said);
            return Some(Answered {
                out: ToolCallOut::refuse(said),
                by,
            });
        }
        let answer = resolved.answer?;
        let by = resolved.by.clone().unwrap_or_default();
        let refuse = |this: &Self, why: String| {
            let said = format!(
                "plugin layer `{by}` answered badly for a guard, so the call is refused: {why}"
            );
            this.complain(hub, &by, &said);
            Some(Answered {
                out: ToolCallOut::refuse(said),
                by: by.clone(),
            })
        };
        match serde_json::from_value::<ToolCallOut>(answer.clone()) {
            Ok(mut out) => {
                if let letibot_plugin::tool_call::Verdict::Rewrite(args) = out.verdict()
                    && let Err(why) = only_smaller(&call.arguments, args)
                {
                    return refuse(self, why);
                }
                // An `answer` stands in for the tool's result and the gate never sees the call, so
                // only a layer the operator vouched for may give one: dropping it sends the call
                // on to the gate as though the layer had passed, and says why once.
                if matches!(out.verdict(), letibot_plugin::tool_call::Verdict::Answer(_))
                    && !self.vouched(&by)
                {
                    out.answer = None;
                    let said = format!(
                        "plugin layer `{by}` answered this call, and an answer stands in for a \
                         tool's result with the gate never seeing the call: that is a pinned \
                         layer's or a built-in's to do, so `{}` runs and meets the gate. Pin the \
                         layer's bytes and load it again if the answer was meant to stand",
                        call.name
                    );
                    self.complain(hub, &by, &said);
                }
                Some(Answered { out, by })
            }
            Err(why) => refuse(
                self,
                format!(
                    "its answer was {answer}, which is not a call's verdict (`pass`, `arguments`, \
                     `refuse` and `answer` are the whole of it): {why}"
                ),
            ),
        }
    }

    /// One call through the `adjudicate_` floor: the names a layer gives the action and the rung it
    /// requires, or `Pass` when nothing answered. A trap or a bad answer is `Trapped`, which the gate
    /// reads as the strictest tier - a floor that cannot answer has not said the call is fine.
    pub fn adjudicate(
        &self,
        hub: &Arc<Hub>,
        action: &letibot_plugin::adjudicate::Action,
    ) -> letibot_tools::adjudicate::Floored {
        use letibot_plugin::adjudicate::{AdjudicateIn, AdjudicateOut, Floor};
        use letibot_tools::adjudicate::Floored;
        let input = AdjudicateIn::new(action.clone());
        let Ok(input) = serde_json::to_value(&input) else {
            return Floored::Trapped {
                by: String::new(),
                why: "the action this host built is not expressible as JSON".into(),
            };
        };
        let Some(resolved) = self.ask(hub, letibot_plugin::adjudicate::HOOK, &input) else {
            return Floored::Pass;
        };
        if let Some((by, why)) = resolved.refused {
            let said = format!("it could not answer, so the call is at the strictest tier: {why}");
            self.complain(hub, &by, &said);
            return Floored::Trapped { by, why };
        }
        let Some(answer) = resolved.answer else {
            return Floored::Pass;
        };
        let by = self.layer_sentence(&resolved.by.unwrap_or_default());
        match serde_json::from_value::<AdjudicateOut>(answer.clone()) {
            // A layer that names nothing and asks for nothing is silence, not an answer.
            Ok(out) if out.says_nothing() => Floored::Pass,
            Ok(out) => {
                // Read before the names move: the rung is what the gate acts on, the names do not.
                let floor = out.required().unwrap_or(Floor::Auto);
                Floored::Raised {
                    intents: out.intents,
                    floor,
                    by,
                }
            }
            Err(why) => {
                let said = format!(
                    "its answer was {answer}, which is not a floor (`intents` and `floor` are the \
                     whole of it): {why}"
                );
                self.complain(hub, &by, &said);
                Floored::Trapped {
                    by,
                    why: format!("its answer is not the contract: {why}"),
                }
            }
        }
    }

    /// Every hook of this stack whose name a prefix begins, topmost layer first and each name once:
    /// the names a kind with a name of its own is asked under (`digest_<name>`, `link_<name>`), as
    /// against a kind resolved by one shared export.
    pub fn hooks_named(&self, prefix: &str) -> Vec<String> {
        let inner = self.lock();
        let mut out: Vec<String> = Vec::new();
        for layer in inner.stack.layers() {
            if layer.disabled || layer.manifest.contract != letibot_plugins::CONTRACT {
                continue;
            }
            for hook in &layer.manifest.hooks {
                if hook.starts_with(prefix)
                    && letibot_plugins::is_hook_kind(hook)
                    && !out.contains(hook)
                {
                    out.push(hook.clone());
                }
            }
        }
        out
    }

    /// One output through this stack's `digest_` hooks: the first layer that answers, in the shape
    /// [`crate::plugins::DigestHooks`] hands the runtime. `None` when nothing answers, and then the
    /// bytes budgeted are the tool's own; a layer that traps is said once and falls to the one below.
    pub fn digest(
        &self,
        hub: &Arc<Hub>,
        call: &letibot_plugin::Call,
        payload: &str,
    ) -> Option<letibot_tools::runtime::Digest> {
        use letibot_plugin::digest::{Input, Out};
        let hooks = self.hooks_named(letibot_plugin::digest::PREFIX);
        if hooks.is_empty() {
            return None;
        }
        let input = serde_json::to_value(Input::new(call.clone(), payload)).ok()?;
        for hook in hooks {
            let Some(resolved) = self.ask(hub, &hook, &input) else {
                continue;
            };
            let Some(answer) = resolved.answer else {
                continue;
            };
            let by = resolved.by.clone().unwrap_or_default();
            let out: Out = match serde_json::from_value(answer.clone()) {
                Ok(out) => out,
                Err(why) => {
                    self.complain(
                        hub,
                        &by,
                        &format!(
                            "the layer `{by}` answered something that is not a digest — \
                             `{{\"payload\": \"…\"}}` is the whole of it: {why}. Its answer was \
                             {answer}, so the call's own output stands"
                        ),
                    );
                    continue;
                }
            };
            match out.checked(payload) {
                Ok(None) => continue,
                Ok(Some(text)) => {
                    return Some(letibot_tools::runtime::Digest {
                        payload: text.to_string(),
                        by: self.layer_sentence(&by),
                    });
                }
                Err(why) => self.complain(
                    hub,
                    &by,
                    &format!(
                        "the layer `{by}` digested this output and the host will not hand it on: \
                         {why}. The call's own output stands"
                    ),
                ),
            }
        }
        None
    }

    /// One reviewer's brief through this stack's `review_brief` layer, or `None` when no layer here
    /// answers it, and then the host's own text stands. A trap or an answer outside the contract is said
    /// once and falls to the native text: a review with no brief would be a review that never starts.
    pub fn review_brief(
        &self,
        hub: &Arc<Hub>,
        req: &letibot_tools::gatekeeper::ReviewRequest,
    ) -> Option<letibot_plugin::review::Brief> {
        let input = letibot_plugin::review::BriefIn::of(letibot_plugin::review::Entry::of(
            req.brief.clone(),
            req.branch.clone(),
            req.base_sha.clone(),
            if req.head_sha.trim().is_empty() {
                req.branch.clone()
            } else {
                req.head_sha.clone()
            },
            req.cwd.clone(),
        ));
        match self.review_answer(hub, &input) {
            Some(Some(brief)) => Some(brief),
            _ => None,
        }
    }

    /// The review queue's own prompt through the same hook (the entry is absent): what a project
    /// wants the session that runs its reviews to be told, in its words.
    pub fn review_queue_brief(&self, hub: &Arc<Hub>) -> Option<letibot_plugin::review::Brief> {
        let input = letibot_plugin::review::BriefIn::queue();
        match self.review_answer(hub, &input) {
            Some(Some(brief)) => Some(brief),
            _ => None,
        }
    }

    /// [`PluginHost::review_brief`]'s body, shared with the queue's own prompt. The outer `Option`
    /// is *a layer answered at all*, the inner *what it said*.
    fn review_answer(
        &self,
        hub: &Arc<Hub>,
        input: &letibot_plugin::review::BriefIn,
    ) -> Option<Option<letibot_plugin::review::Brief>> {
        let input = serde_json::to_value(input).ok()?;
        let resolved = self.ask(hub, letibot_plugin::review::BRIEF, &input)?;
        let Some(answer) = resolved.answer else {
            return Some(None);
        };
        let by = resolved.by.unwrap_or_default();
        match serde_json::from_value::<letibot_plugin::review::Brief>(answer.clone()) {
            Ok(brief) if brief.says_nothing() => Some(None),
            Ok(brief) => Some(Some(brief)),
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "the layer `{by}` answered something that is not a reviewer's brief — \
                         `{{\"brief\": \"…\", \"brief_add\": \"…\", \"require\": [ … ]}}` is the \
                         whole of it: {why}. Its answer was {answer}, so the host's own words stand"
                    ),
                );
                None
            }
        }
    }

    /// The rung a `review_verdict` layer requires of a verdict, or `None` when nothing here answers. A
    /// trap or an answer the contract refuses is `NeedsHuman`: a policy that cannot answer has not said
    /// the branch may land, and a person is exactly who a broken floor means.
    pub fn review_floor(
        &self,
        hub: &Arc<Hub>,
        decision: letibot_tools::gatekeeper::Decision,
    ) -> Option<letibot_plugin::review::Floor> {
        use letibot_plugin::review::{Floor, Verdict, VerdictIn};
        let input = serde_json::to_value(VerdictIn::new(
            Floor::parse(decision.as_str()).unwrap_or(Floor::Accept),
            Vec::new(),
        ))
        .ok()?;
        let resolved = self.ask(hub, letibot_plugin::review::VERDICT, &input)?;
        let by = resolved.by.clone().unwrap_or_default();
        let Some(answer) = resolved.answer else {
            // Nothing answered. A layer that trapped has not said the branch may land, so the row
            // goes to a person; a hook nobody exports is silence and the verdict stands.
            return (!resolved.failures.is_empty()).then_some(Floor::NeedsHuman);
        };
        match serde_json::from_value::<Verdict>(answer.clone()) {
            Ok(out) => out.required(),
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "the layer `{by}` answered something that is not a verdict's floor — \
                         `{{\"floor\": \"…\", \"why\": \"…\"}}` is the whole of it: {why}. Its \
                         answer was {answer}, so the verdict is a person's to make"
                    ),
                );
                Some(Floor::NeedsHuman)
            }
        }
    }

    /// A worktree child's brief through this stack's `brief_worktree` layer. The answer is checked
    /// against the seat words the host can narrow: a class it cannot place is said once and the seat
    /// stands as it was asked, because a dropped word nobody noticed is a seat nobody can describe.
    pub fn child_brief(
        &self,
        hub: &Arc<Hub>,
        input: letibot_plugin::brief::Input,
    ) -> Option<letibot_plugin::brief::Answer> {
        let input = serde_json::to_value(input).ok()?;
        let resolved = self.ask(hub, letibot_plugin::brief::WORKTREE, &input)?;
        let answer = resolved.answer?;
        let by = resolved.by.unwrap_or_default();
        match serde_json::from_value::<letibot_plugin::brief::Answer>(answer.clone()) {
            Ok(out) if out.says_nothing() => None,
            Ok(out) => match out.checked() {
                Ok(()) => Some(out),
                Err(why) => {
                    self.complain(hub, &by, &why);
                    None
                }
            },
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "the layer `{by}` answered something that is not a child's brief — \
                         `{{\"brief_add\": \"…\", \"seat_drop\": [ … ]}}` is the whole of it: {why}. \
                         Its answer was {answer}, so the native brief and the seat stand"
                    ),
                );
                None
            }
        }
    }
}

/// A rewrite may only make a call smaller: the tool is fixed by the contract, and no argument
/// may appear that the model did not send.
fn only_smaller(was: &serde_json::Value, now: &serde_json::Value) -> Result<(), String> {
    match (was, now) {
        (_, serde_json::Value::Null) => Ok(()),
        (serde_json::Value::Object(was), serde_json::Value::Object(now)) => {
            match now.keys().find(|k| !was.contains_key(*k)) {
                Some(k) => Err(format!(
                    "its rewrite adds the argument `{k}`, and a layer can only make a call smaller"
                )),
                None => Ok(()),
            }
        }
        _ => Err("its rewrite changes the shape of the arguments, and a layer can only make a call smaller".into()),
    }
}

/// The runtime's `on_tool_call`, backed by one session's plugin host. A slot and not the host
/// itself: the runtime is built when the harness opens, the host when the daemon opens the session
/// so a session the daemon does not serve leaves the slot empty and its calls run as they did.
pub struct CallHooks {
    slot: Arc<Mutex<Option<Arc<PluginHost>>>>,
    hub: Arc<Hub>,
}

impl CallHooks {
    pub fn new(slot: Arc<Mutex<Option<Arc<PluginHost>>>>, hub: Arc<Hub>) -> CallHooks {
        CallHooks { slot, hub }
    }
}

impl letibot_tools::runtime::CallHook for CallHooks {
    fn before(
        &self,
        call: &letibot_plugin::Call,
        standing: &letibot_tools::runtime::Standing,
    ) -> Option<letibot_tools::runtime::Patched> {
        let host = self
            .slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let answered = host.on_tool_call(&self.hub, call, standing)?;
        // An `answer` is somebody else's words standing in for a result, and the payload cannot
        // say so: it reads like a tool's own output. So the result carries one line naming the
        // layer as a person wrote it. A `refuse` needs none - its outcome already says so.
        let note = matches!(
            answered.out.verdict(),
            letibot_plugin::tool_call::Verdict::Answer(_)
        )
        .then(|| {
            format!(
                "answered by plugin {}; the tool did not run",
                host.layer_sentence(&answered.by)
            )
        });
        Some(letibot_tools::runtime::Patched {
            out: answered.out,
            by: answered.by,
            note,
        })
    }
}

/// The gate's `adjudicate_` floor, backed by one session's plugin host. A slot rather than the host
/// for the reason [`CallHooks`] holds one: the gate is built when the harness opens and the host
/// when the daemon opens the session, so a session nobody serves decides exactly as it did.
pub struct FloorHooks {
    slot: Arc<Mutex<Option<Arc<PluginHost>>>>,
    hub: Arc<Hub>,
}

impl FloorHooks {
    pub fn new(slot: Arc<Mutex<Option<Arc<PluginHost>>>>, hub: Arc<Hub>) -> FloorHooks {
        FloorHooks { slot, hub }
    }
}

impl letibot_tools::adjudicate::FloorHook for FloorHooks {
    fn floor(
        &self,
        action: &letibot_plugin::adjudicate::Action,
    ) -> letibot_tools::adjudicate::Floored {
        let Some(host) = self.slot.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return letibot_tools::adjudicate::Floored::Pass;
        };
        host.adjudicate(&self.hub, action)
    }
}

/// The GitHub panel's `review_verdict` floor, over one session's plugin host: a pull request is judged
/// by the same policy a branch is. A word in and a word out because the registry holds the door and
/// knows neither the tools nor the SDK, and this is the one impl the daemon installs.
pub struct ReviewFloorHooks {
    slot: Arc<Mutex<Option<Arc<PluginHost>>>>,
    hub: Arc<Hub>,
}

impl ReviewFloorHooks {
    pub fn new(slot: Arc<Mutex<Option<Arc<PluginHost>>>>, hub: Arc<Hub>) -> ReviewFloorHooks {
        ReviewFloorHooks { slot, hub }
    }
}

impl letibot_sessionlog::registry::ReviewFloor for ReviewFloorHooks {
    fn floor(&self, decision: &str) -> Option<String> {
        let host = self
            .slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let decision = letibot_tools::gatekeeper::Decision::parse(decision)?;
        host.review_floor(&self.hub, decision)
            .map(|floor| floor.as_str().to_string())
    }
}

/// The runtime's `digest_` hook, backed by one session's plugin host - the same slot as
/// [`CallHooks`], because a session has one stack and two would be two answers to what is in force.
pub struct DigestHooks {
    slot: Arc<Mutex<Option<Arc<PluginHost>>>>,
    hub: Arc<Hub>,
}

impl DigestHooks {
    pub fn new(slot: Arc<Mutex<Option<Arc<PluginHost>>>>, hub: Arc<Hub>) -> DigestHooks {
        DigestHooks { slot, hub }
    }
}

impl letibot_tools::runtime::DigestHook for DigestHooks {
    fn digest(
        &self,
        call: &letibot_plugin::Call,
        payload: &str,
    ) -> Option<letibot_tools::runtime::Digest> {
        let host = self
            .slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        host.digest(&self.hub, call, payload)
    }
}

/// Whether `hook` is one the daemon asks a session's stack for, which is what makes a built-in layer
/// its fallback. The hooks themselves say it, so this cannot drift from the plugins' work.
fn daemon_asks(hook: &str) -> bool {
    letibot_plugin::review::is_review_hook(hook) || hook == letibot_plugin::brief::WORKTREE
}
