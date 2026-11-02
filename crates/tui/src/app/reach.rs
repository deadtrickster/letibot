//! The model a sent message would go to, checked before it goes: at the first message rather than
//! at open, because the message is what is at risk and a local server that comes up later answers
//! the next send. Unreachable is the `vocab` row saying `none`, or the endpoint's `/health` failing.

use super::*;

/// **What answers whether the local model is up** - `/health` on an address, in production
/// [`health`]; a test puts a function here that answers without a network.
pub(crate) type ReachProbe = std::sync::Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// The state of the check: the probe in flight, the message held for it, and why the chooser is
/// up when it is.
pub(crate) struct Reach {
    pub(crate) job: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    pub(crate) held: Option<String>,
    /// The one line the chooser carries while it is up because of this check - `None` when the
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
    /// Which check a message sent now needs, from the daemon's rows. `None` sends at once: a
    /// provider, whose missing key is the daemon's own card, or a daemon that has not said enough -
    /// a check that guessed would hold messages it had no reason to hold.
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

    /// Hold `text` for the model check, or say it can go now. `true` is held: the probe is started
    /// (or the chooser is already up) and the message goes when it passes. Not while a turn runs -
    /// the model is answering or being retried, and a line typed then is steering that turn.
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
        // **Every way the chooser closes without taking a cloud row lands here** - esc on the
        // card or the key card, a local row, a surface that closed it - so the words go back
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

    /// The held message, sent - the same echo [`App::submit`] gives a line it sends.
    fn send_held(&mut self) {
        self.reach.reason = None;
        if let Some(text) = self.reach.held.take() {
            self.scroll = 0;
            self.pending_prompts.push(text.clone());
            self.queued.push(Action::Prompt(Prompt::new(text)));
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

    /// A cloud row was taken from the chooser this check opened: make it the box's default and send
    /// the held message after the switch. Both are queued, so they leave after the switch the caller
    /// returns. `/default-model` and not a write here - the daemon owns providers.toml.
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
