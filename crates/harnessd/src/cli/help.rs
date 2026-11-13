//! **Every flag this daemon parses, in one table, and the help is generated from it.**
//!
//! The parser's `match` and this list are two spellings of one fact, so this table is the one
//! `parse` consults before dispatching: a word that is not here is an unknown argument. That
//! makes "the help lists every flag it parses" true by construction instead of by a hand-kept
//! second copy, which is the disagreement the survey found.
//!
//! A [`Flag`] with a `key` is a setting that belongs in a file: using the flag still works for
//! this release and says the key on stderr, because the flag is going and the file is where
//! that setting lives from the next one.

/// One flag: its spellings, the placeholder for the word it consumes, and its help line.
pub(super) struct Flag {
    /// Every spelling, the canonical one first.
    pub names: &'static [&'static str],
    /// The word after it, or `None` when it takes none.
    pub value: Option<&'static str>,
    /// The one line the help prints.
    pub about: &'static str,
    /// The config key this setting now lives at, when it has one.
    pub key: Option<&'static str>,
}

impl Flag {
    /// `--flag VALUE`, as the help prints it.
    pub fn label(&self) -> String {
        let mut out = self.names.join(", ");
        if let Some(v) = self.value {
            out.push(' ');
            out.push_str(v);
        }
        out
    }
}

const fn flag(
    names: &'static [&'static str],
    value: Option<&'static str>,
    about: &'static str,
    key: Option<&'static str>,
) -> Flag {
    Flag {
        names,
        value,
        about,
        key,
    }
}

