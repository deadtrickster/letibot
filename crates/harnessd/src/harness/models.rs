//! Model switching: `/models`, a provider swap, a declared local model and the way back. The
//! endpoints, vocabularies and windows a session moves between, and what each switch owes the one
//! before it.
use super::*;

/// The daemon's own server, exactly as this session's daemon was started against it: what `/models local` returns
/// to. Captured at open, before any switch can move anything, since a switch overwrites the fields this holds. The
/// vocabulary and wiring are the `Arc`s `Parts` opened with, so a return re-seats that pair.
#[derive(Clone)]
pub(crate) struct OwnServer {
    pub(crate) endpoint: Endpoint,
    pub(crate) model: String,
    /// The GGUF the daemon started on, or `None` when it started on the byte vocabulary. `vocab`
    /// below holds the thing itself either way, and its `source()` is the honest name for both.
    pub(crate) vocab_gguf: Option<PathBuf>,
    pub(crate) dialect: crate::dialect::Dialect,
    pub(crate) media_marker: Option<String>,
    pub(crate) sampling: serde_json::Value,
    pub(crate) vocab: Arc<Vocab>,
    pub(crate) wiring: Arc<Wiring>,
}

/// The sentence a superseded request leaves behind - its asker's wait ends with the daemon's
/// own words rather than with the new requester's.
pub(crate) fn superseded(name: &str) -> String {
    format!(
        "the `{name}` confirmation was superseded by a newer request before anybody answered \
         it, so nothing changed; the newer one is on the screen now"
    )
}

/// A switch waiting for the turn in flight to end: recorded, said, and applied where the prompt span closes. A switch
/// taken inside a failing round re-issues that round on the new model carrying the rest of the old one's turn, which
/// the new model refuses, and a model's first request must follow a user message.
pub(crate) struct PendingSwitch {
    /// The argument [`Harness::set_provider`] will be given when the turn ends: `Some` is a
    /// metered provider, `None` the daemon's own server.
    choice: Option<crate::config::ProviderConfig>,
    /// The model it lands on, in `/models`' spelling (`deepseek/deepseek-flash`, `local`), read
    /// when it was recorded - so the sentence that says what will happen does not have to build
    /// the provider to name it.
    model: String,
}

/// The model a `set_provider` argument would land on, in [`Harness::model_name`]'s spelling
/// (`deepseek/deepseek-flash`, `local`), so a pending switch can be named and compared with what
/// answers now without building a provider for it.
pub(crate) fn model_name_of(choice: &Option<crate::config::ProviderConfig>) -> String {
    match choice {
        None => "local".to_string(),
        Some(pc) => match &pc.model {
            Some(m) => format!("{}/{m}", pc.name),
            None => pc.name.clone(),
        },
    }
}

/// The sentence a failed switch earns, apart from [`Harness::switch_back_notice`] so the wording
/// and the command are testable without a session, a store and a provider. The command is spelled
/// the way `/models` takes it, because the daemon already knows which of five spellings it wants.
pub(crate) fn switch_back_said(previous: &str, now: &str, code: u16) -> String {
    format!(
        "the first round on `{now}` was refused by the provider (http {code}). Nothing this \
         turn produced is lost and the conversation is unchanged: `/models {previous}` takes \
         this session back to `{previous}`, and the next turn goes there."
    )
}

/// The model a session switched away from, kept until a round proves the new one answers: the fact that makes a failed
/// switch recoverable by the person who typed it. Held in `model_name`'s spelling, which is what `/models` takes, so the
/// notice's command is not a second name to translate.
pub(crate) struct SwitchBack {
    /// What answered before the switch: `deepseek/deepseek-flash`, `local`.
    previous: String,
}

impl Harness {
    pub fn attach_oracle(&mut self, endpoint: Endpoint) -> Result<String, String> {
        self.cfg.oracle = Some(endpoint.clone());
        let advisor = model_adjudicator(&self.cfg, "`/supervise`", Some(self.hub.clone()))
            .map_err(|e| e.to_string())?;
        self.gate().attach_advisor(std::sync::Arc::from(advisor))?;
        Ok(format!(
            "guard model at {} for this session. To make it the default, put it in \
             {}:\n  [gatekeeper]\n  endpoint = \"{}\"",
            endpoint.authority(),
            letibot_provider::keys::config_file().display(),
            endpoint.authority()
        ))
    }

    /// Switch to a model this fleet hosts: a `[model."..."]` block with an address, and `local` in every sense that
    /// matters, so this takes the local path with `provider` cleared. The engine tokenizes HERE, so other weights are only
    /// safe through a fork re-rendered under their vocabulary and dialect, and weights matching no dialect are refused.
    pub fn set_local_model(
        &mut self,
        m: &letibot_provider::keys::LocalModel,
    ) -> Result<String, HarnessError> {
        let want = parse_local_url(&m.url)
            .map_err(|e| HarnessError::Setup(format!("[model.\"{}\"] {e}", m.name)))?;
        // One `/props` probe decides the whole switch, before anything moves, so a refusal leaves
        // the session exactly as it was. The daemon's own vocabulary is optional, and without one
        // nothing can be the SAME weights as a vocabulary that does not exist.
        let switch = local_switch_decision(&want, m, self.cfg.vocab_gguf.as_deref())
            .map_err(HarnessError::Setup)?;

        // Off any metered provider first, and by its own door, so the ledger scale and the restored window are handled
        // where that is understood. Its report is discarded, since it describes a move to the daemon's own server, a
        // waypoint here. The other-weights arm withholds that door's re-seat so one decision does not pay two prefills.
        match switch {
            LocalSwitch::SameWeights => {
                // The UNGATED door: this is a waypoint on the road to a fleet model, not a switch the operator asked
                // for, and the gated door would see a prompt in flight, record `local` as pending and then move the
                // endpoint under it. A fleet switch cannot arrive inside a turn anyway.
                self.set_provider_now(None)?;
                self.move_local_endpoint(&want, m)
            }
            LocalSwitch::OtherWeights {
                props,
                dialect,
                gguf,
            } => {
                self.set_own_server(false)?;
                self.set_local_model_weights(&want, m, props, dialect, gguf)
            }
        }
    }

    /// The address move a same-weights switch is: the engine, the dialect and the conversation all
    /// stay, and only where turns are posted changes. Split from [`Harness::set_local_model`] so the
    /// arm that changes nothing but the address reads as exactly that.
    fn move_local_endpoint(
        &mut self,
        want: &Endpoint,
        m: &letibot_provider::keys::LocalModel,
    ) -> Result<String, HarnessError> {
        // The engine is what actually holds the address: `cfg.endpoint` is the record and
        // `engine.endpoint` is where `/completion` is posted. Writing one and not the other is how a
        // switch reports a move it did not make.
        self.cfg.endpoint = want.clone();
        self.engine.endpoint = want.clone();
        self.cfg.model = m.model.clone();
        self.engine.model = m.model.clone();
        if !m.profile.sampling.is_empty() {
            let sampling = serde_json::Value::Object(m.profile.sampling.clone());
            self.cfg.sampling = sampling.clone();
            self.engine.sampling = sampling;
        }
        // A different box is a different KV cache and a different window, so a ratio measured on the
        // old one is not evidence about this one - nor is one measured on a cloud model, whose count
        // is not this ledger's.
        self.cfg.ledger_scale = None;
        self.cfg.carried_scale = None;
        let moved = self.retune_window_local(want, &m.name, m.profile.window);
        let line = format!(
            "turns go to `{}` at {} from the next one on (fleet, no meter){}{moved}",
            m.model,
            want.authority(),
            if m.profile.sampling.is_empty() {
                String::new()
            } else {
                format!(", sampling {}", render_sampling(&m.profile.sampling))
            }
        );
        Ok(line)
    }

