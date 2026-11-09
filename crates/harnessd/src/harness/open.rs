//! Opening a session: `Harness::open_with_registry` and the pieces it assembles (provider,
//! vocabulary, roles, surroundings), plus the import of an existing conversation from opencode or
//! Claude Code.
use super::*;

/// Whether a verbatim reseat's carry can be sent: see [`Harness::reingest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerbatimFit {
    /// It fits with a turn's room beside it.
    Fits,
    /// It fits, short of a turn's room: as close to the window as it was before the reseat,
    /// and the compaction before the next turn makes the room.
    Tight,
    /// It is more than the window holds: nothing could be sent, so the reseat is refused.
    Over,
}

/// The rule: refused only over the window. A schema change moves a conversation by a few thousand
/// tokens, and refusing it for the headroom a turn needs kept a session on its stale prompt at
/// exactly the size it was already running at.
pub fn verbatim_fit(carried: u64, window: u64, headroom: u64) -> VerbatimFit {
    if carried >= window {
        VerbatimFit::Over
    } else if carried + headroom >= window {
        VerbatimFit::Tight
    } else {
        VerbatimFit::Fits
    }
}

/// What a VM-placed child copies, or why it cannot be placed: `Ok(None)` is the session's own copy source
/// (`Config::vm_copy_source`). A `task_start` child copies its own worktree; a plain VM child of a session holding
/// several repositories is refused, because a VM holds one.
pub fn vm_copy_for(
    in_vm: bool,
    has_worktree: bool,
    named: Option<&std::path::Path>,
    workspace: &std::path::Path,
) -> Result<Option<std::path::PathBuf>, String> {
    if !in_vm {
        return Ok(named.map(std::path::Path::to_path_buf));
    }
    if let Some(n) = named {
        let full = if n.is_absolute() {
            n.to_path_buf()
        } else {
            workspace.join(n)
        };
        return match crate::mergequeue::git::repo_root(&full) {
            Ok(top) => Ok(Some(top)),
            Err(_) => Err(format!(
                "`{}` is not a git repository, so there is no one tree for the VM to copy. Name \
                 one of the session's repositories with `repo`.",
                n.display()
            )),
        };
    }
    if has_worktree || crate::mergequeue::git::repo_of_workspace(workspace).is_some() {
        return Ok(None);
    }
    let many = crate::mergequeue::git::repos_of_workspace(workspace);
    if many.len() > 1 {
        let names: Vec<String> = many
            .iter()
            .filter_map(|r| r.file_name().map(|n| format!("`{}`", n.to_string_lossy())))
            .collect();
        return Err(format!(
            "a VM holds one repository, and this session is one project of {} — {}. Name the one \
             it copies with `repo`; nothing was spawned.",
            many.len(),
            names.join(", ")
        ));
    }
    Ok(None)
}

/// The mode a placement puts a session in. A VM is the boundary, so a session inside one runs
/// at allow-all: `Firecode` boots one here, `RemoteVm` boots it on the declared host, and a session
/// attached to an ancestor's VM is inside that VM too.
pub fn mode_in_vm(
    asked: letibot_tools::mode::Mode,
    placement: &letibot_tools::builtins::task::Placement,
    attached: bool,
) -> letibot_tools::mode::Mode {
    if placement.is_vm() || attached {
        letibot_tools::mode::Mode::ALLOW_ALL
    } else {
        asked
    }
}

/// A renderer over this box's vocabulary, with no session behind it: for a caller that needs to turn
/// a prompt into tokens and nothing else. Built the way `open` builds its engine, so what it renders
/// is what a session would.
pub fn engine_for(parts: &Parts, cfg: &Config) -> Result<TurnEngine, HarnessError> {
    TurnEngine::new(
        parts.vocab.clone(),
        parts.wiring.renderer.clone(),
        parts.wiring.parser.clone(),
        cfg.endpoint.clone(),
        letibot_backend::BackendCaps::OWN_SERVER,
        cfg.model.clone(),
        cfg.sampling.clone(),
    )
    .map_err(|e| HarnessError::Setup(format!("the dialect does not fit this vocabulary: {e}")))
    .map(|mut e| {
        // The endpoint's media marker, from `/props`: `None` for a metered provider or a server
        // without `mtmd`, and then a prompt carrying an image is sent without it. The same seven
        // arguments as `Harness::open`'s own construction, two lines apart in this file.
        e.media_marker = cfg.media_marker.clone();
        e
    })
}

/// The tool names in a rendered schema list: both shapes one is rendered in, and a schema whose name
/// cannot be read is left out rather than guessed at. A description's first sentence, for a
/// one-line-per-tool listing: the full text is the model's, this is the operator's reminder.
pub(crate) fn first_sentence(d: &str) -> String {
    let one = d.split_once(". ").map(|(a, _)| a).unwrap_or(d);
    if one.chars().count() > 76 {
        format!("{}…", one.chars().take(75).collect::<String>())
    } else {
        one.to_string()
    }
}

