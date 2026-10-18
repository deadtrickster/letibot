//! **The model a sent message would go to, checked before it goes** — and the chooser when it
//! cannot be reached.
//!
//! The operator, 2026-10-18, after `leticode` on a box with no local model refused with
//! *"nothing serving on 127.0.0.1:8080 ... or a cloud provider instead: --provider
//! deepseek|glm|grok"*: *"idk why i even have to do --provider. horrible ux - we already have
//! models chooser and keys asker"*. So the launcher stopped gating (see "THE LAUNCHER NEVER GATES
//! ON A MODEL" in `scripts/letibot`) and the question moved here, to the two things that were
//! already built for it: the `/models` card ([`Pick::Model`]) and its key card ([`KeyAsk`]).
//!
//! # Why at the first message, and not at open
//!
//! **The message is what is at risk, and the check is per attempt.** At open there is no hook —
//! the settings rows the check reads arrive whenever the daemon answers, and a session reopened to
//! READ it would get a card over the transcript it came to read. At send there is one place every
//! prompt passes ([`App::submit`]), the message can be held while the check runs and sent the
//! moment it passes, and a local server that comes up later simply answers the next message:
//! nothing is latched, every send while the session sits on a local model asks again.
//!
//! # What "cannot be reached" means, and who answers each half
//!
//! * **no local model on this box at all** — the session is on the local route and its `vocab`
//!   row is `none`: the daemon was started with no GGUF and no provider (`Parts::load`), and a
//!   llama-server cannot read the byte ids it would be sent. Known from the rows; no probe.
//! * **nothing serving the local model** — the `endpoint` row's `/health` does not answer 2xx.
//!   Probed on a thread of its own, the way [`App::refresh_git`] reads git: a LAN box that is
//!   down costs a connect timeout, and the loop never waits on it.
//! * **a provider whose key is missing** — NOT here. The daemon already asks for it on the first
//!   turn, on the masked card sudo's password uses, and saves it (0600) with that provider as
//!   the default (`Harness::obtain_key`). A second ask in the head would be the same question
//!   from two places.
//!
//! # What taking a row does with the held message
//!
//! A cloud row is switched to, made `[default]` (`/default-model`, so the question is asked once
//! per box — the daemon's own key card does the same with the first key), and the held message
//! is sent after it. A row with no key goes through the key card first, and the message waits
//! there too. Anything else — a local row, `esc` on either card — puts the words back in the
//! composer, where enter sends them (and checks again).

use super::*;

/// **What answers whether the local model is up** — `/health` on an address, in production
/// [`health`]; a test puts a function here that answers without a network.
pub(crate) type ReachProbe = std::sync::Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// The state of the check: the probe in flight, the message held for it, and why the chooser is
/// up when it is.
pub(crate) struct Reach {
    pub(crate) job: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    pub(crate) held: Option<String>,
    /// The one line the chooser carries while it is up because of this check — `None` when the
    /// operator opened it themselves.
    pub(crate) reason: Option<String>,
    pub(crate) probe: ReachProbe,
}

impl Default for Reach {
    fn default() -> Self {
        Reach {
            job: None,
            held: None,
            reason: None,
            probe: std::sync::Arc::new(health),
        }
    }
}

/// **`GET /health` on `authority`, answered in a few seconds or not at all.** The launcher's own
/// `curl -sf -m 3` reading, which is what this replaces: a 2xx is up, and anything else is the
/// sentence the chooser carries.
pub(crate) fn health(authority: &str) -> Result<(), String> {
    let mut ep = letibot_http::Endpoint::parse(authority)
        .map_err(|e| format!("the local model's address is not one: {e}"))?;
    ep.connect_timeout = std::time::Duration::from_secs(2);
    ep.read_timeout = std::time::Duration::from_secs(3);
    match letibot_http::get(&ep, "/health") {
        Ok(_) => Ok(()),
        // llama-server answers 503 while it loads: something IS there, and it says so.
        Err(letibot_http::HttpError::Status { code, .. }) => Err(format!(
            "the local model on {authority} answered /health with {code}"
        )),
        Err(_) => Err(format!("nothing is serving the local model on {authority}")),
    }
}

/// What a send has to find out first.
enum Check {
    /// The rows already say: no local model on this box.
    NoLocalModel,
    /// Ask this address's `/health`.
    Probe(String),
}

impl App {
    /// **Which check a message sent now needs**, from the daemon's rows. `None` sends at once: a
    /// provider (its missing key is the daemon's card), or a daemon that has not said enough —
    /// no `model` or `endpoint` row is a daemon older than them, or not answered yet, and a check
    /// that guessed would hold messages it had no reason to hold.
    fn reach_check(&self) -> Option<Check> {
        let row = |k: &str| self.settings.iter().find(|r| r.key == k);
        let model = row("model")?;
        let first = model.value.split_whitespace().next().unwrap_or("");
        if !self.keyless_choices().iter().any(|k| k == first) {
            return None;
        }
        if row("vocab").is_some_and(|r| r.value.starts_with("none")) {
            return Some(Check::NoLocalModel);
        }
        let ep = row("endpoint")?.value.trim().to_string();
        (!ep.is_empty()).then_some(Check::Probe(ep))
    }