    /// Say on the status row that the conversation is being re-rendered for other weights, and
    /// when it is done. It holds the worker, so a prompt sent meanwhile reads `queued behind` this
    /// rather than a bare `queued` that looks like a hang.
    fn say_rerendering(&self, items: usize, done: bool) {
        let total = items as u64;
        self.hub.publish(letibot_sessionlog::SessionEvent::Filling {
            what: "re-rendering the conversation for the new weights".into(),
            unit: "items".into(),
            done: if done { total } else { 0 },
            total,
        });
    }

    /// The cross-weights switch: the arm that makes `/models local.glm` work from a daemon started on the qwen
    /// weights. The order is what leaves a session that still works on failure: vocabulary loaded, fork written, engine
    /// moved last.
    fn set_local_model_weights(
        &mut self,
        want: &Endpoint,
        m: &letibot_provider::keys::LocalModel,
        props: letibot_turn::serving::ServedProps,
        dialect: crate::dialect::Dialect,
        gguf: std::path::PathBuf,
    ) -> Result<String, HarnessError> {
        if self.store.is_none() {
            return Err(HarnessError::Setup(format!(
                "switching to the other weights at {} re-renders this conversation \
                 into a fork, and a fork needs a store to write it to; this session \
                 has none, so the weights, the dialect and the address were all left \
                 alone",
                want.authority()
            )));
        }
        // The GGUF `/props` named is a path on the box that RUNS the server, and this box
        // tokenizes. Refused with the path rather than hunted by basename: a basename hunt that
        // found the wrong file would be this switch's own corruption, committed by its own fix.
        if !gguf.is_file() {
            return Err(HarnessError::Setup(format!(
                "{} serves weights at `{}` (its /props answer), and that is not a file \
                 this box can read. A switch tokenizes here and sends ids, so it needs \
                 those weights' GGUF on THIS box — mount or link it at that path (the \
                 first shard of a split model is enough) and switch again. Nothing was \
                 changed.",
                want.authority(),
                gguf.display()
            )));
        }
        let old_dialect = self.cfg.dialect;
        // What tokenizes NOW, read before the engine below replaces it: a GGUF's
        // path, or `bytes:<hash>` for a daemon that started with no GGUF of its
        // own - which is exactly the daemon this arm is new for.
        let old_gguf = self.engine.vocab().source().to_string();
        let was_tokens = self.session.ledger.len();

        // The vocabulary of the weights that will answer: the second a process ever loads, and
        // CPU-only by the shim's own construction, which is what lets a daemon already holding one
        // model's weights load another's tokenizer.
        let vocab = Arc::new(load_gguf(&gguf).map_err(|e| {
            HarnessError::Setup(format!(
                "loading the vocabulary at `{}`, which {}'s /props named: {e}. A \
                 switch tokenizes here and sends ids, so it needs that file on this \
                 box. Nothing was changed.",
                gguf.display(),
                want.authority()
            ))
        })?);
        let wiring = Arc::new(dialect.wiring(self.cfg.effort.as_deref()));
        let sampling = if m.profile.sampling.is_empty() {
            self.cfg.sampling.clone()
        } else {
            serde_json::Value::Object(m.profile.sampling.clone())
        };
        // Resolving the dialect's control and stop tokens against the target's
        // vocabulary is the fit check `open` runs at daemon start; refused here
        // for the same reason - at runtime a mismatch is silent.
        let mut engine = TurnEngine::new(
            vocab.clone(),
            wiring.renderer.clone(),
            wiring.parser.clone(),
            want.clone(),
            letibot_backend::BackendCaps::OWN_SERVER,
            m.model.clone(),
            sampling.clone(),
        )
        .map_err(|e| {
            HarnessError::Setup(format!(
                "the {} dialect does not fit the vocabulary at `{}`: {e}. Every \
                 control token and stop literal must resolve to exactly one vocab \
                 entry — a mismatch is a dialect/weights pairing error, refused now \
                 because at runtime it is silent. Nothing was changed.",
                dialect.name(),
                gguf.display()
            ))
        })?;
        engine.media_marker = props.media_marker.clone();

        // The fork's prefix, keyed by the NEW dialect's template sha: that is the key a resume
        // checks before reusing a rendering, so this row is what stops another dialect's cache row
        // being reused, here or by a daemon that resumes the session later.
        let next = StablePrefix {
            system: self.cfg.system.clone(),
            tools_json: wiring.tools_json(&self.runtime.registry.schemas()),
        };
        let measured = engine
            .open(&format!("{}#switch-probe", self.cfg.session_id), &next)
            .map_err(|e| HarnessError::Setup(format!("rendering the new prompt: {e}")))?;
        let rec = StablePrefixRecord {
            dialect_sha: hex32(&wiring.spec().template_sha),
            system: next.system.clone(),
            tools_json: next.tools_json.clone(),
            tokens: measured.ledger.prefix_tokens().to_vec(),
            h_init: measured.ledger.h_init(),
            vocab_source: gguf.display().to_string(),
        };
        drop(measured);
        let next_id = self
            .store
            .as_ref()
            .expect("refused at the top of this method")
            .put_stable_prefix(&rec)
            .map_err(|e| HarnessError::Store(e.to_string()))?;

        // The engine moves first, because the fork below renders with `self.engine`, and the fork is
        // the first thing that can no longer fail cheaply.
        self.engine = engine;
        // The counter the notes pane measures with moves too: the section is assembled with the
        // engine's own vocabulary, so a pane measuring with the one this session OPENED on would
        // disagree with the prompt about which notes are whole.
        self.note_vocab = vocab.clone();
        self.render = wiring.clone();
        self.cfg.dialect = dialect;
        self.cfg.vocab_gguf = Some(gguf.clone());
        self.cfg.endpoint = want.clone();
        self.cfg.model = m.model.clone();
        self.cfg.sampling = sampling;
        self.cfg.media_marker = props.media_marker.clone();
        self.cfg.ledger_scale = None;
        // The new weights count their own tokens: an estimate carried from a cloud model
        // would be a conversion of a local count into something it is not.
        self.cfg.carried_scale = None;

        // The conversation, re-rendered whole under the new pair: the items are dialect-neutral,
        // while the tokens were a cache of one rendering - the same rationale the resume fork is
        // built on.
        let items = self.session.items.clone();
        let outcome = CompactionOutcome {
            turn_id: format!("{}#weights", self.transcript_id),
            summary: String::new(),
            tool_calls: 0,
            truncated: false,
            cached_tokens: 0,
            reusable: 0,
            generated_tokens: 0,
            budget: None,
            fidelity: None,
        };
        self.compacting = true;
        self.say_rerendering(items.len(), false);
        let forked = self.fork_to_summary(
            &outcome,
            Some(&next),
            Some(&next_id),
            ForkTail {
                items: &items,
                split: None,
                because: "",
            },
        );
        self.compacting = false;
        self.say_rerendering(items.len(), true);
        let fork = forked?;
        self.adopt_reseat(Some((next, next_id)));

        // A different box is a different KV cache and a different window; the
        // ratio measured on the old one is not evidence about this one, and the
        // conversation just changed size under it anyway.
        let moved = self.retune_window_local(want, &m.name, m.profile.window);
        let line = format!(
            "turns go to `{}` at {} from the next one on (fleet, no meter). Weights \
             changed — `{}` → `{}`, dialect {} → {} — so this conversation's {} \
             item(s) were re-rendered for them: {} → {} tokens, fork {}, one cold \
             prefill on the next turn{}{}",
            m.model,
            want.authority(),
            old_gguf,
            gguf.display(),
            old_dialect.name(),
            dialect.name(),
            items.len(),
            was_tokens,
            fork.base_tokens,
            fork.transcript_id,
            if m.profile.sampling.is_empty() {
                String::new()
            } else {
                format!(", sampling {}", render_sampling(&m.profile.sampling))
            },
            moved
        );
        self.publish_settings();
        Ok(line)
    }

