//! **The daemon's metrics instruments** — TODO R81 phase 1, `docs/metrics.md` §4 and §6.
//!
//! Every site in the daemon that records a measurement calls ONE function here, on one line, so
//! a merge that moves the site moves one line and the arithmetic stays put. Each function builds
//! a [`letibot_metrics::Event`] and emits it — a `try_send` on a bounded channel, never SQL and
//! never a wait (the c12d044 rule; the module header of `letibot_metrics` says why). When metrics
//! are off every function here returns at its first branch.
//!
//! **What a turn is, here**: a prompt — every round the harness runs for one door, through
//! `turn_tail::run_turn` — keyed by the engine id of its FIRST round. The hub tap keys the
//! operator's marks the same way (`letibot_sessionlog::metrics_tap`), so a mark joins its turn.
//! [`TurnMeter`] is the harness's half: begun by the turn tail, fed each round and each tool
//! call, and closed by the turn tail with one `turn.start` and one `turn.end` row.
//!
//! `turn.start` is written WITH the end, stamped with the moment the turn began: the id it is
//! keyed by is only minted by the first round, and a start row with no id joins nothing. The
//! cost is that a turn a crash interrupted has no rows; phase 2 can add a provisional start if
//! that turns out to matter.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

use letibot_metrics::{Event, kind};
use letibot_sessionlog::QueuedCommand;
use letibot_tools::ToolResult;
use letibot_turn::metrics::TurnMetrics;

use crate::harness::{HarnessError, Reply};

thread_local! {
    /// **Which turn each session on THIS thread is in, and how many rounds it has run** — for
    /// a site that has no harness in hand (R79's delivery record). A session's turns run on one
    /// thread (the worker for a root, its own thread for a child), so this needs no lock.
    static NOW: RefCell<HashMap<String, (Option<String>, u32)>> = RefCell::new(HashMap::new());
}

fn now_of(session: &str) -> Option<(Option<String>, u32)> {
    NOW.with(|n| n.borrow().get(session).cloned())
}

/// The word a finish reason is recorded as.
pub fn finish_word(f: &letibot_turn::completion::FinishReason) -> String {
    use letibot_turn::completion::FinishReason as F;
    match f {
        F::Eos => "eos".into(),
        F::Word => "word".into(),
        F::Length => "length".into(),
        F::Aborted => "aborted".into(),
        F::Other(s) => s.to_string(),
    }
}

/// A row that died before it was named a nag is not a nag a later turn can touch.
#[derive(Debug, Clone)]
struct PendingNag {
    row: String,
    status: Option<String>,
}

/// **One turn's running numbers** — on the harness, begun and closed by the turn tail.
#[derive(Debug, Default)]
pub struct TurnMeter {
    began_ms: Option<u64>,
    began: Option<Instant>,
    door: String,
    cause: Option<String>,
    fill: Option<(u64, u64)>,
    turn_id: Option<String>,
    rounds: u32,
    tool_calls: u32,
    tool_errors: u32,
    ttfb_ms: Option<u64>,
    model: String,
    finish: Option<String>,
    tokens: [u64; 3],
    cost: Option<u64>,
    nag: Option<PendingNag>,
}

impl TurnMeter {
    /// **A turn begins** — `turn_tail::run_turn`, beside `begin_turn_clock`. `fill` is the
    /// resident count and the window, both in the unit the operator reads.
    pub fn begin(&mut self, session: &str, door: &str, began_ms: u64, fill: Option<(u64, u64)>) {
        let cause = std::mem::take(&mut self.cause);
        *self = TurnMeter {
            began_ms: Some(began_ms),
            began: Some(Instant::now()),
            door: door.to_string(),
            cause,
            fill,
            ..TurnMeter::default()
        };
        NOW.with(|n| n.borrow_mut().insert(session.to_string(), (None, 0)));
    }

    /// The wake's cause, said before the turn it causes begins ([`wake`]).
    pub fn cause(&mut self, cause: &str) {
        self.cause = Some(cause.to_string());
    }

