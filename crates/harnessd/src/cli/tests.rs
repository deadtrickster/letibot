use super::*;

fn stored(rows: &[(&str, Option<&str>)]) -> Vec<letibot_tokencore::store::StoredSession> {
    use letibot_tokencore::store::{SessionRecord, Store};
    let s = Store::open_in_memory().unwrap();
    for (id, title) in rows {
        s.put_session(&SessionRecord {
            id: (*id).into(),
            title: title.map(Into::into),
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: "/w".into(),
            owner: "op".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
    }
    s.list_sessions().unwrap()
}

#[test]
fn a_name_resolves_exact_then_unique_prefix_and_an_id_is_unchanged() {
    let all = stored(&[
        ("s-1", Some("stroppy-rust")),
        ("s-2", Some("stroppy-go")),
        ("s-3", Some("stroppy")),
        ("s-4", None),
    ]);
    let r = |n: &str| resolve_session_name(&all, n, None);
    assert_eq!(r("STROPPY-Rust").unwrap(), "s-1", "exact, any case");
    assert_eq!(r("stroppy").unwrap(), "s-3", "exact beats prefix");
    assert_eq!(r("stroppy-g").unwrap(), "s-2", "unique prefix");
    assert_eq!(r("s-4").unwrap(), "s-4", "an id is an id");
}

/// Rows for the global resolver: (id, title, workspace, origin, parent).
type Row<'a> = (&'a str, &'a str, &'a str, Option<&'a str>, Option<&'a str>);

fn spread(rows: &[Row<'_>]) -> Vec<letibot_tokencore::store::StoredSession> {
    use letibot_tokencore::store::{SessionRecord, Store};
    let s = Store::open_in_memory().unwrap();
    for (id, title, ws, origin, parent) in rows {
        s.put_session(&SessionRecord {
            id: (*id).into(),
            title: Some((*title).into()),
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: (*ws).into(),
            owner: "op".into(),
            role: None,
            approvers: vec![],
            parent_session_id: parent.map(Into::into),
        })
        .unwrap();
        if let Some(o) = origin {
            s.set_session_origin(id, o).unwrap();
        }
    }
    s.list_sessions().unwrap()
}

#[test]
fn a_name_in_this_folder_wins_and_another_folders_is_found_only_without_one() {
    let all = spread(&[
        ("s-1", "stroppy.io", "/p/here", None, None),
        ("s-2", "stroppy.io", "/p/far", None, None),
        ("s-3", "far-only", "/p/far", None, None),
    ]);
    let here = std::path::Path::new("/p/here");
    match find_session(&all, "stroppy.io", here).unwrap() {
        Found::Here(id) => assert_eq!(id, "s-1"),
        f => panic!("{f:?}"),
    }
    match find_session(&all, "FAR-ONLY", here).unwrap() {
        Found::Elsewhere(r) => {
            assert_eq!(r.len(), 1);
            assert_eq!(
                (r[0].id.as_str(), r[0].workspace_root.as_str()),
                ("s-3", "/p/far")
            );
        }
        f => panic!("{f:?}"),
    }
    assert!(find_session(&all, "nope", here).is_err());
}

#[test]
fn a_folder_below_this_one_is_elsewhere_so_home_moves() {
    let all = spread(&[
        ("s-1", "home", "/u", None, None),
        ("s-2", "stroppy.io", "/u/Projects/stroppy-io", None, None),
        ("s-3", "wt", "/u/worktrees/x", None, None),
    ]);
    let home = std::path::Path::new("/u");
    match find_session(&all, "stroppy.io", home).unwrap() {
        Found::Elsewhere(r) => assert_eq!(r[0].id, "s-2"),
        f => panic!("a session in a subfolder of ~ is not here: {f:?}"),
    }
    match find_session(&all, "wt", home).unwrap() {
        Found::Here(id) => assert_eq!(id, "s-3"),
        f => panic!("this folder's own worktree is here: {f:?}"),
    }
}

#[test]
fn a_name_in_several_other_folders_gives_all_of_them() {
    let all = spread(&[
        ("s-1", "stroppy.io", "/p/a", None, None),
        ("s-2", "stroppy.io-old", "/p/b", None, None),
    ]);
    match find_session(&all, "stroppy", std::path::Path::new("/p/here")).unwrap() {
        Found::Elsewhere(r) => assert_eq!(r.len(), 2),
        f => panic!("{f:?}"),
    }
}

#[test]
fn queue_package_and_subagent_sessions_never_match_a_global_name() {
    let all = spread(&[
        ("s-q", "stroppy", "/p/a", Some("queue"), None),
        ("s-p", "stroppy", "/p/b", Some("package"), None),
        ("s-c", "stroppy", "/p/c", None, Some("s-x")),
    ]);
    assert!(find_session(&all, "stroppy", std::path::Path::new("/p/here")).is_err());
}

/// A title typed with spaces and its slug are one name.
#[test]
fn a_slug_resolves_to_the_title_it_was_cut_from() {
    let all = stored(&[("s-1", Some("Stroppy Conformity")), ("s-2", Some("other"))]);
    let r = |n: &str| resolve_session_name(&all, n, None);
    assert_eq!(r("stroppy-conformity").unwrap(), "s-1");
    assert_eq!(r("Stroppy Conformity").unwrap(), "s-1");
}

#[test]
fn an_ambiguous_or_unknown_name_is_refused_with_the_names() {
    let all = stored(&[("s-1", Some("stroppy-rust")), ("s-2", Some("stroppy-go"))]);
    let e = resolve_session_name(&all, "stro", None).unwrap_err();
    assert!(e.contains("s-1") && e.contains("s-2"), "{e}");
    assert!(
        e.contains("stroppy-rust") && e.contains("stroppy-go"),
        "{e}"
    );
    let e = resolve_session_name(&all, "nope", None).unwrap_err();
    assert!(e.contains("no session named") && e.contains("s-1"), "{e}");
}

/// : the two asks, and the pair that means a VM on that machine - the mapping `run`
/// calls once the whole command line has been read. Each half alone is what it always was.
#[test]
fn a_vm_and_a_host_are_two_asks_and_the_pair_is_a_vm_there() {
    use letibot_tools::builtins::task::Placement;
    assert_eq!(placement_from_flags(false, None), Ok(Placement::Host));
    assert_eq!(
        placement_from_flags(true, None),
        Ok(Placement::Firecode),
        "`--where firecode` alone is a VM here"
    );
    assert_eq!(
        placement_from_flags(false, Some("lab2x1")),
        Ok(Placement::Remote("lab2x1".into()))
    );
    assert_eq!(
        placement_from_flags(true, Some("lab2x1")),
        Ok(Placement::RemoteVm("lab2x1".into())),
        "the pair is a VM on that machine, and both flags are honoured"
    );
}

/// A session's own fallback list replaces the box's, and its timeout budget rides along. The
/// list is where a project's sessions go when their model is out; a merge would inherit a third
/// name from the box file that this project never chose.
#[test]
fn a_sessions_fallback_list_replaces_the_boxs_and_carries_its_budget() {
    let mut cfg = Config::for_this_box("/tmp/r119-cli-fallback");
    cfg.fallback = vec!["box/one".into(), "box/two".into()];
    // A session file that says nothing leaves the box's answer alone.
    session_fallback(&mut cfg);
    assert_eq!(
        cfg.fallback,
        vec!["box/one".to_string(), "box/two".to_string()]
    );
    assert_eq!(cfg.fallback_after_timeouts, 3, "the daemon's own budget");

    cfg.leticode.fallback = vec!["glm/glm-4.6".into()];
    cfg.leticode.fallback_after_timeouts = Some(2);
    session_fallback(&mut cfg);
    assert_eq!(
        cfg.fallback,
        vec!["glm/glm-4.6".to_string()],
        "the session's list, not the box's two names and this one"
    );
    assert_eq!(cfg.fallback_after_timeouts, 2);
}

/// The Claude Code listing: the id a person types, then the facts a picker
/// row needs - and the two forms, because a script should not parse a table.
#[test]
fn the_claude_listing_names_the_id_the_model_and_the_first_ask() {
    let dir = std::env::temp_dir().join(format!("letibot-cli-claude-{}-{}", std::process::id(), {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("proj")).unwrap();
    let id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    std::fs::write(
        dir.join("proj").join(format!("{id}.jsonl")),
        concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-10-07T12:28:34.023Z","cwd":"/w","gitBranch":"main","version":"2.1.285","origin":{"kind":"human"},"message":{"role":"user","content":"make the tests pass"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-10-07T12:28:36.000Z","message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
            "\n"
        ),
    )
    .unwrap();

    // The tsv form: six columns, and the first is the id with the namespace mark this
    // head strips.
    assert_eq!(list_claude_in(&dir, true).unwrap(), 0);
    assert_eq!(list_claude_in(&dir, false).unwrap(), 0);

    // A directory that is not there is a sentence naming the directory, not an empty
    // listing - the same pair `--list-sessions` tells apart for the store.
    let absent = dir.join("not-there");
    let err = list_claude_in(&absent, false).unwrap_err();
    assert!(err.contains("no Claude Code transcript directory"), "{err}");
    assert!(
        err.contains("not-there"),
        "the sentence names the path: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The header names the model that ANSWERS the turns, and says the local binding is only the
/// binding.
#[test]
fn the_header_names_the_model_that_answers_and_labels_the_binding() {
    // A metered session: the provider answers, the local alias is only the vocabulary.
    let mut c = crate::config::Config::for_this_box("/tmp");
    c.model = "qwen-3.8-flash-next".into();
    c.endpoint = letibot_turn::Endpoint::new("127.0.0.1", 8080);
    c.provider = Some(crate::config::ProviderConfig {
        name: "glm-coding".into(),
        model: Some("glm-5.3".into()),
        api_key: None,
        thinking: false,
    });
    let lines = BannerModel::of(&c).lines();
    assert_eq!(lines.len(), 2, "two facts, two lines: {lines:?}");
    assert!(
        lines[0].starts_with("  model    glm-coding/glm-5.3 "),
        "the first line is what answers: {:?}",
        lines[0]
    );
    assert!(
        lines[1].starts_with("  vocab    qwen-3.8-flash-next via 127.0.0.1:8080")
            && lines[1].contains("NOT what answers this session's turns"),
        "and the binding is named as one: {:?}",
        lines[1]
    );

    // A local session: one line, and it is both facts at once.
    let mut c = crate::config::Config::for_this_box("/tmp");
    c.model = "qwen-3.8-flash-next".into();
    c.endpoint = letibot_turn::Endpoint::new("127.0.0.1", 8080);
    let lines = BannerModel::of(&c).lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("  model    qwen-3.8-flash-next via 127.0.0.1:8080")
            && !lines[0].contains("vocab"),
        "{:?}",
        lines[0]
    );
}

/// After a restart the banner prints the reserve and the RESUMED count in the provider window's
/// units, not the planning window's ledger units.
#[test]
fn the_banner_prints_counts_in_the_windows_units() {
    let mut c = crate::config::Config::for_this_box("/tmp");
    c.context_window = Some(40_000);
    c.auto_compact = true;
    c.ledger_scale = Some((21_212, 18_246));
    let line = context_line(&c);
    assert!(
        line.contains("40000 tokens; compacts automatically with 2500 left"),
        "{line}"
    );

    let r = crate::harness::ResumeReport {
        transcript_id: "s-1#t3".into(),
        rows: 12,
        tokens: 21_212,
        head: "0123456789abcdef0123".into(),
        workspace: "/tmp".into(),
        notes: Vec::new(),
    };
    assert_eq!(
        resumed_line(&r, Some((18_246, "deepseek tokens".into()))),
        "  RESUMED  s-1#t3 — 12 row(s), 18246 deepseek tokens (21212 ledger tokens), \
             head 0123456789abcdef"
    );
    assert_eq!(
        resumed_line(&r, Some((21_212, "tokens".into()))),
        "  RESUMED  s-1#t3 — 12 row(s), 21212 tokens, head 0123456789abcdef"
    );
}
use letibot_tokencore::store::{SessionRecord, Store};

fn store_with(workspaces: &[(&str, &str)]) -> (Store, TempDir) {
    let d = TempDir::new();
    let store = Store::open(&d.0.join("sessions.db")).expect("opening");
    for (id, root) in workspaces {
        store
            .put_session(&SessionRecord {
                id: (*id).into(),
                title: None,
                model_id: "m".into(),
                dialect_sha: "d".into(),
                workspace_root: (*root).into(),
                owner: "dead".into(),
                approvers: vec![],
                role: None,
                parent_session_id: None,
            })
            .expect("session");
    }
    (store, d)
}

/// A session row with the four fields the resolution reads: where it was opened, what it is
/// called, the seat it was recorded in, and the session that spawned it.
fn session_row(
    store: &Store,
    id: &str,
    workspace: &str,
    title: Option<&str>,
    role: Option<&str>,
    parent: Option<&str>,
) {
    store
        .put_session(&SessionRecord {
            id: id.into(),
            title: title.map(str::to_string),
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: workspace.into(),
            owner: "dead".into(),
            approvers: vec![],
            role: role.map(str::to_string),
            parent_session_id: parent.map(str::to_string),
        })
        .expect("session");
}

/// Give a session rows, and make sure the store's clock has passed `after_ms` FIRST.
/// Returns the `last_activity_ms` the store then reports, which is what the resolution
/// orders by.
fn rows_for(store: &Store, id: &str, rows: u32, after_ms: i64) -> i64 {
    wait_past(after_ms);
    let prefix = store
        .put_stable_prefix(&letibot_tokencore::store::StablePrefixRecord {
            dialect_sha: "d".into(),
            system: format!("system of {id}"),
            tools_json: vec![],
            tokens: vec![1, 2, 3],
            h_init: [0u8; 32],
            vocab_source: "test".into(),
            effort: None,
        })
        .expect("prefix");
    let transcript = format!("{id}#t0");
    store
        .put_transcript(&transcript, id, &prefix)
        .expect("transcript");
    for seq in 0..rows {
        let tokens: Vec<u32> = vec![7, 8, 9];
        store
            .append_item(
                &transcript,
                seq,
                &letibot_transcript::TranscriptItem::User {
                    speaker: Default::default(),
                    parts: vec![letibot_transcript::UserPart::Text {
                        text: format!("row {seq} of {id}"),
                    }],
                },
                &letibot_tokencore::ledger::LedgerRow {
                    item_id: format!("{transcript}.{seq}"),
                    // The prefix above is three tokens long, so the first row starts
                    // after them - the store refuses a row that starts anywhere else
                    // because a resume rebuilds the chain from the prefix's length.
                    tok_offset: 3 + seq * 3,
                    tok_len: 3,
                    h_k: [0u8; 32],
                },
                &tokens,
            )
            .expect("a row");
    }
    // Read back what the store says rather than what this function believes: the stamp is
    // the store's, and it is the number the resolution orders by.
    store
        .session(id)
        .expect("reading")
        .expect("the row just written")
        .last_activity_ms
}

/// Wait until the store's own clock has passed `ms` - for a fixture whose time is stamped at
/// write time and cannot be moved afterwards.
fn wait_past(ms: i64) {
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    };
    assert!(
        crate::testing::wait_until(crate::testing::PATIENCE, || now() > ms),
        "the store's clock never passed {ms}ms"
    );
}

struct TempDir(std::path::PathBuf);
impl TempDir {
    /// A counter in the name, not only the clock. The tests in this module run in parallel
    /// in one process, and macOS's `SystemTime` has microsecond resolution
    fn new() -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "harnessd-scope-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).expect("scratch");
        TempDir(p)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A daemon and a fixture both say `host` until something asks otherwise - the two
/// halves of the property [`Config::reviewers`] states. Both halves are asserted because
/// either alone is a lie
#[test]
fn a_fixture_and_a_daemon_both_place_reviewers_on_the_host_unless_asked() {
    assert_eq!(
        Config::for_this_box("/tmp").reviewers,
        crate::config::Reviewers::Host,
        "a fixture built the ordinary way must state no VM, or a test boots one by accident"
    );
    assert_eq!(
        Config::for_the_daemon("/tmp").reviewers,
        crate::config::Reviewers::Host,
        "the daemon places reviewers on the host unless the project or the flag asks"
    );
    let mut asked = Config::for_the_daemon("/tmp");
    asked.allow_vm_reviewers();
    assert_eq!(asked.reviewers, crate::config::Reviewers::Vm);
}

/// Reviewers stay on this machine unless `reviewer_hosts` is set: a fixture and a
/// daemon both start with the pool `["local"]`.
#[test]
fn a_fixture_and_a_daemon_keep_reviewers_on_this_machine_unless_hosts_are_named() {
    let only = Some(vec!["local".to_string()]);
    assert_eq!(Config::for_this_box("/tmp").reviewer_hosts, only);
    assert_eq!(Config::for_the_daemon("/tmp").reviewer_hosts, only);
}

/// `--reviewers` is parsed, both spellings, and it wins over the file - the flag
/// `--help` has advertised since the two words existed and the parser did not have.
#[test]
fn the_reviewers_flag_takes_both_spellings_and_overrides_the_project() {
    // The default, with no flag at all.
    let mut cfg = Config::for_the_daemon("/tmp");
    assert_eq!(cfg.reviewers, Reviewers::Host, "no flag, no VM");

    // `--reviewers vm` - a spelling that is accepted, not *unknown argument*.
    apply_reviewers_flag(&mut cfg, "vm").expect("`vm` is one of the two words");
    assert_eq!(cfg.reviewers, Reviewers::Vm);

    // `--reviewers host` puts it back over a config that says `vm`, which is the whole point
    // of the flag outranking the project file.
    apply_reviewers_flag(&mut cfg, "host").expect("`host` is the other");
    assert_eq!(cfg.reviewers, Reviewers::Host);

    // A third word is refused BY NAME with both of them in the sentence, rather than
    // silently leaving whatever was there.
    cfg.allow_vm_reviewers();
    let why = apply_reviewers_flag(&mut cfg, "firecode").unwrap_err();
    assert!(why.contains("\"firecode\""), "{why}");
    assert!(
        why.contains("\"host\"") && why.contains("\"vm\""),
        "the refusal names both words: {why}"
    );
    assert_eq!(
        cfg.reviewers,
        Reviewers::Vm,
        "a refused word changes nothing"
    );
}

/// A `main_model` from a file lands on the right `cfg` field. A `provider/model` name
/// sets the metered provider, and a bare alias sets the local model and clears the
/// provider, because the two are the two ways a session's turns go and a name is one or the
#[test]
fn a_file_main_model_lands_on_the_right_config_field() {
    // A `provider/model` name sets the metered provider.
    let mut cfg = Config::for_this_box("/tmp");
    assert_eq!(
        apply_main_model(&mut cfg, "deepseek/deepseek-chat", None),
        None
    );
    let pc = cfg
        .provider
        .expect("a provider/model name sets the provider");
    assert_eq!(pc.name, "deepseek");
    assert_eq!(pc.model.as_deref(), Some("deepseek-chat"));
    assert!(
        pc.api_key.is_none(),
        "the key is resolved along the usual path"
    );

    // A bare alias sets the local model and clears the provider. The clearing
    // is the whole point: this is the line that takes a session off
    // `[default] provider = deepseek` and back onto the box's own model.
    let mut cfg = Config::for_this_box("/tmp");
    cfg.provider = Some(crate::config::ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    assert_eq!(
        apply_main_model(&mut cfg, "qwen-3.8-flash-next", None),
        None
    );
    assert_eq!(cfg.model, "qwen-3.8-flash-next");
    assert!(cfg.provider.is_none(), "a bare alias is a local session");
}

/// A file's alias does not overwrite the binding `--model` set, and says so. The
/// launcher passes `--model` on every start, and what it names is the vocabulary this
/// daemon tokenises with.
#[test]
fn a_file_alias_does_not_rebind_the_command_line_and_says_so() {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.provider = Some(crate::config::ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    let note = apply_main_model(&mut cfg, "glm-5.3-flash", Some("qwen-3.8-flash-next"))
        .expect("a disagreement is said, not swallowed");
    assert!(
        note.contains("glm-5.3-flash") && note.contains("qwen-3.8-flash-next"),
        "the sentence names both: {note}"
    );
    assert_eq!(
        cfg.model, "qwen-3.8-flash-next",
        "the binding is the command line's"
    );
    assert!(
        cfg.provider.is_none(),
        "and the file still cleared the provider"
    );

    // Agreeing is not a disagreement: the file's alias IS the binding, and it
    // applies with nothing to say.
    let mut cfg = Config::for_this_box("/tmp");
    assert_eq!(
        apply_main_model(&mut cfg, "glm-5.3-flash", Some("glm-5.3-flash")),
        None
    );
    assert_eq!(cfg.model, "glm-5.3-flash");
}

/// `--model deepseek` is the door AND the model it sends, not the word `deepseek` as a model id:
/// the API refused that once, and the agent then picked a model four times the price. The catalogue
/// is a fixture file, so nothing here reaches the network.
#[test]
fn a_bare_model_that_is_a_door_resolves_to_that_doors_default_model() {
    let dir = TempDir::new();
    let mut env = letibot_testenv::EnvGuard::lock();
    let (cat, file) = door_fixture(&dir, "plain", "");
    env.set("LETIBOT_MODELS_JSON", &cat);
    let mut cfg = Config::for_this_box("/tmp");
    cfg.providers_file = Some(file.clone());
    let said = cli_door_model(&mut cfg, "deepseek");
    assert_eq!(
        said,
        CliModel::Door {
            said: Some("deepseek -> deepseek/deepseek-flash".into())
        }
    );
    let pc = cfg.provider.clone().expect("the door answers the turns");
    assert_eq!(pc.name, "deepseek");
    assert_eq!(pc.model.as_deref(), Some("deepseek-flash"));

    // The pair is what it says it is, and the alias keeps today's meaning.
    let mut cfg = Config::for_this_box("/tmp");
    cfg.providers_file = Some(file.clone());
    assert_eq!(
        cli_door_model(&mut cfg, "deepseek/deepseek-v4-pro"),
        CliModel::Door { said: None },
        "a pair needs no sentence: its start line already says what it is"
    );
    assert_eq!(
        cfg.provider
            .as_ref()
            .and_then(|p| p.model.clone())
            .as_deref(),
        Some("deepseek-v4-pro")
    );
    let mut cfg = Config::for_this_box("/tmp");
    cfg.providers_file = Some(file.clone());
    assert_eq!(
        cli_door_model(&mut cfg, "qwen-3.8-flash-next"),
        CliModel::Local,
        "a name no door claims is the local binding, as it always was"
    );
    assert!(cfg.provider.is_none());
    assert_eq!(cli_door_model(&mut cfg, "local"), CliModel::Local);

    // A door the box's own file answers to as a model keeps the LOCAL meaning.
    let (cat, local) = door_fixture(
        &dir,
        "local",
        "\n[model.\"deepseek\"]\nurl = \"http://127.0.0.1:9\"\nmodel = \"local-probe\"\n",
    );
    env.set("LETIBOT_MODELS_JSON", &cat);
    let mut cfg = Config::for_this_box("/tmp");
    cfg.providers_file = Some(local);
    assert_eq!(cli_door_model(&mut cfg, "deepseek"), CliModel::Local);
    assert!(cfg.provider.is_none(), "the local meaning is kept");
}

/// The door's own `[<preset>] model` in providers.toml is what a bare word sends, over the
/// catalogue's rule and over the build's frozen fallback.
#[test]
fn a_providers_toml_model_line_is_what_a_bare_door_sends() {
    let dir = TempDir::new();
    let mut env = letibot_testenv::EnvGuard::lock();
    let (cat, file) = door_fixture(&dir, "line", "\n[deepseek]\nmodel = \"deepseek-v4-pro\"\n");
    env.set("LETIBOT_MODELS_JSON", &cat);
    let mut cfg = Config::for_this_box("/tmp");
    cfg.providers_file = Some(file);
    let said = cli_door_model(&mut cfg, "deepseek");
    assert_eq!(
        said,
        CliModel::Door {
            said: Some("deepseek -> deepseek/deepseek-v4-pro".into())
        }
    );
    assert_eq!(
        cfg.provider.and_then(|p| p.model).as_deref(),
        Some("deepseek-v4-pro")
    );
}

/// A catalogue carrying deepseek's two models and a providers.toml that holds its key, both in
/// their own directory under `dir`. The windows are larger than the vendored floor's, which `load`
/// merges under any file: an equal window would let the snapshot's own models decide the rule. The
/// caller pins the catalogue because `EnvGuard` is not reentrant.
fn door_fixture(
    dir: &TempDir,
    name: &str,
    extra: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let here = dir.0.join(name);
    std::fs::create_dir_all(&here).expect("the fixture's own directory");
    let cat = here.join("models.json");
    std::fs::write(
        &cat,
        r#"{"deepseek":{"models":{
            "deepseek-flash":{"limit":{"context":8000000},"cost":{"input":0.15,"output":0.6}},
            "deepseek-v4-pro":{"limit":{"context":4000000},"cost":{"input":0.66,"output":1.98}}}}}}"#,
    )
    .expect("the fixture catalogue");
    let file = here.join("providers.toml");
    std::fs::write(&file, format!("[deepseek]\nkey = \"sk-probe\"\n{extra}"))
        .expect("the fixture providers file");
    (cat, file)
}

/// A model a file named that this box cannot reach is named at startup. The three
/// shapes the tree has paid for: a provider this build does not know, a provider with no
/// key here, and a local alias the server is not serving.
#[test]
fn a_model_a_file_named_that_cannot_be_reached_is_named() {
    let cfg = Config::for_this_box("/tmp");

    // A provider this build does not know.
    let why = unreachable_provider("notaprovider/x").expect("an unknown provider is named");
    assert!(why.contains("notaprovider"), "{why}");

    // A provider this build knows, with no key on this box. `grok` is the one preset whose key
    // this tree documents as absent on the operator's box - the opencode entry is `type
    // oauth`, and an oauth entry is deliberately not read as a key.
    if let Some(why) = unreachable_provider("grok/grok-4-fast") {
        assert!(why.contains("grok"), "{why}");
    }

    // A local alias the server is not serving, with no block to switch to.
    let why = unreachable_local(&cfg, "no-such-alias", Some("qwen-3.8-flash-next"))
        .expect("a local alias nothing serves is named");
    assert!(why.contains("no-such-alias"), "{why}");
    assert!(
        why.contains("400"),
        "and it says what the turn would be: {why}"
    );

    // The reachable cases are silent: the word `local`, an alias the server is
    // serving, a `[model."..."]` block with an address, and any local name at all
    // when the file did not set `main_model`.
    assert_eq!(unreachable_local(&cfg, "local", None), None);
    assert_eq!(
        unreachable_local(&cfg, "qwen-3.8-flash-next", Some("qwen-3.8-flash-next")),
        None,
        "the alias the server reports"
    );
    assert_eq!(unreachable_provider("glm-5.3-flash"), None);
}

/// `unreachable_models` asks the local half only of `main_model`. The guard's model
/// answers at the `[gatekeeper]` endpoint and a subagent's is decided per spawn, so
/// comparing either against this daemon's own server would be a false alarm - and a banner
#[test]
fn only_main_model_is_checked_against_this_daemons_own_server() {
    use crate::leticode_config::LeticodeConfig;
    let cfg = Config::for_this_box("/tmp");
    let files = LeticodeConfig {
        main_model: Some("no-such-alias".into()),
        subagent_model: Some("also-not-served".into()),
        gatekeeper_model: Some("qwen-3.8-27b".into()),
        ..Default::default()
    };
    let found = unreachable_models(&cfg, &files, Some("qwen-3.8-flash-next"));
    assert_eq!(
        found.len(),
        1,
        "one fault, and it is main_model's: {found:?}"
    );
    assert!(found[0].contains("no-such-alias"), "{found:?}");
    assert!(
        !found.iter().any(|w| w.contains("also-not-served")),
        "a subagent's model is decided per spawn, not here: {found:?}"
    );
}

/// The empty scope matched everything. `Path::new("/a").starts_with("")` is true, so a
/// store query scoped to the empty path silently widened to the whole box - and
/// `--continue` then reopened the newest conversation anywhere, which is what the operator
#[test]
fn an_empty_scope_is_refused_rather_than_matching_every_session() {
    let (store, _d) = store_with(&[
        ("s-a", "/home/dead/Projects/leticl"),
        ("s-b", "/home/dead/Projects/rano"),
    ]);
    let e =
        scoped(&store, Some(std::path::Path::new(""))).expect_err("an empty scope is not a scope");
    assert!(e.contains("matches every session"), "{e}");

    // A real scope still narrows, and no scope at all still means the whole
    // store - the deliberate case, which this must not break.
    assert_eq!(
        scoped(
            &store,
            Some(std::path::Path::new("/home/dead/Projects/rano"))
        )
        .expect("scoping")
        .len(),
        1
    );
    assert_eq!(scoped(&store, None).expect("unscoped").len(), 2);
}

/// The prefix match reaches DOWNWARDS only: a session opened in a subdirectory
/// belongs to the tree above it. This is the behaviour `scoped`'s own comment
/// describes, pinned so the empty-path guard above cannot quietly change it.
#[test]
fn a_scope_finds_sessions_opened_beneath_it() {
    let (store, _d) = store_with(&[("s-a", "/home/dead/Projects/rano/crates/ui")]);
    assert_eq!(
        scoped(
            &store,
            Some(std::path::Path::new("/home/dead/Projects/rano"))
        )
        .expect("scoping")
        .len(),
        1,
        "a session in a subdirectory belongs to the tree"
    );
    // And not sideways into a sibling.
    assert!(
        scoped(
            &store,
            Some(std::path::Path::new("/home/dead/Projects/leticl"))
        )
        .expect("scoping")
        .is_empty()
    );
}

/// The listing's archive filter, at the one helper the flag reaches: a put-away session is out of
/// the default listing, beside the rest under `--archived`, and alone under `--archived-only`.
/// `scoped` is the default that every other store verb takes, so it is asserted to be the filter.
#[test]
fn a_put_away_session_is_listed_only_when_it_is_asked_for() {
    use letibot_tokencore::store::Archived;
    let (store, _d) = store_with(&[
        ("s-here", "/home/dead/Projects/rano"),
        ("s-away", "/home/dead/Projects/rano"),
    ]);
    store
        .set_archived("s-away", true, Some("the question was answered"))
        .expect("an archive");

    let ids = |which: Archived| -> Vec<String> {
        let mut v: Vec<String> = scoped_archived(&store, None, which)
            .expect("a listing")
            .into_iter()
            .map(|r| r.id)
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        ids(Archived::No),
        vec!["s-here".to_string()],
        "the default listing leaves it out"
    );
    assert_eq!(
        ids(Archived::Yes),
        vec!["s-away".to_string()],
        "--archived-only is them alone"
    );
    assert_eq!(
        ids(Archived::Any),
        vec!["s-away".to_string(), "s-here".to_string()],
        "--archived puts it back beside the rest"
    );
    // And the row a person reads under `--archived` carries the words it was put away with.
    let rows = scoped_archived(&store, None, Archived::Yes).expect("a listing");
    assert_eq!(
        rows[0].archived_reason.as_deref(),
        Some("the question was answered")
    );
    assert_eq!(
        scoped(&store, None).expect("the default").len(),
        1,
        "`scoped` is the filter that leaves it out"
    );
}

/// The listing groups the store the way `group_of` says - mine first, then one entry
/// per automatic kind, each carrying its own members.
#[test]
fn the_listing_puts_mine_first_and_the_harnesss_own_work_in_its_groups() {
    let (store, _d) = store_with(&[("s-mine", "/ws"), ("s-sub", "/ws")]);
    // A grandchild of the operator's conversation, the queue and one of its reviewers, a
    // package's conversation, and a one-shot.
    store
        .set_session_origin("s-mine", "operator")
        .expect("the origin");
    session_row(&store, "s-gk-sub", "/ws", None, None, Some("s-sub"));
    store
        .set_session_origin("s-sub", "operator")
        .expect("the origin");
    let queue = "s-gatekeeper-0123456789ab";
    session_row(&store, queue, "/ws", None, None, None);
    session_row(&store, "s-gk", "/ws", None, None, Some(queue));
    store
        .set_session_origin(queue, "queue")
        .expect("the origin");
    session_row(&store, "s-pkg", "/ws", None, None, None);
    store
        .set_session_origin("s-pkg", "package")
        .expect("the origin");
    session_row(&store, "s-shot", "/ws", None, None, None);
    store
        .set_session_origin("s-shot", "one_shot")
        .expect("the origin");

    let rows = store.list_sessions().expect("listing");
    let grouped = grouped(&rows);
    let ids = |g: letibot_sessionlog::SessionGroup| -> Vec<String> {
        let mut v: Vec<String> = grouped
            .iter()
            .find(|(k, _)| *k == g)
            .map(|(_, m)| m.iter().map(|s| s.id.clone()).collect())
            .unwrap_or_default();
        // The store's order is recency's and this test is about membership, so the two are
        // sorted apart on purpose.
        v.sort();
        v
    };
    assert_eq!(
        ids(letibot_sessionlog::SessionGroup::Mine),
        ["s-mine", "s-sub"]
    );
    assert_eq!(
        ids(letibot_sessionlog::SessionGroup::Subagents),
        ["s-gk-sub"],
        "a task child is a subagent however deep"
    );
    assert_eq!(
        ids(letibot_sessionlog::SessionGroup::ReviewQueue),
        ["s-gatekeeper-0123456789ab", "s-gk"],
        "the queue's session and its reviewers are one group"
    );
    assert_eq!(ids(letibot_sessionlog::SessionGroup::Packages), ["s-pkg"]);
    assert_eq!(ids(letibot_sessionlog::SessionGroup::OneShot), ["s-shot"]);
    // Mine is first, which is the whole of *mine vs auto*: the order is
    // `SessionGroup::ALL`'s and the printer follows it.
    assert_eq!(grouped[0].0, letibot_sessionlog::SessionGroup::Mine);
}

/// Reviewers the merge queue spawned into a workspace make the newest row a reviewer, and
/// `--continue` must still reopen the conversation, not a review.
#[test]
fn a_reviewer_is_never_the_conversation_continue_reopens() {
    let (store, _d) = store_with(&[]);
    session_row(
        &store,
        "s-mine",
        "/ws",
        Some("the conversation"),
        None,
        None,
    );
    let mine = rows_for(&store, "s-mine", 3, 0);

    // The queue's reviewer: a child of that conversation, seated `gatekeeper`, its title cut
    // from the review brief - and NEWER than anything the person said.
    session_row(
        &store,
        "s-gk",
        "/ws",
        Some(&format!(
            "{} a subagent has finished work on a branch",
            letibot_sessionlog::GATEKEEPER_TITLE_PREFIX
        )),
        Some("gatekeeper"),
        Some("s-mine"),
    );
    let reviewer = rows_for(&store, "s-gk", 40, mine);
    assert!(
        reviewer > mine,
        "the fixture's reviewer has to be the newer row, or this proves nothing"
    );

    let latest = newest_conversation(
        store.list_sessions().expect("listing"),
        Some(std::path::Path::new("/ws")),
    );
    assert_eq!(
        latest.pick.as_ref().map(|s| s.id.as_str()),
        Some("s-mine"),
        "a reviewer is a session and not a conversation"
    );
    assert_eq!(
        latest.hidden, 1,
        "and it is counted as skipped, not quietly dropped"
    );
}

/// The seat carries the mark where the title cannot. A reviewer the person renamed
/// keeps the seat its row recorded, and a head rebuilding a row from the wire has no role
/// to read at all - only the brief's first words.
#[test]
fn a_reviewer_whose_title_was_changed_is_still_not_a_conversation() {
    let (store, _d) = store_with(&[]);
    session_row(
        &store,
        "s-mine",
        "/ws",
        Some("the conversation"),
        None,
        None,
    );
    let mine = rows_for(&store, "s-mine", 2, 0);
    // No parent and a title of the operator's own: nothing but the seat says what this is.
    session_row(
        &store,
        "s-gk",
        "/ws",
        Some("review of agent/idle-clock"),
        Some("gatekeeper"),
        None,
    );
    let reviewer = rows_for(&store, "s-gk", 9, mine);
    assert!(reviewer > mine, "the reviewer is the newer row");

    let latest = newest_conversation(
        store.list_sessions().expect("listing"),
        Some(std::path::Path::new("/ws")),
    );
    assert_eq!(
        latest.pick.as_ref().map(|s| s.id.as_str()),
        Some("s-mine"),
        "the seat is the mark that survives a rename"
    );
}

/// A task child that is newer than its parent is not the answer, and it does not lift
/// its parent's time either
#[test]
fn a_task_child_newer_than_its_parent_is_not_the_conversation() {
    let (store, _d) = store_with(&[]);
    session_row(
        &store,
        "s-mine",
        "/ws",
        Some("the conversation"),
        None,
        None,
    );
    let mine = rows_for(&store, "s-mine", 2, 0);
    // A plain `task` child: no gatekeeper mark anywhere, a parent and nothing else.
    session_row(
        &store,
        "s-task",
        "/ws",
        Some("survey the crate"),
        Some("coder"),
        Some("s-mine"),
    );
    let child = rows_for(&store, "s-task", 12, mine);
    assert!(child > mine, "the child is the newer row");

    let latest = newest_conversation(
        store.list_sessions().expect("listing"),
        Some(std::path::Path::new("/ws")),
    );
    assert_eq!(
        latest.pick.as_ref().map(|s| s.id.as_str()),
        Some("s-mine"),
        "the parent is where a person continues, however recently the child worked"
    );
    assert_eq!(latest.hidden, 1);
}

/// A child whose parent is gone is still a child, and a workspace holding nothing else
/// has nothing to continue. What matters here is the sentence
#[test]
fn a_workspace_holding_nothing_but_children_says_so() {
    let (store, _d) = store_with(&[]);
    // The parent row was deleted (`--delete` takes a session with no rows) or lives in
    // another store; either way this row is a sub-session with nobody above it.
    session_row(
        &store,
        "s-orphan",
        "/ws",
        Some("a child of a session that is gone"),
        Some("coder"),
        Some("s-parent-nobody-has"),
    );
    rows_for(&store, "s-orphan", 4, 0);

    let latest = newest_conversation(
        store.list_sessions().expect("listing"),
        Some(std::path::Path::new("/ws")),
    );
    assert!(latest.pick.is_none(), "an orphan is not a conversation");
    let said = nothing_to_continue(&latest, Some(std::path::Path::new("/ws")));
    assert!(
        said.contains("sub-session"),
        "it names what is there: {said}"
    );
    assert!(said.contains("/ws"), "and where: {said}");
    assert!(
        said.contains("--session ID") && said.contains("session list"),
        "and what a person can do about it: {said}"
    );

    // A store with nothing in it at all is a different sentence, and the old one.
    let empty = newest_conversation(Vec::new(), Some(std::path::Path::new("/ws")));
    assert!(empty.pick.is_none());
    assert_eq!(
        nothing_to_continue(&empty, Some(std::path::Path::new("/ws"))),
        "no stored session under /ws"
    );
}

/// A conversation nobody ever said anything in is skipped - the rule that was here
/// before the reviewer was, pinned so the narrowing above cannot quietly drop it. These
/// rows are often the NEWEST in a store
#[test]
fn a_conversation_with_no_rows_is_not_continued() {
    let (store, _d) = store_with(&[]);
    session_row(&store, "s-old", "/ws", Some("something I said"), None, None);
    let said = rows_for(&store, "s-old", 5, 0);
    // Opened after it, and never prompted into: a row whose time is its creation time, so
    // the clock has to have moved before it is written.
    wait_past(said);
    session_row(&store, "s-blank", "/ws", None, None, None);
    assert!(
        store
            .session("s-blank")
            .expect("reading")
            .expect("the row")
            .last_activity_ms
            > said,
        "the empty conversation has to be the newer row"
    );

    let latest = newest_conversation(
        store.list_sessions().expect("listing"),
        Some(std::path::Path::new("/ws")),
    );
    assert_eq!(latest.pick.as_ref().map(|s| s.id.as_str()), Some("s-old"));
    assert_eq!(latest.empty, 1);
}

/// The exact workspace still beats a descendant of it - asserted here because that sort
/// moved into [`newest_conversation`] and an untested move is a rule nobody is holding.
#[test]
fn an_exact_workspace_beats_a_descendant_of_it() {
    let (store, _d) = store_with(&[]);
    session_row(&store, "s-here", "/ws", Some("here"), None, None);
    let here = rows_for(&store, "s-here", 2, 0);
    session_row(
        &store,
        "s-below",
        "/ws/crates/ui",
        Some("below"),
        None,
        None,
    );
    let below = rows_for(&store, "s-below", 2, here);
    assert!(below > here, "the descendant is the newer row");

    let latest = newest_conversation(
        store.list_sessions().expect("listing"),
        Some(std::path::Path::new("/ws")),
    );
    assert_eq!(
        latest.pick.as_ref().map(|s| s.id.as_str()),
        Some("s-here"),
        "the subtree match is a fallback, not a licence to reach past a closer answer"
    );
}

/// The queue's own session is seated as an orchestrator and titled by its board, so no seat
/// or title mark names it. Its id does, and it must not be the one `--continue` reopens.
#[test]
fn continue_skips_the_queues_own_session_by_id() {
    let (store, _d) = store_with(&[]);
    session_row(
        &store,
        "s-mine",
        "/ws",
        Some("mine"),
        Some("leticode"),
        None,
    );
    let mine = rows_for(&store, "s-mine", 2, 0);
    session_row(
        &store,
        "s-gatekeeper-c2f63a0e6a17",
        "/ws",
        Some("the review queue"),
        Some("orchestrator"),
        None,
    );
    let gk = rows_for(&store, "s-gatekeeper-c2f63a0e6a17", 5, mine);
    assert!(gk > mine, "the queue's session has to be the newer row");

    let latest = newest_conversation(
        store.list_sessions().expect("listing"),
        Some(std::path::Path::new("/ws")),
    );
    assert_eq!(latest.pick.as_ref().map(|s| s.id.as_str()), Some("s-mine"));
    assert_eq!(latest.hidden, 1, "the queue's session is counted as hidden");
}

/// `session list` is the conversations a person picks from: this folder's first - its own
/// workspace ahead of the worktrees below it - then one block per other folder, newest first, and
/// never a task child or a package's own conversation.
#[test]
fn a_plain_listing_is_this_folders_conversations_then_one_block_per_other_folder() {
    let (store, _d) = store_with(&[]);
    // Written oldest first, so the store's own order is the reverse of this list.
    session_row(&store, "s-far", "/p/far", Some("far"), None, None);
    let far = rows_for(&store, "s-far", 1, 0);
    session_row(
        &store,
        "s-wt",
        "/p/here/worktrees/x",
        Some("a worktree"),
        None,
        None,
    );
    let wt = rows_for(&store, "s-wt", 1, far);
    session_row(&store, "s-here", "/p/here", Some("here"), None, None);
    let here = rows_for(&store, "s-here", 1, wt);
    session_row(&store, "s-new", "/p/new", Some("new"), None, None);
    rows_for(&store, "s-new", 1, here);
    // Neither of these two is a conversation a person picks from, though both are sessions in
    // this folder and one of them is the newest row in the store.
    session_row(
        &store,
        "s-child",
        "/p/here",
        Some("child"),
        None,
        Some("s-here"),
    );
    rows_for(&store, "s-child", 2, here + 1);
    session_row(&store, "s-pkg", "/p/here", Some("pkg"), None, None);
    store.set_session_origin("s-pkg", "package").unwrap();

    let all = store.list_sessions().expect("listing");
    let (mine, others) = conversation_blocks(&all, std::path::Path::new("/p/here"));
    let ids = |rows: &[&letibot_tokencore::store::StoredSession]| -> Vec<String> {
        rows.iter().map(|s| s.id.clone()).collect()
    };
    assert_eq!(
        ids(&mine),
        ["s-here", "s-wt"],
        "this folder's own conversation, then the one in its worktree"
    );
    assert_eq!(
        others
            .iter()
            .map(|(ws, members)| (*ws, ids(members)))
            .collect::<Vec<_>>(),
        [
            ("/p/new", vec!["s-new".to_string()]),
            ("/p/far", vec!["s-far".to_string()]),
        ],
        "one block per other folder, the folder used most recently first"
    );
}

/// The help is one line per flag, and a line fits a terminal. A continuation that loses its
/// `\n` welds two flags onto one line: `--room`'s sentence ran into `--reviewers`' for as long as it
/// took somebody to read it.
#[test]
fn the_usage_keeps_one_flag_per_line() {
    let text = usage();
    assert!(text.contains("EVERY FLAG THIS PARSER READS"), "{text}");
    let body: Vec<&str> = text.lines().filter(|l| l.starts_with("  -")).collect();
    assert!(
        body.len() >= help::FLAGS.len(),
        "one line per flag: {} lines for {} flags",
        body.len(),
        help::FLAGS.len()
    );
    for line in &body {
        assert!(
            line.chars().count() <= 100,
            "a usage line is {} columns, so two lines were welded together: {line:?}",
            line.chars().count()
        );
    }
    for flag in [
        "--parent-seat",
        "--no-queue",
        "--room",
        "--reviewers",
        "--gate-host",
        "--reviewers-at-once",
        "--reviewer-hosts",
    ] {
        assert!(
            body.iter().any(|l| l.starts_with(&format!("  {flag} "))),
            "`{flag}` must start its own line:\n{text}"
        );
    }
}

/// A help line that names a value the parser refuses is a refusal nobody can act on: `/mode
/// supervised` was in this text and there is no such command - the flag is `--supervise`.
#[test]
fn the_usage_names_the_supervise_command_that_exists() {
    let text = usage();
    assert!(text.contains("--supervise"), "{text}");
    assert!(!text.contains("/mode supervised"), "{text}");
}

/// `--continue` in a folder whose review queue spoke last reopens the operator's own
/// conversation, not the queue's session: the queue is seated read-only, so taking it lost
/// every mutating tool. Its title is re-worded and its seat is a default one, so the rule is its id.
#[test]
fn the_queue_session_is_never_the_conversation_to_continue() {
    let (store, _d) = store_with(&[]);
    session_row(&store, "s-mine", "/ws", Some("arango work"), None, None);
    let mine = rows_for(&store, "s-mine", 2, 0);
    let queue = letibot_sessionlog::gatekeeper_session(std::path::Path::new("/ws"));
    session_row(
        &store,
        &queue,
        "/ws",
        Some("review the fuzz branch"),
        None,
        None,
    );
    let q = rows_for(&store, &queue, 3, mine);
    assert!(q > mine, "the queue's session is the newer row");

    let latest = newest_conversation(
        store.list_sessions().expect("listing"),
        Some(std::path::Path::new("/ws")),
    );
    assert_eq!(latest.pick.as_ref().map(|s| s.id.as_str()), Some("s-mine"));
    assert_eq!(latest.hidden, 1, "the queue's session is counted as hidden");
}

// --- the flag table, and the help generated from it ------------------------------------------
// `--help` and the parser read one table; these tests hold that in each direction.

/// Every row the table holds is one this parser reads, and the help prints it.
#[test]
fn the_help_lists_every_flag_the_table_holds() {
    let help = help::usage();
    for f in help::FLAGS {
        for name in f.names {
            assert!(
                help.contains(name),
                "the help does not print {name}, which this parser reads"
            );
            check_known(name).unwrap_or_else(|e| panic!("{name} is in the table: {e}"));
        }
    }
    // One line per row, so a row that is in the table cannot be missing from the page.
    let listed = help
        .lines()
        .filter(|l| l.starts_with("  -") || l.starts_with("  harnessd"))
        .count();
    assert!(
        listed >= help::FLAGS.len(),
        "the help has {listed} lines for {} flags",
        help::FLAGS.len()
    );
}

/// Every flag the help names reaches an arm of the parser. The table decides what is
/// accepted, so a row with no arm would be accepted here and refused there; asking for `--help`
/// after each one is what proves the arm exists without opening a store, a socket or a model.
#[test]
fn every_flag_the_help_names_reaches_an_arm() {
    for f in help::FLAGS {
        for name in f.names {
            let mut args: Vec<String> = vec![(*name).to_string()];
            // One dummy word per placeholder: `--handover SESSION DIR` consumes two.
            let words = f.value.map(|v| v.split_whitespace().count()).unwrap_or(0);
            for _ in 0..words {
                args.push("/nonexistent".into());
            }
            args.push("--help".into());
            if let Err(e) = run(&args) {
                assert!(
                    !e.contains("unknown argument"),
                    "{name} is in the table and the help, and the parser has no arm for it: {e}"
                );
            }
        }
    }
}

/// A removed flag is refused BY NAME, with what replaced it — not read as a prompt, and not
/// left to look like a typo.
#[test]
fn a_removed_flag_is_refused_by_name_with_its_replacement() {
    for (name, why) in help::REMOVED {
        let e = check_known(name).expect_err("a removed flag is refused");
        assert!(e.contains(name), "{name}: {e}");
        assert!(e.contains(why), "{name} must name its replacement: {e}");
        assert!(!e.contains("unknown argument"), "{name}: {e}");
        // And the page a person is pointed at names it too.
        assert!(
            help::usage().contains(name),
            "the help should say {name} is gone"
        );
    }
}

/// A config-backed flag the daemon APPLIES says so: the old wording ("is a config setting now")
/// read as "ignored", the defect this row is about, since `--model X` was applied while reading as
/// a flag with no effect.
#[test]
fn a_config_backed_flag_says_it_applies_to_this_run() {
    let said = help::config_notice("--model").expect("--model is config-backed");
    assert!(said.contains("applies to this run"), "{said}");
    assert!(!said.contains("config setting now"), "{said}");
    assert!(
        said.contains("providers.toml"),
        "it names where the setting lives: {said}"
    );
}

/// No spelling appears in two rows: a name that means two flags resolves by table order.
#[test]
fn no_flag_appears_twice() {
    let mut seen: Vec<&str> = Vec::new();
    for f in help::FLAGS {
        for name in f.names {
            assert!(!seen.contains(name), "{name} appears twice in the table");
            seen.push(name);
        }
    }
    for (name, _) in help::REMOVED {
        assert!(
            !seen.contains(name),
            "{name} is removed and still in the table"
        );
    }
}

#[test]
fn show_session_prints_one_fact_per_line_and_the_archive_reason() {
    let mut s = stored(&[("s-1", Some("gate red"))]).remove(0);
    let now = s.created_ms + 3 * 3_600_000;
    let out = show_session(&s, now);
    assert!(out.contains("id        s-1\n"), "{out}");
    assert!(out.contains("title     gate red\n"), "{out}");
    assert!(out.contains("role      -\n"), "{out}");
    assert!(out.contains("created   3h ago\n"), "{out}");
    assert!(out.ends_with("archived  no\n"), "{out}");
    s.archived_ms = Some(now - 120_000);
    s.archived_reason = Some("landed".into());
    assert!(show_session(&s, now).ends_with("archived  2m ago: landed\n"));
}

/// The daemon takes the launcher's five words and refuses anything else by naming them.
#[test]
fn effort_takes_the_five_words_of_the_one_scale() {
    for w in ["off", "low", "medium", "high", "max"] {
        assert_eq!(super::effort_flag(w).as_deref(), Ok(w));
    }
    assert_eq!(super::effort_flag("xhigh").as_deref(), Ok("max"));
    let why = super::effort_flag("heroic").unwrap_err();
    assert!(why.contains("off, low, medium, high, max"), "{why}");
}

/// `--spill-lines LINES` sets the line cap and `--spill-inline BYTES` the byte cap, each leaving
/// the other where it was; `--spill-inline 0` is still the whole cap off, and a value that is not a
/// number is refused with the flag's own name in the sentence.
#[test]
fn spill_lines_sets_the_line_cap_and_leaves_the_byte_cap_alone() {
    let mut cfg = Config::for_this_box("/tmp");
    assert_eq!(cfg.spill, SpillPolicy::FILES);

    super::apply_spill_flag(&mut cfg, "--spill-lines", "300").unwrap();
    assert_eq!(cfg.spill.lines, Some(300));
    assert_eq!(cfg.spill.bytes, None, "the byte cap is still the files'");
    assert_eq!(cfg.output_cap().lines, Some(300));
    assert_eq!(cfg.output_cap().bytes, Some(50 * 1024));

    // Both, in either order, each cap its own.
    super::apply_spill_flag(&mut cfg, "--spill-inline", "4096").unwrap();
    assert_eq!(
        (cfg.output_cap().bytes, cfg.output_cap().lines),
        (Some(4096), Some(300))
    );
    super::apply_spill_flag(&mut cfg, "--spill-lines", "7").unwrap();
    assert_eq!(
        (cfg.output_cap().bytes, cfg.output_cap().lines),
        (Some(4096), Some(7))
    );

    // `--spill-lines 0` is that cap off, not the whole cap; `--spill-inline 0` still is.
    super::apply_spill_flag(&mut cfg, "--spill-lines", "0").unwrap();
    assert_eq!(
        (cfg.output_cap().bytes, cfg.output_cap().lines),
        (Some(4096), None)
    );
    super::apply_spill_flag(&mut cfg, "--spill-inline", "0").unwrap();
    assert_eq!(
        (cfg.output_cap().bytes, cfg.output_cap().lines),
        (None, None)
    );
    let why = super::apply_spill_flag(&mut cfg, "--spill-lines", "lots").unwrap_err();
    assert!(why.contains("--spill-lines"), "{why}");
}

/// One open job for `session` in a store - the row the archive guard reads, and therefore the one
/// the one-shot's tail reads. No process is started: what this is about is the decision, and a box
/// with no cgroup (this one) cannot run the command that would write the row for real.
fn an_open_job(store: &letibot_tokencore::store::Store, session: &str, handle: &str) {
    let mut p = letibot_tokencore::store::ProcessRecord::starting(session, handle, None, 4242);
    p.state = letibot_tokencore::store::ProcState::Running;
    p.command = "cargo test".into();
    store.put_process(&p).expect("the job row");
}

fn ended(store: &letibot_tokencore::store::Store, session: &str, handle: &str) {
    let mut p = letibot_tokencore::store::ProcessRecord::starting(session, handle, None, 4242);
    p.state = letibot_tokencore::store::ProcState::Done;
    p.exit = Some(0);
    store.put_process(&p).expect("the ended row");
}

/// A store file of this test's own, answered as (its directory, the file). A record is opened on
/// it per site rather than shared: `Store` is not `Clone`, and two `Store::open`s of one path are
/// one connection in this process - so the row a thread writes is the row the loop reads, in the
/// order the watcher itself writes and rings in.
fn a_store(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = letibot_sessionlog::testing::scratch(&format!("letibot-oneshot-{tag}"));
    std::fs::create_dir_all(&dir).expect("the scratch dir");
    (dir.clone(), dir.join("sessions.db"))
}

fn a_reply(text: &str) -> crate::Outcome {
    crate::Outcome::Replied(Box::new(crate::harness::Reply {
        text: text.into(),
        metrics: Vec::new(),
        rounds: 1,
        tool_calls: 0,
        truncated: false,
    }))
}

/// A job's end is the one-shot's next turn: the loop waits on the settlement, wakes the session,
/// and answers `Settled` only once nothing is open.
#[test]
fn a_job_that_ends_is_a_turn_and_the_tail_settles() {
    let (dir, file) = a_store("settles");
    let store = letibot_tokencore::store::Store::open(&file).expect("a store");
    let bell = letibot_sessionlog::registry::Bell::new();
    an_open_job(&store, "s-shot", "j51");
    // The job's end as the watcher does it: the row settles first, then the bell is rung. A wait
    // that starts after it must answer at once, which is the whole reason both doors are read.
    let ring = bell.clone();
    let settler = file.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let writer = letibot_tokencore::store::Store::open(&settler).expect("the second handle");
        ended(&writer, "s-shot", "j51");
        ring.ring_settlement_wake("s-shot");
    });
    let mut turns = 0;
    let tail = super::oneshot::drain(
        &store,
        "s-shot",
        &bell,
        std::time::Instant::now() + std::time::Duration::from_secs(20),
        || {
            turns += 1;
            a_reply("the job finished")
        },
    );
    let _ = std::fs::remove_dir_all(&dir);
    match tail {
        super::oneshot::Tail::Settled { turns: ran } => assert_eq!(
            ran, 1,
            "the settlement's turn was not counted, so the run cannot say it ran one"
        ),
        super::oneshot::Tail::Over { jobs } => {
            panic!("the job ended and the tail still says {jobs:?} are open")
        }
    }
    assert_eq!(turns, 1, "the wake door was taken {turns} time(s)");
}