    /// Point `context_window` at the box that will answer: the fleet half of `retune_window`, reading the server rather
    /// than the catalogue. The window does not travel with matching weights, since two servers holding one GGUF can be
    /// started at different `n_ctx`, and a server that will not report one leaves the window `None`, which is said.
    fn retune_window_local(&mut self, want: &Endpoint, name: &str, stated: Option<u64>) -> String {
        // Remembered on the way out, so `/models local` restores the server this
        // daemon was started against rather than keeping another box's number. Same
        // write site and same mirror as `retune_window`, so the two cannot disagree.
        if self.local_window.is_none() {
            self.local_window = Some(self.cfg.context_window);
            if let Ok(mut g) = self.local_window_cell.lock() {
                *g = self.local_window;
            }
        }
        let was = self.cfg.context_window;
        // A stated window beats the probe, as `--context-window` beats `/props` at startup: the
        // operator naming a number is them telling the daemon something it cannot work out, and the
        // case the key exists for is a proxy whose `/props` answers about itself.
        let now = stated.or_else(|| letibot_turn::serving::served_ctx(want));
        self.cfg.context_window = now;
        match now {
            Some(w) if Some(w) == was => String::new(),
            Some(w) => {
                // A local endpoint counts with the ledger's own vocabulary, so no conversion
                // applies and `window_moved` leaves the figure alone.
                self.cfg.context_window = Some(w);
                window_moved(&self.cfg, was, w, self.session.ledger.len() as u64)
            }
            None => format!(
                ". {} did not report an n_ctx, so there is no wall to plan against — \
                 put a window on [model.\"{name}\"] or expect no compaction",
                want.authority()
            ),
        }
    }