    /// The turn this meter is in, once its first round has named it.
    pub fn turn_id(&self) -> &str {
        self.turn_id.as_deref().unwrap_or("")
    }

    /// **One model round** — `run_rounds`, where the round's `TurnMetrics` is kept.
    /// `ledger` is the box's own count of the prompt the provider counted as
    /// `m.prompt_tokens` (R72's two units, side by side).
    pub fn round(
        &mut self,
        session: &str,
        transcript: &str,
        m: &TurnMetrics,
        ledger: u64,
        provider: Option<&str>,
    ) {
        if self.turn_id.is_none() {
            self.turn_id = Some(m.turn_id.clone());
            self.ttfb_ms = m.first_byte_ms;
        }
        let round = self.rounds;
        self.rounds += 1;
        self.model = m.model.clone();
        self.finish = Some(finish_word(&m.finish_reason));
        self.tokens[0] += m.prompt_tokens;
        self.tokens[1] += m.cached_tokens;
        self.tokens[2] += m.predicted_tokens;
        if let Some(c) = m.cost.micros_usd {
            *self.cost.get_or_insert(0) += c;
        }
        NOW.with(|n| {
            n.borrow_mut()
                .insert(session.to_string(), (self.turn_id.clone(), self.rounds))
        });
        if !letibot_metrics::enabled() {
            return;
        }
        Event::new(kind::ROUND)
            .session(session)
            .transcript(transcript)
            .turn(self.turn_id(), Some(round))
            .dur(m.wall_ms)
            .name(m.model.as_str())
            .outcome(finish_word(&m.finish_reason))
            .n1(m.prompt_tokens)
            .n2(m.cached_tokens)
            .n3(m.predicted_tokens)
            .attr_opt("ttfb_ms", m.first_byte_ms)
            .attr("prompt_ms", m.prompt_ms.round() as u64)
            .attr("predicted_ms", m.predicted_ms.round() as u64)
            .attr("ledger_tokens", ledger)
            .attr("server_prompt_tokens", m.prompt_tokens_server)
            .attr("id_slot", m.id_slot)
            .attr("round_turn_id", m.turn_id.as_str())
            .attr_opt("provider", provider)
            .attr_opt("cost_micros_usd", m.cost.micros_usd)
            .emit();
    }

    /// **One tool call** — the round loop, around `runtime.invoke`. The memory is what the
    /// call left on this thread (`bash`'s foreground job), taken here so it cannot be read by
    /// the next call.
    pub fn tool(&mut self, session: &str, r: &ToolResult, started: Instant) {
        let mem = letibot_tools::exec::scope::take_memory();
        let ok = matches!(r.outcome, letibot_transcript::ToolOutcome::Ok);
        self.tool_calls += 1;
        if !ok {
            self.tool_errors += 1;
        }
        if !letibot_metrics::enabled() {
            return;
        }
        let inline = r.payload.len() as u64;
        let full = r
            .spill
            .as_ref()
            .map(|s| s.full_bytes as u64)
            .unwrap_or(inline);
        Event::new(kind::TOOL_CALL)
            .session(session)
            .turn(self.turn_id(), Some(self.rounds.saturating_sub(1)))
            .dur(started.elapsed().as_millis() as u64)
            .name(r.name.as_str())
            .outcome(outcome_word(&r.outcome))
            .n1(inline)
            .n2(full)
            .n3(mem.map(|m| m.peak / 1024).unwrap_or(0))
            .attr_opt("cap_kib", mem.and_then(|m| m.cap).map(|c| c / 1024))
            .attr_opt("oom_kills", mem.map(|m| m.oom_kills).filter(|n| *n > 0))
            .attr("repairs", r.repairs.len())
            .attr("edit", r.edit.is_some())
            .attr("spilled", r.spill.is_some())
            .emit();
    }

