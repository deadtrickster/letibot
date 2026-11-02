//! **Model switching**: `/models`, a provider swap, a declared local model and the
//! way back — the endpoints, vocabularies and windows a session moves between, and
//! what each switch owes the one before it.
use super::*;

/// **The daemon's own server, exactly as this session's daemon was started against
/// it** — what `/models local` returns to.
///
/// Captured at open, before any switch can move anything, because a switch
/// overwrites the very fields this holds: a session that moved to another fleet
/// model's weights has an `engine` rendering another dialect and a `cfg.endpoint`
/// naming another box, and "back to local" is a RETURN, not a no-op. Before this
/// existed the return was partial in a way nobody could see: the window came back
/// (its own remember-on-first-move) while the address stayed wherever the last
/// fleet switch put it, and the line still said *"turns go to the local server"*.
///
/// The vocabulary and the wiring are held here as `Arc`s — the same ones
/// [`Parts`] opened with — so the return does not reload a tokenizer or rebuild a
/// renderer; it reseats the pair the daemon started with.
#[derive(Clone)]
pub(crate) struct OwnServer {
    pub(crate) endpoint: Endpoint,
    pub(crate) model: String,
    /// The GGUF the daemon started on, or `None` when it started on the byte
    /// vocabulary — the same `Option` `Config::vocab_gguf` carries since main
    /// made the vocabulary optional. `vocab` below holds the thing itself
    /// either way, and its `source()` is the honest name for both.
    pub(crate) vocab_gguf: Option<PathBuf>,
    pub(crate) dialect: crate::dialect::Dialect,
    pub(crate) media_marker: Option<String>,
    pub(crate) sampling: serde_json::Value,
    pub(crate) vocab: Arc<Vocab>,
    pub(crate) wiring: Arc<Wiring>,
}

/// The sentence a superseded request leaves behind — its asker's wait ends with the daemon's
/// own words rather than with the new requester's.
pub(crate) fn superseded(name: &str) -> String {
    format!(
        "the `{name}` confirmation was superseded by a newer request before anybody answered \
         it, so nothing changed; the newer one is on the screen now"
    )
}

/// **A switch waiting for the turn in flight to end** — recorded, said, and applied where the
/// prompt span closes.
///
/// MEASURED 2026-10-26, the operator's session: `/models deepseek/deepseek-flash` typed while a
/// round on `glm-coding/glm-5.3` was failing was applied INSIDE the retry loop, so the round was
/// re-issued on deepseek carrying the rest of GLM's turn — a run of assistant tool-call rounds
/// with no `reasoning_content`, which deepseek's thinking mode refuses (`http 400: The
/// `reasoning_content` in the thinking mode must be passed back to the API`). The operator's
/// sentence for the rule: *"we can switch after turn ends"*.
pub(crate) struct PendingSwitch {
    /// The argument [`Harness::set_provider`] will be given when the turn ends: `Some` is a
    /// metered provider, `None` the daemon's own server.
    choice: Option<crate::config::ProviderConfig>,
    /// The model it lands on, in `/models`' spelling (`deepseek/deepseek-flash`, `local`), read
    /// when it was recorded — so the sentence that says what will happen does not have to build
    /// the provider to name it.
    model: String,
}

/// **The model a `set_provider` argument would land on**, in [`Harness::model_name`]'s spelling
/// (`deepseek/deepseek-flash`, `local`) — so a switch that is still pending can be named, and
/// compared with what answers now, without building a provider for it.
pub(crate) fn model_name_of(choice: &Option<crate::config::ProviderConfig>) -> String {
    match choice {
        None => "local".to_string(),
        Some(pc) => match &pc.model {
            Some(m) => format!("{}/{m}", pc.name),
            None => pc.name.clone(),
        },
    }
}

/// **The sentence a failed switch earns** — apart from [`Harness::switch_back_notice`] so
/// the wording and the command are testable without a session, a store and a provider.
///
/// The command is spelled the way the head's `/models` takes it (`Harness::model_name`),
/// not described: an operator reading *"go back to deepseek"* would have to work out
/// which of five spellings the verb accepts, and the daemon already knows.
pub(crate) fn switch_back_said(previous: &str, now: &str, code: u16) -> String {
    format!(
        "the first round on `{now}` was refused by the provider (http {code}). Nothing this \
         turn produced is lost and the conversation is unchanged: `/models {previous}` takes \
         this session back to `{previous}`, and the next turn goes there."
    )
}