/// A job that never ends does not hang the run: the budget is the only clock, and it ends the
/// waiting with the handle in hand.
#[test]
fn a_job_that_never_ends_is_given_up_at_the_budget_naming_the_handle() {
    let (dir, file) = a_store("budget");
    let store = letibot_tokencore::store::Store::open(&file).expect("a store");
    let bell = letibot_sessionlog::registry::Bell::new();
    an_open_job(&store, "s-shot", "j51");
    let began = std::time::Instant::now();
    let tail = super::oneshot::drain(
        &store,
        "s-shot",
        &bell,
        began + std::time::Duration::from_millis(150),
        || panic!("nothing settled, so no turn may run"),
    );
    let spent = began.elapsed();
    let _ = std::fs::remove_dir_all(&dir);
    match tail {
        super::oneshot::Tail::Over { jobs } => assert_eq!(jobs, ["j51"]),
        super::oneshot::Tail::Settled { .. } => {
            panic!("a job no settlement ever named was settled")
        }
    }
    assert!(
        spent < std::time::Duration::from_secs(5),
        "the wait outlived its deadline by {spent:?}"
    );
    // The line a person reads: which jobs, and the door that resumes them.
    let line = super::oneshot::over_budget_line("s-shot", &["j51".into()], super::oneshot::BUDGET);
    assert!(line.contains("`j51`"), "{line}");
    assert!(line.contains("`letibot --session s-shot`"), "{line}");
    assert!(
        line.contains("6h"),
        "the default budget is said as a span: {line}"
    );
}