    /// **A plan check was delivered as this turn** — `Harness::nag_turn`. `row` is the row it
    /// named and `status` that row's status as the check saw it; [`TurnMeter::end`] says whether
    /// the turn the check opened moved it.
    pub fn nag(
        &mut self,
        session: &str,
        row: Option<&str>,
        status: Option<String>,
        operator_row: bool,
    ) {
        if let Some(r) = row {
            self.nag = Some(PendingNag {
                row: r.to_string(),
                status: status.clone(),
            });
        }
        if !letibot_metrics::enabled() {
            return;
        }
        Event::new(kind::NAG)
            .session(session)
            .name("plan-check")
            .outcome("sent")
            .attr_opt("row", row)
            .attr_opt("row_status", status)
            .attr("operator_row", operator_row)
            .emit();
    }

    /// **The turn is over** — `turn_tail::run_turn`, beside `end_turn_clock`. `board` reads the
    /// named row's status now, for a nag's follow-up. One `turn.start` (stamped when it began)
    /// and one `turn.end`.
    pub fn end(
        &mut self,
        session: &str,
        transcript: &str,
        out: Option<&Result<Reply, HarnessError>>,
        board: impl Fn(&str) -> Option<String>,
    ) {
        NOW.with(|n| n.borrow_mut().remove(session));
        let Some(began_ms) = self.began_ms.take() else {
            return;
        };
        // A door that found nothing to say ran no turn: no rows.
        let Some(out) = out else {
            self.nag = None;
            return;
        };
        let nag = self.nag.take();
        if !letibot_metrics::enabled() {
            return;
        }
        let turn = self.turn_id().to_string();
        if let Some(n) = nag {
            let now = board(&n.row);
            Event::new(kind::NAG_FOLLOWUP)
                .session(session)
                .turn(&turn, None)
                .name("plan-check")
                .outcome(if now != n.status {
                    "touched"
                } else {
                    "untouched"
                })
                .attr("row", n.row)
                .attr_opt("status_before", n.status)
                .attr_opt("status_after", now)
                .emit();
        }
        let outcome = match out {
            Ok(r) if r.truncated => "length".to_string(),
            Ok(_) => self.finish.clone().unwrap_or_else(|| "eos".into()),
            Err(HarnessError::ContextWall { .. }) => "wall".into(),
            Err(HarnessError::NoProgress { .. }) => "stalled".into(),
            Err(HarnessError::LoopBound { .. }) => "loop-bound".into(),
            Err(_) => "failed".into(),
        };
        let (fill, window) = self.fill.unwrap_or((0, 0));
        let dur = self
            .began
            .map(|b| b.elapsed().as_millis() as u64)
            .unwrap_or(0);
        Event::new(kind::TURN_START)
            .at(began_ms)
            .session(session)
            .transcript(transcript)
            .turn(&turn, None)
            .name(self.door.as_str())
            .n1(fill)
            .n2(window)
            .n3(self.rounds as u64)
            .attr("model", self.model.as_str())
            .attr_opt("cause", self.cause.take())
            .emit();
        Event::new(kind::TURN_END)
            .session(session)
            .transcript(transcript)
            .turn(&turn, None)
            .dur(dur)
            .name(self.model.as_str())
            .outcome(outcome)
            .n1(self.tokens[0])
            .n2(self.tokens[1])
            .n3(self.tokens[2])
            .attr("door", self.door.as_str())
            .attr("rounds", self.rounds)
            .attr("tool_calls", self.tool_calls)
            .attr("tool_errors", self.tool_errors)
            .attr_opt("ttfb_ms", self.ttfb_ms)
            .attr_opt("cost_micros_usd", self.cost)
            .emit();
    }
}