/// THE TABLE. One row per flag the parser reads.
pub(super) const FLAGS: &[Flag] = &[
    // --- what this process is -----------------------------------------------------------
    flag(
        &["--workspace"],
        Some("DIR"),
        "the folder this daemon serves; a store query scopes to it",
        None,
    ),
    flag(
        &["--socket"],
        Some("PATH"),
        "where this daemon listens and a head attaches",
        None,
    ),
    flag(
        &["--store"],
        Some("DIR|FILE"),
        "the store: a directory (shared.db + projects/) or one .db file",
        None,
    ),
    flag(
        &["--project"],
        Some("KEY"),
        "which project file this daemon serves, when not the folder's own",
        None,
    ),
    flag(
        &["--session"],
        Some("ID"),
        "the session to open; `host` is the box's own agent",
        None,
    ),
    flag(&["--title"], Some("NAME"), "a new session's title", None),
    flag(
        &["--prompt"],
        Some("TEXT"),
        "run these, print the answers, exit (repeatable)",
        None,
    ),
    flag(
        &["--slash"],
        Some("VERB"),
        "a head's slash verb, in order with --prompt",
        None,
    ),
    // --- the model ----------------------------------------------------------------------
    flag(
        &["--model"],
        Some("ALIAS"),
        "the local model, or under --provider the provider's model",
        Some("`[default] model` in providers.toml"),
    ),
    flag(
        &["--dialect"],
        Some("glm|qwen"),
        "the template the ledger renders with",
        None,
    ),
    flag(
        &["--vocab"],
        Some("GGUF"),
        "the vocabulary that tokenises this session",
        None,
    ),
    flag(
        &["--endpoint"],
        Some("HOST:PORT"),
        "the local model server",
        None,
    ),
    flag(
        &["--effort"],
        Some("LEVEL"),
        "how hard it thinks: low | medium | high | xhigh",
        Some("`effort` in that model's `[model.\"NAME\"]` block, providers.toml"),
    ),
    flag(
        &["--context-window"],
        Some("N"),
        "the model's window, when the server will not say",
        Some("`window` in that model's `[model.\"NAME\"]` block, providers.toml"),
    ),
    flag(
        &["--no-auto-compact"],
        None,
        "never fold the context automatically",
        None,
    ),
    flag(
        &["--system"],
        Some("TEXT"),
        "the system prompt itself (`--system-file` reads one from a path)",
        None,
    ),
    flag(
        &["--system-file"],
        Some("FILE"),
        "read the system prompt from a file",
        None,
    ),
    flag(
        &["--provider"],
        Some("NAME"),
        "send the turns to a cloud provider instead of a local model",
        Some("`[default] provider` in providers.toml"),
    ),
    flag(
        &["--api-key"],
        Some("KEY"),
        "the provider's key, for a one-off",
        Some("`[NAME] key` in providers.toml, or $DEEPSEEK_API_KEY and friends"),
    ),
    // --- what the session may do --------------------------------------------------------
    flag(
        &["--role"],
        Some("NAME"),
        "the seat: orchestrator | planner | researcher | coder | runner",
        None,
    ),
    flag(
        &["--mode"],
        Some("NAME"),
        "the approval policy: read-only | always-ask | writes-allowed | …",
        None,
    ),
    flag(
        &["--grant-ro"],
        Some("PATH"),
        "bind a path read-only into the confined view (repeatable)",
        None,
    ),
    flag(&["--bash"], None, "seat `bash` under --role runner", None),
    flag(
        &["--max-subagent-depth"],
        Some("N"),
        "how deep a subagent tree may go below the root (0: no `task`)",
        None,
    ),
    flag(
        &["--no-project-config"],
        None,
        "ignore the project's own leticode.toml",
        None,
    ),
    flag(
        &["--idle-exit"],
        Some("SECS"),
        "exit after this long with nothing to do; 0 never",
        None,
    ),
    flag(
        &["--no-queue"],
        None,
        "serve no merge or review queue",
        None,
    ),
    flag(
        &["--parent-seat"],
        Some("HANDLE"),
        "this daemon's sessions are that Claude Code seat's children",
        None,
    ),
    // --- the gate -----------------------------------------------------------------------
    flag(
        &["--adjudicator"],
        Some("NAME"),
        "who decides a gated call: console | head | model | none",
        None,
    ),
    flag(
        &["--oracle"],
        Some("HOST:PORT"),
        "layer B: the guard model's own llama.cpp /completion endpoint",
        None,
    ),
    flag(
        &["--oracle-budget-ms"],
        Some("MS"),
        "how long the gate waits for the guard before failing closed",
        None,
    ),
    flag(
        &["--oracle-max-tokens"],
        Some("N"),
        "how many tokens the guard may spend on its answer",
        None,
    ),
    flag(
        &["--oracle-question"],
        Some("verdict|scores"),
        "what the guard is asked: a verdict, or FIT and CLAIM 0-10",
        None,
    ),
    flag(
        &["--supervise"],
        None,
        "start with the guard model consulted on every gated call",
        None,
    ),
    // --- where the tools run -------------------------------------------------------------
    flag(
        &["--where"],
        Some("host|firecode"),
        "where this session's tools run: a firecode VM, or a host",
        None,
    ),
    flag(&["--vm-copy"], None, "a copy even on a Mac", None),
    flag(
        &["--vm-arg"],
        Some("ARG"),
        "passed to the VM's own launch, repeatable",
        None,
    ),
    flag(
        &["--host"],
        Some("NAME"),
        "run this session's tools on that ssh host (`[host.NAME]`)",
        None,
    ),
    flag(
        &["--web-fetch"],
        None,
        "attach curl egress behind `web_fetch`",
        None,
    ),
    flag(
        &["--web-search"],
        Some("NAME"),
        "attach a provider behind `web_search` (`brave` is the one)",
        None,
    ),
    flag(
        &["--brave-key"],
        Some("KEY"),
        "that provider's key, for a one-off",
        Some("`[brave] key` in providers.toml, or $BRAVE_API_KEY"),
    ),
    // --- the queues this daemon serves ---------------------------------------------------
    flag(
        &["--reviewers"],
        Some("host|vm"),
        "where this daemon's reviewers run",
        Some("`[queue] reviewers` in the project's leticode.toml"),
    ),
    flag(
        &["--gate-host"],
        Some("NAME"),
        "run the merge gate on a declared host instead of this machine",
        Some("`[queue] gate_host` in the project's leticode.toml"),
    ),
    flag(
        &["--reviewers-at-once"],
        Some("N"),
        "how many reviewers this daemon's queues run at once",
        Some("`[queue] reviewers_at_once` in the project's leticode.toml"),
    ),
    flag(
        &["--reviewer-hosts"],
        Some("LIST"),
        "the machines a reviewer may be placed on, comma separated",
        Some("`[queue] reviewer_hosts` in the project's leticode.toml"),
    ),
    flag(
        &["--room"],
        None,
        "be the BOX'S ROOM COORDINATOR: one per box, no store, a session",
        None,
    ),
    flag(
        &["--flowy"],
        None,
        "hold a flowy seat: messages for it wake this session",
        None,
    ),
    flag(
        &["--flowy-seat"],
        Some("NAME"),
        "which seat (implies --flowy)",
        None,
    ),
    flag(
        &["--flowy-new-reader"],
        None,
        "declare the seat's inbox reader at the head of the log, once",
        None,
    ),
    // --- results, rounds and retries -----------------------------------------------------
    flag(
        &["--spill-inline"],
        Some("BYTES"),
        "over this, a result is spilled to a file with a locator",
        None,
    ),
    flag(
        &["--spill-dir"],
        Some("DIR"),
        "where spilled results go",
        None,
    ),
    flag(
        &["--max-tool-rounds"],
        Some("N"),
        "the round backstop; `off` is none (the default)",
        None,
    ),
    flag(
        &["--stall-rounds"],
        Some("N"),
        "the no-progress band; 0 turns it off",
        None,
    ),
    flag(
        &["--http-retries"],
        Some("N"),
        "how many times a failed round is taken again; 1 is no retry",
        None,
    ),
    flag(
        &["--http"],
        Some("HOST:PORT"),
        "an HTTP head beside the socket, for a prompt-answering daemon",
        None,
    ),
    // --- store queries: they answer and exit ---------------------------------------------
    flag(
        &["--list-sessions"],
        None,
        "what is on disk, grouped: mine first, then the harness's work",
        None,
    ),
    flag(
        &["--tsv"],
        None,
        "a listing in its plain form, one line each",
        None,
    ),
    flag(
        &["--latest-session"],
        None,
        "the id of the newest session, scoped by --workspace",
        None,
    ),
    flag(
        &["--resolve-session"],
        Some("NAME"),
        "a session's id from its name, or from an id",
        None,
    ),
    flag(
        &["--scope"],
        Some("DIR"),
        "scope a store query to one workspace (an empty one is refused)",
        None,
    ),
    flag(
        &["--find-session"],
        Some("NAME"),
        "a session by name under --scope, else in any folder",
        None,
    ),
    flag(
        &["--rename"],
        Some("ID"),
        "name a session, or clear it with an empty title",
        None,
    ),
    flag(
        &["--delete"],
        Some("ID"),
        "remove a session that has no rows",
        None,
    ),
    flag(
        &["--archive"],
        Some("ID"),
        "put a session out of sight and KEEP it (`--archive ID REASON`)",
        None,
    ),
    flag(
        &["--unarchive"],
        Some("ID"),
        "put it back in the list",
        None,
    ),
    flag(
        &["--archived"],
        None,
        "list the archived ones beside the rest",
        None,
    ),
    flag(
        &["--archived-only"],
        None,
        "list nothing but the archived ones",
        None,
    ),
    flag(
        &["--list-claude"],
        None,
        "what Claude Code has on this box (`--session cc-<id>` takes it)",
        None,
    ),
    flag(
        &["--providers"],
        None,
        "every provider: built-ins, `[provider.NAME]` entries and env base URLs, marked",
        None,
    ),
    flag(
        &["--handover"],
        Some("SESSION DIR"),
        "write a session out as a `claude --resume` transcript",
        None,
    ),
    flag(
        &["--note"],
        Some("TRANSCRIPT TEXT"),
        "append one line to a Claude Code transcript, after a backup",
        None,
    ),
    flag(
        &["--note-continued"],
        Some("SESSION FILE"),
        "the same line, composed, plus the Markdown handover",
        None,
    ),
    flag(
        &["--revert"],
        Some("TRANSCRIPT BACKUP"),
        "put that transcript back from the backup, byte for byte",
        None,
    ),
    flag(
        &["--listen"],
        Some("WORDS"),
        "/listen's words, offline: `all`, `only REPO`, `-REPO`, `+REPO`",
        None,
    ),
    // --- the developer verbs: a measurement, not a session -------------------------------
    flag(
        &["--compare"],
        None,
        "run every guard arm over the rows and print them side by side",
        None,
    ),
    flag(
        &["--show-brief"],
        None,
        "print the bytes the guard is handed for a call it could not read",
        None,
    ),
    flag(
        &["--repair-prefixes"],
        None,
        "report the store's structural prefixes that no longer match",
        None,
    ),
    flag(
        &["--etalon"],
        Some("FILE"),
        "recover decision shapes for rows the operator already answered",
        None,
    ),
    flag(
        &["--etalon-oracle"],
        Some("FILE"),
        "the guard itself over an etalon-shaped JSONL (needs --oracle)",
        None,
    ),
    flag(
        &["--etalon-map"],
        Some("FILE OUT"),
        "every command's shape, on a self-organising map, as JSON",
        None,
    ),
    flag(
        &["--backfill-shapes"],
        None,
        "recover shapes for decisions already made (a dry run)",
        None,
    ),
    flag(
        &["--backfill-shapes-write"],
        None,
        "the same, and record what it found",
        None,
    ),
    flag(
        &["--calibrate"],
        None,
        "replay the answered corpus against the guard, and what it earns",
        None,
    ),
    flag(
        &["--calibrate-write"],
        None,
        "the same, and record it where the daemon reads it at startup",
        None,
    ),
    // --- what every program has ---------------------------------------------------------
    flag(
        &["-h", "--help"],
        None,
        "this page: every flag this daemon parses",
        None,
    ),
    flag(
        &["-V", "--version"],
        None,
        "the version, for an installer",
        None,
    ),
];