    /// Ask the operator for the key `key_wanted` names, and make it the session's provider: the masked card is the
    /// sudo password's, so the key reaches providers.toml and nowhere else. Refuses at once when no head that answers
    /// decisions is attached, and a key providers.toml could not hold is asked for again, with `why` as the reason.
    pub(crate) fn obtain_key(&mut self, why: Option<String>) -> Result<(), HarnessError> {
        let Some(pc) = self.key_wanted.clone() else {
            return Ok(());
        };
        let preset = letibot_provider::Preset::parse(&pc.name).map_err(HarnessError::Setup)?;
        let file = letibot_provider::keys::config_file();
        if self.hub.deciding_heads().is_empty() {
            return Err(HarnessError::Setup(format!(
                "no key for `{name}`, and no head is attached to ask for one. Set ${env}, or put \
                 `key = \"…\"` under [{name}] in {file}",
                name = pc.name,
                env = preset.key_env,
                file = file.display(),
            )));
        }
        let mut reason = why;
        for _ in 0..3 {
            // The prompt is persisted with the request (the secret never is), so it says
            // where the key goes and nothing it should not.
            let prompt = format!(
                "{name} needs an API key. It is saved to {file} (mode 0600, under [{name}]) \
                 and this session's turns go to {name}.{again}",
                name = pc.name,
                file = file.display(),
                again = reason
                    .as_deref()
                    .map(|r| format!(" Asking again: {r}"))
                    .unwrap_or_default(),
            );
            let deadline = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| (d + KEY_PATIENCE).as_millis() as u64)
                .unwrap_or(0);
            // An empty `command` is what tells a head this is not sudo's card.
            let (req_id, rx) = self.hub.request_secret(&prompt, "", deadline);
            let given = match rx.recv_timeout(KEY_PATIENCE) {
                Ok(s) => s,
                Err(_) => {
                    self.hub
                        .abandon_secret(&req_id, "nobody, before the deadline");
                    None
                }
            };
            let Some(key) = given.map(|k| k.trim().to_string()) else {
                return Err(HarnessError::Setup(format!(
                    "no key was given for `{name}`, so this turn did not run and nothing was \
                     sent. Send the message again to be asked again, or set ${env}, or put \
                     `key = \"…\"` under [{name}] in {file}",
                    name = pc.name,
                    env = preset.key_env,
                    file = file.display(),
                )));
            };
            if let Some(bad) = unsavable_key(&key) {
                reason = Some(format!("that key was not saved — {bad}."));
                continue;
            }
            letibot_provider::keys::store_key(Some(&file), &pc.name, &key).map_err(|e| {
                HarnessError::Setup(format!("saving the key to {}: {e}", file.display()))
            })?;
            // The first key on a box also chooses the default, so the next `letibot` starts here
            // without a flag. Only when there is none: an existing choice is the operator's.
            let mut made_default = false;
            if let Ok(None) = letibot_provider::keys::default_choice(Some(&file)) {
                made_default =
                    letibot_provider::keys::set_default(Some(&file), &pc.name, pc.model.as_deref())
                        .is_ok();
            }
            self.key_wanted = None;
            self.set_provider(Some(pc.clone()))?;
            self.hub.warn(
                "provider_key_saved",
                format!(
                    "the key for {name} is saved in {file}{default}; this session's turns go to \
                     {name}",
                    name = pc.name,
                    file = file.display(),
                    default = if made_default {
                        format!(", and [default] provider = \"{}\" is set", pc.name)
                    } else {
                        String::new()
                    },
                ),
            );
            return Ok(());
        }
        Err(HarnessError::Setup(format!(
            "no usable key for `{}` after three tries: {}",
            pc.name,
            reason.unwrap_or_default()
        )))
    }

    /// Switch what answers this session's turns, underneath the conversation: `None` is the local server, and the
    /// transcript, ledger and tools are untouched. In flight the request is PENDING, since a mid-round switch re-issues
    /// the round carrying the rest of the old turn. A provider built for the model already on is a rebuild, not a switch.
    pub fn set_provider(
        &mut self,
        choice: Option<crate::config::ProviderConfig>,
    ) -> Result<String, HarnessError> {
        if self.prompt_in_flight.load(Ordering::SeqCst) {
            let target = model_name_of(&choice);
            if target != self.model_name() {
                return Ok(self.pend_switch(choice, target));
            }
        }
        self.set_provider_now(choice)
    }

    /// Record a switch for the end of this turn and say so: the sentence is both the return value and a
    /// `model_switch_pending` warning, so a head that did not type it has it on the status line. The provider is not
    /// built here, since building it would either refuse a request the turn cannot act on or build it twice.
    pub(crate) fn pend_switch(
        &mut self,
        choice: Option<crate::config::ProviderConfig>,
        target: String,
    ) -> String {
        let now = self.model_name();
        let said = format!("turns go to {target} when this turn ends; this one finishes on {now}");
        self.pending_switch = Some(PendingSwitch {
            choice,
            model: target,
        });
        self.hub.warn("model_switch_pending", said.clone());
        said
    }

    /// The switch itself, with no question about the turn span: `set_provider` is the door everything asks through and
    /// this is what it calls once nothing is in flight. `end_turn_clock` must not go through that gate, since the span
    /// has just closed and it is applying what the gate recorded.
    fn set_provider_now(
        &mut self,
        choice: Option<crate::config::ProviderConfig>,
    ) -> Result<String, HarnessError> {
        // A switch clears the measured ratio in both directions: a measurement made on one model is not evidence about
        // another, and carrying it across presents a stale measurement as a current one. The old pair is kept as an
        // estimate that names its model, and the clear sits inside each arm so a refusal still changes nothing.
        match choice {
            // The daemon's own server takes token ids, and a byte-vocabulary session has none it could read: refused
            // before anything changes. The guard reads the vocabulary the engine tokenizes with NOW, which a
            // cross-weights switch may have replaced since the daemon started.
            None if self.engine.vocab().is_bytes() => {
                Err(HarnessError::Setup(NO_VOCAB_FOR_LOCAL.to_string()))
            }
            // `/models local` is a RETURN to the daemon's own weights, address, sampling, marker,
            // window and vocabulary, re-seated, not a provider clear. `set_own_server` is this arm.
            None => self.set_own_server(true),
            Some(pc) => {
                let p = build_provider(
                    &pc,
                    self.cfg.providers_file.as_deref(),
                    provider_notes(&self.hub),
                    self.cfg.deadlines_for(Some(&pc)),
                    self.cfg.effort.as_deref(),
                )
                .map_err(HarnessError::Setup)?;
                let mut line = format!(
                    "turns go to {}/{} from the next one on — METERED; the prefix check is \
                     skipped, the ledger stays the record",
                    p.name(),
                    p.model()
                );
                // The window follows the model: compaction is planned against `context_window`, and
                // it came from the LOCAL server's `/props`, so leaving it there measured a cloud
                // conversation against the llama-server's window.
                line.push_str(&self.retune_window(&pc));
                // After everything that can fail, and before anything reads it.
                self.note_the_way_back();
                self.carry_the_scale();
                self.cfg.ledger_scale = None;
                // The same session's marks and the same write: a model switched to mid-conversation
                // learns for this session and no other.
                self.provider = Some(scoped_provider(
                    p,
                    self.picture_marks.clone(),
                    Some(persist_marks(
                        self.cfg.store_at(),
                        self.cfg.session_id.clone(),
                    )),
                ));
                self.cfg.provider = Some(pc);
                // Say it, or the head goes on drawing the old name: the header reads the model
                // from the daemon's word, and that word was sent once at attach.
                self.publish_settings();
                Ok(line)
            }
        }
    }

    /// Keep the ratio a switch is about to clear, named after the model that measured it. Called only where the switch
    /// SUCCEEDED, since one refused for a missing key changes nothing. A second switch with nothing measured between
    /// keeps the older pair rather than overwriting it with `None`.
    fn carry_the_scale(&mut self) {
        let Some((ledger, provider)) = self.cfg.ledger_scale else {
            return;
        };
        // The model being LEFT, in `/models`' spelling (`deepseek/deepseek-flash`), read
        // before the caller overwrites anything.
        let model = self.model_name();
        self.cfg.carried_scale = Some(crate::config::CarriedScale {
            model,
            ledger,
            provider,
        });
    }

    /// Remember the model this switch is leaving, so a refusal can name the way back.
    ///
    /// Read BEFORE anything moves, and only where the move cannot fail after it.
    fn note_the_way_back(&mut self) {
        self.switch_back = Some(SwitchBack {
            previous: self.model_name(),
        });
    }

    /// A switch that fails says how to go back, or `None` when this is not that. Without it the operator has to
    /// remember what the session had been on, while the daemon knew the name and the command. 4xx only, and the armed
    /// state is TAKEN, so repeating the advice every turn cannot make it furniture.
    pub fn switch_back_notice(&mut self, e: &HarnessError) -> Option<String> {
        let back = self.switch_back.take()?;
        let code = provider_refusal(e)?;
        let now = self.model_name();
        if now == back.previous {
            // The round went to the model this session already was on - a switch that
            // landed on the name in force is not a switch, and there is nowhere back.
            return None;
        }
        Some(switch_back_said(&back.previous, &now, code))
    }

    /// The whole of `/models NAME`, in one place for both callers: the worker's arm and the retry loop's poll. Two
    /// copies would be two places for the screen, the row and the answer to disagree. Asked for in flight it is recorded
    /// and not applied, and the reply still says when it takes. The choice is resolved by the caller.
    pub fn apply_model_choice(
        &mut self,
        choice: crate::slash::ModelChoice,
        mut lines: Vec<String>,
    ) -> crate::slash::SlashReply {
        // What answers before, so the row is written only when it moved: a pending switch leaves
        // this name alone, and a row written with the other model's name would make a restart resume
        // on a model this daemon never ran.
        let was = self.model_name();
        let applied = match choice {
            crate::slash::ModelChoice::OwnServer => self.set_provider(None),
            crate::slash::ModelChoice::Metered(pc) => self.set_provider(Some(pc)),
            // Its own door, because it verifies the vocabulary before it moves
            // anything - see `Harness::set_local_model`.
            crate::slash::ModelChoice::Local(m) => self.set_local_model(&m),
        };
        match applied {
            Ok(line) => {
                // The choice goes to the session row in the same breath: a switch that reached the
                // screen but not the row was a choice that expired with the process. A pending one
                // is written where it is applied instead.
                if self.model_name() != was
                    && let Err(why) = self.persist_provider_choice()
                {
                    lines.push(why);
                }
                lines.push(line);
                crate::slash::SlashReply { lines, ok: true }
            }
            Err(e) => {
                lines.push(e.to_string());
                crate::slash::SlashReply { lines, ok: false }
            }
        }
    }

    /// Back to the daemon's own server: the `None` arm of `set_provider`. `reseat_foreign` is the wrinkle, since
    /// `/models local` means the daemon's server as it was started, while a switch between two fleet models calls this
    /// only to get off a metered provider first and passes `false` to skip a re-seat it would throw away.
    fn set_own_server(&mut self, reseat_foreign: bool) -> Result<String, HarnessError> {
        self.note_the_way_back();
        self.provider = None;
        self.cfg.ledger_scale = None;
        // The carried pair goes too: coming home means the ledger IS the prompt again, so a ratio
        // measured by a cloud model would convert a count that needs no conversion.
        self.cfg.carried_scale = None;
        self.cfg.provider = None;
        // The return itself: every field a fleet switch overwrote, given back the value the daemon
        // started with. Half a return is a line that names the wrong box, which is the provenance
        // defect in report form.
        let own = self.own_server.clone();
        let mut reseat = String::new();
        let foreign = self.on_foreign_weights();
        if foreign && reseat_foreign {
            // The mirror of the switch's cross-weights arm, aimed home: the daemon's own pair, the conversation
            // re-rendered under it, a fork keyed by the daemon's own template sha. What moved is read BEFORE the
            // re-seat, because the re-seat overwrites it.
            let (was_vocab, was_dialect) =
                (self.engine.vocab().source().to_string(), self.cfg.dialect);
            self.reseat_to_own(&own)?;
            reseat = format!(
                ". Weights back — `{}` → `{}`, dialect {} → {} — and the conversation \
                 was re-rendered for them again: one more cold prefill, one more fork",
                was_vocab,
                own.vocab.source(),
                was_dialect.name(),
                own.dialect.name()
            );
        } else if foreign {
            // Off a metered provider on the way to another fleet model's weights:
            // everything the cross-weights switch is about to overwrite is left
            // exactly as it is, and the fork that would waste a prefill is not run.
        } else {
            self.cfg.endpoint = own.endpoint.clone();
            self.cfg.model = own.model.clone();
            self.engine.endpoint = own.endpoint.clone();
            self.engine.model = own.model.clone();
            if self.cfg.media_marker != own.media_marker {
                self.cfg.media_marker = own.media_marker.clone();
                self.engine.media_marker = own.media_marker;
            }
            // A fleet switch may have replaced the sampling; the daemon's own
            // server gets the sampling it started with back.
            self.cfg.sampling = own.sampling.clone();
            self.engine.sampling = own.sampling;
        }
        // Back to the server's own window, the one its `/props` reported at startup. Restored
        // rather than recomputed: the local endpoint is not asked again here, and keeping a cloud
        // model's window over a local conversation is the same bug pointing the other way.
        let mut line = format!(
            "turns go to the local server at {} ({}) from the next one on",
            self.cfg.endpoint.authority(),
            self.cfg.model
        );
        if let Some(w) = self.local_window.take() {
            if w != self.cfg.context_window {
                line.push_str(&match w {
                    Some(w) => format!(". Context window back to {w}"),
                    None => ". The local server never reported a window".into(),
                });
            }
            self.cfg.context_window = w;
        }
        line.push_str(&reseat);
        self.publish_settings();
        Ok(line)
    }

    /// Is this session on weights that are not the daemon's own? The one question that decides whether a return to the
    /// daemon's own server is an address move or a re-seat. Named because two readers ask different questions of it:
    /// whether to fork, and whether a RUNNING TURN may carry a `/models local` out itself.
    pub(crate) fn on_foreign_weights(&self) -> bool {
        self.cfg.vocab_gguf != self.own_server.vocab_gguf
            || self.cfg.dialect != self.own_server.dialect
    }

    /// The re-seat half of a return to the daemon's own weights: the mirror of `set_local_model_weights`, aimed at the
    /// `OwnServer` pair. The facts come from the record rather than a probe: the engine is rebuilt from the daemon's own
    /// vocabulary and wiring, the fork's prefix row is keyed by its template sha, and the conversation is re-rendered.
    fn reseat_to_own(&mut self, own: &OwnServer) -> Result<(), HarnessError> {
        if self.store.is_none() {
            return Err(HarnessError::Setup(
                "coming back to this daemon's own weights re-renders the conversation \
                 into a fork, and a fork needs a store to write it to; this session \
                 has none, so it stays on the weights it is on"
                    .into(),
            ));
        }
        let mut engine = TurnEngine::new(
            own.vocab.clone(),
            own.wiring.renderer.clone(),
            own.wiring.parser.clone(),
            own.endpoint.clone(),
            letibot_backend::BackendCaps::OWN_SERVER,
            own.model.clone(),
            own.sampling.clone(),
        )
        .map_err(|e| {
            HarnessError::Setup(format!(
                "the {} dialect does not fit the vocabulary at `{}`: {e} — the pair \
                 this daemon STARTED with, which cannot have drifted. This is a bug, \
                 not a configuration: the same pair opened this session.",
                own.dialect.name(),
                own.vocab.source()
            ))
        })?;
        engine.media_marker = own.media_marker.clone();
        let next = StablePrefix {
            system: self.cfg.system.clone(),
            tools_json: own.wiring.tools_json(&self.runtime.registry.schemas()),
        };
        let measured = engine
            .open(&format!("{}#return-probe", self.cfg.session_id), &next)
            .map_err(|e| HarnessError::Setup(format!("rendering the home prompt: {e}")))?;
        let rec = StablePrefixRecord {
            dialect_sha: hex32(&own.wiring.spec().template_sha),
            system: next.system.clone(),
            tools_json: next.tools_json.clone(),
            tokens: measured.ledger.prefix_tokens().to_vec(),
            h_init: measured.ledger.h_init(),
            vocab_source: own.vocab.source().to_string(),
        };
        drop(measured);
        let next_id = self
            .store
            .as_ref()
            .expect("refused above")
            .put_stable_prefix(&rec)
            .map_err(|e| HarnessError::Store(e.to_string()))?;

        self.engine = engine;
        // The same assignment as the switch's, in the other direction: the session is back on
        // the vocabulary it opened on and the notes source measures with what the section
        // will. See `Harness::install_notes_source`.
        self.note_vocab = own.vocab.clone();
        self.render = own.wiring.clone();
        self.cfg.dialect = own.dialect;
        self.cfg.vocab_gguf = own.vocab_gguf.clone();
        self.cfg.endpoint = own.endpoint.clone();
        self.cfg.model = own.model.clone();
        self.cfg.sampling = own.sampling.clone();
        self.cfg.media_marker = own.media_marker.clone();

        let items = self.session.items.clone();
        let outcome = CompactionOutcome {
            turn_id: format!("{}#home", self.transcript_id),
            summary: String::new(),
            tool_calls: 0,
            truncated: false,
            cached_tokens: 0,
            reusable: 0,
            generated_tokens: 0,
            budget: None,
            fidelity: None,
        };
        self.compacting = true;
        self.say_rerendering(items.len(), false);
        let forked = self.fork_to_summary(
            &outcome,
            Some(&next),
            Some(&next_id),
            ForkTail {
                items: &items,
                split: None,
                because: "",
            },
        );
        self.compacting = false;
        self.say_rerendering(items.len(), true);
        // The engine moved first, so a fork failure here would leave the session rendering the own dialect over a
        // foreign-rendered ledger, the corruption this file refuses. The fork cannot fail halfway, so this names the
        // state rather than papering over it.
        forked.map_err(|e| {
            HarnessError::Setup(format!(
                "re-rendering the conversation for this daemon's own weights: {e}. \
                 The switch to them did not complete; stay on the weights the \
                 conversation is rendered for and re-try the switch"
            ))
        })?;
        self.adopt_reseat(Some((next, next_id)));
        Ok(())
    }

    /// Point `context_window` at the model that will answer, and say what moved: the sentence to append to the
    /// switch's report, empty when nothing changed. An unknown model leaves the window alone AND says so, since the
    /// silent half of this bug was that nobody could tell which window was in force.
    fn retune_window(&mut self, pc: &crate::config::ProviderConfig) -> String {
        // Remembered on the way out to the first provider, so `/models local`
        // restores the server's own number rather than keeping a cloud model's.
        if self.local_window.is_none() {
            self.local_window = Some(self.cfg.context_window);
            // And mirrored to the runner, which needs it for a `local` child spawned while THIS
            // session is on a provider. One write site, so the two cannot disagree.
            if let Ok(mut g) = self.local_window_cell.lock() {
                *g = self.local_window;
            }
        }
        let Ok(preset) = letibot_provider::Preset::parse(&pc.name) else {
            return String::new();
        };
        let cat = letibot_provider::catalogue::Catalogue::load();
        let was = self.cfg.context_window;
        match preset.window(pc.model.as_deref(), &cat) {
            Some(w) if Some(w) == was => String::new(),
            Some(w) => {
                self.cfg.context_window = Some(w);
                // `window_moved` converts, because a metered model counts with the provider's
                // units and the ledger's count over-counts by the rows it is never sent.
                window_moved(&self.cfg, was, w, self.session.ledger.len() as u64)
            }
            None => format!(
                ". The catalogue has no window for {}/{}, so compaction still plans \
                 against {} — check that is the right size before a long turn",
                pc.name,
                pc.model
                    .clone()
                    .unwrap_or_else(|| preset.default_model(&cat)),
                match was {
                    Some(w) => w.to_string(),
                    None => "no window at all".into(),
                }
            ),
        }
    }

    /// What answers this session's turns right now, for `/models`.
    pub fn provider_line(&self) -> String {
        match &self.provider {
            None => format!(
                "local — {} at {}",
                self.cfg.model,
                self.cfg.endpoint.authority()
            ),
            Some(p) => format!("{}/{} (metered)", p.name(), p.model()),
        }
    }

    /// This session's model, spelled the way `/models` takes it: `local`, `name`, or `name/model`. Two readers ask
    /// which model the session is on rather than what answers it: the session row a restart reads back, and the retry
    /// loop's fallback. `provider_line` is the other question, the one a person reads, and they are not merged.
    pub fn model_name(&self) -> String {
        match &self.cfg.provider {
            None => "local".to_string(),
            Some(pc) => match &pc.model {
                Some(m) => format!("{}/{m}", pc.name),
                None => pc.name.clone(),
            },
        }
    }

    /// The session is over: let the backend release what it holds and say where
    /// its work went. A host backend says nothing; a firecode one brings its VM
    /// down and names the sibling directory.
    pub fn close_backend(&self) -> Option<String> {
        // A VM going down takes what was running in it, and this is the one door it goes down
        // through: a stop, a failed turn and a shutdown all reach here. Recorded as the kill it
        // was before the guest is gone, so no reader guesses it from the client's exit (R173).
        if self.cfg.placement.is_vm() {
            let _ = self.jobs_lost_with_the_vm();
        }
        // The watchers next: their threads hold weak handles and re-check this
        // flag at most one wait chunk after close, so a session that closes does
        // not keep its host - or its cgroups - alive behind a blocked thread.
        if let Some(w) = &self.job_watch {
            w.stop();
        }
        self.runtime.backend.close()
    }

    /// The switch that was waiting for the turn to end, taken: the other half of `set_provider`'s gate. A failure
    /// leaves the session on the model it is on, which is a working session, and says so under its own code.
    pub(crate) fn apply_pending_switch(&mut self) {
        let Some(pending) = self.pending_switch.take() else {
            return;
        };
        match self.set_provider_now(pending.choice) {
            Ok(line) => {
                // The session row follows it, as a switch made between turns does: the row is what a
                // restart resumes on, and a deferred switch that only reached this process would
                // expire with it.
                if let Err(why) = self.persist_provider_choice() {
                    self.hub
                        .warn("model_switch_applied", format!("{line}. {why}"));
                    return;
                }
                self.hub.warn(
                    "model_switch_applied",
                    format!("the turn has ended, so the switch stands: {line}"),
                );
            }
            Err(e) => {
                self.hub.warn(
                    "model_switch_not_applied",
                    format!(
                        "the switch to `{}` was waiting for this turn to end and could not be \
                         made: {e}. This session stays on `{}`.",
                        pending.model,
                        self.model_name()
                    ),
                );
            }
        }
    }

    /// The switch, on the session row, so a restart brings the session back on what it was switched to. Called where
    /// `publish_settings` is, since the two are one act's halves. `local` is stored BY NAME rather than left NULL,
    /// because a daemon whose default is a provider would otherwise hand a switched-back session straight back.
    pub fn persist_provider_choice(&self) -> std::result::Result<(), String> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let stored = self.model_name();
        store
            .set_provider_choice(&self.cfg.session_id, Some(&stored))
            .map_err(|e| format!("the choice was not written to the session row: {e}"))
    }

    /// A resume restores the session's own model, from the row the switch wrote, at both open paths. `None` is never
    /// switched, so the daemon's default stands; `local` is the deliberate switch back, restored as an ACT; a name is
    /// resolved through the same door the switch went through. A preset this build no longer knows leaves the row alone.
    pub fn restore_provider_choice(&mut self) -> Option<String> {
        let stored = self
            .store
            .as_ref()
            .and_then(|s| s.provider_choice(&self.cfg.session_id).ok())
            .flatten()?;
        if stored == "local" {
            self.cfg.provider.as_ref()?;
            return match self.set_provider(None) {
                Ok(_) => Some("restored to the local server".into()),
                Err(e) => Some(format!("could not restore the local server: {e}")),
            };
        }
        let (name, model) = match stored.split_once('/') {
            Some((n, m)) => (n.to_string(), Some(m.to_string())),
            None => (stored.clone(), None),
        };
        // Already there: a live session's second caller (a head switching IN, not a
        // resume) must not re-switch it and repaint the settings.
        let already = self
            .cfg
            .provider
            .as_ref()
            .is_some_and(|pc| pc.name == name && pc.model == model);
        if already {
            return None;
        }
        match crate::slash::models_choice(&name, model.as_deref(), None, None) {
            Ok((crate::slash::ModelChoice::Metered(choice), _)) => {
                match self.set_provider(Some(choice)) {
                    Ok(_) => Some(format!("restored to {stored}")),
                    Err(e) => Some(format!("could not restore {stored}: {e}")),
                }
            }
            // A fleet model is restored through its own door, which re-checks the vocabulary: the daemon being resumed
            // INTO may have been started on different weights, and skipping that check is the one path into the
            // corruption `set_local_model` exists to refuse.
            Ok((crate::slash::ModelChoice::Local(m), _)) => match self.set_local_model(&m) {
                Ok(_) => Some(format!("restored to {stored}")),
                Err(e) => Some(format!("could not restore {stored}: {e}")),
            },
            // `ModelChoice::Local` is `models_choice("local")`, and `local` is handled by name above,
            // so a row that reached here as local would mean the store spelled it some other way:
            // the honest answer is the daemon default rather than a panic on a fact nobody wrote.
            Ok((crate::slash::ModelChoice::OwnServer, _)) | Err(_) => Some(format!(
                "could not restore {stored}: this build does not know that provider, so the \
                 daemon's own default answers"
            )),
        }
    }
}

