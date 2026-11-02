//! **The context wall**: compaction, reseat and forking - the ways a long
//! conversation is carried onto a smaller prompt, and the arithmetic that decides
//! when each is needed.
use super::*;

/// A fork registered in the store but not yet the session's conversation. See
/// `Store::put_fork_unsealed`.
pub(crate) struct UnsealedFork {
    transcript_id: String,
    fingerprint: String,
}

/// What a fork carries verbatim, and why: bundled rather than passed as three arguments, because a tail has
/// contents, it may have started mid-exchange, and it has a reason for being what it is. Splitting them is how
/// one of the three comes to be updated without the others.
pub struct ForkTail<'a> {
    pub items: &'a [TranscriptItem],
    /// Non-zero when the tail could not start at an exchange boundary.
    pub split: Option<TailSplit>,
    /// One of [`letibot_turn::TAIL_BECAUSE`], on the wire as `tail.because`. Empty for a
    /// fork that carries no tail *and* is not a compaction - a re-seat, a re-ingest  -
    /// where a reason would be a claim about a decision nobody made.
    pub because: &'static str,
}

impl ForkTail<'_> {
    /// No tail at all, and no reason. What a re-seat and a re-ingest pass.
    pub const NONE: ForkTail<'static> = ForkTail {
        items: &[],
        split: None,
        because: "",
    };
}

/// What a compaction fork actually did, in numbers rather than the word compacted: the claim is cheap and the
/// evidence is what makes it checkable. `was_tokens` against `base_tokens` is the reduction an operator can see,
/// and `parent_id` with `forked_at` is where the old conversation went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkReport {
    pub transcript_id: String,
    pub parent_id: String,
    /// The session log's seq at the fork.
    pub forked_at: u64,
    /// The old transcript's token count, measured before anything was dropped.
    pub was_tokens: usize,
    /// The new base: the prefix plus the one summary item.
    pub base_tokens: usize,
    /// The summary ran out of room before it finished, carried so the operator is told on the same line that
    /// says compaction worked: it did work, and what it produced is partial, and those are two facts.
    pub truncated: bool,
    /// How many of the newest items were carried verbatim rather than summarised. Non-zero only for a remote
    /// model: a local one is bounded by the KV cache in VRAM, where a tail competes with the pressure the
    /// compaction was called to relieve, while a remote model is bounded by a bill.
    pub tail_items: usize,
    /// Items of the newest exchange left out of the verbatim tail, when it could not start at an exchange
    /// boundary: one big file read is larger than the whole budget. Disclosed rather than left to be discovered,
    /// because such a tail opens with an answer to something no longer present.
    pub tail_dropped: Option<usize>,
    /// The tail as the wire carries it: role and text, in order, so a head can draw the recent past instead of
    /// parsing the note's count. Role is `operator` or `agent`, the vocabulary `Speaker` already uses.
    pub tail_turns: Vec<letibot_sessionlog::event::CompactionTurn>,
    /// Why the tail is what it is - [`ForkTail::because`], carried through so the report
    /// can put it on the wire without recomputing a decision that has already been made.
    pub tail_because: String,
    /// The tail as the WIRE describes it, carrying the reason in the shape both readers share: the object the
    /// warning carries, so the sentence a head draws and the row this report becomes cannot disagree.
    /// `None` for a fork that is not a compaction, where a reason would be a claim nobody made.
    pub tail_why: Option<letibot_sessionlog::event::CompactionTail>,
}

/// What a compaction left behind: the fork's numbers and the summary turn's. Two reports because two things
/// happened and both are disclosed, and `cached_tokens` against `reusable` is the measured cache hit the whole
/// cached strategy exists for. The gap between them is the server's, never ours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactReport {
    pub fork: ForkReport,
    pub summary_turn: CompactionOutcome,
    /// Tools the fork's prompt announces that the old one did not: a compaction forks onto the prompt this
    /// daemon seats now, so a conversation started before a tool existed picks it up by compacting, and the
    /// operator is told.
    pub gained: Vec<String>,
    /// Tools the old prompt announced and the new one does not.
    pub lost: Vec<String>,
    /// Did the operator watch the summary being written? The ordinary compaction streams it through the
    /// session's own sink, while the overrun paths summarise a SCRATCH transcript through a `NullSink`, because
    /// forwarding that progress made a head draw the scratch prompt's count as the session's. False means publish.
    pub summary_was_streamed: bool,
}

/// What a re-seat did: the fork it made, the summary that carried the conversation
/// across, and - the part the operator asked for - which tools the model can now
/// call that it could not before, and which it has lost.
pub struct ReseatReport {
    pub fork: ForkReport,
    pub summary_turn: CompactionOutcome,
    pub gained: Vec<String>,
    pub lost: Vec<String>,
    /// Tools on both sides whose schema changed: the case that started the stale-prompt notice, and the one
    /// the gained/lost pair is silent about. Named here so the re-seat's report can say the schema the model now
    /// reads is not the one it was using.
    pub grew: Vec<String>,
    /// **What changed in each of `grew`, in a phrase** - `monitor (new \`github\`)`,
    /// `todo_write (\`todos[].needs[].kind\` gains \`monitor\`)` - so the model is told what to
    /// look for and not only that something moved. See [`schema_change`].
    pub grew_how: Vec<String>,
}

/// **Each grown tool, named with what changed in it** - [`grown_schemas`]'s names, each with
/// [`schema_change`]'s phrase when there is one.
pub(crate) fn grown_with_how(was: &[String], is: &[String]) -> Vec<String> {
    let (old, new) = (tool_schemas(was), tool_schemas(is));
    grown_schemas(was, is)
        .into_iter()
        .map(|name| match (old.get(&name), new.get(&name)) {
            (Some(a), Some(b)) => match schema_change(a, b) {
                Some(how) => format!("{name} ({how})"),
                None => name,
            },
            _ => name,
        })
        .collect()
}

/// What changed between two versions of one tool's schema, in a phrase: new and removed parameters at any
/// depth, values an enum gained or lost, and `wording` when only the text moved. `None` when the two cannot be
/// read. A re-seat used to say only `Changed: monitor`.
pub fn schema_change(was: &str, is: &str) -> Option<String> {
    let parse = |s: &str| -> Option<serde_json::Value> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        ["/function/parameters", "/parameters", "/input_schema"]
            .iter()
            .find_map(|p| v.pointer(p).cloned())
    };
    let (a, b) = (parse(was)?, parse(is)?);
    let mut out = Vec::new();
    schema_walk("", &a, &b, &mut out);
    if out.is_empty() {
        out.push("wording".to_string());
    }
    const MOST: usize = 6;
    if out.len() > MOST {
        let more = out.len() - MOST;
        out.truncate(MOST);
        out.push(format!("{more} more"));
    }
    Some(out.join("; "))
}