    /// **Hold `text` for the model check, or say it can go now.** `true` is held: the probe is
    /// started (or the chooser is already up) and the message goes when it passes.
    ///
    /// Not while a turn runs: the model is answering, or being retried, and a line typed then
    /// is steering that the daemon merges into the turn — holding it would reorder it.
    pub(crate) fn hold_for_model(&mut self, text: &str) -> bool {
        // **A second message while the first is held joins it**, as steering joins a running
        // turn: one held message, sent once.
        if let Some(held) = &mut self.reach.held {
            held.push('\n');
            held.push_str(text);
            self.redraw = true;
            return true;
        }
        if self.turn_busy() || self.session_id.is_empty() {
            return false;
        }
        match self.reach_check() {
            None => false,
            Some(Check::NoLocalModel) => {
                self.reach.held = Some(text.to_string());
                self.model_unreachable("no local model is set up on this box".to_string());
                true
            }
            Some(Check::Probe(ep)) => {
                self.reach.held = Some(text.to_string());
                let (tx, rx) = std::sync::mpsc::channel();
                let probe = self.reach.probe.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(probe(&ep));
                });
                self.reach.job = Some(rx);
                true
            }
        }
    }

    /// **Collect the probe, and give back a message the chooser did not take.** Called from the
    /// loop (`Driver::tick`), never from a paint; it never waits.
    pub fn poll_reach(&mut self) {
        if let Some(rx) = &self.reach.job {
            match rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Ok(Ok(())) => {
                    self.reach.job = None;
                    self.send_held();
                }
                Ok(Err(why)) => {
                    self.reach.job = None;
                    self.model_unreachable(why);
                }
                // **A probe that died without answering does not keep the message.** The check
                // is a courtesy in front of the send, and the daemon's own retry ladder is still
                // behind it; a lost answer is not evidence the model is down.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.reach.job = None;
                    self.send_held();
                }
            }
        }
        // **Every way the chooser closes without taking a cloud row lands here** — esc on the
        // card or the key card, a local row, a surface that closed it — so the words go back
        // where they were typed rather than being lost behind a card that is gone.
        if self.reach.reason.is_some()
            && self.reach.job.is_none()
            && self.pick != Some(Pick::Model)
            && self.key_ask.is_none()
        {
            self.reach.reason = None;
            if let Some(text) = self.reach.held.take() {
                self.set_composer(&text);
                self.say("your message is back in the composer — enter sends it");
                self.redraw = true;
            }
        }
    }

    /// The held message, sent — the same echo [`App::submit`] gives a line it sends.
    fn send_held(&mut self) {
        self.reach.reason = None;
        if let Some(text) = self.reach.held.take() {
            self.scroll = 0;
            self.pending_prompts.push(text.clone());
            self.queued.push(Action::Prompt(text));
            self.redraw = true;
        }
    }

    /// **The chooser, with the reason on it.** `why` is the first half of the sentence; the
    /// card and the transcript both say it, the card because that is where the eye is and the
    /// transcript because the card goes away.
    fn model_unreachable(&mut self, why: String) {
        let line = format!("{why} — pick a model");
        self.say(&line);
        self.reach.reason = Some(line);
        if self.open_model_pick() {
            self.queued.push(Action::Settings);
        }
    }

    /// **Bare `/models`'s card**, opened: seeded to what answers now. `false` when there is no
    /// session to pick for. The caller asks the daemon for fresh rows (`Action::Settings`).
    pub(crate) fn open_model_pick(&mut self) -> bool {
        if self.session_id.is_empty() {
            self.say("not attached to a session yet");
            return false;
        }
        self.pick = Some(Pick::Model);
        self.close_surface(Surface::Picker);
        self.close_surface(Surface::Config);
        self.seed_pick();
        self.redraw = true;
        true
    }

    /// **A cloud row was taken from the chooser this check opened**: make it the box's default
    /// and send the held message after the switch. Queued, so they leave after the switch the
    /// caller returns — the daemon takes a connection's frames in order.
    ///
    /// `/default-model` and not a write here: the daemon owns providers.toml, and its verb
    /// refuses a default nothing can authenticate — which, after the switch stored the key, it
    /// can. A declared fleet name is not a preset and is not made the default; the verb would
    /// refuse it.
    pub(crate) fn took_cloud_row(&mut self, choice: &str) {
        if self.reach.reason.is_none() {
            return;
        }
        self.queued.push(Action::Slash {
            line: format!("default-model {choice}"),
        });
        self.send_held();
    }
}
