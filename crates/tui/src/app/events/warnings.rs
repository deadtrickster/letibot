//! **Warnings**: a diagnostic the daemon raised, routed to the conversation, the notes or the
//! alarm.

use super::*;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::view::Warned;
use letibot_ui::text::without_control;

/// The daemon's boot steps (`cli::Boot`): `daemon_starting` names the step, `daemon_started`
/// ends it.
pub(crate) const DAEMON_STARTING: &str = "daemon_starting";
pub(crate) const DAEMON_STARTED: &str = "daemon_started";

/// **This daemon's binary is older than the store it serves** (`store::floor` in
/// `letibot-tokencore`). Said on every attach by `server::seat_in` while it is true, and taken as
/// STATE rather than as a note - see [`App::store_newer`].
pub(crate) const STORE_NEWER: &str = "store_newer";

impl App {
    /// **A boot step, taken as the state it is** - `true` when `code` was one, so the caller
    /// draws no row for it. The live arm and the snapshot both come here, so a head that attaches
    /// mid-boot (the hub's log replays them) and one that watched it agree.
    pub(crate) fn boot_warning(&mut self, code: &str, detail: &str) -> bool {
        match code {
            DAEMON_STARTING => {
                self.booting = Some(detail.to_string());
                true
            }
            DAEMON_STARTED => {
                self.booting = None;
                true
            }
            _ => false,
        }
    }

    /// **The model the daemon says answers now** - the `model` row's value, which may be a local
    /// alias in the config pane's spelling. One door for the live arm and for a snapshot, and no
    /// note either way: a name is what the header draws, not a finding to read.
    ///
    /// `at` is when this head HEARD it, which for a replayed row is when it heard the last one: a
    /// snapshot's window is history, and stamped as news it outranks the settings row it is older
    /// than.
    pub(crate) fn model_set(&mut self, value: &str, at: u64) {
        let model = crate::ui::header::header_model(value);
        if !model.is_empty() {
            self.model = model;
            self.model_from_live_at = at;
            self.redraw = true;
        }
    }