/// The run this must not change: no jobs is no tail at all, and the wake door is never taken.
#[test]
fn a_run_that_started_no_job_opens_no_tail() {
    let (dir, file) = a_store("none");
    let store = letibot_tokencore::store::Store::open(&file).expect("a store");
    let bell = letibot_sessionlog::registry::Bell::new();
    // Another session's job is not this one's: the tail reads the session's own rows, or every
    // one-shot on this box would wait for its neighbour's build.
    an_open_job(&store, "s-other", "j7");
    let tail = super::oneshot::drain(
        &store,
        "s-shot",
        &bell,
        std::time::Instant::now() + std::time::Duration::from_secs(20),
        || panic!("a session with no job of its own was woken"),
    );
    let _ = std::fs::remove_dir_all(&dir);
    match tail {
        super::oneshot::Tail::Settled { turns } => assert_eq!(turns, 0),
        super::oneshot::Tail::Over { jobs } => {
            panic!("another session's job was counted: {jobs:?}")
        }
    }
}

/// `--one-shot-budget` is a span, and `0` is refused rather than read as "no bound": the waiting
/// is what the number bounds, so a run without one is a run a wedged job can hold for ever.
#[test]
fn the_one_shot_budget_is_a_span_and_zero_is_refused() {
    use std::time::Duration;
    assert_eq!(
        super::oneshot::budget_from_flag("90s").unwrap(),
        Duration::from_secs(90)
    );
    assert_eq!(
        super::oneshot::budget_from_flag("1h30m").unwrap(),
        Duration::from_secs(90 * 60)
    );
    let zero = super::oneshot::budget_from_flag("0").unwrap_err();
    assert!(zero.contains("--one-shot-budget"), "{zero}");
    assert!(
        zero.contains("bound"),
        "the refusal says what is missing: {zero}"
    );
    let junk = super::oneshot::budget_from_flag("6").unwrap_err();
    assert!(junk.contains("--one-shot-budget"), "{junk}");
    // And the parser reads the flag at all: the help is generated from that table, so a flag the
    // table does not carry is one no operator could have found.
    assert!(super::help::knows("--one-shot-budget"));
}

/// The exit status, which is the half of the contract a script reads: `0` only when nothing is
/// still open, and the budget's `75` outranking a failed step.
#[test]
fn a_one_shot_passes_only_with_no_job_open() {
    assert_eq!(super::oneshot::status(false, 0), 0);
    assert_eq!(super::oneshot::status(false, 1), 1);
    assert_eq!(
        super::oneshot::status(true, 0),
        super::oneshot::EXIT_STILL_OPEN
    );
    assert_eq!(
        super::oneshot::status(true, 2),
        super::oneshot::EXIT_STILL_OPEN,
        "a job still open is the more specific fact, and it names the way back"
    );
}
