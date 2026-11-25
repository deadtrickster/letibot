//! A `limit_decide` layer decides a rate limit in place of the fixed rules. The layer's
//! opaque state is kept per episode, keyed by the limited model, in a file beside the session's
//! logs so a restart does not lose it, and destroyed on the first request that succeeds.

use std::collections::BTreeMap;

use letibot_plugin::limit;
use serde_json::Value;

use super::limit::AfterTheLadder;
use super::*;

/// The episodes' file, in the session's logs directory.
pub(crate) const STATE_FILE: &str = "limit-hook.json";

/// One limit episode: what the layer last returned and how far it has gone.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Episode {
    #[serde(default)]
    pub state: Value,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered: Option<limit::Answered>,
    /// The hook failed once in this episode: the fixed rules decide the rest of it, so a broken
    /// layer is never asked in a loop.
    #[serde(default)]
    pub fell_back: bool,
}

/// Episodes by the limited model, in `/models` spelling.
pub(crate) type Episodes = BTreeMap<String, Episode>;

/// The episodes a previous daemon left for this session. A file that does not parse is dropped:
/// the state is the layer's, and an unreadable one starts the episode fresh.
pub(crate) fn load(cfg: &Config) -> Episodes {
    std::fs::read(cfg.daemon_logs().join(STATE_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

impl Harness {
    /// The state the limit hook holds for `model`'s episode, if one is open.
    pub fn limit_hook_state(&self, model: &str) -> Option<&Value> {
        self.limit_episodes.get(model).map(|e| &e.state)
    }

    /// The cell `ask_user_question` reaches a person through; the limit hook's `ask_user` uses it.
    pub fn question_slot(&self) -> QuestionSlot {
        self.question_slot.clone()
    }

    fn save_limit_episodes(&self) {
        let path = self.cfg.daemon_logs().join(STATE_FILE);
        let out = if self.limit_episodes.is_empty() {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        } else {
            let tmp = path.with_extension("json.tmp");
            serde_json::to_vec(&self.limit_episodes)
                .map_err(std::io::Error::other)
                .and_then(|bytes| {
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    std::fs::write(&tmp, bytes)?;
                    std::fs::rename(&tmp, &path)
                })
        };
        if let Err(e) = out {
            self.hub.warn(
                "limit_hook_unsaved",
                format!(
                    "the limit hook's state was not written to {}: {e}. It holds until this \
                     daemon stops",
                    path.display()
                ),
            );
        }
    }

    /// A request succeeded: every open episode is over and its state is destroyed.
    pub(crate) fn limit_episodes_over(&mut self) {
        if self.limit_episodes.is_empty() {
            return;
        }
        let models: Vec<String> = self.limit_episodes.keys().cloned().collect();
        self.limit_episodes.clear();
        self.save_limit_episodes();
        self.hub.warn(
            "limit_hook_done",
            format!(
                "a request succeeded, so the limit episode on {} is over and the hook's state \
                 is destroyed",
                models.join(", ")
            ),
        );
    }

    /// The hook's answer to a spent ladder, or `None` when the fixed rules decide: no layer, not a
    /// limit, or a hook that failed in this episode.
    pub(crate) fn limit_hook(&mut self, e: &letibot_turn::HttpError) -> Option<AfterTheLadder> {
        use letibot_http::retry::{Policy, Trouble};
        let kind = match Policy::trouble(e)? {
            Trouble::RateLimit => "rate_limit",
            Trouble::Quota => "quota",
            _ => return None,
        };
        let host = self.plugin_host()?;
        if !host.answers(limit::HOOK) {
            return None;
        }
        let here = self.model_name();
        if self
            .limit_episodes
            .get(&here)
            .is_some_and(|ep| ep.fell_back)
        {
            return None;
        }
        let now = self.limit_now_ms();
        let reset_ms = Policy::reset_unbounded_ms(e, now);
        if let Some(at) = reset_ms {
            let jitter = self.limit_probing.jitter_ms();
            self.note_limited(&here, at.saturating_add(jitter));
        }
        let ep = self.limit_episodes.entry(here.clone()).or_default();
        ep.attempt += 1;
        let (state, attempt, answered) = (ep.state.clone(), ep.attempt, ep.answered.take());
        let input = self.limit_input(e, kind, reset_ms, &here, state, attempt, answered);
        let decision = match host.limit(&self.hub, &input)? {
            Ok(d) => d,
            Err(why) => return self.limit_hook_fell_back(&here, &why),
        };
        let out = decision.out;
        if let Some(ep) = self.limit_episodes.get_mut(&here) {
            ep.state = out.state.clone();
        }
        self.save_limit_episodes();
        self.hub.warn(
            "limit_hook",
            format!(
                "{} by `{}` answered `{}` on {here}'s refusal {attempt}: {}. state {}",
                decision.chain.join(" -> "),
                decision.by,
                out.act.name(),
                out.said(),
                out.state
            ),
        );
        let by = decision.by;
        match out.act {
            limit::Do::Wait => {
                let Some(at) = out.when.or(reset_ms) else {
                    return self.limit_hook_fell_back(
                        &here,
                        "`wait` named no `when` and the refusal carried no reset",
                    );
                };
                let said = self.park_until(at, None, e);
                Some(AfterTheLadder::Parked { said })
            }
            limit::Do::Switch { provider, model } => {
                let name = format!("{provider}/{model}");
                if name == here {
                    return self.limit_hook_fell_back(&here, "`switch` named the refusing model");
                }
                if let Some(why) = Self::off_the_list(&input, "switch", &name) {
                    return self.limit_hook_fell_back(&here, &why);
                }
                match self.switch_on_a_refusal(&name, e) {
                    Ok(next) => Some(AfterTheLadder::Moved {
                        why: format!(
                            "the limit hook `{by}` moved this session from {here} to {next}"
                        ),
                        next,
                    }),
                    Err(why) => self.limit_hook_fell_back(&here, &format!("`{name}`: {why}")),
                }
            }
            limit::Do::NewSession { provider, model } => {
                // Before the fork: a target nobody listed is a spend nobody chose, and the fork
                // would already be a session to close.
                let target = format!("{provider}/{model}");
                if let Some(why) = Self::off_the_list(&input, "new_session", &target) {
                    return self.limit_hook_fell_back(&here, &why);
                }
                match self.limit_new_session(&provider, &model, &here, &by) {
                    Ok(said) => Some(AfterTheLadder::Stopped { said }),
                    Err(why) => self.limit_hook_fell_back(&here, &why),
                }
            }
            limit::Do::AskUser { question, options } => {
                match self.limit_ask(&here, question, options) {
                    Ok(answered) => {
                        if let Some(ep) = self.limit_episodes.get_mut(&here) {
                            ep.answered = Some(answered);
                        }
                        self.save_limit_episodes();
                        Some(AfterTheLadder::Again)
                    }
                    Err(why) => self.limit_hook_fell_back(&here, &why),
                }
            }
            limit::Do::Stop => {
                let said = format!(
                    "[limit] the limit hook `{by}` ended this turn on {here}'s refusal: {}",
                    super::limit::said_one_line(&e.to_string(), 200)
                );
                if let Err(err) = self.record_notice(&said) {
                    self.hub.warn(
                        "limit_hook_unsaved",
                        format!("{said} The row was not written: {err}"),
                    );
                }
                Some(AfterTheLadder::Stopped { said })
            }
            limit::Do::Call { .. } => {
                self.limit_hook_fell_back(&here, "a `call` reached the host unresolved")
            }
        }
    }

    /// A target the hook named, refused unless it is one of the candidates the operator
    /// configured: the fixed fallback list is the only spend they chose, and `switch` and
    /// `new_session` are the two acts that spend it.
    fn off_the_list(input: &limit::Input, verb: &str, name: &str) -> Option<String> {
        let listed = input
            .candidates
            .iter()
            .any(|c| format!("{}/{}", c.provider, c.model) == name);
        (!listed).then(|| format!("`{verb}` named `{name}`, which is not one of the candidates"))
    }

    /// The hook failed: said on its own row, and the fixed rules decide the rest of the episode.
    fn limit_hook_fell_back(&mut self, here: &str, why: &str) -> Option<AfterTheLadder> {
        let ep = self.limit_episodes.entry(here.to_string()).or_default();
        ep.fell_back = true;
        let state = ep.state.clone();
        self.save_limit_episodes();
        self.hub.warn(
            "limit_hook_fallback",
            format!(
                "the limit hook failed on {here}'s refusal: {why}. The fixed rules decide this \
                 episode. state {state}"
            ),
        );
        None
    }

    #[allow(clippy::too_many_arguments)] // One input record, assembled from what the caller holds.
    fn limit_input(
        &self,
        e: &letibot_turn::HttpError,
        kind: &str,
        reset_ms: Option<u64>,
        here: &str,
        state: Value,
        attempt: u32,
        answered: Option<limit::Answered>,
    ) -> limit::Input {
        let (provider, model) = crate::slash::split_model_name(here);
        let (status, body) = match e {
            letibot_turn::HttpError::Status { code, body, .. } => (*code, body.as_str()),
            _ => (0, ""),
        };
        let candidates =
            crate::leticode_config::fallback_for(&self.cfg.fallback_for, &self.cfg.fallback, here)
                .iter()
                .filter(|name| name.as_str() != here)
                .map(|name| {
                    let (provider, model) = crate::slash::split_model_name(name);
                    limit::Candidate {
                        provider,
                        model: model.unwrap_or_default(),
                        limited_until_ms: self.limited_until(name),
                    }
                })
                .collect();
        limit::Input {
            v: letibot_plugin::V,
            now_ms: self.limit_now_ms(),
            refusal: limit::Refusal {
                status,
                provider,
                model: model.unwrap_or_default(),
                kind: kind.to_string(),
                window: window_of(body),
                reset_ms,
                attempt,
                said: super::limit::said_one_line(&e.to_string(), 200),
            },
            usage: None,
            session: limit::Session {
                id: self.cfg.session_id.clone(),
                seat: format!("{:?}", self.cfg.seat).to_lowercase(),
                parent: self.cfg.parent_session_id.clone(),
                turn_in_flight: self.prompt_in_flight.load(Ordering::SeqCst),
            },
            candidates,
            state,
            answered,
        }
    }

    /// `ask_user`: the operator's card, through the head `ask_user_question` reaches.
    fn limit_ask(
        &self,
        here: &str,
        question: String,
        options: Vec<String>,
    ) -> Result<limit::Answered, String> {
        use letibot_tools::builtins::intent::Question;
        let asker = self
            .question_slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or("`ask_user` has no head to ask in this session")?;
        let q = Question {
            text: question.clone(),
            options: options.clone(),
            because: format!("{here} refused on a usage limit and the limit hook asks"),
            default: None,
            timeout: None,
        };
        let (a, by) = asker
            .ask(&q)
            .map_err(|e| format!("`ask_user` was not answered: {e:?}"))?;
        let text = match (a.option, &a.free) {
            (Some(i), _) => options.get(i).cloned().unwrap_or_default(),
            (None, Some(free)) => free.clone(),
            (None, None) if a.abstain => "abstained".into(),
            (None, None) => String::new(),
        };
        let text = match &a.note {
            Some(note) => format!("{text} ({note})"),
            None => text,
        };
        Ok(limit::Answered {
            question,
            options,
            option: a.option,
            text,
            by,
        })
    }

    /// `new_session`: this conversation is forked into a new session, as `/fork` does, and the
    /// turn continues there on the target. Fork, never a cold brief.
    /// This session ends its turn with a row naming the fork.
    fn limit_new_session(
        &mut self,
        provider: &str,
        model: &str,
        here: &str,
        by: &str,
    ) -> Result<String, String> {
        let me = self.cfg.session_id.clone();
        let keep = self.session.items.len();
        let (id, _) = self
            .fork_into_new_session(keep)
            .map_err(|e| format!("`new_session` could not fork this conversation: {e}"))?;
        // On the store row, so the fork opens on the target: a `/models` queued ahead of the
        // turn is refused while the fork is not open yet.
        let target = format!("{provider}/{model}");
        self.store
            .as_ref()
            .ok_or("`new_session` forked into a store this session no longer has")?
            .set_provider_choice(&id, Some(&target))
            .map_err(|e| format!("`new_session` could not put the fork {id} on {target}: {e}"))?;
        let wiring = letibot_sessionlog::registry::SessionWiring {
            workspace: self.cfg.workspace.display().to_string(),
            ..Default::default()
        };
        let hub = self
            .session_registry
            .create(id.clone(), format!("forked from {me}"), wiring)
            .map_err(|e| format!("`new_session` could not open the fork {id}: {e}"))?;
        let resume = format!(
            "[limit] {here} refused on a usage limit mid-turn and the limit hook `{by}` forked \
             this conversation from {me} onto {target}. Continue the turn."
        );
        let queued = hub.submit_daemon(
            format!("agent:{me}"),
            "limit-continue",
            CommandKind::Prompt {
                text: resume,
                attachments: Vec::new(),
                context: None,
                from: Some(me.clone()),
                delivery: Default::default(),
            },
        );
        if !queued {
            return Err(format!("the fork {id}'s queue refused the turn"));
        }
        self.limit_carry_the_unread(&hub, &me, &id);
        let said = format!(
            "[limit] {here} refused on a usage limit and the limit hook `{by}` forked this \
             conversation into {id}, where the turn continues on {target}. This turn \
             ends here."
        );
        if let Err(e) = self.record_notice(&said) {
            self.hub.warn(
                "limit_hook_unsaved",
                format!("{said} The row was not written: {e}"),
            );
        }
        self.hub.warn("limit_new_session", said.clone());
        Ok(said)
    }

    /// What this conversation has not read yet moves to the fork: the prompts queued for the turn
    /// that is ending, and the steers a refused round handed back. Left here they run on the model
    /// that refused and each one can fork again. A notice is not a prompt and stays.
    pub(crate) fn limit_carry_the_unread(&mut self, fork: &Arc<Hub>, me: &str, id: &str) {
        let queued = self.hub.take_queued_prompts();
        let handed: Vec<letibot_turn::SteeringMessage> = {
            let mut held = self.handed_back.lock().unwrap_or_else(|e| e.into_inner());
            let handed: Vec<_> = held
                .iter()
                .filter(|m| m.notice.is_none())
                .cloned()
                .collect();
            held.retain(|m| m.notice.is_some());
            handed
        };
        let mut carried = queued.clone();
        for (i, m) in handed.iter().enumerate() {
            carried.push(letibot_sessionlog::hub::QueuedCommand {
                head_id: letibot_sessionlog::hub::DAEMON_SUBMITTER.to_string(),
                identity: match m.from_operator {
                    true => "human".to_string(),
                    false => format!("agent:{me}"),
                },
                client_request_id: format!("fork-{id}-{i}"),
                at_seq: 0,
                enqueued_ms: letibot_sessionlog::event::now_ms(),
                kind: CommandKind::Prompt {
                    text: m.text.clone(),
                    // A steer's parts are already rendered user parts, which a frame's
                    // attachment is not, so an attachment on one does not survive the move.
                    attachments: Vec::new(),
                    context: None,
                    from: m.from_session.clone().or_else(|| m.peer.clone()),
                    delivery: letibot_sessionlog::protocol::PromptDelivery::Queue,
                },
            });
        }
        let n = carried.len();
        if fork.adopt_prompts(carried) {
            return;
        }
        // The fork closed under it: each went back where it came from, whole. The journal is not
        // told, so a steer stays taken until this turn's own persist releases it.
        self.hub.adopt_prompts(queued);
        self.handed_back
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(handed);
        self.hub.warn(
            "limit_new_session_queue_not_carried",
            format!(
                "the fork {id} took no prompt: the {n} this session had not read stayed here, \
                 and they run on the model that refused"
            ),
        );
    }
}

/// The cell `ask_user_question` and the limit hook share.
pub type QuestionSlot = Arc<Mutex<Option<Arc<dyn letibot_tools::builtins::intent::Questioner>>>>;

/// The window a refusal's prose names (`5 hour`), when it names one.
fn window_of(body: &str) -> Option<String> {
    let at = body.find(" for ")? + 5;
    let rest = &body[at..];
    let end = rest.find(['.', '"', ','])?;
    let w = rest[..end].trim();
    (w.split_whitespace().count() == 2 && w.starts_with(|c: char| c.is_ascii_digit()))
        .then(|| w.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_read_from_the_providers_prose() {
        assert_eq!(
            window_of("{\"message\":\"Usage limit reached for 5 hour. Reset at 21:00\"}"),
            Some("5 hour".into())
        );
        assert_eq!(window_of("Rate limit reached"), None);
        assert_eq!(window_of("waiting for the server, again"), None);
    }

    /// The list a hook's target is held to is the operator's configured fallbacks, and a name off
    /// it is refused in the verb's own words: `switch` and `new_session` both spend it.
    #[test]
    fn a_target_off_the_candidates_is_refused_by_name() {
        let input = limit::Input {
            v: letibot_plugin::V,
            now_ms: 0,
            refusal: limit::Refusal {
                status: 429,
                provider: "glm".into(),
                model: "glm-5.3".into(),
                kind: "rate_limit".into(),
                window: None,
                reset_ms: None,
                attempt: 1,
                said: String::new(),
            },
            usage: None,
            session: limit::Session {
                id: "s".into(),
                seat: "main".into(),
                parent: None,
                turn_in_flight: true,
            },
            candidates: vec![limit::Candidate {
                provider: "deepseek".into(),
                model: "deepseek-flash".into(),
                limited_until_ms: None,
            }],
            state: Value::Null,
            answered: None,
        };
        assert_eq!(
            Harness::off_the_list(&input, "switch", "deepseek/deepseek-flash"),
            None,
            "a configured fallback is the spend the operator chose"
        );
        let why = Harness::off_the_list(&input, "new_session", "kimi/kimi-k2")
            .expect("a model nobody listed is refused");
        assert!(why.contains("`new_session` named `kimi/kimi-k2`"), "{why}");
        assert!(why.contains("not one of the candidates"), "{why}");
    }
}
