//! **The context wall**: compaction, reseat and forking — the ways a long
//! conversation is carried onto a smaller prompt, and the arithmetic that decides
//! when each is needed.
use super::*;

/// A fork registered in the store but not yet the session's conversation. See
/// `Store::put_fork_unsealed`.
pub(crate) struct UnsealedFork {
    transcript_id: String,
    fingerprint: String,
}

/// **What a fork carries verbatim, and why** — R27's ruled tail, as the fork needs it.
///
/// Bundled rather than passed as three arguments because the three are one fact: a tail
/// has contents, it may have started mid-exchange, and it has a reason for being what it
/// is. Splitting them is how one of the three comes to be updated without the others.
pub struct ForkTail<'a> {
    pub items: &'a [TranscriptItem],
    /// Non-zero when the tail could not start at an exchange boundary.
    pub split: Option<TailSplit>,
    /// One of [`letibot_turn::TAIL_BECAUSE`], on the wire as `tail.because`. Empty for a
    /// fork that carries no tail *and* is not a compaction — a re-seat, a re-ingest —
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

/// What a compaction fork actually did — numbers, not the word "compacted".
///
/// The same rule as [`ResumeReport`]: the claim is cheap and the evidence is what
/// makes it checkable. `was_tokens` against `base_tokens` is the reduction an
/// operator can see, and `parent_id` with `forked_at` is where the old
/// conversation went.
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
    /// **The summary ran out of room before it finished.** Carried so the
    /// operator is told on the same line that says compaction worked -- it did
    /// work, and what it produced is partial, and those are two facts rather
    /// than one.
    pub truncated: bool,
    /// How many of the newest items were carried into the new base verbatim
    /// rather than summarised. Zero is the old behaviour and is still what a
    /// re-seat does, where the point is to change the prefix and not to shorten.
    ///
    /// **Non-zero only for a remote model** (R27, `head-parity-2026-09-21.md`): a
    /// local model is bounded by the KV cache in VRAM, where a tail competes with
    /// the pressure the compaction was called to relieve, and a remote model is
    /// bounded by a bill, where the newest exchanges verbatim are affordable and
    /// buy back exactly what a summary is worst at.
    pub tail_items: usize,
    /// **Items of the newest exchange left out of the verbatim tail**, when the
    /// tail could not start at an exchange boundary — one big file read is larger
    /// than the whole tail budget, so the tail begins inside the exchange that is
    /// still in progress. `None` is the ordinary case: the tail starts where an
    /// exchange does, or there is no tail.
    ///
    /// Disclosed rather than left to be discovered, because a tail that begins
    /// mid-exchange opens with an answer to something that is no longer present,
    /// and the model reading the new base is the reader who has to know that.
    pub tail_dropped: Option<usize>,
    /// **The tail as the wire carries it** — role and text, in order, so a head can draw
    /// the recent past instead of parsing the note's count.
    ///
    /// Role is `operator` or `agent`, the vocabulary `Speaker` already uses. Empty for a
    /// fork with no tail, which is every local compaction and every re-seat.
    pub tail_turns: Vec<letibot_sessionlog::event::CompactionTurn>,
    /// Why the tail is what it is — [`ForkTail::because`], carried through so the report
    /// can put it on the wire without recomputing a decision that has already been made.
    pub tail_because: String,
    /// **The tail as the WIRE describes it**, carrying the reason in the shape both readers
    /// already share.
    ///
    /// `tail_because` above is the same fact as a bare string, and this is the object the
    /// warning carries — `carried` plus `because` plus `dropped` — so the sentence a head
    /// draws and the row this report becomes cannot say different things about one
    /// compaction. [`letibot_sessionlog::event::CompactionTail::why_line`] is the one place
    /// the reason becomes words.
    ///
    /// `None` for a fork that is not a compaction — a re-seat, a re-ingest — where a reason
    /// would be a claim about a decision nobody made.
    pub tail_why: Option<letibot_sessionlog::event::CompactionTail>,
}

/// What a compaction left behind: the fork's numbers and the summary turn's.
///
/// Two reports because two different things happened and a caller discloses both:
/// [`ForkReport`] is what the session's base now is, and `summary_turn` is how the
/// summary was paid for — `cached_tokens` against `reusable` is the measured cache
/// hit the whole cached strategy exists for, and the gap between them is the
/// server's, never ours (the prompts are proven identical over the span).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactReport {
    pub fork: ForkReport,
    pub summary_turn: CompactionOutcome,
    /// **Tools the fork's prompt announces that the old one did not.** A
    /// compaction forks onto the prompt this daemon seats now, so a conversation
    /// started before a tool existed picks it up by compacting — and the operator
    /// is told, because a capability that appears silently is one nobody uses.
    /// Empty on the ordinary compaction, where the two prompts are the same.
    pub gained: Vec<String>,
    /// Tools the old prompt announced and the new one does not.
    pub lost: Vec<String>,
    /// **Did the operator watch the summary being written?**
    ///
    /// The ordinary compaction runs its summary turn through the session's own
    /// sink, so the text streams to every head as it is generated and the fork
    /// that follows needs no announcement. The overrun paths cannot: they
    /// summarise a SCRATCH transcript, and `summarise_one` sends that to a
    /// `NullSink` on purpose — forwarding its progress once made the head draw
    /// the scratch prompt's token count as the session's context. Its comment
    /// ends "progress is the caller's to report, in words, per half", and the
    /// caller reported the start and nothing else.
    ///
    /// Measured 2026-09-20. The operator ran `/compact` on a session 1.5M
    /// tokens over its window; it folded correctly, 1504081 -> 758940 tokens
    /// onto a new transcript with a 2167-token summary as its first item — and
    /// their screen said *"started summarization of the first 1500+ and then
    /// scrolled some s-tasks and that is it, not summary output, nothing"*. The
    /// work was right and the account of it went to the daemon's stderr.
    ///
    /// False means the summary has to be published, because nobody saw it.
    pub summary_was_streamed: bool,
}

/// What a re-seat did: the fork it made, the summary that carried the conversation
/// across, and — the part the operator asked for — which tools the model can now
/// call that it could not before, and which it has lost.
pub struct ReseatReport {
    pub fork: ForkReport,
    pub summary_turn: CompactionOutcome,
    pub gained: Vec<String>,
    pub lost: Vec<String>,
    /// **Tools on both sides whose schema changed** — the case that started the stale-prompt
    /// notice (`todo_write`'s grown `target`, then its `operator` need kind), and the one the
    /// gained/lost pair is silent about. Named here so the re-seat's report, and the model's
    /// copy of it, can say the schema the model now reads is not the one it was using.
    pub grew: Vec<String>,
    /// **What changed in each of `grew`, in a phrase** — `monitor (new \`github\`)`,
    /// `todo_write (\`todos[].needs[].kind\` gains \`monitor\`)` — so the model is told what to
    /// look for and not only that something moved. See [`schema_change`].
    pub grew_how: Vec<String>,
}

/// **Each grown tool, named with what changed in it** — [`grown_schemas`]'s names, each with
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

/// **What changed between two versions of one tool's schema, in a phrase** — new and removed
/// parameters at any depth (`todos[].needs[].name`), values an enum gained or lost, and
/// `wording` when only the text describing it moved. `None` when the two cannot be read.
///
/// The operator: *"trying to name a change inside a tool is nice to have"* — a re-seat used to
/// say only `Changed: monitor`, and the model had to find the `github` argument by itself.
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

/// **Tools seated on both sides whose schema text differs**, sorted — the `grew` half of the
/// stale-prefix comparison, as one function so the notice and the re-seat's report name the
/// same tools.
fn grown_schemas(was: &[String], is: &[String]) -> Vec<String> {
    let (was, is) = (tool_schemas(was), tool_schemas(is));
    is.iter()
        .filter(|(name, schema)| was.get(*name).is_some_and(|old| old != *schema))
        .map(|(name, _)| name.clone())
        .collect()
}

