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

/// The Claude Code listing (Package H): the id a person types, then the facts a picker
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

/// follow-up, defect C: after a restart the banner printed the reserve in the
/// planning window's ledger units (2905) beside a 40000 provider window whose
/// reserve is 2500, and the RESUMED count as the ledger's length with no unit.
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
    while now() <= ms {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
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

/// Reviewers stay on this machine unless `reviewer_hosts` is set (D18): a fixture and a
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
/// per automatic kind, each carrying its own members. The operator's ask is one sentence
/// and it was about both places a session is listed
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

/// The operator's report, as a test. Ten reviewers spawned into one workspace by the
/// merge queue made the newest row in it a reviewer, and `--continue` - which asked only
/// for the newest row - reopened a review instead of the conversation that enqueued the
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
        said.contains("--session ID") && said.contains("--sessions"),
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

/// **The help is one line per flag, and a line fits a terminal.** A continuation that loses its
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
// The survey's item: `--help` listed fewer flags than the parser read, because one was a
// hand-kept list and the other was a `match`. Both are now the table, and these are the tests
// that say so in each direction.

/// **Every row the table holds is one this parser reads**, and the help prints it.
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

/// **Every flag the help names reaches an arm of the parser.** The table decides what is
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

/// **A removed flag is refused BY NAME, with what replaced it** — not read as a prompt, and not
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