/// The tail as the wire carries it: role and text, in order. Only the two item kinds that are words
/// somebody said, because the wire's `CompactionTurn` is `{role, text}` and a tool result is not a
/// turn; the payload stays in the transcript and `tail_items` counts it.
pub(crate) fn wire_turns(
    items: &[TranscriptItem],
) -> Vec<letibot_sessionlog::event::CompactionTurn> {
    use letibot_sessionlog::event::CompactionTurn;
    items
        .iter()
        .filter_map(|it| match it {
            TranscriptItem::User { parts, .. } => {
                let text = parts
                    .iter()
                    .filter_map(|p| match p {
                        UserPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (!text.is_empty()).then(|| CompactionTurn {
                    role: "operator".into(),
                    text,
                })
            }
            // Reasoning is deliberately NOT a turn: it is the model thinking, not its answer. A tail
            // is the literal recent past, and the reasoning block is the one part nothing reads.
            TranscriptItem::Assistant { text, .. } if !text.is_empty() => Some(CompactionTurn {
                role: "agent".into(),
                text: text.clone(),
            }),
            _ => None,
        })
        .collect()
}

pub(crate) fn tool_names(tools_json: &[String]) -> std::collections::BTreeSet<String> {
    tool_schemas(tools_json).into_keys().collect()
}

/// The tool schemas by name, verbatim: the same read as [`tool_names`], keeping the schema text. The
/// extra half is what lets a schema that GREW be named, since a tool whose parameters gained a field
/// is in neither the added set nor the gone set.
pub(crate) fn tool_schemas(tools_json: &[String]) -> std::collections::BTreeMap<String, String> {
    tools_json
        .iter()
        .filter_map(|j| {
            let v: serde_json::Value = serde_json::from_str(j).ok()?;
            let n = v
                .pointer("/function/name")
                .or_else(|| v.get("name"))?
                .as_str()?;
            Some((n.to_string(), j.clone()))
        })
        .collect()
}

/// The instructions a prompt carries, as a stale check compares them: without the standing notes,
/// which are re-read at every fork, and with the scratch directory's pid masked, or every restart
/// would read as a changed prompt for every session on the box.
pub fn instructions_of(system: &str) -> String {
    let text = crate::standing_notes::replace(system, None);
    const AT: &str = "/scratch-";
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(i) = rest.find(AT) {
        let (head, tail) = rest.split_at(i + AT.len());
        out.push_str(head);
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            out.push('N');
        }
        rest = &tail[digits..];
    }
    out.push_str(rest);
    out
}

/// The fingerprint of a session's FROZEN prefix: the instructions, the tool schemas and the seat,
/// and nothing else. The standing notes are stripped first, because they are re-read on every base
/// rebuild and would report every session stale. A hash, because the same text must compare equal.
pub fn prefix_fingerprint(seat: Seat, prefix: &StablePrefix) -> String {
    let mut h = Sha256::new();
    h.update(seat.as_str().as_bytes());
    h.update([0]);
    h.update(instructions_of(&prefix.system).as_bytes());
    h.update([0]);
    for tool in &prefix.tools_json {
        h.update(tool.as_bytes());
        h.update([0]);
    }
    hex32(&h.finalize().into())
}

/// The sentence a head is told when a resumed session speaks a prompt this daemon would not compose
/// now: one function, so the TUI's one-line card (`stale_card`) can be pinned against it.
pub fn stale_prefix_sentence(
    session_id: &str,
    seated: &str,
    moved: &[&str],
    added: &[String],
    gone: &[String],
    grew: &[String],
) -> String {
    // Neither moved and the hash still differs: the seat's own name is the third input, and it is in
    // the address precisely so that this case has a sentence rather than an empty list.
    let (what, verb) = match moved {
        [] => ("the seat".to_string(), "has"),
        [one] => ((*one).to_string(), "have"),
        [a, b] => (format!("{a} and {b}"), "have"),
        many => (many.join(", "), "have"),
    };
    let mut say = format!(
        "session {} was seated {} with a prompt this daemon would not \
         compose now: {what} {verb} changed since. This session keeps the \
         prefix it was created with — message 0 is frozen, and rewriting it \
         is a full cold re-prefill — so the model cannot call a tool seated \
         since, and a schema that grew is one it has never been told about",
        session_id, seated,
    );
    if !added.is_empty() {
        say.push_str(&format!(
            ". Seated here but not in this conversation's prompt, so the \
             model cannot call them: {}",
            added.join(", ")
        ));
    }
    if !gone.is_empty() {
        say.push_str(&format!(
            ". In the prompt but not seated here, so a call to them refuses: {}",
            gone.join(", ")
        ));
    }
    if !grew.is_empty() {
        say.push_str(&format!(
            ". Seated on both sides with a different schema, so the model is \
             working from the older one: {}",
            grew.join(", ")
        ));
    }
    say.push_str(
        ". `/reseat` rebuilds the prompt from what is seated now, carrying \
         this conversation across as it is; a new session gets the seated one \
         from the start.",
    );
    say
}

/// What the MODEL is told about a stale prompt: what it can and cannot call, and
/// whose the decision is. The head's sentence names the session and the clock it was seated at,
/// and a model can act on neither, so this is its own sentence over the same lists.
pub fn stale_prompt_for_the_model(added: &[String], gone: &[String], grew: &[String]) -> String {
    let mut said: Vec<String> = Vec::new();
    if !added.is_empty() {
        said.push(format!(
            "{} exist but you cannot call them",
            added.join(", ")
        ));
    }
    if !gone.is_empty() {
        said.push(format!("{} will refuse", gone.join(", ")));
    }
    if !grew.is_empty() {
        said.push(format!("{} changed", grew.join(", ")));
    }
    if said.is_empty() {
        said.push("the instructions changed".into());
    }
    format!(
        "[stale prompt] Your tool list is out of date: {}. Updating it is the person's call: \
         don't ask. Mention a missing tool only when you need it.",
        said.join("; ")
    )
}

/// When a session was seated, in the launcher's own spelling: `10-08 21:14`. An unreadable clock is
/// SAID rather than rendered as the epoch, because `0` is *nobody recorded when*.
fn seated_at(ms: i64) -> String {
    if ms <= 0 {
        return "an unrecorded time".into();
    }
    let secs = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`; both are owned here. The
    // `_r` form is the one that does not hand back a shared static, which matters because
    // a harness is opened from more than one thread.
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if !ok {
        return "an unrecorded time".into();
    }
    format!(
        "{:02}-{:02} {:02}:{:02}",
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

/// How long a turn waits for a busy store before its persist gives up. Every contention measured
/// on this box cleared inside seven minutes, so a store still locked after this is wedged rather
/// than busy, and saying so is the useful act.
pub(crate) const STORE_BUSY_PATIENCE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Whether the store already holds this transcript row. A `SELECT` through the escape hatch, because
/// the append-only guarantees are triggers, so a read here cannot weaken them.
fn transcript_exists(store: &Store, transcript_id: &str) -> bool {
    store
        .connection()
        .query_row(
            "SELECT 1 FROM transcript WHERE id = ?1",
            [transcript_id],
            |r| r.get::<_, i64>(0),
        )
        .is_ok()
}

/// The most recent daemon stop in the store, whatever session's rows named it: the down time is a
/// fact about the BOX, so it looks at every `daemon_life` row with a stop and answers the latest.
/// Intermediate restarts are counted, and the gap is between the LAST stop and THIS start.
pub(crate) fn latest_box_stop(store: &Store) -> crate::restart::Stopped {
    let latest = store
        .lives()
        .ok()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|l| l.stopped_ms.map(|at_ms| (at_ms, l.instance)))
        .max_by_key(|(at_ms, _)| *at_ms);
    match latest {
        // The instance is only `max_by_key`'s tie-breaker: `Stopped::At` says when the BOX stopped,
        // and which session's row told us is not part of that fact.
        Some((at_ms, _)) => crate::restart::Stopped::At {
            at_ms,
            estimated_from: None,
        },
        None => crate::restart::Stopped::Unknown { seen_ms: 0 },
    }
}

/// What an open learns from the store before it builds anything: the connection, the corpus beside
/// it, this session's own row and the claim on its transcript.
struct Opening {
    store: Option<Store>,
    corpus_sink: Option<std::sync::Arc<dyn letibot_tools::CorpusSink>>,
    corpus_counts: Option<letibot_tokencore::store::CorpusCounts>,
    stored: Option<letibot_tokencore::store::StoredSession>,
    writer: Option<crate::writer::Claim>,
}

/// Where a session's tools run and what bounds them, with the home, promote channel and scratch
/// directory every backend is handed before it is boxed.
struct Placed {
    backend: Box<dyn letibot_tools::backend::ExecBackend>,
    described: String,
    writable: bool,
    monitors: Option<Arc<Monitors>>,
    surroundings: letibot_tools::Surroundings,
    confined: bool,
    /// The tools run over the whole host: nothing was asked for that this platform can build,
    /// or a boundary was asked for and failed. Not the same as `!confined`, which a file-tool-only
    /// backend also is.
    whole_host: bool,
}

impl Harness {
    /// Open a session: resolve the dialect against the vocabulary, seat the tools, render the
    /// stable prefix and record it. Every failure raised here is one that would otherwise be silent
    /// at runtime, which is why none of them waits for the first turn.
    pub fn open(parts: &Parts, cfg: Config, hub: Arc<Hub>) -> Result<Self, HarnessError> {
        Self::open_with(parts, cfg, hub, None, None)
    }

    /// As [`Harness::open`], with an adjudicator and an extra tool. Uses a fresh empty session
    /// registry, so a caller reaching this without one gets a `task` tool that cannot spawn: the
    /// honest state for a session nobody registered.
    pub fn open_with(
        parts: &Parts,
        cfg: Config,
        hub: Arc<Hub>,
        adjudicator: Option<Box<dyn Adjudicator>>,
        extra_tool: Option<Box<dyn Tool>>,
    ) -> Result<Self, HarnessError> {
        Self::open_with_registry(
            parts,
            cfg,
            hub,
            adjudicator,
            extra_tool,
            letibot_sessionlog::registry::Registry::new(),
        )
    }

    /// [`Harness::open_with`] plus the daemon's session registry, so `task` spawns a real session: an attachable hub and a resumable store row. Tests pass a fresh one.
    /// Opens the store first (it may change the config, hence `&mut`): connection, corpus, the session's row and transcript claim, and the workspace, title and seat that row decides.
    fn open_the_stored(cfg: &mut Config, notes: &mut Vec<String>) -> Result<Opening, HarnessError> {
        // A `[embeddings]` section the box WROTE and did not get is said here, once per session:
        // silently scoring the words alone would leave nobody able to fix the key.
        if let Some(why) = cfg.embedder.fault() {
            notes.push(format!(
                "the `[embeddings]` section did not produce an embedder, so every score this \
                 session makes is the words alone: {why}"
            ));
        }
        // The store opens before anything else, because it may change the configuration: a session
        // already in it carries its own workspace root, and its tools must be confined to THAT tree
        // rather than to the directory this daemon happened to start in.
        let store = match &cfg.store {
            None => None,
            Some(path) => Some(
                Store::open(path)
                    .map_err(|e| HarnessError::Store(format!("opening {path:?}: {e}")))?,
            ),
        };
        // A handle onto the connection `store` holds, not a second one, since the store became one
        // connection per file per process. A corpus that will not open is reported and not fatal: a
        // harness that will not start because its telemetry will not is a worse harness.
        let opened_corpus = cfg
            .store
            .as_ref()
            .and_then(|path| crate::corpus::StoreCorpus::open(path).ok())
            .map(std::sync::Arc::new);
        // Job handles must be above every one the store has seen, or the first job after a restart
        // is an old `j7` to every reader. Seeded on every open, because a session can arrive from a
        // daemon that minted higher handles.
        if let Some(s) = &store
            && let Ok(n) = s.max_job_number()
        {
            letibot_tools::exec::JobId::seed_above(n);
        }
        // As the table holds it, including every earlier run's rows.
        let corpus_counts = opened_corpus.as_ref().map(|c| c.counts().0);
        let corpus_sink: Option<std::sync::Arc<dyn letibot_tools::CorpusSink>> =
            opened_corpus.map(|c| c as std::sync::Arc<dyn letibot_tools::CorpusSink>);
        let stored = match &store {
            None => None,
            Some(s) => s
                .session(&cfg.session_id)
                .map_err(|e| HarnessError::Store(e.to_string()))?,
        };
        // One process appends to one transcript, and this is where that is decided: the claim is a
        // row naming this process, taken before a single row is written. A session another live
        // process holds is refused by name; one whose live copy is elsewhere is never claimed.
        let moved = stored
            .as_ref()
            .and_then(|s| s.lives_on.clone())
            .filter(|s| !s.is_empty());
        let writer = match (&store, moved.is_some()) {
            (Some(s), false) => {
                Some(crate::writer::claim(s, &cfg.session_id).map_err(HarnessError::Setup)?)
            }
            _ => None,
        };
        if let Some(st) = &stored {
            // A copy that lives somewhere else is not opened: `--migrate` marks the copy it leaves,
            // and a turn in a marked copy writes into a conversation nobody else will see. A
            // refusal rather than a note, because the note lands in the transcript the model answers in.
            if let Some(on) = st.lives_on.as_ref().filter(|s| !s.is_empty()) {
                return Err(HarnessError::Setup(format!(
                    "session {} lives on `{on}`, not here — it was moved there and the copy in \
                 this store is not being appended to. Nothing was opened. `letibot --session \
                 {}` attaches to where it is (re-establishing the forwarded socket if it \
                 dropped), and `letibot --migrate {} --to local` brings it back here.",
                    cfg.session_id, cfg.session_id, cfg.session_id
                )));
            }
            if !st.workspace_root.is_empty()
                && std::path::Path::new(&st.workspace_root) != cfg.workspace
            {
                notes.push(format!(
                    "the workspace is {} — this session's own, from the store — not {}, \
                 which is where this daemon was started. A resumed session's tools \
                 are confined to the tree the conversation is about.",
                    st.workspace_root,
                    cfg.workspace.display()
                ));
                cfg.workspace = PathBuf::from(&st.workspace_root);
            }
            if let Some(t) = st.title.as_ref().filter(|t| !t.is_empty()) {
                cfg.title = t.clone();
            }
            // The seat comes from the session too, and it must land HERE, because `cfg.seat`
            // decides which backend is built below. Unparseable is an error and not a fallback to
            // the daemon's role: either way the tools and the backend would disagree.
            if let Some(name) = st.role.as_ref().filter(|r| !r.is_empty()) {
                let seat = Seat::parse(name).map_err(|e| {
                HarnessError::Setup(format!(
                    "session {} records the role `{name}`, which this build does not know: {e}. Refusing rather than re-seating the conversation with the daemon's own role, which would change its tools without saying so.",
                    cfg.session_id
                ))
            })?;
                if seat != cfg.seat {
                    notes.push(format!(
                        "the role is `{}` — this session's own, from the store — not \
                     `{}`, which is what this daemon was started with. The tools \
                     AND the backend follow the session's role, or they would \
                     disagree about what it may write.",
                        seat.as_str(),
                        cfg.seat.as_str()
                    ));
                    cfg.seat = seat;
                }
            }
        }

        // An opencode session is seated in ITS directory: the conversation happened there, and a
        // session at the wrong root is the `--session` defect. Read before the backend is built,
        // because the root is baked into it.
        if stored.is_none()
            && let Some(oc_id) = cfg.session_id.strip_prefix("oc-").map(str::to_string)
            && let Some(db) = letibot_opencode::default_db_path()
            && let Ok(src) = letibot_opencode::Source::open(&db)
            && let Ok(dir) = src.directory(&oc_id)
            && !dir.is_empty()
        {
            cfg.workspace = PathBuf::from(dir);
        }

        // And the same for a Claude Code transcript, except that a root which is NOT there is not a
        // seat: its `cwd` is a memory that may since have been deleted, and a session seated at one
        // fails on its first tool call instead of at startup.
        if stored.is_none()
            && let Some(cc_id) = cfg.session_id.strip_prefix("cc-").map(str::to_string)
            && let Some(dir) = letibot_claude::default_projects_dir()
            && let Ok(src) = letibot_claude::Source::open(&dir)
            && let Ok(cwd) = src.directory(&cc_id)
            && !cwd.is_empty()
            && std::path::Path::new(&cwd).is_dir()
        {
            cfg.workspace = PathBuf::from(cwd);
        }
        Ok(Opening {
            store,
            corpus_sink,
            corpus_counts,
            stored,
            writer,
        })
    }

    /// This session's point and the two facts that ride the config: the per-project point (longest ancestor wins, recorded consent disclosed), whether the session is unconfined, and whose child it is.
    /// Answers where the point came from.
    fn resolve_the_point(
        parts: &Parts,
        cfg: &mut Config,
        store: &Option<Store>,
        stored: Option<&letibot_tokencore::store::StoredSession>,
        session_registry: &Arc<letibot_sessionlog::registry::Registry>,
        notes: &mut Vec<String>,
    ) -> String {
        // The mode is a session property, resolved after the workspace is final: the per-project
        // point is looked up now (longest ancestor wins), so a project with no row keeps the
        // daemon's default and one with a row takes its own.
        let mut mode_source = String::from("daemon flag / default");
        {
            // Read fresh: another daemon may have written a row since this one started.
            let mut store = parts.mode_store.write().unwrap();
            store.refresh();
            let store = &*store;
            // The session's own point first - the one it was put at, which a restart or
            // a reseat must bring back whatever the project row says now. The operator:
            // *"and again peerdb allow-all lost after restart/reseat"*.
            let own = cfg
                .parent_session_id
                .is_none()
                .then(|| store.session(&cfg.session_id))
                .flatten();
            // A subagent keeps its parent's point and never re-reads the row: it must not differ
            // from its parent in either direction, and a row naming a point the child cannot carry
            // killed it before its first turn. The row is for a session somebody opened there.
            if let Some((own, when)) = own {
                cfg.mode = own;
                mode_source = "this session's own (set with /mode)".into();
                if own.name == letibot_tools::mode::Mode::ALLOW_ALL.name
                    && let Some(when) = when
                {
                    cfg.mode = letibot_tools::mode::Mode::ALLOW_ALL_HERE;
                    mode_source = format!("this session's own, at your word of {when}");
                    notes.push(format!(
                        "`allow-all` for this session — on this box, with nothing confining it, \
                     because you said so on {when}. `/mode <other>` takes it back."
                    ));
                } else {
                    notes.push(format!(
                        "mode is `{}` for this session (set with /mode)",
                        cfg.mode.name
                    ));
                }
            } else if cfg.parent_session_id.is_some() {
                // The point itself is the parent's, read live by the spawn and carried in
                // `cfg.mode`; whether this seat can carry it is decided below, where the
                // seat's own supplies exist. Here is where the row says where it came from.
                mode_source = child_mode_source(None);
            } else if store.is_set(&cfg.workspace) {
                let before = cfg.mode.name;
                cfg.mode = store.for_project(&cfg.workspace);
                mode_source = "project store".into();
                // And the recorded consent, when the row names the point that needs one: the
                // operator's answer is in the store so a restart stops throwing it away, and it is
                // DISCLOSED here, because a point that came back silently is the failure.
                if cfg.mode.name == letibot_tools::mode::Mode::ALLOW_ALL.name
                    && let Some(when) = store.consent(&cfg.workspace)
                {
                    cfg.mode = letibot_tools::mode::Mode::ALLOW_ALL_HERE;
                    mode_source = format!("project store, at your word of {when}");
                    notes.push(format!(
                        "`allow-all` for {} — on this box, with nothing confining it, because you \
                     said so on {when}. Nothing is asked here and the always-ask list is \
                     admitted; `/mode <other>` takes it back and forgets the answer.",
                        cfg.workspace.display()
                    ));
                } else if cfg.mode.name != before {
                    notes.push(format!(
                    "mode is `{}` for {} (from the project store), not the daemon default `{before}`",
                    cfg.mode.name, cfg.workspace.display()
                ));
                }
            }
        }

        // leticode's opencode parity, and a subagent's inheritance of it, is a fact of the SESSION and
        // not of the seat: a subagent re-seats to `coder`, so the seat alone would re-confine it. It
        // is also read back from the store, because the row a spawn leaves says only the SEAT.
        cfg.unconfined = cfg.unconfined
            || cfg.seat == Seat::Leticode
            || match (&store, &stored) {
                (Some(store), Some(st)) => unconfined_by_inheritance(store, st),
                _ => false,
            };

        // A session a session started has that session as its parent: every open passes through
        // here, and only here are the caller's word, the registry's entry and the row in one hand.
        // After the mode block, which asks whether this session inherits a LIVE parent's point.
        cfg.parent_session_id = session_registry.parent_for(
            &cfg.session_id,
            cfg.parent_session_id.clone(),
            stored.as_ref().and_then(|s| s.parent_session_id.as_deref()),
        );
        mode_source
    }

    /// A session whose tools run over the whole host says so, on the hub the head draws and in
    /// the daemon's log. It is the session with no command in it: one that seats a shell gets a
    /// boundary, and a boundary that was asked for and could not be built says that instead.
    fn say_no_boundary(hub: &Arc<Hub>) {
        let said = "this session runs UNCONFINED: it seats no shell, so there is no command for a \
                    boundary to go in front of — a `leticode` seat asks for none, and the wide seat \
                    wraps commands: the Seatbelt one on a Mac, bubblewrap on Linux. Its FILE TOOLS \
                    are held to the session's perimeter instead, at the level its \
                    `[perimeter.<name>]` table names: a credential store, letibot's own store and \
                    every `[boundary]` or `[perimeter]` hide are refused to them, and what else \
                    they reach is the level's. What a file tool reads it can copy into the \
                    transcript. `docs/boundary-and-adjudication.md` §4k has the long form."
            .to_string();
        eprintln!("letibot: {said}");
        hub.warn(UNCONFINED_SESSION, said);
    }

    /// A wide seat that could not be built is a REFUSAL, not a fallback, on every platform: the
    /// alternative is the whole host with the operator's keys readable. Says it, then hands back
    /// the sentence.
    fn say_wide_seat_refused(hub: &Arc<Hub>, e: &dyn std::fmt::Display) -> String {
        let said = format!(
            "this session asks for the wide seat and no boundary could be built for it: {e}\n\n\
             It is refused rather than opened UNCONFINED, which would give a command this user's \
             rights over the whole host with the operator's keys readable. The wide seat is \
             bubblewrap around this machine's own filesystem with the credential stores masked, \
             and it needs {BOUNDARY_NEEDS} A session that seats no shell needs no boundary at \
             all."
        );
        eprintln!("letibot: {said}");
        hub.warn(BOUNDARY_ABSENT, said.clone());
        said
    }

    /// The seat a session's FILE TOOLS are held to where no substrate measured a view for its
    /// commands: a `leticode` seat asks for no boundary and seats no shell, so before R121 its file
    /// tools had the whole host and no table could reach them. The perimeter is the view instead.
    fn fence_seat(cfg: &Config) -> Result<letibot_tools::exec::seat::SeatView, HarnessError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let p = cfg.perimeter.as_deref().ok_or_else(|| {
            HarnessError::Setup(
                "this session has no perimeter, so there is nothing to hold its file tools to; \
                 it is resolved before any backend is chosen"
                    .to_string(),
            )
        })?;
        crate::perimeter::seat_for_fence(p, &cfg.mode, &cfg.workspace, &home)
            .map_err(HarnessError::Setup)
    }

    /// The view this machine's confined commands are threaded with: the session's own perimeter
    /// over what exists under the operator's home, plus `[boundary]`. A config that is refused
    /// refuses the seat.
    fn local_seat(cfg: &Config) -> Result<letibot_tools::exec::seat::SeatView, HarnessError> {
        use letibot_tools::exec::seat;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let r = crate::perimeter::resolved(&cfg.workspace, &home).map_err(HarnessError::Setup)?;
        let p = crate::perimeter::of(cfg).map_err(HarnessError::Setup)?;
        let view = p
            .render(
                &cfg.mode,
                "this machine",
                &seat::present_under(&home),
                &home,
                &p.secret_values(&home).0,
            )
            .map_err(HarnessError::Setup)?;
        Ok(letibot_tools::exec::boundary::apply(view, &r))
    }

    /// The default seat's view: the session's own perimeter - wide for a top-level session, tight
    /// for a child - with `[boundary]` applied. The secrets it carries are asked for once, here,
    /// outside the boundary; a child carries none unless its perimeter names one.
    fn default_seat(
        cfg: &Config,
        hub: &Arc<Hub>,
    ) -> Result<letibot_tools::exec::seat::SeatView, HarnessError> {
        use letibot_tools::exec::seat;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        // Refused before a secret is asked for: a bad config opens nothing.
        let r = crate::perimeter::resolved(&cfg.workspace, &home).map_err(HarnessError::Setup)?;
        let p = crate::perimeter::of(cfg).map_err(HarnessError::Setup)?;
        let (secrets, said) = p.secret_values(&home);
        for w in said {
            hub.warn("gh_token_absent", w);
        }
        let view = p
            .render(
                &cfg.mode,
                "this machine",
                &seat::present_under(&home),
                &home,
                &secrets,
            )
            .map_err(HarnessError::Setup)?;
        Ok(letibot_tools::exec::boundary::apply(view, &r))
    }

    /// The default local seat for a `leticode` session: commands inside the seat's boundary over
    /// the checkout, file tools still rooted at `/`. `None` where the seat asks for no command, and
    /// a machine with no boundary fails closed on Linux and falls back to unconfined on a Mac.
    /// The seat comes back with the backend because the file tools obey the same fence.
    fn wide_seat(
        cfg: &Config,
        hub: &Arc<Hub>,
        may_exec: bool,
    ) -> Result<Option<(HostBackend, letibot_tools::exec::seat::SeatView)>, HarnessError> {
        if !(cfg.unconfined && cfg.allow_bash && may_exec) {
            return Ok(None);
        }
        let seat = Self::default_seat(cfg, hub)?;
        let built = HostBackend::confined_over(
            "/",
            &cfg.workspace,
            confined_grants(cfg, cfg.landing_root.as_deref()),
            seat.clone(),
        )
        .and_then(|b| b.with_cwd(&cfg.workspace));
        match built {
            Ok(b) => Ok(Some((b, seat))),
            Err(e) => Err(HarnessError::Setup(Self::say_wide_seat_refused(hub, &e))),
        }
    }

    /// Where this session's tools run and what bounds them: host backend, firecode VM, or remote checkout, then the shared home, promote channel and scratch directory.
    /// The one place a downgrade, placement or missing boundary is decided.
    fn place_the_tools(
        cfg: &mut Config,
        hub: &Arc<Hub>,
        session_registry: &Arc<letibot_sessionlog::registry::Registry>,
    ) -> Result<Placed, HarnessError> {
        // The backend the seat needs, and the constructor is the disclosure: choosing
        // `HostBackend::executable` would be choosing no boundary, which no daemon may do on an
        // operator's behalf, and a downgrade closes doors here too. leticode roots at `/`.
        use letibot_tools::schema::Access as Cls;
        // The perimeter this session's commands run in, resolved once: the table its config names,
        // else the one its parent's spawn composed for it, else its level's default. Resolved
        // BEFORE the seat, because a config that is refused must refuse the session.
        if cfg.perimeter.is_none() {
            let p = crate::perimeter::of(cfg).map_err(HarnessError::Setup)?;
            cfg.perimeter = Some(Arc::new(p));
        }
        // And the record of what the operator granted into the view mid-session. Made HERE rather
        // than at the gate, because a child is built from this config: a registry the gate alone
        // held would be one record per session, and five siblings ask one question five times.
        if cfg.view_grants.is_none() {
            cfg.view_grants = Some(Arc::new(letibot_tools::view::ViewGrants::new()));
        }
        let may_exec = !cfg.downgrade.denies(Cls::Exec);
        let may_write = !cfg.downgrade.denies(Cls::Write);
        // Set by the arms that hand the session the whole host, so the prompt and the head both
        // say it: silence here is how a session reads as confined when nothing bounds it.
        let mut whole_host = false;
        // A VM on THIS box, which is the local firecode backend's door. A VM on a declared
        // host is booted by that host's own `firecode` over ssh, so it goes through the
        // placement below instead.
        let local_vm = cfg.vm_attach.is_some()
            || cfg.placement == letibot_tools::builtins::task::Placement::Firecode;
        // A VM placement is the boundary, so the mode inside is allow-all, and the downgrade still
        // composes on top because it is the caller's explicit ask.
        let in_vm = cfg.placement.is_vm() || cfg.vm_attach.is_some();
        cfg.mode = mode_in_vm(cfg.mode, &cfg.placement, cfg.vm_attach.is_some());
        // Where these tools run when it is another machine: not a boundary, so not an allow-all,
        // and the name is resolved here so a session reopened after the file changed fails with a
        // sentence rather than with a bare "command not found".
        let remote = match cfg.placement.host() {
            Some(name) => {
                let hosts = letibot_tools::remote::Hosts::load();
                let Some(host) = hosts.get(name).cloned() else {
                    return Err(HarnessError::Setup(hosts.refusal(name)));
                };
                Some(host)
            }
            None => None,
        };
        // A session opening on a workspace that is gone says so, by that name: every backend below
        // roots itself there and would otherwise fail with a bare ENOENT, or with a paragraph
        // blaming the cgroup and bwrap first.
        if !cfg.workspace.is_dir() {
            let said = workspace_gone(&cfg.workspace);
            hub.warn(WORKSPACE_GONE, said.clone());
            return Err(HarnessError::Setup(said));
        }
        // Set by the two arms whose commands run inside a boundary on this box: the seat that
        // fence was built from, so the file tools are fenced with the same one.
        let mut seat: Option<letibot_tools::exec::seat::SeatView> = None;
        // `backend_confined` is the VM's and nothing else here, and it stops being a guess the line
        // after the substrate is chosen. A remote placement confines nothing by itself, so claiming
        // it did made `Prereq::Confinement` pass for a session that had no boundary.
        let backend: HostBackend = if local_vm {
            // A placeholder the VM arm below replaces; the host tree is not touched by a
            // session whose tools run elsewhere, and this read-only view is never used.
            HostBackend::new(&cfg.workspace)
                .map_err(|e| HarnessError::Setup(format!("workspace {:?}: {e}", cfg.workspace)))?
        } else if remote.is_some() {
            // The same placeholder, and nothing measured yet: the question is settled by
            // `SshBackend::place` below, which probes the host.
            HostBackend::new(&cfg.workspace)
                .map_err(|e| HarnessError::Setup(format!("workspace {:?}: {e}", cfg.workspace)))?
        } else if let Some((b, seat_of)) = Self::wide_seat(cfg, hub, may_exec)? {
            seat = Some(seat_of);
            b
        } else if cfg.unconfined {
            // A `leticode` seat asks for no boundary, and this session seats no shell for one to
            // go in front of, so it gets the whole host. Said on the hub and in the prompt; the
            // mode is what decides each command, and it is not a boundary.
            whole_host = true;
            // Said by the top-level session; a child it started inherits the same state. A
            // boundary that was asked for and refused has already said so itself.
            if cfg.parent_session_id.is_none() && !(cfg.allow_bash && may_exec) {
                Self::say_no_boundary(hub);
            }
            // R121: the file tools are still this session's, so they are held to its perimeter -
            // what `read`, `write`, `edit`, `grep` and `glob` reach is the view its commands would
            // have had, taken from the table rather than from a substrate nothing asked to build.
            seat = Some(Self::fence_seat(cfg)?);
            if cfg.allow_bash && may_exec {
                HostBackend::executable("/")
            } else if may_write {
                HostBackend::writable("/")
            } else {
                HostBackend::new("/")
            }
            .map_err(|e| HarnessError::Setup(format!("whole-host backend: {e}")))?
            // Reaching the whole host is not the same as starting at its root: a
            // relative path, and a command's `cwd`, start in the workspace.
            .with_cwd(&cfg.workspace)
            .map_err(|e| {
                HarnessError::Setup(format!("workspace {:?} as cwd: {e}", cfg.workspace))
            })?
        } else if cfg.seat.needs_exec_backend() && may_exec {
            let seat_of = Self::local_seat(cfg)?;
            let built = HostBackend::confined_granting(
                &cfg.workspace,
                confined_grants(cfg, cfg.landing_root.as_deref()),
                seat_of.clone(),
            );
            match built {
                Ok(b) => {
                    seat = Some(seat_of);
                    b
                }
                Err(e) => {
                    let said = format!(
                        "the `{}` role needs a confined execution backend and one could not be \
                     built over {:?}: {e}.\n\nThis is a refusal, not a degradation: the \
                     alternative is `HostBackend::executable`, which gives a process the \
                     cgroup that bounds its lifetime and NO boundary on what it can read \
                     — the operator's whole filesystem, with a banner that would have to \
                     say so. It needs {BOUNDARY_NEEDS} The error above says which is \
                     missing. Another thing the message above may \
                     name instead: the project root itself. Both `runner` and `coder` \
                     root their view at the workspace, so a workspace that is a file, \
                     a dangling symlink or absent fails here and no boundary is the \
                     wrong thing to blame. Seat `--role orchestrator` for a session \
                     with no exec path at all.",
                        cfg.seat.as_str(),
                        cfg.workspace
                    );
                    eprintln!("letibot: {said}");
                    hub.warn(BOUNDARY_ABSENT, said.clone());
                    return Err(HarnessError::Setup(said));
                }
            }
        } else if cfg.seat.needs_writable_backend() && may_write {
            HostBackend::writable(&cfg.workspace)
                .map_err(|e| HarnessError::Setup(format!("workspace {:?}: {e}", cfg.workspace)))?
        } else {
            HostBackend::new(&cfg.workspace)
                .map_err(|e| HarnessError::Setup(format!("workspace {:?}: {e}", cfg.workspace)))?
        };
        // The home the backend expands `~` against is the session's, read once from
        // the surroundings the gate will also carry - so a `~` and the gate's own
        // `region_of` agree about where the operator lives.
        let surroundings = surroundings_for(cfg);
        let backend = backend.with_home(
            surroundings
                .home
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_default(),
        );
        // A head's Ctrl+B reaches the `bash` wait loop through the hub's promote
        // channel, so the request is honoured while the worker is blocked inside it.
        let backend = backend.with_promote_channel(hub.promote_channel());
        // The session's scratch directory, where tools put working artifacts too big for the
        // transcript: per-session and in the runtime dir, so a fetched page never touches the
        // operator's tree. A backend that cannot reach it simply has no scratch.
        let scratch = scratch_dir();
        // 0700, not the umask's default: `create_dir_all` cannot take a mode, and 0755 would leave
        // the scratch world-readable wherever the parent already existed. The mode applies to every
        // component this creates; the ones that exist were 0700 when they were made.
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut b = std::fs::DirBuilder::new();
            b.mode(0o700).recursive(true);
            if let Err(e) = b.create(&scratch) {
                // stderr rather than a log framework: this crate has none, and a
                // scratch that cannot be made is a start-up fact the operator sees
                // beside the daemon's own banner or nowhere.
                eprintln!("letibot: scratch dir {}: {e}", scratch.display());
            }
        }
        let backend = backend.with_scratch_dir(&scratch);
        // The session's own `/tmp`, for a command that runs inside a boundary: a fresh tmpfs per
        // call lost a half-written patch, and the failure read as a broken shell. It is a directory
        // of its own, created and removed by the backend; with no boundary `/tmp` is left alone.
        let session_tmp = scratch.join("tmp").join(&cfg.session_id);
        let backend = backend
            .with_session_tmp(&session_tmp)
            .map_err(|e| HarnessError::Setup(format!("this session's /tmp: {e}")))?;
        // R121 item 8: the file tools obey the perimeter the shell's view draws, so a path a
        // command cannot reach is refused to `read` and `write` in the same words. The letibot
        // store is reachable only where this session's own `sessions/<id>/` is. A seat whose view
        // nobody measured - no shell to put a boundary in front of - gets the same fence rendered
        // from its own perimeter, so the two arms cannot say different things about one table.
        let backend = match (&seat, backend.boundary()) {
            (Some(seat_of), measured) => {
                let view = match measured {
                    Some(m) => m.view.clone(),
                    None => {
                        let mut v = letibot_tools::exec::confine::ViewSpec::of_seat(
                            &cfg.workspace,
                            seat_of,
                        );
                        for g in confined_grants(cfg, cfg.landing_root.as_deref()) {
                            v = v.granting(g);
                        }
                        v
                    }
                };
                let mut fence = letibot_tools::exec::fence::FileFence::of(seat_of, &view)
                    .allowing(&scratch)
                    .allowing(&session_tmp);
                if let Some(store) = crate::mergequeue::git::letibot_data_dir() {
                    fence = fence.owning(&store, &cfg.session_id);
                }
                backend.with_file_fence(fence)
            }
            _ => backend,
        };
        if backend.boundary().is_some() {
            cfg.session_scratch = Some(session_tmp.clone());
        }
        // One boxed backend from here on, whichever substrate: the host one built
        // above, or a firecode VM booted on a copy of the tree `Config::vm_copy_source` names.
        let (backend, backend_described, backend_writable, monitors): (
            Box<dyn letibot_tools::backend::ExecBackend>,
            String,
            bool,
            Option<Arc<Monitors>>,
        ) = if local_vm {
            // The tree the VM is booted on, and it is NOT always the workspace: `vm_copy_source`
            // is the one spelling of the decision, so a session seated above its repositories does
            // not push the whole folder through `copy_tree`.
            let fc = if let Some(project) = &cfg.vm_attach {
                letibot_tools::firecode::FirecodeBackend::attach(
                    None,
                    project.clone(),
                    may_write,
                    may_exec,
                )
                .map_err(|e| {
                    HarnessError::Setup(format!("running this session in its parent's VM: {e}"))
                })?
            } else {
                // `--vm` on a Mac: the workspace itself, mounted read-write, every repository in
                // it - not a copy of one. A copy otherwise, as before.
                let mounts = cfg.vm_mounts_workspace();
                let mut spec = letibot_tools::firecode::FirecodeSpec::new(
                    if mounts {
                        cfg.workspace.clone()
                    } else {
                        cfg.vm_copy_source()
                    },
                    &cfg.session_id,
                );
                spec.mount_rw = mounts;
                spec.writable = may_write;
                spec.exec = may_exec;
                spec.up_args = cfg.vm_args.clone();
                spec.verify = cfg.vm_verify.clone();
                letibot_tools::firecode::FirecodeBackend::up(&spec).map_err(|e| {
                    HarnessError::Setup(format!("placing this session in a firecode VM: {e}"))
                })?
            };
            // A promoted run's output has a file here too: the drain is this process's, so the
            // file lands in this daemon's scratch directory, where the model's `read` can open it.
            if let Some(h) = fc.host_processes() {
                h.set_scratch_dir(&scratch);
            }
            let described = fc.describe();
            let writable = fc.is_writable();
            let monitors = fc.host_processes().and_then(|h| h.monitors().cloned());
            (Box::new(fc), described, writable, monitors)
        } else if let Some(host) = &remote {
            // The ssh placement: the repository's HEAD goes over as a bundle, the session works in
            // a checkout there, and its commits come back when it ends. No allow-all and no
            // boundary: a remote host is the operator's machine, so it gets a host session's gate.
            let spec = letibot_tools::remote::SshSpec {
                host: host.clone(),
                name: cfg.session_id.clone(),
                // The repository this session is about, by the one resolution the tree placement
                // already uses: `SshBackend::place` refuses by name a source that is not one.
                source: cfg.vm_copy_source(),
                writable: may_write,
                exec: may_exec && (cfg.allow_bash || cfg.seat.needs_exec_backend()),
                ssh: None,
                // The operator's own home and config directory: which key a host connects with,
                // and which host keys have been accepted for letibot, are the running session's
                // facts and not the caller's.
                paths: None,
                // The bound every call that is not a command carries ([`STEP_BOUND`]): a session's
                // tools must not be able to hold the daemon on a host that has stopped answering.
                bound: None,
                // The bound an arranging step carries ([`PLACE_STEP_BOUND`], a minute): these run
                // while that machine may be building, and four placements died at 20 s there.
                place_bound: None,
                egress: letibot_tools::exec::seat::network_rule(&cfg.mode, &host.name),
                // Ask the host, which is what production always does. A placement probes once
                // per host (cached), and a host that cannot answer runs unconfined with the reason
                // on the session's own notes - see `SshBackend::place`.
                confine: None,
                // the same placement, with a VM booted on that host. `--host NAME
                // --vm` places the tools in a firecode guest there; the probe and the boot are
                // `SshBackend::place`'s, and a host that cannot boot one is refused by name.
                vm: cfg.placement.is_vm(),
            };
            let sb = letibot_tools::remote::SshBackend::place(&spec)
                .map_err(|e| HarnessError::Setup(e.to_string()))?;
            // A stalled host call is on the head while it stalls, not only when it fails.
            {
                let hub = hub.clone();
                sb.on_slow(std::sync::Arc::new(move |code: &str, detail: String| {
                    hub.warn(code, detail);
                }));
            }
            // Where the session is, and what was left behind. Published rather than
            // logged: a tree the operator is not looking at has to say so on the surface
            // they read, including the local edits that were NOT carried.
            for note in sb.placement_notes() {
                hub.warn(REMOTE_PLACEMENT, note.clone());
            }
            let described = sb.describe();
            let writable = sb.is_writable();
            // No monitor mechanism on this host: a `monitor` call refuses by name rather
            // than watching something nothing reaps.
            (Box::new(sb), described, writable, None)
        } else {
            let described = backend.describe();
            let writable = backend.is_writable();
            // What this daemon manages, declared before any tool runs: the model server it talks
            // to. `pkill` refuses it by name and a `process` monitor never finds it.
            if let Some(h) = backend.host_processes() {
                // The only clock a background job has: it is ended when it is DEAD, never at an
                // age. `[exec] job_dead_after_seconds`, the project's over the user's.
                h.set_dead_after(crate::leticode_config::job_dead_after(
                    &cfg.leticode_user,
                    &cfg.workspace,
                    cfg.read_project_config,
                ));
                h.protect_model_server(
                    cfg.endpoint.port,
                    format!(
                        "the model server this session talks to, on {}",
                        cfg.endpoint.authority()
                    ),
                    true,
                );
                // What every command carries: the way back to this daemon (the head's sudo
                // prompt), `HOME`, and a `sudo` shim ahead of `PATH` when the helper is beside
                // this binary.
                let mut env = vec![
                    (
                        "LETIBOT_SOCKET".to_string(),
                        cfg.socket.display().to_string(),
                    ),
                    ("LETIBOT_SESSION".to_string(), cfg.session_id.clone()),
                ];
                if let Ok(home) = std::env::var("HOME") {
                    env.push(("HOME".to_string(), home));
                }
                match crate::sudo::install() {
                    Ok(plumbing) => {
                        env.push((
                            "SUDO_ASKPASS".to_string(),
                            plumbing.askpass.display().to_string(),
                        ));
                        h.set_path_prefix(Some(plumbing.shims.display().to_string()));
                    }
                    Err(why) => {
                        eprintln!("letibot: sudo will have no way to ask for a password: {why}")
                    }
                }
                // And the pairs this session's spawn gave it, so a worktree child's builds share
                // the main tree's cache. After the daemon's own, so a spawn's word wins; never
                // `PATH`, which the host pins.
                env.extend(cfg.build_env.iter().cloned());
                h.set_standing_env(env);
                // A stop ends the runs this session has in flight: the worker inside a run has not
                // reached `next_command`, so the thread that took the `Stop` kills the cgroup here.
                // `kill_job_as` makes the run's own row say the daemon was stopping.
                let id = cfg.session_id.clone();
                // What this session can SAY is running, without ending it, so the announcement
                // comes before the `Bye` a kill that takes seconds would otherwise follow.
                {
                    let host = Arc::clone(h);
                    session_registry.name_runs(
                        &id,
                        Arc::new(move || {
                            host.jobs()
                                .into_iter()
                                .filter(|v| v.state.is_running())
                                .map(|v| format!("`{}` (`{}`)", v.command, v.id))
                                .collect()
                        }),
                    );
                }
                let running = Arc::clone(h);
                session_registry.watch_runs(
                    &id,
                    Arc::new(move || {
                        // The reason travels into the run's own state and out onto the row a
                        // head would have drawn: `killed by the daemon stopping`. What the stop
                        // says out loud was named before this ran - see `name_runs` above.
                        let _ = running.end_running("the daemon stopping");
                    }),
                );
                // The boundary this session's `!term` pane runs inside, read off this host at the
                // moment the pane opens so a grant made since is in it. A session with no host
                // registers nothing.
                let host = Arc::clone(h);
                session_registry.bound_panes(
                    &id,
                    Arc::new(move |cwd: &std::path::Path, env: &[(String, String)]| {
                        host.terminal_wrap(cwd, env).map_err(|e| e.to_string())
                    }),
                );
            }
            let monitors = backend.host_processes().and_then(|h| h.monitors().cloned());
            (Box::new(backend), described, writable, monitors)
        };

        // `backend_confined`, asked of the backend that was actually built: a VM is a boundary by
        // construction, and a host or ssh backend is one only where a confinement was measured. A
        // remote session whose probe failed is unconfined, which is what its `describe` says.
        let backend_confined = in_vm || backend.boundary().is_some();

        Ok(Placed {
            backend,
            described: backend_described,
            writable: backend_writable,
            monitors,
            surroundings,
            confined: backend_confined,
            whole_host,
        })
    }

    /// The conversation this session opens on: the stored one resumed as it stands, forked onto this build's prompt when the stored rendering or vocabulary moved, repaired onto the prefix its rows chain from when it will not rebuild, else a fresh one.
    /// Answers transcript id, session, stored row count, resume rebuild, seat prefix and store id, and the attach sentence when the prompt is one this daemon would not compose now.
    #[allow(clippy::type_complexity, clippy::too_many_arguments)]
    fn open_the_conversation(
        store: &Option<Store>,
        stored: &Option<letibot_tokencore::store::StoredSession>,
        engine: &mut TurnEngine,
        parts: &Parts,
        cfg: &Config,
        prefix: &StablePrefix,
        dialect_sha: &str,
        notes: &mut Vec<String>,
    ) -> Result<
        (
            String,
            Session,
            usize,
            Option<ResumeReport>,
            StablePrefix,
            String,
            Option<String>,
            Option<String>,
            letibot_plugin::lifecycle::Stale,
        ),
        HarnessError,
    > {
        // A session already in the store is RESUMED, not opened a second time. Deciding it here rather than on a flag
        // is what makes the two entry points agree, since a restart and a head asking a running daemon are the same act.
        let resumed = match (&store, &stored) {
            (Some(s), Some(st)) if st.items > 0 => {
                let transcript_id = st.transcript_id.clone().ok_or_else(|| {
                    HarnessError::Store(format!(
                        "session {} has {} row(s) but no transcript row to hang them on; \
                     this store was written by something else",
                        st.id, st.items
                    ))
                })?;
                Some((s, transcript_id))
            }
            _ => None,
        };

        Ok(match resumed {
            Some((s, transcript_id)) => {
                let loaded = s
                    .load_transcript(&transcript_id)
                    .map_err(|e| HarnessError::Store(e.to_string()))?;

                // The refusal that has to stay: the tokens were produced by one renderer against one vocabulary, and
                // appending with another dialect would put two templates' bytes in one prompt with nothing downstream
                // able to see it, since the chain still verifies.
                let stored_meta = s
                    .stable_prefix_meta(&loaded.stable_prefix_id)
                    .map_err(|e| HarnessError::Store(e.to_string()))?;
                let stored_sha = stored_meta
                    .as_ref()
                    .map(|m| m.dialect_sha.clone())
                    .unwrap_or_default();
                // And the vocabulary that cut the stored ids: a resume replays them as numbers, so ids from another
                // vocabulary are valid numbers that mean other text. Same remedy as a dialect change, from the same
                // record; two GGUFs are compared nowhere here.
                let stored_vocab = stored_meta
                    .as_ref()
                    .map(|m| m.vocab_source.clone())
                    .unwrap_or_default();
                let vocab_moved = vocab_differs(&stored_vocab, parts.vocab.source());
                // A rendering that cannot be reused is not a conversation that cannot be continued:
                // `item_json` is dialect-neutral, so the items are the record and the tokens are a
                // cache. It forks rather than rewriting, and the cost is one cold prefill, stated.
                if stored_sha != dialect_sha || vocab_moved {
                    let store = s;
                    let items: Vec<letibot_transcript::TranscriptItem> =
                        loaded.items.iter().map(|(i, _, _)| i.clone()).collect();

                    let probe = engine
                        .open(&format!("{transcript_id}#reprefill-probe"), prefix)
                        .map_err(|e| {
                            HarnessError::Setup(format!("rendering the new prompt: {e}"))
                        })?;
                    let rec = StablePrefixRecord {
                        dialect_sha: dialect_sha.to_string(),
                        system: prefix.system.clone(),
                        tools_json: prefix.tools_json.clone(),
                        tokens: probe.ledger.prefix_tokens().to_vec(),
                        h_init: probe.ledger.h_init(),
                        vocab_source: parts.vocab.source().to_string(),
                    };
                    drop(probe);
                    let new_prefix_id = store
                        .put_stable_prefix(&rec)
                        .map_err(|e| HarnessError::Store(e.to_string()))?;

                    // `#tN`, N counted the way `fork_to_summary` counts it, so the
                    // two forks cannot collide on an id.
                    let n: i64 = store
                        .connection()
                        .query_row(
                            "SELECT COUNT(*) FROM transcript WHERE session_id = ?1",
                            [cfg.session_id.as_str()],
                            |r| r.get(0),
                        )
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                    let new_id = format!("{}#t{}", cfg.session_id, n);

                    let mut rebuilt = engine
                        .open(&new_id, prefix)
                        .map_err(|e| HarnessError::Setup(format!("opening the fork: {e}")))?;
                    // Each item rendered against the history as it stood before it,
                    // which is `render_incremental`'s contract and what keeps the
                    // ledger rows aligned with the items.
                    rebuilt
                        .append_items(engine, &items, &mut letibot_turn::events::NullSink)
                        .map_err(|e| {
                            HarnessError::Setup(format!(
                                "re-rendering this conversation under {}: {e}",
                                parts.wiring.spec().name
                            ))
                        })?;

                    // Unsealed until every row is in: a daemon that dies while
                    // the rows go in reopens on the stored transcript, whole, not on part of
                    // this one. Sealed after the rows, and the fingerprint follows the seal.
                    store
                        .put_fork_unsealed(
                            &new_id,
                            &cfg.session_id,
                            &new_prefix_id,
                            &transcript_id,
                            items.len() as u32,
                        )
                        .map_err(|e| HarnessError::Store(e.to_string()))?;

                    // Written before the head attaches, so a daemon that dies now leaves a fork
                    // that resumes. A chunk per `BEGIN IMMEDIATE` transaction, because one
                    // autocommit per row held every daemon's write lock for minutes.
                    let rows = rebuilt.ledger.rows();
                    let mut run = Vec::with_capacity(rows.len());
                    for (i, row) in rows.iter().enumerate() {
                        let tokens = rebuilt.ledger.item_tokens(i).ok_or_else(|| {
                            HarnessError::Store(format!("no tokens for re-rendered row {i}"))
                        })?;
                        run.push((&rebuilt.items[i], row, tokens));
                    }
                    store
                        .append_items(&new_id, 0, run)
                        .map_err(|e| HarnessError::Store(format!("re-rendered rows: {e}")))?;
                    store
                        .seal_transcript(&new_id)
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                    // The session now speaks the prefix this daemon composes, and the column says
                    // so: left alone, it would report itself stale on its very next resume, having
                    // just been re-rendered onto exactly the prompt the comparison would compose.
                    store
                        .set_prefix_fingerprint(
                            &cfg.session_id,
                            &prefix_fingerprint(cfg.seat, prefix),
                        )
                        .map_err(|e| HarnessError::Store(e.to_string()))?;

                    let before: usize = loaded.items.iter().map(|(_, _, t)| t.len()).sum();
                    let recorded_under = if vocab_moved {
                        format!(
                            "the vocabulary `{}` and this daemon tokenizes with `{}`",
                            stored_vocab,
                            parts.vocab.source()
                        )
                    } else {
                        format!(
                            "dialect template {} and this daemon renders {}",
                            &stored_sha[..16.min(stored_sha.len())],
                            &dialect_sha[..16]
                        )
                    };
                    notes.push(format!(
                        "this conversation was recorded under {recorded_under}. Its {} item(s) \
                     were RE-RENDERED for this one — \
                     the stored tokens are a cache of the other rendering, the items \
                     themselves are the record, and nothing was detokenized to do it. \
                     {} token(s) became {}, which is one cold prefill, once, on the next \
                     turn. The old transcript {} keeps its tokens and its chain; this is \
                     a fork, and nothing stored was rewritten.",
                        items.len(),
                        before,
                        rebuilt.ledger.len(),
                        transcript_id,
                    ));

                    let rows = rebuilt.ledger.rows().len();
                    let report = ResumeReport {
                        transcript_id: new_id.clone(),
                        rows,
                        tokens: rebuilt.ledger.len(),
                        head: rebuilt.ledger_head(),
                        workspace: cfg.workspace.display().to_string(),
                        notes: std::mem::take(notes),
                    };
                    // Every row was written above, so none is pending.
                    (
                        new_id,
                        rebuilt,
                        rows,
                        Some(report),
                        prefix.clone(),
                        new_prefix_id,
                        // Re-rendered onto the prefix this daemon composes, so the session
                        // speaks it: nothing to say to a head that attaches.
                        None,
                        None,
                        // And a repaired session is not a re-seat either: the same prompt written
                        // again, so there is nothing a lifecycle layer could name as changed.
                        Default::default(),
                    )
                } else {
                    // A transcript whose rows do not rebuild is repaired, not refused for ever:
                    // `prefix_repair` forks onto the prefix the rows were really written under and
                    // leaves the old transcript untouched. Anything else keeps its refusal.
                    let mut transcript_id = transcript_id;
                    // The prefix the conversation is seated with - the row's own, or the one a
                    // repair found the rows chaining from.
                    let mut seated_prefix_id = loaded.stable_prefix_id.clone();
                    let mut repaired = false;
                    let session = match Session::restore(&loaded) {
                        Ok(session) => session,
                        Err(e) => match prefix_repair::rows_on_another_prefix(
                            s,
                            &cfg.session_id,
                            &loaded,
                            &e,
                        )
                        .map_err(|e| HarnessError::Store(e.to_string()))?
                        {
                            Some(fixed) => {
                                transcript_id = fixed.transcript_id;
                                seated_prefix_id = fixed.prefix_id;
                                repaired = true;
                                notes.push(fixed.note);
                                fixed.session
                            }
                            None => {
                                return Err(HarnessError::Store(format!(
                                    "session {}: {e}",
                                    cfg.session_id
                                )));
                            }
                        },
                    };
                    // What a head attaching to this session should be told, or `None` when the
                    // session speaks the prompt this daemon would compose now - and the same
                    // finding in the model's own voice, set and cleared with it.
                    let mut stale: Option<String> = None;
                    let mut stale_for_model: Option<String> = None;
                    // The same facts as a LIST, for a lifecycle layer: a layer that names the tool
                    // that arrived cannot get it out of a sentence (X21, G4).
                    let mut stale_facts = letibot_plugin::lifecycle::Stale::default();

                    // The prefix the session's own tokens came from - the store's
                    // record, not the daemon's current render. A compaction fork keeps
                    // it, exactly as the resume itself does.
                    let own = s
                        .stable_prefix_record(&seated_prefix_id)
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                    let own_prefix = StablePrefix {
                        system: own.system,
                        tools_json: own.tools_json,
                    };
                    let own_fingerprint = prefix_fingerprint(cfg.seat, &own_prefix);
                    // The fingerprint follows the transcript: a repair forked onto another prompt,
                    // so a column left behind would report this session stale against a prompt
                    // nobody is speaking.
                    if repaired {
                        s.set_prefix_fingerprint(&cfg.session_id, &own_fingerprint)
                            .map_err(|e| HarnessError::Store(e.to_string()))?;
                    }

                    // Message 0 is frozen, held up here against what this daemon would seat now: a build that changed
                    // the instructions, the schemas or the seat composes a prompt the session will never speak, and says
                    // nothing. The comparison is `prefix_fingerprint`'s, with the stored row as the fallback.
                    let seated = match repaired {
                        true => own_fingerprint.clone(),
                        false => stored
                            .as_ref()
                            .and_then(|r| r.prefix_fingerprint.clone())
                            .unwrap_or_else(|| own_fingerprint.clone()),
                    };
                    if seated != prefix_fingerprint(cfg.seat, prefix) {
                        let (was, is) = (
                            tool_schemas(&own_prefix.tools_json),
                            tool_schemas(&prefix.tools_json),
                        );
                        let mut added: Vec<String> = Vec::new();
                        let mut gone: Vec<String> = Vec::new();
                        // A schema that GREW is named as one, and it is not the same finding as a tool that arrived:
                        // it is in both lists, so the added/gone pair is silent about it, and a grown schema is exactly
                        // the case that started this.
                        let mut grew: Vec<String> = Vec::new();
                        for (name, schema) in &is {
                            match was.get(name) {
                                None => added.push(name.clone()),
                                Some(old) if old != schema => grew.push(name.clone()),
                                Some(_) => {}
                            }
                        }
                        for name in was.keys() {
                            if !is.contains_key(name) {
                                gone.push(name.clone());
                            }
                        }
                        let instructions =
                            instructions_of(&own_prefix.system) != instructions_of(&prefix.system);
                        let mut moved: Vec<&str> = Vec::new();
                        if instructions {
                            moved.push("the system instructions");
                        }
                        if !added.is_empty() || !gone.is_empty() || !grew.is_empty() {
                            moved.push("the tool schemas");
                        }
                        let say = stale_prefix_sentence(
                            &cfg.session_id,
                            &seated_at(stored.as_ref().map(|r| r.created_ms).unwrap_or(0)),
                            &moved,
                            &added,
                            &gone,
                            &grew,
                        );
                        // On the session's own log, and said AT ATTACH rather than here: a warning published before a
                        // head attaches is filed `Placed::Before` and not drawn. So it goes into the registry, which also
                        // tells a head already watching. The model gets its own, shorter sentence.
                        stale = Some(say);
                        stale_for_model = Some(stale_prompt_for_the_model(&added, &gone, &grew));
                        let mut tools: Vec<String> = added
                            .iter()
                            .chain(gone.iter())
                            .chain(grew.iter())
                            .cloned()
                            .collect();
                        tools.sort();
                        tools.dedup();
                        stale_facts = letibot_plugin::lifecycle::Stale {
                            prompt: stale.clone(),
                            tools,
                        };
                    }

                    let rows = session.ledger.rows().len();
                    let report = ResumeReport {
                        transcript_id: transcript_id.clone(),
                        rows,
                        tokens: session.ledger.len(),
                        head: session.ledger_head(),
                        workspace: cfg.workspace.display().to_string(),
                        notes: std::mem::take(notes),
                    };
                    (
                        transcript_id,
                        session,
                        rows,
                        Some(report),
                        own_prefix,
                        seated_prefix_id,
                        stale,
                        stale_for_model,
                        stale_facts,
                    )
                }
            }
            None => {
                let transcript_id = format!("{}#t0", cfg.session_id);
                let session = engine
                    .open(&transcript_id, prefix)
                    .map_err(|e| HarnessError::Setup(format!("opening the session: {e}")))?;
                let mut prefix_id = String::new();
                if let Some(s) = &store {
                    let rec = StablePrefixRecord {
                        dialect_sha: dialect_sha.to_string(),
                        system: prefix.system.clone(),
                        tools_json: prefix.tools_json.clone(),
                        tokens: session.ledger.prefix_tokens().to_vec(),
                        h_init: session.ledger.h_init(),
                        vocab_source: parts.vocab.source().to_string(),
                    };
                    prefix_id = s
                        .put_stable_prefix(&rec)
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                    // `INSERT OR IGNORE`-shaped by hand: a session row may already be here with no rows behind it, so
                    // re-opening it is a resume of an empty conversation rather than a collision.
                    if stored.is_none() {
                        s.put_session(&SessionRecord {
                            id: cfg.session_id.clone(),
                            title: Some(cfg.title.clone()).filter(|t| !t.is_empty()),
                            // Who answers, not what the vocabulary binds: `cfg.model` is the local alias whose tokenizer renders
                            // the prompt, while the turns may go to a provider. This row is what the picker draws. `Sessions::wiring`
                            // is the same fact from one expression.
                            model_id: cfg.prompt_model_name(),
                            dialect_sha: dialect_sha.to_string(),
                            workspace_root: cfg.workspace.display().to_string(),
                            owner: cfg.owner.clone(),
                            // Record it, so a later resume seats what this
                            // conversation was built with rather than whatever the
                            // daemon is running as then.
                            role: Some(cfg.seat.as_str().to_string()),
                            approvers: vec![],
                            parent_session_id: cfg.parent_session_id.clone(),
                        })
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                    }
                    if !transcript_exists(s, &transcript_id) {
                        s.put_transcript(&transcript_id, &cfg.session_id, &prefix_id)
                            .map_err(|e| HarnessError::Store(e.to_string()))?;
                        // The fingerprint of what this session was seated with, written where its transcript row is: a
                        // fork re-seats a session long after its creation and rewrites this, which is why it is not a field
                        // on the record above.
                        s.set_prefix_fingerprint(
                            &cfg.session_id,
                            &prefix_fingerprint(cfg.seat, prefix),
                        )
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                    } else if s
                        .item_count(&transcript_id)
                        .map_err(|e| HarnessError::Store(e.to_string()))?
                        == 0
                        && s.transcript_prefix_id(&transcript_id)
                            .map_err(|e| HarnessError::Store(e.to_string()))?
                            .as_deref()
                            != Some(prefix_id.as_str())
                    {
                        // An empty transcript on ANOTHER prompt is re-seated onto the one this daemon composes: with no rows
                        // there is no chain to rebuild and no cache to keep, so the old prefix would cost a new conversation a
                        // message 0 that cannot call the tools seated since. A transcript that HOLDS rows is untouched.
                        s.set_transcript_prefix(&transcript_id, &prefix_id)
                            .map_err(|e| HarnessError::Store(e.to_string()))?;
                        // And the fingerprint follows, for the reason its column exists: a session still describing the
                        // old prompt would report itself stale on the next resume, having just been re-seated.
                        s.set_prefix_fingerprint(
                            &cfg.session_id,
                            &prefix_fingerprint(cfg.seat, prefix),
                        )
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                        notes.push(format!(
                            "this session's transcript `{transcript_id}` was seated earlier with a \
                         prompt this daemon does not compose (a different build wrote its \
                         prefix row) and had no rows in it, so it has been re-seated onto the \
                         prompt this daemon composes. Nothing was lost: an empty transcript \
                         has no tokens whose chain depends on message 0. The work is done now \
                         and not at the first turn, which is where it used to fail — see \
                         `Store::set_transcript_prefix`."
                        ));
                    }
                }
                (
                    transcript_id,
                    session,
                    0,
                    None,
                    prefix.clone(),
                    prefix_id,
                    None,
                    None,
                    Default::default(),
                )
            }
        })
    }

    /// The embedder's start-up facts on the session's hub, which a head draws: the launcher
    /// sends daemon stderr nowhere, and the model-facing note alone leaves the operator blind.
    fn say_the_embedder(hub: &Hub, cfg: &Config) {
        if let Some(said) = cfg.embedder.take_fetch_note() {
            hub.warn("embedder_fetching", said);
        }
        if let Some(why) = cfg.embedder.fault() {
            hub.warn(
                "embedder_unavailable",
                format!("no embedder, so every score is the words alone: {why}"),
            );
        }
    }

    pub fn open_with_registry(
        parts: &Parts,
        mut cfg: Config,
        hub: Arc<Hub>,
        adjudicator: Option<Box<dyn Adjudicator>>,
        extra_tool: Option<Box<dyn Tool>>,
        session_registry: Arc<letibot_sessionlog::registry::Registry>,
    ) -> Result<Self, HarnessError> {
        let mut notes: Vec<String> = Vec::new();
        // The store first, because it may change the configuration: a session already in it
        // carries its own workspace and seat, and the backend built below follows those.
        let Opening {
            store,
            corpus_sink,
            corpus_counts,
            stored,
            writer,
        } = Self::open_the_stored(&mut cfg, &mut notes)?;
        Self::say_the_embedder(&hub, &cfg);

        let mut mode_source = Self::resolve_the_point(
            parts,
            &mut cfg,
            &store,
            stored.as_ref(),
            &session_registry,
            &mut notes,
        );

        let Placed {
            backend,
            described: backend_described,
            writable: backend_writable,
            monitors,
            surroundings,
            confined: backend_confined,
            whole_host: backend_whole_host,
        } = Self::place_the_tools(&mut cfg, &hub, &session_registry)?;
        let backend_boundary = backend.boundary();
        let backend_boundary_refusal = backend.boundary_refusal();

        // Retrieval is inert and stays inert: `ask_code` and `ask_corpus` abstain, and nothing is
        // behind them. A stub that answered would be the failure this design exists to prevent.
        let retrieval: Arc<dyn letibot_tools::builtins::retrieval::Retrieval> =
            Arc::new(letibot_tools::builtins::retrieval::Unavailable);
        // The web, the forge and MCP are REGISTERED rather than absent, so the registry is a
        // superset and the role decides what a prompt carries. Nothing is attached unless the
        // operator attached it: a search provider is egress and a credential.
        let mut external = letibot_tools::ExternalBackends::unattached();
        if let Some(name) = cfg.web_search.as_deref() {
            match name {
                "brave" => match letibot_websearch::Brave::attach(None) {
                    Ok(b) => external.search = std::sync::Arc::new(b),
                    // Refuse at open, not at the first search: a 401 three seconds into a turn
                    // names neither the key nor the file it is missing from.
                    Err(why) => {
                        return Err(HarnessError::Setup(format!("--web-search brave: {why}")));
                    }
                },
                other => {
                    return Err(HarnessError::Setup(format!(
                        "--web-search {other}: this build has `brave`"
                    )));
                }
            }
        }
        if cfg.web_fetch {
            match letibot_webfetch::CurlFetcher::attach() {
                Ok(f) => external.fetch = std::sync::Arc::new(f),
                // Refuse at open, not at the first fetch: a missing curl into a turn names
                // neither the binary nor the flag that would have attached it.
                Err(why) => return Err(HarnessError::Setup(format!("--web-fetch: {why}"))),
            }
        }
        let external = external;
        // Both halves of the intent ledger are built for every session: not attaching them makes
        // every completion `NoEncoder`, which is a state whose reason is ours.
        let intent = Arc::new(IntentLedger::new());
        // The question slot: the wiring is built before the gate, and the `Answers` it waits on is
        // built WITH the gate as one act with the hub's answer sink. So the gate fills the slot and
        // the tool reads it when called, which is also when a head is attached.
        let question_head: Arc<
            std::sync::Mutex<Option<Arc<dyn letibot_tools::builtins::intent::Questioner>>>,
        > = Default::default();
        let intent_wiring = intent_tools::Wiring {
            ledger: intent.clone(),
            questioner: Arc::new(crate::answers::SlotQuestioner(question_head.clone())),
            ..intent_tools::Wiring::standalone()
        };
        // `--role planner` IS plan mode, so the state says so: otherwise `PlanMode::active` is
        // false for the seat whose whole name is plan mode. The turn and call ids are this seat
        // rather than a decision, because inventing a call id would point a refusal at nobody.
        if cfg.seat == Seat::Planner {
            intent_wiring
                .plan
                .enter("open", &format!("--role {}", cfg.seat.as_str()));
        }

        let mut registry: Registry = letibot_tools::read_only_tools(retrieval.clone())
            .map_err(|e| HarnessError::Setup(format!("registering the M1 tool set: {e}")))?;
        // The session's todo list, restored before the tool that writes it is seated: a resume comes
        // back with the plan the model was working from. A storeless run still gets the tool, it just
        // has nowhere to persist the list.
        let todo_board = Arc::new(TodoBoard::new(
            store
                .as_ref()
                .map(|s| s.todos(&cfg.session_id).unwrap_or_default())
                .unwrap_or_default(),
        ));
        // And registered in the daemon's board map, root or child alike: the one place a parent's
        // `todo_write` with a `target` can find this session's board, so what a parent told a child
        // survives the restart.
        parts
            .child_boards
            .register(&cfg.session_id, todo_board.clone());
        // The slot a child of this session reads to join the tree. Declared
        // here, before the runner that holds it, and filled once `job_watch` exists
        // below - see `HarnessTaskRunner::tree_watch`.
        let tree_watch_slot: Arc<std::sync::Mutex<Option<Arc<crate::jobwatch::JobWatchers>>>> =
            Default::default();
        // The slot a child of this session reads to reach the head's answers, seeded with what THIS
        // session inherited: a child's card is answered by its tree's root, so a child with its own
        // answers would answer into a hub no head is attached to.
        let head_answers_slot: Arc<std::sync::Mutex<Option<Arc<crate::answers::Answers>>>> =
            Arc::new(std::sync::Mutex::new(parts.tree_head.clone()));
        // The daemon's own server window, shared with the runner: a spawn may ask for `model:
        // "local"` while this session is on a cloud provider, and then the child plans against the
        // SERVER's window, which is this cell.
        let local_window_cell: Arc<std::sync::Mutex<Option<Option<u64>>>> = Default::default();
        // This session's point, in a cell its own runner can see: `base` is a clone taken at open
        // and nothing updates it, so a cell is the only way a spawn can read the point the session
        // is AT rather than the one it opened at. Built here, after the mode is final.
        let point_cell: Arc<std::sync::Mutex<letibot_tools::mode::Mode>> =
            Arc::new(std::sync::Mutex::new(cfg.mode));
        // The slot a child of this session hands its survivors to - this session's process host,
        // filled once the backend has one, `tree_watch_slot`'s reason: a child's estate is settled
        // against the session that outlives it, and a leftover with nowhere to go is an orphan.
        let heir_slot: Arc<std::sync::Mutex<Option<Arc<dyn letibot_tools::exec::ProcessHost>>>> =
            Default::default();
        // Where this session's plugin host lands, filled by the daemon once it opens the session's
        // layers and read by a child of this session for what it inherits. Declared here, before the
        // runner that holds it, `tree_watch_slot`'s reason.
        let plugin_slot: Arc<std::sync::Mutex<Option<Arc<crate::plugins::PluginHost>>>> =
            Default::default();
        // This session's handle list, in one place: the runner is what `task` and `task_result`
        // ask, and the tree entry is what the daemon's worker asks when it runs a parked child's
        // turn - see `crates::trees`. One map, or the two would hand out different children.
        let tree_slots: crate::harness::children::TaskSlots =
            parts.tree_slots.clone().unwrap_or_default();
        let task_runner: Arc<dyn letibot_tools::builtins::task::TaskRunner> =
            Arc::new(HarnessTaskRunner {
                vocab: parts.vocab.clone(),
                wiring: parts.wiring.clone(),
                mode_store: parts.mode_store.clone(),
                registry: session_registry.clone(),
                heir: heir_slot.clone(),
                base: cfg.clone(),
                tasks: parts.tasks.clone(),
                skills: parts.skills.clone(),
                lsp: parts.lsp.clone(),
                child_boards: parts.child_boards.clone(),
                // And the daemon's review desk, for the same reason: the child's own harness
                // registers `review_verdict` bound to its own session id, and the desk it submits to
                // has to be the one this session opened the review on.
                reviews: parts.reviews.clone(),
                claude: parts.claude.clone(),
                slots: tree_slots.clone(),
                trees: parts.trees.clone(),
                // Filled below, once this harness has built its own `job_watch`.
                tree_watch: tree_watch_slot.clone(),
                // Filled as this session's gate is built: a root's own answers, or the ones
                // its tree inherited (see `HarnessTaskRunner::head_answers`).
                head_answers: head_answers_slot.clone(),
                local_window: local_window_cell.clone(),
                point: point_cell.clone(),
                foreign_vocabs: Default::default(),
                placed: Arc::new(std::sync::Mutex::new(Vec::new())),
                merge_store: Arc::new(std::sync::Mutex::new(None)),
                plugin_slot: plugin_slot.clone(),
            });
        // Cloned before `with_session_tools` takes it: `digest` folds its findings through the same
        // subagent runner `task` uses, or a digest would be a subagent tree `task_result` cannot show.
        let digest_runner = task_runner.clone();
        // The daemon half of `todo_write`'s `target`, bound to THIS session as the caller: the author
        // stamp and the parentage check are the daemon's facts and never the call's claim.
        let child_todos: Arc<dyn letibot_tools::builtins::todo::ChildTodos> =
            Arc::new(crate::child_todos::ParentTodos::new(
                cfg.session_id.clone(),
                session_registry.clone(),
                parts.child_boards.clone(),
            ));
        // And it is the session's holder of subagent handles, for the watchers and `job_kill`
        // both. Cloned because `task_runner` is moved into the registry next, and all three must
        // see one runner: a second would hand out handles its sibling cannot collect or stop.
        let subagent_runner = task_runner.clone();
        // The merge queue, from the model's side, registered before the session tools so the real
        // desk is the one seated. Bound to THIS session, so every move is recorded as
        // `agent:<session>`; a daemon with no store gets the tool that says so.
        let queue_desk: Arc<dyn letibot_tools::builtins::queue::QueueDesk> = match &cfg.store {
            Some(path) => Arc::new(crate::mergequeue::desk::SessionQueue {
                store_path: path.clone(),
                session: cfg.session_id.clone(),
                workspace: cfg.workspace.clone(),
                registry: session_registry.clone(),
            }),
            None => Arc::new(letibot_tools::builtins::queue::NoQueue),
        };
        registry
            .register(Box::new(letibot_tools::builtins::queue::QueueTool::new(
                queue_desk,
            )))
            .map_err(|e| HarnessError::Setup(format!("registering the queue tool: {e}")))?;
        // The plugin loop: a pane written, built, tested and loaded at run time. The host does not
        // exist yet (the daemon builds it when it opens a session), so the desk holds a SLOT the
        // daemon fills; scaffold and build need no host at all.
        let plugin_desk: Arc<dyn letibot_tools::builtins::plugin::PluginDesk> =
            Arc::new(crate::plugins::desk::SessionPlugins::new(
                cfg.session_id.clone(),
                crate::plugins::session_plugins_dir(&cfg.session_id),
                plugin_slot.clone(),
                hub.clone(),
            ));
        registry
            .register(Box::new(letibot_tools::builtins::plugin::PluginTool::new(
                plugin_desk.clone(),
            )))
            .and_then(|_| {
                registry.register(Box::new(
                    letibot_tools::builtins::plugin::PluginHooksTool::new(plugin_desk),
                ))
            })
            .map_err(|e| HarnessError::Setup(format!("registering the plugin tools: {e}")))?;
        // The model's own verbs on its own session (X24): each one lands in this session's own queue,
        // so a re-seat happens at the end of the turn that asked for it with no head and no tmux.
        let self_desk: std::sync::Arc<dyn letibot_tools::builtins::session_self::SessionSelf> =
            std::sync::Arc::new(crate::selfverbs::SessionAsks::new(
                cfg.session_id.clone(),
                hub.clone(),
            ));
        letibot_tools::register_self_verbs(&mut registry, self_desk)
            .map_err(|e| HarnessError::Setup(format!("registering the self verbs: {e}")))?;
        // `task_resume`: registered here rather than in `with_session_tools` because only the
        // daemon's runner records children to bring back, and a registry without one has nothing
        // for the verb to act on.
        registry
            .register(Box::new(
                letibot_tools::builtins::task::TaskResumeTool::new(task_runner.clone()),
            ))
            .map_err(|e| HarnessError::Setup(format!("registering task_resume: {e}")))?;
        // The host's room, registered before `with_session_tools` so the real link is the one
        // seated. Two conditions are the design: a coordinator exists on this box, and this session
        // is top-level, since a subagent speaks through its parent. A tag's agent is the exception.
        let top_level = crate::room::is_top_level(&cfg);
        //
        // A connection that cannot be made is said once and is not fatal: a session whose room is
        // unreachable is a session that works, and its `room` tool refuses by name.
        let mut room_connector: Option<crate::room::link::Connector> = None;
        let room_late = std::sync::Arc::new(crate::room::link::LateRoom::default());
        let seat = crate::room::seat::SeatCell::new(cfg.workspace.clone(), Some(hub.clone()));
        let room = match (cfg.room_link, top_level, crate::room::socket_path()) {
            (true, true, Some(socket)) => {
                let bell: std::sync::Arc<dyn Fn(&str) + Send + Sync> = {
                    let bell = std::sync::Arc::clone(session_registry.bell());
                    std::sync::Arc::new(move |session: &str| bell.ring_wake(session))
                };
                // Who may restart the room on this session's behalf: the launcher, on the daemon's
                // own path, and nobody on a fixture or on the coordinator itself. A box with no
                // launcher gets `None`, so the pane offers no key rather than one that refuses.
                let supervisor: Option<std::sync::Arc<dyn crate::room::supervise::RoomSupervisor>> =
                    crate::room::supervise::Launcher::found()
                        .filter(|_| cfg.restart_room)
                        .map(|l| std::sync::Arc::new(l) as _);
                // Which room this session is in: the project its chain hangs from, keyed the way the
                // rest of the tree keys a folder, and the box's own room for one it cannot place. The
                // coordinator's own sessions serve every project, so they are in the host room only.
                let project = if cfg.room {
                    String::new()
                } else {
                    crate::room::roster::Roster::new(session_registry.clone()).room_key_of(
                        &cfg.session_id,
                        cfg.parent_session_id.as_deref(),
                        &cfg.workspace.display().to_string(),
                    )
                };
                // The tree, which the room cannot see for itself. One roster per daemon would
                // be one cache per daemon; this is one per open, which is what the link needs and
                // nothing more - `Registry::list` is the same call the plugin feed makes per frame.
                let roster: std::sync::Arc<dyn crate::room::roster::RosterSource> =
                    crate::room::roster::Roster::new(session_registry.clone());
                let session = cfg.session_id.clone();
                let path = socket.clone();
                let seat = seat.clone();
                // The queue's rows the `read` claim check looks up: the store THIS daemon opened,
                // which a link without one reports as a check it could not make.
                let db = store
                    .as_ref()
                    .and_then(|s| s.path())
                    .map(|p| p.to_path_buf());
                let connect: crate::room::link::Connector = Box::new(move || {
                    let room = crate::room::link::SessionRoom::connect(
                        &session,
                        &path,
                        &project,
                        Some(roster.clone()),
                        bell.clone(),
                        supervisor.clone(),
                    )?;
                    room.set_seat(seat.clone());
                    room.set_store(db.clone());
                    Ok(room)
                });
                // No socket yet is not an error: the coordinator may start after this session, and
                // the room pass tries again.
                let first = if socket.exists() {
                    connect()
                } else {
                    Err(String::new())
                };
                room_connector = Some(connect);
                match first {
                    Ok(room) => {
                        room_late.set(room.clone());
                        Some(room)
                    }
                    Err(why) if why.is_empty() => None,
                    Err(why) => {
                        notes.push(format!(
                            "the room at {} could not be reached ({why}), so `room` will refuse \
                             until it is: the coordinator is a daemon of its own (`harnessd \
                             --room`)",
                            socket.display()
                        ));
                        None
                    }
                }
            }
            _ => None,
        };
        if room_connector.is_some() {
            registry
                .register(Box::new(letibot_tools::builtins::room::RoomTool::new(
                    room_late.clone(),
                )))
                .map_err(|e| HarnessError::Setup(format!("registering the room tool: {e}")))?;
        }
        // The host's own changelog, written: a change to this machine's settings is done by a
        // `bash` call and recorded here, so the reason and the operator's words outlive the
        // conversation. Registered before `with_session_tools`, which seats a stub only where none is.
        registry
            .register(Box::new(
                letibot_tools::builtins::changelog::HostChangelogTool::new(std::sync::Arc::new(
                    crate::changelog_scope::ConfigChangelog::new(cfg.session_id.clone()),
                )),
            ))
            .map_err(|e| HarnessError::Setup(format!("registering the host changelog: {e}")))?;
        // This box's own settings, in the platform's vocabulary: a `set` reads the current value,
        // applies the platform's command and appends the changelog line, so the record's `before`
        // is a measurement. It carries the channel because a root setting goes through `sudo -A`.
        registry
            .register(Box::new(
                letibot_tools::builtins::host_config::HostConfigTool::new(std::sync::Arc::new(
                    crate::changelog_scope::ConfigChangelog::new(cfg.session_id.clone()),
                ))
                .with_channel(letibot_tools::builtins::host_config::Channel::new(
                    cfg.socket.clone(),
                    cfg.session_id.clone(),
                )),
            ))
            .map_err(|e| HarnessError::Setup(format!("registering the host config: {e}")))?;
        registry = letibot_tools::with_session_tools(
            registry,
            todo_board.clone(),
            Some(child_todos),
            // The room's half of `todo_write`: a row that waits on another session's answer. It is
            // the same slot the `room` tool talks through, so a link made after the open reaches
            // both - and a box with no coordinator refuses the ask by name.
            Some(room_late.clone()),
            task_runner,
            parts.skills.clone(),
            parts.lsp.clone(),
            // The queue's own session reads and corrects the reviewers it was handed, and the
            // runner behind these two verbs will address any live session in the daemon.
            if crate::sessions::gatekeeper::is_the_queue_session(&cfg) {
                letibot_tools::builtins::task::TaskReach::OwnChildren
            } else {
                letibot_tools::builtins::task::TaskReach::Anywhere
            },
        )
        .map_err(|e| HarnessError::Setup(format!("registering the todo tool: {e}")))?;
        registry
            .register(Box::new(letibot_tools::builtins::write::Write))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::edit::Edit)))
            .map_err(|e| HarnessError::Setup(format!("registering the write tools: {e}")))?;
        // The reviewer's own door, registered for EVERY session: a role that names a tool the
        // registry does not hold is refused at resolve. What makes it usable is the desk, which
        // answers only a child whose review the queue opened. The id is bound here, not read.
        registry
            .register(Box::new(
                letibot_tools::builtins::verdict::ReviewVerdictTool::new(
                    parts.reviews.clone(),
                    cfg.session_id.clone(),
                ),
            ))
            .map_err(|e| HarnessError::Setup(format!("registering the review verdict: {e}")))?;
        // Standing notes, scoped to the stored workspace and the box-wide dir. Registered for every session; the role decides who seats it.
        // The near-copy gate scores with the box's second tier, so a paraphrased duplicate is refused.
        registry
            .register(Box::new(
                letibot_tools::builtins::notes::NotesTool::new(std::sync::Arc::new(
                    crate::notes_scope::ConfigNotes::of(&cfg),
                ))
                .with_tier(cfg.tier()),
            ))
            .map_err(|e| HarnessError::Setup(format!("registering the notes tool: {e}")))?;
        registry
            // The box-wide `[memory]` under the session's project's own: the
            // project is this session's workspace, settled above, not the daemon's.
            .register(Box::new(
                letibot_tools::builtins::bash::Bash::with_memory(
                    crate::leticode_config::memory_policy(
                        cfg.memory,
                        &cfg.leticode_user,
                        &cfg.workspace,
                        cfg.read_project_config,
                    ),
                )
                .with_foreground(crate::leticode_config::foreground_policy(
                    &cfg.leticode_user,
                    &cfg.workspace,
                    cfg.read_project_config,
                ))
                .with_firecode_route(firecode_route(&cfg)),
            ))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::jobs::JobList)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::jobs::JobOutput)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::jobs::JobExtend)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::jobs::JobWait)))
            .and_then(|_| {
                registry.register(Box::new(
                    letibot_tools::builtins::jobs::JobKill::with_tasks(subagent_runner.clone()),
                ))
            })
            .and_then(|_| {
                // The monitor tool, recorded - each declaration's own arguments go to the
                // store, so a daemon restart can make the same call again (`crate::restart`).
                registry.register(Box::new(crate::restart::RecordingMonitor {
                    inner: letibot_tools::builtins::monitor::Monitor,
                    path: cfg.store.clone(),
                    session_id: cfg.session_id.clone(),
                    instance: session_registry.instance().to_string(),
                }))
            })
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::pkill::Pkill)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::ps::Ps)))
            .and_then(|_| {
                // The session id is how `screen tab` finds the tab the operator allowed it.
                registry.register(Box::new(
                    letibot_tools::builtins::screen::ScreenTool::new()
                        .with_session(cfg.session_id.clone()),
                ))
            })
            .map_err(|e| HarnessError::Setup(format!("registering the exec tools: {e}")))?;
        // Work across hosts: `fanout` and `scenario`, with the daemon's own process book behind
        // them, so a branch on ANOTHER machine is a supervised process a restart finds. A session
        // with no store gets the tool that works and writes nothing.
        let process_book: std::sync::Arc<dyn letibot_tools::fleet::ProcessBook> = match &cfg.store {
            Some(path) => std::sync::Arc::new(crate::supervise::StoreBook::new(
                path.clone(),
                cfg.session_id.clone(),
            )),
            None => std::sync::Arc::new(letibot_tools::fleet::NoBook),
        };
        // Resource leases: the block of a shared host's ports this session may use, and the prefix
        // its containers are named under, behind the same store. With no store the tool refuses a
        // `ready: {"port": "lease"}` with a sentence rather than starting against port 0.
        let leases = cfg
            .store
            .as_ref()
            .map(|path| std::sync::Arc::new(crate::leases::blocks(path, &cfg.session_id)));
        registry
            .register(Box::new({
                let mut tool =
                    letibot_tools::builtins::fanout::Fanout::new().with_book(process_book.clone());
                if let Some(leases) = &leases {
                    tool = tool.with_leases(std::sync::Arc::clone(leases));
                }
                tool
            }))
            .and_then(|_| {
                let mut tool =
                    letibot_tools::builtins::scenario::Scenario::new().with_book(process_book);
                if let Some(leases) = &leases {
                    tool = tool.with_leases(std::sync::Arc::clone(leases));
                }
                registry.register(Box::new(tool))
            })
            .map_err(|e| HarnessError::Setup(format!("registering the fleet tools: {e}")))?;
        let mut registry = letibot_tools::external_tools(registry, &external).map_err(|e| {
            HarnessError::Setup(format!("registering the outside-world tools: {e}"))
        })?;
        intent_tools::register_into(&mut registry, &intent_wiring)
            .map_err(|e| HarnessError::Setup(format!("registering the intent tools: {e}")))?;
        if let Some(t) = extra_tool {
            registry
                .register(t)
                .map_err(|e| HarnessError::Setup(format!("registering an extra tool: {e}")))?;
        }
        // The `flowy` door is `Sessions`' to seat, and a harness opened without it gets the door
        // with nothing behind it: the tool says "no seat". The `harness` view is read-only and is
        // seated for every session, a subagent included, because it is the one most likely to guess.
        let disclosure_slot: crate::facts::DisclosureSlot = Default::default();
        if registry.get("harness").is_none() {
            let facts = std::sync::Arc::new(crate::facts::DaemonFacts::new(
                hub.clone(),
                disclosure_slot.clone(),
            ));
            registry
                .register(Box::new(
                    letibot_tools::builtins::harness_view::HarnessView::new(facts),
                ))
                .map_err(|e| HarnessError::Setup(format!("registering the harness view: {e}")))?;
        }
        // The session can read its own conversation, including the parts compaction replaced: after
        // a compaction the rows are not in the context at all and the store is the only copy. A tool
        // that is absent reads as a capability this build lacks, so it refuses by name instead.
        if registry.get("transcript").is_none() {
            let src: Arc<dyn letibot_tools::builtins::transcript::TranscriptSource> =
                match cfg.store.as_deref() {
                    Some(path) => {
                        match crate::transcript_source::StoreTranscripts::open(
                            path,
                            cfg.session_id.clone(),
                        ) {
                            Ok(s) => Arc::new(s),
                            // A store the daemon writes to but this reader cannot
                            // open is worth saying out loud once; the tool then
                            // refuses by name rather than reporting no history.
                            Err(e) => {
                                hub.warn("transcript_store", e);
                                Arc::new(letibot_tools::builtins::transcript::NoTranscript)
                            }
                        }
                    }
                    None => Arc::new(letibot_tools::builtins::transcript::NoTranscript),
                };
            let digest: Arc<dyn letibot_tools::builtins::digest::DigestRunner> = Arc::new(
                letibot_tools::builtins::digest::SubagentDigest::new(digest_runner),
            );
            // The gate's own record, by the same argument and from the same file: a model must not
            // derive the storage, its format, or where it lives.
            let decisions: Arc<dyn letibot_tools::builtins::decisions::DecisionSource> =
                match cfg.store.as_deref() {
                    Some(path) => match crate::decision_source::StoreDecisions::open(
                        path,
                        cfg.session_id.clone(),
                    ) {
                        Ok(s) => Arc::new(s),
                        Err(e) => {
                            hub.warn("decision_corpus", e);
                            Arc::new(letibot_tools::builtins::decisions::NoDecisions)
                        }
                    },
                    None => Arc::new(letibot_tools::builtins::decisions::NoDecisions),
                };
            registry
                .register(Box::new(
                    letibot_tools::builtins::transcript::TranscriptTool::new(src.clone()),
                ))
                .and_then(|_| {
                    registry.register(Box::new(letibot_tools::builtins::digest::DigestTool::new(
                        src, digest,
                    )))
                })
                .and_then(|_| {
                    registry.register(Box::new(
                        letibot_tools::builtins::decisions::DecisionsTool::new(decisions),
                    ))
                })
                .map_err(|e| {
                    HarnessError::Setup(format!("registering the transcript tools: {e}"))
                })?;
        }
        if cfg.parent_session_id.is_none()
            && cfg.seat != Seat::Runner
            && registry.get("flowy").is_none()
        {
            let (door, _slot) = letibot_flowy::Flowy::unattached();
            registry
                .register(Box::new(door))
                .map_err(|e| HarnessError::Setup(format!("registering the flowy door: {e}")))?;
        }

        // The role comes from the SESSION when it has one: `cfg.seat` already IS the session's role,
        // taken from the store at the top of this function where it still had time to reach the
        // backend. One read, one seat, where two separate resolutions could disagree.
        let role = role_for(&cfg);
        let registry = registry
            .resolve_role(&role)
            .map_err(|e| {
                HarnessError::Setup(format!("seating the `{}` role: {e}", cfg.seat.as_str()))
            })?
            // The downgrade, applied to what is SEATED: a denied class's tools leave
            // the prompt, so the model is not told it has what the gate would refuse.
            .without_access(&cfg.downgrade.deny);
        // And a skill that declares `allowed-tools` may only NARROW this seat: read over the skills this session was
        // STARTED with, since a session's tool list is its prompt and message zero is frozen.
        let ceiling = parts.skills.tool_ceiling(&cfg.skills);
        let mut registry = match &ceiling {
            Some(allowed) => {
                let before: std::collections::BTreeSet<String> =
                    registry.schemas().iter().map(|s| s.name.clone()).collect();
                let narrowed = registry.only_tools(allowed);
                let gone: Vec<&String> = before.difference(allowed).collect();
                if !gone.is_empty() {
                    let names: Vec<String> = gone.iter().map(|n| format!("`{n}`")).collect();
                    notes.push(format!(
                        "a skill this session was started with declares `allowed-tools`, so {} \
                         {} not seated here — a skill may narrow a session and never widen it",
                        names.join(", "),
                        if names.len() == 1 { "is" } else { "are" },
                    ));
                }
                narrowed
            }
            None => registry,
        };
        // And the tools this session's PLUGIN LAYERS declare: after the role, since a layer's tool is a seat the
        // project added and `resolve_role` would refuse a role naming it. Only a layer IN FORCE counts, not a built-in's
        // own table, and a tool that cannot be seated says why once in a `plugin_layer` warning.
        let mut not_seated: Vec<String> = Vec::new();
        let declared = crate::plugins::declared_tools(&cfg.session_id, &cfg.workspace);
        let mut plugin_tools = 0usize;
        for decl in declared {
            let name = decl.tool.name.clone();
            if registry.get(&name).is_some() {
                not_seated.push(format!(
                    "`{name}` (layer `{}`) is already the name of a tool this seat has",
                    decl.layer
                ));
                continue;
            }
            let access = crate::plugins::tool::PluginTool::access(&decl.capabilities);
            if cfg.downgrade.deny.contains(&access) {
                not_seated.push(format!(
                    "`{name}` (layer `{}`) is a {access:?} tool and this session's downgrade \
                     denies that class",
                    decl.layer
                ));
                continue;
            }
            if plugin_tools >= crate::plugins::tool::MAX_TOOLS {
                not_seated.push(format!(
                    "`{name}` (layer `{}`) is past the {} tools a session seats from its layers",
                    decl.layer,
                    crate::plugins::tool::MAX_TOOLS
                ));
                continue;
            }
            // Somebody else's prose, so `register_foreign`: the lint's findings are recorded on the registry rather
            // than refusing the tool, since a layer's description is a file a person wrote and `plugin test` is where
            // the wording is reported back to the model that can fix it.
            registry
                .register_foreign(Box::new(crate::plugins::tool::PluginTool::new(
                    decl,
                    cfg.session_id.clone(),
                    cfg.workspace.clone(),
                    plugin_slot.clone(),
                    hub.clone(),
                )))
                .map_err(|e| {
                    HarnessError::Setup(format!("seating the plugin tool `{name}`: {e}"))
                })?;
            plugin_tools += 1;
        }
        if !not_seated.is_empty() {
            hub.warn(
                "plugin_layer",
                format!(
                    "a layer declares a tool this session did not seat: {}. The layer is still \
                     loaded; `plugin {{\"action\":\"stack\"}}` lists what is in force.",
                    not_seated.join("; ")
                ),
            );
        }
        let schemas = registry.schemas();
        let seated: Vec<String> = schemas.iter().map(|s| s.name.clone()).collect();
        // What the prompt is composed from: the tools actually seated, after the role and the
        // downgrade, and the scratch directory the backend actually reaches. Read here, where both
        // are facts, and not off the role's intention before open.
        let seat_facts = crate::config::SeatFacts {
            tools: seated.iter().cloned().collect(),
            scratch: cfg
                .session_scratch
                .as_ref()
                .map(|p| p.display().to_string())
                .or_else(|| backend.scratch_dir()),
            subagent: cfg.parent_session_id.is_some(),
            // A linked worktree's `.git` is a file naming the main repository's gitdir.
            worktree: cfg.workspace.join(".git").is_file(),
            // What a command here can reach, measured rather than assumed: `None` for a backend
            // with no view, so an unconfined session's prompt is the bytes it always was.
            view: view_brief(backend_boundary.as_ref())
                .or_else(|| backend_whole_host.then(whole_host_brief)),
        };

        use letibot_tools::schema::Access;
        let has_write_tools = schemas.iter().any(|s| s.access == Access::Write);
        let has_exec_tools = schemas.iter().any(|s| s.access == Access::Exec);
        let has_network_tools = schemas.iter().any(|s| s.access == Access::Network);
        // The gate is reachable from any class that is not unattended, not from `write` alone: a
        // planner with `say` and no writes still has a code path to a question, and asking only
        // about writes would call that session unattended when it is not.
        let gated = has_write_tools || has_exec_tools || has_network_tools;

        // The refusal that has to come before the banner: a seat that can reach the gate with
        // nobody behind it would start, print, and refuse every call with `not_run` one turn later.
        // The check is on the adjudicator's own answer, so both spellings of nobody are one check.
        if gated
            && let Some(adj) = &adjudicator
            && adj.describe().starts_with("none")
        {
            let gated_tools: Vec<&str> = schemas
                .iter()
                .filter(|s| !s.access.is_unattended())
                .map(|s| s.name.as_str())
                .collect();
            return Err(HarnessError::Setup(format!(
                "the `{}` role seats {} tool(s) that must be adjudicated — {} — and the \
                 adjudicator attached is `{}`.\n\nThis refuses to open rather than \
                 opening and refusing every call. Failing closed is right and it is not \
                 a substitute for saying so before anything runs: a session in that \
                 state prints a banner, takes a prompt, and returns `not_run` — nobody \
                 decided — one turn later, having meanwhile told the model in its stable \
                 prefix that it has tools it cannot use.\n\nAttach an adjudicator, or \
                 seat `--role orchestrator`, which does not carry these tools at all.",
                cfg.seat.as_str(),
                gated_tools.len(),
                gated_tools.join(", "),
                adj.describe()
            )));
        }

        // The point's prerequisites, checked before anything opens: `Mode::check` refuses by name
        // and hands back no weaker point, because an operator who asked for one point and silently
        // got another finds out one wasted turn later. What is available is READ, never assumed.
        let supplies: Option<(Vec<letibot_tools::mode::Prereq>, letibot_tools::mode::Seats)> = {
            use letibot_tools::mode::Prereq;
            let mut have: Vec<Prereq> = Vec::new();
            if backend_writable {
                have.push(Prereq::WritableBackend);
            }
            // A reachable adjudicator, by its own account. `HeadAdjudicator::describe`
            // says "none attached right now" when no head can answer, and that string
            // is the honest reading of *reachable* rather than *attached*.
            let reachable = adjudicator
                .as_ref()
                .map(|a| {
                    let d = a.describe();
                    !d.starts_with("none") && !d.contains("none attached")
                })
                // No adjudicator was handed in, so the harness builds one from
                // `cfg.adjudicator`, and both values it can build reach a person.
                .unwrap_or(true);
            if reachable {
                have.push(Prereq::ReachableAdjudicator);
            }
            if has_exec_tools && backend_confined {
                have.push(Prereq::Confinement);
            }
            // The guard model, by the same fact the advisor is built from: `cfg.oracle` is `Some` when an endpoint is
            // known. Without this `automode` cannot open however the box is configured. Reachability
            // is not tested here, since a probe at open would wait on another box.
            if cfg.oracle.is_some() {
                have.push(Prereq::Oracle);
            }
            // What the role actually seated, read off the resolved schemas rather than off its
            // name: a read-only seat needs no writable backend however strict the point is, and
            // requiring one unconditionally stopped every read-only session from opening.
            let seats = letibot_tools::mode::Seats {
                write: has_write_tools,
                exec: has_exec_tools,
                network: has_network_tools,
            };
            // A child's point is its parent's, clamped by this seat: `seats` and `have` do not
            // exist any earlier, which is why the clamp is not beside the mode-store lookup. It is
            // a clamp and not a refusal because a child's point is not a choice anybody makes.
            if cfg.parent_session_id.is_some() {
                let inherited = cfg.mode.inherited_by(&have, seats, cfg.seat.as_str());
                if let Some(why) = inherited.why() {
                    cfg.mode = inherited.mode();
                    // The row says where the point came from AND that it was clamped - a
                    // child silently at a different point than its parent is the state
                    // that made this a bug hunt in the first place.
                    mode_source = child_mode_source(Some(why));
                    // And the same sentence on the open notes, which is where a reader
                    // looks when the settings row is not in front of them.
                    notes.push(why.to_string());
                }
            }
            if let Err(why) = cfg.mode.check(&have, seats) {
                return Err(HarnessError::Setup(why));
            }
            // Kept for `/mode` mid-session: what this session CAN supply is fixed at open, and a
            // point the operator moves to later is checked against the same list.
            Some((have, seats))
        };

        let trail = Arc::new(TrailMirror::default());
        // What this session will hold, and the ceiling it states for itself - see
        // `crate::footprint` for the arithmetic. Built here, before the struct literal, because
        // the ceiling is the CONFIG's window and the literal is a list of values.
        let footprint_gauges =
            crate::footprint::Gauges::new(crate::footprint::ceiling_for(cfg.context_window));
        let injected: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let monitor_cursor = Arc::new(AtomicUsize::new(0));
        // What this session's questions and their answers are recorded in, created before the
        // gate: the questioner that fills it is built with the gate and the harness that reads it
        // after, and it is the same `Arc` the `HeadQuestioner` is handed.
        let asked: crate::answers::AskLog = Default::default();

        // Where a gate admission nobody was asked about waits to be said. Created before the gate
        // for the reason `asked` is: the gate runs inside the runtime, where the harness is not in
        // hand, so the sentence is handed over rather than spoken.
        let gate_admissions: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();

        // The gate, built here so the three seams cannot be skipped. See
        // `open_with`'s docs for why this is not a parameter.
        let (gate, trail_installed, denials_surfaced): (Box<dyn Gate>, bool, bool) = match (
            adjudicator,
            gated,
        ) {
            // Nothing can reach it, and `NoBoundary` is what every session that exists today has.
            // A caller who attached an adjudicator to a read-only seat meant it, so that falls
            // through below and gets a real gate that nothing ever calls.
            (None, false) => (Box::new(NoBoundary), false, false),
            (adj, _) => {
                let adj: Box<dyn Adjudicator> = match (adj, cfg.adjudicator) {
                    (Some(a), _) => a,
                    // The answer path, installed as one act: the sink has to be on the hub before
                    // the adjudicator can be asked anything, since it posts to that hub and then
                    // waits. Separating the two lines is a session that asks nobody can answer.
                    (None, AdjudicatorChoice::Head) => {
                        let answers = Arc::new(crate::answers::Answers::new());
                        hub.set_answer_sink(answers.clone());
                        // And the tree can reach it: a subagent's card is posted on a ROOT's hub
                        // and settled through that sink, so this is what makes a child's ask
                        // answerable. Recorded with the sink, because the two are one act.
                        *head_answers_slot.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(answers.clone());
                        // And `ask_user_question` reaches the same head, in the same act: one more
                        // slot filled elsewhere would be a second path to a person.
                        *question_head.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(
                            crate::answers::HeadQuestioner::new(hub.clone(), answers.clone())
                                .with_ask_log(asked.clone())
                                .with_workspace(cfg.workspace.clone()),
                        ));
                        Box::new(crate::answers::HeadAdjudicator::new(hub.clone(), answers))
                    }
                    (None, AdjudicatorChoice::Console) => {
                        Box::new(letibot_tools::ConsoleAdjudicator::stdio(cfg.owner.clone()))
                    }
                    (None, AdjudicatorChoice::Model) => {
                        // The model decides; when it cannot, the person does: an oracle timeout
                        // is a hand-off, not a refusal. The answer sink is installed as one act
                        // with the adjudicator that uses it, the pair rule the Head arm spells out.
                        let model =
                            model_adjudicator(&cfg, "`--adjudicator model`", Some(hub.clone()))?;
                        let answers = Arc::new(crate::answers::Answers::new());
                        hub.set_answer_sink(answers.clone());
                        // The tree's route to a person, recorded with the sink - a subagent's
                        // card reaches the same answer path the root's own escalation does.
                        *head_answers_slot.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(answers.clone());
                        // And a question's route, in the same act: under `--adjudicator model` the
                        // head is still where a question goes. A question is not an adjudication,
                        // so it is never handed to the oracle first.
                        *question_head.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(
                            crate::answers::HeadQuestioner::new(hub.clone(), answers.clone())
                                .with_ask_log(asked.clone())
                                .with_workspace(cfg.workspace.clone()),
                        ));
                        Box::new(crate::answers::EscalateOnTimeout::new(
                            std::sync::Arc::from(model),
                            std::sync::Arc::new(crate::answers::HeadAdjudicator::new(
                                hub.clone(),
                                answers,
                            )),
                        ))
                    }
                };
                // The guard model, attached whenever there is one: built from `--oracle` and NOT
                // from the mode, which is what makes `/supervise` a toggle. `None` is not a
                // failure; a session without one runs as before and `/supervise` refuses by name.
                let advisor: Option<std::sync::Arc<dyn Adjudicator>> = match &cfg.oracle {
                    Some(_) => {
                        Some(model_adjudicator(&cfg, "`--oracle`", Some(hub.clone()))?.into())
                    }
                    None => None,
                };
                let trail_for_gate = trail.clone();
                let g = AdjudicatedGate::new(adj)
                    .with_identity(cfg.session_id.clone(), cfg.owner.clone())
                    // Where this project sits: without this the gate is at `UNSEEN_PROJECT`,
                    // which is the fail-closed default and not what the operator recorded.
                    .with_mode(cfg.mode)
                    // exec follows the mode (so `allow-all` admits `bash`) rather than the rule
                    // that exec always asks; off for the confined seats, which keep the rule.
                    .with_exec_follows_mode(cfg.unconfined)
                    // opencode's `permission` config, so the allow/deny/ask rules govern before
                    // the mode; a subagent inherits them.
                    .with_permission(downgraded_ruleset(
                        &cfg.permission,
                        &cfg.downgrade,
                        &schemas,
                    ))
                    // *Always allow* is written to the operator's file, so it is a
                    // preapproval every later daemon starts with.
                    .with_permission_sink(std::sync::Arc::new(|rule| {
                        let path = letibot_tools::permission::file_path()
                            .ok_or_else(|| "no $HOME, so no permission file".to_string())?;
                        letibot_tools::permission::append_to_file(&path, rule)
                    }))
                    // Layer A needs to know where it is standing: undeclared means
                    // `ShellTrust::Unknown`, under which a bare command name is unresolved and the
                    // call is `not_run`, which is the fail-closed direction.
                    .with_surroundings(surroundings)
                    // Without this the trail is `NotCollected`, which is not the same fact as an
                    // empty trail.
                    .with_trail_source(move |_call: &GateCall<'_>| trail_for_gate.trail())
                    // Where the tools actually run: a card tells the truth about the boundary, and
                    // a host with none asks about the project's own code. Read off the backend
                    // above, never probed here - the gate takes the fact it is handed.
                    .with_boundary(backend_boundary, backend_boundary_refusal)
                    // What the agent says it is doing, so the guard can connect the operator's
                    // words to a call that does not repeat them. Outside the trail's numbering, so
                    // it can never be what an `ALLOW <n>` cites.
                    .with_agent_claim({
                        let t = trail.clone();
                        move || t.claim()
                    })
                    // Without this a denial reaches the model and stops there.
                    .with_denial_sink(Box::new(HubDenials::new(hub.clone())))
                    // And the other direction: a call the gate admitted without asking
                    // is told, not only recorded - see [`Harness::flush_gate_admissions`].
                    .with_admission_sink(Box::new(HubAdmissions::new(gate_admissions.clone())))
                    // The `adjudicate_` floor: the one seam that can raise a tier after layer A, and
                    // a slot for the same reason `on_tool_call`'s is one - the daemon fills it when it
                    // opens this session's layers, so a session nobody serves decides as it did.
                    .with_floor_hook(std::sync::Arc::new(crate::plugins::FloorHooks::new(
                        plugin_slot.clone(),
                        hub.clone(),
                    )))
                    // What this tree was already granted into its view, so a second ask for a
                    // path a grant covers is not a question at all.
                    .with_view_grants(match &cfg.view_grants {
                        Some(g) => Arc::clone(g),
                        None => Arc::new(letibot_tools::view::ViewGrants::new()),
                    })
                    // And whose call an ask is, when it is not this session's own: a subagent's
                    // card is drawn at the tree's root, where nobody sees which child asked.
                    .with_asker(
                        cfg.parent_session_id
                            .is_some()
                            .then(|| cfg.session_id.clone()),
                    );
                let g = match advisor {
                    // A point whose decider is the MODEL starts supervised: `Decider::Model` is the
                    // whole content of the point, and a session that opened at automode with the
                    // guard attached and never consulted is the banner-says-one-thing state.
                    Some(a) => g.with_advisor(a).start_supervised(
                        cfg.supervise || cfg.mode.decider == letibot_tools::mode::Decider::Model,
                    ),
                    None => g,
                };
                // The corpus: without it the gate's rows die with the daemon, including the
                // labelled ones. It writes through the session store's own connection.
                let g = match corpus_sink.clone() {
                    Some(sink) => g.with_corpus_sink(sink),
                    None => g,
                };
                // The shape cache, warm: it lived on the gate and died with the process, and the operator restarts a
                // session exactly when one has gone wrong. Seeded from the store and re-checked at every lookup; a store
                // that cannot answer is a note rather than a refusal to start.
                let mut g = g;
                if let Some(st) = &store {
                    let ws = cfg.workspace.display().to_string();
                    match st.approved_shapes(&ws) {
                        Ok(rows) => {
                            let n = g.seed_shapes(rows);
                            if n > 0 {
                                notes.push(format!(
                                    "{n} command shape{} you approved in this project before are remembered — they will not be asked again",
                                    if n == 1 { "" } else { "s" }
                                ));
                            }
                        }
                        Err(e) => notes.push(format!(
                            "the shape cache could not be warmed ({e}), so shapes you approved before will be asked again"
                        )),
                    }
                }
                (Box::new(g), true, true)
            }
        };

        // The encoder, attached once for the life of the session, since constructing `IntentSink` is what tells the
        // ledger an encoder exists. And the background-job watcher, which rings the bell so a settlement arrives as an
        // unprompted turn, built whether or not this session can start a process: `task` needs no host.
        let bell = Some(Arc::clone(session_registry.bell()));
        let processes = backend.processes_arc();
        // The heir's slot, filled the moment the host exists: a child that ends hands its survivors
        // here, and a slot filled later is a child whose estate had nowhere to go.
        *heir_slot.lock().unwrap_or_else(|e| e.into_inner()) = processes.clone();
        let job_watch = match &processes {
            Some(h) => JobWatchers::new(h, &hub, bell),
            None => JobWatchers::watching_tasks(&hub, bell),
        };
        // And the session's subagents settle through it too: `task` returns the same `Backgrounded` outcome `bash`
        // does, and a watcher without the runner asked the host about a name it had never heard, spun on it until the
        // session closed and leaked a thread per call.
        let job_watch = Some(job_watch.with_tasks(&subagent_runner));
        // And a child joins its PARENT's set: the watching bookkeeping, the level above, and the ROOT where a
        // permission card is posted. The RING is not among them, since it names the session that OWNS a settlement, and
        // the queue is not joined either: a child drains its own, and one that arrives after its turn is handed up.
        let job_watch = match (&parts.tree_watch, job_watch) {
            (Some(tree), Some(w)) => Some(w.shares_tree(tree)),
            (_, other) => other,
        };
        // And this session's own set is what ITS children will join, filled after the join above so the value a
        // child reads is already the joined one: that is how the ROOT stays the tree's root at every depth.
        if let Some(w) = &job_watch {
            *tree_watch_slot.lock().expect("tree watch") = Some(Arc::clone(w));
            // And the tree table hears it, so the daemon's worker can settle this session's
            // children while this session runs on another thread: a parked child's later turn is
            // the worker's, and its slot and queue are this session's own.
            parts.trees.register(
                &cfg.session_id,
                crate::harness::trees::Tree {
                    watch: Arc::clone(w),
                    slots: tree_slots.clone(),
                },
            );
            // Where a settlement gets written down: the watcher threads cannot be handed this harness's `Store`, so
            // they get its PATH and open a handle onto the process's one connection. This session's own id and not the
            // tree's, since a job a child backgrounded is the child's row.
            if let Some(path) = store.as_ref().and_then(|s| s.path()) {
                w.record_jobs_into(
                    path.to_path_buf(),
                    cfg.session_id.clone(),
                    session_registry.instance(),
                    // Where this session's jobs run, which is what the record's `host` column says, and what decides
                    // how a job's identity is spelled: a pid on this box, a command marker on another machine.
                    match &cfg.placement {
                        letibot_tools::builtins::task::Placement::Remote(name) => {
                            Some(name.clone())
                        }
                        _ => None,
                    },
                );
            }
        }
        // And the jobs a daemon that died was still running: the record is what outlived it, and adopting it puts
        // the job back in this host's table so every reader works as before. A job whose process is already gone
        // settles as `Dead`, never as an exit of zero. A record this backend cannot adopt is left alone.
        if let (Some(procs), Some(store)) = (processes.as_ref(), store.as_ref()) {
            for rec in store.processes_of(&cfg.session_id).unwrap_or_default() {
                if !rec.state.is_open() {
                    continue;
                }
                let adopted = letibot_tools::exec::Adopted {
                    handle: rec.handle.clone(),
                    command: rec.command.clone(),
                    slug: None,
                    cwd: rec.cwd.clone(),
                    pid: rec.pid,
                    identity: rec.identity.clone(),
                    scope: rec.scope.clone(),
                    // A record from a daemon that died: the cgroup it names died with that
                    // process, so the directory has to be made again from the name.
                    scope_dir: None,
                    owner_dir: None,
                };
                if let Ok(id) = procs.adopt(&adopted)
                    && let Some(w) = &job_watch
                {
                    w.watch(id.0.clone());
                }
            }
        }
        let tool_sink = JobWatchSink::new(
            IntentSink::new(intent.clone(), ToolLogSink::new(hub.clone())),
            job_watch.clone(),
        )
        .noting_edits(seat.clone());

        // Read, never asserted: the backend's own answers and the schemas that were actually seated, not which role
        // was requested. Four instances of one defect were paid for to learn that distinction.
        let firecode = seated
            .iter()
            .any(|n| n == "task" || n == "task_start")
            .then(letibot_tools::firecode::installed)
            .flatten();
        let wiring = GateWiring {
            adjudicator: gate.describe(),
            backend_writable,
            has_write_tools,
            external: letibot_tools::ExternalWiring::read(retrieval.as_ref(), &external, &schemas),
            role: role.name.clone(),
            seated,
            has_exec_tools,
            has_network_tools,
            backend: backend_described,
            denials_surfaced,
            trail_installed,
            // The ledger's own answer, not the fact that a line above meant to
            // attach one.
            intent_encoder: intent.encoder(),
            // A wake needs three things and this reads all three: a monitor registry, the `monitor` tool seated, and
            // `Sessions::watch` having armed the waiter, which the daemon stamps and a harness a test drives honestly
            // answers `Unarmed`.
            monitor_wake: crate::config::MonitorWake::Unarmed,
            // Read from the sink, not from the config: `--store` being set is not the same fact as a corpus
            // connection having opened, and the counts come from the table rather than from a count this process kept.
            corpus: corpus_counts,
            // Asked once, here, and only where a subagent can be started - FIRECODE-NOTES
            // item 4: a VM placement's preconditions were found out at spawn, one refusal per
            // attempt. The banner says them up front.
            firecode,
        };

        let spiller = build_spiller(&cfg)?;
        // Where a compaction's evicted bodies go: the handle the runtime's `recall` tool asks through, so ids the map prints are ids a model can open.
        // Built before the runtime because the tool is seated at open.
        let recall = std::sync::Arc::new(letibot_tools::Recall::new(cfg.planning_window()));
        // The question a long tool asks before spending another minute: for the whole of a `digest` or a `job_wait`
        // nothing polls the head, so the hub's answer goes down into the runtime and a tool about to spend real time can
        // find out the operator is waiting and stop.
        let waiting_hub = hub.clone();
        // And the fact that closes the gap: `job_wait` on a job the daemon is already watching returns at once rather
        // than blocking on an answer in flight. Wired to the same watcher set the sink feeds, so there is one fact read
        // in two places. `None` when the backend cannot start processes.
        let delivering = job_watch.clone();
        // And the other half of that fact: a wait, a promoted call or `wake: "now"`
        // tells the same set the result is wanted the moment it ends, so it is not held for the
        // next break. See `crate::delivery`.
        let wanting = job_watch.clone();
        // And where a spawned job's change snapshot goes, read by the same set at the settlement.
        let sweeping = job_watch.clone();
        let runtime = ToolRuntime::new(registry, backend)
            .with_spiller(spiller)
            .with_recall(recall.clone())
            .with_gate(gate)
            // What this session's own commands may see, so a spawn starts from it and the operator
            // can be asked for anything beyond: `task`'s `widen` is one card, and no mode decides it.
            .with_perimeter(std::sync::Arc::new(
                crate::perimeter::of(&cfg).map_err(HarnessError::Setup)?,
            ))
            // And the plugin layer that sees a call before the gate, and the one a `monitor` call can arm a condition
            // from: both backed by the slot the `plugin` tool asks through, so a session nobody fills the slot for runs
            // its calls exactly as it always did.
            .with_call_hook(std::sync::Arc::new(crate::plugins::CallHooks::new(
                plugin_slot.clone(),
                hub.clone(),
            )))
            // And the layer that sees an output before the spill budgets it: the digest is what the
            // model's context pays for, so it rides the same slot as the call hook.
            .with_digest_hook(std::sync::Arc::new(crate::plugins::DigestHooks::new(
                plugin_slot.clone(),
                hub.clone(),
            )))
            // A one-shot's session tells its tools so, which is what makes `bash` refuse
            // `background: true`: there is no later head and no daemon left watching when that
            // job's result arrives. See `InvokeCtx::one_shot`.
            .with_one_shot(cfg.one_shot)
            // Who wrote a layer's bytes, recorded at the write: a `write` or an `edit` landing a `plugin.toml` appends
            // to the trust book, so who put this here is a fact on disk rather than a guess from a path.
            .with_provenance(std::sync::Arc::new(crate::plugins::Provenance::new(
                cfg.session_id.clone(),
            )))
            .with_operator_waiting(std::sync::Arc::new(move || waiting_hub.has_queued_prompt()))
            .with_completion_delivered(std::sync::Arc::new(move |job: &str| {
                delivering.as_ref().is_some_and(|w| w.delivering(job))
            }))
            .with_completion_wanted(std::sync::Arc::new(
                move |job: &str, why: letibot_tools::runtime::WantedNow| {
                    if let Some(w) = &wanting {
                        w.want_now(job, why);
                    }
                },
            ))
            // The snapshot a background job's `bash` call takes at the spawn, handed to the set that
            // settles it: only the watcher sees both ends of a job, so only it can read the change
            // back (R161). `None` when the backend cannot start processes, and then nothing is kept.
            .with_job_sweeps(sweeping.as_ref().map(|w| w.sweeps()))
            // The operator's own run can be ANSWERED: the object is installed on the registry here because this is
            // the half that has the pipe, the write end of the operator's stdin having come out of the job the tool
            // spawned. Keyed by session, since a pipe belongs to one session's run.
            .with_operator_runs({
                let prompts = crate::prompt::Prompts::new(&cfg.session_id, hub.clone());
                session_registry.set_prompt(&cfg.session_id, prompts.clone());
                let runs = prompts.clone();
                std::sync::Arc::new(
                    move |what: letibot_tools::runtime::OperatorRun<'_>| match what {
                        letibot_tools::runtime::OperatorRun::Answerable {
                            job,
                            command,
                            stdin,
                        } => runs.opened(job, command, stdin),
                        letibot_tools::runtime::OperatorRun::Waiting { job, question } => {
                            runs.asking(job, question)
                        }
                        letibot_tools::runtime::OperatorRun::Ended { job } => runs.ended(job),
                        letibot_tools::runtime::OperatorRun::Unreadable { job, quiet } => {
                            runs.unreadable(job, quiet)
                        }
                    },
                )
            });

        // And the plugin conditions this session's monitors may arm, installed on the process host's own `Monitors`,
        // because that is what the `monitor` tool asks for everything else. The runtime is shared from here on, so the
        // monitors hold the same gate the session's calls do: two gates would be two answers to what may run.
        let runtime = std::sync::Arc::new(runtime);
        if let Some(processes) = runtime.backend.processes()
            && let Some(monitors) = processes.monitors()
        {
            let look_gate: std::sync::Arc<dyn crate::plugins::conditions::Unasked> =
                runtime.clone();
            monitors.set_plugin_conditions(crate::plugins::conditions::HostConditions::new(
                plugin_slot.clone(),
                hub.clone(),
                Some(look_gate),
            ));
        }

        let mut engine = TurnEngine::new(
            parts.vocab.clone(),
            parts.wiring.renderer.clone(),
            parts.wiring.parser.clone(),
            cfg.endpoint.clone(),
            // A llama.cpp server we run: token ids in, per-stage cache accounting,
            // a wall-clock meter, and a structural prefix guarantee - so the
            // prefix post-flight assertion is allowed to run rather than being skipped.
            letibot_backend::BackendCaps::OWN_SERVER,
            cfg.model.clone(),
            cfg.sampling.clone(),
        )
        .map_err(|e| {
            HarnessError::Setup(format!(
                "the dialect does not fit this vocabulary: {e}. \
                 Every control token and stop literal must resolve to exactly one \
                 vocab entry; a mismatch here is a model/dialect pairing error, and \
                 it is raised now because at runtime it is silent."
            ))
        })?;
        // And this endpoint's media marker - see `engine_for`, whose construction this duplicates.
        engine.media_marker = cfg.media_marker.clone();

        // Standing notes are composed here at open, not only at a base rebuild, so a fresh session's message zero carries them, plus the skills it started with.
        // A pin that answers to nothing is the operator's typo: said once, with the name typed.
        let unknown = parts.skills.unknown_pins();
        if !unknown.is_empty() {
            hub.warn(
                SKILLS_PIN,
                format!(
                    "a pinned skill nothing on this box answers to: {}. `[skills] pin` takes the \
                     names `skill list` reports; the rest of the pins are in force.",
                    unknown
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        let mute = cfg.leticode.queue_mute();
        if !mute.is_empty() {
            let keys = crate::mergequeue::repositories_of_workspace(&cfg.workspace);
            let unknown = crate::mergequeue::unknown_mutes(&keys, mute);
            if !unknown.is_empty() {
                hub.warn(
                    QUEUE_MUTE,
                    format!(
                        "`[queue] mute` names no repository of this workspace: {}. A name is a \
                         repository's directory name or key; the rest of the mutes are in force.",
                        unknown
                            .iter()
                            .map(|n| format!("`{n}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }
        let declared = crate::standing_notes::preloaded(parts.skills.as_ref(), &cfg.skills);
        // Through the backend the session's tools use: a placed session's instruction files are on
        // that host, and this is the first prompt they have to reach.
        let notes_files = crate::standing_notes::Reader::new(&*runtime.backend, &cfg.workspace)
            .with_notes(cfg.project_notes());
        let standing = crate::standing_notes::section_with_skills(
            &notes_files,
            &cfg.global_notes(),
            &cfg.box_dirs,
            parts.vocab.as_ref(),
            Some(parts.skills.as_ref()),
            declared.as_deref(),
        );
        // And the base prompt is composed here when the session asked for it: for the tools just seated, with the
        // notes, the session's own paragraph and its fabric block in that order. A config without it keeps the `system`
        // it was given.
        match cfg.seat_prompt.clone() {
            Some(prompt) => cfg.compose_for_seat(&seat_facts, &prompt, standing.as_deref()),
            None => cfg.system = crate::standing_notes::replace(&cfg.system, standing.as_deref()),
        }
        cfg.seat_facts = Some(seat_facts);
        let prefix = StablePrefix {
            system: cfg.system.clone(),
            tools_json: parts.wiring.tools_json(&schemas),
        };
        let dialect_sha = hex32(&parts.wiring.spec().template_sha);

        let (
            transcript_id,
            session,
            persisted,
            resume,
            prefix,
            prefix_id,
            stale,
            stale_for_model,
            stale_facts,
        ) = Self::open_the_conversation(
            &store,
            &stored,
            &mut engine,
            parts,
            &cfg,
            &prefix,
            &dialect_sha,
            &mut notes,
        )?;
        // The sentence is left in the registry at the END of `open`, after the stored
        // conversation has been republished - see the comment there.

        // The window this session plans its compaction against, written down: otherwise the number was knowable
        // only at the moment it was used, and a child that ran past its wall left nothing saying which wall it had.
        // Written for a root and a child alike. After the match above, because the row it updates is created there.
        if let Some(store) = &store
            && let Err(e) = store.set_window(&cfg.session_id, cfg.context_window)
        {
            // A bookkeeping write failing is not a reason to refuse a session, and it is
            // not silent either: `notes` is what the operator reads at startup, and this is
            // the one place a reader would look for why the store cannot answer the question.
            notes.push(format!(
                "the window this session plans against could not be written to the store \
                 (`{e}`). It is in force for this run; after a restart nothing on disk says \
                 which window it was."
            ));
        }

        // The trail is seeded from what was restored, and it says what it lost: a resumed session's `User` rows are
        // indistinguishable, since the operator's prompts, a head's steering and this harness's own notices are all
        // the same item. Seeding them as the operator's is the permissive direction for authorisation, so it is a note.
        let mut resume = resume;
        if resume.is_some() {
            trail.seed(&session.items);
            let user_rows = session
                .items
                .iter()
                .filter(|i| matches!(i, TranscriptItem::User { .. }))
                .count();
            if let Some(r) = resume.as_mut()
                && gated
            {
                r.notes.push(format!(
                    "the authorisation trail was seeded from {user_rows} restored user \
                     row(s), all read as the OPERATOR's words and none with a clock. The \
                     store does not record which user rows this harness injected — a \
                     steering message and a §5.7 notice are the same shape as a prompt — \
                     so a rebuilt trail is the permissive reading. Everything said from \
                     now on is recorded with its real speaker and its real distance."
                ));
            }
        }

        // The standing choice is resolved by the DAEMON, not here: reading `providers.toml` made opening a harness
        // depend on a file outside its config, and every test inherited the operator's live choice - two `compact` tests
        // billed four turns to a real provider. A key that does not resolve is asked for; any other failure refuses.
        let mut key_wanted = None;
        // What this session's provider has learned about pictures, restored from its own row: one
        // session's refusal must not re-render another session's history, and a restart must not
        // earn the same 400 again. Marks this build cannot parse are none, which costs one refusal.
        let picture_marks = std::sync::Arc::new(std::sync::Mutex::new(
            store
                .as_ref()
                .and_then(|s| s.session_marks(&cfg.session_id).ok().flatten())
                .and_then(|json| letibot_provider::marks::Marks::from_json(&json))
                .unwrap_or_default(),
        ));
        let marks_for_provider = picture_marks.clone();
        let provider: Option<Box<dyn letibot_backend::MessagesBackend>> = match &cfg.provider {
            None => None,
            Some(pc) => match build_provider(
                pc,
                &cfg.sampling,
                provider_notes(&hub),
                cfg.deadlines_for(Some(pc)),
            ) {
                Ok(p) => Some(scoped_provider(
                    p,
                    marks_for_provider,
                    Some(persist_marks(cfg.store.clone(), cfg.session_id.clone())),
                )),
                Err(_) if key_is_missing(pc) => {
                    key_wanted = Some(pc.clone());
                    None
                }
                Err(e) => return Err(HarnessError::Setup(e)),
            },
        };
        // The ledger-to-provider pair survives a restart because it is already in the store: `context_tokens` is the
        // provider's count for the last prompt and `context_ledger` this box's count for the SAME prompt, so the pair is
        // recovered rather than recomputed. A row written before the column existed is refused rather than guessed at.
        let mut wall_anchor = None;
        if cfg.provider.is_some()
            && cfg.ledger_scale.is_none()
            && let Some(store) = &store
            && let Ok(Some(row)) = store.session(&cfg.session_id)
            && let Some((ledger, provider)) =
                recovered_scale(row.context_tokens, row.context_ledger, session.ledger.len())
        {
            cfg.ledger_scale = Some((ledger, provider));
            wall_anchor = Some(WallAnchor {
                transcript: session.transcript_id.clone(),
                ledger,
                provider,
                measured: true,
                scale: sent_scale(
                    &session.ledger,
                    &session.items,
                    ledger,
                    provider,
                    true,
                    !tool_names(&prefix.tools_json).is_empty(),
                ),
            });
        }
        // Fill the harness view's slot, now that there is something to disclose: the tool was registered before the
        // gate and the backend existed, to be in the prompt, and this is the first point where its disclosures exist.
        *disclosure_slot.lock().unwrap_or_else(|e| e.into_inner()) = cfg
            .disclosures(&wiring)
            .into_iter()
            .map(|d| (d.subject, d.state, d.detail))
            .collect();
        // What `/models local` returns to, captured from the pre-move `cfg` and `parts`: the daemon's own server,
        // vocabulary and dialect. A switch to other weights replaces the engine wholesale, so back to local is a re-seat
        // of this pair and a re-render, not a field flip.
        let own_server = OwnServer {
            endpoint: cfg.endpoint.clone(),
            model: cfg.model.clone(),
            vocab_gguf: cfg.vocab_gguf.clone(),
            dialect: cfg.dialect,
            media_marker: cfg.media_marker.clone(),
            sampling: cfg.sampling.clone(),
            vocab: parts.vocab.clone(),
            wiring: parts.wiring.clone(),
        };
        let mut h = Harness {
            mode_store: parts.mode_store.clone(),
            // The claim on this session's transcript, held for the harness's life - see
            // `crate::writer`. Dropped with the harness, which is what gives the session back to
            // whoever opens it next; never read, and named `_` because of that.
            _writer: writer,
            deliverable: None,
            // no prompt yet: the first `TurnStarted` after a prompt stamps it
            turn_began_ms: None,
            prompt_in_flight: Arc::new(AtomicBool::new(false)),
            turn_open: false,
            session_registry: session_registry.clone(),
            tasks: parts.tasks.clone(),
            mode_source,
            wiring,
            cfg,
            engine,
            note_vocab: parts.vocab.clone(),
            skills: parts.skills.clone(),
            session,
            runtime,
            hub,
            store,
            corpus: corpus_sink,
            transcript_id,
            prefix,
            prefix_id,
            persisted,
            system_updates: 0,
            last_turn_id: String::new(),
            resumed: resume,
            lifecycle_facts: stale_facts.clone(),
            compacting: false,
            folded: false,
            swept_calls: Default::default(),
            wall_anchor,
            render: parts.wiring.clone(),
            supplies,
            open_notes: notes,
            new_title: None,
            bang_seq: 0,
            // Attached by `Sessions` right after open - see the field. `None` until
            // then is unreachable in a daemon, and harmless in a bare-harness test.
            bang: None,
            trail,
            footprint_gauges,
            asked,
            gate_admissions,
            todos: todo_board,
            todos_version: 0,
            asks_said: Vec::new(),
            nag_choice: None,
            nag_clock: Default::default(),
            operator_added_at: None,
            nag_next: None,
            stale_told: None,
            unsealed_fork: None,
            meter: Default::default(),
            intent,
            injected,
            handed_back: Default::default(),
            stopped: Default::default(),
            monitors,
            monitor_cursor,
            room,
            room_late,
            room_connector,
            seat,
            tool_sink,
            job_watch,
            // The same `Arc` the tool registry has, cloned before it was moved in - see the
            // field: an interrupt has to be able to stop this session's children.
            subagents: subagent_runner,
            reviews: parts.reviews.clone(),
            claude: parts.claude.clone(),
            gatekeepers: Default::default(),
            restart_notices: Vec::new(),
            workspace_gone_said: false,
            cheap_turn: false,
            monitors_seen_live: Vec::new(),
            gate_told: Default::default(),
            outage: None,
            provider,
            picture_marks,
            key_wanted,
            // Filled on the first switch away from local, never at open: a session
            // that started on a provider has no local window to go back to, and
            // `None` here says exactly that.
            local_window: None,
            local_window_cell: local_window_cell.clone(),
            point_cell: point_cell.clone(),
            // Captured from the pre-move `cfg` and `parts` above: everything `/models local` returns to, held before
            // the first switch can overwrite it. A return needs a record and cannot re-derive.
            own_server,
            // Nothing to go back to until a switch has been made.
            switch_back: None,
            // Nothing waiting: a switch asked for between turns takes effect at once.
            pending_switch: None,
            // And no turn owed: only a fallback ever arms one.
            fallback_continuation: None,
            continuation_of_fallback: false,
            // No run of troubles until a round meets one.
            trouble_kind: String::new(),
            trouble_count: 0,
            // Filled by `Sessions::open_plugins` for a session the daemon serves; `None` reads
            // `plugin scaffold`/`build` only, and says so by name (see the field).
            plugin_slot: plugin_slot.clone(),
            // Nothing has asked for a confirmation yet - and a request outlives no restart by
            // design: a card is about now, and the head that was waiting for the answer is a
            // process that stopped with the daemon.
            mode_confirm: None,
            recall,
            // Nothing evicted yet: a session's first compaction is what fills this.
            recall_map: Vec::new(),
            // No limit until the provider puts a time on one, and nothing limited to filter by.
            limit_park: None,
            limited: Default::default(),
            limit_return: None,
            limit_row: Default::default(),
        };
        h.announce_the_open(stale, stale_for_model);
        h.import_the_foreign_conversation();
        Ok(h)
    }

    /// Say that the session is open, once everything it is made of exists: the board's first settings, the activity
    /// and footprint sources, the reads a head attaches for, and the resume's own republish. Split out so the last step
    /// is a sentence rather than a hundred lines between the struct literal and `Ok`.
    fn announce_the_open(&mut self, stale: Option<String>, stale_for_model: Option<String>) {
        // The recovered anchor rolled forward to the ledger as rebuilt, so the pair
        // every `Config` reader converts with is the count of THIS conversation from the
        // first thing that reads it - a `/reseat` before any turn included.
        self.measure_resident();
        // The operator's `/nag pause`, from the session's own row - before the settings are
        // first published, so the first `nags` row a head reads is already the true one.
        self.restore_nag_pause();
        // What the board already waits on the operator for is the state, not news: the settings row
        // says it, and only a question asked from here on is announced.
        self.asks_said = crate::operator_ask::waiting(&self.todos.snapshot());
        self.publish_settings();
        self.publish_jobs();
        // What this session is waiting on, and how to work it out again: registered here because this is the first
        // point where the hub, the watcher set, the monitor table and the board exist together. From here on the hub
        // re-derives after any event that can have moved it, so only the changes no event announces are said.
        self.hub.set_activity_source(crate::activity::source(
            self.job_watch.clone(),
            self.monitors.clone(),
            self.monitor_cursor.clone(),
            self.prompt_in_flight.clone(),
            self.limit_row.clone(),
        ));
        self.publish_activity();
        // And what this session's harness HOLDS - the half of a session's memory that is
        // not on the hub at all. Registered here, where the harness, its transcript, its ledger
        // and its trail all exist, and read by the memory watchdog's census.
        self.restate_footprint();
        self.hub.set_footprint(crate::footprint::source(
            self.footprint_gauges.clone(),
            self.trail.clone(),
        ));
        // And the notes source, which is read rather than pushed - a head's `ListNotes`
        // is answered from it, so it has to be installed before a head can attach. See
        // `Harness::install_notes_source`.
        self.install_notes_source();
        // And the skills source, for the same reason: a head's `ListSkills` is answered from
        // it, and it has to be in place before a head can attach.
        self.install_skills_source();
        // And the live job-output read, for the same reason and with the same timing: the
        // jobs pane's Enter is answered from it while a turn holds the worker. See
        // `Harness::install_job_output_source`.
        self.install_job_output_source();
        if self.resumed.is_some() {
            // The RESUME's own sentence, and it is not the carry's: a resume re-announces stored rows so a head can draw
            // the conversation it already had, while the carry's wording was read here and asked why a prompt was rebuilt.
            // It also restores the CONVERSATION, which a compaction splits across transcripts.
            let before = self.ancestor_tail();
            self.republish_after(letibot_sessionlog::RESTORING_ROWS, before);
        }
        // What a head attaching to this session should be told, left in the registry for `seat_in`: a session no
        // harness has opened has no entry, and this is a no-op. HERE, after the republish, because a live note is filed
        // at the seam it arrives at, and said before the rows it would be drawn above the whole conversation.
        self.session_registry
            .set_stale_prefix(&self.cfg.session_id, stale, stale_for_model);
    }

    /// X21's `open` - or `reseat`, because a resume IS one: the prompt the conversation speaks may
    /// have moved under it, and that is the fact this event exists to hand over. Asked when the
    /// session's layers first exist (`Sessions::open_plugins`, which runs after the harness is
    /// built), and by then everything about the open has happened - so nothing a layer answers can be
    /// the difference between a session that opened and one that did not.
    pub(crate) fn announce_lifecycle(&mut self) {
        let event = if self.resumed.is_some() {
            "reseat"
        } else {
            "open"
        };
        let facts = self.lifecycle_facts.clone();
        self.lifecycle(event, facts);
    }

    /// One lifecycle event (X21), asked with the facts this session has and its acts performed: a
    /// sentence the model must see (`say`), a board row (`add_todo`), a monitor (`arm`), a message in
    /// the session's room (`room_say`). Each is the host's own door, and an act it has not got is said
    /// by name rather than dropped.
    pub(crate) fn lifecycle(&mut self, event: &str, stale: letibot_plugin::lifecycle::Stale) {
        let Some(host) = self.plugin_host() else {
            return;
        };
        let rows = self
            .todos
            .snapshot()
            .into_iter()
            .map(|t| letibot_plugin::lifecycle::Row {
                id: t.content.clone(),
                content: t.content,
                // The wire's own words, because a layer reads this beside a pane's rows.
                status: word(&t.status),
                by: word(&t.by),
                needs: Vec::new(),
            })
            .collect();
        let input = letibot_plugin::lifecycle::Input {
            v: letibot_plugin::V,
            event: event.to_string(),
            session: self.cfg.session_id.clone(),
            stale,
            rows,
            now_ms: Some(letibot_sessionlog::event::now_ms()),
        };
        let Ok(input) = serde_json::to_value(&input) else {
            return;
        };
        let out = host.lifecycle(&self.hub, event, &input);
        for (verb, args) in out.acts {
            match verb.as_str() {
                "say" => {
                    let text = args
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if text.is_empty() {
                        continue;
                    }
                    // Both doors: the log, so a person watching the session sees it, and the model,
                    // because a `say` is the sentence a release's own wording used to be.
                    self.hub.warn(format!("lifecycle_{event}"), text.clone());
                    let _ = self.note_to_model(&text);
                }
                "add_todo" => {
                    let content = args
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if content.is_empty() {
                        continue;
                    }
                    let added = self.todos.upsert_model(&[(
                        content,
                        letibot_tokencore::store::TodoStatus::Pending,
                        Vec::new(),
                    )]);
                    if added > 0 {
                        self.flush_todos_or_say(&format!("the row `{event}` added"));
                    }
                }
                "arm" => {
                    let arm = letibot_plugin::tool_result::Arm::new(
                        args.get("plugin")
                            .and_then(|p| p.as_str())
                            .unwrap_or_default(),
                        args.get("args").cloned().unwrap_or(serde_json::Value::Null),
                    );
                    let said = self.arm_plugin_monitor(&arm);
                    let _ = self.note_to_model(said.trim());
                }
                "room_say" => {
                    let text = args.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    let Some(room) = self.room.clone() else {
                        self.hub.warn(
                            format!("lifecycle_{event}"),
                            "a lifecycle layer asked for a room message, and this session has no \
                             room: nothing was said",
                        );
                        continue;
                    };
                    let mut said = serde_json::Map::new();
                    said.insert("text".into(), serde_json::Value::String(text.to_string()));
                    if let Err(why) = room.call("say", &serde_json::Value::Object(said)) {
                        self.hub.warn(format!("lifecycle_{event}"), why);
                    }
                }
                other => {
                    self.hub.warn(
                        format!("lifecycle_{event}"),
                        format!(
                            "a lifecycle layer asked for `{other}`, and an event performs `say`, \
                             `add_todo`, `arm` and `room_say`: nothing was done"
                        ),
                    );
                }
            }
        }
    }

    /// A session whose id marks somebody else's conversation reads it in - an opencode
    /// one or a Claude Code transcript (Package H), never a resume of either.
    fn import_the_foreign_conversation(&mut self) {
        // A session whose id marks an opencode conversation reads it in: `oc-` is the namespace mark and the rest is
        // opencode's id. Only for a session that is not a resume, which is the idempotency rule. The import never fails
        // the session, since by the time it is read the head is already a working head with a screen.
        if self.resumed.is_none()
            && let Some(oc_id) = self.cfg.session_id.strip_prefix("oc-").map(str::to_string)
        {
            self.import_opencode(&oc_id);
        }
        // And a `cc-` session reads a Claude Code transcript in, by the same rule in one place
        // (`letibot_claude::import_target`), so a resumed session imports nothing and a re-run finds the stored one.
        if let Some(cc_id) =
            letibot_claude::import_target(self.resumed.is_some(), &self.cfg.session_id)
                .map(str::to_string)
        {
            self.import_claude(&cc_id);
        }
    }

    /// Read an opencode conversation into this session: the half that turns the source's rows into THIS
    /// session's rows and keeps the screen live, never failing the session. The read is on the worker because
    /// `append_items` is the worker's, and the screen is up first because a head attaches to the registry's hub.
    fn import_opencode(&mut self, oc_id: &str) {
        let Some(path) = letibot_opencode::default_db_path() else {
            self.import_note(
                "import_no_db",
                "no $HOME, so no opencode database path to read. The session stays up and \
                 empty."
                    .into(),
            );
            return;
        };
        let source = match letibot_opencode::Source::open(&path) {
            Ok(s) => s,
            Err(e) => {
                self.import_note(
                    "import_no_db",
                    format!(
                        "{e}\n\nThe session stays up: prompt it like any other and it runs on \
                         this head's own model."
                    ),
                );
                return;
            }
        };
        let tree = match source.tree(oc_id) {
            Ok(t) => t,
            Err(e) => {
                self.import_note(
                    "import_no_session",
                    format!("{e}\n\nThe session stays up and empty."),
                );
                return;
            }
        };

        // Rule 1, said where a reader will see it: these are another agent's rows, under another provider, and a
        // reader who cannot tell whose they are cannot weigh them. The origin goes on the log the head reads.
        self.import_note("imported", tree.root.origin());
        let total = tree.parts;
        const WHAT: &str = "importing an opencode conversation";
        self.filling(WHAT, "parts", 0, total);

        let mut failure: Option<String> = None;
        let mut since_persist = 0u64;
        // The run of tool results the read is inside: `render_items` closes a tool turn at the end
        // of every batch, so a batch boundary inside a run is a turn the rows do not have.
        let mut held: Vec<TranscriptItem> = Vec::new();
        let report = source.read_tree(oc_id, &mut |ev| match ev {
            letibot_opencode::Event::Progress { done, .. } => {
                // Throttled: one tick per 64 parts is a live bar, and one per part is
                // thousands of publishes for a quarter-second read.
                if done % 64 == 0 {
                    self.filling(WHAT, "parts", done, total);
                }
            }
            letibot_opencode::Event::Row(r) => {
                if failure.is_some() {
                    return;
                }
                if let Err(e) = self.import_row(r.item, &mut held) {
                    failure = Some(e.to_string());
                    return;
                }
                since_persist += 1;
                if since_persist >= 512 {
                    since_persist = 0;
                    if let Err(e) = self.persist() {
                        failure = Some(e.to_string());
                    }
                }
            }
            letibot_opencode::Event::Scrap(s) => self.import_note("import_scrap", s.said),
        });
        if failure.is_none()
            && let Err(e) = self.flush_imported(&mut held)
        {
            failure = Some(e.to_string());
        }

        match (failure, report) {
            (Some(e), _) => self.import_note(
                "import_failed",
                format!(
                    "the import stopped part-way: {e}. What arrived is on the screen and in \
                     the store; the rest did not."
                ),
            ),
            (None, Err(e)) => self.import_note(
                "import_failed",
                format!(
                    "reading opencode's database stopped part-way: {e}. What arrived is on the \
                     screen and in the store; the rest did not."
                ),
            ),
            (None, Ok(report)) => {
                let _ = self.persist();
                // The last tick clears the head's line; the summary is the durable
                // residue, and it carries the spend rule 2 is about.
                self.filling(WHAT, "parts", total, total);
                self.import_note("imported_summary", report.summary());
            }
        }
    }

    /// Read a Claude Code conversation into this session (Package H): the same split and the same refusal to fail
    /// the session as `import_opencode`. The counter is BYTES here, because a transcript is a file whose length is
    /// known from `stat`, and counting its lines first would be a second pass over it: the bar is a fact about the file.
    fn import_claude(&mut self, cc_id: &str) {
        let Some(dir) = letibot_claude::default_projects_dir() else {
            self.import_note(
                "import_no_db",
                "no $HOME, so no Claude Code transcript directory to read. The session stays up \
                 and empty."
                    .into(),
            );
            return;
        };
        let source = match letibot_claude::Source::open(&dir) {
            Ok(s) => s,
            Err(e) => {
                self.import_note(
                    "import_no_db",
                    format!(
                        "{e}\n\nThe session stays up: prompt it like any other and it runs on \
                         this head's own model."
                    ),
                );
                return;
            }
        };
        let tree = match source.tree(cc_id) {
            Ok(t) => t,
            Err(e) => {
                self.import_note(
                    "import_no_session",
                    format!("{e}\n\nThe session stays up and empty."),
                );
                return;
            }
        };

        // Rule 1, said where a reader will see it - the uuid, the directory, the branch,
        // the model and the Claude Code version. Another agent's rows under another vendor's
        // model; a reader who cannot tell whose they are cannot weigh them.
        self.import_note("imported", tree.root.origin());
        let total = tree.root.bytes;
        const WHAT: &str = "importing a Claude Code conversation";
        self.filling(WHAT, "bytes", 0, total);

        let mut failure: Option<String> = None;
        let mut since_persist = 0u64;
        // A run of tool results is one batch, for the reason the opencode import gives.
        let mut held: Vec<TranscriptItem> = Vec::new();
        let report = source.read(cc_id, &mut |ev| match ev {
            letibot_claude::Event::Progress { bytes, .. } => {
                // Throttled to about one tick a megabyte: a live bar, and not thousands of
                // publishes for a read that takes a second.
                if bytes / (1 << 20) != (bytes.saturating_sub(64 * 1024)) / (1 << 20) {
                    self.filling(WHAT, "bytes", bytes, total);
                }
            }
            letibot_claude::Event::Row(r) => {
                if failure.is_some() {
                    return;
                }
                if let Err(e) = self.import_row(r.item, &mut held) {
                    failure = Some(e.to_string());
                    return;
                }
                since_persist += 1;
                if since_persist >= 512 {
                    since_persist = 0;
                    if let Err(e) = self.persist() {
                        failure = Some(e.to_string());
                    }
                }
            }
            letibot_claude::Event::Scrap(s) => self.import_note("import_scrap", s.said),
        });
        if failure.is_none()
            && let Err(e) = self.flush_imported(&mut held)
        {
            failure = Some(e.to_string());
        }

        match (failure, report) {
            (Some(e), _) => self.import_note(
                "import_failed",
                format!(
                    "the import stopped part-way: {e}. What arrived is on the screen and in \
                     the store; the rest did not."
                ),
            ),
            (None, Err(e)) => self.import_note(
                "import_failed",
                format!(
                    "reading the Claude Code transcript stopped part-way: {e}. What arrived is \
                     on the screen and in the store; the rest did not."
                ),
            ),
            (None, Ok(report)) => {
                let _ = self.persist();
                self.filling(WHAT, "bytes", total, total);
                self.import_note("imported_summary", report.summary());
            }
        }
    }

    /// One progress tick for a named operation, from the daemon's own counter. The head draws a bar from this and
    /// from nothing else, since deriving one from body-less rows would mean inferring the operation from the symptom.
    pub(crate) fn filling(&self, what: &str, unit: &str, done: u64, total: u64) {
        self.hub.publish(letibot_sessionlog::SessionEvent::Filling {
            what: what.to_string(),
            unit: unit.to_string(),
            done,
            total,
        });
    }

    /// Put the restored conversation back on the session's log: a resumed harness holds the transcript and the hub does
    /// not, so each row is announced as a live one is and both the snapshot and attached paths get it free. Not re-renders
    /// and not re-runs, so no turn events and no timings no clock measured. `what` is the caller's words.
    fn ancestor_tail(&self) -> Vec<(String, TranscriptItem, [u8; 32])> {
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        let bound = letibot_sessionlog::view::ViewBounds::default().items;
        let mut room = bound.saturating_sub(self.session.ledger.rows().len());
        let mut chunks: Vec<Vec<(String, TranscriptItem, [u8; 32])>> = Vec::new();
        let mut cursor = store.parent_of(&self.transcript_id).ok().flatten();
        while room > 0 {
            let Some(id) = cursor.take() else { break };
            let Ok(t) = store.load_transcript(&id) else {
                break;
            };
            let n = t.items.len();
            let take = n.min(room);
            chunks.push(
                t.items
                    .into_iter()
                    .skip(n - take)
                    .map(|(item, row, _)| (row.item_id, item, row.h_k))
                    .collect(),
            );
            room -= take;
            cursor = t.parent_transcript_id;
        }
        // Collected newest-parent first; published oldest first, so the view reads in order.
        chunks.into_iter().rev().flatten().collect()
    }

    fn republish_after(&self, what: &str, before: Vec<(String, TranscriptItem, [u8; 32])>) {
        let current = self
            .session
            .ledger
            .rows()
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                self.session
                    .items
                    .get(i)
                    .map(|item| (row.item_id.clone(), item.clone(), row.h_k))
            });
        let rows: Vec<(String, TranscriptItem, [u8; 32])> =
            before.into_iter().chain(current).collect();
        let total = rows.len() as u64;
        for (i, (item_id, item, h_k)) in rows.iter().enumerate() {
            // One progress tick per row is a flood, and the flood is what breaks the bar: ~2000 events over a head's
            // capped queue demotes it and the bar freezes at its last tick. So a tick every `FILLING_STRIDE` rows, with
            // the first and last always ticking, and the stride on the TICK only: skipping the row lost the conversation.
            if i % FILLING_STRIDE == 0 || (i as u64 + 1) == total {
                self.filling(what, "rows", i as u64 + 1, total);
            }
            self.hub.append_row(
                item_id,
                letibot_tokencore::store::item_kind(item),
                &hex32(h_k),
                item,
            );
            self.publish_citations(item_id, item);
        }
        // The walk ending is the operation ending, so say so: a row with no matching item ends the walk below
        // `total`, and a bar with no completing tick vanishes because a timer ran out rather than because the work
        // finished. Published unconditionally, so it ends by fact. This is the second time this shape has been measured.
        self.filling(what, "rows", total, total);
    }

    /// The last `n` rows of this session's STORED transcript, as compact text: the durable record, since the store is
    /// what survives a restart. `None` when there is no store or no transcript, said as itself rather than as an empty
    /// tail, because I could not look and there was nothing there are different facts.
    pub(crate) fn stored_tail(&self, n: usize) -> String {
        let Some(store) = self.store.as_ref() else {
            return "The store is not open, so the logs could not be read.".into();
        };
        let loaded = match store.load_transcript(&self.transcript_id) {
            Ok(l) => l,
            Err(e) => return format!("The logs could not be read: {e}"),
        };
        let items = &loaded.items;
        let start = items.len().saturating_sub(n);
        let mut out = String::from("the last rows of this session's own log:\n");
        for (item, _, _) in &items[start..] {
            let line = match item {
                TranscriptItem::User { parts, .. } => {
                    let text: String = parts
                        .iter()
                        .map(|p| match p {
                            UserPart::Text { text } => text.clone(),
                            _ => "[non-text part]".into(),
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!("  user: {}", one_line(&text, 90))
                }
                TranscriptItem::Assistant {
                    text, tool_calls, ..
                } => {
                    if text.trim().is_empty() && !tool_calls.is_empty() {
                        format!(
                            "  assistant: (no prose) -> {} call(s): {}",
                            tool_calls.len(),
                            tool_calls
                                .iter()
                                .map(|c| c.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    } else {
                        format!("  assistant: {}", one_line(text, 90))
                    }
                }
                TranscriptItem::ToolResult {
                    name,
                    outcome,
                    payload,
                    ..
                } => format!("  {name} -> {outcome:?}: {}", one_line(payload, 70)),
                TranscriptItem::System { text, .. } => format!("  system: {}", one_line(text, 80)),
                TranscriptItem::Notice { text, .. } => format!("  notice: {}", one_line(text, 80)),
                // The two that carry no facts about the RUN: reasoning is the model thinking and a segment mark is
                // the head's boundary between runs. A log tail looking for what happened to this call names neither.
                TranscriptItem::Reasoning { .. }
                | TranscriptItem::SegmentMark { .. }
                | TranscriptItem::Offer { .. } => continue,
            };
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

/// Read a GGUF's vocabulary - llama.cpp's job, so only in a build with the `local`
/// feature. Without it the answer is a sentence saying so, and a cloud provider still works.
#[cfg(feature = "local")]
pub(crate) fn load_gguf(path: &std::path::Path) -> Result<Vocab, letibot_tokencore::VocabError> {
    letibot_llama::load(path)
}

#[cfg(not(feature = "local"))]
pub(crate) fn load_gguf(path: &std::path::Path) -> Result<Vocab, letibot_tokencore::VocabError> {
    Err(letibot_tokencore::VocabError::NoLlama {
        path: path.display().to_string(),
    })
}

/// The GGUF's own file name, which is what two servers holding one model agree on even when the
/// directories differ. A split GGUF's `-00001-of-00006` suffix is kept: two servers that disagree
/// about the shard count are not serving the same file.
pub(crate) fn vocab_basename(p: &std::path::Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Sampling as one short line for a reply the operator reads, in the file's own order.
pub(crate) fn render_sampling(s: &serde_json::Map<String, serde_json::Value>) -> String {
    s.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A provider backend from its config: preset, key (missing is a refusal naming variable and file), model, switches.
/// `notes` is passed in because the provider must stay below this crate's session log. `deadlines` are the first-byte and answer budgets, resolved from the config by [`crate::config::Config::deadlines_for`].
pub fn build_provider(
    pc: &crate::config::ProviderConfig,
    sampling: &serde_json::Value,
    notes: letibot_provider::openai::Notes,
    deadlines: crate::leticode_config::Deadlines,
) -> Result<Box<dyn letibot_backend::MessagesBackend>, String> {
    let preset = letibot_provider::Preset::parse(&pc.name)?;
    let creds = letibot_provider::keys::resolve(preset, pc.api_key.as_deref(), None)
        .map_err(|e| format!("provider {}: {e}", pc.name))?;
    let mut p = letibot_provider::OpenAiProvider::new(preset, pc.model.as_deref(), creds)
        .with_notes(notes)
        .with_first_byte(deadlines.first_byte)
        .with_response_timeout(deadlines.answer);
    p.thinking = pc.thinking;
    p.sampling = provider_sampling(sampling);
    Ok(Box::new(p))
}

/// A provider as ONE session's backend: its requests are assembled under that session's marks, so
/// what a refusal teaches is learned by this conversation - and `learned` writes the mark down.
pub(crate) fn scoped_provider(
    inner: Box<dyn letibot_backend::MessagesBackend>,
    marks: std::sync::Arc<std::sync::Mutex<letibot_provider::marks::Marks>>,
    learned: Option<letibot_provider::marks::Learned>,
) -> Box<dyn letibot_backend::MessagesBackend> {
    letibot_provider::marks::scoped(inner, marks, learned)
}

/// Where a learned mark goes: the session's own row, which is what makes it survive a restart. By
/// path because the store hands one connection per file, and a refusal is rare.
pub(crate) fn persist_marks(
    store: Option<std::path::PathBuf>,
    session_id: String,
) -> std::sync::Arc<dyn Fn(&letibot_provider::marks::Marks) + Send + Sync> {
    std::sync::Arc::new(move |marks: &letibot_provider::marks::Marks| {
        let Some(path) = &store else { return };
        if let Ok(s) = letibot_tokencore::store::Store::open(path) {
            let _ = s.set_session_marks(&session_id, &marks.to_json());
        }
    })
}

/// Where a provider's own note reaches the operator: the session's hub, one crate up. One closure, so a fact the
/// provider learns arrives as the warning a head already draws, and the provider keeps needing no session log.
pub(crate) fn provider_notes(
    hub: &std::sync::Arc<letibot_sessionlog::Hub>,
) -> letibot_provider::openai::Notes {
    let hub = hub.clone();
    std::sync::Arc::new(move |said: String| {
        hub.warn("model_refused_images", said);
    })
}

/// The sampling the operator configured, as a provider's body fields. The local
/// server's knobs (`top_k`, `seed`, llama.cpp's own names) are not the API's;
/// only what the OpenAI shape carries goes through.
fn provider_sampling(sampling: &serde_json::Value) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    if let Some(obj) = sampling.as_object() {
        for k in [
            "temperature",
            "top_p",
            "seed",
            "max_tokens",
            "presence_penalty",
            "frequency_penalty",
        ] {
            if let Some(v) = obj.get(k) {
                out.insert(k.to_string(), v.clone());
            }
        }
    }
    serde_json::Value::Object(out)
}

/// The parent's ruleset plus a `deny` per tool of a denied class: the tools are not seated either, so this holds if
/// a name reaches the gate anyway, since last rule wins and these are last. The seated schemas by class, plus the
/// well-known names of each class whether seated or not, so a tool registered later is refused by name.
pub(crate) fn downgraded_ruleset(
    parent: &letibot_tools::permission::Ruleset,
    downgrade: &letibot_tools::schema::Downgrade,
    seated: &[letibot_tools::schema::ToolSchema],
) -> letibot_tools::permission::Ruleset {
    use letibot_tools::permission::{Action, Rule};
    use letibot_tools::schema::Access;
    let mut rules = parent.clone();
    if downgrade.is_none() {
        return rules;
    }
    let well_known: &[(Access, &[&str])] = &[
        // `notes` is in the `Write` arm on purpose: a standing note is written to the operator's tree and read into
        // every later session's prompt, which makes it a more persistent write than a file edit, so a subagent denied
        // `write` must not be able to plant one either.
        (Access::Write, &["write", "edit", "exit_plan_mode", "notes"]),
        (Access::Exec, &["bash", "monitor", "job_kill", "lsp"]),
        (
            Access::Network,
            &["flowy", "web_search", "web_fetch", "forge", "mcp"],
        ),
    ];
    let mut names: Vec<String> = Vec::new();
    for (class, known) in well_known {
        if downgrade.denies(*class) {
            names.extend(known.iter().map(|n| n.to_string()));
        }
    }
    names.extend(
        seated
            .iter()
            .filter(|s| downgrade.denies(s.access))
            .map(|s| s.name.clone()),
    );
    names.sort();
    names.dedup();
    for name in names {
        rules.push(Rule::new(&name, "*", Action::Deny));
    }
    rules
}

/// Did this session inherit a whole-host standing from an ancestor? The store records only the SEAT, which is not
/// the same fact for these sessions, so it is read back up the parent chain the row carries. A row the walk cannot
/// read is not an answer and not an error: an unproven standing is the confined one.
fn unconfined_by_inheritance(store: &Store, from: &StoredSession) -> bool {
    // The cap is [`Config::max_subagent_depth`]'s order of magnitude and not its value: the
    // chain is a real parent tree, so this only bounds a cycle a corrupted store could have.
    const MAX_ANCESTORS: usize = 32;
    let mut parent = from.parent_session_id.clone();
    let mut seen = 0usize;
    while let Some(id) = parent {
        seen += 1;
        if seen > MAX_ANCESTORS {
            return false;
        }
        let Ok(Some(row)) = store.session(&id) else {
            return false;
        };
        if row.role.as_deref() == Some(Seat::Leticode.as_str()) {
            return true;
        }
        parent = row.parent_session_id.clone();
    }
    false
}

/// A todo's state or author as the wire spells it: a lowercase word, or the author string itself
/// (`Parent <id>`). Serialised rather than matched, so a fifth author needs no edit here.
fn word<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod unconfined_by_inheritance_tests {
    use super::unconfined_by_inheritance;
    use letibot_tokencore::store::{SessionRecord, Store};

    /// A store holding `rows` - `(id, role, parent)`, in order - and the id of the LAST
    /// one, which is the session the walk is asked about.
    fn chain(tag: &str, rows: &[(&str, Option<&str>, Option<&str>)]) -> (Store, String) {
        let path = std::env::temp_dir().join(format!(
            "letibot-inherit-{tag}-{}-{}.db",
            std::process::id(),
            letibot_sessionlog::registry::now_ms()
        ));
        let store = Store::open(&path).expect("a store");
        for (id, role, parent) in rows {
            store
                .put_session(&SessionRecord {
                    id: (*id).to_string(),
                    title: None,
                    model_id: "a model".into(),
                    dialect_sha: "a dialect".into(),
                    workspace_root: "/tmp".into(),
                    owner: "dead".into(),
                    approvers: Vec::new(),
                    role: role.map(str::to_string),
                    parent_session_id: parent.map(str::to_string),
                })
                .expect("the row");
        }
        let last = rows.last().expect("at least one row").0.to_string();
        (store, last)
    }

    fn stands(store: &Store, id: &str) -> bool {
        let row = store
            .session(id)
            .expect("the store answers")
            .expect("the row is there");
        unconfined_by_inheritance(store, &row)
    }

    /// A subagent's standing is its ancestors', and the row says only the seat: a `coder` child of a leticode
    /// session is unconfined, and so is ITS child, which is what a reopened session has to be given back.
    #[test]
    fn a_coder_child_of_a_leticode_session_stands_where_its_root_stood() {
        let (store, child) = chain(
            "subagent",
            &[
                ("s-root", Some("leticode"), None),
                ("s-root-sub-1", Some("coder"), Some("s-root")),
            ],
        );
        assert!(
            stands(&store, &child),
            "a coder child of leticode is unconfined"
        );
        let (store, grandchild) = chain(
            "grandchild",
            &[
                ("s-root", Some("leticode"), None),
                ("s-root-sub-1", Some("coder"), Some("s-root")),
                ("s-root-sub-1-sub-2", Some("coder"), Some("s-root-sub-1")),
            ],
        );
        assert!(
            stands(&store, &grandchild),
            "the standing travels the whole chain, not one level"
        );
    }

    /// A coder session's child stays confined, which is the other half of the rule the
    /// spawn applies (see `Config::unconfined`): its parent is, so it is.
    #[test]
    fn a_child_of_a_coder_session_stays_confined() {
        let (store, child) = chain(
            "confined",
            &[
                ("s-root", Some("coder"), None),
                ("s-root-sub-1", Some("coder"), Some("s-root")),
            ],
        );
        assert!(!stands(&store, &child));
    }

    /// A chain the store cannot answer for is not an answer. An ancestor whose row is
    /// gone leaves the standing unproven, and unproven is confined - the state every
    /// session that predates this had, and not a reason to refuse an open.
    #[test]
    fn an_ancestor_the_store_does_not_hold_is_not_unconfined() {
        let (store, child) = chain(
            "orphan",
            &[(
                "s-root-sub-1",
                Some("coder"),
                Some("s-a-parent-that-is-gone"),
            )],
        );
        assert!(!stands(&store, &child));
    }
}

/// The role a seat resolves to, with `bash` removed when it was not asked for: a SESSION decision, since `m2_runner`
/// is nine tools and a session that took eight is still that role. Dropping the name is the only way to seat a runner
/// without a shell, since leaving it in would put `bash` in the prompt.
pub(crate) fn role_for(cfg: &Config) -> Role {
    role_for_seat(cfg.seat, cfg)
}

/// The same table, reached from a seat the store named rather than from `cfg`. `cfg` is still needed for `allow_bash`,
/// which is a daemon-wide flag and deliberately not per session: a shell is not the session's to record.
pub(crate) fn role_for_seat(seat: Seat, cfg: &Config) -> Role {
    let mut r = base_role_for_seat(seat, cfg);
    // The model's own verbs on its own session (X24), on every seat that runs a conversation -
    // a child's included, since a subagent that overran its window is the case the operator named.
    // Native, so no seat can be without them: a session told its prompt moved with no verb to act on
    // it is the tmux `send-keys` the doc measured (G3).
    if !matches!(seat, Seat::Runner | Seat::NotesKeeper) {
        for verb in letibot_plugin::command::SELF_VERBS {
            r.tools.push((*verb).to_string());
        }
        r.max_tools += letibot_plugin::command::SELF_VERBS.len();
    }
    // The room: the `flowy` tool is registered by `Sessions` for every root session, and the role names it under the
    // same condition, since `resolve_role` refuses a role naming a tool the session does not have. Always a door for a
    // root, seat or no seat: without one the tool says so and names `/flowy login`.
    if cfg.parent_session_id.is_none()
        && !matches!(seat, Seat::Runner | Seat::Gatekeeper | Seat::NotesKeeper)
    {
        r.tools.push("flowy".into());
        r.max_tools += 1;
    }
    // `web_search` is seated only when something is behind it, and a subagent inherits that. Conditional because a
    // tool schema is prompt bytes, so seating it unconditionally would re-prefill every stored conversation to add a
    // tool that refuses. A reviewer reads the references, so the gatekeeper seats the two web tools; `flowy` stays off.
    if cfg.web_search.is_some() && !matches!(seat, Seat::Runner | Seat::NotesKeeper) {
        r.tools.push("web_search".into());
        r.max_tools += 1;
    }
    // `web_fetch` seats on the same rule as `web_search`: only when
    // something is behind it, so a session started without `--web-fetch` is
    // byte-identical to one from before the flag existed.
    if cfg.web_fetch && !matches!(seat, Seat::Runner | Seat::NotesKeeper) {
        r.tools.push("web_fetch".into());
        r.max_tools += 1;
    }
    r
}

pub(crate) fn base_role_for_seat(seat: Seat, cfg: &Config) -> Role {
    // The review queue's own session is nobody's seat at a keyboard: its prompt prescribes reading
    // and correcting the reviewers the queue's clock starts, so it holds `task_result` and
    // `task_message` and neither a child of its own nor a shell. By id, because its seat is default.
    if crate::sessions::gatekeeper::is_the_queue_session(cfg) {
        return roles::review_queue();
    }
    match seat {
        Seat::Orchestrator => roles::m1_orchestrator(),
        Seat::Planner => roles::planner(),
        Seat::Researcher => roles::m3_researcher(),
        Seat::Coder => {
            let mut r = roles::m2_coder();
            // Same rule as the runner below: the role lists `bash`, the flag seats it.
            if !cfg.allow_bash {
                r.tools.retain(|t| t != "bash");
            }
            r
        }
        Seat::Runner => {
            let mut r = roles::m2_runner();
            if !cfg.allow_bash {
                r.tools.retain(|t| t != "bash");
            }
            r
        }
        // The reviewer: read-only by construction, `bash` only behind the daemon's flag  -
        // without it the gatekeeper reads the files and cannot diff, and says so in its verdict.
        Seat::Gatekeeper => {
            let mut r = roles::gatekeeper();
            if !cfg.allow_bash {
                r.tools.retain(|t| t != "bash");
            }
            r
        }
        // The notes keeper: the read tools and `notes`, and no exec path at all, so there is no flag to strip. A repair
        // goes through `notes`, the one writer that keeps a note a note, and nothing on this seat can touch code.
        Seat::NotesKeeper => roles::notes_keeper(),
        Seat::Leticode => {
            let mut r = roles::leticode();
            if !cfg.allow_bash {
                // The whole exec surface is behind the flag: `bash` and the job verbs it feeds, `monitor`, and `fanout`
                // and `scenario`, which start commands on other machines. A session with no shell has nothing to wait on and
                // nothing it may start elsewhere either.
                r.tools.retain(|t| {
                    !matches!(
                        t.as_str(),
                        "bash"
                            | "job_list"
                            | "job_output"
                            | "job_wait"
                            | "job_kill"
                            | "monitor"
                            | "pkill"
                            | "ps"
                            | "screen"
                            | "fanout"
                            | "scenario"
                    )
                });
            }
            r
        }
    }
}

/// Where layer A is standing, and what this daemon can claim about it: the pin is honest only where the spawn
/// `env_clear()`s and filters `BASH_ENV` and its three companions, which `HostProcesses` does. A seat with no exec
/// backend spawns no shell, so `Unknown` stays and a bare command is unresolved. The root and `$HOME` are filled in.
pub fn scratch_dir() -> std::path::PathBuf {
    // The head's own rule, applied to the daemon's half: a world-readable name in `temp_dir()` would not do, as
    // it holds what the jobs write and the model read. `$XDG_RUNTIME_DIR` is per-user, 0700 and tmpfs, and the fallback
    // creates its directory at 0700. Still per daemon process, so the one place that cleans it up can find it.
    let pid = std::process::id();
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => std::path::PathBuf::from(d)
            .join("letibot")
            .join(format!("scratch-{pid}")),
        None => std::env::temp_dir()
            .join(format!("letibot-{}", unsafe { libc::getuid() }))
            .join(format!("scratch-{pid}")),
    }
}

/// What a confined session's view holds beyond its project: the operator's `--grant-ro` paths and the merge queue's
/// landing tree, READ-ONLY, since the next gate runs in it, and created empty so the bind has something to bind. The
/// build cache is the main tree's `target/` for an UNCONFINED child only, since the bwrap view does not hold it.
pub fn worktree_build_env(
    placement: &letibot_tools::builtins::task::WorktreePlacement,
) -> Option<(String, String)> {
    if placement.main_tree {
        return None;
    }
    let common = git_in(
        &placement.path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()?;
    let main = std::path::Path::new(common.trim()).parent()?;
    Some((
        "CARGO_TARGET_DIR".to_string(),
        main.join("target").display().to_string(),
    ))
}

pub fn confined_grants(
    cfg: &Config,
    landing_root: Option<&std::path::Path>,
) -> Vec<letibot_tools::exec::confine::Grant> {
    use letibot_tools::exec::confine::Grant;
    let mut out: Vec<Grant> = cfg
        .grants_ro
        .iter()
        .map(|p| Grant::ReadOnly {
            path: p.clone(),
            why: "granted on the command line with --grant-ro".into(),
        })
        .collect();
    // A landing tree per repository of the workspace - one, or each of a one-org project
    // folder's, since the queue serves every one of them there.
    let landings: Vec<PathBuf> = landing_root
        .map(|root| {
            crate::mergequeue::git::guard_not_real(root);
            crate::mergequeue::git::repos_of_workspace(&cfg.workspace)
                .iter()
                .map(|repo| crate::mergequeue::git::landing_dir_in(root, repo))
                .filter(|dir| std::fs::create_dir_all(dir).is_ok())
                .collect()
        })
        .unwrap_or_default();
    for dir in landings {
        if out.iter().any(|g| g.path() == dir) {
            continue;
        }
        out.push(Grant::ReadOnly {
            path: dir,
            why: "the merge queue's landing tree for this repository, where its gate runs: \
                  readable so the gate's evidence can be inspected, never writable"
                .into(),
        });
    }
    for g in letibot_tools::exec::seat::worktree_git_grants(&cfg.workspace) {
        if !out.iter().any(|h| h.path() == g.path()) {
            out.push(g);
        }
    }
    out
}

/// The view the prompt states, taken from the boundary the backend MEASURED and never from a
/// canned sentence: a session that lost its confinement is told nothing about a view.
pub(crate) fn view_brief(boundary: Option<&letibot_tools::exec::Boundary>) -> Option<String> {
    boundary.map(|b| b.model_brief())
}

/// What the prompt says when there is no boundary at all, which is a measured fact and not a
/// silence: a session whose tools run on this host with nothing in front of them.
pub(crate) fn whole_host_brief() -> String {
    "There is NO boundary on this session: it is not confined to the workspace. A command here \
     runs as this user over this whole host, so every file they can read (their home, their keys, \
     other projects) is readable and their processes are reachable. The FILE TOOLS are held to \
     this session's perimeter instead: a credential store, letibot's own store and every path a \
     `[boundary] hide` or a `[perimeter.<name>] hide` names are refused to them, naming the key, \
     and beyond that they reach what the perimeter's level allows - the machine for a top-level \
     session, its project and the toolchain for a child. Nothing refuses a path outside the \
     workspace, so one is somebody's property and not absent. The gate decides each call on what \
     it does, and that is not a boundary."
        .to_string()
}

pub fn surroundings_for(cfg: &Config) -> letibot_tools::Surroundings {
    let mut env = letibot_tools::Surroundings::from_env(cfg.workspace.display().to_string())
        .with_known_hosts();
    // The gate places a path by region, and the scratch is its own region - see
    // `Region::Scratch`. Without this it reads as shared `/tmp`, and destruction
    // there is an ask.
    env.scratch = Some(scratch_dir().display().to_string());
    // The shell is pinned whenever the backend can start a process: both the confined and the unconfined backend spawn
    // through `HostProcesses`, which `env_clear`s and fixes `PATH`, so a bare command resolves through a pinned PATH and
    // the `Unknown` that refused `echo` was the wrong claim for an unconfined seat.
    let may_exec = !cfg.downgrade.denies(letibot_tools::schema::Access::Exec);
    let pinned = (cfg.seat.needs_exec_backend() || (cfg.unconfined && cfg.allow_bash)) && may_exec;
    if pinned {
        env = env.with_pinned_shell(
            "the exec backend spawns /bin/sh -c, non-interactive, with the \
             environment cleared before its own pairs; PATH is the one fixed at \
             seat time and BASH_ENV, ENV, SHELLOPTS, BASHOPTS are never set",
        );
    }
    // The environment every command gets, read from the same places `spawn` reads, so nothing is inferred from command text.
    // `Host` only where the command runs in this process's view (a VM, remote host or confined session is another disk),
    // since only then does this process compose the environment.
    let on_this_box = matches!(
        cfg.placement,
        letibot_tools::builtins::task::Placement::Host
    );
    if pinned && on_this_box {
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
        let mut pairs: Vec<(String, String)> = vec![("PATH".to_string(), path.clone())];
        if let Some(home) = &env.home {
            pairs.push(("HOME".to_string(), home.clone()));
        }
        pairs.extend(cfg.build_env.iter().cloned());
        // And the keep-list a boundary passes through: `KEEP_ENV` is what a bwrap'd command keeps from THIS process, so
        // those values are what it really starts with.
        for name in letibot_tools::exec::confine::KEEP_ENV {
            if *name == "PATH" {
                continue;
            }
            if let Ok(value) = std::env::var(name) {
                pairs.push(((*name).to_string(), value));
            }
        }
        env.session = letibot_tools::intent::env::SessionEnv::seeded(pairs);
        env.shell_path = Some(path);
        if !cfg.seat.needs_exec_backend() {
            env.programs = letibot_tools::intent::ProgramLookup::Host;
        }
    }
    env
}

fn build_spiller(cfg: &Config) -> Result<letibot_tools::Spiller, HarnessError> {
    use letibot_tools::{FixedBudget, MemoryStore, NoBudget, Spiller};
    let budget: Box<dyn letibot_tools::InlineBudget> = match cfg.spill {
        // unset is a genuine no-op, not a hidden constant. The daemon says so
        // at startup (`Config::disclosures`) so that "nothing spilled" is read as
        // "nothing was configured to" rather than as "nothing was big enough".
        SpillPolicy::Unset => Box::new(NoBudget),
        SpillPolicy::Inline(n) => Box::new(FixedBudget(n)),
    };
    let store: Box<dyn letibot_tools::SpillStore> = match &cfg.spill_storage {
        SpillStorage::Memory => Box::new(MemoryStore::new()),
        SpillStorage::Dir(d) => Box::new(
            letibot_tools::spill::FileStore::new(d, &cfg.session_id)
                .map_err(|e| HarnessError::Setup(format!("spill directory {d:?}: {e}")))?,
        ),
    };
    Ok(Spiller::new(budget, store))
}

#[cfg(test)]
mod prefix_fingerprint_tests {
    use super::{Seat, StablePrefix, prefix_fingerprint, seated_at};

    fn prefix(system: &str, tools: &[&str]) -> StablePrefix {
        StablePrefix {
            system: system.to_string(),
            tools_json: tools.iter().map(|t| (*t).to_string()).collect(),
        }
    }

    /// The three inputs are all in the address, and the one that must not be is not. Each assertion is a different way
    /// the comparison could be wrong: a seat ignored, an instruction ignored, a schema ignored, or a note counted that
    /// would report every session stale the moment anybody wrote one.
    #[test]
    fn the_frozen_three_are_in_the_address_and_the_live_one_is_not() {
        let one = prefix("instructions", &["{\"name\":\"read\"}"]);
        let other_system = prefix("instructions, edited", &["{\"name\":\"read\"}"]);
        let other_tools = prefix(
            "instructions",
            &["{\"name\":\"read\"}", "{\"name\":\"bash\"}"],
        );
        assert_eq!(
            prefix_fingerprint(Seat::Coder, &one),
            prefix_fingerprint(Seat::Coder, &one),
            "the same prefix hashes the same, or nothing downstream can compare"
        );
        assert_ne!(
            prefix_fingerprint(Seat::Coder, &one),
            prefix_fingerprint(Seat::Runner, &one),
            "the seat is part of what a session was seated with"
        );
        assert_ne!(
            prefix_fingerprint(Seat::Coder, &one),
            prefix_fingerprint(Seat::Coder, &other_system),
            "the instructions are"
        );
        assert_ne!(
            prefix_fingerprint(Seat::Coder, &one),
            prefix_fingerprint(Seat::Coder, &other_tools),
            "the tool schemas are"
        );

        // And a schema that GREW moves it with the list unchanged - `todo_write`'s
        // `target`, which is the case this whole feature was written for and the one an
        // added/gone comparison of tool NAMES cannot see.
        let before = prefix(
            "instructions",
            &["{\"name\":\"todo_write\",\"parameters\":[]}"],
        );
        let after = prefix(
            "instructions",
            &["{\"name\":\"todo_write\",\"parameters\":[{\"name\":\"target\"}]}"],
        );
        assert_ne!(
            prefix_fingerprint(Seat::Coder, &before),
            prefix_fingerprint(Seat::Coder, &after),
            "a grown schema is a changed prompt, and the names are identical"
        );

        // The standing-notes section, by its own markers, is stripped before hashing  -
        // see `standing_notes::replace`, which is what does the stripping.
        let with_a = prefix(
            "instructions\n\n[standing-notes-begin]\nnote one\n[standing-notes-end]",
            &["{\"name\":\"read\"}"],
        );
        let with_b = prefix(
            "instructions\n\n[standing-notes-begin]\nnote two\n[standing-notes-end]",
            &["{\"name\":\"read\"}"],
        );
        assert_eq!(
            prefix_fingerprint(Seat::Coder, &with_a),
            prefix_fingerprint(Seat::Coder, &with_b),
            "the notes are re-read at every base rebuild; counting them would make every \
             session stale on every note"
        );
        assert_eq!(
            prefix_fingerprint(Seat::Coder, &with_a),
            prefix_fingerprint(
                Seat::Coder,
                &prefix("instructions", &["{\"name\":\"read\"}"])
            ),
            "and stripping them leaves exactly the composed prompt"
        );
    }

    /// The seated time is a time, and a missing clock is not the epoch: `0` is nobody recorded when, and a sentence
    /// reading seated 01-01 00:00 about a session nobody dated is the class of lie the notice exists to avoid.
    #[test]
    fn an_unrecorded_time_is_said_and_a_recorded_one_is_the_launchers_spelling() {
        assert_eq!(seated_at(0), "an unrecorded time");
        assert_eq!(seated_at(-1), "an unrecorded time");
        // 2026-10-08 21:14 UTC is 2026-10-08 in every zone this runs in except a +3h one
        // crossing midnight - so the assertion is the SHAPE, which is what the launcher's
        // `%m-%d %H:%M` fixes, and not a zone's arithmetic.
        let said = seated_at(1_791_500_040_000);
        assert_eq!(said.len(), 11, "{said}");
        assert!(
            said.chars().enumerate().all(|(i, c)| match i {
                2 => c == '-',
                5 => c == ' ',
                8 => c == ':',
                _ => c.is_ascii_digit(),
            }),
            "`MM-DD HH:MM`, the launcher's own spelling: {said}"
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod mac_default_seat_tests {
    use super::*;

    fn checkout() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("letibot-macseat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a checkout");
        dir.canonicalize().expect("a checkout path")
    }

    fn placed(allow_bash: bool) -> (Placed, bool) {
        placed_in(allow_bash, None)
    }

    fn placed_in(allow_bash: bool, mode: Option<letibot_tools::mode::Mode>) -> (Placed, bool) {
        let dir = checkout();
        let mut cfg = Config::for_this_box(&dir);
        if let Some(m) = mode {
            cfg.mode = m;
        }
        cfg.seat = Seat::Leticode;
        cfg.unconfined = true;
        cfg.allow_bash = allow_bash;
        cfg.session_id = format!("mac-seat-{allow_bash}");
        let hub = Hub::new(&cfg.session_id);
        let head = hub
            .attach("h", "tui", letibot_sessionlog::protocol::Caps::default(), 0)
            .head_id;
        let registry = letibot_sessionlog::registry::Registry::new();
        let p = Harness::place_the_tools(&mut cfg, &hub, &registry).expect("places");
        hub.close();
        let warned = match hub.next_batch(&head, 256) {
            letibot_sessionlog::hub::Delivery::Events(b) => b.events().iter().any(|e| {
                matches!(&e.event, SessionEvent::Warning { code, .. } if code == BOUNDARY_ABSENT)
            }),
            _ => false,
        };
        (p, warned)
    }

    fn probe_works() -> bool {
        let view = letibot_tools::exec::confine::ViewSpec::project_only(checkout());
        letibot_tools::exec::confine::probe(view, letibot_tools::exec::confine::Egress::Denied)
            .is_ok()
    }

    #[test]
    fn the_default_seat_with_bash_runs_inside_the_seatbelt_boundary() {
        if !probe_works() {
            return letibot_tokencore::apparatus::absent("a seatbelt boundary the probe accepts");
        }
        let (p, warned) = placed(true);
        assert!(p.confined, "no boundary: {}", p.described);
        assert!(p.backend.boundary().is_some());
        assert!(!warned, "a boundary was built and still warned");
    }

    #[test]
    fn its_commands_write_the_checkout_and_are_refused_outside_it() {
        if !probe_works() {
            return letibot_tokencore::apparatus::absent("a seatbelt boundary the probe accepts");
        }
        let (p, _) = placed(true);
        let dir = checkout();
        let outside =
            std::env::temp_dir().join(format!("letibot-macseat-out-{}", std::process::id()));
        std::fs::create_dir_all(&outside).expect("a directory outside the checkout");
        let run = |script: String| {
            p.backend
                .run(&letibot_tools::backend::Command {
                    argv: vec!["/bin/sh".into(), "-c".into(), script],
                    cwd: String::new(),
                    env: Vec::new(),
                })
                .expect("the backend runs a command")
                .exit
        };
        assert_eq!(run("touch inside.txt".to_string()), 0);
        assert!(dir.join("inside.txt").exists());
        assert_ne!(run(format!("touch '{}/outside.txt'", outside.display())), 0);
        assert!(!outside.join("outside.txt").exists());
    }

    #[test]
    fn the_seat_gets_the_network_when_its_mode_seats_network_calls_and_not_otherwise() {
        if !probe_works() {
            return letibot_tokencore::apparatus::absent("a seatbelt boundary the probe accepts");
        }
        use letibot_tools::mode::Mode;
        let egress = |m: Mode| {
            placed_in(true, Some(m))
                .0
                .backend
                .boundary()
                .expect("a boundary")
                .egress
        };
        assert!(egress(Mode::WRITES_ALLOWED).allowed(), "writes-allowed");
        assert!(!egress(Mode::READ_ONLY).allowed(), "read-only");
    }

    #[test]
    fn a_grant_into_the_view_keeps_the_sessions_network() {
        if !probe_works() {
            return letibot_tokencore::apparatus::absent("a seatbelt boundary the probe accepts");
        }
        let (p, _) = placed_in(true, Some(letibot_tools::mode::Mode::WRITES_ALLOWED));
        let extra = std::env::temp_dir().join(format!("letibot-macseat-g-{}", std::process::id()));
        std::fs::create_dir_all(&extra).expect("a directory to grant");
        p.backend
            .grant_into_view(&extra.canonicalize().expect("path"), false, "test")
            .expect("the grant takes");
        assert!(p.backend.boundary().expect("a boundary").egress.allowed());
    }

    #[test]
    fn the_model_is_told_the_in_view_scratch_and_not_the_daemons_root() {
        if !probe_works() {
            return letibot_tokencore::apparatus::absent("a seatbelt boundary the probe accepts");
        }
        let dir = checkout();
        let mut cfg = Config::for_this_box(&dir);
        cfg.seat = Seat::Leticode;
        cfg.unconfined = true;
        cfg.allow_bash = true;
        cfg.session_id = "mac-scratch".into();
        let hub = Hub::new(&cfg.session_id);
        let registry = letibot_sessionlog::registry::Registry::new();
        let p = Harness::place_the_tools(&mut cfg, &hub, &registry).expect("places");
        let told = cfg.model_scratch();
        assert_ne!(told, scratch_dir());
        assert!(told.starts_with(scratch_dir()));
        let run = p
            .backend
            .run(&letibot_tools::backend::Command {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("echo x > '{}/m.txt'", told.display()),
                ],
                cwd: String::new(),
                env: Vec::new(),
            })
            .expect("runs");
        assert_eq!(run.exit, 0, "the scratch the model is told is writable");
        let root_write = p
            .backend
            .run(&letibot_tools::backend::Command {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("echo x > '{}/m.txt'", scratch_dir().display()),
                ],
                cwd: String::new(),
                env: Vec::new(),
            })
            .expect("runs");
        assert_ne!(
            root_write.exit, 0,
            "the daemon's scratch root stays outside"
        );
    }

    #[test]
    fn the_default_seat_without_bash_asks_for_no_boundary() {
        let (p, _) = placed(false);
        assert!(!p.confined);
    }
}

#[cfg(test)]
mod embedder_warning_tests {
    use super::*;
    use letibot_sessionlog::event::SessionEvent;

    fn codes(hub: &Hub) -> Vec<(String, String)> {
        hub.retained()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::Warning { code, detail, .. } => Some((code.clone(), detail.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_failed_embedder_and_a_first_fetch_are_warnings_on_the_hub() {
        let base = std::env::temp_dir().join(format!("embedder-warn-{}", std::process::id()));
        std::fs::create_dir_all(&base).expect("a scratch dir");
        let cfg = Config::for_this_box(&base);

        let quiet = Hub::new("s-quiet");
        Harness::say_the_embedder(&quiet, &cfg);
        assert!(codes(&quiet).is_empty(), "no section, nothing to say");

        cfg.embedder
            .set_fault("fetching model.safetensors: boom".to_string());
        cfg.embedder
            .set_fetch_note("fetching the embeddings model (87 MiB)".to_string());
        let hub = Hub::new("s-fault");
        Harness::say_the_embedder(&hub, &cfg);
        let got = codes(&hub);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].0, "embedder_fetching");
        assert_eq!(got[1].0, "embedder_unavailable");
        assert!(got[1].1.contains("boom"), "{got:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod no_boundary_tests {
    use super::*;

    /// A `leticode` seat asks for no boundary, and this session seats no shell for a wide one to
    /// go in front of. The session must not read as confined because nobody said otherwise: the
    /// head gets a warning and the prompt gets the whole-host sentence.
    #[test]
    fn a_session_with_no_boundary_says_so_on_the_hub_and_in_its_brief() {
        let dir = std::env::temp_dir().join(format!("letibot-noboundary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a fixture workspace");
        let mut cfg = Config::for_this_box(&dir);
        cfg.seat = Seat::Leticode;
        // The whole-host arm a `leticode` seat takes: no shell, so the test needs no cgroup, and
        // the file tools still reach every path this user can.
        cfg.unconfined = true;
        cfg.allow_bash = false;
        cfg.session_id = "no-boundary-test".into();
        let hub = Hub::new(&cfg.session_id);
        let head = hub
            .attach("h", "tui", letibot_sessionlog::protocol::Caps::default(), 0)
            .head_id;
        let registry = letibot_sessionlog::registry::Registry::new();
        let placed =
            Harness::place_the_tools(&mut cfg, &hub, &registry).expect("an unconfined session");
        let warned = match hub.next_batch(&head, 256) {
            letibot_sessionlog::hub::Delivery::Events(b) => b.events().iter().any(|e| {
                matches!(&e.event, SessionEvent::Warning { code, .. } if code == UNCONFINED_SESSION)
            }),
            _ => false,
        };
        hub.close();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            placed.whole_host,
            "the whole-host arm must say it is one: {}",
            placed.described
        );
        assert!(
            placed.backend.boundary().is_none(),
            "nothing bounds a whole-host backend"
        );
        assert!(warned, "the head was told nothing about it");
        let brief = whole_host_brief();
        assert!(brief.contains("NO boundary"), "{brief}");
        assert!(brief.contains("not confined"), "{brief}");
        // And the view a confined session would state is still absent, so the two cannot read
        // alike: `whole_host_brief` is chosen by the flag and not by a missing boundary.
        assert_eq!(view_brief(None), None);
    }

    /// R121: this session seats no shell, so no substrate measured a view for its commands - but its
    /// FILE TOOLS are still this session's, and they are held to its `[perimeter.<name>]` table.
    /// The credential stores and letibot's own store are refused with a command's own sentence, a
    /// `[perimeter] hide` names the key that wrote it, and the workspace is untouched.
    #[test]
    fn a_session_with_no_shell_has_its_file_tools_held_to_its_perimeter() {
        use letibot_tools::exec::boundary::{Entry, Origin};
        use letibot_tools::exec::perimeter::Perimeter;
        use letibot_tools::exec::seat::Level;
        let dir = std::env::temp_dir().join(format!("letibot-noshell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a fixture workspace");
        std::fs::write(dir.join("main.rs"), "fn main() {}\n").expect("a source file");
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let hidden = home.join("dataset-of-the-operator");
        let mut p = Perimeter::default_for(Level::Wide);
        p.hide.push(Entry {
            value: hidden.display().to_string(),
            from: Origin {
                file: home.join(".config/letibot/leticode.toml"),
                line: 7,
                user: true,
            },
        });

        let mut cfg = Config::for_this_box(&dir);
        cfg.seat = Seat::Leticode;
        cfg.unconfined = true;
        cfg.allow_bash = false;
        cfg.session_id = "noshell-fence".into();
        cfg.perimeter = Some(std::sync::Arc::new(p));
        let hub = Hub::new(&cfg.session_id);
        let registry = letibot_sessionlog::registry::Registry::new();
        let placed =
            Harness::place_the_tools(&mut cfg, &hub, &registry).expect("an unconfined session");
        hub.close();
        let _ = std::fs::remove_dir_all(&dir);

        let backend = &placed.backend;
        assert!(
            backend.boundary().is_none(),
            "nothing confines a seat that runs no command: {}",
            placed.described
        );
        // A credential store: the never-grant sentence, and no key is offered for it.
        let why = backend
            .file_refusal(&home.join(".ssh/id_ed25519").display().to_string())
            .expect("a credential store is refused to a file tool");
        assert!(why.contains("`~/.ssh` is never granted"), "{why}");
        assert!(!why.contains("grants_ro"), "no key reaches it: {why}");
        // A `[perimeter.top-level] hide`: refused, naming the table that wrote it and the line.
        let why = backend
            .file_refusal(&hidden.join("x.parquet").display().to_string())
            .expect("a perimeter's hide is refused to a file tool");
        assert!(
            why.contains("`[perimeter.top-level] hide`") && why.contains("leticode.toml:7"),
            "{why}"
        );
        // The row's own example: the letibot store was reachable to these tools through `/`.
        let store = crate::mergequeue::git::letibot_data_dir().expect("a data dir");
        let why = backend
            .file_refusal(&store.join("sessions.db").display().to_string())
            .or_else(|| backend.file_refusal(&store.join("keys/k").display().to_string()))
            .expect("the store is refused to a file tool");
        assert!(why.contains("the letibot store"), "{why}");
        // And nothing else is: the whole host is readable, which is what its commands have.
        assert_eq!(
            backend.file_refusal(&home.join("notes.txt").display().to_string()),
            None,
            "a wide perimeter reaches the rest of the machine"
        );
    }

    /// A grant made in a session covers its subagents: the record is made once, at the session's
    /// open, and a child's config is the parent's (`children.rs` spreads `..self.base.clone()`),
    /// so five siblings ask one question once rather than five times.
    #[test]
    fn a_child_is_built_with_its_parents_view_grants() {
        use letibot_tools::view::ViewAccess;
        let dir = std::env::temp_dir().join(format!("letibot-viewgrants-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a fixture workspace");
        let mut cfg = Config::for_this_box(&dir);
        cfg.seat = Seat::Leticode;
        cfg.unconfined = true;
        cfg.allow_bash = false;
        cfg.session_id = "s-view-parent".into();
        let hub = Hub::new(&cfg.session_id);
        let registry = letibot_sessionlog::registry::Registry::new();
        Harness::place_the_tools(&mut cfg, &hub, &registry).expect("the parent's tools are placed");
        let parent = cfg.view_grants.clone().expect("a record at open");

        // The child's config as `children.rs` assembles it: the parent's, with its own id and
        // parent, and no field of its own for the record.
        let mut child = Config {
            session_id: "sub-1794122961532460811".into(),
            parent_session_id: Some("s-view-parent".into()),
            ..cfg.clone()
        };
        Harness::place_the_tools(&mut child, &hub, &registry)
            .expect("the child's tools are placed");
        let theirs = child
            .view_grants
            .clone()
            .expect("a record at the child's open");
        assert!(
            std::sync::Arc::ptr_eq(&parent, &theirs),
            "a child was opened with a record of its own, so it asks again what its parent answered"
        );
        // The same record, either way round: the grant one of them gets is the other's.
        theirs.record(std::path::Path::new("/build"), false);
        assert_eq!(
            parent.covers(
                std::path::Path::new("/build/letibot-r114/review"),
                ViewAccess::Read
            ),
            Some(false)
        );
        hub.close();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_wide_seat_tests {
    use super::*;

    fn warned(hub: &Hub, code: &str) -> Vec<String> {
        hub.retained()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::Warning {
                    code: c, detail, ..
                } if c == code => Some(detail.clone()),
                _ => None,
            })
            .collect()
    }

    /// A `leticode` session with a shell on Linux is bounded by the wide seat, or REFUSED: what it
    /// must never be is this user's rights over the whole host with a warning. Both branches are
    /// asserted, because a box that cannot nest a namespace can only measure the refusal.
    #[test]
    fn a_leticode_session_with_a_shell_is_bounded_or_refused_and_never_whole_host() {
        let dir = std::env::temp_dir().join(format!("letibot-linuxwide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a fixture workspace");
        let mut cfg = Config::for_this_box(&dir);
        cfg.seat = Seat::Leticode;
        cfg.unconfined = true;
        cfg.allow_bash = true;
        cfg.session_id = "linux-wide-seat".into();
        let hub = Hub::new(&cfg.session_id);
        let registry = letibot_sessionlog::registry::Registry::new();
        let placed = Harness::place_the_tools(&mut cfg, &hub, &registry);
        let refused = warned(&hub, BOUNDARY_ABSENT);
        let unconfined = warned(&hub, UNCONFINED_SESSION);
        hub.close();

        match placed {
            Ok(p) => {
                assert!(p.confined, "a wide seat is a boundary: {}", p.described);
                assert!(!p.whole_host, "{}", p.described);
                assert!(refused.is_empty(), "a boundary was built: {refused:?}");
                assert!(
                    unconfined.is_empty(),
                    "it is not unconfined: {unconfined:?}"
                );
                let boundary = p.backend.boundary().expect("a measured boundary");
                assert!(boundary.view.wide, "the view is the machine itself");
                assert!(
                    matches!(boundary.view.home, letibot_tools::exec::HomeView::Dir(_)),
                    "the wide seat's $HOME is a real directory"
                );
                assert!(
                    !boundary.view.masked.is_empty(),
                    "the stores are the fence, and there are some"
                );
                let brief = view_brief(Some(&boundary)).expect("the view a model is told");
                assert!(brief.contains("AROUND this machine"), "{brief}");
                assert!(!brief.contains("not on this machine's disk"), "{brief}");
            }
            Err(e) => {
                let m = format!("{e}");
                assert!(
                    m.contains("wide seat") && m.contains("UNCONFINED"),
                    "a refusal has to say what it refused: {m}"
                );
                assert!(
                    !refused.is_empty(),
                    "the head is told the boundary was not built: {m}"
                );
                assert!(
                    unconfined.is_empty(),
                    "a refused session never says it runs unconfined: {unconfined:?}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `[boundary]` that does not parse, or asks for `~/.ssh`, refuses the session with the
    /// file and line. It never opens the unconfigured view.
    #[test]
    fn a_boundary_config_that_is_refused_refuses_the_seat() {
        for (n, body, want) in [
            ("syntax", "[boundary\nhide = []\n", "leticode.toml"),
            (
                "ssh",
                "[boundary]\ngrants_ro = [\"~/.ssh\"]\n",
                "leticode.toml:2: [boundary] grants_ro",
            ),
        ] {
            let dir = std::env::temp_dir().join(format!(
                "letibot-boundary-refused-{n}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a fixture workspace");
            std::fs::write(dir.join("leticode.toml"), body).expect("a config");
            let mut cfg = Config::for_this_box(&dir);
            cfg.seat = Seat::Leticode;
            cfg.unconfined = true;
            cfg.allow_bash = true;
            cfg.session_id = format!("boundary-refused-{n}");
            let hub = Hub::new(&cfg.session_id);
            let registry = letibot_sessionlog::registry::Registry::new();
            let placed = Harness::place_the_tools(&mut cfg, &hub, &registry);
            hub.close();
            match placed {
                Ok(_) => panic!("{n}: a refused config opened a session"),
                Err(e) => assert!(format!("{e}").contains(want), "{n}: {e}"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(test)]
mod floor_layer_tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::{Duration, Instant};

    use letibot_sessionlog::Registry;
    use letibot_sessionlog::client::{HeadClient, Inbound, pump};
    use letibot_sessionlog::event::SessionEvent;
    use letibot_sessionlog::protocol::{Caps, ServerFrame};
    use letibot_sessionlog::server::serve_registry;
    use letibot_tools::{Access, GateCall, GateDecision};

    use crate::config::{Config, ProviderConfig, Seat};
    use crate::sessions::Sessions;

    /// How long a card may take. The head adjudicator's own budget is longer, so a wait that runs
    /// out here is this fixture failing rather than the product timing out.
    const PATIENCE: Duration = Duration::from_secs(20);

    /// A scratch workspace, carrying the committed `prod-floor` layer in the PROJECT scope when
    /// `with_layer` - `.letibot/plugins` is where a project's reviewed policy lives.
    fn workspace(tag: &str, with_layer: bool) -> PathBuf {
        let dir = letibot_sessionlog::testing::scratch_root().join(format!(
            "letibot-floor-{tag}-{}-{}",
            std::process::id(),
            letibot_sessionlog::registry::now_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a workspace");
        if with_layer {
            let layer = dir.join(".letibot/plugins/prod-floor");
            std::fs::create_dir_all(&layer).expect("the layer's directory");
            let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/floor-layer");
            for file in ["plugin.toml", "plugin.wasm"] {
                std::fs::copy(fixture.join(file), layer.join(file)).expect("the fixture");
            }
        }
        dir
    }

    /// The config `plugins_tool.rs` opens with: the byte vocabulary, a provider that is named and
    /// never called, and the point that admits a write inside the workspace without asking.
    fn config(session: &str, ws: &Path) -> Config {
        let mut cfg = Config::for_this_box(ws);
        cfg.seat = Seat::Leticode;
        cfg.session_id = session.to_string();
        cfg.vocab_gguf = None;
        cfg.provider = Some(ProviderConfig {
            name: "deepseek".into(),
            model: None,
            api_key: Some("sk-not-used".into()),
            thinking: false,
        });
        cfg.web_search = None;
        cfg.permission = Vec::new();
        cfg.mode = letibot_tools::mode::Mode::WRITES_ALLOWED;
        let scratch = letibot_sessionlog::testing::scratch_root();
        cfg.store = Some(scratch.join(format!("{session}.db")));
        cfg.socket = scratch.join(format!("{session}.sock"));
        cfg
    }

    /// A session the daemon opened, its config, and the registry its socket is served from. The
    /// `Parts` is leaked because a `Sessions` borrows it for its whole life, which is the shape the
    /// daemon's own loop has: there is one, and it outlives every session.
    fn opened(tag: &str, with_layer: bool) -> (Sessions<'static>, Config, Arc<Registry>) {
        let ws = workspace(tag, with_layer);
        let cfg = config(&format!("s-floor-{tag}"), &ws);
        let parts: &'static crate::Parts = Box::leak(Box::new(
            crate::Parts::load(&cfg).expect("the byte vocabulary loads"),
        ));
        let registry = Registry::new();
        registry
            .create(cfg.session_id.clone(), "", Sessions::wiring(&cfg))
            .expect("a fresh registry");
        let sessions =
            Sessions::open_first(parts, cfg.clone(), registry.clone()).expect("the session opens");
        (sessions, cfg, registry)
    }

    /// The call both tests make: a write inside the session's own workspace, which at
    /// `writes allowed` is settled by the point and asks nobody.
    fn a_write(ws: &Path) -> (String, serde_json::Value) {
        let path = ws.join("notes.md").display().to_string();
        (ws.display().to_string(), serde_json::json!({"path": path}))
    }

    /// Wait for a card and answer it with `option`, handing back the event it was posed as.
    fn answer_a_card(rx: &Receiver<Inbound>, head: &mut HeadClient, option: &str) -> SessionEvent {
        let deadline = Instant::now() + PATIENCE;
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(inbound) => {
                    let ServerFrame::Event(envelope) = inbound.frame() else {
                        continue;
                    };
                    if let SessionEvent::DecisionRequested { req_id, .. } = &envelope.event {
                        head.answer(req_id, option)
                            .expect("the head answers the card");
                        return envelope.event;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(e) => panic!("the head's pump died: {e}"),
            }
            assert!(
                Instant::now() < deadline,
                "no card arrived within {PATIENCE:?}"
            );
        }
    }

    /// The floor at a session the daemon opened: the project's layer says its production hosts ask,
    /// and a write the point would have settled becomes a card a head answers.
    #[test]
    fn a_project_layers_floor_raises_a_daemon_sessions_call_to_a_card() {
        let (sessions, cfg, registry) = opened("card", true);
        let server = serve_registry(registry, &cfg.socket).expect("the socket binds");
        let caps = Caps {
            can_decide: true,
            ..Caps::default()
        };
        let (mut head, _hello, reader) =
            HeadClient::attach(&cfg.socket, &cfg.session_id, 0, "tui", "dead", caps)
                .expect("a head attach");
        let (tx, rx) = channel();
        std::thread::spawn(move || pump(reader, tx));
        // The head's half on its own thread: the gate's is the one that blocks on the answer.
        let answering = std::thread::spawn(move || answer_a_card(&rx, &mut head, "deny"));

        let h = sessions
            .harness_of(&cfg.session_id)
            .expect("the session the daemon holds");
        let (workspace, args) = a_write(&cfg.workspace);
        let call = GateCall {
            name: "write",
            access: Access::Write,
            args: &args,
            turn_id: "t1",
            call_id: "c1",
            workspace: &workspace,
            target_exists: Some(true),
            scripts: &[],
        };
        let decision = h
            .runtime
            .gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .admit(&call);
        assert!(
            matches!(decision, GateDecision::Refuse { .. }),
            "the head denied the card the floor raised: {decision:?}"
        );

        let Some(SessionEvent::DecisionRequested {
            kind,
            options,
            summary,
            ..
        }) = Some(answering.join().expect("the head's thread"))
        else {
            panic!("the floor's raise was not posed as a permission card");
        };
        assert_eq!(kind, "permission", "a raised call is a permission card");
        let ids: Vec<&str> = options.iter().map(|o| o.option_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["allow_once", "deny", "deny_and_tell"],
            "the always-ask ladder, which is what a floor at `ask` merges to"
        );
        assert!(
            summary.contains("notes.md"),
            "the card names the file it is about: {summary}"
        );
        drop(server);
    }

    /// And the control: the same session, the same call, no layer - the point settles it, nobody is
    /// asked, and the gate's own answer is what it was before this seam existed. A session whose
    /// gate asked anyway would fail here, because its adjudicator is a head and no head is attached.
    #[test]
    fn a_session_with_no_floor_layer_decides_as_it_did() {
        let (sessions, cfg, _registry) = opened("bare", false);
        let h = sessions
            .harness_of(&cfg.session_id)
            .expect("the session the daemon holds");
        assert!(
            h.plugin_host().is_some(),
            "the daemon's own host is on the slot, which is what the floor is read through"
        );
        let (workspace, args) = a_write(&cfg.workspace);
        let call = GateCall {
            name: "write",
            access: Access::Write,
            args: &args,
            turn_id: "t1",
            call_id: "c1",
            workspace: &workspace,
            target_exists: Some(true),
            scripts: &[],
        };
        let decision = h
            .runtime
            .gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .admit(&call);
        assert_eq!(
            decision,
            GateDecision::Admit,
            "no layer, no floor: the point settles a write inside the workspace"
        );
    }
}

/// `[boundary] firecode_only` as a route, for a session whose commands run on this machine. A
/// session already inside a VM has nothing to route to, and one with no such key has no route.
fn firecode_route(cfg: &Config) -> Option<letibot_tools::firecode_route::FirecodeRoute> {
    if cfg.placement.is_vm() || cfg.vm_attach.is_some() {
        return None;
    }
    let only = crate::perimeter::load(&cfg.workspace).ok()?.firecode_only;
    (!only.is_empty()).then(|| {
        letibot_tools::firecode_route::production(
            only,
            cfg.workspace.clone(),
            cfg.session_id.clone(),
        )
    })
}