/// **Start this daemon's metrics writer** — `metrics.db` beside the store (so on the operator's
/// box `~/.local/share/letibot/metrics.db`, and a test's temp store gets a temp file), under the
/// `[metrics]` section of `providers.toml` (on by default). A daemon with no store records
/// nothing; a file that will not open is said on stderr and the daemon starts without it —
/// metrics are never a reason a daemon does not start.
pub fn start_writer(cfg: &crate::config::Config) {
    let Some(store) = &cfg.store else {
        return;
    };
    let raw = letibot_provider::keys::metrics(None);
    let settings = letibot_metrics::store::Settings::from_raw(
        raw.enabled.as_deref(),
        raw.keep_days.as_deref(),
        raw.snippets.as_deref(),
        raw.rate_per_sec.as_deref(),
        raw.max_mb.as_deref(),
    );
    if !settings.enabled {
        eprintln!("  metrics: off ([metrics] enabled = false)");
        return;
    }
    let path = letibot_metrics::store::path_beside(store);
    let me = crate::mergequeue::lease::this_daemon();
    let id = letibot_metrics::store::Identity {
        daemon: me.owner.clone(),
        build: me.build.clone(),
    };
    match letibot_metrics::store::start(&path, id, settings) {
        Ok(Some(_)) => eprintln!("  metrics: {}", path.display()),
        Ok(None) => {}
        Err(e) => eprintln!(
            "  metrics: {} did not open ({e}); this daemon records no metrics",
            path.display()
        ),
    }
}

/// **The turn tail's two lines** — `turn_tail::run_turn`, beside the turn clock.
pub fn turn_begins(h: &mut crate::harness::Harness, door: &str) {
    let fill = letibot_metrics::enabled().then(|| {
        let resident = h.measure_resident().tokens;
        (resident, h.config().shown_wall().0)
    });
    let session = h.config().session_id.clone();
    let began = letibot_sessionlog::event::now_ms();
    h.meter.begin(&session, door, began, fill);
}

/// See [`turn_begins`]. `out` is `None` for a door that ran no turn; `walls` how many times
/// the tail continued past the wall — a turn that ENDS at the wall gets a `wall` row.
pub fn turn_ends(
    h: &mut crate::harness::Harness,
    out: Option<&Result<Reply, HarnessError>>,
    walls: usize,
) {
    let session = h.config().session_id.clone();
    let transcript = h.transcript_id().to_string();
    if let Some(Err(e)) = out {
        wall(&session, h.meter.turn_id(), e, walls);
    }
    let rows: Vec<(String, String)> = if letibot_metrics::enabled() {
        h.todo_snapshot_words()
    } else {
        Vec::new()
    };
    let board = |row: &str| rows.iter().find(|(c, _)| c == row).map(|(_, s)| s.clone());
    h.meter.end(&session, &transcript, out, board);
}

fn outcome_word(o: &letibot_transcript::ToolOutcome) -> String {
    // The outcome's own serde tag (`ok`, `abstained`, `backgrounded`, …), so the word in the
    // file is the word on the wire and no list here can fall behind the enum.
    serde_json::to_value(o)
        .ok()
        .and_then(|v| {
            v.get("outcome")
                .and_then(|k| k.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "other".into())
}

/// **One command's wait on the worker, and its run** (c12d044) — `Daemon::run`, around
/// `sessions.dispatch`. n1 is from the head's submit to the dequeue; `dur` the dispatch. The queue
/// depth the design names (n2) is not read in phase 1: no count of a hub's queue is to hand here.
/// **A moment on both clocks** — the wall clock a row is stamped with, and the monotonic one a
/// duration is measured on.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub ms: u64,
    pub at: Instant,
}

impl Clock {
    pub fn now() -> Clock {
        Clock {
            ms: letibot_metrics::now_ms(),
            at: Instant::now(),
        }
    }
}

pub fn outcome_of(o: &crate::sessions::Outcome) -> &'static str {
    use crate::sessions::Outcome;
    match o {
        Outcome::Replied(_) => "ok",
        Outcome::Compacted(_) => "compacted",
        Outcome::Failed(_) => "failed",
        Outcome::Ignored => "ignored",
        Outcome::HandedOn => "handed-on",
    }
}