fn schema_walk(path: &str, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value;
    let at = |k: &str| {
        if path.is_empty() {
            k.to_string()
        } else {
            format!("{path}.{k}")
        }
    };
    if let (Some(Value::Object(pa)), Some(Value::Object(pb))) =
        (a.get("properties"), b.get("properties"))
    {
        for (k, vb) in pb {
            match pa.get(k) {
                None => out.push(format!("new `{}`", at(k))),
                Some(va) => schema_walk(&at(k), va, vb, out),
            }
        }
        for k in pa.keys().filter(|k| !pb.contains_key(*k)) {
            out.push(format!("no `{}`", at(k)));
        }
    }
    if let (Some(ia), Some(ib)) = (a.get("items"), b.get("items")) {
        schema_walk(&format!("{path}[]"), ia, ib, out);
    }
    // A union of shapes (`anyOf`/`oneOf`): its object variants, compared in order.
    for key in ["anyOf", "oneOf"] {
        if let (Some(Value::Array(va)), Some(Value::Array(vb))) = (a.get(key), b.get(key)) {
            let objects = |v: &Vec<Value>| -> Vec<Value> {
                v.iter()
                    .filter(|x| x.get("properties").is_some())
                    .cloned()
                    .collect()
            };
            for (x, y) in objects(va).iter().zip(objects(vb).iter()) {
                schema_walk(path, x, y, out);
            }
        }
    }
    if let (Some(Value::Array(ea)), Some(Value::Array(eb))) = (a.get("enum"), b.get("enum")) {
        let name = if path.is_empty() { "it" } else { path };
        let word = |v: &Value| {
            v.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string())
        };
        let gained: Vec<String> = eb.iter().filter(|v| !ea.contains(v)).map(word).collect();
        let lost: Vec<String> = ea.iter().filter(|v| !eb.contains(v)).map(word).collect();
        if !gained.is_empty() {
            out.push(format!(
                "`{name}` gains {}",
                gained
                    .iter()
                    .map(|g| format!("`{g}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !lost.is_empty() {
            out.push(format!(
                "`{name}` loses {}",
                lost.iter()
                    .map(|g| format!("`{g}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
}

/// **Tools seated on both sides whose schema text differs**, sorted - the `grew` half of the
/// stale-prefix comparison, as one function so the notice and the re-seat's report name the
/// same tools.
fn grown_schemas(was: &[String], is: &[String]) -> Vec<String> {
    let (was, is) = (tool_schemas(was), tool_schemas(is));
    is.iter()
        .filter(|(name, schema)| was.get(*name).is_some_and(|old| old != *schema))
        .map(|(name, _)| name.clone())
        .collect()
}

/// What the MODEL is told about a stale prompt, once, as information: the person owns the decision through a
/// card on their head, so the model's copy no longer asks it to keep saying so, which was the nagging reported.
/// It ends with whose decision it is, and it is delivered once per sentence, in the plan-check's turn.
pub fn stale_notice_for_the_model(said: &str) -> String {
    format!(
        "{said} For your information, said once: the person sees this on their screen as a \
         card offering `/reseat`, and whether to re-seat is their decision. Do not ask them to \
         re-seat and do not repeat this; carry on with the work, and if a tool you need is \
         missing from your prompt, say that plainly when it matters."
    )
}

/// What the model is told when a reseat lands: one line, and the three things it must not say - no token
/// count, no request to restate the re-seat (a restatement is not work), and no stop asking for `/reseat`,
/// which planted the word it forbade.
pub fn reseated_for_the_model(summarise: bool, r: &ReseatReport) -> String {
    let mut said = String::from("[reseat] Your system prompt and tools were updated");
    if summarise {
        said.push_str("; earlier history is summarised above");
    }
    said.push('.');
    if !r.gained.is_empty() {
        said.push_str(&format!(" New: {}.", r.gained.join(", ")));
    }
    if !r.lost.is_empty() {
        said.push_str(&format!(" Removed: {}.", r.lost.join(", ")));
    }
    if !r.grew.is_empty() {
        let named = if r.grew_how.is_empty() {
            &r.grew
        } else {
            &r.grew_how
        };
        said.push_str(&format!(
            " Changed (read before calling): {}.",
            named.join(", ")
        ));
    }
    said.push_str(" Carry on.");
    said
}

impl Harness {
    /// How big this conversation is in the units its wall is in, and the pair every `Config` conversion reads.
    /// Called before every wall decision, since the ledger moves between rounds and a stale pair would convert
    /// those rows by a ratio they have nothing to do with. Without a measurement it says so.
    pub fn measure_resident(&mut self) -> Resident {
        let len = self.session.ledger.len() as u64;
        if self.cfg.ledger_scale.is_none() {
            // A switch cleared the scale (`set_provider`, `set_own_server`): the anchor
            // was a measurement on the model before, and goes with it.
            self.wall_anchor = None;
            // The carried ratio still converts: nothing here has measured THIS model, so `measured` is `None`
            // and a line says so, but a pair measured on the model before the switch beats the ledger's bytes and
            // stops a fresh provider ending the turn at round 0.
            return Resident {
                tokens: self.cfg.shown_tokens(len),
                measured: None,
                since: 0,
            };
        }
        let Some(anchor) = &self.wall_anchor else {
            return Resident {
                tokens: self.cfg.shown_tokens(len),
                measured: None,
                since: 0,
            };
        };
        let (anchor, now) = roll_resident(
            anchor,
            &self.session.transcript_id,
            &self.session.ledger,
            &self.session.items,
            self.provider.is_some(),
        );
        self.wall_anchor = Some(anchor);
        if len > 0 && now.tokens > 0 {
            self.cfg.ledger_scale = Some((len, now.tokens));
        }
        now
    }

    /// The turn after a re-seat: the model is told it happened and answers. The same seam as
    /// `continue_after_wall` and the nag: a user item in the harness's voice, never readable as the operator's.
    /// A re-seat is a worker command, so it never runs inside a turn, and this is the next boundary.
    pub fn run_after_reseat(&mut self, text: &str) -> Result<Reply, HarnessError> {
        let mut text = text.to_string();
        let (riding, told) = self.batch_riding_in();
        if let Some(meanwhile) = riding {
            text.push_str("\n\n");
            text.push_str(&meanwhile);
        }
        self.compact_if_at_the_wall();
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        let out = self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        });
        self.the_batch_arrived(told, out.is_ok());
        out
    }

    /// The turn after the wall, without a human typing continue. A wall stop ends the turn half-done and the
    /// compaction that follows replaces the transcript, after which nothing ran; so this seam is a user item in
    /// the harness's voice. The tail calls it while there is room and a bound of continuations left.
    pub fn continue_after_wall(&mut self) -> Result<Reply, HarnessError> {
        // No "context wall" sentence: the context check tells the model not to size by it, and the
        // compaction note right above already says what the summary is. Sent only after a real
        // compaction.
        let mut text =
            "Continue from the summary above; don't redo what it records as done.".to_string();
        // A continuation is a turn the model takes anyway, so the held batch rides in.
        let (riding, told) = self.batch_riding_in();
        if let Some(meanwhile) = riding {
            text.push_str("\n\n");
            text.push_str(&meanwhile);
        }
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        let out = self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        });
        self.the_batch_arrived(told, out.is_ok());
        out
    }

    /// The same contract from the side that owns the harness: compact when the next turn would not fit,
    /// announce it in the daemon's own words, and apply the no-progress guard. Returns whether a compaction was
    /// ATTEMPTED, since a wall notice saying so needs it to be a fact.
    pub(crate) fn compact_if_at_the_wall(&mut self) -> bool {
        self.compact_at_the_wall(WallWhy::Tidy)
    }

    /// The other question: a turn was just REFUSED for context length, so is there a way forward? One gate
    /// apart, since this does not read `auto_compact`, which the guard turns off without freeing anything. The
    /// refusal is the evidence, and nothing stands this door down.
    pub(crate) fn compact_because_refused(&mut self) -> bool {
        self.compact_at_the_wall(WallWhy::Refused)
    }

    /// **One body, two questions.** `should_compact`/`at_the_wall` are the
    /// decision - the arithmetic is NOT copied - and `why` is the only thing the
    /// two doors disagree on.
    fn compact_at_the_wall(&mut self, why: WallWhy) -> bool {
        // Measured first, so the decision and the line are one count: the roll brings `ledger_scale` to this
        // ledger and the provider-sized count, and `should_compact` on the ledger figure below is then exactly the
        // provider's count against the provider's window.
        let now = self.measure_resident();
        let resident = self.session.ledger.len() as u64;
        // The one gate the two questions differ by: the pre-emptive door reads `auto_compact` and the threshold,
        // and the refused door reads neither. Which fold, and whether the pre-emptive door stands down, is the wall
        // controller's: one state, changed only by measured counts.
        let refused = matches!(why, WallWhy::Refused);
        if !refused
            && !self
                .cfg
                .pre_emptive_compact(resident, self.provider.is_some())
        {
            return false;
        }
        let Some(fold) = self.cfg.wall.fold_for(refused, &self.session.transcript_id) else {
            return false;
        };
        // Shown in the operator's units, decided in the ledger's: `should_compact` is the decision and the line
        // prints the count it was made on, in the window's own units. `Config::scale` and not `ledger_scale`, or a
        // converted count would print beside a unit it had just named as the provider's.
        let scale = self.cfg.scale();
        let (w, h) = self.cfg.shown_wall();
        let r = now.tokens;
        let unit = self.token_unit();
        let basis = now.basis();
        // A refusal-answer names what sent it, because the operator watching a
        // session that "compacts automatically" deserves to know why THIS one
        // compacted with the automatic compaction stood down.
        let because = match why {
            WallWhy::Tidy => String::new(),
            WallWhy::Refused => " The turn before this one was refused for length, so this \
                                 compaction is under duress: the refusal is the wall either \
                                 way."
                .into(),
        };
        let pressing = match fold {
            crate::wall::Fold::Ordinary => String::new(),
            f => format!(
                " The last fold was measured still at the wall, so this one is the {} fold.",
                f.name()
            ),
        };
        // **And it names the session, for the reason the sibling's line does**: the
        // log is one file every daemon appends to, and a child's compaction lines
        // land between its parent's work with nothing to say whose they were.
        eprintln!(
            "  {}: compacting: {r} of {w} {unit} resident{basis}, less than the {h} the \
             next turn needs; {} fold",
            self.cfg.session_id,
            fold.name()
        );
        // An announcement, not a row: the head keeps it as the state behind the progress line,
        // so one compaction draws one durable line, its report. An older head knows no such code
        // and draws the routine row it always drew.
        self.hub.publish(SessionEvent::Warning {
            code: "compacting".into(),
            detail: format!(
                "{r} of {w} {unit} resident{basis}, leaving less than the {h} the next \
                 turn needs — compacting now, as one more message so the prefix the \
                 server already holds is reused. This is the wall, not a judgement about \
                 the conversation.{because}{pressing}"
            ),
            compaction: None,
        });
        let (started, compacted) = (Instant::now(), self.compact_with(fold));
        let ok = compacted.is_ok();
        match compacted {
            Ok(report) => {
                // The new base measured in its own right - the fork re-bases the anchor
                // (`roll_resident`), so the after-count is the base's, not the old
                // history's ratio applied to it.
                let base = self.measure_resident();
                // An estimate is said as one, with `≈`, until the next round measures it. The report used to print
                // the base's estimate in the same breath as the measured count it was folded from, and the guard
                // below JUDGED that estimate.
                let estimated = self.cfg.wall.awaiting_on(&self.session.transcript_id)
                    || self.cfg.carried_note().is_some();
                let scales = crate::sessions::Scales {
                    before: scale,
                    // The base's own pair when one measured it; else the ratio the whole
                    // session is being converted by, which is what `measure_resident` used
                    // for the after-count as well.
                    after: self.cfg.ledger_scale.or(scale),
                    unit: Some(unit.clone()),
                    estimated,
                };
                self.hub.publish(SessionEvent::Warning {
                    code: "compacted".into(),
                    detail: crate::sessions::compaction_said(&report, scales.clone()),
                    compaction: Some(Box::new(crate::sessions::compaction_wire(
                        &report,
                        "compacted",
                        scales,
                        r,
                        w,
                        h,
                    ))),
                });
                // Whether it helped is not decided here: it used to be, on the base's estimate, for good. A
                // metered base is judged by the first round that measures it, and a local one inside `compact_with`,
                // where the count is exact. Either way the controller said it, once.
                //
                // The report is the one row this compaction leaves: a second row here repeated
                // these numbers and drew a second size for one fold. The log keeps the short
                // line an operator reads in a tail.
                let cut = if report.fork.truncated {
                    crate::sessions::SUMMARY_CUT_OFF
                } else {
                    ""
                };
                let said = compacted_line(r, base, &self.token_unit(), self.provider.is_some());
                eprintln!("  {}: {said}{cut}", self.cfg.session_id);
            }
            Err(e) => {
                // Announced and swallowed, as the sibling does: the room check in `submit_as_a_normal_session`
                // decides the turn's fate, so a compaction that did not run leaves no room, and this line is why the
                // operator can see it was tried rather than infer it never was.
                eprintln!("  {}: compaction FAILED: {e}", self.cfg.session_id);
                self.hub.publish(SessionEvent::Warning {
                    code: "auto_compact_failed".into(),
                    detail: format!(
                        "the automatic compaction did not run: {e}. This session is over \
                         its context budget and nothing made room, so the next turn will \
                         hit the context wall. `/compact` retries it."
                    ),
                    compaction: None,
                });
                // And if this is the first request after a model switch, say how to go back: the compaction runs
                // through this harness and not the turn tail, and it is the door the measured failure took.
                if let Some(said) = self.switch_back_notice(&e) {
                    self.hub.publish(SessionEvent::Warning {
                        code: "model_switch_refused".into(),
                        detail: said,
                        compaction: None,
                    });
                }
            }
        }
        self.meter_compaction(matches!(why, WallWhy::Tidy), ok, started, (r, resident));
        // It ran. Whether it HELPED is the controller's, in its own words either
        // way; what this answers is the narrower question a caller asks before
        // claiming an attempt was made.
        true
    }

    /// A measured count, into the wall controller: the one place its state changes on a measurement, and the
    /// one place that says so. `count` is in the window's units, either a round's `prompt_tokens` or a local
    /// base's exact size, and the judgement the no-progress guard used to make on an estimate is made here.
    pub(crate) fn observe_wall(&mut self, count: u64) {
        let (window, headroom) = self.cfg.shown_wall();
        if window == 0 {
            return;
        }
        let wall = crate::wall::Wall { window, headroom };
        let metered = self.provider.is_some();
        let transcript = self.session.transcript_id.clone();
        let Some(change) = self.cfg.wall.measured(&transcript, count, wall, metered) else {
            return;
        };
        let unit = self.token_unit();
        crate::instruments::wall_change(&self.cfg.session_id, &change, window, &unit);
        use crate::wall::Change;
        let (code, detail) = match change {
            Change::Helped {
                fold,
                before,
                estimate,
                measured,
            } => (
                "auto_compact",
                format!(
                    "measured: the {} fold of {before} left {measured} of {window} {unit} \
                     (estimated ≈ {estimate} before the provider counted it) — room again.",
                    fold.name()
                ),
            ),
            Change::Escalated {
                fold,
                next,
                before,
                measured,
            } => (
                "auto_compact",
                format!(
                    "measured: the {} fold of {before} left {measured} of {window} {unit}, \
                     still within {headroom} of the window — the next compaction is the {} \
                     fold, which keeps less verbatim.",
                    fold.name(),
                    next.name()
                ),
            ),
            Change::Stuck { before, measured } => {
                self.persist_stuck();
                (
                    "auto_compact_no_progress",
                    format!(
                        "compacted from {before} to {measured} {unit} (MEASURED by the \
                         provider) and that is STILL within {headroom} of the {window} \
                         window after every fold — ordinary, smaller tail, sliced — so \
                         compaction before a turn is off for this session rather than \
                         looping once per turn. A turn the provider refuses is still \
                         folded; a measured count under {} re-arms it, as do `/compact` and \
                         `/auto-compact on`. The summary itself is near the wall: start a \
                         fresh session, or raise --context-window if the server really \
                         has more.",
                        window.saturating_sub(crate::wall::RELEASE_HEADROOMS * headroom)
                    ),
                )
            }
            Change::Released { measured } => {
                self.persist_stuck();
                (
                    "auto_compact",
                    format!(
                        "re-armed: measured {measured} of {window} {unit}, comfortably below \
                         the wall, so compaction before a turn is on again for this session."
                    ),
                )
            }
        };
        eprintln!("  {}: {detail}", self.cfg.session_id);
        self.hub.publish(SessionEvent::Warning {
            code: code.into(),
            detail,
            compaction: None,
        });
        self.publish_settings();
    }

    /// **The operator's re-arm** - `/auto-compact on`. Clears `Stuck` (and whatever the
    /// controller was waiting for), and the operator's own `--no-auto-compact` with it:
    /// the verb says ON. Returns the sentence the verb answers with.
    pub fn rearm_auto_compact(&mut self) -> String {
        let was_off = !self.cfg.auto_compact;
        let was = self.cfg.wall.rearm();
        self.cfg.auto_compact = true;
        if was {
            self.persist_stuck();
        }
        self.publish_settings();
        match (was || was_off, self.cfg.wall.last_measured) {
            (false, _) => "automatic compaction was already on".into(),
            (true, Some(n)) => format!(
                "automatic compaction is on again; the last measured count was {n} {}",
                self.token_unit()
            ),
            (true, None) => "automatic compaction is on again".into(),
        }
    }

    /// **Whether the count is the estimate of a metered base no round has measured**  -
    /// the one count no wall judgement is made on (`crate::wall`).
    pub fn base_is_unmeasured(&self) -> bool {
        self.provider.is_some() && self.cfg.wall.awaiting_on(&self.session.transcript_id)
    }

    /// The wall controller's state, for a caller that reports it.
    pub fn wall(&self) -> &crate::wall::WallControl {
        &self.cfg.wall
    }

    /// **The controller's `Stuck` on the session row**, or its absence - the v19 pair is
    /// the durable half, so a restart comes back stuck exactly when the controller was.
    fn persist_stuck(&mut self) {
        let pair = self.cfg.wall.stuck_pair();
        if let Some(store) = &self.store
            && let Err(e) = store.set_auto_compact_stood_down(&self.cfg.session_id, pair)
        {
            // Said, not swallowed: the in-memory state is right for this process; the
            // row is what a restart reads.
            eprintln!(
                "  {}: could not write the wall controller's state to the store: {e}",
                self.cfg.session_id
            );
            self.hub.publish(SessionEvent::Warning {
                code: "auto_compact_no_progress".into(),
                detail: format!(
                    "the wall controller's state could not be written down: {e}. It holds \
                     until this daemon stops."
                ),
                compaction: None,
            });
        }
    }

    pub fn compact(&mut self) -> Result<CompactReport, HarnessError> {
        self.compact_with(crate::wall::Fold::Ordinary)
    }

    /// A compaction with the fold named: `compact` is the ordinary one `/compact` asks for, and the wall
    /// controller names a harder one when a measured count said the last did not make room. Every fold that lands
    /// is reported to the controller HERE, whoever asked, so a `/compact` on a stuck session restarts the ladder.
    pub fn compact_with(&mut self, fold: crate::wall::Fold) -> Result<CompactReport, HarnessError> {
        let before = self.measure_resident().tokens;
        self.compacting = true;
        let out = self.compact_inner(fold);
        self.compacting = false;
        if out.is_ok() {
            let base = self.measure_resident();
            let transcript = self.session.transcript_id.clone();
            if self.cfg.wall.folded(fold, &transcript, before, base.tokens) {
                // It was stuck; the row says so until it is told otherwise.
                self.persist_stuck();
            }
            if self.provider.is_none() {
                self.observe_wall(base.tokens);
            }
            self.publish_settings();
        }
        out
    }

    /// The prompt this daemon would seat a NEW conversation with, and the store row for it, or `None` when that
    /// is already the prompt being spoken. `/reseat` and every compaction fork want the same two values, and the
    /// probe render is what makes the row honest. Public so the re-seat half is testable offline.
    pub fn reseat_target(&mut self) -> Result<Option<(StablePrefix, String)>, HarnessError> {
        // The standing notes are re-read HERE, on every base rebuild, because every fork computes its next
        // prefix through here: `/compact`, the wall, `/reseat`, `/reingest`. The section is swapped by marker, so
        // overrides and an appended fabric block are carried through. Unchanged files make it a no-op.
        match self.reseat_candidate() {
            None => Ok(None),
            Some(next) => self.seat_target(next).map(Some),
        }
    }

    /// The prompt a fork would land on, decided and NOT yet recorded: the first half of `reseat_target`, for a
    /// caller with a check to make first. `reingest` once refused an over-window carry after the prompt had moved
    /// and a row had been written, so every refused attempt left an orphan. `None` when unchanged or storeless.
    fn reseat_candidate(&mut self) -> Option<StablePrefix> {
        let declared = crate::standing_notes::preloaded(self.skills.as_ref(), &self.cfg.skills);
        let notes = crate::standing_notes::section_with_skills(
            &self.cfg.workspace,
            &self.cfg.global_notes(),
            &self.cfg.box_dirs,
            self.engine.vocab(),
            Some(self.skills.as_ref()),
            declared.as_deref(),
        );
        let system = crate::standing_notes::replace(&self.cfg.system, notes.as_deref());
        let next = StablePrefix {
            system,
            tools_json: self.render.tools_json(&self.runtime.registry.schemas()),
        };
        if next == self.prefix {
            self.cfg.system = next.system;
            return None;
        }
        // No store is not a refusal here - a compaction without one refuses later
        // and for its own reason. It is simply nowhere to record a new prefix, so
        // the fork keeps the old one.
        self.store.as_ref()?;
        Some(next)
    }

    /// **Record the prompt [`Self::reseat_candidate`] decided**: its `stable_prefix` row, and
    /// `cfg.system` moved to it - the second half of [`Self::reseat_target`].
    fn seat_target(&mut self, next: StablePrefix) -> Result<(StablePrefix, String), HarnessError> {
        let measured = self
            .engine
            .open(&format!("{}#reseat-probe", self.cfg.session_id), &next)
            .map_err(|e| HarnessError::Setup(format!("rendering the new prompt: {e}")))?;
        let rec = StablePrefixRecord {
            dialect_sha: hex32(&self.render.spec().template_sha),
            system: next.system.clone(),
            tools_json: next.tools_json.clone(),
            tokens: measured.ledger.prefix_tokens().to_vec(),
            h_init: measured.ledger.h_init(),
            vocab_source: self.engine.vocab().source().to_string(),
        };
        drop(measured);
        let id = self
            .store
            .as_ref()
            .ok_or_else(|| HarnessError::Setup("no store to record the new prompt in".into()))?
            .put_stable_prefix(&rec)
            .map_err(|e| HarnessError::Store(e.to_string()))?;
        self.cfg.system = next.system.clone();
        Ok((next, id))
    }

    /// Re-seat without summarising: carry the whole conversation across through `fork_to_summary`, since a
    /// tool's schema lives in the stable prefix and a schema change needs a fork. It costs a cold prefill of the
    /// whole history, the trade against a summary, and refuses where the carry could not be prefilled.
    pub fn reingest(&mut self) -> Result<ReseatReport, HarnessError> {
        if self.store.is_none() {
            return Err(HarnessError::Setup(
                "re-seating forks the conversation onto a new prompt, and a fork needs a \
                 store to write it to; this session has none."
                    .into(),
            ));
        }
        // Decided, not yet recorded: the row and `cfg.system` wait for the fit check below, so a
        // refusal changes nothing.
        let Some(next) = self.reseat_candidate() else {
            return Err(HarnessError::Setup(
                "this conversation's prompt already carries exactly the tools that are \
                 seated, so there is nothing to re-seat."
                    .into(),
            ));
        };
        // The carry has to fit, and fit has to be measured in ONE unit: the ledger over-counts against a
        // provider, since it holds the reasoning rows the provider drops. Convert to the PROVIDER's units and compare
        // there, and when nobody has measured the ratio, do NOT refuse, because the error is all one way.
        let per_item: Vec<u64> = (0..self.session.items.len())
            .map(|i| {
                self.session
                    .ledger
                    .item_tokens(i)
                    .map(|t| t.len() as u64)
                    .unwrap_or(0)
            })
            .collect();
        let resident: u64 = per_item.iter().sum();
        // In the provider's units where they differ, and the ledger's where they
        // do not - a local endpoint counts with the vocabulary the ledger uses, so
        // `provider_tokens` is `None` there and the ledger figure IS the figure.
        let carried = match self.provider.is_some() {
            true => self.cfg.provider_tokens(resident),
            false => Some(resident),
        };
        let fit = match (self.cfg.context_window, carried) {
            (Some(window), Some(carried)) => verbatim_fit(carried, window, self.cfg.headroom()),
            _ => VerbatimFit::Fits,
        };
        if let Some(window) = self.cfg.context_window
            && let Some(carried) = carried
            && fit == VerbatimFit::Over
        {
            let note = if carried == resident {
                String::new()
            } else {
                format!(
                    " (that is {resident} token(s) in this box's own ledger; the provider \
                     counts the same conversation as {carried}, and its number is the one \
                     that decides)"
                )
            };
            return Err(HarnessError::Setup(format!(
                "carrying this conversation verbatim would put {carried} token(s) in front \
                 of a {window}-token window{note}, which is more than the window holds. \
                 Re-seating without summarising only works while the conversation still \
                 fits; `/reseat summarise` summarises and fits, and `/compact` does the \
                 same without changing the prompt. Nothing was changed."
            )));
        }
        // The fit is checked: now the prompt is recorded.
        let (next, next_id) = self.seat_target(next)?;
        // Unmeasured ratio: carry it, and say that the fit was not checked rather
        // than pretend either way. The first metered turn measures the scale, so
        // this is a window of one turn per daemon and not a standing hole.
        if carried.is_none() {
            self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                code: "reseat_unchecked".into(),
                detail: format!(
                    "this session has not taken a metered turn yet, so how the provider \
                     counts it is unmeasured and the carry could not be checked against \
                     the window. Going ahead: {resident} token(s) by this box's ledger, \
                     which over-counts what a provider is sent. If the next turn does not \
                     fit, `/compact` is the way back."
                ),

                compaction: None,
            });
        }

        let before = tool_names(&self.prefix.tools_json);
        let after = tool_names(&next.tools_json);
        let grew = grown_schemas(&self.prefix.tools_json, &next.tools_json);
        let grew_how = grown_with_how(&self.prefix.tools_json, &next.tools_json);
        let items: Vec<TranscriptItem> = self.session.items.clone();

        // No summary turn at all - that is the whole point - so the outcome is
        // the empty one, and `fork_to_summary` reads an empty summary as "this
        // was not a compaction" and writes the note that says so.
        let outcome = CompactionOutcome {
            turn_id: format!("{}#reingest", self.transcript_id),
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
        let out = (|| -> Result<ReseatReport, HarnessError> {
            // The whole conversation, carried as it is, which is what a re-ingest is: an empty `because` says no
            // decision about a tail was taken. It goes through `ForkTail` so the fork has one shape for both callers.
            let fork = self.fork_to_summary(
                &outcome,
                Some(&next),
                Some(&next_id),
                ForkTail {
                    items: &items,
                    split: None,
                    because: "",
                },
            )?;
            Ok(ReseatReport {
                fork,
                summary_turn: outcome,
                gained: after.difference(&before).cloned().collect(),
                lost: before.difference(&after).cloned().collect(),
                grew: grew.clone(),
                grew_how: grew_how.clone(),
            })
        })();
        self.compacting = false;
        let mut out = out?;
        // And the harness now SPEAKS the prompt the fork landed on, as `reseat` does at the same point. Without
        // this a metered turn was still sent the OLD tool schemas after a `/reseat` that reported them rebuilt, and
        // `reseat_target` then re-forked a session already on the seated prompt.
        self.prefix = next;
        self.prefix_id = next_id;
        out.gained.sort();
        out.lost.sort();
        self.publish_settings();
        // Under the window but short of a turn's room: the conversation was already this size on the old prompt,
        // and the room a turn needs is the next compaction's to make. So this goes ahead and says so, and it is
        // said once the fork has landed, since a fork that failed re-seated nothing.
        if fit == VerbatimFit::Tight
            && let (Some(window), Some(carried)) = (self.cfg.context_window, carried)
        {
            self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                code: "reseat_tight".into(),
                detail: format!(
                    "re-seated verbatim at {carried} of a {window}-token window, less than the \
                     {} a turn needs to the side — as it was before the reseat. That room is \
                     made by compaction, as it would have been without the reseat; \
                     `/compact` makes it now.",
                    self.cfg.headroom()
                ),
                compaction: None,
            });
        }
        Ok(out)
    }

    /// Re-seat and summarise, replacing the conversation with the summary. This is `/reseat summarise`: the lossy
    /// kind, asked for by name, while a bare `/reseat` runs `reingest`. Reach for it when the point is to shrink as
    /// well as to change message zero.
    pub fn reseat(&mut self) -> Result<ReseatReport, HarnessError> {
        if self.store.is_none() {
            return Err(HarnessError::Setup(
                "re-seating forks the conversation onto a new prompt, and a fork needs a \
                 store to write it to; this session has none."
                    .into(),
            ));
        }
        // The same two values a compaction computes, computed the same way. A
        // `None` here is the conversation already speaking the seated prompt.
        let Some((next, next_id)) = self.reseat_target()? else {
            return Err(HarnessError::Setup(
                "this conversation's prompt already carries exactly the tools that are \
                 seated, so there is nothing to re-seat. Nothing was changed, and nothing \
                 needed to be: every compaction now forks onto the seated prompt, so a \
                 conversation that has compacted since the daemon started is already on it."
                    .into(),
            ));
        };

        let before = tool_names(&self.prefix.tools_json);
        let after = tool_names(&next.tools_json);
        let grew = grown_schemas(&self.prefix.tools_json, &next.tools_json);
        let grew_how = grown_with_how(&self.prefix.tools_json, &next.tools_json);

        self.compacting = true;
        let out = (|| -> Result<ReseatReport, HarnessError> {
            let mut sink = CapturingSink::new(self.hub.clone());
            let answerer = Self::answerer(&self.provider, &self.prefix);
            let mut ladder = letibot_http::retry::Ladder::new(letibot_http::retry::Policy::new(
                self.cfg.http_retries,
            ));
            // The same ask a compaction gets, because this is the same loss: `/reseat summarise`
            // replaces the conversation with a record, and a record sized by the model's guess is the
            // failure R22 is about. No tail is carried, so nothing is measured as already carried.
            let ask = self
                .cfg
                .planning_window()
                .map(|window| letibot_turn::SummaryAsk {
                    budget: letibot_turn::summary_budget(
                        window,
                        self.session.ledger.prefix_len() as u64,
                        self.cfg.headroom(),
                        self.session.ledger.len() as u64,
                    ),
                    carried: "",
                });
            let outcome = Self::run_summary_turn(
                &self.hub,
                &mut self.engine,
                &mut self.session,
                &mut sink,
                &answerer,
                &mut ladder,
                ask.as_ref(),
            )?;
            if outcome.tool_calls > 0 {
                return Err(HarnessError::Setup(format!(
                    "the summary turn proposed {} tool call(s); a summary is a record, not an action, so nothing was re-seated.",
                    outcome.tool_calls
                )));
            }
            let fork =
                self.fork_to_summary(&outcome, Some(&next), Some(&next_id), ForkTail::NONE)?;
            // Only after the fork has landed: until then this harness is still
            // speaking the old prompt, and a `self.prefix` that ran ahead of the
            // transcript would make every later turn build the wrong bytes.
            self.prefix = next;
            self.prefix_id = next_id;
            Ok(ReseatReport {
                fork,
                summary_turn: outcome,
                gained: after.difference(&before).cloned().collect(),
                lost: before.difference(&after).cloned().collect(),
                grew: grew.clone(),
                grew_how: grew_how.clone(),
            })
        })();
        self.compacting = false;
        out
    }

    /// Compaction is one summary turn appended to the conversation itself, unless the conversation arrived with
    /// no room to hold one. OVERRUN is arriving over the budget: then the history is summarised somewhere else,
    /// at 200-375 tok/s, which is ten minutes of silence on a local server, and a clean fold a cloud can afford.
    fn compact_by_folding(
        &mut self,
        items: &[TranscriptItem],
        split: usize,
        sink: &mut CapturingSink,
    ) -> Result<(Harvest, Vec<TranscriptItem>, Option<TailSplit>), HarnessError> {
        let prefix = self.prefix.clone();
        let scratch = format!("{}#overrun", self.transcript_id);
        let answerer = Self::answerer(&self.provider, &self.prefix);
        let harvest = summarise_first_half(
            &mut self.engine,
            &prefix,
            &scratch,
            items,
            split,
            sink,
            &answerer,
        )
        .map_err(HarnessError::Turn)?;
        Ok((
            harvest,
            items[split..].to_vec(),
            tail_split_of(items, split),
        ))
    }

    fn compact_inner(&mut self, fold: crate::wall::Fold) -> Result<CompactReport, HarnessError> {
        let mut sink = CapturingSink::new(self.hub.clone());

        // A compaction re-seats: it is already forking onto a fresh transcript and paying a new prefix's cold
        // prefill, so carrying the tool list this daemon seats NOW is free - and not carrying it made `/compact`
        // then `/reseat` two summaries instead of one. `None` when the seated prompt is the one being spoken.
        let reseat = self.reseat_target()?;

        let per_item: Vec<u64> = (0..self.session.ledger.rows().len())
            .map(|i| {
                self.session
                    .ledger
                    .item_tokens(i)
                    .map(|t| t.len() as u64)
                    .unwrap_or(0)
            })
            .collect();
        let prefix_tokens = self.session.ledger.prefix_len() as u64;
        // Ledger units: `per_item` and `prefix_tokens` below are the ledger's.
        let window = self.cfg.planning_window().unwrap_or(u64::MAX);

        // The fold the controller named. The ordinary one is the fold the size calls for; the sliced fold is the
        // two-half plan asked for by NAME, since a history that fits one summary call never reaches it by size.
        use crate::wall::Fold;
        let smaller = fold == Fold::SmallerTail;
        let plan = if fold == Fold::Sliced {
            plan_cut(&per_item, prefix_tokens, window)
        } else {
            plan_overrun(&per_item, prefix_tokens, window)
        };
        match plan {
            // The ordinary path, and the one that runs almost always.
            OverrunPlan::NotOverrun => {
                // The tail is planned from the history as it stands BEFORE the summary turn, and that is not a
                // detail: the turn appends its instruction and its answer to this session, and a tail that could reach
                // those would put the question and its answer into the record that replaces them.
                let items: Vec<TranscriptItem> = self.session.items.clone();
                // The first choice when a remote backend can afford it: the fold is the cleanest of the three
                // strategies, and what it costs is the cache. Asked with the door's own reserve and not the window's, so
                // a resumed session does not have the whole history summarised down to a 15,000-token tail.
                let folded = self
                    .provider
                    .is_some()
                    .then(|| plan_fold(&per_item, prefix_tokens, window, self.cfg.headroom()))
                    .flatten()
                    .map(|split| {
                        if smaller {
                            smaller_tail(&per_item, prefix_tokens, window, split)
                        } else {
                            split
                        }
                    });
                if let Some(split) = folded {
                    self.hub.publish(SessionEvent::Warning {
                        code: "compacting".into(),
                        detail: format!(
                            "at the wall: summarising the first {split} item(s) and continuing \
                             on the summary plus the rest, verbatim"
                        ),
                        compaction: None,
                    });
                    let (harvest, tail, tail_split) =
                        self.compact_by_folding(&items, split, &mut sink)?;
                    // The fold's summary ran in a scratch transcript, so its outcome is built here rather than coming
                    // back from `run_summary_turn`: none of this session's own prefix was reused and no tokens were served
                    // from its cache. The overrun arm builds the same record.
                    let outcome = CompactionOutcome {
                        turn_id: format!("{}#overrun", self.transcript_id),
                        summary: harvest.summary,
                        tool_calls: harvest.tool_calls,
                        truncated: harvest.truncated,
                        cached_tokens: 0,
                        reusable: 0,
                        generated_tokens: 0,
                        budget: None,
                        fidelity: None,
                    };
                    let because = if tail.is_empty() { "" } else { "fold" };
                    let fork = self.fork_to_summary(
                        &outcome,
                        reseat.as_ref().map(|(p, _)| p),
                        reseat.as_ref().map(|(_, id)| id.as_str()),
                        ForkTail {
                            items: &tail,
                            split: tail_split,
                            because,
                        },
                    )?;
                    let (gained, lost) = self.adopt_reseat(reseat);
                    return Ok(CompactReport {
                        fork,
                        summary_turn: outcome,
                        gained,
                        lost,
                        // The scratch summary went to a `NullSink`; nobody saw it.
                        summary_was_streamed: false,
                    });
                }
                let answerer = Self::answerer(&self.provider, &self.prefix);
                let mut ladder = letibot_http::retry::Ladder::new(
                    letibot_http::retry::Policy::new(self.cfg.http_retries),
                );
                // Summary plus verbatim recent turns, on the remote path only: a local model gets the template and
                // nothing else, because a tail here would be resident tokens competing with the KV pressure that called
                // this compaction.
                let remote = self.provider.is_some();
                // The smaller-tail fold halves the budget; the rest of the plan is the same.
                let tail_plan = if smaller && remote {
                    plan_tail(&items, &per_item, tail_budget(window) / 2)
                } else {
                    plan_compaction_tail(&items, &per_item, window, remote)
                };
                let tail: Vec<TranscriptItem> = match tail_plan.from() {
                    Some(from) => items[from..].to_vec(),
                    None => Vec::new(),
                };
                // **What this record is asked for** (R22): a size, and the facts the tail already carries, so a
                // record is not sent back for something the new base holds verbatim. `resident` is the same
                // `prefix + items` the overrun arithmetic compared against the window a few lines up.
                let budget = letibot_turn::summary_budget(
                    window,
                    prefix_tokens,
                    self.cfg.headroom(),
                    prefix_tokens + per_item.iter().sum::<u64>(),
                );
                let carried = letibot_turn::carried_text(&tail);
                let ask = letibot_turn::SummaryAsk {
                    budget,
                    carried: &carried,
                };
                let ran = Self::run_summary_turn(
                    &self.hub,
                    &mut self.engine,
                    &mut self.session,
                    &mut sink,
                    &answerer,
                    &mut ladder,
                    Some(&ask),
                );
                // The summary turn appends through the engine, not through `reconcile`, so nothing
                // walked the rows it left: a refused attempt keeps its instruction, and a census
                // that never walked it under-reports what a session that cannot compact holds.
                self.restate_footprint();
                let outcome = ran?;
                if outcome.tool_calls > 0 {
                    return Err(HarnessError::Setup(format!(
                        "the summary turn proposed {} tool call(s); a summary is a record, \
                         not an action, so nothing was compacted. The transcript is \
                         unchanged apart from the instruction and the turn themselves — \
                         try again.",
                        outcome.tool_calls
                    )));
                }
                // The fork's tail: the same plan the record was measured against, so what the fork carries and
                // what the fidelity gate counted as carried cannot differ.
                let fork = self.fork_to_summary(
                    &outcome,
                    reseat.as_ref().map(|(p, _)| p),
                    reseat.as_ref().map(|(_, id)| id.as_str()),
                    ForkTail {
                        split: tail_plan.split(),
                        because: tail_because(&tail_plan, remote, !items.is_empty()),
                        items: &tail,
                    },
                )?;
                let (gained, lost) = self.adopt_reseat(reseat);
                Ok(CompactReport {
                    fork,
                    summary_turn: outcome,
                    gained,
                    lost,
                    // This arm ran `run_compaction` through the session's own
                    // sink, so every head watched the summary being written.
                    summary_was_streamed: true,
                })
            }

            OverrunPlan::Hopeless {
                prefix_tokens,
                window,
            } => Err(HarnessError::Setup(format!(
                "this session's PROMPT is {prefix_tokens} token(s) of a {window} token \
                 window, so no summary of the conversation can make room however short it \
                 is. Message zero is the system prompt and the tool schemas, and compaction \
                 never rewrites it. What helps is a larger --context-window if the server \
                 has one, a shorter --system, or fewer seated tools. Nothing was compacted."
            ))),

            plan @ OverrunPlan::Cut { .. } => {
                let items: Vec<TranscriptItem> = self.session.items.clone();
                let prefix = self.prefix.clone();
                let scratch = format!("{}#overrun", self.transcript_id);

                // A cloud provider is the affordable case. `tail_split` is carried out of the arms with the tail: the
                // fold splits halfway BY TOKENS, so it can start mid-exchange without anybody deciding that, and such a
                // tail opens with an answer to a question that is no longer present.
                let (harvest, tail, tail_split) = if self.provider.is_some() {
                    // The reserve is the door's, not the window's: a history that fits only by spending `headroom()` is
                    // one this arm was asked about. The sliced fold skips the verbatim half, which is what the gentler
                    // folds were measured leaving at the wall.
                    let split = (fold != Fold::Sliced)
                        .then(|| plan_fold(&per_item, prefix_tokens, window, self.cfg.headroom()))
                        .flatten()
                        .map(|split| {
                            if smaller {
                                smaller_tail(&per_item, prefix_tokens, window, split)
                            } else {
                                split
                            }
                        });
                    match split {
                        Some(split) => {
                            self.hub.publish(SessionEvent::Warning {
                                code: "compacting".into(),
                                detail: format!(
                                    "over budget: summarising the first {split} item(s) and \
                                     continuing on the summary plus the rest, verbatim"
                                ),

                                compaction: None,
                            });
                            self.compact_by_folding(&items, split, &mut sink)?
                        }
                        // No workable fold: fall through to the two-half plan,
                        // which asks less of the split.
                        None => {
                            let answerer = Self::answerer(&self.provider, &self.prefix);
                            let h = summarise_overrun(
                                &mut self.engine,
                                &prefix,
                                &scratch,
                                &items,
                                &plan,
                                &mut sink,
                                &answerer,
                            )
                            .map_err(HarnessError::Turn)?;
                            (h, Vec::new(), None)
                        }
                    }
                } else {
                    let (cut, tail_from) = match plan {
                        OverrunPlan::Cut { cut, tail_from, .. } => (cut, tail_from),
                        _ => (0, 0),
                    };
                    self.hub.publish(SessionEvent::Warning {
                        code: "compacting".into(),
                        detail: format!(
                            "over budget with no room for a summary in place: summarising \
                             off to one side, in two halves that overlap. First the recent \
                             {} item(s), which the server has not seen and must read cold; \
                             then the older {cut} item(s), whose prompt the server already \
                             holds. This takes minutes and the context count above does not \
                             move until it lands.",
                            items.len() - tail_from
                        ),

                        compaction: None,
                    });
                    let answerer = Self::answerer(&self.provider, &self.prefix);
                    let h = summarise_overrun(
                        &mut self.engine,
                        &prefix,
                        &scratch,
                        &items,
                        &plan,
                        &mut sink,
                        &answerer,
                    )
                    .map_err(HarnessError::Turn)?;
                    (h, Vec::new(), None)
                };

                let outcome = CompactionOutcome {
                    turn_id: format!("{}#overrun", self.transcript_id),
                    summary: harvest.summary,
                    tool_calls: harvest.tool_calls,
                    truncated: harvest.truncated,
                    cached_tokens: 0,
                    reusable: 0,
                    generated_tokens: 0,
                    budget: None,
                    fidelity: None,
                };
                // The overrun fold's own reason, not `plan_tail`'s: this tail was placed by arithmetic on the window,
                // halfway BY TOKENS. Empty when there is no tail at all, since a local overrun summarises both halves.
                let because = if tail.is_empty() { "" } else { "fold" };
                let fork = self.fork_to_summary(
                    &outcome,
                    reseat.as_ref().map(|(p, _)| p),
                    reseat.as_ref().map(|(_, id)| id.as_str()),
                    ForkTail {
                        items: &tail,
                        split: tail_split,
                        because,
                    },
                )?;
                let (gained, lost) = self.adopt_reseat(reseat);
                Ok(CompactReport {
                    fork,
                    summary_turn: outcome,
                    gained,
                    lost,
                    // The scratch summary went to a `NullSink`; nobody saw it.
                    summary_was_streamed: false,
                })
            }
        }
    }

    /// Take on the prompt the fork just opened under, and only after the fork has landed: until then this
    /// harness still speaks the old prompt, and a `self.prefix` ahead of the transcript would make every later turn
    /// build the wrong bytes. Returns what the announced tool list gained and lost.
    pub fn adopt_reseat(
        &mut self,
        reseat: Option<(StablePrefix, String)>,
    ) -> (Vec<String>, Vec<String>) {
        let Some((prefix, id)) = reseat else {
            return (Vec::new(), Vec::new());
        };
        let before = tool_names(&self.prefix.tools_json);
        let after = tool_names(&prefix.tools_json);
        self.prefix = prefix;
        self.prefix_id = id;
        (
            after.difference(&before).cloned().collect(),
            before.difference(&after).cloned().collect(),
        )
    }

    /// The fork: replace the resident history with one summary item, in a new transcript the store links back to
    /// this one. Split from `compact` so it runs without a model server, and the old transcript is persisted first
    /// and kept whole, since compaction never deletes anything, it stops carrying the history.
    pub fn fork_to_summary(
        &mut self,
        outcome: &CompactionOutcome,
        onto: Option<&StablePrefix>,
        onto_id: Option<&str>,
        tail: ForkTail<'_>,
    ) -> Result<ForkReport, HarnessError> {
        self.fork_onto(outcome, onto, onto_id, tail, None)
    }

    /// The fork, with the new transcript's first item handed in, or `None` for the note a compaction writes about
    /// itself. One body for every fork, because everything around the first item is the same act: the old transcript
    /// kept, the store row linking them, the fingerprint, the swap. `/clear` is the one caller with its own item.
    pub(crate) fn fork_onto(
        &mut self,
        outcome: &CompactionOutcome,
        onto: Option<&StablePrefix>,
        onto_id: Option<&str>,
        tail: ForkTail<'_>,
        lead: Option<TranscriptItem>,
    ) -> Result<ForkReport, HarnessError> {
        let tail_items: &[TranscriptItem] = tail.items;
        let tail_split = tail.split;
        let old_id = self.transcript_id.clone();
        let was_tokens = self.session.ledger.len();
        // **Whether this fork carries the conversation verbatim** - a re-ingest (`/reseat`):
        // no summary, no first item of its own, every row of the history in the tail. Then the
        // provider's last measurement still describes the conversation, and is kept (below).
        let verbatim = lead.is_none()
            && outcome.summary.is_empty()
            && tail_split.is_none()
            && tail_items.len() == self.session.items.len();
        let prefix_before = self.session.ledger.prefix_tokens().len() as i64;
        // The prefix the FORK opens under, which is the caller's choice and not always this session's: a
        // compaction keeps the one the conversation speaks under, and a re-seat exists to hand in a different one.
        // Cloned rather than borrowed, since `persist` needs `&mut self`.
        let onto: StablePrefix = onto.cloned().unwrap_or_else(|| self.prefix.clone());
        let onto_id: String = onto_id
            .map(str::to_string)
            .unwrap_or_else(|| self.prefix_id.clone());
        // The old transcript is flushed with the summary turn in it, before the fork writes anything: a fork that
        // dropped the turn that justified it would leave the store unable to say where the summary came from. With
        // no store this is a no-op.
        self.persist()?;
        let Some(store) = self.store.as_ref() else {
            return Err(HarnessError::Setup(
                "compaction needs a store to fork into; this session has none, so there \
                 is nowhere to put the replacement history and nothing to link it to"
                    .into(),
            ));
        };
        let n: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM transcript WHERE session_id = ?1",
                [self.cfg.session_id.as_str()],
                |r| r.get(0),
            )
            .map_err(|e| HarnessError::Store(e.to_string()))?;
        let new_id = format!("{}#t{}", self.cfg.session_id, n);
        // The fork point is the session **log's** seq, which is what §5.5's
        // `forked_at_seq` means: where in the session's event stream the divergence
        // happened, not where in the transcript.
        let forked_at = self.hub.head_seq();
        // Nothing is written to the store yet, since the row used to go in before a row of the new transcript
        // existed and a crash during the carry resumed onto a partial conversation. The row goes in UNSEALED before
        // the swap, and the seal, the fingerprint and the stale flag follow the last row.
        let fingerprint = prefix_fingerprint(self.cfg.seat, &onto);
        let mut next = self
            .engine
            .open(&new_id, &onto)
            .map_err(|e| HarnessError::Setup(format!("opening the compacted transcript: {e}")))?;
        // A record that was cut off says so, in the record: this base is what every later turn reads as the whole
        // of what came before, and it used to claim to be the summary of the full history with a third of it missing.
        // Not a refusal, since a session at the wall has nowhere else to go, and the sentence goes FIRST.
        let cut = if outcome.truncated {
            " The summary is incomplete (cut off); treat anything missing as unknown and check \
             with `transcript`."
        } else {
            ""
        };
        // A fork that summarised says so; one that did not must not: an empty summary is `reingest`, and telling
        // the model everything before is replaced by a summary it is not shown contradicts what it can see. The
        // sentence also changed when a tail arrived, for the same reason: `CompactionTail::why_line` words it.
        let tail_why_text = (!tail.because.is_empty()).then(|| {
            letibot_sessionlog::event::CompactionTail {
                turns: Vec::new(),
                carried: tail_items.len() as u64,
                because: tail.because.to_string(),
                dropped: 0,
            }
            .why_line()
            .unwrap_or_default()
        });
        let carried = match tail_items.len() {
            // WHY there is no tail, said in the row itself, because an absence with two causes and one appearance
            // cannot be told apart otherwise. Here and not only on the wire, since this is the durable one: the store
            // has no events table, so after a restart the item is all a reader has.
            0 if !tail.because.is_empty() => format!(" {}", tail_why_text.unwrap_or_default()),
            0 => String::new(),
            n => format!(
                " The last {n} item(s) of it follow this note VERBATIM — as they were \
                 written, not as a description of them."
            ),
        };
        let mid_exchange = match tail_split {
            None => String::new(),
            Some(TailSplit { dropped }) => format!(
                " The verbatim part begins in the MIDDLE of an exchange: {dropped} item(s) \
                 of it were dropped from the front, so its first message answers something \
                 that is no longer here. Stated rather than left to be discovered."
            ),
        };
        let note: TranscriptItem = match lead {
            Some(item) => item,
            None => TranscriptItem::System {
                text: if outcome.summary.is_empty() {
                    // One line, no transcript id: the `[reseat]` notice is the one that says what
                    // changed, and a carried conversation keeps every earlier re-seat's note, so
                    // these stacked two and three deep.
                    "System prompt and tools updated; the conversation continues unchanged. \
                     Nothing was summarised."
                        .to_string()
                } else {
                    format!(
                        "This conversation was compacted: what was said before this point is \
                     replaced by the summary below, which was written over the full history \
                     of transcript {old_id} and proposed no tool calls.{carried}{mid_exchange}{cut}\n\n{}",
                        outcome.summary
                    )
                },
                origin: SystemOrigin::Update,
            },
        };
        // The recent past is carried verbatim, not described: compaction used to keep none of it, so a session
        // came back unable to see the turn it was in the middle of. The newest items under their own budget, appended
        // after the summary, mapped to the wire's role and text as appended so the report and the bytes cannot differ.
        let mut body: Vec<TranscriptItem> = Vec::with_capacity(1 + tail_items.len());
        body.push(note);
        body.extend_from_slice(tail_items);
        let tail_turns = wire_turns(tail_items);

        // A carry the head can watch: rendering and writing the new transcript is the long half of a fork, and the
        // worker is inside this command until it ends. Said from the first moment, then once per committed chunk and
        // never per row, and ended by the final total, which clears the line.
        let carrying = body.len() >= FILLING_STRIDE;
        // Two phases, each with its own bar: the render onto the new prompt, which is the long one,
        // and the write of the rendered rows to the store.
        let (rendering_what, filling_what) = if verbatim {
            (
                "re-seating: rendering the conversation onto the new prompt",
                "re-seating: writing the carried conversation",
            )
        } else {
            (
                "compacting: rendering the new transcript",
                "compacting: writing the new transcript",
            )
        };
        if carrying {
            self.filling(rendering_what, "rows", 0, body.len() as u64);
        }
        let mut sink = CapturingSink::new(self.hub.clone());
        let hub = self.hub.clone();
        let rendered =
            next.append_items_reporting(&self.engine, &body, &mut sink, &mut |done, of| {
                // Every `FILLING_STRIDE` rows and the last, as every other fill ticks.
                if carrying && (done % FILLING_STRIDE as u64 == 0 || done == of) {
                    hub.publish(letibot_sessionlog::SessionEvent::Filling {
                        what: rendering_what.to_string(),
                        unit: "rows".to_string(),
                        done,
                        total: of,
                    });
                }
            });
        if carrying {
            // The write's own bar starts from nothing, so a reader sees the second phase begin.
            self.filling(filling_what, "rows", 0, body.len() as u64);
        }
        if let Err(e) = rendered {
            if carrying {
                self.filling(filling_what, "rows", body.len() as u64, body.len() as u64);
            }
            return Err(e.into());
        }
        // The transcript row, unsealed (see above), with a busy store's patience (R91.6).
        let registered = {
            let (session_id, new_id, onto_id, old_id) =
                (&self.cfg.session_id, &new_id, &onto_id, &old_id);
            self.store_patiently("the fork's transcript row", |store| {
                store.put_fork_unsealed(new_id, session_id, onto_id, old_id, forked_at as u32)
            })
        };
        if let Err(e) = registered {
            if carrying {
                self.filling(filling_what, "rows", body.len() as u64, body.len() as u64);
            }
            return Err(e);
        }
        // The swap is the point of no return, after every store write that could fail: a harness that swapped and
        // then could not persist would be a session whose transcript row exists and whose body does not.
        self.session = next;
        self.transcript_id = new_id.clone();
        self.persisted = 0;
        self.unsealed_fork = Some(UnsealedFork {
            transcript_id: new_id.clone(),
            fingerprint,
        });
        self.reconcile(&mut sink, &body);
        // AND EVERY ROW JUST PUBLISHED REPLACED THE ONES EVERY READER HOLDS: `append_items` publishes under
        // `new_id`, so a reader folding what it is told draws both conversations. The event names the transcript it
        // replaces, not a count, and goes AFTER the rows, so a fork that failed has not already emptied every reader.
        self.hub
            .publish(letibot_sessionlog::SessionEvent::TranscriptForked {
                transcript_id: new_id.clone(),
                parent_id: old_id.clone(),
            });
        // The write, ticking per committed chunk - see "a carry the head can watch" above.
        let what = filling_what;
        let hub = self.hub.clone();
        let total = body.len() as u64;
        let persisted = self.persist_reporting(&mut |done, of| {
            if carrying {
                hub.publish(letibot_sessionlog::SessionEvent::Filling {
                    what: what.to_string(),
                    unit: "rows".to_string(),
                    done,
                    total: of,
                });
            }
        });
        if carrying && persisted.is_err() {
            // The line must not be left walking over a write that stopped.
            self.filling(what, "rows", total, total);
        }
        // Past the swap a failed write is not a failed fork: the session speaks the new transcript in this daemon,
        // so returning an error sent every caller down its refusal path - `reingest` returned before the prefix moved,
        // and the operator was told nothing changed. The rows stay in memory and the next store write takes them up.
        if let Err(e) = persisted {
            self.hub.publish(SessionEvent::Warning {
                code: "fork_unstored".into(),
                detail: format!(
                    "the conversation moved to {new_id} in this daemon, but its rows are not all \
                     stored yet ({e}). They are written with the next store write; until then a \
                     restart resumes {old_id}, as it was before the fork."
                ),
                compaction: None,
            });
        }
        // The stored context size is about the OLD conversation: the last prompt the provider measured, which this
        // fork replaced. Only a turn can put a real number here, so it is cleared rather than estimated. A verbatim
        // fork is the exception, since it replaced nothing: its count is kept and its ledger anchor moved.
        if let Some(store) = &self.store {
            let kept = if verbatim {
                store
                    .session(&self.cfg.session_id)
                    .ok()
                    .flatten()
                    .and_then(|row| {
                        let delta =
                            self.session.ledger.prefix_tokens().len() as i64 - prefix_before;
                        let ledger = (row.context_ledger? as i64 + delta).max(1) as u64;
                        Some((row.context_tokens?, row.context_cached, ledger))
                    })
            } else {
                None
            };
            let _ = match kept {
                Some((tokens, cached, ledger)) => {
                    store.set_context(&self.cfg.session_id, Some(tokens), cached, Some(ledger))
                }
                None => store.set_context(&self.cfg.session_id, None, None, None),
            };
        }
        Ok(ForkReport {
            transcript_id: new_id,
            parent_id: old_id,
            forked_at,
            was_tokens,
            base_tokens: self.session.ledger.len(),
            truncated: outcome.truncated,
            tail_items: tail_items.len(),
            // The reason, in the wire's own shape - and `None` when there is no reason to
            // give, which is a re-seat and a re-ingest (`ForkTail::NONE` passes `""`).
            tail_why: (!tail.because.is_empty()).then(|| {
                letibot_sessionlog::event::CompactionTail {
                    turns: tail_turns.clone(),
                    carried: tail_items.len() as u64,
                    because: tail.because.to_string(),
                    dropped: tail_split.map(|s| s.dropped as u64).unwrap_or(0),
                }
            }),
            tail_dropped: tail_split.map(|s| s.dropped),
            tail_turns,
            tail_because: tail.because.to_string(),
        })
    }

    /// Every row of the fork is stored, so make it the session's conversation: the seal, then the fingerprint and
    /// the stale flag that follow the transcript. A seal that fails stays owed and is retried after the next store
    /// write; the parent is still what a resume opens, which is consistent, only behind.
    pub(crate) fn seal_fork(&mut self) {
        let Some(u) = self.unsealed_fork.take() else {
            return;
        };
        if u.transcript_id != self.transcript_id {
            // Superseded by a later fork before its rows were all written: a dead branch.
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        if let Err(e) = store.seal_transcript(&u.transcript_id) {
            eprintln!("  store: sealing {}: {e}", u.transcript_id);
            self.unsealed_fork = Some(u);
            return;
        }
        if let Err(e) = store.set_prefix_fingerprint(&self.cfg.session_id, &u.fingerprint) {
            eprintln!("  store: the fork's prefix fingerprint: {e}");
        }
        self.session_registry
            .set_stale_prefix(&self.cfg.session_id, None);
        // And the model may be told about a LATER staleness, once, as it was about this one.
        self.stale_told = None;
    }

    /// A resume restores the wall controller's `Stuck` from the row it wrote: without this the row is only read on
    /// the lazy path a head's switch reaches, and the one resume everybody types would come back re-armed. And it is
    /// no longer for good, since the first round that measures comfortably below the wall releases it.
    pub fn restore_auto_compact(&mut self) -> Option<String> {
        let (before, measured) = self
            .store
            .as_ref()
            .and_then(|s| s.session(&self.cfg.session_id).ok())
            .flatten()?
            .auto_compact_stood_down?;
        // Idempotent: a second caller (a head switching IN to a live session)
        // finds the controller already stuck and says nothing.
        if self.cfg.wall.is_stuck() {
            return None;
        }
        // A pair that cannot be the provider's count is not a finding: a provider counts nothing past its own
        // window, so a stored measured value over the window is the old guard's pair in LEDGER tokens. Such a row is
        // cleared and not restored, and the next measured round decides, which is the controller's rule anyway.
        if let Some(window) = self.cfg.context_window
            && measured > window
        {
            if let Some(store) = &self.store {
                let _ = store.set_auto_compact_stood_down(&self.cfg.session_id, None);
            }
            return None;
        }
        self.cfg.wall.restore_stuck(before, measured);
        self.publish_settings();
        Some(format!(
            "restored with compaction before a turn OFF: the wall controller was stuck \
             (a fold of {before} measured {measured}, still at the wall). A turn the \
             provider refuses is still folded, and a measured count comfortably below the \
             wall re-arms it"
        ))
    }
}

/// **The stored provider count and the ledger length it was measured at, or `None`
/// when there is nothing to anchor to.**
///
/// The decision is a free function for the reason `sweep_abandoned_calls` gives
/// about its own: it can be tested without a daemon, a model or a socket. That is
/// not tidiness here - a provider cannot be pointed at a canned server (a
/// `ProviderConfig` names a vendor and nothing else), so a gate left inline in
/// `Harness::open` could only ever be exercised by a test that talks to
/// DeepSeek. The bug this replaces was found in production precisely because
/// nothing checked the pairing.
///
/// **What it returns is an ANCHOR, not a ratio** - `(ledger, provider)`: the
/// provider counted `provider` tokens for the prompt whose ledger was `ledger`
/// long. [`roll_resident`] adds what was appended after `ledger` to it, sized the
/// way the provider will be sent it, and that sum is the resident count.
///
/// # Why equality was the test, and why it is not any more
///
/// This used to give the pair back only when the stored ledger EQUALLED the
/// rebuilt one, because the pair was used as a RATIO: `ledger_now ÷ ratio`. A
/// count from a smaller conversation divided into a bigger ledger overstates the
/// ratio, and the session compacts too late - MEASURED 2026-10-02: a store holding
/// 940,211, measured on a ~950k-token conversation, was paired with a rebuilt
/// ledger of 1,486,369; the window came out 1,580,888 for a session already 1.46M
/// deep, and it died at the provider's 400 on every attempt.
///
/// The equality closed that, and opened the opposite hole. Anything appended after
/// the last round's `persist_context` - a notice, a nag, the tool result of a turn
/// that did not end in a round - broke it, the pair was refused, the window went
/// UNSCALED, and the raw ledger was compared against the provider's window.
/// MEASURED 2026-10-11 (R72), leticl's `s-1791151788256943467` on deepseek-flash,
/// the first idle pass after a restart:
///
/// ```text
/// compacting: 3454266 of 1000000 tokens resident, less than the 62500 the next turn needs
/// compacted: 75408 tokens resident now, was 3454266.
/// ```
///
/// That is a session the provider counted at ~740k, folded to 75k. The comment at the
/// call chose that direction on purpose (*"it compacts early, which is the
/// direction nobody loses a session to"*), and early by 4.6× is losing the
/// session's working context on every restart.
///
/// An anchor has neither failure. The 2026-10-02 conversation comes back as
/// 940,211 PLUS the ~536k ledger tokens appended since, sized as sent - larger, so
/// it compacts; the 2026-10-11 one comes back as ~740k plus one notice - and does
/// not. Nothing is divided, so a stale count cannot shrink a big ledger, and a
/// later append cannot throw the count away.
///
/// It refuses in three cases, and each lands on the unscaled window:
///
///   * no stored pair - a row written before v12, or one whose turn never
///     finished. Unverifiable is not the same as wrong, but the two are the same
///     as unmeasured;
///   * a stored ledger LONGER than the one rebuilt - the conversation it measured
///     is not this one (a fork clears the row, so this is a row nothing else
///     explains, and a count of something else is not evidence);
///   * a zero on either side, which is not a measurement of anything.
pub(crate) fn recovered_scale(
    stored_provider: Option<u64>,
    stored_ledger: Option<u64>,
    ledger_now: usize,
) -> Option<(u64, u64)> {
    let provider_tokens = stored_provider?;
    let stored_ledger = stored_ledger?;
    if provider_tokens == 0 || stored_ledger == 0 || ledger_now == 0 {
        return None;
    }
    if stored_ledger > ledger_now as u64 {
        return None;
    }
    Some((stored_ledger, provider_tokens))
}

/// Whether the round about to be sent stops at the wall: `resident` is the ledger count of the prompt it would
/// send and `window` the planning window in the same units. Round 0 stops only past the window itself and only where
/// a compaction could make room, since a prompt whose message zero alone is past it is a session no summary rescues.
pub(crate) fn wall_stops_round(
    round: usize,
    resident: u64,
    prefix: u64,
    headroom: u64,
    window: u64,
) -> bool {
    if round == 0 {
        resident > window && prefix + headroom < window
    } else {
        resident + headroom >= window
    }
}

/// **Whether the wall stops THIS round** - [`wall_stops_round`]'s arithmetic plus the one
/// question about where the count came from.
///
/// MEASURED 2026-10-25, `s-1791394076177451000`: a session switched from deepseek to
/// `glm-coding` was stopped at round 0 with
///
/// ```text
/// context wall: stopped after 0 round(s) at 2708271 of 1000000 ledger tokens
///               (unscaled: no round on glm-coding has measured yet)
/// ```
///
/// The stop was 2,708,271 LEDGER tokens against a 1,000,000-token window, where the ledger
/// of a metered session is `Vocab::bytes` (one id per byte of the rendered history). It is
/// not a count of tokens at all, and comparing it with a token window says nothing about
/// whether the prompt fits. The provider is the only one that can answer that question, and
/// it answers it with a refusal that the compaction door already handles on evidence
/// (`WallWhy::Refused`). So at round 0 an unmeasured count does not stop the turn.
///
/// **Carried is not unmeasured.** A ratio measured on the model before the switch converts
/// bytes into that model's tokens - the same quantity, off by however differently the two
/// tokenizers count - so the comparison is meaningful and the wall may fire on it, with
/// every line that prints it saying which model measured the ratio. Only *nothing has ever
/// been measured* is the case where the arithmetic means nothing at all.
pub(crate) fn wall_stops_this_round(
    round: usize,
    resident: u64,
    prefix: u64,
    headroom: u64,
    window: u64,
    basis: crate::config::WallBasis<'_>,
) -> bool {
    if round == 0 && basis == crate::config::WallBasis::Unmeasured {
        return false;
    }
    wall_stops_round(round, resident, prefix, headroom, window)
}

/// Where the smaller-tail fold splits: the ordinary fold's split moved later until the verbatim part is at most
/// half what it was, or the ordinary split unchanged when the longer first part would not fit one summary call, where
/// the next rung is the answer.
pub(crate) fn smaller_tail(
    per_item: &[u64],
    prefix_tokens: u64,
    window: u64,
    split: usize,
) -> usize {
    let verbatim: u64 = per_item[split..].iter().sum();
    let mut rest = verbatim;
    let mut at = split;
    while at + 1 < per_item.len() && rest > verbatim / 2 {
        rest -= per_item[at];
        at += 1;
    }
    let first: u64 = per_item[..at].iter().sum();
    if prefix_tokens + first + letibot_turn::WRITE_ROOM <= window {
        at
    } else {
        split
    }
}

/// The closing line of an automatic compaction, in one unit, said once, shared by both doors so they cannot word
/// the same fact two ways. The after-count of a metered session is an estimate until a round on the new base measures
/// it, so the line prints `≈ N` rather than an estimate beside a measurement as if they were the same kind of number.
pub(crate) fn compacted_line(before: u64, after: Resident, unit: &str, metered: bool) -> String {
    if metered && after.measured.is_none() {
        return format!(
            "compacted: {before} → ≈ {} {unit} resident, estimated until the next round \
             measures it (the new base sized as it will be sent).",
            after.tokens
        );
    }
    format!("compacted: {before} → {} {unit} resident.", after.tokens)
}

/// Where the provider last counted this conversation, and its own count of that prompt. The wall used to be judged
/// as a ledger length divided by a ratio, which is a property of how much of the history was reasoning at one moment,
/// so it now counts the provider's number and adds only what came after, sized the way it will be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WallAnchor {
    /// The transcript the count was taken on. A compaction forks a new one, and a
    /// count of the old conversation says nothing about the new one's size.
    pub(crate) transcript: String,
    /// The ledger length of the prompt the count belongs to - a row boundary.
    pub(crate) ledger: u64,
    /// The count. The provider's own when `measured`; otherwise the estimate that
    /// stands in for one after a fork, until a round on the new base measures.
    pub(crate) provider: u64,
    /// Whether `provider` came from the provider.
    pub(crate) measured: bool,
    /// The last measurement as a ratio of what was SENT: the sent-sized ledger tokens and the provider's count of one
    /// prompt, since a metered ledger is one token per byte. Every sent-sized delta goes through this ratio, carried
    /// across a fork, and it is `None` until something measured.
    pub(crate) scale: Option<(u64, u64)>,
}

impl WallAnchor {
    /// `sent` ledger tokens in the provider's units, through [`WallAnchor::scale`].
    fn convert(&self, sent: u64) -> u64 {
        match self.scale {
            Some((ledger, provider)) if ledger > 0 => {
                ((sent as u128 * provider as u128) / ledger as u128) as u64
            }
            _ => sent,
        }
    }
}

/// The sent-sized ledger tokens between two row boundaries, `[from, to)` - see
/// [`sent_since`].
fn sent_between(
    ledger: &letibot_tokencore::ledger::TokenLedger,
    items: &[TranscriptItem],
    from: u64,
    to: u64,
    metered: bool,
) -> u64 {
    sent_since(ledger, items, from, metered).saturating_sub(sent_since(ledger, items, to, metered))
}

/// The measured ratio for a prompt of `ledger_at` ledger tokens counted as `provider`
/// - `None` on a local session, where the ledger is the prompt.
pub(crate) fn sent_scale(
    ledger: &letibot_tokencore::ledger::TokenLedger,
    items: &[TranscriptItem],
    ledger_at: u64,
    provider: u64,
    metered: bool,
) -> Option<(u64, u64)> {
    if !metered || provider == 0 {
        return None;
    }
    let prefix = u64::from(ledger.prefix_len());
    let sent = prefix + sent_between(ledger, items, prefix, ledger_at, metered);
    (sent > 0).then_some((sent, provider))
}

/// **One reading of how big this conversation is, in the units its wall is in**  -
/// the provider's on a metered session, the ledger's on a local one (where the
/// two are the same count by construction: the ledger IS the prompt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resident {
    /// The whole count. What the wall is judged on, and what a line prints.
    pub tokens: u64,
    /// The provider's own count at its last round, when `tokens` is built on one  -
    /// the number the head shows. `None` when nothing measured this transcript.
    pub measured: Option<u64>,
    /// What was appended after that count and will be sent, sized as sent.
    /// `tokens == measured + since` whenever `measured` is `Some`.
    pub since: u64,
}