/// What one `/props` probe says about switching to a declared local model: the decision, or a refusal naming the fact
/// it is missing. The free function both doors need, so a session switching and a spawn cannot drift. `same_vocab` is
/// the operator's assertion about the WEIGHTS and says nothing about the dialect, which the same-weights arm keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalSwitch {
    /// The target serves the weights this session's vocabulary already tokenizes for, measured by basename or asserted
    /// with `same_vocab = true`: the engine, dialect and vocabulary all stay and only the address moves. A session with
    /// no vocabulary of its own can never take this arm, since there are no weights to be the same as.
    SameWeights,
    /// The target serves other weights: the switch must load their vocabulary and seat their
    /// dialect, and the conversation must be re-rendered into a fork before the first turn on them.
    OtherWeights {
        /// The probe everything here was derived from, for the caller's marker
        /// and its report.
        props: letibot_turn::serving::ServedProps,
        /// The dialect the served template (or the block's assertion) settled on.
        dialect: crate::dialect::Dialect,
        /// The GGUF `/props` named, as a path THIS box must be able to read.
        gguf: std::path::PathBuf,
    },
}

/// Decide a switch to a declared local model from one `/props` probe. Every refusal names the endpoint, what it looked
/// for, and the key that asserts the fact by hand, since it refused names nothing and this is the sentence read at a
/// picker. The daemon's own vocabulary is an `Option`, and with none the decision changes shape rather than refusing.
pub fn local_switch_decision(
    want: &letibot_turn::http::Endpoint,
    m: &letibot_provider::keys::LocalModel,
    vocab: Option<&std::path::Path>,
) -> Result<LocalSwitch, String> {
    // `same_vocab` is not sampling, so the profile reader files it under `unknown`, where a key it does not interpret
    // belongs. But only where there is a vocabulary to be the same as: a cloud-only daemon has none, so the probe
    // below decides alone.
    let asserted_same = vocab.is_some()
        && m.profile
            .unknown
            .iter()
            .any(|u| u == "same_vocab" || u.starts_with("same_vocab "));
    if asserted_same {
        return Ok(LocalSwitch::SameWeights);
    }
    let props = letibot_turn::serving::served_props(want).map_err(|why| match vocab {
        Some(vocab) => format!(
            "{why}, so this cannot check which weights answer there or which \
             template they render. Put `same_vocab = true` in [model.\"{}\"] to \
             assert they are the ones `{}` tokenizes for, or — if they are other \
             weights — a reachable target that answers /props.",
            m.name,
            vocab.display()
        ),
        None => format!(
            "{why}, so this cannot check which weights answer there or which \
             template they render. This daemon has no vocabulary of its own (it was \
             started for a cloud provider, whose turns need none), so there are no \
             weights here a switch could keep and no vocabulary `same_vocab = true` \
             could assert against: the switch to `{}` needs a reachable target that \
             answers /props and names the GGUF it serves, whose vocabulary and \
             dialect are then loaded from that file.",
            m.name
        ),
    })?;
    let Some(theirs) = props.model_path.clone() else {
        return Err(match vocab {
            Some(vocab) => format!(
                "{} answered /props but named no model_path, so there is no GGUF to \
                 load a vocabulary from. Put `same_vocab = true` in [model.\"{}\"] to \
                 assert by hand that it serves the ones `{}` tokenizes for.",
                want.authority(),
                m.name,
                vocab.display()
            ),
            None => format!(
                "{} answered /props but named no model_path, and this daemon has no \
                 vocabulary of its own that `same_vocab = true` could assert against: \
                 no weights here to keep, none reported there to load. The switch to \
                 `{}` was refused and nothing changed.",
                want.authority(),
                m.name
            ),
        });
    };
    if vocab
        .is_some_and(|vocab| vocab_basename(std::path::Path::new(&theirs)) == vocab_basename(vocab))
    {
        // Same weights carry the same template, so the dialect this session speaks is the dialect they render: nothing
        // to re-derive and the switch is the address move it always was. Unreachable with no vocabulary of our own.
        return Ok(LocalSwitch::SameWeights);
    }
    let dialect = dialect_for_target(&props, m, want).map_err(|why| {
        format!(
            "{}, so the switch to `{}` at {}{} \
             was refused and nothing changed",
            why,
            m.model,
            want.authority(),
            match vocab {
                // With no vocabulary of this daemon's own, the GGUF the target
                // reported is the only weights this switch could ever have
                // seated - so the refusal names it, which is what is true.
                None => format!(
                    " (it reports its weights at `{theirs}`; this daemon has no \
                     vocabulary of its own, so those are the only weights this \
                     switch could ever have seated)"
                ),
                Some(_) => String::new(),
            }
        )
    })?;
    Ok(LocalSwitch::OtherWeights {
        props,
        dialect,
        gguf: std::path::PathBuf::from(theirs),
    })
}