/// **What the MODEL is told about a stale prompt — once, as information.**
///
/// The operator: *"regarding reseat - since it is important id suggest to show me "select" with
/// one option - reseat and abstain and then when on abstain keep reseat select open. this way i
/// keep reseat in mind because it stares at me yet can continue at my risk"*. The person now
/// owns the decision through a card on their head that stays up until the prompt is current,
/// so the model's copy no longer ends with *say this to the person and keep saying it until
/// they act* — which made every plan-check a reminder the model passed on, the nagging the
/// operator reported. It ends with whose decision it is and that the model is not to press it.
/// Delivered once per sentence (`Harness::stale_told`), in the plan-check's turn.
pub fn stale_notice_for_the_model(said: &str) -> String {
    format!(
        "{said} For your information, said once: the person sees this on their screen as a \
         card offering `/reseat`, and whether to re-seat is their decision. Do not ask them to \
         re-seat and do not repeat this; carry on with the work, and if a tool you need is \
         missing from your prompt, say that plainly when it matters."
    )
}

/// **What the model is told when a reseat lands** — one line, and the three things it must not
/// say.
///
/// **R93.E8**: no token count, no request to restate the re-seat in a line (a restatement is not
/// work), and no *"stop asking for `/reseat`"*, which planted the word it forbade.
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
    /// **How big this conversation is, in the units its wall is in** — and the pair
    /// every `Config` conversion reads, brought up to date with it. R72.
    ///
    /// For a session with a measurement: the provider's own count at its last round,
    /// plus what has been appended since and will be sent (see [`roll_resident`]), and
    /// `cfg.ledger_scale` becomes `(ledger now, that count)` — so `should_compact`,
    /// `at_the_wall`, `room_for_next_turn` and `shown_tokens` all judge and print the
    /// one number, and `shown_tokens(ledger_len())` IS it, exactly. Called before every
    /// wall decision, because the ledger moves between rounds (a tool result, a notice)
    /// and a pair from the last round would convert those rows by a ratio they have
    /// nothing to do with.
    ///
    /// Without a measurement the reading is the ledger's own, and says so
    /// (`measured: None`): unscaled on a metered model that has not run a round —
    /// `Config::tokens_are_unscaled` — and the truth on a local one before its first.
    /// A scale this session did not anchor (a subagent's, inherited) is read the way it
    /// always was, as a ratio, rather than mistaken for a count of this conversation.
    pub fn measure_resident(&mut self) -> Resident {
        let len = self.session.ledger.len() as u64;
        if self.cfg.ledger_scale.is_none() {
            // A switch cleared the scale (`set_provider`, `set_own_server`): the anchor
            // was a measurement on the model before, and goes with it.
            self.wall_anchor = None;
            // **The carried ratio still converts.** Nothing here has measured THIS model,
            // so `measured` is `None` and a line says so; but a pair measured on the model
            // before the switch is a better count than the ledger's bytes, and using it is
            // what stops a fresh provider stopping the turn at round 0 with
            // `2708271 of 1000000 ledger tokens (unscaled …)`. `shown_tokens` falls back to
            // the ledger's own figure when there is no pair at all, which is the old
            // behaviour exactly.
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

    /// **The turn after the wall, without a human typing "continue".**
    ///
    /// A wall stop ends the turn with the work half-done, and the compaction that
    /// follows it (the shared turn tail, `turn_tail::run_turn`) replaces the transcript with a summary —
    /// after which, until now, nothing ran. The worker went idle, the prompt was
    /// still open, and the operator typed "continue" by hand to start the turn the
    /// summary had just recorded the state of. Measured on 2026-09-15 and on every
    /// compaction since: *"again have to write it after compaction"*.
    ///
    /// opencode never had the hole, because its compaction is a step inside the run
    /// loop and the run never ends. This harness cannot run compaction — it reaches
    /// the store, the resume chain and the registry, which `Sessions` owns — so the
    /// seam is a user item in the harness's own voice, the same shape as
    /// [`Harness::wake`]: `Speaker::Agent`, never an authorisation, and never
    /// readable as the operator's words. The shared turn tail calls this while
    /// there is room and a bound of continuations left; a wall the compaction did
    /// not clear still comes back as [`HarnessError::ContextWall`], which is the
    /// honest answer.
    /// **The turn after a re-seat: the model is told it happened, and answers.**
    ///
    /// The operator: *"that reseat happened shall reach the model too. and it should start the
    /// turn"*. `text` is [`reseated_for_the_model`]'s. The same seam as
    /// [`Harness::continue_after_wall`] and the plan-check's nag — a user item in the
    /// harness's own voice (`Speaker::Agent`), never readable as the operator's words — and
    /// the held deliveries ride in, because this is a turn the model takes anyway.
    ///
    /// A re-seat is a command on the worker's queue, so it never runs inside a turn: by the
    /// time this is called the turn that was running (if any) has ended, and this is the next
    /// step boundary.
    pub fn run_after_reseat(&mut self, text: &str) -> Result<Reply, HarnessError> {
        let mut text = text.to_string();
        if let Some(meanwhile) = self.deliveries().and_then(|d| d.at_turn_start()) {
            text.push_str("\n\n");
            text.push_str(&meanwhile);
        }
        self.compact_if_at_the_wall();
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        })
    }

    pub fn continue_after_wall(&mut self) -> Result<Reply, HarnessError> {
        // R93.E1: no "context wall" (the context check tells the model not to size by it), and
        // the compaction note right above already says what the summary is. Sent only after a
        // real compaction (`turn_tail::run_turn`).
        let mut text =
            "Continue from the summary above; don't redo what it records as done.".to_string();
        // R79: a continuation is a turn the model takes anyway — the held batch rides in.
        if let Some(meanwhile) = self.deliveries().and_then(|d| d.at_turn_start()) {
            text.push_str("\n\n");
            text.push_str(&meanwhile);
        }
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        })
    }

    /// `Sessions::compact_if_at_the_wall`'s contract, from the side that owns the
    /// harness: **compact when the next turn would not fit**, announce it in the
    /// same words the daemon's own sessions get, and apply the no-progress guard
    /// that stops a too-large summary from looping once per turn.
    ///
    /// Same name as the `Sessions` method on purpose: it is the same seam, and the
    /// next person asking *why does this resurface* should find both doors in one
    /// grep. The sibling's doc carries the history (the measured 2026-09-15 wall
    /// that created it); this one carries the child's half.
    ///
    /// Returns whether a compaction was actually ATTEMPTED — `false` when the
    /// session is not at the threshold, or automatic compaction is off, or no
    /// window is configured. A wall notice that says *"a compaction was
    /// ATTEMPTED"* needs that to be a fact rather than a hope, and until this
    /// existed a child's was a promise nothing behind it kept.
    pub(crate) fn compact_if_at_the_wall(&mut self) -> bool {
        self.compact_at_the_wall(WallWhy::Tidy)
    }

    /// **The other question: a turn was just REFUSED for context length — is
    /// there a way forward?** Same body as [`Self::compact_if_at_the_wall`], and
    /// the difference is one gate: this one does not read `auto_compact`, because
    /// the no-progress guard turns that flag off **without freeing anything**, and
    /// a refusal recovery gated on it would refuse the only lever it has.
    ///
    /// MEASURED 2026-10-09, session `s-1789919514688401228`: one automatic
    /// compaction made no room (1,849,499 ledger tokens in, still over a
    /// 1,000,000-token plan), the guard switched compaction off, and the next
    /// thirty provider refusals met a recovery that would not act. `plan_overrun`
    /// — the two-half fold a `Refused` compaction reaches through `compact()` —
    /// is the strategy for exactly that state, and it never ran once.
    ///
    /// The refusal itself is the evidence, so there is no threshold to re-check:
    /// the provider measured the prompt against the real window and said no. And
    /// nothing stands this door down: the latch that used to (`forced_gave_up`, set
    /// when a forced fold "still left the session at the wall") was set on an
    /// ESTIMATE of the new base, and on 2026-10-11 it switched the overrun
    /// protection off for the daemon's life after a fold deepseek then measured at
    /// half the window. The wall controller (`crate::wall`) picks the fold: the one
    /// the size calls for when nothing has been tried, the sliced fold from every
    /// other state, `Stuck` included.
    pub(crate) fn compact_because_refused(&mut self) -> bool {
        self.compact_at_the_wall(WallWhy::Refused)
    }

    /// **One body, two questions.** `should_compact`/`at_the_wall` are the
    /// decision — the arithmetic is NOT copied — and `why` is the only thing the
    /// two doors disagree on.
    fn compact_at_the_wall(&mut self, why: WallWhy) -> bool {
        // **Measured first, so the decision and the line are one count (R72).** The
        // roll brings `ledger_scale` to `(this ledger, the provider-sized count)`, and
        // `should_compact` on the ledger figure below is then exactly the provider's
        // count against the provider's window.
        let now = self.measure_resident();
        let resident = self.session.ledger.len() as u64;
        // The one gate the two questions differ by: the PRE-EMPTIVE door reads
        // `auto_compact` and the threshold; the REFUSED door reads neither. Which fold,
        // and whether the pre-emptive door stands down (an unmeasured base, `Stuck`), is
        // the wall controller's — one state, changed only by measured counts.
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
        // **Shown in the operator's units, decided in the ledger's** — the same rule
        // the `Sessions` sibling states: `should_compact` above is the decision,
        // and the line prints the count it was made on, in the window's own units
        // (`Config::shown_wall`), with the unit named.
        //
        // `Config::scale` and not `ledger_scale`: on a session whose switch carried a
        // ratio, the count the decision was taken on is the CONVERTED one, and a line that
        // converted with nothing would print the ledger's bytes beside a unit it had just
        // named as the provider's.
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
        self.hub.publish(SessionEvent::Warning {
            code: "auto_compact".into(),
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
                // The new base measured in its own right — the fork re-bases the anchor
                // (`roll_resident`), so the after-count is the base's, not the old
                // history's ratio applied to it.
                let base = self.measure_resident();
                // **An estimate is said as one** — `≈`, until the next round measures
                // it. The report used to print the base's estimate in the same breath
                // as the measured count it was folded from, and the guard below it
                // JUDGED that estimate: 1,039,055 for a base deepseek counted at
                // 507,355 (pg-noop, 2026-10-11).
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
                // **Whether it helped is not decided here.** It used to be — `at_the_wall`
                // on the base's estimate, then `auto_compact = false` and the refused
                // door's latch, for good. A metered base is judged by the first round
                // that measures it (`observe_wall`, from `run_rounds`); a local one was
                // judged inside `compact_with`, because its prompt IS the ledger and
                // the count is exact. Either way the controller said it, once.
                let cut = if report.fork.truncated {
                    " The summary was CUT OFF at the model's length limit — it is \
                     incomplete, and the base says so too."
                } else {
                    ""
                };
                let said = compacted_line(r, base, &self.token_unit(), self.provider.is_some());
                eprintln!("  {}: {said}{cut}", self.cfg.session_id);
                self.hub.publish(SessionEvent::Warning {
                    code: "auto_compact".into(),
                    detail: format!("{said}{cut}"),
                    compaction: None,
                });
            }
            Err(e) => {
                // Announced and swallowed, for the same reason as the sibling's: the
                // room check in `submit_as_a_normal_session` is what decides the
                // turn's fate, and a compaction that did not run leaves no room —
                // so the wall error stands, and this line is why the operator can
                // see it was tried rather than infer it never was.
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
                // **And if this is the first request after a model switch, say how to go
                // back.** The compaction runs through this harness, not through the turn
                // tail, and it is the door the measured failure took: the summary turn
                // carried the same history and was refused the same way.
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

    /// **A measured count, into the wall controller — the one place its state changes on
    /// a measurement, and the one place that says so.**
    ///
    /// `count` is in the window's units: a round's `prompt_tokens` (called from
    /// `run_rounds` at every round's end, children included — they run the same loop),
    /// or a local base's exact size (from [`Self::compact_with`]). The judgement the
    /// no-progress guard used to make on the estimate right after the fork is made here,
    /// on the provider's number: the pg-noop case of 2026-10-11 would have read
    /// *"measured: the ordinary fold left 507355 of 1000000 deepseek tokens"* and stood
    /// nothing down.
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

    /// **The operator's re-arm** — `/auto-compact on`. Clears `Stuck` (and whatever the
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

    /// **Whether the count is the estimate of a metered base no round has measured** —
    /// the one count no wall judgement is made on (`crate::wall`).
    pub fn base_is_unmeasured(&self) -> bool {
        self.provider.is_some() && self.cfg.wall.awaiting_on(&self.session.transcript_id)
    }

    /// The wall controller's state, for a caller that reports it.
    pub fn wall(&self) -> &crate::wall::WallControl {
        &self.cfg.wall
    }

    /// **The controller's `Stuck` on the session row**, or its absence — the v19 pair is
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

    /// **A compaction with the fold named** — [`Self::compact`] is the ordinary one, the
    /// one `/compact` asks for; the wall controller names a harder one when a measured
    /// count said the last did not make room (`crate::wall`).
    ///
    /// Every fold that lands is reported to the controller HERE, whoever asked for it,
    /// so a `/compact` on a stuck session restarts the ladder rather than leaving a state
    /// about a transcript that is gone. A metered base waits for its first round to be
    /// measured; a local one is measured now, because its prompt IS the ledger.
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

    /// **Re-seat this conversation onto the tool list that is seated NOW.**
    ///
    /// The tool schemas live in the stable prefix, and a session's prefix is fixed
    /// the moment it is created — a resume replays the stored one because the
    /// stored tokens were produced under it, and a compaction fork keeps it for the
    /// same reason. So a conversation started by a daemon with no shell can never
    /// call one, however the daemon that reopened it is seated: the registry has
    /// the tool, the prompt has never heard of it, and the banner — computed from
    /// the registry — says `exec` while the model sits there unable to.
    ///
    /// That is the whole of the operator's hour: *"i did letibot --stop and
    /// leticode --continue but still no exec"*, and then the diagnosis, which was
    /// theirs: *"i guess it is because we announce tools once at the start of the
    /// session"*. Exactly so.
    ///
    /// What cannot be done is swapping the list under a live transcript: every
    /// token in it was produced under the old prefix, and appending to them under a
    /// new one builds a prompt no model was trained on — the same rule the dialect
    /// check enforces at resume. What CAN be done is what compaction already does
    /// for a different reason: summarise, then continue in a fresh transcript. The
    /// only difference here is which prefix the fork opens under, so this is
    /// `compact` with one argument changed.
    ///
    /// The cost is stated because it is real: the conversation continues from a
    /// summary, not from its own tokens, and the prefix is new so the server's
    /// cache for it is cold. Both are what changing the announced tools costs, and
    /// a version that hid either would be hiding the thing the operator is paying.
    /// **The prompt this daemon would seat a NEW conversation with**, and the
    /// store row for it — or `None` when that is already the prompt being spoken.
    ///
    /// Both `/reseat` and every compaction fork want the same two values, computed
    /// the same way, so they are computed once here. The probe render is what
    /// makes the row honest: a `stable_prefix` written with an empty token list
    /// verified as `hash chain broken at row 0` on the way back in, because the
    /// tokens are what a resume rebuilds the chain from.
    /// Public for the same reason [`Harness::fork_to_summary`] is: the summary
    /// turn needs a model server and this does not, so the re-seat half of a
    /// compaction is drivable — and therefore testable — offline.
    pub fn reseat_target(&mut self) -> Result<Option<(StablePrefix, String)>, HarnessError> {
        // **The standing notes are re-read HERE, on every base rebuild.** This is
        // the operator's ask — *"make sure AGENTS.md reread after each compaction"* —
        // and the seam it lands on is this one, because every fork computes its next
        // prefix through here: `/compact`, the wall, `/reseat`, `/reingest`. The
        // section is swapped inside the composed prompt by marker, so `prompts.toml`
        // overrides and a fabric block appended after it are carried through
        // untouched — the frozen-input rule this field's own history set
        // (`compose_for_seat`) loses nothing, and only the notes move.
        //
        // A compaction is a cold prefill by construction (the fork replaces the
        // history), so moving the head here costs nothing extra; and when the files
        // have NOT changed, the swap is a no-op, `next == self.prefix` below, and
        // the fork keeps the seated prefix without a new row. This and open are
        // the whole schedule — a reseat or a compaction is a re-open of the
        // same head — and it is the same for every session.
        match self.reseat_candidate() {
            None => Ok(None),
            Some(next) => self.seat_target(next).map(Some),
        }
    }

    /// **The prompt a fork would land on, decided and NOT yet recorded** — the first half of
    /// [`Self::reseat_target`], for a caller that has a check to make before anything is
    /// written (R91.13: `reingest` refused an over-window carry AFTER `cfg.system` had moved
    /// and a `stable_prefix` row had been written, so every refused attempt left an orphan row
    /// and a system prompt the session was not speaking). `None` when the prompt is unchanged
    /// (the standing notes are still taken up then, as they always were), or when there is no
    /// store to record a new one in.
    fn reseat_candidate(&mut self) -> Option<StablePrefix> {
        let declared = crate::standing_notes::preloaded(self.skills.as_ref(), &self.cfg.skills);
        let notes = crate::standing_notes::section_with_skills(
            &self.cfg.workspace,
            &crate::standing_notes::global_dir(),
            &crate::leticode_config::BoxDirs::for_this_box(),
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
        // No store is not a refusal here — a compaction without one refuses later
        // and for its own reason. It is simply nowhere to record a new prefix, so
        // the fork keeps the old one.
        self.store.as_ref()?;
        Some(next)
    }

    /// **Record the prompt [`Self::reseat_candidate`] decided**: its `stable_prefix` row, and
    /// `cfg.system` moved to it — the second half of [`Self::reseat_target`].
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

    /// **Re-seat without summarising: carry the whole conversation across.**
    ///
    /// **This is what `/reseat` does.** The operator asked for it — *"is there a
    /// way to reseat without summarizing? … like reingest full context"* — and
    /// then made it the default: *"id say flip it - reset is loseless and reset
    /// summarize will be not"*. A verb named for message zero should not cost
    /// you everything under message zero unless you said so.
    ///
    /// A tool's schema lives in the stable prefix, which is message zero, so a
    /// running session keeps the prompt it opened under and a schema change
    /// needs a fork. [`Harness::reseat`] pays for that fork with a summary turn
    /// — which is right when the point is to shrink, and pure loss when the
    /// point is only to change message zero.
    ///
    /// This forks onto the seated prompt with every item carried verbatim. The
    /// machinery is `fork_to_summary`'s own: `tail` has always been "items kept
    /// as themselves", and this passes all of them with no summary in front.
    ///
    /// **What it costs.** The prefix changed, so nothing the server has cached
    /// matches and the next turn pays a cold prefill of the WHOLE history — on a
    /// metered provider, the whole conversation re-sent and re-billed once. That
    /// is the trade against `reseat`, and it is the operator's to make: a summary
    /// is cheaper and lossy, this is dearer and lossless. The default is the
    /// lossless one, because a re-seat that quietly ate the conversation is a
    /// surprise you cannot undo, and a bill is one you can see coming.
    ///
    /// It refuses where it cannot help: over the window, a verbatim carry would
    /// fork onto a base that cannot be prefilled at all, and the honest answer
    /// there is the summary it was trying to avoid.
    pub fn reingest(&mut self) -> Result<ReseatReport, HarnessError> {
        if self.store.is_none() {
            return Err(HarnessError::Setup(
                "re-seating forks the conversation onto a new prompt, and a fork needs a \
                 store to write it to; this session has none."
                    .into(),
            ));
        }
        // **Decided, not yet recorded (R91.13)** — the row and `cfg.system` wait for the fit
        // check below, so a refusal changes nothing.
        let Some(next) = self.reseat_candidate() else {
            return Err(HarnessError::Setup(
                "this conversation's prompt already carries exactly the tools that are \
                 seated, so there is nothing to re-seat."
                    .into(),
            ));
        };
        // **The carry has to fit, and "fit" has to be measured in ONE unit.**
        //
        // `resident` below is counted off the ledger, with this box's own
        // vocabulary. On a metered provider that is not the number the provider
        // will refuse at and not the number the operator's header shows: the
        // ledger holds the reasoning rows that `letibot_provider::messages` drops,
        // so it over-counts. Measured on the operator's own session, 2026-09-20:
        // 991,596 in the ledger against 671,280 the provider counted for the same
        // conversation, 49% apart.
        //
        // This compared the ledger figure against `planning_window()`, which is
        // the window in LEDGER units — correct, but only while `ledger_scale` is
        // known. Unmeasured, `planning_window` hands back the provider's own
        // window unchanged, and the comparison silently became ledger-against-
        // provider. That refused a `/reseat` whose carry had 400k tokens of room,
        // and printed `991596 of 1000000` under a header reading 671k with no way
        // to reconcile the two.
        //
        // So: convert to the PROVIDER's units and compare there, because that is
        // the number the operator can check. And when nobody has measured the
        // ratio yet, **do not refuse** — an unmeasured ratio is not a small one,
        // the error is all in one direction, and losing a lossless re-seat over a
        // conversion the daemon admits it cannot do is the worse failure.
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
        // do not — a local endpoint counts with the vocabulary the ledger uses, so
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

        // No summary turn at all — that is the whole point — so the outcome is
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
        };
        self.compacting = true;
        let out = (|| -> Result<ReseatReport, HarnessError> {
            // **The whole conversation, carried as it is** — that is what a re-ingest is,
            // and an empty `because` says no decision about a tail was taken. It goes
            // through `ForkTail` rather than a bare slice because the fork needs one
            // shape for both callers, not because this is a compaction.
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
        // **And the harness now SPEAKS the prompt the fork landed on** — `reseat` makes the
        // same swap at the same point, after the fork, for the reason it gives there.
        //
        // This line was missing. The fork opened the new transcript under `next`, so a local
        // turn (which renders from the ledger) spoke the new prompt; but a metered turn is
        // sent `self.prefix.system` and `self.prefix.tools_json`, so a provider session went
        // on being handed the OLD tool schemas after a `/reseat` that reported them rebuilt —
        // until a daemon restart resumed the new transcript under its own stored prefix. And
        // `reseat_target` compares against `self.prefix`, so a second `/reseat` re-forked a
        // session that was already on the seated prompt. pg-noop is a deepseek session, and
        // its `/reseat` at 00:32 on 2026-10-12 was this one.
        self.prefix = next;
        self.prefix_id = next_id;
        out.gained.sort();
        out.lost.sort();
        self.publish_settings();
        // **Under the window but short of a turn's room**: the conversation was already this
        // size on the old prompt, and the reseat changes that by the schema's difference. The
        // room a turn needs is the compaction's to make before the next turn, as it would have
        // been without the reseat — so this goes ahead and says so. **Said once the fork has
        // landed (R91.13)**: it reads "re-seated", and a fork that failed re-seated nothing.
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

    /// **Re-seat and summarise**, replacing the conversation with the summary.
    ///
    /// This is `/reseat summarise`: the lossy kind, and it is the one you ask for
    /// by name. A bare `/reseat` runs [`Harness::reingest`]. Reach for this when
    /// the point is to shrink as well as to change message zero, or when the
    /// conversation no longer fits in front of the window.
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
            let outcome = Self::run_summary_turn(
                &self.hub,
                &mut self.engine,
                &mut self.session,
                &mut sink,
                &answerer,
                &mut ladder,
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

    /// **Compaction is one summary turn appended to the conversation itself** --
    /// unless the conversation arrived with no room to hold one.
    ///
    /// The ordinary path is the cheap one and is used whenever it can be: the
    /// server already holds the prompt, so the prefill is one message. The
    /// operator's rule, after a batched rewrite that used a fallback for
    /// everything and was reverted for it: *"when no overrun - normal
    /// compaction"*.
    ///
    /// OVERRUN is when it arrives over the budget -- *"either unlucky tool like a
    /// file read or model change"* -- so the summary turn has nowhere to write.
    /// Then, and only then, the conversation is summarised somewhere else, and
    /// WHICH way depends on the backend rather than on the conversation:
    ///
    ///   * a cloud provider takes messages rather than tokens, and a prefix-cache
    ///     miss is priced rather than waited on, so the clean fold is affordable:
    ///     summarise the first half, continue on `summary ++ second half`, with
    ///     the recent past kept VERBATIM.
    ///   * the local server prefills at 200-375 tok/s, where that same fold is ten
    ///     minutes of silence. So both halves are summarised instead, overlapping
    ///     at the seam, with the expensive half arranged to be a true prefix of
    ///     what the server already holds.
    ///
    /// See `crates/turn/src/compaction.rs` for the arithmetic and for why the
    /// cache and the log are two different things.
    /// **Summarise the first half, keep the rest verbatim** — the fold, carried out.
    ///
    /// Two arms go through here so they cannot drift: the overrun arm (the history is
    /// past the window) and the ordinary arm (a remote session is at the wall, where
    /// `plan_fold`'s own doc says the fold is the cleanest of the three strategies and
    /// the one to prefer wherever it can be afforded).
    ///
    /// **The summary turn runs against the FIRST HALF only**, in a scratch transcript,
    /// and that is also why the fold is the *safe* answer at the wall rather than
    /// merely the better one: the ordinary path asks the model to read the whole
    /// history and write a summary with less than a reserve of room left to do it in.
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

        // **A compaction re-seats.** It is already forking onto a fresh transcript
        // and already paying the cold prefill that a new prefix costs, so carrying
        // the tool list this daemon seats NOW is free — and not carrying it is
        // what made `/compact` then `/reseat` two summaries instead of one.
        //
        // Measured in the operator's own session (`…#t8`, five rows): row 0 is the
        // `/compact` summary; row 1 is `/reseat` asking for a summary of a
        // transcript that is already nothing but a summary; row 2 is the model,
        // with nothing to summarise, running `git log` instead — which tripped the
        // "a summary is a record, not an action" guard and refused the re-seat;
        // rows 3 and 4 are the retry. Three summary turns and two cold prefills to
        // pick up a tool list. Their reading: *"let compaction automatically
        // reseat so new tools picked up"*.
        //
        // `None` when the seated prompt is the one already being spoken, which is
        // the common case and costs a comparison.
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

        // **The fold the controller named** (`crate::wall`). The ordinary one is what
        // a compaction always was: the fold the size calls for. The sliced fold is the
        // two-half plan asked for by NAME — `plan_overrun` reaches it by size only, and
        // a history that fits one summary call never did, however little the ordinary
        // fold freed (pg-noop, 2026-10-11: 941,101 folded to 507,355, half verbatim).
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
                // **The tail is planned from the history as it stands BEFORE the
                // summary turn**, and that is not a detail: `run_compaction`
                // appends its instruction and its answer to this session, and a
                // tail that could reach those would put the question and its
                // answer into the record that replaces them. The clone is the
                // same one the overrun arm takes and for the same reason.
                let items: Vec<TranscriptItem> = self.session.items.clone();
                // **The first choice when a remote backend can afford it.** `plan_fold`'s
                // doc: the fold is the cleanest of the three strategies and the one to
                // prefer wherever it can be afforded, and what it costs is the cache —
                // ten minutes of cold prefill on a local llama.cpp, six cents on a cloud
                // provider. Local keeps the ordinary path below, deliberately.
                //
                // **Asked with the door's own reserve, not the window's.** The door fires
                // at `resident + headroom >= window` — a headroom BEFORE the window is
                // full — so a resumed session arrives inside the band where `plan_fold`
                // used to decline and this arm used to summarise the whole history down to
                // a 15,000-token tail. Measured 2026-10-10, session
                // `s-1789462738453908838`: 958,397 in, 100,005 out, and the part of the
                // conversation that was still live went with it.
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
                        code: "auto_compact".into(),
                        detail: format!(
                            "at the wall: summarising the first {split} item(s) and continuing \
                             on the summary plus the rest, verbatim"
                        ),
                        compaction: None,
                    });
                    let (harvest, tail, tail_split) =
                        self.compact_by_folding(&items, split, &mut sink)?;
                    // The fold's summary ran in a scratch transcript, so its outcome is
                    // built here rather than coming back from `run_summary_turn`: none of
                    // this session's own prefix was reused and no tokens were served from
                    // its cache. The overrun arm builds the same record for the same
                    // reason.
                    let outcome = CompactionOutcome {
                        turn_id: format!("{}#overrun", self.transcript_id),
                        summary: harvest.summary,
                        tool_calls: harvest.tool_calls,
                        truncated: harvest.truncated,
                        cached_tokens: 0,
                        reusable: 0,
                        generated_tokens: 0,
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
                let outcome = Self::run_summary_turn(
                    &self.hub,
                    &mut self.engine,
                    &mut self.session,
                    &mut sink,
                    &answerer,
                    &mut ladder,
                )?;
                if outcome.tool_calls > 0 {
                    return Err(HarnessError::Setup(format!(
                        "the summary turn proposed {} tool call(s); a summary is a record, \
                         not an action, so nothing was compacted. The transcript is \
                         unchanged apart from the instruction and the turn themselves — \
                         try again.",
                        outcome.tool_calls
                    )));
                }
                // **Summary plus verbatim recent turns, on the remote path only**
                // — R27's ruled split, and `plan_compaction_tail` carries the
                // reason. A local model gets the template and nothing else,
                // because a tail here would be resident tokens competing with the
                // KV pressure that called this compaction in the first place.
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

                // A cloud provider is the affordable case; see the doc above.
                //
                // `tail_split` is carried out of the arms with the tail: the fold
                // splits halfway BY TOKENS, so it can start mid-exchange without
                // anybody having decided that, and a tail that opens with an answer
                // to a question that is no longer present is the one thing the
                // reader has to be told about (R27).
                let (harvest, tail, tail_split) = if self.provider.is_some() {
                    // **The reserve is the door's, not the window's**: a history that
                    // fits only by spending `headroom()` is one this arm was asked about.
                    // The sliced fold skips the verbatim half altogether — that half is
                    // what the gentler folds were measured leaving at the wall.
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
                                code: "auto_compact".into(),
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
                        code: "auto_compact".into(),
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
                };
                // The overrun fold's own reason, not `plan_tail`'s: this tail was placed
                // by arithmetic on the window, half way BY TOKENS, and the reason the
                // whole tail is what it is has nothing to do with a budget it was
                // measured against. Empty when there is no tail at all — a local
                // overrun summarises both halves and carries nothing.
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

    /// Take on the prompt the fork just opened under.
    ///
    /// **Only after the fork has landed.** Until then this harness is still
    /// speaking the old prompt, and a `self.prefix` that ran ahead of the
    /// transcript would make every later turn build the wrong bytes — the same
    /// ordering `reseat` spells out at its own swap.
    /// Returns what the announced tool list gained and lost, for the report.
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

    /// **The fork.** Replace the resident history with one summary item, in a new
    /// transcript the store links back to this one.
    ///
    /// Split from [`Harness::compact`] so it can be driven without a model server:
    /// the summary turn needs one, this half needs only the vocabulary, the store
    /// and the harness — so the store, resume-chain and ledger behaviour is
    /// testable offline, and the turn side is tested in `letibot-turn`.
    ///
    /// The old transcript is persisted first and kept whole — compaction never
    /// deletes anything, it stops *carrying* it. The new transcript's body is one
    /// system-update item holding the summary; the next prompt is the prefix plus
    /// that item, which is the cold re-prefill `docs/compaction.md` §3 says is
    /// unavoidable, measured in tokens rather than minutes.
    pub fn fork_to_summary(
        &mut self,
        outcome: &CompactionOutcome,
        onto: Option<&StablePrefix>,
        onto_id: Option<&str>,
        tail: ForkTail<'_>,
    ) -> Result<ForkReport, HarnessError> {
        self.fork_onto(outcome, onto, onto_id, tail, None)
    }

    /// **The fork, with the new transcript's first item handed in** — or `None` for the
    /// note a compaction or a re-seat writes about itself.
    ///
    /// One body for every fork, because everything around the first item is the same
    /// act whoever asks: the old transcript persisted and kept whole, the store row that
    /// links the two, the fingerprint, the swap, `TranscriptForked`, the stored context
    /// count cleared. `/clear` is the one caller with a first item of its own (an
    /// orientation built from the session's state, spoken as the harness — see
    /// `harness/clear.rs`), and a second copy of this function for it would be a second
    /// place for the rules above to drift.
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
        // **Whether this fork carries the conversation verbatim** — a re-ingest (`/reseat`):
        // no summary, no first item of its own, every row of the history in the tail. Then the
        // provider's last measurement still describes the conversation, and is kept (below).
        let verbatim = lead.is_none()
            && outcome.summary.is_empty()
            && tail_split.is_none()
            && tail_items.len() == self.session.items.len();
        let prefix_before = self.session.ledger.prefix_tokens().len() as i64;
        // The prefix the FORK opens under, which is the caller's choice and not
        // always this session's. A compaction keeps the one the conversation has
        // been speaking under; a re-seat is the whole point of being able to hand
        // in a different one, because the tool list lives in here.
        // Cloned rather than borrowed: `persist` below needs `&mut self`, and a
        // prefix is two strings and a small vector — cheaper than threading a
        // borrow through the whole fork.
        let onto: StablePrefix = onto.cloned().unwrap_or_else(|| self.prefix.clone());
        let onto_id: String = onto_id
            .map(str::to_string)
            .unwrap_or_else(|| self.prefix_id.clone());
        // The old transcript is flushed with the summary turn in it, and before the
        // fork writes anything: a fork that dropped the turn that justified it
        // would leave the store unable to say where the summary came from. With no
        // store this is a no-op, and the refusal below is the only thing that
        // happens.
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
        // **Nothing is written to the store yet (R91.2).** The transcript row, the fingerprint
        // and the stale flag were written here, before a row of the new transcript existed: a
        // crash during the carry resumed onto a partial conversation, and an error before the
        // swap left the dead fork the store's newest and the reseat offer cleared on a session
        // still speaking the stale prompt. Now the row goes in UNSEALED just before the swap
        // (`Store::put_fork_unsealed` — not what a resume opens), and the seal, the fingerprint
        // and the stale flag follow the last row (`Harness::seal_fork`, from `persist_reporting`).
        //
        // What the seal does, kept here because this is where a reader looks for it:
        //
        // **The fingerprint follows the transcript.** A fork is the other place a session's
        // prefix is decided — `/reseat`, a compaction that picked up the seated tools, a dialect
        // or model switch — and a column left at what the session was CREATED with would report
        // every re-seated session stale on the next attach. A plain compaction writes the same
        // hash it read, deliberately: the write is unconditional so that there is no second rule
        // about when it is needed.
        //
        // **And there is nothing left to tell an attaching head.** A fork either moves the
        // session onto the seated prompt or leaves it on one that was never stale (`onto` is
        // `self.prefix` only when `reseat_target` answered `None`), so the stale notice stops
        // being true — once the fork is the session's, and not before.
        let fingerprint = prefix_fingerprint(self.cfg.seat, &onto);
        let mut next = self
            .engine
            .open(&new_id, &onto)
            .map_err(|e| HarnessError::Setup(format!("opening the compacted transcript: {e}")))?;
        // **A record that was cut off says so, in the record.**
        //
        // This is the base every later turn reads as the whole of what came
        // before. When the summary turn ran out of room mid-sentence, that base
        // used to claim to be "the summary, written over the full history" — with
        // the last third missing and nothing anywhere saying it. A model reading
        // it cannot tell, and neither could the operator until they read to the
        // end and found `&page.final_url, &page`.
        //
        // Not a refusal: a session at the wall has nowhere else to go, and an
        // incomplete record that admits it is more useful than no compaction at
        // all. The sentence goes FIRST, before the summary, because a reader who
        // stops early is exactly the reader who needs it.
        let cut = if outcome.truncated {
            " The summary is incomplete (cut off); treat anything missing as unknown and check \
             with `transcript`."
        } else {
            ""
        };
        // **A fork that summarised says so; one that did not must not.**
        //
        // An empty summary is `reingest`: the prompt changed and the conversation
        // did not. Telling the model "everything before this is replaced by the
        // summary below" and then showing it no summary — with the whole history
        // underneath — is a sentence that contradicts what it can see.
        //
        // **And the sentence had to change when the tail arrived.** It said
        // "everything said before this point is replaced by the summary below",
        // which stopped being true the moment a remote compaction carried the
        // newest exchanges verbatim: the reader can see both, and a note that
        // claims a region was replaced while showing it is the same class of
        // contradiction as the re-ingest case above.
        // One implementation with the sentence the head is given: see
        // `CompactionTail::why_line`, which both readers call.
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
            // **WHY there is no tail, said in the row itself** — R41's shape one document
            // over: an absence with two causes and one appearance. The `carried` clause below
            // says how much when there is some; with none, this said nothing at all, so a
            // reader of the record could not tell R27's ruling working from the budget losing
            // to one item.
            //
            // **Here and not only on the wire, because this is the durable one.** The
            // `compacted` warning is published to attached heads and the store has no events
            // table: after a restart the transcript item is all a reader has, and it is where
            // the question has to be answerable. `tail.because` is empty for a re-seat, which
            // is not a compaction and gets no clause.
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
                    // R93.E7: one line, no transcript id — the `[reseat]` notice is the one
                    // that says what changed, and a carried conversation keeps every
                    // earlier re-seat's note, so these stacked two and three deep.
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
        // **The recent past is carried over verbatim, not described.**
        //
        // Compaction used to keep none of it: the new base was the summary and
        // nothing else, so a session came back from a compaction unable to see the
        // turn it was in the middle of -- that turn was now a sentence about a
        // turn. The tail is the newest items, under their own budget, appended after
        // the summary so the order of the conversation is preserved: everything
        // old as prose, then the last stretch as itself.
        //
        // **Mapped to the wire's role/text as it is appended**, so the report and the
        // bytes cannot describe different tails: one pass over one slice.
        let mut body: Vec<TranscriptItem> = Vec::with_capacity(1 + tail_items.len());
        body.push(note);
        body.extend_from_slice(tail_items);
        let tail_turns = wire_turns(tail_items);

        // **A carry the head can watch.** Rendering and writing the new transcript is the long
        // half of a fork that carries the conversation (`/reseat` carries all of it), and the
        // worker is inside this command until it ends — so a head with nothing on its screen but
        // the old conversation and a `queued` prompt was the whole of what the operator saw for
        // 6.5 minutes on 2026-10-12. Said from the first moment (the render is seconds of its
        // own on a long conversation), then once per committed chunk below — never per row, so
        // the head's queue is not flooded (see `FILLING_STRIDE`) — and ended by `total of
        // total`, which is what clears the line. Only for a carry big enough to be a wait.
        let carrying = body.len() >= FILLING_STRIDE;
        // **Two phases, each with its own bar**: the render onto the new prompt, which is the
        // long one and used to tick nothing at all — the bar sat at `0 of N` for all of it, then
        // jumped and cleared as the write below raced through (the operator: *"progress bars do
        // not work … always at zero"*) — and the write of the rendered rows to the store.
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
        // The swap is the point of no return, and it is deliberately **after** every
        // store write that could fail: a harness that swapped and then could not
        // persist would be a session whose transcript row exists but whose body does
        // not, which is the one state a resume cannot rebuild honestly.
        self.session = next;
        self.transcript_id = new_id.clone();
        self.persisted = 0;
        self.unsealed_fork = Some(UnsealedFork {
            transcript_id: new_id.clone(),
            fingerprint,
        });
        self.reconcile(&mut sink, &body);
        // **AND EVERY ROW THAT WAS JUST PUBLISHED REPLACED THE ONES EVERY READER IS HOLDING.**
        //
        // `append_items` above publishes them under `new_id`, so every row a reader is now being
        // sent is numbered in a transcript that is not the one the rows it already holds belong
        // to. Nothing in the stream said so, and a reader can only fold what it is told:
        // MEASURED in the head's fixtures, a 40-row conversation plus a 60-row carry left
        // `items.len() == 100` — the conversation drawn twice, the reader anchored on a row of the
        // transcript that went. The operator, scrolled up and reading, typed `/reseat` and was
        // left with *"thousands of lines 'below'"*.
        //
        // **Two readers, and the daemon's own is the one that would have been missed.** A head
        // folds this into its rows; the hub's view folds it into the rows a LATE head is handed,
        // and without it a head that attached after the fork would be given both conversations —
        // the same defect one attach later, with no event to blame it on.
        //
        // **After the rows, and the order is not what makes it correct** — which is the reason it
        // can be chosen for what it costs instead. The event names the transcript it replaces
        // rather than a count, so a reader drops exactly those rows whenever it hears the
        // statement: a reader that gets the statement first clears and fills, and one that gets
        // the rows first holds both for an instant and then drops the right ones. Two things then
        // decide the order, and both point the same way:
        //
        //  * **A reconnect that missed the statement is not stranded.** A head acks what it has
        //    written and resumes from that seq, so a statement published BEFORE the rows leaves a
        //    one-seq window in which a resuming head replays the rows and not the boundary between
        //    them — and holds both conversations with nothing left to tell it. Published after,
        //    the same window replays the statement alone, which is the half that is enough.
        //  * **A fork that FAILS must not have already emptied every reader.** `append_items`
        //    returns `Result` and the session is swapped only if it succeeds; a statement
        //    published first would leave every reader with nothing while the session still speaks
        //    the transcript it was told it had left.
        self.hub
            .publish(letibot_sessionlog::SessionEvent::TranscriptForked {
                transcript_id: new_id.clone(),
                parent_id: old_id.clone(),
            });
        // The write, ticking per committed chunk — see "a carry the head can watch" above.
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
        // **Past the swap a failed write is not a failed fork (R91.2).** The session speaks the
        // new transcript in this daemon, so returning an error here sent every caller down its
        // refusal path — `reingest` returned before `self.prefix = next`, so metered turns sent
        // the OLD schemas against the new transcript, and the operator was told "Nothing was
        // changed". The rows stay in memory and the next store write takes them up from where
        // this one stopped; the fork is sealed when it does, and until then a restart resumes
        // the parent, whole. Said, because the gap is real.
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
        // **The stored context size is about the OLD conversation.** It is the
        // last prompt the provider measured, and that prompt no longer exists:
        // this fork replaced the history with a summary. Only a turn can put a
        // real number here, so until one runs there is no number — the operator
        // restarted a session that had just compacted 1.5M tokens down and the
        // header still read `1.05m`, because nothing between a compaction and
        // the next turn ever wrote to this row.
        //
        // Cleared rather than estimated. The count this row holds is the
        // provider's own, and dividing the new base by a measured ratio would put
        // a derived number where every other reader expects a measured one.
        //
        // **Except a verbatim fork**, which replaced nothing: a `/reseat` carries every row across
        // and changes only message zero. Clearing its measurement threw away the one thing a
        // restart needs to read the ledger in the provider's units — the operator's peerdb
        // session, 2026-10-12, re-seated, restarted before a round measured again, and was
        // folded on 2.4M ledger bytes it had never been measured at. So the provider's count is
        // kept and its ledger anchor moved by exactly the prefix's change: every row sits where
        // it sat, shifted by the new message zero. That is the measurement, re-addressed, and not
        // an estimate.
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
            // The reason, in the wire's own shape — and `None` when there is no reason to
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

    /// **Every row of the fork is stored: make it the session's conversation** (R91.2) — the
    /// seal, then the fingerprint and the stale flag that follow the transcript (see
    /// `fork_onto`). A seal that fails stays owed and is tried after the next store write; the
    /// parent is still what a resume opens, which is consistent, only behind.
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

    /// **A resume restores the wall controller's `Stuck`**, from the row it wrote.
    ///
    /// `restore_provider_choice`'s shape, for its reason: without this call the row
    /// would only be read on the lazy path a head's switch reaches, and the one resume
    /// everybody types (`--continue`) would come back re-armed — the finding silently
    /// unrecorded, which is the loop it exists to stop. And it is no longer for good:
    /// the first round that measures the session comfortably below the wall releases
    /// it (`observe_wall`). Returns the sentence a resume should say, or `None` when
    /// there was nothing to restore.
    ///
    /// Rows written before 2026-10-11 hold the old guard's pair in LEDGER tokens; they
    /// are printed as they are, and the next measured count is what decides.
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
        // **A pair that cannot be the provider's count is not a finding.** A Stuck the
        // controller wrote was MEASURED — the provider's `prompt_tokens` — and a provider
        // counts nothing past its own window (it refuses the request instead), so a
        // stored "measured" over the window is the old guard's pair in LEDGER tokens.
        // MEASURED 2026-10-11 on pg-noop: a restart restored "a fold of 3213409 measured
        // 1687673" against a 1000000 window — ledger numbers — and turned compaction off
        // until the first round measured 584730 and released it. The operator saw the
        // false OFF. Such a row is cleared and not restored; the next measured round
        // decides, which is the controller's rule anyway.
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
/// not tidiness here — a provider cannot be pointed at a canned server (a
/// `ProviderConfig` names a vendor and nothing else), so a gate left inline in
/// `Harness::open` could only ever be exercised by a test that talks to
/// DeepSeek. The bug this replaces was found in production precisely because
/// nothing checked the pairing.
///
/// **What it returns is an ANCHOR, not a ratio** — `(ledger, provider)`: the
/// provider counted `provider` tokens for the prompt whose ledger was `ledger`
/// long. [`roll_resident`] adds what was appended after `ledger` to it, sized the
/// way the provider will be sent it, and that sum is the resident count.
///
/// # Why equality was the test, and why it is not any more
///
/// This used to give the pair back only when the stored ledger EQUALLED the
/// rebuilt one, because the pair was used as a RATIO: `ledger_now ÷ ratio`. A
/// count from a smaller conversation divided into a bigger ledger overstates the
/// ratio, and the session compacts too late — MEASURED 2026-10-02: a store holding
/// 940,211, measured on a ~950k-token conversation, was paired with a rebuilt
/// ledger of 1,486,369; the window came out 1,580,888 for a session already 1.46M
/// deep, and it died at the provider's 400 on every attempt.
///
/// The equality closed that, and opened the opposite hole. Anything appended after
/// the last round's `persist_context` — a notice, a nag, the tool result of a turn
/// that did not end in a round — broke it, the pair was refused, the window went
/// UNSCALED, and the raw ledger was compared against the provider's window.
/// MEASURED 2026-10-11 (R72), leticl's `s-1791151788256943467` on deepseek-flash,
/// the first idle pass after a restart:
///
/// ```text
/// compacting: 3454266 of 1000000 tokens resident, less than the 62500 the next turn needs
/// compacted: 75408 tokens resident now, was 3454266.
/// ```
///
/// — a session the provider counted at ~740k, folded to 75k. The comment at the
/// call chose that direction on purpose (*"it compacts early, which is the
/// direction nobody loses a session to"*), and early by 4.6× is losing the
/// session's working context on every restart.
///
/// An anchor has neither failure. The 2026-10-02 conversation comes back as
/// 940,211 PLUS the ~536k ledger tokens appended since, sized as sent — larger, so
/// it compacts; the 2026-10-11 one comes back as ~740k plus one notice — and does
/// not. Nothing is divided, so a stale count cannot shrink a big ledger, and a
/// later append cannot throw the count away.
///
/// It refuses in three cases, and each lands on the unscaled window:
///
///   * no stored pair — a row written before v12, or one whose turn never
///     finished. Unverifiable is not the same as wrong, but the two are the same
///     as unmeasured;
///   * a stored ledger LONGER than the one rebuilt — the conversation it measured
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

/// **Whether the round about to be sent stops at the wall** — `resident` is the ledger
/// count of the prompt it would send (the tool results of the round before already in
/// it), `window` the planning window in the same units.
///
/// Round 0 stops only past the window itself; every later round stops inside the
/// reserve. See the call in `run_rounds` for why the two differ.
///
/// **And round 0 stops only where a compaction could make room** — where the stable
/// prefix (`prefix`, the system prompt and the tool schemas, which no compaction
/// rewrites) leaves the reserve free. A prompt whose message zero alone is past the
/// window is a session no summary can rescue: the compaction door already refuses it
/// in its own words (*"this session's PROMPT is N token(s) of a W token window"*), and
/// stopping the round as well would only trade the server's verdict for a compaction
/// that is known to fail. The between-rounds check is unchanged.
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

/// **Whether the wall stops THIS round** — [`wall_stops_round`]'s arithmetic plus the one
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
/// — 2,708,271 LEDGER tokens against a 1,000,000-token window, where the ledger of a
/// metered session is `Vocab::bytes` (one id per byte of the rendered history). It is not a
/// count of tokens at all, and comparing it with a token window says nothing about whether
/// the prompt fits. The provider is the only one that can answer that question, and it
/// answers it with a refusal that the compaction door already handles on evidence
/// (`WallWhy::Refused`). So at round 0 an unmeasured count does not stop the turn.
///
/// **Carried is not unmeasured.** A ratio measured on the model before the switch converts
/// bytes into that model's tokens — the same quantity, off by however differently the two
/// tokenizers count — so the comparison is meaningful and the wall may fire on it, with
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

/// **Where the smaller-tail fold splits** — the ordinary fold's split moved later until
/// the verbatim part is at most half what it was, or the ordinary split unchanged when
/// the longer first part would not fit one summary call (the next rung, the sliced fold,
/// is the answer to that).
///
/// The ordinary fold (`plan_fold`) summarises the first half of the history BY TOKENS and
/// carries the second half verbatim: on pg-noop's 941,101 that was the ~500k the provider
/// then measured. Halving the verbatim part is the step between that and carrying nothing.
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

/// **The closing line of an automatic compaction**, in one unit, said once: *"compacted:
/// 937686 → 75408 deepseek tokens resident"*. Shared by both doors (`Harness` and
/// `Sessions`), so the two cannot word the same fact two ways.
///
/// The after-count of a metered session is an ESTIMATE until a round on the new base
/// measures it — the provider has not seen the base yet — and the line says so rather
/// than printing an estimate beside a measurement as if they were the same kind of
/// number.
///
/// **And it is printed as one** — `≈ N`, *estimated until the next round measures it*.
/// MEASURED 2026-10-11 on pg-noop: an estimate of 1,039,055 for a base deepseek then
/// counted at 507,355, printed beside the measured 941,101 as if they were the same kind
/// of number, and judged as one.
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

/// **Where the provider last counted this conversation** — the ledger length of the
/// prompt it was sent, and its own count of that prompt. R72.
///
/// The wall used to be judged as `ledger.len() ÷ ratio`, with the ratio
/// `(ledger, prompt_tokens)` learned at the end of the last round. That ratio is
/// not a property of two tokenizers — the ledger holds reasoning the provider does
/// not count (see `Config::ledger_scale`'s table) — so it is a property of how much
/// of the history was reasoning AT ONE MOMENT, and it was taken with the two halves
/// at two different moments besides: `prompt_tokens` is the prompt BEFORE the
/// round's reply, the ledger length was read AFTER it. Every row appended after it
/// was converted by that ratio — a tool result the provider is sent whole was
/// counted at a third of its size, and a reasoning block it is not sent at all was
/// counted at a third more than nothing. The operator's report: *"leticl head shows
/// me 740k tokens yet it just started compaction citing 900k+ tokens"*.
///
/// So the wall counts the provider's own number and adds to it only what came
/// after, sized the way it will be sent. See [`roll_resident`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WallAnchor {
    /// The transcript the count was taken on. A compaction forks a new one, and a
    /// count of the old conversation says nothing about the new one's size.
    pub(crate) transcript: String,
    /// The ledger length of the prompt the count belongs to — a row boundary.
    pub(crate) ledger: u64,
    /// The count. The provider's own when `measured`; otherwise the estimate that
    /// stands in for one after a fork, until a round on the new base measures.
    pub(crate) provider: u64,
    /// Whether `provider` came from the provider.
    pub(crate) measured: bool,
    /// **The last measurement as a ratio of what was SENT**: `(sent-sized ledger tokens,
    /// the provider's count)` of one prompt, reasoning and marks left out of the first.
    ///
    /// A metered session's ledger is `Vocab::bytes` — one token per BYTE — so the sent
    /// rows are not "the provider's size to within 4-17%", they are two to four times
    /// it. Measured on pg-noop, 2026-10-11: a `/reseat` carried a transcript whose sent
    /// rows were 1,276,902 bytes, deepseek counted it at 598k, and the estimate fired an
    /// ordinary fold at "1276902 of 1000000". Every sent-sized delta — the rows after a
    /// measurement, and a fork's whole new base — goes through this ratio. Carried across
    /// a fork, because the bytes-per-token of what is sent belongs to the content, not
    /// to the transcript id. `None` until something measured, or on a local session,
    /// whose ledger IS the prompt.
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

/// The sent-sized ledger tokens between two row boundaries, `[from, to)` — see
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
/// — `None` on a local session, where the ledger is the prompt.
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

/// **One reading of how big this conversation is, in the units its wall is in** —
/// the provider's on a metered session, the ledger's on a local one (where the
/// two are the same count by construction: the ledger IS the prompt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resident {
    /// The whole count. What the wall is judged on, and what a line prints.
    pub tokens: u64,
    /// The provider's own count at its last round, when `tokens` is built on one —
    /// the number the head shows. `None` when nothing measured this transcript.
    pub measured: Option<u64>,
    /// What was appended after that count and will be sent, sized as sent.
    /// `tokens == measured + since` whenever `measured` is `Some`.
    pub since: u64,
}

impl Resident {
    /// **The count's provenance, said once, in the line that prints the count** —
    /// so the head's number and the wall's are visibly the same quantity: *"(740000
    /// at the last round + 3000 appended since)"*. Empty when there is no
    /// measurement to relate to.
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

/// **What the ledger holds after `from` that the next request will carry**, in
/// ledger tokens — the provider-sized estimate of an append.
///
/// Ledger tokens are the session's own vocabulary, not the provider's; WITHOUT the
/// reasoning the two agree to 4-17% (`Config::ledger_scale`'s table), which is
/// ordinary variance between two tokenizers and stable, unlike the reasoning share.
/// So the estimate keeps every row the request carries and drops the ones it does
/// not:
///
///   * `Reasoning` on a metered session — what moves the ledger and not the
///     provider's count; the table above measures it at 27-34% of a history, and
///     the session R72 is about at more than three quarters;
///   * `SegmentMark` on a metered session — the store's, never sent
///     (`letibot_provider::messages`).
///
/// On a LOCAL session the request is the ledger itself (`engine.rs`: `let mut
/// prompt = session.ledger.tokens().to_vec()`), so everything after `from` is
/// carried, reasoning included, and this is the ledger's own difference.
///
/// Rows and items are one-to-one (`Session::append_items`), so a row is classified
/// by the item at its index. A row that STARTS before `from` is not counted: `from`
/// is a row boundary wherever this harness wrote it.
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

/// **A round's reasoning, as one text** — every `Reasoning` item the round produced, in order, for
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

/// **The resident count, rolled forward from `anchor` to the ledger as it stands.**
///
/// Two cases, and the second is the one a fork reaches:
///
///   * the anchor is on this transcript and not past its end — the provider's
///     count plus [`sent_since`] the anchor. The count the head shows is `measured`,
///     and the rest is said as `since`, so a line can name both;
///   * it is not — a compaction forked a new transcript, or the ledger is shorter
///     than the anchor. The count described a conversation this one is not, so the
///     estimate starts over from the new base: its prefix plus everything sent,
///     unmeasured, and the returned anchor is re-based there so the next roll adds
///     only what comes after. The first round on the new base replaces it with a
///     measurement. Dividing the base by the OLD ratio was what printed "compacted:
///     726656 tokens resident now" for a summary and a verbatim tail whose reasoning
///     share was nothing like the history it replaced.
///
/// Returns the anchor to keep and the reading.
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

/// **`0` is unbounded**, which is the default; see [`Config::max_tool_rounds`].
///
/// `usize::MAX` rather than a second loop shape, so there is one body and one
/// place a round is counted — two loops that had to stay in step is how the
/// `round + 1` in the reporting would come to mean two different things.
///
/// A named function rather than a `match` inline, because this is the whole of
/// "infinity" and it is one line that a test can hold to.
pub(crate) fn round_backstop(configured: usize) -> usize {
    match configured {
        0 => usize::MAX,
        n => n,
    }
}