impl Resident {
    /// The count's provenance, said once, in the line that prints the count, so the head's number and the wall's are
    /// visibly the same quantity. Empty when there is no measurement to relate to.
    pub fn basis(&self) -> String {
        match self.measured {
            Some(m) if self.since > 0 => {
                format!(" ({m} at the last round + {} appended since)", self.since)
            }
            Some(_) => " (the provider's count at the last round)".into(),
            None => String::new(),
        }
    }
}

/// What the ledger holds after `from` that the next request will carry, in ledger tokens: the provider-sized
/// estimate of an append, so it drops reasoning and the segment mark on a metered session while on a local one the
/// request IS the ledger. A row that starts before `from` is not counted.
fn sent_since(
    ledger: &letibot_tokencore::ledger::TokenLedger,
    items: &[TranscriptItem],
    from: u64,
    metered: bool,
) -> u64 {
    let len = ledger.len() as u64;
    if !metered {
        return len.saturating_sub(from);
    }
    ledger
        .rows()
        .iter()
        .zip(items)
        .filter(|(row, _)| u64::from(row.tok_offset) >= from)
        .filter(|(_, item)| {
            !matches!(
                item,
                TranscriptItem::Reasoning { .. } | TranscriptItem::SegmentMark { .. }
            )
        })
        .map(|(row, _)| u64::from(row.tok_len))
        .sum()
}