/// Which dialect does this target render? From its own template when it says, from the block's assertion when it
/// cannot, refused when neither speaks: a served template settles it, since it cannot drift from what is rendered, and
/// guessing a dialect is valid ids meaning other words.
fn dialect_for_target(
    props: &letibot_turn::serving::ServedProps,
    m: &letibot_provider::keys::LocalModel,
    want: &letibot_turn::http::Endpoint,
) -> Result<crate::dialect::Dialect, String> {
    use crate::dialect::Dialect;
    let all = [Dialect::Glm, Dialect::Qwen];
    // An asserted value that names no dialect is refused rather than dropped: honouring an
    // assertion nobody can read is the regression the profile reader's own test names, one layer
    // down.
    let asserted = m.profile.dialect.as_deref().map(|name| {
        Dialect::parse(name).ok_or_else(|| {
            format!(
                "[model.\"{}\"] asserts `dialect = \"{name}\"`, which is not a dialect \
                 this tree drives; the names are {}",
                m.name,
                all.iter()
                    .map(|d| d.family())
                    .collect::<Vec<_>>()
                    .join(" and ")
            )
        })
    });
    let matched: Vec<Dialect> = props
        .chat_template
        .as_deref()
        .map(|served| {
            all.iter()
                .copied()
                .filter(|d| letibot_turn::serving::template_matches(d.wiring(None).spec(), served))
                .collect()
        })
        .unwrap_or_default();
    match (matched.as_slice(), asserted) {
        // An unreadable assertion is refused before anything else, with its own
        // sentence: a key nobody can honour must not be stepped over, whatever
        // the template said - the operator wrote it, so it is read or refused.
        (_, Some(Err(why))) => Err(why),
        ([], None) if props.chat_template.is_none() => Err(format!(
            "{} did not report a chat_template, so the dialect cannot be derived \
             from what it renders. Put `dialect = \"glm\"` (or `\"qwen\"`) in \
             [model.\"{}\"] to assert one by hand",
            want.authority(),
            m.name
        )),
        ([], None) => Err(format!(
            "{} reports a chat template {} bytes long that matches no dialect \
             this tree drives ({}), so driving it with either renderer would be a \
             guess about weights it was not written for. Put `dialect = \"glm\"` \
             (or `\"qwen\"`) in [model.\"{}\"] to assert one by hand",
            want.authority(),
            props.chat_template.as_deref().map(str::len).unwrap_or(0),
            all.iter()
                .map(|d| d.family())
                .collect::<Vec<_>>()
                .join(" and "),
            m.name
        )),
        ([], Some(Ok(d))) => Ok(d),
        ([only], None) => Ok(*only),
        ([only], Some(Ok(d))) if *only == d => Ok(d),
        ([only], Some(Ok(d))) => Err(format!(
            "{} reports the {} dialect's own chat template, while [model.\"{}\"] \
             asserts `dialect = \"{}\"`. These disagree, and the server's template \
             is the one it renders with — fix the block (or the server) and switch \
             again. Guessing between them is the silent corruption this check exists \
             to refuse.",
            want.authority(),
            only.family(),
            m.name,
            d.family()
        )),
        // Two byte-identical templates would mean two dialects rendering the same bytes, which is
        // unreachable with the dialects this tree ships; the honest answer if it ever happens is a
        // person's, not a first element the compiler would silently pick.
        (many, _) => Err(format!(
            "{} reports a chat template that matches {} dialects this tree drives \
             ({}), which this code cannot choose between",
            want.authority(),
            many.len(),
            many.iter()
                .map(|d| d.family())
                .collect::<Vec<_>>()
                .join(" and ")
        )),
    }
}

