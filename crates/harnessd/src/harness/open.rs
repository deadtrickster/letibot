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

/// What a VM-placed child copies, or why it cannot be placed: `Ok(None)` is the session's own copy
/// source (`Config::vm_copy_source`). A `task_start` child copies its own worktree and names
/// nothing; a plain VM child of a session holding several repositories is refused, because a VM
/// holds one.
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

    /// [`Harness::open_with`] plus the daemon's session registry, so `task` spawns a real session:
    /// a hub an operator can attach and a store row it can resume. Tests pass a fresh one.
    pub fn open_with_registry(
        parts: &Parts,
        mut cfg: Config,
        hub: Arc<Hub>,
        adjudicator: Option<Box<dyn Adjudicator>>,
        extra_tool: Option<Box<dyn Tool>>,
        session_registry: Arc<letibot_sessionlog::registry::Registry>,
    ) -> Result<Self, HarnessError> {
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
        let mut notes: Vec<String> = Vec::new();
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

        // The mode is a session property, resolved after the workspace is final: the per-project
        // point is looked up now (longest ancestor wins), so a project with no row keeps the
        // daemon's default and one with a row takes its own.
        let mut mode_source = String::from("daemon flag / default");
        {
            // **Read fresh**: another daemon may have written a row since this one started.
            let mut store = parts.mode_store.write().unwrap();
            store.refresh();
            let store = &*store;
            // **The session's own point first** — the one it was put at, which a restart or
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

        // The backend the seat needs, and the constructor is the disclosure: choosing
        // `HostBackend::executable` would be choosing no boundary, which no daemon may do on an
        // operator's behalf, and a downgrade closes doors here too. leticode roots at `/`.
        use letibot_tools::schema::Access as Cls;
        let may_exec = !cfg.downgrade.denies(Cls::Exec);
        let may_write = !cfg.downgrade.denies(Cls::Write);
        // A VM placement is the boundary, so the mode inside is allow-all, and a session attached
        // to an ancestor's VM is inside that VM too. The downgrade still composes on top, because
        // it is the caller's explicit ask.
        let in_vm = cfg.placement.is_vm() || cfg.vm_attach.is_some();
        // **A VM on THIS box**, which is the local firecode backend's door. A VM on a declared
        // host is booted by that host's own `firecode` over ssh, so it goes through the
        // placement below instead.
        let local_vm = cfg.vm_attach.is_some()
            || cfg.placement == letibot_tools::builtins::task::Placement::Firecode;
        if in_vm {
            cfg.mode = letibot_tools::mode::Mode::ALLOW_ALL;
        }
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
            hub.publish(SessionEvent::Warning {
                code: WORKSPACE_GONE.into(),
                detail: said.clone(),
                compaction: None,
            });
            return Err(HarnessError::Setup(said));
        }
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
        } else if cfg.unconfined {
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
            let built = HostBackend::confined_granting(
                &cfg.workspace,
                confined_grants(&cfg, cfg.landing_root.as_deref()),
            );
            match built {
                Ok(b) => b,
                // A Mac whose boundary could not be built is unconfined and says so, on the
                // session's hub and in the daemon's log. On Linux this arm stays a refusal: bwrap
                // and a delegated cgroup are things a host can be fixed to have.
                #[cfg(target_os = "macos")]
                Err(e) => {
                    let said = format!(
                        "this session is UNCONFINED, and its commands run with this user's \
                         rights over this whole host. A macOS boundary was asked for and could \
                         not be built: {e}\n\nWhat that means here, in the words the gate uses: \
                         a command that runs code (a test runner, a build) is decided per \
                         command by the operator, because nothing else is stopping what that \
                         code can read. `docs/boundary-and-adjudication.md` §4k says what the \
                         boundary would have given and why the probe refused."
                    );
                    eprintln!("letibot: {said}");
                    hub.publish(SessionEvent::Warning {
                        code: BOUNDARY_ABSENT.into(),
                        detail: said,
                        compaction: None,
                    });
                    let b = if cfg.allow_bash {
                        HostBackend::executable("/")
                    } else {
                        HostBackend::writable("/")
                    }
                    .map_err(|e| HarnessError::Setup(format!("whole-host backend: {e}")))?;
                    b.with_cwd(&cfg.workspace).map_err(|e| {
                        HarnessError::Setup(format!("workspace {:?} as cwd: {e}", cfg.workspace))
                    })?
                }
                #[cfg(not(target_os = "macos"))]
                Err(e) => {
                    return Err(HarnessError::Setup(format!(
                        "the `{}` role needs a confined execution backend and one could not be \
                         built over {:?}: {e}.\n\nThis is a refusal, not a degradation: the \
                         alternative is `HostBackend::executable`, which gives a process the \
                         cgroup that bounds its lifetime and NO boundary on what it can read \
                         — the operator's whole filesystem, with a banner that would have to \
                         say so. Two things it needs and the error above says which is \
                         missing: a delegated cgroup v2 subtree, and a usable unprivileged \
                         namespace boundary (bwrap). A third thing the message above may \
                         name instead: the project root itself. Both `runner` and `coder` \
                         root their view at the workspace, so a workspace that is a file, \
                         a dangling symlink or absent fails here and no boundary is the \
                         wrong thing to blame. Seat `--role orchestrator` for a session \
                         with no exec path at all.",
                        cfg.seat.as_str(),
                        cfg.workspace
                    )));
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
        // the surroundings the gate will also carry — so a `~` and the gate's own
        // `region_of` agree about where the operator lives.
        let surroundings = surroundings_for(&cfg);
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
                // The session's network rule, which the boundary expresses: denied unless the
                // point seats network calls, and `Ask` counts as seated, because a boundary that
                // refused a call after the card said yes would be a second decision nobody made.
                egress: if cfg.mode.seats(letibot_tools::schema::Access::Network) {
                    letibot_tools::exec::Egress::Host {
                        why: format!(
                            "the `{}` mode seats network calls, so a confined command on {} \
                             shares its network namespace rather than being handed an empty one",
                            cfg.mode.name, host.name
                        ),
                    }
                } else {
                    letibot_tools::exec::Egress::Denied
                },
                // **Ask the host, which is what production always does.** A placement probes once
                // per host (cached), and a host that cannot answer runs unconfined with the reason
                // on the session's own notes — see `SshBackend::place`.
                confine: None,
                // **R100: the same placement, with a VM booted on that host.** `--host NAME
                // --vm` places the tools in a firecode guest there; the probe and the boot are
                // `SshBackend::place`'s, and a host that cannot boot one is refused by name.
                vm: cfg.placement.is_vm(),
            };
            let sb = letibot_tools::remote::SshBackend::place(&spec)
                .map_err(|e| HarnessError::Setup(e.to_string()))?;
            // **Where the session is, and what was left behind.** Published rather than
            // logged: a tree the operator is not looking at has to say so on the surface
            // they read, including the local edits that were NOT carried.
            for note in sb.placement_notes() {
                hub.publish(SessionEvent::Warning {
                    code: REMOTE_PLACEMENT.into(),
                    detail: note.clone(),
                    compaction: None,
                });
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
                h.protect_listener(
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
                        // says out loud was named before this ran — see `name_runs` above.
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
        // **The slot a child of this session reads to join the tree** (R58). Declared
        // here, before the runner that holds it, and filled once `job_watch` exists
        // below — see `HarnessTaskRunner::tree_watch`.
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
        let task_runner: Arc<dyn letibot_tools::builtins::task::TaskRunner> =
            Arc::new(HarnessTaskRunner {
                vocab: parts.vocab.clone(),
                wiring: parts.wiring.clone(),
                mode_store: parts.mode_store.clone(),
                registry: session_registry.clone(),
                base: cfg.clone(),
                tasks: parts.tasks.clone(),
                skills: parts.skills.clone(),
                lsp: parts.lsp.clone(),
                child_boards: parts.child_boards.clone(),
                // And the daemon's review desk, for the same reason: the child's own harness
                // registers `review_verdict` bound to its own session id, and the desk it submits to
                // has to be the one this session opened the review on.
                reviews: parts.reviews.clone(),
                slots: parts.tree_slots.clone().unwrap_or_default(),
                // Filled below, once this harness has built its own `job_watch` (R58).
                tree_watch: tree_watch_slot.clone(),
                // Filled as this session's gate is built: a root's own answers, or the ones
                // its tree inherited (see `HarnessTaskRunner::head_answers`).
                head_answers: head_answers_slot.clone(),
                local_window: local_window_cell.clone(),
                point: point_cell.clone(),
                foreign_vocabs: Default::default(),
                placed: Arc::new(std::sync::Mutex::new(Vec::new())),
                merge_store: Arc::new(std::sync::Mutex::new(None)),
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
        let plugin_slot: Arc<std::sync::Mutex<Option<Arc<crate::plugins::PluginHost>>>> =
            Default::default();
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
        let top_level = match cfg.parent_session_id {
            None => true,
            Some(_) => crate::room::tag_of(&cfg.session_id).is_some(),
        };
        //
        // A connection that cannot be made is said once and is not fatal: a session whose room is
        // unreachable is a session that works, and its `room` tool refuses by name.
        let room = match (cfg.room_link, top_level, crate::room::socket_path()) {
            (true, true, Some(socket)) if socket.exists() => {
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
                // Which room this session is in: the project its chain hangs from, keyed the way
                // the rest of the tree keys a folder. A session the daemon cannot place is in the
                // box's own room, which is [`letibot_room::DEFAULT_PROJECT`].
                let project_root = crate::room::roster::Roster::new(session_registry.clone())
                    .project_of(&cfg.workspace.display().to_string());
                let project = if project_root.is_empty() {
                    letibot_room::DEFAULT_PROJECT.to_string()
                } else {
                    letibot_room::project_key(&project_root)
                };
                // **The tree, which the room cannot see for itself.** One roster per daemon would
                // be one cache per daemon; this is one per open, which is what the link needs and
                // nothing more — `Registry::list` is the same call the plugin feed makes per frame.
                let roster: std::sync::Arc<dyn crate::room::roster::RosterSource> =
                    crate::room::roster::Roster::new(session_registry.clone());
                match crate::room::link::SessionRoom::connect(
                    &cfg.session_id,
                    &socket,
                    &project,
                    Some(roster),
                    bell,
                    supervisor,
                ) {
                    Ok(room) => Some(room),
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
        if let Some(room) = &room {
            registry
                .register(Box::new(letibot_tools::builtins::room::RoomTool::new(
                    room.clone(),
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
            task_runner,
            parts.skills.clone(),
            parts.lsp.clone(),
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
        // Standing notes, written: the loop was one-way until the `notes` tool, scoped to the
        // stored workspace and the box-wide dir. Registered for every session so a role that names
        // it resolves; the ROLE decides who seats it.
        registry
            .register(Box::new(letibot_tools::builtins::notes::NotesTool::new(
                std::sync::Arc::new(crate::notes_scope::ConfigNotes::new(
                    cfg.workspace.clone(),
                    cfg.global_notes(),
                )),
            )))
            .map_err(|e| HarnessError::Setup(format!("registering the notes tool: {e}")))?;
        registry
            // The box-wide `[memory]` under the session's project's own (R76 item 7): the
            // project is this session's workspace, settled above, not the daemon's.
            .register(Box::new(letibot_tools::builtins::bash::Bash::with_memory(
                crate::leticode_config::memory_policy(
                    cfg.memory,
                    &cfg.leticode_user,
                    &cfg.workspace,
                    cfg.read_project_config,
                ),
            )))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::jobs::JobList)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::jobs::JobOutput)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::jobs::JobWait)))
            .and_then(|_| {
                registry.register(Box::new(
                    letibot_tools::builtins::jobs::JobKill::with_tasks(subagent_runner.clone()),
                ))
            })
            .and_then(|_| {
                // **The monitor tool, recorded** — each declaration's own arguments go to the
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
                                hub.publish(SessionEvent::Warning {
                                    code: "transcript_store".into(),
                                    detail: e,

                                    compaction: None,
                                });
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
                            hub.publish(SessionEvent::Warning {
                                code: "decision_corpus".into(),
                                detail: e,

                                compaction: None,
                            });
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
        // backend. One read, one seat, where the two used to be resolved separately and disagree.
        let role = role_for(&cfg);
        let registry = registry
            .resolve_role(&role)
            .map_err(|e| {
                HarnessError::Setup(format!("seating the `{}` role: {e}", cfg.seat.as_str()))
            })?
            // The downgrade, applied to what is SEATED: a denied class's tools leave
            // the prompt, so the model is not told it has what the gate would refuse.
            .without_access(&cfg.downgrade.deny);
        // **And a skill that declares `allowed-tools` may only NARROW this seat** (D17: a skill
        // is tighten-only, never a pre-approval). Read over the skills this session was STARTED
        // with: a skill loaded mid-conversation narrows nothing, because a session's tool list is
        // its prompt and message zero is frozen.
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
        // **And the tools this session's PLUGIN LAYERS declare** (`docs/plugins-plan.md` §4.8).
        //
        // After the role, because a layer's tool is not one the role names — it is a seat the
        // project added, and `resolve_role` would refuse a role naming a tool the role's list does
        // not carry. The declarations are read from `plugin.toml`s only (`crate::plugins::
        // declared_tools`): the prompt is composed here and the plugin host is built later by the
        // daemon, so the tool holds the same slot the `plugin` tool does and answers through it
        // once it is filled.
        //
        // **A layer IN FORCE, and not the binary's own table** — a built-in's `[[tools]]` is what a
        // project installs (a `plugin.toml` copied into `.letibot/plugins/`, no `plugin.wasm`), not
        // a tool every session is handed. That is `declared_tools`'s decision and the reason it
        // reads three scopes; `crates/harnessd/tests/wired.rs` is where the default set it must not
        // move is written down, and `a_layer_in_force_seats_its_tool_and_no_layer_seats_none`
        // beside it is the other half.
        //
        // A tool that cannot be seated says why, once, in a `plugin_layer` warning — the class the
        // plugin host already uses for a layer whose answer was refused. Silence would be a model
        // that does not know a project offered it something.
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
            // **Somebody else's prose, so `register_foreign`** — the lint's findings are recorded
            // on the registry rather than refusing the tool. A layer's description is a file a
            // person wrote, and refusing a project's own tool over its wording would be the
            // harness making a policy nobody asked for; `plugin test` is where the wording is
            // reported back to the model that can fix it.
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
            hub.publish(SessionEvent::Warning {
                code: "plugin_layer".into(),
                detail: format!(
                    "a layer declares a tool this session did not seat: {}. The layer is still \
                     loaded; `plugin {{\"action\":\"stack\"}}` lists what is in force.",
                    not_seated.join("; ")
                ),
                compaction: None,
            });
        }
        let schemas = registry.schemas();
        let seated: Vec<String> = schemas.iter().map(|s| s.name.clone()).collect();
        // What the prompt is composed from: the tools actually seated, after the role and the
        // downgrade, and the scratch directory the backend actually reaches. Read here, where both
        // are facts, and not off the role's intention before open.
        let seat_facts = crate::config::SeatFacts {
            tools: seated.iter().cloned().collect(),
            scratch: backend.scratch_dir(),
            subagent: cfg.parent_session_id.is_some(),
            // A linked worktree's `.git` is a file naming the main repository's gitdir.
            worktree: cfg.workspace.join(".git").is_file(),
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
            // The guard model, by the same fact the advisor is built from: `cfg.oracle` is `Some`
            // when an endpoint is known. Nothing used to push this, so `automode` could not open
            // however the box was configured, and the refusal named a line the file already had.
            // Reachability is not tested here: a TCP probe at open would wait on another box.
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
                    // The row says where the point came from AND that it was clamped — a
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
        // **What this session will hold, and the ceiling it states for itself** — see
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

        // **The gate, built here so the three seams cannot be skipped.** See
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
                        // The tree's route to a person, recorded with the sink — a subagent's
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
                    // Off unless a `leticode.toml` says otherwise: a host with no boundary then
                    // asks about every test runner, build and project script, because asking about
                    // every build on an unconfined box buys `allow-all` rather than safety.
                    .with_unconfined_code_asks(cfg.leticode.permissions.ask_unconfined_code())
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
                    // Where the tools actually run, so a card tells the truth about the boundary
                    // instead of reciting one fixed sentence. Read off the backend above.
                    .with_boundary(backend_boundary, backend_boundary_refusal)
                    // What the agent says it is doing, so the guard can connect the operator's
                    // words to a call that does not repeat them. Outside the trail's numbering, so
                    // it can never be what an `ALLOW <n>` cites.
                    .with_agent_claim({
                        let t = trail.clone();
                        move || t.claim()
                    })
                    // §4b. Without this a denial reaches the model and stops there.
                    .with_denial_sink(Box::new(HubDenials::new(hub.clone())))
                    // **And the other direction** (D16): a call the gate admitted without asking
                    // is told, not only recorded — see [`Harness::flush_gate_admissions`].
                    .with_admission_sink(Box::new(HubAdmissions::new(gate_admissions.clone())));
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
                // The shape cache, warm: it lived on the gate and died with the process, and the
                // operator restarts a session exactly when one has gone wrong. Seeded from the
                // store, bounded in SQL, and re-checked at every lookup. A store that cannot
                // answer is a note rather than a refusal to start: being asked is the safe side.
                let mut g = g;
                // A narrowing in force is disclosed, like every other one: an operator who forgot
                // they set it would read the cards as a regression. Silence is the default because
                // the default is off.
                if cfg.leticode.permissions.ask_unconfined_code() {
                    notes.push(format!(
                        "[permissions] ask_unconfined_code is ON{}: a session with no measured \
                         boundary asks about every test runner, build and project script, every \
                         time — no session grant, no approved shape and no `permission` rule \
                         settles one (R98). Only the per-call answer does",
                        match cfg.leticode.path.as_deref() {
                            Some(p) => format!(" ({})", p.display()),
                            None => String::new(),
                        }
                    ));
                }
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

        // The encoder, attached once for the life of the session: constructing `IntentSink` is what
        // tells the ledger an encoder exists, and it lives on the harness rather than per round so
        // it is attached before the banner is read and its call_id map spans a whole turn.
        // The background-job watcher: a job backgrounded this session settles
        // between turns, when nobody else is publishing, so the daemon watches
        // each one and publishes the settlement. `None` when the backend cannot
        // start processes — no host, no jobs, no threads. The sink is the hook
        // because the `Backgrounded` result is where a job id first exists.
        //
        // **And it is given the worker's bell (R7).** A settlement was reaching the
        // heads and nobody else: the model that could act on it had no route to a
        // background job's result but `job_wait`. The watcher now also queues a
        // completion and rings this bell, so the settlement arrives as an unprompted
        // turn of its own — the same route a fired monitor takes.
        // **Built whether or not this session can start a process.** `task` needs no
        // process host, so keying the watcher set on one would leave a read-only seat that
        // hands work to a child with no way to be told the child finished — the same
        // defect surviving in the sessions least able to notice it.
        let bell = Some(Arc::clone(session_registry.bell()));
        let processes = backend.processes_arc();
        let job_watch = match &processes {
            Some(h) => JobWatchers::new(h, &hub, bell),
            None => JobWatchers::watching_tasks(&hub, bell),
        };
        // **And the session's subagents settle through it too.** `task` returns the
        // same `Backgrounded` outcome `bash --background` does, so the sink below
        // already arms a watcher for it — and that watcher asked the host about a name
        // the host has never heard, answered `NeverStarted`, and spun on it until the
        // session closed: the completion never queued, the bell never rung, and a
        // thread leaked per `task` call. Giving the watchers the runner is what makes
        // the handle route to its own wait (R7's hop, for a subagent).
        let job_watch = Some(job_watch.with_tasks(&subagent_runner));
        // **And a child joins its PARENT's set** (R58, and the tree read as a supervision tree
        // in 2026-10-06). What joining gives it is three facts, and none of them is a ring
        // target any more:
        //
        //   * the `watching`/`settled` bookkeeping, so a tree does not watch or settle one
        //     handle twice;
        //   * **the level above** — whose queue what this session can no longer drain goes to,
        //     and whose id the ring for it names;
        //   * **the ROOT**, which the ask route posts a permission card to (`tree_root`), because
        //     that is the one session in a tree a head can be attached to.
        //
        // The RING is not among them: it names the session that OWNS a settlement — this one,
        // for what this session starts — and the daemon serves a session it does not hold by
        // handing the wake to the thread that does. Ringing the root instead is what put a
        // grandchild's notice somewhere nobody was looking; see `jobwatch`'s module header.
        // A root has no tree to join (`tree_watch` is `None` from `Parts::load`) and keeps its
        // own set, whose ring names itself.
        //
        // **The queue is NOT joined with it** (2026-10-05). A child's set keeps its own
        // completions, so what it backgrounds is drained by the child — and a settlement
        // that arrives after the child's turn is over is handed up one level by
        // `JobWatchers::stop` rather than read by the main session as its own.
        let job_watch = match (&parts.tree_watch, job_watch) {
            (Some(tree), Some(w)) => Some(w.shares_tree(tree)),
            (_, other) => other,
        };
        // **And this session's own set is what ITS children will join** (R58). Filled
        // here, after the join above, so the value a child reads is already the joined
        // one — which is how the ROOT (the session a permission card can be drawn by)
        // stays the tree's root at every depth without anything walking a parent chain.
        if let Some(w) = &job_watch {
            *tree_watch_slot.lock().expect("tree watch") = Some(Arc::clone(w));
            // **Where a settlement gets written down.** The watcher threads settle jobs on
            // their own thread and cannot be handed this harness's `Store` — a
            // `rusqlite::Connection` is `Send` and not `Sync` — so they are given the store's
            // PATH and open a handle onto the process's one connection; `Store::path` says why
            // that is ordinary and why it answers `None` for an in-memory store. A run with no
            // store records nothing, which is the same answer it gives for todos.
            //
            // **This session's own id, not the tree's**, for the reason the queue is per
            // session: a job a child backgrounded is the child's row, in the child's session.
            if let Some(path) = store.as_ref().and_then(|s| s.path()) {
                w.record_jobs_into(
                    path.to_path_buf(),
                    cfg.session_id.clone(),
                    session_registry.instance(),
                    // **Where this session's jobs run**, which is what the supervisor's record
                    // says in its `host` column — `None` for this box, a `hosts.toml` name
                    // otherwise. It is also what decides how a job's identity is spelled: a
                    // pid on this box, a command marker on another machine.
                    match &cfg.placement {
                        letibot_tools::builtins::task::Placement::Remote(name) => {
                            Some(name.clone())
                        }
                        _ => None,
                    },
                );
            }
        }
        // **And the jobs a daemon that died was still running** — RH7's supervisor, schema 34.
        //
        // The record is what outlived that daemon. Adopting it puts the job back in this host's
        // own table, and then every reader above this layer — `jobs`, `job_output`, `job_kill`,
        // `job_wait`, the pane, the settlement that wakes this session — works exactly as it did
        // before the restart, which is the whole reason the record exists. A job whose process is
        // already gone settles as [`letibot_tools::exec::JobState::Dead`] — *gone, and no exit was
        // recorded* — never as an exit of zero.
        //
        // A record this backend cannot adopt (a remote host's, in this slice) is left alone: it
        // stays open for [`crate::supervise`] to check, rather than being dropped or, worse,
        // settled as ended by a daemon that cannot see the process.
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
        );

        // Read, never asserted. `is_writable` is the backend's own answer,
        // `describe` is the backend's own words, `Gate::describe` is the gate's, and
        // the access-class questions are answered by the schemas that were actually
        // seated a few lines above — not by which role was requested. Four instances
        // of one defect were paid for to learn that distinction.
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
            // A wake needs three things and this reads all three: a monitor
            // registry (so there is something to watch), the `monitor` tool seated
            // (so something can declare a watch), and `Sessions::watch` having
            // armed the waiter. The third is the daemon's and is stamped by
            // [`Harness::declare_monitor_wake`] when it arms — a harness driven
            // directly by a test has no daemon and honestly says `false`.
            monitor_wake: false,
            // **Read from the sink, not from the config.** `--store` being set is
            // not the same fact as a corpus connection having opened, and the
            // counts come from the table rather than from a count this process
            // kept — which would read as zero on a store full of earlier runs.
            corpus: corpus_counts,
            // **Asked once, here, and only where a subagent can be started** — FIRECODE-NOTES
            // item 4: a VM placement's preconditions were found out at spawn, one refusal per
            // attempt. The banner says them up front.
            firecode,
        };

        let spiller = build_spiller(&cfg)?;
        // **The question a long tool asks before spending another minute.**
        //
        // `digest` folds two dozen parts and `job_wait` holds a deadline; for the
        // whole of either, this call is all the session is doing and nothing polls
        // the head. So the hub's own answer goes down into the tool runtime, and a
        // tool that is about to spend real time can find out that the operator is
        // waiting and stop. See `InvokeCtx::operator_waiting`.
        let waiting_hub = hub.clone();
        // **And the fact that closes R23.** `job_wait` on a job the daemon is already
        // watching returns at once rather than blocking on an answer that is in
        // flight — see `InvokeCtx::completion_delivered`. Wire to the same watcher
        // set the sink feeds, so "there is a watcher" and "the wait must not block"
        // are one fact read in two places rather than two facts that can disagree.
        // `None` when the backend cannot start processes: no host, no jobs, no
        // watcher, and every wait keeps its old behaviour.
        let delivering = job_watch.clone();
        // **And the other half of that fact (R79)**: a wait, a promoted call or `wake: "now"`
        // tells the same set the result is wanted the moment it ends, so it is not held for the
        // next break. See `crate::delivery`.
        let wanting = job_watch.clone();
        let runtime = ToolRuntime::new(registry, backend)
            .with_spiller(spiller)
            .with_gate(gate)
            // **And the plugin layer that sees a call before the gate** (`docs/plugins-plan.md`
            // §4.1), and the one a `monitor` call can arm a condition from (§4.4). Both are backed
            // by the same slot the `plugin` tool asks through: the runtime is built here and the
            // host when the DAEMON opens the session, so a session nobody fills the slot for — a
            // subagent — runs its calls and its monitors exactly as it always did.
            .with_call_hook(std::sync::Arc::new(crate::plugins::CallHooks::new(
                plugin_slot.clone(),
                hub.clone(),
            )))
            // **A one-shot's session tells its tools so**, which is what makes `bash` refuse
            // `background: true`: there is no later head and no daemon left watching when that
            // job's result arrives. See `InvokeCtx::one_shot`.
            .with_one_shot(cfg.one_shot)
            // **Who wrote a layer's bytes, recorded at the write** — D17's third arm. A `write` or
            // an `edit` that lands a `plugin.toml`, a module or a file beside one appends to the
            // trust book, so *who put this here* is a fact on disk rather than a guess from a path.
            // The book is the file the plugin host's stack also reads, so a record taken here is
            // visible to the layer's own trust decision at its next poll.
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
            // **The operator's own run can be ANSWERED** — see `crate::prompt`.
            //
            // Three reports and one object, and the object is installed on the registry
            // here because **this is the half that has the pipe**: the exec host was built
            // a few lines above, and the write end of the operator's stdin came out of the
            // job the tool spawned. The server's reader thread is the half that needs it,
            // and it cannot ask the run — the run's own thread (see `crate::bangrun`) is
            // blocked inside the very command that is asking.
            //
            // Keyed by session: a pipe belongs to one session's run, and a `!send` in one
            // session must never write into another's command.
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

        // **And the plugin conditions this session's monitors may arm** (`docs/plugins-plan.md`
        // §4.4). Installed on the process host's own `Monitors`, because that is what the `monitor`
        // tool asks for everything else it needs — and a session whose host cannot start processes
        // has no monitors to install into, which is the same sentence a `monitor` call already gets.
        //
        // **The runtime is shared from here on**, because the gate inside it is what a look is now
        // adjudicated by the ladder of — without asking (`Gate::admit_unasked`), so a watch can
        // neither park its own thread on a card nor do what the model could not. Wrapping it here,
        // rather than at the `Harness` literal below, is what lets the monitors hold the same gate
        // the session's calls do: two gates would be two answers to *what may run*.
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
            // a wall-clock meter, and a structural prefix guarantee — so §18.1-I1's
            // post-flight assertion is allowed to run rather than being skipped.
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
        // And this endpoint's media marker — see `engine_for`, whose construction this duplicates.
        engine.media_marker = cfg.media_marker.clone();

        // **The standing notes are composed HERE, at open, and not only at a base rebuild.**
        //
        // `reseat_target` re-reads them on every fork — the schedule the operator asked for,
        // *"make sure AGENTS.md reread after each compaction"* — but a fork was the ONLY thing
        // that ran it, so a FRESH session's message zero carried no notes at all: they arrived
        // at the first compaction, which for a long session is hours in. Measured 2026-10-10
        // by a child chasing an unrelated red: in a workspace with a note, a fresh harness's
        // `reseat_target` returned `Some` (its `next` differed from its seated prefix), which
        // is also why `wired.rs::reingest_writes_no_summary_and_says_so_in_the_note` failed in
        // any worktree that had one.
        //
        // A RESUMED session is unaffected: its prefix comes from the store, and this only
        // decides what a NEW conversation's message zero says.
        // **Named `standing` and not `notes`**: there is a `let mut notes: Vec<String>` alive in
        // this function from the resume-report path, and a local called `notes` here shadows it
        // for every later use in the body.
        // **And the skills this session was STARTED with**, whole: `task {skills}` put them on
        // the spec, a declared seat read them from its config, and a session started by a parent
        // inherited its list. The bodies land inside the same section as the index they are named
        // in, so a fork re-reads both together.
        let declared = crate::standing_notes::preloaded(parts.skills.as_ref(), &cfg.skills);
        let standing = crate::standing_notes::section_with_skills(
            &cfg.workspace,
            &cfg.global_notes(),
            &cfg.box_dirs,
            parts.vocab.as_ref(),
            Some(parts.skills.as_ref()),
            declared.as_deref(),
        );
        // **And the base prompt is composed HERE when the session asked for it**
        // (`Config::seat_prompt`): for the tools just seated, with the notes, the
        // session's own paragraph and its fabric block in that order. A config without
        // it keeps the `system` it was given — a test's fixture, `--system`.
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

        // A session already in the store is **resumed**, not opened a second time.
        //
        // Deciding it here rather than on a flag is what makes the two entry points
        // agree: `harnessd --session X` on a restart, and a head asking a running
        // daemon to resume X, are the same act and used to be two — the first of
        // which failed with a UNIQUE constraint on `session.id` after doing the
        // whole vocabulary load.
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

        let (transcript_id, session, persisted, resume, prefix, prefix_id, stale) = match resumed {
            Some((s, transcript_id)) => {
                let loaded = s
                    .load_transcript(&transcript_id)
                    .map_err(|e| HarnessError::Store(e.to_string()))?;

                // **The refusal that has to stay.** The tokens in this transcript
                // were produced by one renderer against one vocabulary. Appending to
                // them with a different dialect would put two templates' bytes in
                // one prompt, and nothing downstream can see it: the chain still
                // verifies, because every row was hashed by whoever wrote it.
                let stored_meta = s
                    .stable_prefix_meta(&loaded.stable_prefix_id)
                    .map_err(|e| HarnessError::Store(e.to_string()))?;
                let stored_sha = stored_meta
                    .as_ref()
                    .map(|m| m.dialect_sha.clone())
                    .unwrap_or_default();
                // **And the vocabulary that cut the stored ids.** A resume replays them as
                // numbers, so ids from the byte vocabulary under a GGUF — or the reverse, or
                // under another dialect's byte table — are valid numbers that mean other
                // text. Same remedy as a dialect change, from the same record. Two GGUFs are
                // compared nowhere here, as before: a local model's file is the operator's
                // binding, refused at the switch rather than guessed at by path.
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
                        .open(&format!("{transcript_id}#reprefill-probe"), &prefix)
                        .map_err(|e| {
                            HarnessError::Setup(format!("rendering the new prompt: {e}"))
                        })?;
                    let rec = StablePrefixRecord {
                        dialect_sha: dialect_sha.clone(),
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
                        .open(&new_id, &prefix)
                        .map_err(|e| HarnessError::Setup(format!("opening the fork: {e}")))?;
                    // Each item rendered against the history as it stood before it,
                    // which is `render_incremental`'s contract and what keeps the
                    // ledger rows aligned with the items.
                    rebuilt
                        .append_items(&engine, &items, &mut letibot_turn::events::NullSink)
                        .map_err(|e| {
                            HarnessError::Setup(format!(
                                "re-rendering this conversation under {}: {e}",
                                parts.wiring.spec().name
                            ))
                        })?;

                    // **Unsealed until every row is in (R91.2)**: a daemon that dies while
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
                            &prefix_fingerprint(cfg.seat, &prefix),
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
                        notes: std::mem::take(&mut notes),
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
                    )
                } else {
                    // A transcript whose rows do not rebuild is repaired, not refused for ever:
                    // `prefix_repair` forks onto the prefix the rows were really written under and
                    // leaves the old transcript untouched. Anything else keeps its refusal.
                    let mut transcript_id = transcript_id;
                    // The prefix the conversation is seated with — the row's own, or the one a
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
                    // session speaks the prompt this daemon would compose now.
                    let mut stale: Option<String> = None;

                    // The prefix the session's own tokens came from — the store's
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

                    // Message 0 is frozen, and this is where that is held up against what this
                    // daemon would seat now: a build that changed the instructions, the schemas or
                    // the seat composes a prompt the session will never speak, and says nothing.
                    // The comparison is a hash of both sides, computed by `prefix_fingerprint`; a
                    // session seated before the column existed falls back to the stored prefix row.
                    let seated = match repaired {
                        true => own_fingerprint.clone(),
                        false => stored
                            .as_ref()
                            .and_then(|r| r.prefix_fingerprint.clone())
                            .unwrap_or_else(|| own_fingerprint.clone()),
                    };
                    if seated != prefix_fingerprint(cfg.seat, &prefix) {
                        let (was, is) = (
                            tool_schemas(&own_prefix.tools_json),
                            tool_schemas(&prefix.tools_json),
                        );
                        let mut added: Vec<String> = Vec::new();
                        let mut gone: Vec<String> = Vec::new();
                        // **A schema that GREW is named as one**, and it is not the same
                        // finding as a tool that arrived: it is in both lists, so the
                        // added/gone pair is silent about it — and a grown schema is exactly
                        // the case that started this (`todo_write`'s `target`).
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
                        // On the session's own log, and said AT ATTACH rather than here: a warning
                        // published before a head attaches is filed `Placed::Before` and not drawn.
                        // So it goes into the registry, where the attach path publishes it, and the
                        // registry also tells a head that is already watching (the restart case).
                        stale = Some(say);
                    }

                    let rows = session.ledger.rows().len();
                    let report = ResumeReport {
                        transcript_id: transcript_id.clone(),
                        rows,
                        tokens: session.ledger.len(),
                        head: session.ledger_head(),
                        workspace: cfg.workspace.display().to_string(),
                        notes: std::mem::take(&mut notes),
                    };
                    (
                        transcript_id,
                        session,
                        rows,
                        Some(report),
                        own_prefix,
                        seated_prefix_id,
                        stale,
                    )
                }
            }
            None => {
                let transcript_id = format!("{}#t0", cfg.session_id);
                let session = engine
                    .open(&transcript_id, &prefix)
                    .map_err(|e| HarnessError::Setup(format!("opening the session: {e}")))?;
                let mut prefix_id = String::new();
                if let Some(s) = &store {
                    let rec = StablePrefixRecord {
                        dialect_sha: dialect_sha.clone(),
                        system: prefix.system.clone(),
                        tools_json: prefix.tools_json.clone(),
                        tokens: session.ledger.prefix_tokens().to_vec(),
                        h_init: session.ledger.h_init(),
                        vocab_source: parts.vocab.source().to_string(),
                    };
                    prefix_id = s
                        .put_stable_prefix(&rec)
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                    // `INSERT OR IGNORE`-shaped by hand: a session row may already be
                    // here with no rows behind it — the daemon that made it never got
                    // a prompt — and re-opening it is a resume of an empty
                    // conversation, not a collision.
                    if stored.is_none() {
                        s.put_session(&SessionRecord {
                            id: cfg.session_id.clone(),
                            title: Some(cfg.title.clone()).filter(|t| !t.is_empty()),
                            // **Who answers, not what the vocabulary binds.** `cfg.model` is the
                            // local alias whose tokenizer renders the prompt; on a box whose
                            // `providers.toml` names `[default] provider = "deepseek"` the turns
                            // go to deepseek and the row said qwen. This row is what
                            // `letibot --sessions` and the ctrl-s picker draw, so the wrong one
                            // made a deepseek session look like a local one — the operator's
                            // *"with --new it started with qwen, not default model"*.
                            // `Sessions::wiring` is the same fact for the live registry, and the
                            // two are written from one expression so they cannot disagree again.
                            model_id: cfg.prompt_model_name(),
                            dialect_sha,
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
                        // **The fingerprint of what this session was seated with**, written
                        // where its transcript row is written — the one rule the column has,
                        // and the reason it is not a field on `SessionRecord` above: a fork
                        // re-seats a session long after its creation and rewrites this.
                        s.set_prefix_fingerprint(
                            &cfg.session_id,
                            &prefix_fingerprint(cfg.seat, &prefix),
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
                        // **An empty transcript on ANOTHER prompt is re-seated onto the one this
                        // daemon composes, and this is the defect that made it necessary.**
                        //
                        // MEASURED 2026-10-26 in the operator's store: `s-gatekeeper#t0` was
                        // seated 2026-10-11 under a 24,407-token prompt with no rows, a daemon
                        // running newer binaries opened it on 2026-10-25 on its own 32,162-token
                        // prompt, and the rows it appended started at 32162 — in a transcript
                        // whose prefix is 24,407 tokens, so no resume has been able to rebuild it
                        // since (`row 0 starts at 32162, should start at 24407`). The open went
                        // through here: the transcript row already existed, so nothing rewrote
                        // it, and the ledger was seated on the new prompt.
                        //
                        // **(a), re-seat, and not (b), keep the stored prefix.** The frozen-prefix
                        // rule (`prefix_stale`) exists because every row was tokenized under
                        // message 0 and a resume rebuilds the chain from it. With no rows there
                        // is no chain to rebuild and no server cache to keep: the first turn
                        // prefills cold whichever prompt is used. So keeping the old prefix would
                        // buy nothing and cost a brand-new conversation a message 0 that cannot
                        // call the tools seated since — a `prefix_stale` sentence for a
                        // conversation nobody has had yet.
                        //
                        // A transcript that already HOLDS rows is not touched here: those rows
                        // speak the prefix the row names, and moving it would make them
                        // unreadable. `set_transcript_prefix` refuses that case itself — the
                        // `item_count` above is what keeps this arm from reaching it.
                        s.set_transcript_prefix(&transcript_id, &prefix_id)
                            .map_err(|e| HarnessError::Store(e.to_string()))?;
                        // **And the fingerprint follows**, for the reason its column exists: a
                        // session whose fingerprint still describes the old prompt would report
                        // itself stale on the next resume, having just been re-seated onto the one
                        // this daemon composes.
                        s.set_prefix_fingerprint(
                            &cfg.session_id,
                            &prefix_fingerprint(cfg.seat, &prefix),
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
                (transcript_id, session, 0, None, prefix, prefix_id, None)
            }
        };

        // The sentence is left in the registry at the END of `open`, after the stored
        // conversation has been republished — see the comment there.

        // **The window this session plans its compaction against, written down.** See
        // `StoredSession::context_window`: the number was otherwise knowable only at the
        // moment it was used, and a child that ran past its wall left nothing behind saying
        // which wall it had. Written for a root and a child alike — `cfg.context_window` is
        // the resolved number either way, and for a child it is the one the spawn chose for
        // the model that answers IT rather than its parent's.
        //
        // After the match above, because the row it updates is created inside it.
        if let Some(store) = &store
            && let Err(e) = store.set_window(&cfg.session_id, cfg.context_window)
        {
            // **A bookkeeping write failing is not a reason to refuse a session**, and it is
            // not silent either: `notes` is what the operator reads at startup, and this is
            // the one place a reader would look for why the store cannot answer the question.
            notes.push(format!(
                "the window this session plans against could not be written to the store \
                 (`{e}`). It is in force for this run; after a restart nothing on disk says \
                 which window it was."
            ));
        }

        // **The trail is seeded from what was restored, and it says what it lost.**
        //
        // A resumed session's `User` rows are indistinguishable in the store: the
        // operator's prompts, a head's steering and this harness's own notices are
        // all `TranscriptItem::User`, and nothing recorded which was which. Seeding
        // them as the operator's words is the permissive direction for
        // authorisation, so it is a resume note rather than a silent property — the
        // operator can see that this session's adjudications are reading a trail
        // whose speakers were inferred, and start a fresh session if that matters.
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

        // **The standing choice is resolved by the DAEMON, not here.**
        //
        // This used to read `providers.toml` itself, which made opening a harness
        // depend on a file outside the config it was handed — and every test that
        // opens one inherited the operator's live standing choice. Caught by two
        // `compact` tests that assert a DEAD endpoint and got real answers from
        // deepseek instead: four turns and ~22k prompt tokens, billed to the
        // operator, by `cargo test`.
        //
        // `harnessd`'s own startup resolves it into `cfg.provider` before any
        // session exists, and `Sessions` clones that config for every session it
        // opens — so a real daemon behaves exactly as before and a `Config` built
        // in a test carries only what the test put in it.
        // **A key that does not resolve is asked for, not refused** — see `key_wanted`.
        // Any other failure (an unknown preset, an unreadable providers.toml) still
        // refuses here: those are not something a pasted key fixes.
        let mut key_wanted = None;
        let provider: Option<Box<dyn letibot_backend::MessagesBackend>> = match &cfg.provider {
            None => None,
            Some(pc) => match build_provider(pc, &cfg.sampling, provider_notes(&hub)) {
                Ok(p) => Some(p),
                Err(_) if key_is_missing(pc) => {
                    key_wanted = Some(pc.clone());
                    None
                }
                Err(e) => return Err(HarnessError::Setup(e)),
            },
        };
        // **The ledger-to-provider ratio survives a restart, because it is already
        // in the store.**
        //
        // `Config::ledger_scale` is measured at the end of each metered turn and
        // lives in memory, so a restart threw it away and everything that plans
        // against the window — `should_compact`, `room_for_next_turn`, the
        // `/reseat` carry check — fell back to comparing ledger tokens against a
        // provider window until the next turn measured it again.
        //
        // That window is not a rare one: it is exactly the sequence this daemon
        // asks for. Restart to pick up new tools, `/reseat` to get them into
        // message zero — and the re-seat is the first thing that runs, before any
        // turn. The operator hit it within minutes of the restart:
        // *"reseat_unchecked — this session has not taken a metered turn yet"*.
        //
        // Nothing has to be re-measured. `context_tokens` on the session row IS
        // the provider's count for the last prompt, written by `persist_context`
        // at the end of the turn that measured it, and `context_ledger` beside it
        // is this box's count for that SAME prompt (v12) — so the pair is
        // recovered rather than recomputed, and no model is asked anything.
        //
        // **Both halves, or neither.** The paragraph that used to stand here said
        // the ledger "is the one just rebuilt — the conversation has not changed
        // since", and that was an assumption rather than a measurement: it holds
        // only when nothing was appended between the last turn and this restart.
        // The gate below asks the row instead of assuming.
        // **And only when the pair measures THIS conversation.**
        //
        // A ratio needs both counts from the same prompt, and the row now records
        // them together. Without this check the pair was reconstructed from
        // `session.ledger.len()` — TODAY's ledger — which is the same conversation
        // only if nothing was appended since the measurement. At a resume it
        // usually is not, and the error lands the dangerous way: a count from a
        // SMALLER conversation divided into a BIGGER ledger overstates the ratio,
        // so the window grows and the session compacts too late rather than too
        // early.
        //
        // Measured 2026-10-02, on the session this check would have saved: a store
        // holding 940,211, measured on a ~950k-token conversation, was paired with
        // a rebuilt ledger of 1,486,369 — 0.63 where the truth was 1.016. The
        // window came out 1,580,888 for a session already 1.46M tokens deep, so it
        // never compacted and died at the provider's 400 on every attempt.
        //
        // A row with no `context_ledger` was written before the column existed and
        // is REFUSED rather than guessed at. Unverifiable is not the same as wrong,
        // but the two are the same as unmeasured, and the unscaled window is the
        // honest answer for both — it compacts early, which is the direction nobody
        // loses a session to. See `Config::tokens_are_unscaled`.
        //
        // **And the pair is an ANCHOR now, not a ratio (R72).** The equality test that
        // stood here refused the pair whenever anything had been appended after the last
        // round — a notice was enough — and the unscaled window it fell back to compacted
        // a session the provider counted at ~740k down to 75k on its first idle pass. See
        // `recovered_scale` for both measurements and `Harness::measure_resident` for the
        // roll forward, which runs once the harness exists, at the end of `open`.
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
                scale: sent_scale(&session.ledger, &session.items, ledger, provider, true),
            });
        }
        // **Fill the harness view's slot, now that there is something to disclose.**
        // The tool was registered before the gate and the backend existed — it had
        // to be, to be in the prompt — and this is the first point where the
        // disclosures it reports can be computed.
        *disclosure_slot.lock().unwrap_or_else(|e| e.into_inner()) = cfg
            .disclosures(&wiring)
            .into_iter()
            .map(|d| (d.subject, d.state, d.detail))
            .collect();
        // **What `/models local` returns to**, captured from the pre-move `cfg` and
        // `parts` before the struct expression below takes `cfg` by value: the
        // daemon's own server, vocabulary and dialect. A switch to other weights
        // replaces the engine wholesale, so "back to local" is a re-seat of this
        // pair and a re-render of the conversation under it, not a field flip.
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
            // **The claim on this session's transcript, held for the harness's life** — see
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
            compacting: false,
            wall_anchor,
            render: parts.wiring.clone(),
            supplies,
            open_notes: notes,
            new_title: None,
            bang_seq: 0,
            // Attached by `Sessions` right after open — see the field. `None` until
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
            stale_told: None,
            unsealed_fork: None,
            meter: Default::default(),
            intent,
            injected,
            handed_back: Default::default(),
            monitors,
            monitor_cursor,
            room,
            tool_sink,
            job_watch,
            // The same `Arc` the tool registry has, cloned before it was moved in — see the
            // field: an interrupt has to be able to stop this session's children.
            subagents: subagent_runner,
            reviews: parts.reviews.clone(),
            gatekeepers: Default::default(),
            restart_notices: Vec::new(),
            workspace_gone_said: false,
            monitors_seen_live: Vec::new(),
            gate_told: Default::default(),
            outage: None,
            provider,
            key_wanted,
            // Filled on the first switch away from local, never at open: a session
            // that started on a provider has no local window to go back to, and
            // `None` here says exactly that.
            local_window: None,
            local_window_cell: local_window_cell.clone(),
            point_cell: point_cell.clone(),
            // Captured from the pre-move `cfg` and `parts` above: everything
            // `/models local` returns to, held before the first switch can
            // overwrite it. See the field's own doc for why a return needs a
            // record and cannot re-derive.
            own_server,
            // Nothing to go back to until a switch has been made.
            switch_back: None,
            // Nothing waiting: a switch asked for between turns takes effect at once.
            pending_switch: None,
            // And no turn owed: only a fallback ever arms one.
            fallback_continuation: None,
            continuation_of_fallback: false,
            // Filled by `Sessions::open_plugins` for a session the daemon serves; `None` reads
            // `plugin scaffold`/`build` only, and says so by name (see the field).
            plugin_slot: plugin_slot.clone(),
            // Nothing has asked for a confirmation yet — and a request outlives no restart by
            // design: a card is about now, and the head that was waiting for the answer is a
            // process that stopped with the daemon.
            mode_confirm: None,
        };
        h.announce_the_open(stale);
        h.import_the_foreign_conversation();
        Ok(h)
    }

    /// **Say that the session is open, once everything it is made of exists**: the board's
    /// first settings, the activity and footprint sources, the reads a head attaches for,
    /// and the resume's own republish. Split out of `open_with_registry` so its last step is
    /// a sentence rather than a hundred lines between the struct literal and `Ok`.
    fn announce_the_open(&mut self, stale: Option<String>) {
        // The recovered anchor rolled forward to the ledger as rebuilt, so the pair
        // every `Config` reader converts with is the count of THIS conversation from the
        // first thing that reads it — a `/reseat` before any turn included.
        self.measure_resident();
        // **The operator's `/nag pause`, from the session's own row** — before the settings are
        // first published, so the first `nags` row a head reads is already the true one.
        self.restore_nag_pause();
        // What the board already waits on the operator for is the state, not news: the settings row
        // says it, and only a question asked from here on is announced.
        self.asks_said = crate::operator_ask::waiting(&self.todos.snapshot());
        self.publish_settings();
        self.publish_jobs();
        // **What this session is waiting on** (`PROTOCOL_VERSION` 52), and how to work it out again.
        //
        // Registered here rather than at the hub's construction because this is the first point at
        // which the three halves exist together: the hub (the turn, its calls, the open cards), the
        // watcher set (the jobs and children), the monitor table, and the board the job rule is read
        // against. From here on the hub re-derives the value after any event that can have moved it
        // (`Hub::publish`) and drops an unchanged one, so what the daemon has to say explicitly is
        // only the changes no event announces — see `crate::activity`.
        self.hub.set_activity_source(crate::activity::source(
            self.job_watch.clone(),
            self.monitors.clone(),
            self.monitor_cursor.clone(),
            self.prompt_in_flight.clone(),
        ));
        self.publish_activity();
        // **And what this session's harness HOLDS** (R66) — the half of a session's memory that is
        // not on the hub at all. Registered here, where the harness, its transcript, its ledger
        // and its trail all exist, and read by the memory watchdog's census.
        self.restate_footprint();
        self.hub.set_footprint(crate::footprint::source(
            self.footprint_gauges.clone(),
            self.trail.clone(),
        ));
        // **And the notes source, which is read rather than pushed** — a head's `ListNotes`
        // is answered from it, so it has to be installed before a head can attach. See
        // `Harness::install_notes_source`.
        self.install_notes_source();
        // **And the skills source**, for the same reason: a head's `ListSkills` is answered from
        // it, and it has to be in place before a head can attach.
        self.install_skills_source();
        // **And the live job-output read**, for the same reason and with the same timing: the
        // jobs pane's Enter is answered from it while a turn holds the worker. See
        // `Harness::install_job_output_source`.
        self.install_job_output_source();
        if self.resumed.is_some() {
            // **The RESUME's own sentence, and it is not the carry's.** The operator read the
            // carry's wording here and asked the obvious question — *"why it was decided to carry
            // the conversation to the new prompt?"* — about an operation that rebuilds no prompt:
            // a resume re-announces stored rows so the head can draw the conversation it already
            // had. Naming the wrong operation is the same defect the `Filling` event was written
            // to end, one layer up: an indicator must be the fact, not a rendering of the fact.
            // ...and it restores the CONVERSATION, which a compaction splits across
            // transcripts: the tail of the ones before comes back too, up to the view's bound.
            let before = self.ancestor_tail();
            self.republish_after(letibot_sessionlog::RESTORING_ROWS, before);
        }
        // **What a head attaching to this session should be told**, left in the registry for
        // `seat_in` — see the comment in the resume arm for why it is not said there. A
        // session no harness has opened has no entry, and this is then a no-op.
        //
        // **HERE, after the republish, and not beside the comparison.** The daemon binds its
        // socket before it opens the session, so a head that reconnected across a restart is
        // already attached while this runs — it was handed an empty snapshot and is being sent
        // every restored row live. `set_stale_prefix` tells such a head at once (the
        // operator's *"ok reseat came … but later"*: it used to wait for the next attach), and
        // it must tell it AFTER the rows, because a live note is filed at the seam it arrives
        // at: said before the republish, it would be drawn above the whole restored
        // conversation, which on a long session is a screen nobody scrolls to.
        self.session_registry
            .set_stale_prefix(&self.cfg.session_id, stale);
    }

    /// **A session whose id marks somebody else's conversation reads it in** — an opencode
    /// one (R6) or a Claude Code transcript (Package H), never a resume of either.
    fn import_the_foreign_conversation(&mut self) {
        // **R6: a session whose id marks an opencode conversation reads it in.**
        //
        // `oc-` is the namespace mark and the rest is opencode's opaque id
        // (`ses_f68f5d…`). Only for a session that is **not** a resume: re-running the
        // same `--session oc-<id>` finds the stored session and resumes it rather than
        // importing a second copy, which is the idempotency rule. The import never
        // fails the session — a missing database or id is a sentence on the log and
        // the head stays up, because by the time it is read the head is already a
        // working head with a screen.
        if self.resumed.is_none()
            && let Some(oc_id) = self.cfg.session_id.strip_prefix("oc-").map(str::to_string)
        {
            self.import_opencode(&oc_id);
        }
        // **And a `cc-` session reads a Claude Code transcript in** (Package H). The rule is
        // the same one and it lives in one place — `letibot_claude::import_target` — so a
        // resumed session imports nothing and a re-run of the same `--session cc-<uuid>`
        // finds the stored conversation instead of appending a second copy of it.
        if let Some(cc_id) =
            letibot_claude::import_target(self.resumed.is_some(), &self.cfg.session_id)
                .map(str::to_string)
        {
            self.import_claude(&cc_id);
        }
    }

    /// **Read an opencode conversation into this session (R6).**
    ///
    /// The reading is `letibot-opencode`'s; this is the half that turns its rows into
    /// **this** session's rows and keeps the screen live while it does. It is
    /// deliberately not allowed to fail the session: a missing database or a missing id
    /// is a sentence on the log, and the head stays up.
    ///
    /// # Why the read is on the worker and the screen is still up first
    ///
    /// `Session::append_items` is the one writer and it is the worker's, so the reading
    /// happens here. What makes that *"the UI is up before anything is read"* rather
    /// than a startup dependency is that the **head attaches to the registry's hub, not
    /// to this harness**: the session is registered — and attachable — before the worker
    /// opens it, so a head joining now gets an empty snapshot and every row as a live
    /// event. Rows land as they are read because each is appended and published as the
    /// reader reaches it, and `ImportProgress` reports the reader's own count so the bar
    /// is the fact and not a rendering of it.
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

        // **Rule 1, said where a reader will see it.** These are another agent's rows,
        // under another provider; a reader who cannot tell whose they are cannot weigh
        // them. The origin — the id, the directory, the provider/model — goes on the log
        // the head reads.
        self.import_note("imported", tree.root.origin());
        let total = tree.parts;
        const WHAT: &str = "importing an opencode conversation";
        self.filling(WHAT, "parts", 0, total);

        let mut failure: Option<String> = None;
        let mut since_persist = 0u64;
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
                if let Err(e) = self.append_imported(&[r.item]) {
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

    /// **Read a Claude Code conversation into this session (Package H).**
    ///
    /// The reading is `letibot-claude`'s; this is the half that turns its rows into **this**
    /// session's rows and keeps the screen live while it does — the same split, the same
    /// rules and the same refusal to fail the session as [`Harness::import_opencode`] beside
    /// it. A missing transcript directory, or an id no transcript has, is a sentence on the
    /// log and the head stays up: by the time it is read the head is already a working head
    /// with a screen.
    ///
    /// # The counter is BYTES here, and that is the source's shape
    ///
    /// opencode counts parts because its database can count them before the first row. A
    /// transcript is a 111 MB file on this box, and counting its lines first would be a whole
    /// second pass over it; its length is known from `stat` before anything is read. So the
    /// bar is bytes consumed against bytes on disk — a fact about the file rather than a
    /// rendering of the wait.
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

        // **Rule 1, said where a reader will see it** — the uuid, the directory, the branch,
        // the model and the Claude Code version. Another agent's rows under another vendor's
        // model; a reader who cannot tell whose they are cannot weigh them.
        self.import_note("imported", tree.root.origin());
        let total = tree.root.bytes;
        const WHAT: &str = "importing a Claude Code conversation";
        self.filling(WHAT, "bytes", 0, total);

        let mut failure: Option<String> = None;
        let mut since_persist = 0u64;
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
                if let Err(e) = self.append_imported(&[r.item]) {
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

    /// One progress tick for a named operation, from the daemon's own counter.
    ///
    /// The head draws a bar from this and from nothing else: it does not derive one
    /// from body-less rows, because that would mean *inferring* the operation from
    /// the symptom, which is the defect this event exists to end.
    pub(crate) fn filling(&self, what: &str, unit: &str, done: u64, total: u64) {
        self.hub.publish(letibot_sessionlog::SessionEvent::Filling {
            what: what.to_string(),
            unit: unit.to_string(),
            done,
            total,
        });
    }

    /// Put the restored conversation back on the session's log.
    ///
    /// A resumed harness holds the transcript; the **hub** does not, and a head
    /// attaching to it would be told it had joined an empty session with a 39,384-token
    /// prompt. So each restored row is announced exactly as a live one is —
    /// `TranscriptAppended`, then the body through `Hub::record_item` — and both the
    /// snapshot path and the already-attached path get it for free, because they are
    /// the paths a live append already takes.
    ///
    /// These are **not** re-renders and not re-runs: no turn events, no metrics, no
    /// `TurnFinished`. What a head shows after a resume is the conversation, not a
    /// replay of the turns that produced it, and inventing turn boundaries here would
    /// put timings on the screen that no clock measured.
    ///
    /// **And it says what it is doing** — in the caller's words, because the callers are
    /// different operations. A resume re-announces stored rows so an attaching head can draw
    /// the conversation; a re-seat carries that conversation onto a rebuilt prompt. Both
    /// announce thousands of rows and both are a real wait on a real session, but only one of
    /// them rebuilds a prompt, and a single sentence for the two told the operator they were
    /// watching the other one. That is why `what` is a parameter: **the layer that owns the
    /// fact states it**, which is this event's own rule.
    ///
    /// One `Filling` per row is the counter; the head draws it whenever the total is large
    /// enough to be worth a bar.
    /// **The rows a compaction put behind the current transcript, as many as the view holds.**
    ///
    /// The operator, after a restart: *"conversation was gone — only a small recent portion was
    /// displayed"*. A resume republished `self.session.ledger` — the CURRENT transcript — and a
    /// compaction makes the current transcript a new one: the summary and what came after it. So
    /// a session compacted twenty times came back as its last 791 rows, `items_dropped` said
    /// nothing came before them, and no head could scroll above them. Before the restart it
    /// could: the hub had lived through the compaction and still held the older rows.
    ///
    /// So a resume walks the chain, newest parent first, and takes rows from the END of each
    /// until the view's own bound is met — the same bound the view trims to, so this never
    /// publishes a row the view would drop. The ledger and the model's prompt are untouched:
    /// this is what the SCREEN is given, which is the conversation, not what the model reads,
    /// which is the summary. The first row of the current transcript is the compaction's own
    /// system row, so the seam between the two says what it is without a mark of ours.
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
            // **ONE PROGRESS TICK PER ROW IS A FLOOD, AND THE FLOOD IS WHAT BREAKS THE BAR.**
            //
            // This published ~2000 `Filling` events as fast as a loop can go — on top of a
            // `TranscriptAppended` and a `record_item` for every row — and a head's queue has a
            // cap. Over the cap the hub DEMOTES the head rather than blocking
            // (`hub.rs:1369`): the queue is cleared, `needs_resync` is set, and from then on
            // *"Already demoted; its queue is going to be thrown away"* — so the ticks stop
            // arriving. MEASURED: the operator watched the bar freeze and then vanish —
            // *"it was back and kinda frozen at 44 — the bar wasn't moving for seconds and then
            // disappeared"* — on a head that finished holding all 2039 items, i.e. one that was
            // demoted and handed a snapshot mid-restore.
            //
            // So the bar froze at the last tick it heard and never saw the rest, because the
            // event stream carrying it had been dropped. **The thing that feeds the progress
            // indicator was killing the progress.** A bar is read as a fraction, not as a row
            // count, so a tick every `FILLING_STRIDE` rows loses the reader nothing and costs
            // the head's queue ~30 events instead of ~2000.
            //
            // The first and last rows always tick, so a bar appears promptly and — with the
            // completion below — ends by fact.
            //
            // **The stride is on the TICK, never on the row.** `7614785` put a `continue` here,
            // ahead of the publish, so it skipped the row as well as the tick: a restore of 791
            // rows announced 13 of them — row 0, every 64th, and the last. The operator after
            // the next restart: *"conversation was gone — only a small recent portion was
            // displayed"*. The head held 13 restored rows plus the live turns since, and nothing
            // said anything was missing, because nothing had been dropped: it was never sent.
            if i % FILLING_STRIDE == 0 || (i as u64 + 1) == total {
                self.filling(what, "rows", i as u64 + 1, total);
            }
            self.hub
                .publish(letibot_sessionlog::SessionEvent::TranscriptAppended {
                    item_id: item_id.clone(),
                    kind: letibot_tokencore::store::item_kind(item).to_string(),
                    ledger_head: hex32(h_k),
                });
            self.hub.record_item(item_id, item.clone());
            self.publish_citations(item_id, item);
        }
        // **THE WALK ENDING IS THE OPERATION ENDING, SO SAY SO.**
        //
        // The operator watched this bar freeze and then vanish: *"it was back and kinda frozen at
        // 44 — the bar wasn't moving for seconds and then disappeared."* Both halves have a cause,
        // and only one of them is the head's.
        //
        // `continue` above skips a ledger row with no matching item, so when the two disagree the
        // walk ends on an index BELOW `total` and the last tick anybody receives is short of it.
        // The head then holds a bar that says `44 of N` with no completing tick to clear it —
        // `note-filling` clears on `done == total` and on nothing else — until its 15-second
        // no-news window expires and the line disappears. That expiry is a good guard against a
        // daemon that has DIED and a bad way to end an operation that has finished: the bar
        // vanishes because a timer ran out, not because the work completed.
        //
        // So the completion is published unconditionally, after the walk. It costs one event and
        // it makes the bar end by FACT rather than by silence — which is the rule the head's own
        // docstring states (*"a bar that cannot end is worse than no bar"*) and the reason
        // `done == total` is the clear.
        //
        // This is the second time this exact shape has been measured; leticl's `filling-active-p`
        // records the first (`57 of 1790 rows` for three and a half minutes). A head-side timer
        // hid it that time, which is why it came back.
        self.filling(what, "rows", total, total);
    }

    /// The last `n` rows of this session's STORED transcript, as compact text.
    ///
    /// The durable record rather than the in-memory one: the store is what survives a restart, and
    /// it is where a reader would go looking afterwards. `None` when there is no store or no
    /// transcript — said as itself rather than as an empty tail, because "I could not look" and
    /// "there was nothing there" are different facts (`DISCLOSURE`).
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
                // **The two that carry no facts about the RUN.** Reasoning is the model thinking
                // and a segment mark is the head's own boundary between runs; a log tail looking
                // for *what happened to this call* names neither, and inventing a rendering for
                // them here would put two rows of the model's thoughts into a sentence about a
                // missing result.
                TranscriptItem::Reasoning { .. } | TranscriptItem::SegmentMark { .. } => continue,
            };
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

/// Read a GGUF's vocabulary — llama.cpp's job, so only in a build with the `local`
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

/// A provider backend from its config: the preset, the key (a missing one is a
/// refusal naming the variable and the file), the model, the switches.
///
/// `notes` is where a provider says something a person has to read — a model refusing a
/// picture, today. Passed in rather than found, because this crate's session log is one
/// level above the provider and the provider must stay below it; see
/// [`provider_notes`], which is the only caller with a hub.
pub fn build_provider(
    pc: &crate::config::ProviderConfig,
    sampling: &serde_json::Value,
    notes: letibot_provider::openai::Notes,
) -> Result<Box<dyn letibot_backend::MessagesBackend>, String> {
    let preset = letibot_provider::Preset::parse(&pc.name)?;
    let creds = letibot_provider::keys::resolve(preset, pc.api_key.as_deref(), None)
        .map_err(|e| format!("provider {}: {e}", pc.name))?;
    let mut p =
        letibot_provider::OpenAiProvider::new(preset, pc.model.as_deref(), creds).with_notes(notes);
    p.thinking = pc.thinking;
    p.sampling = provider_sampling(sampling);
    Ok(Box::new(p))
}

/// **Where a provider's own note reaches the operator** — the session's hub, one crate up.
///
/// One closure, so the fact a provider learns ("this model refused an image") arrives as
/// the warning a head already draws, and `letibot_provider` keeps needing no session log.
/// The code is the one a grep for this behaviour looks for.
pub(crate) fn provider_notes(
    hub: &std::sync::Arc<letibot_sessionlog::Hub>,
) -> letibot_provider::openai::Notes {
    let hub = hub.clone();
    std::sync::Arc::new(move |said: String| {
        hub.publish(letibot_sessionlog::SessionEvent::Warning {
            code: "model_refused_images".into(),
            detail: said,
            compaction: None,
        });
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

/// The parent's ruleset plus a `deny` per tool of a denied class. The tools are
/// not seated either (see `without_access`), so this is the third reader of the
/// same fact and the one that holds if a name reaches the gate anyway: last
/// rule wins in opencode's `evaluate`, and these are last.
///
/// Two sources of names. The seated schemas, by their declared class — after
/// `without_access` that set is empty, which is the point. And the well-known
/// names of each class whether seated or not, so a tool registered outside the
/// role later (an `extra_tool`) that carries a denied class is refused by name
/// rather than admitted because nobody wrote a rule for it. A rule about an
/// unseated name costs nothing.
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
        // `notes` is in the `Write` arm on purpose: a standing note is written
        // to the operator's tree and read into every later session's prompt,
        // which makes it a MORE persistent write than a file edit, not a
        // session-state one — a subagent denied `write` must not be able to
        // plant one either.
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

/// **Did this session inherit a whole-host standing from an ancestor?**
///
/// [`Config::unconfined`] is set for [`Seat::Leticode`] and **inherited by its subagents**
/// (`sub_cfg` builds a child from `..self.base.clone()`), which is the only way a `coder`
/// subagent is ever unconfined — its own seat does not say so and neither does anything
/// else on its row. So the rule the spawn applies is applied *again* here, from the rows
/// it left: a session stands where its ancestors stand, and an ancestor that recorded
/// `leticode` is one whose whole tree is unconfined.
///
/// # Why the walk instead of a recorded flag
///
/// The store's `session` row records the SEAT (`role`, v2) and not the standing, and the
/// two are not the same fact for exactly the sessions this is about: a subagent's row says
/// `coder` whether its parent was a leticode session or a coder one. So it is read back the
/// way it was handed down — up the `parent_session_id` chain the row already carries (v4).
/// A top-level session has no parent and is off this path entirely: its own seat is the
/// whole answer, which is the line above this call.
///
/// **A row this walk cannot read is not an answer and not an error.** A vanished ancestor,
/// a role the build cannot parse, or a chain longer than `MAX_ANCESTORS` all leave the
/// standing unproven, and an unproven standing is the confined one — which is what every
/// session that predates this had, and a refusal to open over an ancestor's bad row would
/// cost a conversation for a fact about somebody else.
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

#[cfg(test)]
mod unconfined_by_inheritance_tests {
    use super::unconfined_by_inheritance;
    use letibot_tokencore::store::{SessionRecord, Store};

    /// A store holding `rows` — `(id, role, parent)`, in order — and the id of the LAST
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

    /// **A subagent's standing is its ancestors', and the row says only the seat.** A
    /// `coder` child of a leticode session is unconfined, and so is ITS child — that is
    /// what `sub_cfg`'s `..self.base.clone()` hands down at every spawn, and what a
    /// reopened session has to be given back.
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

    /// **A coder session's child stays confined**, which is the other half of the rule the
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

    /// **A chain the store cannot answer for is not an answer.** An ancestor whose row is
    /// gone leaves the standing unproven, and unproven is confined — the state every
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

/// The role a seat resolves to, with `bash` removed when it was not asked for.
///
/// The removal is here rather than in `letibot_tools::roles` because it is a
/// *session* decision and not a change to what the runner role is: `m2_runner` is
/// nine tools and says so, and a session that took eight of them is a session, not
/// a different role. `Registry::resolve_role` refuses a role naming a tool the
/// build does not have, so dropping the name is the only way to seat a runner
/// without a shell — leaving it in and hoping the tool refuses would put `bash` in
/// the prompt, which is the model being told it has a capability it does not.
pub(crate) fn role_for(cfg: &Config) -> Role {
    role_for_seat(cfg.seat, cfg)
}

/// The same table, reached from a seat the store named rather than from `cfg`.
///
/// `cfg` is still needed for [`Config::allow_bash`], which is a daemon-wide flag and
/// deliberately not per session: a shell is the one capability that is not the
/// session's to record.
pub(crate) fn role_for_seat(seat: Seat, cfg: &Config) -> Role {
    let mut r = base_role_for_seat(seat, cfg);
    // **The room.** The `flowy` tool is registered by `Sessions` for every root
    // session, and the role names it under exactly the same condition —
    // `Registry::resolve_role` refuses a role naming a tool the session does not
    // have. The runner has no spare seat (`m2_runner` is nine and says so).
    // Always a door for a root session, seat or no seat: without one the tool
    // says so and names `/flowy login`. That is what lets a seat arrive while
    // the session is open. A subagent hears through its parent.
    if cfg.parent_session_id.is_none()
        && !matches!(seat, Seat::Runner | Seat::Gatekeeper | Seat::NotesKeeper)
    {
        r.tools.push("flowy".into());
    }
    // **`web_search` is seated only when something is behind it**, and a subagent
    // inherits that, because a survey is the errand most worth handing one.
    //
    // Conditional for the reason `external/mod.rs` gives: a tool schema is prompt
    // bytes in the stable prefix, so seating it unconditionally would re-prefill
    // every stored conversation on this box to add a tool that refuses. A session
    // started without `--web-search` is byte-identical to yesterday's.
    // **A reviewer reads the references** (the operator, 2026-10-26: *"reviewers should be able
    // (and actually encouraged) to read mans, docs and so on"*), so the gatekeeper seats the two
    // web tools under the same conditions as every other seat; `flowy` above stays off it.
    if cfg.web_search.is_some() && !matches!(seat, Seat::Runner | Seat::NotesKeeper) {
        r.tools.push("web_search".into());
        r.max_tools += 1;
    }
    // **`web_fetch` seats on the same rule as `web_search`**: only when
    // something is behind it, so a session started without `--web-fetch` is
    // byte-identical to one from before the flag existed.
    if cfg.web_fetch && !matches!(seat, Seat::Runner | Seat::NotesKeeper) {
        r.tools.push("web_fetch".into());
        r.max_tools += 1;
    }
    r
}

pub(crate) fn base_role_for_seat(seat: Seat, cfg: &Config) -> Role {
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
        // **The reviewer**: read-only by construction, `bash` only behind the daemon's flag —
        // without it the gatekeeper reads the files and cannot diff, and says so in its verdict.
        Seat::Gatekeeper => {
            let mut r = roles::gatekeeper();
            if !cfg.allow_bash {
                r.tools.retain(|t| t != "bash");
            }
            r
        }
        // **The notes keeper**: the read tools and `notes`, and no exec path at all, so there is
        // no flag to strip. A repair goes through `notes` — the one writer that keeps a note a
        // note (a bare NAME, `.letibot/notes/` only, the author's abstract, the historical
        // caveat) — and nothing on this seat can touch code.
        Seat::NotesKeeper => roles::notes_keeper(),
        Seat::Leticode => {
            let mut r = roles::leticode();
            if !cfg.allow_bash {
                // **The whole exec surface is behind the flag**: `bash` and the job
                // verbs it feeds, `monitor` which watches processes a shell
                // starts, and — since RH8 — `fanout` and `scenario`, which start
                // commands on other machines. A session with no shell has nothing to wait on,
                // read, kill or watch, and nothing it may start elsewhere either: a verb that
                // could run a command on a host while its own seat cannot run one here would be
                // the flag's whole point walked around.
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
                            | "fanout"
                            | "scenario"
                    )
                });
            }
            r
        }
    }
}

/// **Where layer A is standing**, and what this daemon can honestly claim about it.
///
/// `docs/boundary-and-adjudication.md` §5 states the requirement rather than
/// assuming it, *"because a requirement crosses a merge where an assumption does
/// not"*: `Surroundings::with_pinned_shell` is honest only where the spawn path
/// `env_clear()`s before its explicit pairs so `PATH` is pinned rather than
/// inherited, and unsets `BASH_ENV`, `ENV`, `SHELLOPTS` and `BASHOPTS` —
/// `BASH_ENV` **is** sourced by bash for non-interactive shells, so a
/// distribution where `/bin/sh` is bash has a real injection point.
///
/// Since R10 the host spawn does exactly that — `HostProcesses` pins the `PATH`
/// it was constructed with, clears the rest, filters the four by name, and the
/// test `a_bash_env_planted_in_the_parent_never_reaches_the_child` holds the
/// claim — so a seat with an exec backend declares the pin. A seat without one
/// spawns no shell at all, so there is nothing to claim and `Unknown` stays,
/// under which a **bare** command name is unresolved and the call is `not_run` —
/// the fail-closed direction. A resolved parse is not a resolved meaning, and a
/// shell resolves a bare name through aliases, functions and `PATH`, none of
/// which are in the text.
///
/// What is filled in either way is the part this daemon **does** know: the
/// workspace root and `$HOME`, both of which layer A needs to place a path.
/// [`letibot_tools::Surroundings::from_env`] is the constructor that says
/// reading the environment is a decision at a call site.
/// **Where this daemon's sessions put working artifacts.**
///
/// One function so the backend that hands it to tools and the gate that decides
/// about paths inside it cannot disagree about where it is — a gate pointed at a
/// different directory than the one the tools use is a gate that permits deletion
/// in a place nothing writes, and asks about the place everything does.
///
/// Per daemon process rather than per session: a pid is stable for the daemon's
/// life, which is what "this box is running letibot right now" means, and a
/// session id in the path would make the directory unfindable from the one place
/// that has to clean it up.
pub fn scratch_dir() -> std::path::PathBuf {
    // **THE HEAD'S OWN RULE, APPLIED TO THE DAEMON'S HALF.** This used to be
    // `/tmp/letibot-scratch-<pid>` — `temp_dir()` at a predictable name, world-readable
    // — and it holds what the jobs write, the web pages fetch spills, the peek logs:
    // anything the model read. The TUI's `head_runtime_dir` already ruled that shape
    // wrong for its own spills ("a world-readable file at a name anyone can predict is
    // both a disclosure and the classic symlink target"), and the operator's ask,
    // 2026-10-05, was the same ruling one layer down: *"job output should go to your
    // scratch directory."* `$XDG_RUNTIME_DIR` is per-user, mode 0700 and tmpfs by
    // construction; the fallback creates its per-user directory at 0700 rather than
    // inheriting `/tmp`'s 0755 (the creation site sets the mode — `create_dir_all`
    // cannot).
    //
    // Still per daemon process, and still no session id in the path, for the reason
    // above: findable from the one place that has to clean it up.
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

/// **What a confined session's view holds beyond its project** — the operator's `--grant-ro`
/// paths, and, added by the daemon, the merge queue's landing tree of the repository this
/// workspace is (under `landing_root`), READ-ONLY.
///
/// The landing tree is where this project's gate runs. The firecode session, 2026-10-11, on the
/// old `.git/letibot-landing`: *"`.git` is on the never-write list — I can't inspect the landing
/// worktree, and no grant reaches it. So I'll reproduce it instead: a fresh clone"*. A model told
/// its target is red should be able to read the tree the red was measured in; it must not be
/// able to write there, because the next gate runs in it. The directory is created (empty) when
/// the queue has not made it yet, so the bind has something to bind: `bwrap --ro-bind` of a
/// missing path fails every command.
/// **The environment a `task_start` worktree child's commands carry for the shared build
/// cache**: `CARGO_TARGET_DIR` = the main tree's `target/`. `None` for the main-tree exception
/// (it is the main tree), and for a worktree git cannot place (nothing to share then, and a guess
/// would point a build somewhere nobody chose).
///
/// The main tree is found from the worktree: `git rev-parse --git-common-dir` inside a linked
/// worktree is the main checkout's `.git`, and its parent is the main tree.
///
/// **Reaches the commands of an unconfined child** (leticode's `/` backend, the seat the operator's
/// children run under), through the exec host's standing environment. A CONFINED child's commands
/// do not get it: the bwrap view is the worktree and nothing else, so the main tree's `target/` is
/// absent inside the namespace, and pointing cargo at it would fail a build that otherwise works
/// cold. Granting that directory read-write into a child's view would let a confined child write
/// the binaries the operator runs from it — a decision for the operator, not a side effect of a
/// build-cache optimisation.
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
    // **A landing tree per repository of the workspace** — one, or each of a one-org project
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
    out
}

pub fn surroundings_for(cfg: &Config) -> letibot_tools::Surroundings {
    let mut env = letibot_tools::Surroundings::from_env(cfg.workspace.display().to_string())
        .with_known_hosts();
    // The gate places a path by region, and the scratch is its own region — see
    // `Region::Scratch`. Without this it reads as shared `/tmp`, and destruction
    // there is an ask.
    env.scratch = Some(scratch_dir().display().to_string());
    // The shell is pinned whenever the backend can start a process. Both the
    // confined backend (coder/runner) and the unconfined leticode one (`--bash`)
    // spawn through `HostProcesses`, which `env_clear`s and fixes `PATH` — so a
    // bare command name resolves through a pinned PATH either way, and the
    // `Unknown` that refused `echo` was the wrong claim for an unconfined seat.
    let may_exec = !cfg.downgrade.denies(letibot_tools::schema::Access::Exec);
    let pinned = (cfg.seat.needs_exec_backend() || (cfg.unconfined && cfg.allow_bash)) && may_exec;
    if pinned {
        env = env.with_pinned_shell(
            "the exec backend spawns /bin/sh -c, non-interactive, with the \
             environment cleared before its own pairs; PATH is the one fixed at \
             seat time and BASH_ENV, ENV, SHELLOPTS, BASHOPTS are never set",
        );
    }
    // **The environment every command of this session really gets** — D14/R95. It is read
    // from the same two places `HostProcesses::spawn` reads them: the `PATH` this process
    // pins on every child after `env_clear()`, and the pairs the daemon puts beside it
    // (`HOME`, and `Config::build_env`'s `CARGO_TARGET_DIR`). Nothing here is inferred from a
    // command's text, which is what makes a reading that rests on it true of the real spawn.
    //
    // **And what this gate can see of the disk** — D15/R96's second half. `Host` only where
    // the command runs in this process's own filesystem view: a `firecode` VM and a remote
    // placement are different disks, and a *confined* session is a different view of this one
    // (`/tmp` inside the boundary is the session's, not the box's), so naming a file there
    // would be naming the wrong file. `None` leaves a `PATH` the command sets unresolved,
    // which is exactly what it was before this existed.
    if pinned {
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
        let mut pairs: Vec<(String, String)> = vec![("PATH".to_string(), path.clone())];
        if let Some(home) = &env.home {
            pairs.push(("HOME".to_string(), home.clone()));
        }
        pairs.extend(cfg.build_env.iter().cloned());
        // **And the keep-list a boundary passes through.** `exec::confine::KEEP_ENV` is what
        // a bwrap'd command keeps from THIS process (`--clearenv` then `--setenv` each), so
        // those values are what the command really starts with — for a local placement. A
        // remote or VM placement reads its keep-list on the FAR side (`EnvSource::Remote`),
        // and seeding this box's `LANG` for it would be a claim about another machine.
        if matches!(
            cfg.placement,
            letibot_tools::builtins::task::Placement::Host
        ) {
            for name in letibot_tools::exec::confine::KEEP_ENV {
                if *name == "PATH" {
                    continue;
                }
                if let Ok(value) = std::env::var(name) {
                    pairs.push(((*name).to_string(), value));
                }
            }
        }
        env.session = letibot_tools::intent::env::SessionEnv::seeded(pairs);
        env.shell_path = Some(path);
        if !cfg.seat.needs_exec_backend()
            && matches!(
                cfg.placement,
                letibot_tools::builtins::task::Placement::Host
            )
        {
            env.programs = letibot_tools::intent::ProgramLookup::Host;
        }
    }
    env
}

fn build_spiller(cfg: &Config) -> Result<letibot_tools::Spiller, HarnessError> {
    use letibot_tools::{FixedBudget, MemoryStore, NoBudget, Spiller};
    let budget: Box<dyn letibot_tools::InlineBudget> = match cfg.spill {
        // D6: unset is a genuine no-op, not a hidden constant. The daemon says so
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

    /// **The three inputs are all in the address, and the one that must not be is not.**
    ///
    /// The four assertions are the whole contract of the hash, and each is a different way
    /// the comparison could be wrong: a seat ignored would call two differently-seated
    /// sessions identical, an instruction ignored would miss `--system`, a schema ignored
    /// would miss the operator's own case, and a note counted would report every session
    /// stale the moment anybody wrote one down.
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

        // **And a schema that GREW moves it with the list unchanged** — `todo_write`'s
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

        // The standing-notes section, by its own markers, is stripped before hashing —
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

    /// **The seated time is a time, and a missing clock is not the epoch.**
    ///
    /// `0` is *nobody recorded when*, and a sentence reading *seated 01-01 00:00* about a
    /// session nobody dated is the class of lie the whole notice exists to avoid.
    #[test]
    fn an_unrecorded_time_is_said_and_a_recorded_one_is_the_launchers_spelling() {
        assert_eq!(seated_at(0), "an unrecorded time");
        assert_eq!(seated_at(-1), "an unrecorded time");
        // 2026-10-08 21:14 UTC is 2026-10-08 in every zone this runs in except a +3h one
        // crossing midnight — so the assertion is the SHAPE, which is what the launcher's
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