#[allow(clippy::too_many_arguments)]
pub fn worker_cmd(
    session: &str,
    cmd: &QueuedCommand,
    at: Clock,
    outcome: &crate::sessions::Outcome,
) {
    if !letibot_metrics::enabled() {
        return;
    }
    let (started_ms, started) = (at.ms, at.at);
    let outcome = outcome_of(outcome);
    Event::new(kind::WORKER_CMD)
        .at(started_ms)
        .session(session)
        .dur(started.elapsed().as_millis() as u64)
        .name(cmd.kind.verb())
        .outcome(outcome)
        .n1(started_ms.saturating_sub(cmd.enqueued_ms))
        .attr("identity", cmd.identity.as_str())
        .emit();
}

/// **An idle pass on the worker that took long enough to matter** (> 250 ms) — the sweep, the
/// plan checks, the parking, the reviews. Under the floor it is not a row: a pass a second is
/// the idle arm's normal rate.
pub fn worker_pass(name: &str, started: Instant, touched: usize) {
    let ms = started.elapsed().as_millis() as u64;
    if ms <= 250 || !letibot_metrics::enabled() {
        return;
    }
    Event::new(kind::WORKER_PASS)
        .dur(ms)
        .name(name)
        .n1(touched as u64)
        .emit();
}

/// **A between-turns wake, and what woke it** — `Harness::wake`. `causes` is each cause's
/// count of notices, in the order they were gathered; `ran` whether it became a turn, whose
/// `turn.start` then carries the first cause.
pub fn wake(meter: &mut TurnMeter, session: &str, causes: &[(&str, usize)], ran: bool) {
    let first = causes
        .iter()
        .find(|(_, n)| *n > 0)
        .map(|(c, _)| *c)
        .unwrap_or("none");
    if ran {
        meter.cause(first);
    }
    if !letibot_metrics::enabled() {
        return;
    }
    let items: usize = causes.iter().map(|(_, n)| n).sum();
    let all: serde_json::Map<String, serde_json::Value> = causes
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(c, n)| (c.to_string(), (*n).into()))
        .collect();
    Event::new(kind::WAKE)
        .session(session)
        .name(first)
        .outcome(if ran { "ran" } else { "empty" })
        .n1(items as u64)
        .attr("causes", serde_json::Value::Object(all))
        .emit();
}