/// A declared url's `host:port`, or `None` when it names no usable address. `config` needs it to tell which declared
/// model a session is on, and it asks the same parser the switch used: comparing url text would make two spellings of
/// one address two different models.
pub fn local_url_authority(url: &str) -> Option<String> {
    parse_local_url(url).ok().map(|e| e.authority())
}

/// `http://192.168.1.78:8082` or `192.168.1.78:8082` into an `Endpoint`: a scheme is accepted and discarded, because
/// the operator writes the url they curl, and so is a path, since the client appends its own route.
pub(crate) fn parse_local_url(url: &str) -> Result<Endpoint, String> {
    let rest = url
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let authority = rest.split('/').next().unwrap_or("").trim();
    if authority.is_empty() {
        return Err(format!("`{url}` names no host"));
    }
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| format!("`{url}` names no port; write it as HOST:PORT"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| format!("`{port}` in `{url}` is not a port"))?;
    if host.is_empty() {
        return Err(format!("`{url}` names no host"));
    }
    Ok(Endpoint::new(host, port))
}

/// Do stored ids mean something else under this vocabulary? Yes when one side is the byte vocabulary and the other is
/// not, or both are byte tables of different literals. Two GGUFs are not decided here, and an empty stored source is a
/// row from before the column was filled, taken to be a GGUF.
pub(crate) fn vocab_differs(stored: &str, current: &str) -> bool {
    let bytes = |s: &str| s.starts_with(letibot_tokencore::BYTES_SOURCE);
    match (bytes(stored), bytes(current)) {
        (false, false) => false,
        (true, true) => stored != current,
        _ => true,
    }
}