/// **The model a session switched away from**, kept until a round proves the new one
/// answers — the fact that makes a failed switch recoverable by the person who typed it.
///
/// Held in [`Harness::model_name`]'s spelling, which is the one `/models` takes, so the
/// command in the notice is not a second name to translate.
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

    /// **Switch to a model this fleet hosts** — `/models dense78`, `/models local.glm`.
    ///
    /// Declared in `providers.toml` as a `[model."…"]` block with an address; see
    /// [`letibot_provider::keys::LocalModel`]. These are `local` in every sense that
    /// matters here: no key, no meter, an OpenAI-compatible or llama.cpp server on the
    /// LAN. So this takes the local path — `provider` cleared — and moves where that
    /// path points.
    ///
    /// # Why it verifies the vocabulary before it moves
    ///
    /// The engine tokenizes HERE, with this daemon's GGUF, and sends token ids. Point
    /// it at a server holding different weights and nothing fails: the ids are valid
    /// numbers and mean other words. That is silent corruption of the one thing a
    /// transcript is for, so a switch that cannot show the weights match does not
    /// happen — unless it does something better than refuse: load the vocabulary of
    /// the weights the target itself names, seat the dialect the target's own
    /// template settles on, and re-render the conversation into a fork under both.
    ///
    /// Four outcomes, and each is a sentence rather than a shrug:
    ///
    ///   * `/props` names the same GGUF this daemon tokenizes with — the address
    ///     moves, and nothing else does. The engine, the dialect and the
    ///     conversation's tokens all stay.
    ///   * `/props` names OTHER weights whose template matches a dialect this tree
    ///     drives (or the block asserts one) — a full switch: their vocabulary
    ///     loaded from the GGUF they name, their dialect seated, the conversation
    ///     re-rendered into a fork keyed by that dialect's template, the window
    ///     retuned against the box that will answer. `/models local.glm` from a
    ///     qwen daemon is this arm, and it is the arm the operator asked for.
    ///   * the weights differ and no dialect can be named honestly — refused,
    ///     naming what was looked for and the key that asserts it by hand.
    ///   * nothing answers, or `/props` names no model at all (a proxy, a server
    ///     without it) — refused, and `same_vocab = true` in the block asserts by
    ///     hand that the weights match. An operator saying *"I know these are the
    ///     same weights"* is a decision on the record; this guessing it would not be.
    ///
    /// # Why the cross-weights arm forks rather than swapping the renderer under a
    /// live transcript
    ///
    /// Every token in the session's ledger was rendered by one dialect against one
    /// vocabulary. Appending another dialect's bytes to them builds a prompt no
    /// model was trained on — the exact corruption the vocabulary check exists to
    /// refuse, one layer down. So the switch re-renders the conversation whole,
    /// the same way a resume into a daemon of another dialect does, into a fork
    /// whose stable-prefix row is keyed by the NEW dialect's template sha: another
    /// dialect's cache row is never reused, by construction rather than by check.
    pub fn set_local_model(
        &mut self,
        m: &letibot_provider::keys::LocalModel,
    ) -> Result<String, HarnessError> {
        let want = parse_local_url(&m.url)
            .map_err(|e| HarnessError::Setup(format!("[model.\"{}\"] {e}", m.name)))?;
        // **One `/props` probe decides the whole switch** — weights, dialect,
        // marker — before anything moves, so a refusal leaves the session exactly
        // as it was. The daemon's own vocabulary is optional since main made it
        // so (a box with only an API key starts on the byte one), and the
        // decision takes it as an `Option`: with one, behaviour is as it was;
        // without one, nothing can be the SAME weights as a vocabulary that does
        // not exist, and the probe alone settles which weights the switch seats
        // — the target's own, reported by the target.
        let switch = local_switch_decision(&want, m, self.cfg.vocab_gguf.as_deref())
            .map_err(HarnessError::Setup)?;

        // Off any metered provider first, and by its own door, so the ledger scale and
        // the restored window are handled where that is understood. Its report is
        // discarded deliberately: it describes a move to the daemon's own server, which
        // is a waypoint here rather than where this switch lands.
        //
        // **But not its re-seat half, when this switch replaces the engine anyway.**
        // `/models local`'s door returns a session sitting on the daemon's own
        // weights — re-rendered back, if a previous switch moved it — and a
        // conversation forked back only to be forked again onto the target's
        // dialect a moment later pays two cold prefills for one decision. So the
        // same-weights arm takes the full door and the other-weights arm takes
        // the door with its re-seat withheld: the engine this arm is about to
        // discard is not rebuilt first.
        match switch {
            LocalSwitch::SameWeights => {
                // **The UNGATED door, and the one place that matters.** This is a waypoint on the
                // road to a fleet model, not a switch the operator asked for: the gated door would
                // see a prompt in flight and record `local` as a pending switch, and the endpoint
                // would then move under it — half a switch, which is worse than either. A fleet
                // switch cannot arrive inside a turn anyway (`slash::mid_turn_switch_ok` refuses
                // it), and this keeps the two facts from disagreeing if that ever changes.
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

    /// The address move a same-weights switch is: the engine, the dialect and the
    /// conversation all stay, and only where turns are posted changes.
    ///
    /// Split from [`Harness::set_local_model`] so the arm that changes nothing but
    /// the address is readable as exactly that, beside the arm that changes
    /// everything.
    fn move_local_endpoint(
        &mut self,
        want: &Endpoint,
        m: &letibot_provider::keys::LocalModel,
    ) -> Result<String, HarnessError> {
        // **The engine is what actually holds the address** — `cfg.endpoint` is the
        // record and `engine.endpoint` is where `/completion` is posted. Writing one
        // and not the other is how a switch reports a move it did not make.
        self.cfg.endpoint = want.clone();
        self.engine.endpoint = want.clone();
        self.cfg.model = m.model.clone();
        self.engine.model = m.model.clone();
        if !m.profile.sampling.is_empty() {
            let sampling = serde_json::Value::Object(m.profile.sampling.clone());
            self.cfg.sampling = sampling.clone();
            self.engine.sampling = sampling;
        }
        // A different box is a different KV cache and a different window; the ratio
        // measured on the old one is not evidence about this one.
        self.cfg.ledger_scale = None;
        // And a ratio measured on a CLOUD model -- `None` here unless this session came
        // from one -- is not evidence about a local ledger, whose count IS the prompt.
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

    /// **The cross-weights switch** — the arm that makes `/models local.glm` work
    /// from a daemon started on the qwen weights.
    ///
    /// Everything here happens in an order where a failure leaves a session that
    /// still works: the probe and the dialect were decided before this was called,
    /// the vocabulary loads before anything is written, the fork is written before
    /// anything is swapped, and the engine moves only once the fork exists. A
    /// failure at any step is the sentence naming the step.
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
        // The GGUF `/props` named is a path on the box that RUNS the server; a
        // LAN target can name one this box cannot read, and the switch tokenizes
        // HERE. Refused with the path and what to do about it, rather than
        // searched for by basename — a basename hunt that found the wrong file
        // would be this switch's own corruption, committed by its own fix.
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
        // own — which is exactly the daemon this arm is new for.
        let old_gguf = self.engine.vocab().source().to_string();
        let was_tokens = self.session.ledger.len();

        // **The vocabulary of the weights that will answer.** The second
        // vocabulary a process ever loads — the daemon's own came first, at
        // start — and the one a box short of memory cannot afford. It is CPU-only
        // by the shim's own construction (see `csrc/shim.c`), which is what lets
        // a daemon already holding one model's weights load another's tokenizer.
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
        // for the same reason — at runtime a mismatch is silent.
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

        // **The fork's prefix, keyed by the NEW dialect's own template sha.**
        // `stable_prefix.dialect_sha` is the key a resume checks before reusing a
        // rendering, so the row this writes is what makes a switch to another
        // dialect never reuse this one's cache — including by a daemon that
        // resumes this session later.
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

        // **The engine moves first**, because the fork below renders with
        // `self.engine` — and the fork is the first thing that can no longer fail
        // cheaply, so by the time it runs, everything it renders with is already
        // the pair the next turn will use.
        self.engine = engine;
        // **And the counter the notes pane measures with moves with it.** The section is
        // assembled with the engine's own vocabulary, so a pane that went on measuring with
        // the one this session OPENED on would disagree with the prompt about which notes are
        // whole — the one thing the pane exists to show. See `Harness::install_notes_source`,
        // whose closure reads this field.
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

        // **The conversation, re-rendered whole under the new pair.** The items
        // are dialect-neutral (`item_json`); the tokens were a cache of one
        // rendering — the same rationale the resume fork is built on, and the
        // same machinery `reingest` drives: fork, carry every item, summarise
        // nothing.
        let items = self.session.items.clone();
        let outcome = CompactionOutcome {
            turn_id: format!("{}#weights", self.transcript_id),
            summary: String::new(),
            tool_calls: 0,
            truncated: false,
            cached_tokens: 0,
            reusable: 0,
            generated_tokens: 0,
        };
        self.compacting = true;
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

    /// **Point `context_window` at the box that will answer** — the fleet half of
    /// [`Harness::retune_window`], reading the server instead of the catalogue.
    ///
    /// This is not symmetry for its own sake. Measured on this fleet the day the
    /// switch landed: `127.0.0.1:8080` serves the 27B at `n_ctx` 262144 and
    /// `192.168.1.78:8082` serves **the same GGUF** at 57344. Without this the
    /// switch inherited the window this daemon started with, so a session that moved
    /// to .78 planned compaction against 262144 tokens on a server holding 57344 —
    /// and the conversation would run off the end of the KV cache with every local
    /// check saying there was room. The weights matching is exactly what made it
    /// easy to miss: the vocabulary check passes, and the window is the thing that
    /// does not travel with it.
    ///
    /// A server that will not report `n_ctx` leaves the window `None`, which is this
    /// config's word for *no wall* rather than for a large one — the same reading
    /// `cli` takes for a local endpoint that will not say. It is said out loud,
    /// because planning against nothing is a decision the operator should see.
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
        // **A stated window beats the probe**, for the same reason `--context-window`
        // beats `/props` at startup: the operator naming a number is them telling the
        // daemon something it cannot work out, and the case the key exists for is a
        // proxy whose `/props` answers about itself rather than the server behind it.
        let now = stated.or_else(|| letibot_turn::serving::served_ctx(want));
        self.cfg.context_window = now;
        match now {
            Some(w) if Some(w) == was => String::new(),
            Some(w) => {
                let resident = self.session.ledger.len() as u64;
                let mut said = match was {
                    Some(old) => format!(". Context window {old} → {w}"),
                    None => format!(". Context window now {w}"),
                };
                // The consequence, not just the number — `retune_window`'s own rule:
                // being told after the turn that it had to compact is being told
                // too late. A move to a SMALLER box is where this bites, and that is
                // the ordinary direction for a fleet switch.
                if resident + self.cfg.headroom() >= w {
                    said.push_str(&format!(
                        "; this conversation is {resident} token(s), so the next turn \
                         compacts first"
                    ));
                }
                said
            }
            None => format!(
                ". {} did not report an n_ctx, so there is no wall to plan against — \
                 put a window on [model.\"{name}\"] or expect no compaction",
                want.authority()
            ),
        }
    }

    /// **Ask the operator for the key [`Harness::key_wanted`] names, and make it the
    /// session's provider.**
    ///
    /// The masked card is the sudo password's: [`letibot_sessionlog::hub::Hub::request_secret`]
    /// raises `SecretRequested` to every head and the answer comes back on a channel, never
    /// in an event — so the key reaches providers.toml and nowhere else. The worker blocks
    /// while it waits, which is safe for the same reason it is for sudo: the answer is
    /// handled on the answering head's own reader thread.
    ///
    /// Refuses at once, rather than after a wait, when no head that answers decisions is
    /// attached (a one-shot `--prompt`, say): the sentence then names the variable and the
    /// file instead. A key that providers.toml could not hold is asked for again, saying
    /// why, up to three times. `why` is the reason for asking again — a 401's own words.
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
            // **The first key on a box also chooses the default**, so the next `letibot`
            // starts here without a flag. Only when there is none: an existing choice is the
            // operator's and is not this card's to move.
            let mut made_default = false;
            if let Ok(None) = letibot_provider::keys::default_choice(Some(&file)) {
                made_default =
                    letibot_provider::keys::set_default(Some(&file), &pc.name, pc.model.as_deref())
                        .is_ok();
            }
            self.key_wanted = None;
            self.set_provider(Some(pc.clone()))?;
            self.hub.publish(SessionEvent::Warning {
                code: "provider_key_saved".into(),
                detail: format!(
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
                compaction: None,
            });
            return Ok(());
        }
        Err(HarnessError::Setup(format!(
            "no usable key for `{}` after three tries: {}",
            pc.name,
            reason.unwrap_or_default()
        )))
    }

    /// **Switch what answers this session's turns**, underneath the conversation.
    /// `None` is the local server. The transcript, the ledger and the tools are
    /// untouched: the next turn simply goes elsewhere. Returns a line saying what
    /// answers now; a provider whose key cannot be found is refused and nothing
    /// changes.
    ///
    /// # A switch never lands inside a turn
    ///
    /// **While a prompt is in flight the request is PENDING**, and this returns the
    /// sentence that says when it will take: it is recorded ([`Harness::pending_switch`]),
    /// said on the hub (`model_switch_pending`), and applied where the prompt span closes
    /// ([`Harness::apply_pending_switch`], from [`Harness::end_turn_clock`]).
    ///
    /// The measurement is the whole of the rule. MEASURED 2026-10-26, the operator's
    /// session: a `/models deepseek/deepseek-flash` typed while a round on `glm-coding` was
    /// failing was applied inside the retry loop, so the round was re-issued on deepseek
    /// carrying the rest of GLM's turn — a run of assistant tool-call rounds with no
    /// `reasoning_content`, which deepseek's thinking mode refuses
    /// (`http 400: The reasoning_content in the thinking mode must be passed back to the
    /// API`). A model's first request must follow a USER message, so the turn that was
    /// running on the old model has to end before the new one speaks. Their words for the
    /// rule: *"we can switch after turn ends"*.
    ///
    /// **Two things are not a switch and are not deferred.** A provider built for the
    /// model the session is ALREADY on is a rebuild (`Harness::obtain_key` renews a
    /// refused key through this very door, and a deferred rebuild would retry the round
    /// with the key the provider just refused), so the target name is compared against
    /// [`Harness::model_name`] first and an unchanged name goes straight through.
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

    /// **Record a switch for the end of this turn, and say so** — the pending half of
    /// [`Harness::set_provider`].
    ///
    /// The sentence is the return value (so the verb's reply carries it) *and* a
    /// `model_switch_pending` warning (so a head that is not the one that typed it — and the
    /// one that is, once the reply has scrolled — has it on the status line rather than only in
    /// the scrollback).
    ///
    /// **It does not build the provider.** A switch that would be refused — a key that does not
    /// resolve — is refused when it is applied, and `model_switch_not_applied` is what says so
    /// then. Building it here would either refuse a request the turn cannot act on yet or build
    /// one twice.
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
        self.hub.publish(SessionEvent::Warning {
            code: "model_switch_pending".into(),
            detail: said.clone(),
            compaction: None,
        });
        said
    }

    /// **The switch itself**, with no question about the turn span — [`Harness::set_provider`]
    /// is the door everything asks through, and this is what it calls once nothing is in flight.
    ///
    /// `end_turn_clock` is the one caller that must not go through the gate (the span has just
    /// closed, and it is applying what the gate recorded).
    fn set_provider_now(
        &mut self,
        choice: Option<crate::config::ProviderConfig>,
    ) -> Result<String, HarnessError> {
        // **A switch clears the ratio, in BOTH directions** — ruled 2026-09-24, and the
        // reason is this document's own: **a measurement made on one model is not evidence
        // about another.** `ledger_scale` is `(ledger tokens, the model's prompt tokens)` for
        // one prompt on one model, and carrying it across a switch presents a stale
        // measurement as a current one.
        //
        // **Not re-derived.** There is nothing to re-derive it FROM at the moment of the
        // switch — the first round on the new model is what produces it — so "re-derive"
        // would mean inventing a ratio, which is a guess wearing a number's clothes. Cleared,
        // and the first round sets it; until then the unscaled count stands, which
        // `Config::planning_window` already returns when the scale is `None`, and
        // `Config::tokens_are_unscaled` is how a reader says so rather than treating it as
        // calibrated.
        //
        // **The old pair is KEPT, as an estimate that names the model it was measured
        // on** — the half of this that used to be wrong. Clearing is right: the first
        // round on the new model's tokenizer is what measures it, and until then nothing
        // about this model has been measured. What clearing must not do is turn the
        // ledger's own bytes into the number compared with the new model's window:
        // MEASURED 2026-10-25, a session switched to `glm-coding` stopped at round 0 with
        // `2708271 of 1000000 ledger tokens (unscaled: no round on glm-coding has measured
        // yet)` and never sent a byte. See `Config::CarriedScale` for the whole of it, and
        // `Config::carried_note` for how every line that prints a converted count says
        // whose measurement it is.
        //
        // **The clear is inside each arm and not before the `match`**, because this method
        // promises that a switch which cannot be built changes nothing — and a scale dropped
        // for a provider whose key turned out to be missing would be a change made by a
        // refusal. `carry_the_scale` is called after the provider that can fail is built,
        // on the same line as the clear it is the other half of.
        match choice {
            // The daemon's own server takes token ids, and a byte-vocabulary session has
            // none it could read: refused before anything changes, as the promise above
            // says. (main's guard, kept: a daemon with no vocabulary of its own has
            // nothing `/models local` could return to. The guard and the return below
            // read the same fact — the vocabulary the engine tokenizes with NOW, which
            // a cross-weights switch may have replaced since the daemon started.)
            None if self.engine.vocab().is_bytes() => {
                Err(HarnessError::Setup(NO_VOCAB_FOR_LOCAL.to_string()))
            }
            // **`/models local` is a RETURN** — to the daemon's own weights, address,
            // sampling, marker, window and vocabulary, re-seated — and not a provider
            // clear. `set_own_server` is this arm; see its doc for the re-seat half.
            None => self.set_own_server(true),
            Some(pc) => {
                let p = build_provider(&pc, &self.cfg.sampling, provider_notes(&self.hub))
                    .map_err(HarnessError::Setup)?;
                let mut line = format!(
                    "turns go to {}/{} from the next one on — METERED; the prefix check is \
                     skipped, the ledger stays the record",
                    p.name(),
                    p.model()
                );
                // **The window follows the model.** Compaction is planned against
                // `context_window` — `plan_overrun`, `should_compact` and
                // `headroom` all read it — and it came from the LOCAL server's
                // `/props`. Leaving it there meant a conversation answered by a
                // cloud model was measured against the llama-server's window, and
                // compacted at the wrong time or not at all.
                line.push_str(&self.retune_window(&pc));
                // After everything that can fail, and before anything reads it.
                self.note_the_way_back();
                self.carry_the_scale();
                self.cfg.ledger_scale = None;
                self.provider = Some(p);
                self.cfg.provider = Some(pc);
                // **Say it, or the head goes on drawing the old name.** The
                // header reads the model from the daemon's word, and the daemon's
                // word was sent once at attach: the operator switched leticl to
                // deepseek and the top row still said qwen, indefinitely. The
                // `model` settings row carries what answers now, and this is what
                // makes it carry it.
                self.publish_settings();
                Ok(line)
            }
        }
    }

    /// **Keep the ratio a switch is about to clear, named after the model that measured
    /// it** — the other half of the clear in [`Harness::set_provider`], and see
    /// `Config::CarriedScale` for why the pair is kept at all.
    ///
    /// Called only where the switch SUCCEEDED: a switch refused for a missing key changes
    /// nothing, and a scale moved by a refusal would be a change made by nothing.
    ///
    /// **A second switch with nothing measured in between keeps the older pair rather
    /// than overwriting it with `None`** — `deepseek → glm-coding → grok` is one session
    /// whose only measurement was deepseek's, and that is still the best estimate there is
    /// for either of the others.
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

    /// **Remember the model this switch is leaving, so a refusal can name the way back.**
    ///
    /// Read BEFORE anything moves, and only where the move cannot fail after it — see
    /// [`Harness::switch_back_notice`] for what reads it.
    fn note_the_way_back(&mut self) {
        self.switch_back = Some(SwitchBack {
            previous: self.model_name(),
        });
    }

    /// **A switch that fails says how to go back** — the notice a provider's refusal earns
    /// on the first round after a model switch, or `None` when this is not that.
    ///
    /// MEASURED 2026-10-25, `s-1791394076177451000`: a long session was switched to
    /// `glm-coding`, and every request it sent came back
    /// `http 400: messages.content.type is invalid, allowed values: ['text']`. The screen
    /// said `FAILED — http 400: …` and stopped there; the operator had to remember what
    /// the session had been on and type it back by hand, and the daemon knew both the name
    /// and the command. That is the whole of this function.
    ///
    /// **4xx only**, because that is the class that refuses *what was sent*: a 429 is the
    /// ladder's (it waits, and may fall back), a 5xx is an endpoint that is unwell, and
    /// neither is evidence about the model. A transport failure says nothing about it
    /// either.
    ///
    /// The armed state is TAKEN, so one switch earns one notice: a second failure is a
    /// session that has a problem the way back does not fix, and repeating the advice
    /// every turn is how a warning becomes furniture.
    pub fn switch_back_notice(&mut self, e: &HarnessError) -> Option<String> {
        let back = self.switch_back.take()?;
        let code = provider_refusal(e)?;
        let now = self.model_name();
        if now == back.previous {
            // The round went to the model this session already was on — a switch that
            // landed on the name in force is not a switch, and there is nowhere back.
            return None;
        }
        Some(switch_back_said(&back.previous, &now, code))
    }

    /// **The whole of `/models NAME`, in one place, for both callers.**
    ///
    /// There are two: the worker's arm (`Sessions::slash`, between turns, which is where the
    /// verb has always been answered) and the retry loop's poll of the queue
    /// ([`Harness::take_queued_model`], which is the operator's own line answered DURING the turn
    /// they typed it in rather than after it). Two copies of a switch would be two places for the
    /// screen, the session row and the thing that actually answers to disagree — which is the
    /// defect [`Harness::persist_provider_choice`] was written for, one door along.
    ///
    /// **A switch asked for while a prompt is in flight is recorded and NOT applied** — see
    /// [`Harness::set_provider`]. The reply still comes back `ok`, and it says when the switch
    /// takes (*"turns go to X when this turn ends"*), which is the honest answer: it was accepted,
    /// and it is not in force yet.
    ///
    /// **The choice is resolved by the caller, not here.** `models_choice` needs no
    /// harness, and the worker's path must still carry the resolver's own notes when
    /// the session it names is not open — so what this takes is the resolved choice
    /// and the lines it came with.
    pub fn apply_model_choice(
        &mut self,
        choice: crate::slash::ModelChoice,
        mut lines: Vec<String>,
    ) -> crate::slash::SlashReply {
        // **What answers before, so the row is written only when it moved.** A pending switch
        // leaves this name alone, and a session row written with the OTHER model's name would
        // make a restart resume on a model this daemon never ran.
        let was = self.model_name();
        let applied = match choice {
            crate::slash::ModelChoice::OwnServer => self.set_provider(None),
            crate::slash::ModelChoice::Metered(pc) => self.set_provider(Some(pc)),
            // Its own door, because it verifies the vocabulary before it moves
            // anything — see `Harness::set_local_model`.
            crate::slash::ModelChoice::Local(m) => self.set_local_model(&m),
        };
        match applied {
            Ok(line) => {
                // **The choice goes to the session row in the same breath** —
                // `persist_provider_choice`'s own doc is why. A switch that reached
                // the screen but not the row was a choice that expired with the
                // process, which is the defect this whole change is. A pending one is written
                // where it is applied instead ([`Harness::apply_pending_switch`]).
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

    /// **Back to the daemon's own server** — the `None` arm of [`Harness::set_provider`].
    ///
    /// `reseat_foreign` is the one honest wrinkle: `/models local` means *this
    /// daemon's own server, exactly as it was started*, so a session sitting on
    /// another fleet model's WEIGHTS comes back to the daemon's vocabulary and
    /// dialect here, conversation re-rendered into a fork under them. But a switch
    /// straight from one fleet model's weights to another's calls this only to get
    /// off a metered provider first — its engine is about to be replaced, and a
    /// conversation forked back to the daemon's dialect only to be forked again a
    /// moment later pays two cold prefills for one decision — so that caller passes
    /// `false` and takes the field clears without the re-seat.
    fn set_own_server(&mut self, reseat_foreign: bool) -> Result<String, HarnessError> {
        self.note_the_way_back();
        self.provider = None;
        self.cfg.ledger_scale = None;
        // **And the carried pair goes with it**: coming home means the ledger IS the
        // prompt again (the engine sends its own token ids), so a ratio measured by a
        // cloud model would convert a count that needs no conversion. See
        // `Config::scale`, which would ignore it anyway — cleared here so that the only
        // state a reader has to reason about is the one that applies.
        self.cfg.carried_scale = None;
        self.cfg.provider = None;
        // **The return itself**: address, alias, sampling, marker, vocabulary,
        // dialect — every field a fleet switch overwrote, given back the value
        // the daemon started with. The window is restored from its own memory
        // below, the same way it always was.
        //
        // Before this was a return it was half of one: the address stayed
        // wherever the last fleet switch put it while the line still said "the
        // local server", and the operator's own file documents `local` as
        // *(the bare word) is the daemon's own model*. A line that names the
        // wrong box is the provenance defect in report form.
        let own = self.own_server.clone();
        let mut reseat = String::new();
        let foreign = self.on_foreign_weights();
        if foreign && reseat_foreign {
            // The mirror of the switch's cross-weights arm, aimed home: the
            // daemon's own pair, the conversation re-rendered under it, a fork
            // keyed by the daemon's own template sha. It can only fail where the
            // switch can fail, and for the same reasons. What moved is said in
            // the values AS THEY STOOD — the re-seat overwrites them, so they are
            // read before it runs.
            // The names in the line are the vocabularies' own `source()`s — a
            // GGUF's path, or `bytes:<hash>` for a daemon that started on the
            // byte vocabulary — read before the re-seat overwrites them.
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
        // Back to the server's own window, which is the one its `/props`
        // reported at startup. Restored rather than recomputed: the local
        // endpoint is not asked again here, and keeping a cloud model's
        // window over a local conversation is the same bug pointing the
        // other way.
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

    /// **Is this session on weights that are not the daemon's own?**
    ///
    /// The one question that decides whether a return to the daemon's own server is an
    /// address move or a re-seat: [`Harness::set_own_server`] re-renders the conversation
    /// into a fork when the vocabulary or the dialect has been moved by a fleet switch,
    /// and only then.
    ///
    /// Named rather than inlined because it now has two readers, and they are asking
    /// different questions of the same fact: `set_own_server` is deciding whether to
    /// fork, and `slash::mid_turn_switch_ok` is deciding whether a RUNNING TURN may
    /// carry a `/models local` out itself — which it may exactly when the return is the
    /// cheap arm. Two copies of this expression would be two answers to that.
    pub(crate) fn on_foreign_weights(&self) -> bool {
        self.cfg.vocab_gguf != self.own_server.vocab_gguf
            || self.cfg.dialect != self.own_server.dialect
    }

    /// The re-seat half of a return to the daemon's own weights: the mirror of
    /// [`Harness::set_local_model_weights`], aimed at the [`OwnServer`] pair.
    ///
    /// Everything is the same shape and for the same reasons — the engine is
    /// rebuilt from the daemon's own vocabulary and wiring (no load, they are the
    /// `Arc`s open captured), the fork's prefix row is keyed by the daemon's own
    /// template sha, the conversation is re-rendered whole and nothing is
    /// summarised. The one thing that differs is where the facts come from: not a
    /// probe, the record.
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
        };
        self.compacting = true;
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
        // The engine moved first, so a fork failure here would leave the session
        // rendering the own dialect over a foreign-rendered ledger — the
        // corruption this whole file refuses. The fork cannot fail halfway
        // (`fork_to_summary` writes every store row before it swaps), so this
        // names the state rather than papering over it.
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

    /// Point `context_window` at the model that will answer, and say what moved.
    ///
    /// Returns the sentence to append to the switch's report, empty when nothing
    /// changed. An unknown model leaves the window alone **and says so** — the
    /// silent half of this bug was that nobody could tell which window was in
    /// force.
    fn retune_window(&mut self, pc: &crate::config::ProviderConfig) -> String {
        // Remembered on the way out to the first provider, so `/models local`
        // restores the server's own number rather than keeping a cloud model's.
        if self.local_window.is_none() {
            self.local_window = Some(self.cfg.context_window);
            // **And mirrored to the runner**, which needs it for a `local` child spawned
            // while THIS session is on a provider. One write site, so the two cannot
            // remember different windows.
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
                let resident = self.session.ledger.len() as u64;
                let mut said = match was {
                    Some(old) => format!(". Context window {old} → {w}"),
                    None => format!(". Context window now {w}"),
                };
                // The consequence, not just the number: a switch that puts the
                // conversation over the new model's wall compacts on the next
                // turn, and being told afterwards is being told too late.
                if resident + self.cfg.headroom() >= w {
                    said.push_str(&format!(
                        "; this conversation is {resident} token(s), so the next turn \
                         compacts first"
                    ));
                }
                said
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

    /// **This session's model, spelled the way `/models` takes it** — `local`, `name`,
    /// or `name/model`.
    ///
    /// Two readers, and both are asking *which model is this session on* rather than
    /// *what is answering it*: the session row ([`Harness::persist_provider_choice`],
    /// where a restart reads it back through the same door) and the retry loop's
    /// fallback, which must not spend a name on the model that just refused the round.
    /// [`Harness::provider_line`] is the other question — the one a person reads, which
    /// names the endpoint and says METERED — and the two are deliberately not merged.
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
        // The watchers first: their threads hold weak handles and re-check this
        // flag at most one wait chunk after close, so a session that closes does
        // not keep its host — or its cgroups — alive behind a blocked thread.
        if let Some(w) = &self.job_watch {
            w.stop();
        }
        self.runtime.backend.close()
    }

    /// **The switch that was waiting for the turn to end, taken** — the other half of
    /// [`Harness::set_provider`]'s gate.
    ///
    /// `None` is the ordinary case and costs nothing. A failure — a key that no longer resolves,
    /// a provider that will not build — leaves the session on the model it is on, which is a
    /// working session, and says so under its own code: a request the operator made that did not
    /// take is theirs to correct, not a fault of the session's.
    pub(crate) fn apply_pending_switch(&mut self) {
        let Some(pending) = self.pending_switch.take() else {
            return;
        };
        match self.set_provider_now(pending.choice) {
            Ok(line) => {
                // **The session row follows it**, the same as a switch made between turns
                // (`Harness::apply_model_choice`): the row is what a restart resumes on, and a
                // deferred switch that only reached this process would expire with it.
                if let Err(why) = self.persist_provider_choice() {
                    self.hub.publish(SessionEvent::Warning {
                        code: "model_switch_applied".into(),
                        detail: format!("{line}. {why}"),
                        compaction: None,
                    });
                    return;
                }
                self.hub.publish(SessionEvent::Warning {
                    code: "model_switch_applied".into(),
                    detail: format!("the turn has ended, so the switch stands: {line}"),
                    compaction: None,
                });
            }
            Err(e) => {
                self.hub.publish(SessionEvent::Warning {
                    code: "model_switch_not_applied".into(),
                    detail: format!(
                        "the switch to `{}` was waiting for this turn to end and could not be \
                         made: {e}. This session stays on `{}`.",
                        pending.model,
                        self.model_name()
                    ),
                    compaction: None,
                });
            }
        }
    }

    /// **THE SWITCH, ON THE SESSION ROW** — so a restart brings the session back on
    /// what it was switched to rather than on the daemon's own default.
    ///
    /// Called from the `/models` arm the moment the in-memory switch has taken, the
    /// same place `publish_settings` is: the two are one act's two halves, and a
    /// switch that reached the screen but not the row was a choice that expired with
    /// the process. `local` is stored BY NAME rather than left NULL, because the two
    /// answer different questions at a resume — *no opinion* against *the local
    /// server, deliberately* — and a daemon whose own default is a provider would
    /// otherwise hand a switched-back session straight back to it.
    pub fn persist_provider_choice(&self) -> std::result::Result<(), String> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let stored = self.model_name();
        store
            .set_provider_choice(&self.cfg.session_id, Some(&stored))
            .map_err(|e| format!("the choice was not written to the session row: {e}"))
    }

    /// **A RESUME RESTORES THE SESSION'S OWN MODEL**, from the row the switch wrote.
    ///
    /// The defect, in one sentence: a session switched to a cloud provider came back
    /// on the CLI default after a restart, because `set_provider` was in-memory and
    /// the session row had nowhere to record the choice. Called at both open paths —
    /// the first session a `--continue` resumes and the lazy `harness()` a head's
    /// switch reaches — before any turn runs.
    ///
    /// Three spellings of the row, three behaviours, and the distinction is the
    /// point:
    ///
    /// * `None` — never switched, so the daemon's own default stands, which is what
    ///   such a session always did;
    /// * `local` — the deliberate switch back, restored as an ACT (`set_provider(None)`)
    ///   rather than skipped, because a daemon whose default is a provider would
    ///   otherwise hand the session straight back to it;
    /// * `name` or `name/model` — resolved through the SAME door the switch went
    ///   through (`models_choice`), so a restore cannot build a provider the verb
    ///   would refuse. A preset this build no longer knows fails the restore and the
    ///   daemon default stands — said rather than silent, and the row is left alone so
    ///   a build that does know the preset still finds it.
    ///
    /// Returns the sentence a resume should say, or `None` when there was nothing to
    /// restore.
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
            // **A fleet model is restored through its own door**, which re-checks the
            // vocabulary. That check is not ceremony on a resume: the daemon being
            // resumed INTO may have been started on different weights than the one
            // that stored the row, and a restore that skipped it would be the one
            // path into exactly the corruption `set_local_model` exists to refuse.
            Ok((crate::slash::ModelChoice::Local(m), _)) => match self.set_local_model(&m) {
                Ok(_) => Some(format!("restored to {stored}")),
                Err(e) => Some(format!("could not restore {stored}: {e}")),
            },
            // `ModelChoice::Local` is `models_choice("local")`, and `local` is handled
            // by name above — a row that reached here as local would mean the store
            // spelled a switch to the local server some other way, and the honest
            // answer is the daemon default rather than a panic on a fact nobody wrote.
            Ok((crate::slash::ModelChoice::OwnServer, _)) | Err(_) => Some(format!(
                "could not restore {stored}: this build does not know that provider, so the \
                 daemon's own default answers"
            )),
        }
    }
}

/// **What one `/props` probe says about switching to a declared local model** —
/// the decision, or a refusal that names the fact it is missing.
///
/// The free function both doors need, so their answers cannot drift:
/// [`Harness::set_local_model`] for a session switching, and the spawn path for a
/// child seated on a declared local model. Its predecessor answered one question
/// (*are these the weights we tokenize with?*) with a basename comparison and a
/// refusal; the switch this tree owes the operator asks three, and each has its
/// own honest "cannot tell" rather than a default:
///
///   * **which weights answer there** — `/props`'s `model_path`, the vocabulary's
///     one source;
///   * **which template they render** — `/props`'s `chat_template`, the dialect's
///     one source, because what the target SAYS it renders with cannot drift from
///     what it actually renders the way a table of weight names can;
///   * **what to do when they are other weights** — this used to be "refuse, and
///     let the operator start another daemon". Now it is a switch: the target's
///     own vocabulary, loaded from the GGUF it names, and the dialect its
///     template (or the block's hand-asserted `dialect =` key) settles on.
///
/// # The three refusals that are not this function's business
///
/// Loading the vocabulary and resolving the dialect against it happen in the
/// caller, where a failure can leave the caller's state untouched. This function
/// only decides; it probes nothing twice and mutates nothing.
///
/// # Why `same_vocab = true` is read here and not interpreted here
///
/// It is the operator's assertion that the target serves the weights this
/// session's vocabulary already tokenizes for — the same kind of decision on the
/// record `dialect =` is. It short-circuits the weights question because the
/// person who wrote it answered it; it does NOT assert anything about the
/// dialect, which is why the same-weights arm keeps the dialect this session
/// already speaks. A `same_vocab` that also smuggled a dialect change would be a
/// key that guesses, and a guess is the one thing this switch refuses to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalSwitch {
    /// The target serves the weights this session's vocabulary already tokenizes
    /// for — measured by basename, or asserted with `same_vocab = true`. The
    /// engine, dialect and vocabulary all stay; only the address moves.
    ///
    /// A session with no vocabulary of its own (the byte vocabulary a cloud-only
    /// daemon starts on) can never take this arm: there are no weights to be the
    /// same as, so such a switch always seats the target's own.
    SameWeights,
    /// **The target serves other weights**: the switch must load their vocabulary
    /// and seat their dialect, and the conversation must be re-rendered into a
    /// fork before the first turn on them.
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

/// Decide a switch to a declared local model from ONE `/props` probe.
///
/// Every refusal is a sentence naming the endpoint, what it looked for, and the
/// key that asserts the fact by hand — because "it refused" names nothing and
/// this is the sentence an operator reads at a picker.
///
/// The daemon's own vocabulary is an `Option`, because main made it one: `None`
/// is the byte vocabulary, and there the decision changes shape rather than
/// refusing wholesale — nothing can be `SameWeights`, `same_vocab = true` has
/// no vocabulary to assert against, and the probe alone settles that the switch
/// seats the weights the target itself reports, with their vocabulary and
/// dialect. The refusals that remain name the GGUF the target reported.
pub fn local_switch_decision(
    want: &letibot_turn::http::Endpoint,
    m: &letibot_provider::keys::LocalModel,
    vocab: Option<&std::path::Path>,
) -> Result<LocalSwitch, String> {
    // `same_vocab` is not sampling, so the profile reader files it under
    // `unknown` — which is where a key it does not interpret belongs. Read
    // here, where it means something (see the enum's doc for what it does
    // and does not assert).
    //
    // **But only where there is a vocabulary to be the same as.** A daemon
    // started for a cloud provider has none — the byte vocabulary — and there
    // the assertion cannot be true: the target's weights differ from a
    // vocabulary that does not exist, so NOTHING can be `SameWeights` and the
    // probe below decides alone, whatever the block asserts. The switch that
    // comes out of it is the honest one for that box anyway: the target's own
    // reported weights, with their vocabulary loaded and their dialect seated.
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
        // Same weights carry the same template, so the dialect this session
        // speaks is the dialect they render: nothing to re-derive, nothing to
        // re-render, and the switch is the address move it always was.
        // (Unreachable with no vocabulary of our own: a basename cannot match
        // one that does not exist.)
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
                // seated — so the refusal names it, which is what is true.
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

/// **Which dialect does this target render?** — from its own template when it
/// says, from the block's assertion when it cannot, refused when neither speaks.
///
/// The order is the whole decision, and it is not a priority contest:
///
///   * the **served template** matching a dialect this tree drives is the one
///     source that cannot drift from what the server actually renders, so it
///     settles the question outright;
///   * the **asserted `dialect =`** exists for the cases where that source is not
///     available honestly — a server too old to report a template, or one whose
///     template matches nothing this tree drives;
///   * a served template that matches NEITHER dialect with no assertion is a
///     refusal, because the alternative is guessing a dialect for weights it was
///     not written for — valid ids meaning other words, silently;
///   * an assertion that CONTRADICTS a matching template is a refusal too, for
///     the same reason in the other direction: the server's own template is the
///     thing it renders with, and a block that disagrees with it is a mistake
///     somewhere that a person should fix rather than a tie this code breaks.
fn dialect_for_target(
    props: &letibot_turn::serving::ServedProps,
    m: &letibot_provider::keys::LocalModel,
    want: &letibot_turn::http::Endpoint,
) -> Result<crate::dialect::Dialect, String> {
    use crate::dialect::Dialect;
    let all = [Dialect::Glm, Dialect::Qwen];
    // An asserted value that names no dialect is refused rather than dropped:
    // honouring an assertion nobody can read is the regression the profile
    // reader's own test names, one layer down.
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
        // the template said — the operator wrote it, so it is read or refused.
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
        // Two byte-identical templates would mean two dialects rendering the
        // same bytes; unreachable with the dialects this tree ships, and the
        // honest answer if it ever happens is a person's, not a first-element
        // pick the compiler would silently make of it.
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

/// `http://192.168.1.78:8082` or `192.168.1.78:8082` into an [`Endpoint`].
///
/// A scheme is accepted and discarded rather than refused: the operator writes the
/// url they `curl`, and `[model."x"] url = "http://…"` is the shape that reads
/// naturally beside a key block's `url`. A path is accepted and discarded too —
/// `/v1/chat/completions` is what the curl carries and the client appends its own
/// route, so keeping it would produce `/v1/chat/completions/completion`.
/// **A declared url's `host:port`**, or `None` when it names no usable address.
///
/// `config` needs it to tell which declared model a session is actually on, and it
/// must ask the same parser the switch used: comparing the url text would make
/// `http://192.168.1.78:8082` and `192.168.1.78:8082` two different models, and the
/// operator may have written either.
pub fn local_url_authority(url: &str) -> Option<String> {
    parse_local_url(url).ok().map(|e| e.authority())
}

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

/// The GGUF's own file name, which is what two servers holding one model agree on
/// even when the directories differ. A split GGUF's `-00001-of-00006` suffix is kept:
/// two servers that disagree about the shard count are not serving the same file.
/// **Do stored ids mean something else under this vocabulary?** Yes when one side is the
/// byte vocabulary and the other is not, or both are byte tables of different literals.
/// Two GGUFs: not decided here (see the resume that calls this). An empty stored source is
/// a row from before the column was filled, and is taken to be a GGUF — which every such
/// row is.
pub(crate) fn vocab_differs(stored: &str, current: &str) -> bool {
    let bytes = |s: &str| s.starts_with(letibot_tokencore::BYTES_SOURCE);
    match (bytes(stored), bytes(current)) {
        (false, false) => false,
        (true, true) => stored != current,
        _ => true,
    }
}

/// **What is left over when `git worktree add -b <branch>` fails** (TODO R92.17) — a branch an
/// earlier child left behind, or a registration whose directory is gone (locked, or not yet
/// pruned). Said with the commands that clear it; empty when neither is the cause.
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

/// **Tell `key` to session `me` once** — once per daemon in `told` (the session's own memory),
/// and once across restarts in the store's `queue_told` (`told:<session>:<key>`, R92.7): a
/// restarted daemon that re-reads a red tip or a parked row does not tell it again, and so a ring
/// about it wakes nobody to nothing. A store that cannot answer is not a reason to stay silent.
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

/// **Does `pc`'s key fail to resolve** — the one failure a pasted key fixes?
pub(crate) fn key_is_missing(pc: &crate::config::ProviderConfig) -> bool {
    let Ok(preset) = letibot_provider::Preset::parse(&pc.name) else {
        return false;
    };
    matches!(
        letibot_provider::keys::resolve(preset, pc.api_key.as_deref(), None),
        Err(letibot_provider::keys::KeyError::Missing { .. })
    )
}

/// **Would a key saved to providers.toml be the one used?** Not when the command line
/// (`--api-key`) or the environment (`$DEEPSEEK_API_KEY`…) supplies it: both come first in
/// the resolution order, so asking would save a key that is then ignored.
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

/// **Why a pasted key cannot be saved**, or `None` when it can. providers.toml is parsed by
/// hand (`keys::parse_file`): a `#` anywhere starts a comment and quotes are stripped, not
/// escaped — so such a key would be saved and read back as something else.
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

/// How long the key card waits for an answer — longer than sudo's 120 s, because this one
/// may send somebody to a provider's website to make a key.
pub(crate) const KEY_PATIENCE: std::time::Duration = std::time::Duration::from_secs(600);

/// Why a session on the byte vocabulary cannot be pointed at a local model.
pub(crate) const NO_VOCAB_FOR_LOCAL: &str = "this daemon has no model vocabulary — it was started for a cloud provider, whose \
     turns need none — and a local llama-server reads token ids computed here. Start a \
     daemon with --vocab <the served model's GGUF> to run on a local model.";

/// Why a turn on a daemon with no model chosen is refused — see `Parts::load`.
pub(crate) const NO_MODEL_CHOSEN: &str = "no model is chosen for this session: this box has no local \
     model's vocabulary (GGUF) and no cloud provider. `/models` picks one — a cloud model asks \
     for its key once, and becomes this box's default.";