/// **A result reaching the model** (R79) — the one call in `DeliveryLog::record`, R79's single
/// record site, so every rule and door it has reaches this file. Mid-turn when the session's
/// current turn has already run a round: a delivery at a turn's start is not an interruption.
pub fn delivered_all(session: &str, rows: &[crate::delivery::Delivered<'_>]) {
    rows.iter().for_each(|d| delivered(session, d));
}

/// One of [`delivered_all`]'s rows.
pub fn delivered(session: &str, d: &crate::delivery::Delivered<'_>) {
    if !letibot_metrics::enabled() {
        return;
    }
    let (turn, rounds) = now_of(session).unwrap_or((None, 0));
    let waited = d
        .delivered
        .duration_since(d.arrived)
        .unwrap_or_default()
        .as_millis() as u64;
    let rule = d.rule.word();
    let how = match rule.split(':').next().unwrap_or("") {
        "immediate" => "immediate",
        "starvation" => "starved",
        // Never held: it arrived while nothing was running (R79's turn-end rule).
        "idle" => "idle",
        _ => "batched",
    };
    Event::new(kind::DELIVERY)
        .session(session)
        .turn(
            turn.as_deref().unwrap_or(""),
            (rounds > 0).then(|| rounds - 1),
        )
        .dur(waited)
        .name(d.kind.word())
        .outcome(how)
        .n1(d.batch as u64)
        .n2(rounds as u64)
        .attr("rule", rule)
        .attr("handle", d.handle)
        .attr("mid_turn", rounds > 0)
        .attr("door", if rounds > 0 { "steer" } else { "turn-start" })
        .emit();
}

/// **A compaction** — automatic (`pre-wall` / `refused-door`) or the operator's. `before` and
/// `after` in the unit the operator reads; the ledger pair beside them (R72/R73).
#[allow(clippy::too_many_arguments)]
pub fn compaction(
    session: &str,
    door: &str,
    outcome: &str,
    started: Instant,
    (before, after): (u64, u64),
    ledger: (u64, u64),
    unit: &str,
) {
    if !letibot_metrics::enabled() {
        return;
    }
    let turn = now_of(session).and_then(|(t, _)| t).unwrap_or_default();
    Event::new(kind::COMPACTION)
        .session(session)
        .turn(&turn, None)
        .dur(started.elapsed().as_millis() as u64)
        .name(door)
        .outcome(outcome)
        .n1(before)
        .n2(after)
        .attr("before_ledger", ledger.0)
        .attr("after_ledger", ledger.1)
        .attr("unit", unit)
        .emit();
}

/// [`compaction`] for the operator's `/compact` (`Sessions::compact`), measured off the harness.
pub fn operator_compaction(h: &crate::harness::Harness, before: u64, started: Instant, ok: bool) {
    let after = h.ledger_len() as u64;
    let c = h.config();
    compaction(
        &c.session_id,
        "operator",
        if ok { "ok" } else { "failed" },
        started,
        (c.shown_tokens(before), c.shown_tokens(after)),
        (before, after),
        &h.token_unit(),
    );
}

/// **A turn stopped at the context wall** — the turn tail, which every wall reaches through.
pub fn wall(session: &str, turn: &str, e: &HarnessError, continues: usize) {
    let HarnessError::ContextWall {
        rounds,
        resident,
        window,
        unit,
        ledger,
    } = e
    else {
        return;
    };
    if !letibot_metrics::enabled() {
        return;
    }
    Event::new(kind::WALL)
        .session(session)
        .turn(turn, None)
        .outcome("stopped")
        .n1(*resident)
        .n2(*window)
        .n3(*rounds as u64)
        .attr("unit", unit.as_str())
        .attr("ledger_resident", ledger.0)
        .attr("ledger_window", ledger.1)
        .attr("continues", continues)
        .emit();
}

/// **A wall controller transition** (`crate::wall`) — the judgement a fold is given once a
/// count is MEASURED. Same kind as [`wall`] (the turn stop), told apart by `outcome`:
/// `helped`, `escalated`, `stuck`, `released`. `n1` is what was folded (0 for a release),
/// `n2` the measured count, `n3` the window; the fold and, for `escalated`, the next rung
/// are attributes. The `compaction` row's `after` is the base's estimate; this row is the
/// measurement it is judged by (pg-noop 2026-10-11: ≈ 1,039,055, measured 507,355).
pub fn wall_change(session: &str, c: &crate::wall::Change, window: u64, unit: &str) {
    if !letibot_metrics::enabled() {
        return;
    }
    use crate::wall::Change;
    let (outcome, fold, next, before, measured) = match *c {
        Change::Helped {
            fold,
            before,
            measured,
            ..
        } => ("helped", fold.name(), "", before, measured),
        Change::Escalated {
            fold,
            next,
            before,
            measured,
        } => ("escalated", fold.name(), next.name(), before, measured),
        Change::Stuck { before, measured } => ("stuck", "sliced", "", before, measured),
        Change::Released { measured } => ("released", "", "", 0, measured),
    };
    let turn = now_of(session).and_then(|(t, _)| t).unwrap_or_default();
    Event::new(kind::WALL)
        .session(session)
        .turn(&turn, None)
        .outcome(outcome)
        .n1(before)
        .n2(measured)
        .n3(window)
        .attr("fold", fold)
        .attr("next", next)
        .attr("unit", unit)
        .emit();
}

/// **R71's context claim, judged** — beside the jsonl line (dual-written in phase 1). The
/// sentence travels as the `snippet`, which the writer keeps for this kind by default
/// (the operator's decision 5) and strips if `[metrics] snippets` says otherwise.
pub fn context_claim(session: &str, f: &crate::context_claim::Finding) {
    if !letibot_metrics::enabled() {
        return;
    }
    use crate::context_claim::Verdict;
    let (turn, rounds) = now_of(session).unwrap_or((None, 0));
    let outcome = match (&f.verdict, f.fill) {
        (Verdict::Slap(_), _) => "slapped",
        (Verdict::Repeated(_), _) => "repeated",
        (Verdict::Quiet, Some(fill)) if fill.at_wall => "at-wall",
        (Verdict::Quiet, Some(_)) => "true-report",
        (Verdict::Quiet, None) => "unmeasured",
    };
    Event::new(kind::CONTEXT_CLAIM)
        .session(session)
        .turn(turn.as_deref().unwrap_or(""), rounds.checked_sub(1))
        .outcome(outcome)
        .n1(f.fill.map(|x| x.resident).unwrap_or(0))
        .n2(f.fill.map(|x| x.window).unwrap_or(0))
        .n3((f.claim.score * 1000.0).round().max(0.0) as u64)
        .attr(
            "snippet",
            f.claim.sentence.chars().take(200).collect::<String>(),
        )
        .emit();
}

/// **The daemon's memory** (R66) — memwatch, every fifth tick and at every census step.
pub fn mem_sample(
    rss: u64,
    live: usize,
    peak: usize,
    threads: usize,
    census: &[(letibot_sessionlog::hub::HubCensus, usize)],
) {
    if !letibot_metrics::enabled() {
        return;
    }
    let sessions = census.len();
    let heads: usize = census.iter().map(|(c, _)| c.heads).sum();
    const MIB: u64 = 1 << 20;
    Event::new(kind::MEM_SAMPLE)
        .n1(rss / MIB)
        .n2(live as u64 / MIB)
        .n3(peak as u64 / MIB)
        .attr("threads", threads)
        .attr("sessions", sessions)
        .attr("heads", heads)
        .emit();
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_sessionlog::CommandKind;

    fn reply() -> Reply {
        Reply {
            text: String::new(),
            metrics: Vec::new(),
            rounds: 1,
            tool_calls: 0,
            truncated: false,
        }
    }

    /// **c12d044 as a number**: the wait is dequeue minus the head's submit, the run is the
    /// dispatch, and the verb is the command's own.
    #[test]
    fn a_worker_command_row_is_its_wait_and_its_run() {
        let cmd = QueuedCommand {
            head_id: "h1".into(),
            identity: "dead".into(),
            client_request_id: "c1".into(),
            at_seq: 0,
            enqueued_ms: 1_000,
            kind: CommandKind::Prompt {
                text: "hi".into(),
                attachments: Vec::new(),
            },
        };
        let ((), got) = letibot_metrics::capture(|| {
            let at = Clock {
                ms: 30_000,
                at: Instant::now(),
            };
            worker_cmd("s-1", &cmd, at, &crate::sessions::Outcome::Ignored);
        });
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, kind::WORKER_CMD);
        assert_eq!(got[0].name.as_deref(), Some("prompt"));
        assert_eq!(got[0].n[0], Some(29_000), "29 s in the queue");
        assert_eq!(got[0].ts_ms, 30_000, "stamped at the dequeue");
    }

    /// **R79's record is the delivery row** — mid-turn when the session's turn has run a
    /// round, at the turn's start otherwise; the wait, the rule and the batch carried over.
    #[test]
    fn a_delivery_row_knows_whether_it_interrupted_a_turn() {
        use crate::delivery::{Delivered, ResultKind, Rule, Why};
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let d = Delivered {
            handle: "j7",
            kind: ResultKind::Job,
            arrived: at,
            delivered: at + std::time::Duration::from_millis(4_500),
            rule: Rule::Immediate(Why::Needed),
            batch: 2,
        };
        let mut meter = TurnMeter::default();
        let ((), got) = letibot_metrics::capture(|| {
            meter.begin("s-d", "prompt", 0, None);
            delivered("s-d", &d);
            NOW.with(|n| n.borrow_mut().insert("s-d".into(), (Some("t#1".into()), 2)));
            delivered("s-d", &d);
        });
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].get("mid_turn"), Some(&false.into()));
        assert_eq!(got[0].get("door"), Some(&"turn-start".into()));
        assert_eq!(got[1].get("mid_turn"), Some(&true.into()));
        assert_eq!(got[1].turn_id.as_deref(), Some("t#1"));
        assert_eq!(got[1].round, Some(1));
        assert_eq!(got[1].dur_ms, Some(4_500), "arrived → delivered");
        assert_eq!(got[1].outcome.as_deref(), Some("immediate"));
        assert_eq!(got[1].get("rule"), Some(&"immediate:needed".into()));
        assert_eq!(got[1].n[0], Some(2), "the batch");
    }

    /// **A wake says what woke it**, and the turn it starts carries the cause.
    #[test]
    fn a_wake_row_names_its_first_cause_and_the_turn_carries_it() {
        let mut meter = TurnMeter::default();
        let ((), got) = letibot_metrics::capture(|| {
            meter.begin("s-w", "wake", 5, Some((10, 100)));
            wake(
                &mut meter,
                "s-w",
                &[("queue", 0), ("monitor", 0), ("result", 2), ("due", 1)],
                true,
            );
            meter.end("s-w", "tr", Some(&Ok(reply())), |_| None);
        });
        let w = got.iter().find(|e| e.kind == kind::WAKE).unwrap();
        assert_eq!(w.name.as_deref(), Some("result"));
        assert_eq!(w.outcome.as_deref(), Some("ran"));
        assert_eq!(w.n[0], Some(3));
        let s = got.iter().find(|e| e.kind == kind::TURN_START).unwrap();
        assert_eq!(s.name.as_deref(), Some("wake"));
        assert_eq!(s.get("cause"), Some(&"result".into()));
        assert_eq!(s.ts_ms, 5, "stamped when it began");
        assert_eq!((s.n[0], s.n[1]), (Some(10), Some(100)), "fill and window");
    }

    /// **R77: did the turn a check opened touch the row it named?**
    #[test]
    fn a_nag_follow_up_says_whether_the_named_row_moved() {
        for (after, want) in [
            (Some("completed"), "touched"),
            (Some("pending"), "untouched"),
        ] {
            let mut meter = TurnMeter::default();
            let ((), got) = letibot_metrics::capture(|| {
                meter.begin("s-n", "nag", 0, None);
                meter.nag(
                    "s-n",
                    Some("finish the migration"),
                    Some("pending".into()),
                    true,
                );
                meter.end("s-n", "tr", Some(&Ok(reply())), |row| {
                    assert_eq!(row, "finish the migration");
                    after.map(str::to_string)
                });
            });
            let nag = got.iter().find(|e| e.kind == kind::NAG).unwrap();
            assert_eq!(nag.get("operator_row"), Some(&true.into()));
            let f = got.iter().find(|e| e.kind == kind::NAG_FOLLOWUP).unwrap();
            assert_eq!(f.outcome.as_deref(), Some(want));
        }
    }

    #[test]
    fn a_door_that_ran_no_turn_writes_no_turn_rows() {
        let mut meter = TurnMeter::default();
        let ((), got) = letibot_metrics::capture(|| {
            meter.begin("s-e", "wake", 0, None);
            meter.end("s-e", "tr", None, |_| None);
        });
        assert!(got.is_empty(), "{got:#?}");
    }

    #[test]
    fn a_wall_row_carries_both_units() {
        let e = HarnessError::ContextWall {
            rounds: 3,
            resident: 990_000,
            window: 1_000_000,
            unit: "deepseek tokens".into(),
            ledger: (3_400_000, 3_500_000),
        };
        let ((), got) = letibot_metrics::capture(|| wall("s", "t#1", &e, 1));
        assert_eq!(got[0].n, [Some(990_000), Some(1_000_000), Some(3)]);
        assert_eq!(got[0].get("ledger_resident"), Some(&3_400_000u64.into()));
        assert_eq!(got[0].get("continues"), Some(&1.into()));
    }
}