/// What is left over when `git worktree add -b <branch>` fails: a branch an earlier child left
/// behind, or a registration whose directory is gone. Said with the commands that clear it; empty
/// when neither is the cause.
pub(crate) fn leftover_hint(workspace: &str, branch: &str, err: &str) -> String {
    let mut hint = String::new();
    let branch_left = git_in(
        workspace,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();
    if branch_left {
        hint.push_str(&format!(
            " The branch `{branch}` already exists — left by an earlier child. If it is landed \
             or abandoned, `git branch -D {branch}` (after `git worktree prune`) frees the name; \
             otherwise use another `slug`."
        ));
    }
    if err.contains("already registered") || err.contains("locked") || err.contains("missing") {
        hint.push_str(
            " A worktree registration whose directory is gone is in the way: `git worktree \
             prune` clears it (`git worktree unlock <path>` first if it is locked).",
        );
    }
    hint
}

/// Tell `key` to session `me` once: once per daemon in `told` and once across restarts in `queue_told`, so a
/// restarted daemon that re-reads a red tip does not tell it again. A store that cannot answer is not a reason to stay
/// silent.
pub(crate) fn told_once(
    told: &mut std::collections::HashSet<String>,
    store: &letibot_tokencore::store::Store,
    me: &str,
    key: String,
) -> bool {
    if !told.insert(key.clone()) {
        return false;
    }
    store
        .tell_once(
            &format!("told:{me}:{key}"),
            crate::mergequeue::gate::now_ms(),
        )
        .unwrap_or(true)
}

/// Does `pc`'s key fail to resolve: the one failure a pasted key fixes?
pub(crate) fn key_is_missing(pc: &crate::config::ProviderConfig) -> bool {
    let Ok(preset) = letibot_provider::Preset::parse(&pc.name) else {
        return false;
    };
    matches!(
        letibot_provider::keys::resolve(preset, pc.api_key.as_deref(), None),
        Err(letibot_provider::keys::KeyError::Missing { .. })
    )
}

/// Would a key saved to providers.toml be the one used? Not when the command line (`--api-key`) or
/// the environment supplies it: both come first in the resolution order, so asking would save a key
/// that is then ignored.
pub(crate) fn key_is_replaceable(pc: &crate::config::ProviderConfig) -> bool {
    if pc.api_key.is_some() {
        return false;
    }
    let Ok(preset) = letibot_provider::Preset::parse(&pc.name) else {
        return false;
    };
    match letibot_provider::keys::resolve(preset, None, None) {
        Ok(c) => !c.from.starts_with('$'),
        Err(_) => true,
    }
}

/// Why a pasted key cannot be saved, or `None` when it can. providers.toml is parsed by hand
/// (`keys::parse_file`): a `#` anywhere starts a comment and quotes are stripped, not escaped, so
/// such a key would be saved and read back as something else.
fn unsavable_key(k: &str) -> Option<&'static str> {
    if k.is_empty() {
        Some("it was empty")
    } else if k.contains('#') || k.contains('"') {
        Some("it contains `#` or `\"`, which providers.toml cannot hold")
    } else if k.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Some("it contains a space or a control character — likely a paste of more than the key")
    } else {
        None
    }
}

/// How long the key card waits for an answer - longer than sudo's 120 s, because this one
/// may send somebody to a provider's website to make a key.
pub(crate) const KEY_PATIENCE: std::time::Duration = std::time::Duration::from_secs(600);

/// Why a session on the byte vocabulary cannot be pointed at a local model.
pub(crate) const NO_VOCAB_FOR_LOCAL: &str = "this daemon has no model vocabulary — it was started for a cloud provider, whose \
     turns need none — and a local llama-server reads token ids computed here. Start a \
     daemon with --vocab <the served model's GGUF> to run on a local model.";

/// Why a turn on a daemon with no model chosen is refused - see `Parts::load`.
pub(crate) const NO_MODEL_CHOSEN: &str = "no model is chosen for this session: this box has no local \
     model's vocabulary (GGUF) and no cloud provider. `/models` picks one — a cloud model asks \
     for its key once, and becomes this box's default.";

/// The sentence a window move appends to the switch's report, with the conversation's size in the
/// units of the window it is compared against: a metered model counts with the provider's, whose
/// figure is smaller than the ledger's by the reasoning rows it is never sent - `wall.rs`'s rule.
fn window_moved(
    cfg: &crate::config::Config,
    was: Option<u64>,
    now: u64,
    ledger_tokens: u64,
) -> String {
    let resident = match cfg.provider.is_some() {
        true => cfg.shown_tokens(ledger_tokens),
        false => ledger_tokens,
    };
    let mut said = match was {
        Some(old) => format!(". Context window {old} → {now}"),
        None => format!(". Context window now {now}"),
    };
    // The consequence, not just the number: being told after the turn that it had to
    // compact is too late, and a move to a SMALLER box is where it bites.
    if resident + cfg.headroom() >= now {
        said.push_str(&format!(
            "; this conversation is {resident} token(s), so the next turn compacts first"
        ));
    }
    said
}

#[cfg(test)]
mod window_moved_tests {
    use super::window_moved;
    use crate::config::Config;

    /// The number in the sentence is the window's own unit. The ledger over-counts a cloud
    /// conversation by the reasoning rows the provider is never sent, so a raw count claimed a
    /// compaction the next turn would not need.
    #[test]
    fn the_conversations_size_is_measured_in_the_windows_own_units() {
        let mut cfg = Config::for_this_box("/nonexistent-workspace");
        cfg.provider = Some(crate::config::ProviderConfig {
            name: "deepseek".into(),
            model: Some("deepseek-chat".into()),
            api_key: None,
            thinking: false,
        });
        // The ratio this tree measures on this box: the ledger over-counts by 49%.
        cfg.ledger_scale = Some((100, 51));
        cfg.context_window = Some(60_000);

        let ledger = 60_000u64;
        let quiet = window_moved(&cfg, None, 60_000, ledger);
        assert!(
            !quiet.contains("compacts first"),
            "30600 tokens plus the headroom is under the window: {quiet}"
        );
        // The control: the same conversation counted in the ledger's units does claim it.
        let mut raw = cfg.clone();
        raw.ledger_scale = None;
        let loud = window_moved(&raw, None, 60_000, ledger);
        assert!(
            loud.contains("this conversation is 60000 token(s)"),
            "an unmeasured count is the ledger's own figure, and that is what over-counted: {loud}"
        );

        // And when it does claim, the sentence names the window's unit too.
        let over = window_moved(&cfg, Some(120_000), 30_000, ledger);
        assert!(over.contains("Context window 120000 → 30000"), "{over}");
        assert!(
            over.contains("this conversation is 30600 token(s)"),
            "{over}"
        );
    }
}

#[cfg(test)]
mod vocab_differs_tests {
    use super::vocab_differs;

    #[test]
    fn only_a_move_into_or_out_of_bytes_or_between_byte_tables_differs() {
        let (q, g) = ("bytes:aaaaaaaaaaaa", "bytes:bbbbbbbbbbbb");
        assert!(!vocab_differs(q, q));
        assert!(vocab_differs(q, g), "two byte tables mean different ids");
        assert!(vocab_differs(q, "/m/Qwen.gguf"));
        assert!(vocab_differs("/m/Qwen.gguf", q));
        // Two GGUFs are the operator's binding, refused at the switch, not here.
        assert!(!vocab_differs("/m/Qwen.gguf", "/other/Qwen.gguf"));
        // A row from before the column was filled is a GGUF row.
        assert!(!vocab_differs("", "/m/Qwen.gguf"));
        assert!(vocab_differs("", q));
    }
}