/// **What this release took away, and what replaced it.** A removed flag is refused BY NAME: a
/// flag read as a prompt is a billed turn answering a sentence nobody asked for.
pub(super) const REMOVED: &[(&str, &str)] = &[
    (
        "--restore-within",
        "nothing to set - it always restores, for up to an hour",
    ),
    (
        "--flowy-addr",
        "$FLOWY_ADDR, or the seat's own file (`flowy mint` writes it)",
    ),
    (
        "--flowy-token-file",
        "$FLOWY_TOKEN, or the seat's own file (`flowy mint` writes it)",
    ),
    (
        "--intent-prose",
        "nothing to set - the prose half had no reader",
    ),
    (
        "--thinking",
        "the provider's default; `/thinking` turns it on in a session",
    ),
];

/// The row for a spelling, if this parser reads it.
pub(super) fn find(name: &str) -> Option<&'static Flag> {
    FLAGS.iter().find(|f| f.names.contains(&name))
}

/// Whether this parser reads `name`.
pub(super) fn knows(name: &str) -> bool {
    find(name).is_some()
}

/// The replacement for a removed flag, if this release removed it.
pub(super) fn removed(name: &str) -> Option<&'static str> {
    REMOVED
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, why)| *why)
}

/// What a config-backed flag says when it is used: the key, in one line.
pub(super) fn config_notice(name: &str) -> Option<String> {
    let f = find(name)?;
    let key = f.key?;
    Some(format!("{name} is a config setting now: {key}"))
}