    /// **A warning the daemon raised.** One family of
    /// [`App::event`]'s arms, moved here verbatim; `event` hands it only these variants.
    pub(crate) fn on_warning(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        match e {
            SessionEvent::Warning { code, detail, .. } => {
                // The daemon publishes both this and `TurnFailed` on purpose - one is
                // state, one history - but on a screen they are one sentence twice.
                // Counted as filtered, not dropped: `filtered` says "chose not to show".
                if code == "turn_failed" {
                    return Disposition::Filtered;
                }
                // **The daemon's boot, as a state and not as rows** (R92.2) - the line on the
                // tail and the label on a queued prompt; the steps are not the conversation.
                if self.boot_warning(&code, &detail) {
                    self.redraw = true;
                    return Disposition::Filtered;
                }
                // A state rather than a row: it must sit where the Responding text is
                // drawn, and it needs no clearing because only a restart ends it. The
                // daemon re-says it on every attach, so a late head gets it too.
                if code == STORE_NEWER {
                    self.store_newer = Some(detail.to_string());
                    self.redraw = true;
                    return Disposition::Filtered;
                }
                // The fork has two halves: this warning precedes it, `compacted` and
                // `reseated` follow. Resolving at the first half stops an echo saying
                // `queued` for ever over a row the fork summarised away.
                //
                // An announcement is state, not a row: it says why the fold is running, which is
                // what the progress line's click shows. Drawing it as a note made one compaction
                // three rows.
                if code == "compacting" {
                    self.mark_fork();
                    self.compacting_why = Some(detail.clone());
                    self.redraw = true;
                    return Disposition::Filtered;
                }
                // An older daemon sends the same announcement under this code: mark the fork and
                // draw the note it always drew, so a head one version ahead still says what is
                // happening rather than nothing.
                if code == "auto_compact" {
                    self.mark_fork();
                }
                // `cleared` is the third fork's report (`/clear`), and the same resolution.
                if code == "compacted" || code == "reseated" || code == "cleared" {
                    self.resolve_fork();
                    // The progress event is ephemeral - it carries no "done" - so the
                    // durable warning that the fork landed is what ends the walking line.
                    // A fold that failed ends it too, below: not a fold still running.
                    self.compacting = None;
                    self.compacting_why = None;
                }
                if code == "auto_compact_failed" {
                    self.compacting = None;
                    self.compacting_why = None;
                }
                // The "N rows are being loaded" line is this head's own claim, so the
                // daemon's `resume_failed` is the one fact that ends it without rows. A
                // line promising a conversation still on its way would be stale.
                if code == "resume_failed" {
                    self.restoring = None;
                }
                // The reseat card (`app::reseat`): `prefix_stale` raises it and is still
                // filed as a note row for `/notes`; `prefix_current`/`reseated` take it
                // down on every head, and `reseat_refused` lets alt-r send again.
                if code == "prefix_stale" {
                    self.prefix_stale(detail.clone());
                }
                if code == "reseated" {
                    self.prefix_current();
                }
                if code == "reseat_refused" {
                    self.reseat_refused(&detail);
                }
                // The clear is the card's signal and nothing else: the `reseated` row (or the
                // `compacted` one) already says in the record what moved the session, so a
                // second row saying the notice no longer applies is the same fact twice.
                if code == "prefix_current" {
                    self.prefix_current();
                    return Disposition::Filtered;
                }
                // This warning is the live half of the channel, and `auto_compact_off` is
                // what the resident line and `/status` read. The settings row is the other
                // half, for a head that attaches later - see the `Settings` arm.
                if code == "auto_compact_no_progress" {
                    self.auto_compact_off = Some(format!("auto-compaction is off: {detail}"));
                }
                // The live half of `nags_paused`; the daemon's prose is not parsed. The
                // settings are asked for so the line names the pause's end in the
                // daemon's own words. A resume clears it outright.
                if code == "nags_paused" {
                    if self.nags_paused.is_none() {
                        self.nags_paused = Some(String::new());
                    }
                    self.queued.push(Action::Settings);
                }
                if code == "nags_resumed" {
                    self.nags_paused = None;
                }
                // Until the switch lands, "did my `/models` take?" needs an answer that
                // does not scroll away with the turn it is about, so it is a state. The
                // daemon's sentence is kept verbatim: it is the one that says when.
                if code == "model_switch_pending" {
                    self.model_pending = Some(detail.clone());
                    // Drawn on the status row rather than filed: a pending switch is
                    // something about to happen, and the applied/not-applied row that
                    // follows is the record.
                    self.redraw = true;
                    return Disposition::Filtered;
                }
                if code == "model_switch_applied" || code == "model_switch_not_applied" {
                    self.model_pending = None;
                    self.queued.push(Action::Settings);
                }
                // The model answering NOW, on the daemon's word rather than a request: the `model`
                // row moved on somebody else's act, and a name is state, not a finding to file.
                if code == letibot_sessionlog::protocol::MODEL_SET {
                    self.model_set(&detail, self.seq);
                    return Disposition::Filtered;
                }
                // A slash LISTING opens the pane and a slash sentence stays a note. Both
                // arrive under one code with `detail` = the echoed command then the reply,
                // so length is the only thing that tells them apart.
                if code == "slash" || code == "slash_refused" {
                    let (echo, body) = detail.split_once('\n').unwrap_or((&detail, ""));
                    let lines: Vec<String> = body.lines().map(without_control).collect();
                    if lines.len() > 3 {
                        self.slash_out = Some((echo.to_string(), lines));
                        self.push_surface(Surface::Listing);
                        return Disposition::Rendered;
                    }
                }
                // A refusal of a read the head made for a notice's second line is the
                // notice's answer, not the operator's business: the notice shows the
                // daemon's words. Matched by handle, else every pending notice ask.
                if code == "job_output_refused" {
                    use crate::ui::transcript::job_card::JobTail;
                    let pending: Vec<String> = self
                        .job_tails
                        .iter()
                        .filter(|(_, t)| matches!(t, JobTail::Asked))
                        .map(|(j, _)| j.clone())
                        .collect();
                    let pane_reading = self
                        .job_out
                        .as_ref()
                        .is_some_and(|v| v.loading || v.polling);
                    let named: Vec<String> = pending
                        .iter()
                        .filter(|j| detail.contains(&format!("`{j}`")))
                        .cloned()
                        .collect();
                    let theirs = if !named.is_empty() {
                        named
                    } else if !pane_reading {
                        pending
                    } else {
                        Vec::new()
                    };
                    let pane_named = self
                        .job_out
                        .as_ref()
                        .is_some_and(|v| (v.loading || v.polling) && theirs.contains(&v.job));
                    if !theirs.is_empty() && !pane_named {
                        for j in theirs {
                            self.job_tail_landed(&j, JobTail::Gone(detail.clone()));
                        }
                        return Disposition::Filtered;
                    }
                }
                // Answered in the pane that asked, which is still open: left alone it
                // would sit at `reading...` for ever, waiting for a window that is not
                // coming. The conversation gets the note as well.
                if code == "job_output_refused"
                    && let Some(v) = self.job_out.as_mut()
                {
                    v.loading = false;
                    v.polling = false;
                    v.error = Some(detail.clone());
                    self.redraw = true;
                }
                // Weather notes go on the edge (`warning::ALARM_ONLY`): the triangle, a counter with
                // `/status` holding the number, and the daemon's sentence listed by `/notes` and
                // drawn nowhere - filed BESIDE the conversation, which is why this is `Filtered`.
                if letibot_sessionlog::warning::to_the_alarm(&code) {
                    // **The sentence is filed, not dropped.** Filtering it here and filing it only on
                    // the snapshot door gave one warning two records decided by when you attached:
                    // a head that came later listed it and a live head kept only the count.
                    self.note_beside(Note::Warned(Warned {
                        code: code.clone(),
                        detail,
                        ts,
                    }));
                    if !self.count_edge_note(&code) {
                        self.note(Note::Warned(Warned {
                            code: "alarm_only_unregistered".into(),
                            detail: format!(
                                "`{code}` is classified as edge-bound and this head has no \
                                 counter for it, so nothing on the edge points at the sentence \
                                 `/notes` now lists. See `warning::ALARM_ONLY`."
                            ),
                            ts,
                        }));
                    }
                    self.redraw = true;
                    return Disposition::Filtered;
                }
                self.note(Note::Warned(Warned { code, detail, ts }));
                Disposition::Rendered
            }
            _ => unreachable!("on_warning was handed an event it does not handle"),
        }
    }

    /// **A provider in trouble, as one row**: the same sentence a warning is, with the model and the
    /// reset time the type carries and the prose does not. The row's code is the kind, so a burst of
    /// rate limits reads as one line rather than as a paragraph.
    pub(crate) fn on_provider_trouble(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        let SessionEvent::ProviderTrouble {
            kind,
            model,
            detail,
            reset_ms,
            count,
            ..
        } = e
        else {
            unreachable!("on_provider_trouble was handed an event it does not handle")
        };
        let mut said = format!("{model}: {detail}");
        if let Some(at) = reset_ms {
            let at = crate::ui::transcript::blocks::clock_time(at);
            if !at.is_empty() {
                said.push_str(&format!(", and the provider said its limit lifts at {at}"));
            }
        }
        if count > 1 {
            said.push_str(&format!(" — {count} in a row"));
        }
        self.note(Note::Warned(Warned {
            code: format!("provider_{kind}"),
            detail: said,
            ts,
        }));
        Disposition::Rendered
    }
}
