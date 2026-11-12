//! Warnings: a diagnostic the daemon raised, routed to the conversation, the notes or the
//! alarm.

use super::*;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::view::Warned;
use letibot_ui::text::without_control;

/// The daemon's boot steps (`cli::Boot`): `daemon_starting` names the step, `daemon_started`
/// ends it.
pub(crate) const DAEMON_STARTING: &str = "daemon_starting";
pub(crate) const DAEMON_STARTED: &str = "daemon_started";

/// This daemon's binary is older than the store it serves (`store::floor` in
/// `letibot-tokencore`). Said on every attach by `server::seat_in` while it is true, and taken as
/// STATE rather than as a note - see [`App::store_newer`].
pub(crate) const STORE_NEWER: &str = "store_newer";

/// The one line a session's developer diagnostics get (`warning::is_developer`): its own code, so
/// the conversation says *this is a diagnostic* rather than looking like a fault of the session.
pub(crate) const DEVELOPER_CODE: &str = "diagnostics";

impl App {
    /// A boot step, taken as the state it is - `true` when `code` was one, so the caller
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

    /// The model the daemon says answers now - the `model` row's value, possibly a local alias. One door for the
    /// live arm and a snapshot, and no note: a name is what the header draws, not a finding.
    /// `at` is when this head HEARD it; a replayed row is history and must not outrank the settings row it is older than.
    pub(crate) fn model_set(&mut self, value: &str, at: u64) {
        let model = crate::ui::header::header_model(value);
        if !model.is_empty() {
            self.model = model;
            self.model_from_live_at = at;
        }
    }

    /// A warning the daemon raised. One family of
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
                // The daemon's boot, as a state and not as rows - the line on the
                // tail and the label on a queued prompt; the steps are not the conversation.
                if self.boot_warning(&code, &detail) {
                    return Disposition::Filtered;
                }
                // A state rather than a row: it must sit where the Responding text is
                // drawn, and it needs no clearing because only a restart ends it. The
                // daemon re-says it on every attach, so a late head gets it too.
                if code == STORE_NEWER {
                    self.store_newer = Some(detail.to_string());
                    return Disposition::Filtered;
                }
                // The fork has two halves: this warning precedes it, `compacted` and `reseated` follow. Resolving at the first
                // stops an echo saying `queued` for ever over a row the fork summarised away. An announcement is state, not a
                // row: it says why the fold is running, which the progress line's click shows.
                if code == "compacting" {
                    self.mark_fork();
                    self.compacting_why = Some(detail.clone());
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
                }
                // Weather notes go on the edge (`warning::ALARM_ONLY`): the triangle, a counter with
                // `/status` holding the number, and the daemon's sentence listed by `/notes` and
                // drawn nowhere - filed BESIDE the conversation, which is why this is `Filtered`.
                if letibot_sessionlog::warning::to_the_alarm(&code) {
                    // The sentence is filed, not dropped. Filtering it here and filing it only on
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
                    return Disposition::Filtered;
                }
                // A developer's diagnostic: the harness measured itself, and no reader has a move.
                // `/notes` lists every one and the daemon's log has the numbers; the conversation
                // gets ONE line per session, because a census is not something to read between turns.
                if letibot_sessionlog::warning::is_developer(&code) {
                    self.note_beside(Note::Warned(Warned {
                        code: code.clone(),
                        detail: detail.clone(),
                        ts,
                    }));
                    if self.developer_said {
                        return Disposition::Filtered;
                    }
                    self.developer_said = true;
                    self.note(Note::Warned(Warned {
                        code: DEVELOPER_CODE.into(),
                        detail: format!(
                            "`{code}` is one: the harness measuring itself, with no move this \
                             session can make. `/notes` lists every one as it arrives and the \
                             daemon's log holds the numbers."
                        ),
                        ts,
                    }));
                    return Disposition::Rendered;
                }
                self.note(Note::Warned(Warned { code, detail, ts }));
                Disposition::Rendered
            }
            _ => unreachable!("on_warning was handed an event it does not handle"),
        }
    }

    /// A provider in trouble, in the one place it belongs: a rate limit, a timeout and a stall are
    /// the LIVE line's business (the ladder is handling them, and [`App::on_model_retry`] draws it),
    /// so they file no row here; a spent account and a move are the two that do.
    pub(crate) fn on_provider_trouble(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        let SessionEvent::ProviderTrouble {
            kind,
            model,
            detail,
            reset_ms,
            count,
            switched_to,
        } = e
        else {
            unreachable!("on_provider_trouble was handed an event it does not handle")
        };
        // A move the daemon made is said by `model_fallback`, with the refusal that caused it and
        // where the session went: this event's `switched_to` is the same fact as a type, and a
        // second row for it would be the two-rows-for-one-event defect this head keeps deleting.
        if switched_to.is_some() || kind != "quota" {
            return Disposition::Filtered;
        }
        // The account cannot pay: no wait lifts it, so this is the one trouble that is a row - a
        // Failure, whose action is the table's (`provider_quota`).
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

    /// The model not answering, as ONE live line whatever the attempt: the wire sends the line's whole
    /// value, so it is rewritten rather than added to. A count that goes DOWN is a second outage; a
    /// late head must not fold two series into one line. `gone` takes the line down and leaves nothing
    /// behind - a recovered retry is not a finding, and a spent ladder has one row of its own.
    pub(crate) fn on_model_retry(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        let SessionEvent::ModelRetry {
            host,
            attempts,
            said,
            log,
            ended,
            gone,
        } = e
        else {
            unreachable!("on_model_retry was handed an event it does not handle")
        };
        let live = self.notes.iter().position(
            |(_, n)| matches!(n, Note::Retrying(r) if r.live() && r.attempts <= attempts),
        );
        if gone {
            if let Some(i) = live {
                self.drop_note(i);
            }
            return Disposition::Rendered;
        }
        match live {
            None => self.file_retry(host, attempts, said, log, ended, ts),
            Some(i) => {
                let seam = match self.notes[i].0 {
                    Placed::Seam(at) => at,
                    Placed::Before => 0,
                };
                if let (_, Note::Retrying(r)) = &mut self.notes[i] {
                    r.host = host;
                    r.attempts = attempts;
                    r.said = said;
                    r.log = log;
                    r.ended = ended;
                }
                // The row is a seam the walk has already passed and the walk is incremental, so the
                // one honest way to move a drawn row is to make it draw again from the row before.
                self.invalidate_history_from(seam.saturating_sub(1));
            }
        }
        Disposition::Rendered
    }

    /// The row a series opens with, filed where the conversation is now.
    fn file_retry(
        &mut self,
        host: String,
        attempts: u32,
        said: String,
        log: Vec<String>,
        ended: Option<String>,
        ts: u64,
    ) {
        self.retry_series += 1;
        self.file_note(Note::Retrying(Retry {
            series: self.retry_series,
            host,
            attempts,
            said,
            log,
            ended,
            ts,
        }));
    }
}