/// **The help, generated from the table** — so it cannot list a flag the parser refuses, or
/// omit one it reads.
pub(super) fn usage() -> String {
    let mut out = String::from(
        "harnessd — the session owner: the store, the model connection and the gate.\n\
         \n\
         \x20 harnessd                      serve the socket and wait\n\
         \x20 harnessd --prompt TEXT …      run these, print the answers, exit\n\
         \x20 harnessd --store DIR --list-sessions   answer a store query, exit\n\
         \n\
         EVERY FLAG THIS PARSER READS\n",
    );
    let width = FLAGS
        .iter()
        .map(|f| f.label().chars().count())
        .max()
        .unwrap_or(20)
        + 2;
    for f in FLAGS {
        out.push_str(&format!(
            "  {:width$}{}\n",
            f.label(),
            f.about,
            width = width
        ));
    }
    out.push_str("\nREMOVED IN THIS RELEASE — refused by name, with what replaced each\n");
    for (name, why) in REMOVED {
        out.push_str(&format!("  {name}: {why}\n"));
    }
    out.push_str(
        "\nFlags with a config key still work for this release and print the key they moved to:\n\
         `[queue] …` in the project's leticode.toml, `[default] …` and `[brave] key` in\n\
         ~/.config/letibot/providers.toml.\n",
    );
    out
}