/// **A round's reasoning, as one text** - every `Reasoning` item the round produced, in order, for
/// the context check (R71). The engine streams reasoning as several items; one thought is judged
/// whole.
pub(crate) fn reasoning_text(items: &[TranscriptItem]) -> String {
    items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Reasoning { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The resident count, rolled forward from `anchor` to the ledger as it stands. If the anchor is on this transcript
/// it is the provider's count plus what was sent since; if not, a fork replaced it, so the estimate starts over from
/// the new base and the anchor is re-based there, rather than dividing it by a ratio from another conversation.
pub(crate) fn roll_resident(
    anchor: &WallAnchor,
    transcript_now: &str,
    ledger: &letibot_tokencore::ledger::TokenLedger,
    items: &[TranscriptItem],
    metered: bool,
) -> (WallAnchor, Resident) {
    let len = ledger.len() as u64;
    if anchor.transcript == transcript_now && anchor.ledger <= len {
        let since = anchor.convert(sent_since(ledger, items, anchor.ledger, metered));
        return (
            anchor.clone(),
            Resident {
                tokens: anchor.provider + since,
                measured: anchor.measured.then_some(anchor.provider),
                since: if anchor.measured { since } else { 0 },
            },
        );
    }
    let prefix = u64::from(ledger.prefix_len());
    let tokens = anchor.convert(prefix + sent_since(ledger, items, prefix, metered));
    (
        WallAnchor {
            transcript: transcript_now.to_string(),
            ledger: len,
            provider: tokens,
            measured: false,
            scale: anchor.scale,
        },
        Resident {
            tokens,
            measured: None,
            since: 0,
        },
    )
}

/// `0` is unbounded, which is the default. `usize::MAX` rather than a second loop shape, so there is one body and
/// one place a round is counted, since two loops to keep in step is how the reporting drifts. A named function because
/// this is the whole of infinity.
pub(crate) fn round_backstop(configured: usize) -> usize {
    match configured {
        0 => usize::MAX,
        n => n,
    }
}
