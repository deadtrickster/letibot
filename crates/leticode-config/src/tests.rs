use super::*;

/// A temp dir for a `leticode.toml` fixture, unique per test so parallel
/// tests do not share a file. Temp files, not the tree: nothing removes them,
/// and nothing outside `std::env::temp_dir` is ever written.
fn project_dir(test: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("letibot-leticode-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A fresh subdirectory, for a test that needs two files in one temp tree.
fn sub(dir: &Path, name: &str) -> PathBuf {
    let d = dir.join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The file a test case is about, written fresh so a rerun cannot see the
/// previous case's bytes.
fn write_file(dir: &Path, text: &str) -> PathBuf {
    let path = dir.join(FILE_NAME);
    std::fs::write(&path, text).unwrap();
    path
}

/// The walk-up finds the nearest file and stops at the root, so a session started in a
/// subdirectory finds the project's settings and a file in a parent does not shadow
/// one in the workspace.
#[test]
fn the_walk_up_finds_the_nearest_file_and_stops_at_the_root() {
    let root = project_dir("walk_up");
    // A file at the project root.
    let root_file = root.join(FILE_NAME);
    std::fs::write(&root_file, "main_model = \"root-model\"\n").unwrap();
    // A session started two levels down, with no file of its own.
    let sub = root.join("crates").join("harnessd");
    std::fs::create_dir_all(&sub).unwrap();
    let found = LeticodeConfig::discover(&sub).expect("the walk-up finds the root file");
    assert_eq!(
        found, root_file,
        "the nearest file at or above the workspace"
    );
    let cfg = LeticodeConfig::load(&found).unwrap();
    assert_eq!(cfg.main_model.as_deref(), Some("root-model"));

    // A file in the workspace beats one in a parent: the nearest wins.
    let sub_file = sub.join(FILE_NAME);
    std::fs::write(&sub_file, "main_model = \"sub-model\"\n").unwrap();
    let found = LeticodeConfig::discover(&sub).expect("the walk-up finds the sub file");
    assert_eq!(
        found, sub_file,
        "the workspace's own file is nearer than the root's"
    );
    let cfg = LeticodeConfig::load(&found).unwrap();
    assert_eq!(cfg.main_model.as_deref(), Some("sub-model"));

    // A workspace with no file at or above it is a no-file, and the walk
    // stops at the filesystem root rather than looping.
    let empty = project_dir("walk_up_empty");
    std::fs::remove_file(&root_file).unwrap();
    std::fs::remove_file(&sub_file).unwrap();
    assert!(
        LeticodeConfig::discover(&empty).is_none(),
        "no file at or above the workspace is a no-file"
    );
}

/// The precedence, stated once and checked here: command line, project file, user
/// file, built-in default, with an unset key falling through. The level comes back
/// with the name, because "which model" without "which file" is half an answer.
#[test]
fn the_precedence_is_flag_then_project_then_user_then_fall_through() {
    // The flag beats both files.
    assert_eq!(
        precedence(Some("cli"), Some("project"), Some("user")),
        Some(("cli".to_string(), Level::Flag))
    );
    // The project file beats the user file.
    assert_eq!(
        precedence(None, Some("project"), Some("user")),
        Some(("project".to_string(), Level::Project))
    );
    // The user file is the fall-through when the flag and the project are silent.
    assert_eq!(
        precedence(None, None, Some("user")),
        Some(("user".to_string(), Level::User))
    );
    // All unset is the fall-through to the built-in default, which the caller
    // names - `Level::Builtin` is the default variant for exactly this reason.
    assert_eq!(precedence(None, None, None), None);
    assert_eq!(Level::default(), Level::Builtin);
}

/// `[browser]` decides who may prompt sessions, so a project file's copy is ignored.
#[test]
fn browser_keys_are_read_checked_and_taken_from_the_user_file_only() {
    let u = project_dir("browser_user");
    let p = project_dir("browser_project");
    let user = LeticodeConfig::load_as(
        &write_file(
            &u,
            "[browser]\nport = 8791\nextension_id = \"kbaoealgdkldcgaojgdehmdaaekkmffd\"\n",
        ),
        Level::User,
    )
    .expect("the user file parses");
    assert_eq!(user.browser.port, Some(8791));
    let project =
        LeticodeConfig::load_as(&write_file(&p, "[browser]\nport = 9999\n"), Level::Project)
            .expect("the project file parses");
    assert_eq!(user.overridden_by(&project).browser, user.browser);
    let bad = write_file(&p, "[browser]\nextension_id = \"nope\"\n");
    let e = LeticodeConfig::load_as(&bad, Level::User).expect_err("not an id");
    assert!(e.contains("extension_id"), "{e}");
    let bad = write_file(&p, "[browser]\nhost = \"0.0.0.0\"\n");
    let e = LeticodeConfig::load_as(&bad, Level::User).expect_err("no such key");
    assert!(e.contains("[browser]"), "{e}");
}

/// The user level is the same file at a fixed path, read by the same call, so the two
/// levels cannot disagree about what a key means and the user level is not a second
/// grammar to learn.
#[test]
fn the_user_files_notes_keys_win_over_the_projects() {
    let u = project_dir("notes_user");
    let p = project_dir("notes_project");
    let user = LeticodeConfig::load_as(
        &write_file(&u, "[notes]\nrepo = \"~/notes\"\n"),
        Level::User,
    )
    .expect("user");
    let project = LeticodeConfig::load(&write_file(
        &p,
        "[notes]\nrepo = \"/elsewhere\"\nremote = \"https://example.invalid/n.git\"\n",
    ))
    .expect("project");
    let merged = user.overridden_by(&project);
    assert_eq!(merged.notes.repo.as_deref(), Some("~/notes"));
    assert_eq!(
        merged.notes.remote.as_deref(),
        Some("https://example.invalid/n.git"),
        "a key the user left unset falls through to the project's"
    );
    let bad = LeticodeConfig::load(&write_file(&p, "[notes]\ndir = \"x\"\n"));
    assert!(bad.unwrap_err().contains("[notes]"));
}

#[test]
fn the_user_file_is_the_same_parser_at_a_fixed_path() {
    let dir = project_dir("user_level");
    let path = write_file(&dir, "main_model = \"glm-5.3-flash\"\n");
    let user = LeticodeConfig::load_as(&path, Level::User).expect("the same reader");
    assert_eq!(user.main_model.as_deref(), Some("glm-5.3-flash"));
    assert_eq!(user.level, Level::User, "the level rides the value");
    assert_eq!(user.path.as_deref(), Some(path.as_path()));

    // The same bytes at the project level parse identically and say so.
    let project = LeticodeConfig::load(&path).expect("the project level");
    assert_eq!(project.main_model, user.main_model);
    assert_eq!(project.level, Level::Project);

    // A path with no file is `Ok` with nothing, at either level: the level
    // below, not an error.
    let absent = LeticodeConfig::load_as(&dir.join("not-here.toml"), Level::User)
        .expect("a missing file is not an error");
    assert!(absent.main_model.is_none());
    assert!(absent.path.is_none());
    assert!(absent.set_models().is_empty());

    // And the fixed path is beside `providers.toml`, not `$HOME` spelled a
    // second time: one file knows where the config dir is.
    assert_eq!(user_path().file_name().unwrap(), FILE_NAME);
    assert_eq!(
        user_path().parent(),
        letibot_provider::keys::config_file().parent()
    );
}

/// An unparsable user file is loud and does not take the daemon down: the reader
/// answers `Err` rather than a config that silently set nothing, and the caller turns
/// that into a sentence and a session that still starts.
#[test]
fn an_unparsable_user_file_names_the_file_and_the_line() {
    let dir = project_dir("user_unparsable");
    let path = write_file(
        &dir,
        "main_model = \"glm-5.3-flash\"\nthis line has no equals sign\n",
    );
    let why = LeticodeConfig::load_as(&path, Level::User)
        .expect_err("a file that does not parse is not a silent nothing");
    assert!(
        why.contains(&path.display().to_string()),
        "the refusal names the file: {why}"
    );
    assert!(
        why.contains("line 2"),
        "and the line the parser stopped on: {why}"
    );

    // And it does not take the daemon down: the caller turns this `Err` into a
    // sentence and runs on the empty config, which is a working config rather than a
    // half-read one. Asserted here because the caller's fallback is where it lives.
    let fell_back = LeticodeConfig::load_as(&path, Level::User).unwrap_or_default();
    assert!(fell_back.main_model.is_none());
    assert!(fell_back.roles.is_empty());
    assert!(
        fell_back.path.is_none(),
        "and it names no file it did not read"
    );
    assert!(fell_back.set_models().is_empty());
    // The floor under a spawn is still the operator's own numbers, so a refused
    // file does not silently hand children the daemon's greedy literal either.
    assert_eq!(
        fell_back.parameters_for(SUBAGENT_ROLE).temperature,
        Some(0.7)
    );
}

/// An unknown key at the user level is named too: `deny_unknown_fields` is the same
/// wall at both levels, so a user file is not the place a capability sneaks in just
/// because it is yours.
#[test]
fn an_unknown_key_in_the_user_file_is_named() {
    let dir = project_dir("user_unknown_key");
    for (name, text, wanted) in [
        ("typo", "subagent_modle = \"local\"\n", "subagent_modle"),
        ("capability", "allow_bash = true\n", "allow_bash"),
    ] {
        let path = dir.join(format!("{name}.toml"));
        std::fs::write(&path, text).unwrap();
        let why = LeticodeConfig::load_as(&path, Level::User)
            .expect_err("a key this version does not know is refused, not ignored");
        assert!(why.contains(wanted), "{wanted} must be named: {why}");
    }
}

/// The two file levels merge key by key, the project's over the user's, so
/// `subagent_model` in one file and `[roles.subagent]` in the other both land.
#[test]
fn the_project_level_overrides_the_user_level_key_by_key() {
    let dir = project_dir("user_merge");
    let user_path = dir.join("user.toml");
    std::fs::write(
        &user_path,
        "main_model = \"glm-5.3-flash\"\nsubagent_model = \"local\"\n\
             gatekeeper_model = \"qwen-3.8-27b\"\n\
             [roles.subagent]\ntemperature = 0.7\ntop_p = 0.8\n\
             [roles.coder]\ntemperature = 1.0\n",
    )
    .unwrap();
    let project_path = dir.join("project.toml");
    std::fs::write(
        &project_path,
        "main_model = \"deepseek/deepseek-chat\"\n\
             [roles.subagent]\ntemperature = 1.0\n",
    )
    .unwrap();
    let user = LeticodeConfig::load_as(&user_path, Level::User).unwrap();
    let project = LeticodeConfig::load(&project_path).unwrap();
    let merged = user.overridden_by(&project);

    assert_eq!(
        merged.main_model.as_deref(),
        Some("deepseek/deepseek-chat"),
        "the project's main_model wins"
    );
    assert_eq!(
        merged.subagent_model.as_deref(),
        Some("local"),
        "a key only the user file set survives the merge"
    );
    assert_eq!(merged.gatekeeper_model.as_deref(), Some("qwen-3.8-27b"));
    assert_eq!(merged.path.as_deref(), Some(project_path.as_path()));
    assert_eq!(merged.level, Level::Project);

    // Key by key inside a role: the project's temperature, the user's top_p.
    let sub = merged.parameters_for(SUBAGENT_ROLE);
    assert_eq!(sub.temperature, Some(1.0), "the project's number");
    assert_eq!(sub.top_p, Some(0.8), "the user's number, untouched");
    assert_eq!(
        sub.presence_penalty,
        Some(1.5),
        "and dense78 underneath both"
    );
    // A role only the user file knows is still there.
    assert_eq!(merged.role_model("coder"), None);
    assert_eq!(merged.parameters_for("coder").temperature, Some(1.0));

    // Nothing in either file is the empty merge.
    let empty = LeticodeConfig::default().overridden_by(&LeticodeConfig::default());
    assert!(empty.set_models().is_empty());
    assert!(empty.main_model.is_none());
}

/// A key the version does not know is named, not ignored: a reader that accepts a
/// typo'd `subagent_modle` and reads nothing reproduces the defect one line lower.
#[test]
fn an_unknown_key_is_named_not_ignored() {
    let dir = project_dir("unknown_key");
    let path = write_file(&dir, "subagent_modle = \"typo\"\n");
    let err = LeticodeConfig::load(&path).expect_err("an unknown key is Err");
    assert!(err.contains("subagent_modle"), "the typo is named: {err}");
    assert!(
        err.contains(FILE_NAME) || err.contains(&path.display().to_string()),
        "the file is named: {err}"
    );
}

/// The `[embeddings]` table is carried and not interpreted: it names where a vector comes
/// from, so refusing the file for carrying it would make the mode unwritable on every box
/// that declared one, and this reader seats no tool and sets no model from it.
#[test]
fn the_embeddings_table_is_carried_not_refused() {
    let dir = project_dir("embeddings_table");
    let path = write_file(
        &dir,
        "[embeddings]\nmode = \"internal\"\nmodel = \"/tmp/nowhere\"\n",
    );
    let cfg = LeticodeConfig::load(&path).expect("the section is carried");
    assert!(cfg.main_model.is_none(), "and sets no model at this level");
}

/// The `[bridge]` table is carried and not interpreted too: the permission relay's key is the
/// MCP bridge's own, and this reader refuses nothing it has no opinion about - except that a
/// `deny_unknown_fields` refusal here would make the daemon reject the very file that turns the
/// bridge's feature on.
#[test]
fn the_bridge_table_is_carried_not_refused() {
    let dir = project_dir("bridge_table");
    let path = write_file(&dir, "[bridge]\npermission_relay = true\n");
    let cfg = LeticodeConfig::load(&path).expect("the section is carried");
    assert!(cfg.main_model.is_none(), "and sets no model at this level");
}

/// A typo inside a role's table is named too, and that is the half serde cannot do:
/// `[roles.coder]` is read key by key, so a `temperatuer` that was accepted and read
/// as nothing would leave the operator no way to tell they were ignored.
#[test]
fn a_typo_inside_a_role_table_is_named_not_ignored() {
    let dir = project_dir("role_typo");
    let path = write_file(&dir, "[roles.coder]\ntemperatuer = 0.7\n");
    let err = LeticodeConfig::load(&path).expect_err("a typo in a role table is Err");
    assert!(err.contains("temperatuer"), "the typo is named: {err}");
    assert!(err.contains("coder"), "the role is named: {err}");
    assert!(
        err.contains("temperature"),
        "and the keys it could have meant are listed: {err}"
    );
}

/// A window or a reply cap in a role's table is refused by name, and the sentence says
/// why: a child's samplers are the operator's numbers but its context limit is not,
/// because that is measured on the box that answers it.
#[test]
fn a_window_or_a_reply_cap_in_a_role_table_is_refused_by_name() {
    let dir = project_dir("no_window");
    for key in ["window", "context_window", "max_tokens", "n_ctx"] {
        let path = write_file(&dir, &format!("[roles.subagent]\n{key} = 57344\n"));
        let err = LeticodeConfig::load(&path).expect_err("a window is refused");
        assert!(err.contains(key), "the key is named: {err}");
        assert!(
            err.contains("context limit") || err.contains("room"),
            "and the operator's own reason is given: {err}"
        );
        assert!(
            err.contains("temperature"),
            "and the keys this file does get to set are listed: {err}"
        );
    }
    // The refusal is about the ROLE's parameters, not a ban on the word:
    // `[model."dense78"]` in providers.toml still states its window, which is
    // where a window belongs.
    let ok = write_file(&dir, "subagent_model = \"local\"\n");
    assert!(LeticodeConfig::load(&ok).is_ok());
}

/// A file that will not parse says so and says where, and it does not take the session
/// down: the daemon runs on its own defaults and says so.
#[test]
fn an_unparseable_file_is_loud_and_does_not_take_the_session_down() {
    let dir = project_dir("unparseable");
    let path = write_file(&dir, "main_model = \n");
    let err = LeticodeConfig::load(&path).expect_err("an unparseable file is Err");
    assert!(
        err.contains(&path.display().to_string()),
        "the file is named: {err}"
    );
    // The parser's own message carries the line and what was expected.
    assert!(err.contains("line 1"), "the line is named: {err}");
    // And the absent-file case is the daemon's own defaults, not an error:
    // the session runs, it just runs on what the daemon has.
    let missing = dir.join("no-such-file.toml");
    let cfg = LeticodeConfig::load(&missing).expect("a missing file is Ok");
    assert_eq!(cfg, LeticodeConfig::default(), "absent is the defaults");
}

/// An absent file is simply the defaults: the daemon's own models and nothing to name.
/// A regression here would put a complaint on every start of every daemon with no
/// project file, which is most of them.
#[test]
fn an_absent_file_is_silently_the_defaults() {
    let dir = project_dir("absent");
    let missing = dir.join(FILE_NAME);
    assert!(!missing.exists());
    let cfg = LeticodeConfig::load(&missing).expect("a missing file is Ok");
    assert_eq!(cfg, LeticodeConfig::default());
    assert!(cfg.set_models().is_empty(), "nothing to disclose");
}

/// The file may set a model and may not widen a capability: an `allow_bash` or a
/// `role = "runner"` is a top-level key the version does not know, and
/// `deny_unknown_fields` refuses it by name.
#[test]
fn a_project_file_cannot_widen_a_capability() {
    let dir = project_dir("no_widen");
    // A capability key, not a model key.
    let path = write_file(&dir, "allow_bash = true\n");
    let err = LeticodeConfig::load(&path).expect_err("a capability key is refused");
    assert!(
        err.contains("allow_bash"),
        "the capability key is named: {err}"
    );
    // A seat key, the other direction of the same widening.
    let path = write_file(&dir, "role = \"runner\"\n");
    let err = LeticodeConfig::load(&path).expect_err("a seat key is refused");
    assert!(err.contains("role"), "the seat key is named: {err}");
}

/// The well-formed file gives the models it set and the parameters it set, so "where
/// did this model come from" is answerable from the screen. A file that sets nothing
/// discloses nothing, which is the absent-file case.
#[test]
fn a_well_formed_file_gives_its_models_and_the_disclosure_names_them() {
    let dir = project_dir("well_formed");
    let path = write_file(
        &dir,
        "main_model = \"deepseek/deepseek-chat\"\n\
             subagent_model = \"local\"\n\
             gatekeeper_model = \"qwen3-4b\"\n\
             judge_model = \"qwen3-4b\"\n\n\
             [roles]\n\
             reviewer = \"deepseek/deepseek-reasoner\"\n\n\
             [roles.subagent]\n\
             temperature = 0.7\n\
             top_p = 0.8\n\
             top_k = 20\n\
             min_p = 0\n\
             presence_penalty = 1.5\n",
    );
    let cfg = LeticodeConfig::load(&path).expect("a well-formed file is Ok");
    assert_eq!(cfg.main_model.as_deref(), Some("deepseek/deepseek-chat"));
    assert_eq!(cfg.subagent_model.as_deref(), Some("local"));
    assert_eq!(cfg.gatekeeper_model.as_deref(), Some("qwen3-4b"));
    assert_eq!(cfg.judge_model.as_deref(), Some("qwen3-4b"));
    assert_eq!(
        cfg.role_model("reviewer"),
        Some("deepseek/deepseek-reasoner")
    );
    assert_eq!(
        cfg.role_model("coder"),
        None,
        "a role the file did not name"
    );
    let set = cfg.set_models();
    assert!(
        set.iter()
            .any(|s| s.contains("main deepseek/deepseek-chat")),
        "{set:?}"
    );
    assert!(set.iter().any(|s| s.contains("subagent local")), "{set:?}");
    assert!(
        set.iter().any(|s| s.contains("gatekeeper qwen3-4b")),
        "{set:?}"
    );
    assert!(set.iter().any(|s| s.contains("judge qwen3-4b")), "{set:?}");
    assert!(
        set.iter()
            .any(|s| s.contains("reviewer deepseek/deepseek-reasoner")),
        "{set:?}"
    );
    assert!(
        set.iter()
            .any(|s| s.contains("temperature=0.7") && s.contains("presence_penalty=1.5")),
        "the parameters the file set are disclosed too: {set:?}"
    );
}

/// A role the file does not know is carried, not refused: `[roles]` is a named-role
/// override, so a future role needs a line and no new key. The refusal is for a key
/// the version does not know, which is a typo.
#[test]
fn a_role_the_file_does_not_know_is_carried_not_refused() {
    let dir = project_dir("future_role");
    let path = write_file(&dir, "[roles]\nsome_future_role = \"a-model\"\n");
    let cfg = LeticodeConfig::load(&path).expect("a future role is Ok");
    assert_eq!(cfg.role_model("some_future_role"), Some("a-model"));
}

/// The subagent runs on the operator's measured parameters and the file does not have
/// to say so, because a fix that only works in projects that knew about it is no fix.
/// The answer carries samplers and no window: a child's wall is measured where it runs.
#[test]
fn the_subagent_runs_on_the_parameters_measured_on_dense78() {
    // The absent-file case: the operator's numbers, stated in code.
    let cfg = LeticodeConfig::default();
    let p = cfg.parameters_for(SUBAGENT_ROLE);
    assert_eq!(p.temperature, Some(0.7));
    assert_eq!(p.top_p, Some(0.8));
    assert_eq!(p.top_k, Some(20));
    assert_eq!(p.min_p, Some(0.0));
    assert_eq!(p.presence_penalty, Some(1.5));

    // And as the call sees them: the five keys, in the operator's order, and
    // no key that describes the weights or the room.
    let sampling = p.to_sampling();
    let keys: Vec<&String> = sampling.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["temperature", "top_p", "top_k", "min_p", "presence_penalty"],
        "the five measured on dense78 and nothing else"
    );
    assert_eq!(sampling["temperature"], 0.7);
    assert_eq!(sampling["top_k"], 20);
    assert_eq!(sampling["presence_penalty"], 1.5);
    for absent in ["window", "context_window", "max_tokens", "seed"] {
        assert!(
            sampling.get(absent).is_none(),
            "`{absent}` is not a subagent parameter: {sampling}"
        );
    }
    assert!(
        !LeticodeConfig::default()
            .parameters_for(SUBAGENT_ROLE)
            .is_empty(),
        "a subagent is never handed an empty profile -- the daemon's greedy literal is what \
             makes it overthink"
    );
}

/// A role's table sets its own numbers over the operator's, key by key: `[roles.subagent]`
/// is every spawn and `[roles.coder]` is coder spawns. A file that changes `temperature`
/// alone must not lose `presence_penalty`, which is the key they named as the cure.
#[test]
fn a_role_table_sets_the_parameters_key_by_key() {
    let dir = project_dir("role_params");
    let path = write_file(
        &dir,
        "[roles.subagent]\ntemperature = 0.7\ntop_p = 0.8\ntop_k = 20\nmin_p = 0\n\
             presence_penalty = 1.5\n\n\
             [roles.coder]\ntemperature = 1.0\ntop_k = 40\n",
    );
    let cfg = LeticodeConfig::load(&path).expect("a role's parameters are Ok");

    // The subagent's own table: the operator's numbers, written down.
    let sub = cfg.parameters_for(SUBAGENT_ROLE);
    assert_eq!(sub.temperature, Some(0.7));
    assert_eq!(sub.presence_penalty, Some(1.5));

    // A coder spawn: its two numbers over the subagent's, the rest kept.
    let coder = cfg.parameters_for("coder");
    assert_eq!(coder.temperature, Some(1.0), "the seat's own temperature");
    assert_eq!(coder.top_k, Some(40), "and the seat's own top_k");
    assert_eq!(coder.top_p, Some(0.8), "a key it did not set is kept");
    assert_eq!(
        coder.presence_penalty,
        Some(1.5),
        "including the one the operator named as the cure"
    );

    // A project that changes ONE number keeps the rest: the same merge, one
    // level down, which is what makes the five a profile rather than five
    // unrelated knobs.
    let path = write_file(&dir, "[roles.subagent]\ntemperature = 0.2\n");
    let cfg = LeticodeConfig::load(&path).expect("one number is enough");
    let p = cfg.parameters_for(SUBAGENT_ROLE);
    assert_eq!(p.temperature, Some(0.2));
    assert_eq!(p.top_p, Some(0.8), "the operator's own dense78 top_p");
    assert_eq!(p.min_p, Some(0.0), "and a min_p SET to zero is not absent");
}

/// The model a spawn runs on, and where each name comes from: `subagent_model` is the
/// file's word for every child, a `[roles.X] model` is its word for one seat's children
/// and wins, and a file that set neither answers `None`.
#[test]
fn a_seat_model_wins_over_the_subagent_model_and_none_means_the_callers_own() {
    let dir = project_dir("spawn_model");
    let path = write_file(
        &dir,
        "subagent_model = \"local\"\n\n\
             [roles.researcher]\nmodel = \"deepseek/deepseek-reasoner\"\n",
    );
    let cfg = LeticodeConfig::load(&path).expect("both shapes are Ok");
    assert_eq!(
        cfg.spawn_model("researcher"),
        Some("deepseek/deepseek-reasoner")
    );
    assert_eq!(
        cfg.spawn_model("coder"),
        Some("local"),
        "a seat the file did not name falls to subagent_model"
    );
    assert_eq!(
        LeticodeConfig::default().spawn_model("coder"),
        None,
        "and a file that said nothing leaves `local` to the caller"
    );
}

/// One seat, two answers, refused by name: the guard and the adjudicator are the single
/// oracle in this build, so `gatekeeper_model` and `judge_model` are two words for one
/// setting. Two different models is the file disagreeing with itself about who guards.
#[test]
fn the_two_names_for_the_adjudicator_may_not_disagree() {
    let dir = project_dir("one_oracle");
    let path = write_file(
        &dir,
        "gatekeeper_model = \"qwen3-4b\"\njudge_model = \"glm-5.3-flash\"\n",
    );
    let err = LeticodeConfig::load(&path).expect_err("two models for one seat is Err");
    assert!(err.contains("gatekeeper_model"), "named: {err}");
    assert!(err.contains("judge_model"), "named: {err}");
    assert!(
        err.contains(&path.display().to_string()),
        "and the file: {err}"
    );

    // The same model twice, or either alone, is not a disagreement.
    let path = write_file(
        &dir,
        "gatekeeper_model = \"qwen3-4b\"\njudge_model = \"qwen3-4b\"\n",
    );
    assert!(LeticodeConfig::load(&path).is_ok());
    let path = write_file(&dir, "judge_model = \"glm-5.3-flash\"\n");
    let cfg = LeticodeConfig::load(&path).expect("one word is enough");
    assert_eq!(cfg.judge_model.as_deref(), Some("glm-5.3-flash"));
}

/// `[exec]` sets the threshold and the switch; absent is on at 60 s; a zero or unknown key
/// refuses the file.
#[test]
fn exec_keys_set_the_foreground_policy() {
    let dir = project_dir("exec_keys");
    let none = LeticodeConfig::default();
    let p = foreground_policy(&none, &dir, true);
    assert!(p.auto_background);
    assert_eq!(p.threshold, std::time::Duration::from_secs(60));
    write_file(
        &dir,
        "[exec]\nauto_background = false\nforeground_seconds = 7\n",
    );
    let p = foreground_policy(&none, &dir, true);
    assert!(!p.auto_background);
    assert_eq!(p.threshold, std::time::Duration::from_secs(7));
    assert!(foreground_policy(&none, &dir, false).auto_background);
    let path = write_file(&dir, "[exec]\nforeground_seconds = 0\n");
    assert!(LeticodeConfig::load(&path).is_err());
    let path = write_file(&dir, "[exec]\nauto_bg = true\n");
    assert!(LeticodeConfig::load(&path).unwrap_err().contains("[exec]"));
}

/// `[exec] job_dead_after_seconds` is the ONLY clock a background job has: absent is thirty
/// minutes, a project's number wins over the user's, and `0` turns the check off rather than
/// arming a window of nothing.
#[test]
fn the_dead_after_window_is_a_key_and_zero_turns_the_check_off() {
    let dir = project_dir("job_dead_after");
    let none = LeticodeConfig::default();
    assert_eq!(
        job_dead_after(&none, &dir, true),
        Some(DEFAULT_JOB_DEAD_AFTER)
    );
    assert_eq!(DEFAULT_JOB_DEAD_AFTER, Duration::from_secs(1800));
    write_file(&dir, "[exec]\njob_dead_after_seconds = 60\n");
    assert_eq!(
        job_dead_after(&none, &dir, true),
        Some(Duration::from_secs(60))
    );
    // Nothing said for the project is the level below.
    assert_eq!(
        job_dead_after(&none, &dir, false),
        Some(DEFAULT_JOB_DEAD_AFTER)
    );
    write_file(&dir, "[exec]\njob_dead_after_seconds = 0\n");
    assert_eq!(job_dead_after(&none, &dir, true), None, "zero turns it off");
    // And a misspelt key refuses the file by name rather than leaving a default in force.
    let path = write_file(&dir, "[exec]\njob_dead_after = 60\n");
    let why = LeticodeConfig::load(&path).unwrap_err();
    assert!(
        why.contains("[exec]") && why.contains("job_dead_after_seconds"),
        "{why}"
    );
}

/// The project's `[memory] default` beats the box-wide one for a session in it, and the
/// policy names the project file as the source, which is what the `bash` description
/// then says. A session elsewhere keeps the box's number.
#[test]
fn a_projects_memory_default_overrides_the_box_wide_one_and_says_so() {
    use letibot_tools::exec::memory::{Amount, MemoryPolicy, PolicySource};
    const GIB: u64 = 1 << 30;
    let dir = project_dir("memory-default");
    write_file(&dir, "[memory]\ndefault = \"4G\"\n");
    let sub = dir.join("crates/x");
    std::fs::create_dir_all(&sub).unwrap();
    let box_wide = MemoryPolicy {
        default: Amount::Bytes(64 << 20),
        default_from: PolicySource::Box,
        ..MemoryPolicy::default()
    };
    let none = LeticodeConfig::default();
    let p = memory_policy_at(box_wide, &none, &sub, true, Some(100 * GIB));
    assert_eq!(p.default, Amount::Bytes(4 * GIB));
    assert_eq!(p.default_from, PolicySource::Project);
    assert_eq!(p.ceiling, Amount::Fraction(0.5), "the ceiling was not set");
    // `--no-project-config`, and a workspace with no file: the box's number, named so.
    let off = memory_policy_at(box_wide, &none, &sub, false, Some(100 * GIB));
    assert_eq!(off, box_wide);
    // And the user level sits between the two: the project still wins.
    let user = LeticodeConfig {
        memory: MemoryKeys {
            default: Some(Amount::Bytes(GIB)),
            ceiling: None,
        },
        path: Some(PathBuf::from("/h/.config/letibot/leticode.toml")),
        level: Level::User,
        ..Default::default()
    };
    let u = memory_policy_at(box_wide, &user, &sub, false, Some(100 * GIB));
    assert_eq!(
        (u.default, u.default_from),
        (Amount::Bytes(GIB), PolicySource::User)
    );
    let both = memory_policy_at(box_wide, &user, &sub, true, Some(100 * GIB));
    assert_eq!(both.default_from, PolicySource::Project);
}

/// A file may narrow the ceiling and may not raise it, and its default may not pass
/// it: the ceiling is where the operator is asked, and the file is one a session can write.
#[test]
fn a_project_file_may_narrow_the_memory_ceiling_and_not_raise_it() {
    use letibot_tools::exec::memory::{Amount, MemoryPolicy};
    const GIB: u64 = 1 << 30;
    let none = LeticodeConfig::default();
    let dir = project_dir("memory-raise");
    write_file(&dir, "[memory]\nceiling = \"90%\"\ndefault = \"80G\"\n");
    let p = memory_policy_at(MemoryPolicy::default(), &none, &dir, true, Some(100 * GIB));
    assert_eq!(
        p,
        MemoryPolicy::default(),
        "neither the raise nor the default past it"
    );
    let dir = project_dir("memory-narrow");
    write_file(&dir, "[memory]\nceiling = \"8G\"\ndefault = \"2G\"\n");
    let p = memory_policy_at(MemoryPolicy::default(), &none, &dir, true, Some(100 * GIB));
    assert_eq!(p.ceiling, Amount::Bytes(8 * GIB));
    assert_eq!(p.default, Amount::Bytes(2 * GIB));
}

/// A `[memory]` key this build does not know, or a value that is not an amount, refuses
/// the file by name - the loud failure every other key here gets.
#[test]
fn a_bad_memory_table_is_refused_by_name() {
    let dir = project_dir("memory-bad");
    let path = write_file(&dir, "[memory]\ndefualt = \"4G\"\n");
    let err = LeticodeConfig::load(&path).unwrap_err();
    assert!(err.contains("defualt") && err.contains("[memory]"), "{err}");
    let path = write_file(&dir, "[memory]\ndefault = \"lots\"\n");
    let err = LeticodeConfig::load(&path).unwrap_err();
    assert!(
        err.contains("[memory] default") && err.contains("lots"),
        "{err}"
    );
}

/// `[skills] pin` is read in the order the file wrote it, and one key this build does
/// not know is refused by name.
#[test]
fn the_skills_table_is_read_and_its_one_key_is_named() {
    let dir = project_dir("skills");
    let path = write_file(
        &dir,
        "[skills]\npin = [\"latex-presentation\", \"release\"]\n",
    );
    let cfg = LeticodeConfig::load(&path).expect("one key parses");
    assert_eq!(
        cfg.skills.pin,
        ["latex-presentation", "release"],
        "the names, in the file's order"
    );
    assert!(
        LeticodeConfig::default().skills.is_empty(),
        "a file that says nothing pins nothing"
    );

    let path = write_file(&dir, "[skills]\nnever_offer = [\"x\", \"y\"]\n");
    let cfg = LeticodeConfig::load(&path).expect("never_offer parses");
    assert_eq!(cfg.skills.never_offer, ["x", "y"]);

    let path = write_file(&dir, "[skills]\npi = [\"x\"]\n");
    let err = LeticodeConfig::load(&path).unwrap_err();
    assert!(err.contains("`pi`") && err.contains("[skills]"), "{err}");
}

/// Both levels' pins apply: the user's names first, then the project's new ones, each
/// once. A pin asks for a skill, so a project cannot unsay the operator's list.
#[test]
fn the_two_levels_pin_skills_by_union() {
    let user_dir = project_dir("skills-user");
    let user_path = write_file(&user_dir, "[skills]\npin = [\"a\", \"b\"]\n");
    let user = LeticodeConfig::load_as(&user_path, Level::User).expect("the user file parses");
    let project_root = project_dir("skills-project");
    let project_path = write_file(&project_root, "[skills]\npin = [\"b\", \"c\"]\n");
    let project = LeticodeConfig::load(&project_path).expect("the project file parses");
    assert_eq!(
        user.overridden_by(&project).skills.pin,
        ["a", "b", "c"],
        "the union, each name once, the user's order first"
    );
}

/// `[skills.when.<name>]` is a table per skill, and a key this build does not know is refused by
/// name - the same loudness every other table in this file has.
#[test]
fn the_when_table_is_read_per_skill_and_its_keys_are_named() {
    let dir = project_dir("skills-when");
    let path = write_file(
        &dir,
        "[skills.when.instrument-dashboard]\nrequires = [\"flowy\"]\n\
         [skills.when.status-page]\nunless = [\"flowy\", \"bash\"]\n",
    );
    let cfg = LeticodeConfig::load(&path).expect("the tables parse");
    assert_eq!(cfg.skills.when["instrument-dashboard"].requires, ["flowy"]);
    assert!(
        cfg.skills.when["instrument-dashboard"].unless.is_empty(),
        "a key the table did not write is empty, not inherited"
    );
    assert_eq!(cfg.skills.when["status-page"].unless, ["flowy", "bash"]);
    assert!(cfg.skills.when["status-page"].requires.is_empty());
    assert!(
        LeticodeConfig::default().skills.is_empty(),
        "a file that names no skill needs nothing of any seat"
    );

    let path = write_file(
        &dir,
        "[skills.when.instrument-dashboard]\nneeds = [\"flowy\"]\n",
    );
    let err = LeticodeConfig::load(&path).unwrap_err();
    assert!(
        err.contains("needs") && err.contains("requires"),
        "the refusal names the key and the two there are: {err}"
    );
}

/// Both levels may say what one skill needs, and the operator's answer is the one in force, whole:
/// a project file can make a skill fit less, and cannot unhide what the operator hid.
#[test]
fn the_operator_wins_the_skill_both_levels_speak_about() {
    let user_dir = project_dir("skills-when-user");
    let user_path = write_file(
        &user_dir,
        "[skills.when.dashboard]\nrequires = [\"flowy\"]\n\
         [skills.when.both]\nrequires = [\"flowy\"]\nunless = []\n",
    );
    let user = LeticodeConfig::load_as(&user_path, Level::User).expect("the user file parses");
    let project_root = project_dir("skills-when-project");
    let project_path = write_file(
        &project_root,
        // The project tries to unhide what the operator gated, and gates one of its own.
        "[skills.when.dashboard]\nrequires = []\n\
         [skills.when.local]\nrequires = [\"bash\"]\n",
    );
    let project = LeticodeConfig::load(&project_path).expect("the project file parses");
    let merged = user.overridden_by(&project).skills.when;
    assert_eq!(
        merged["dashboard"].requires,
        ["flowy"],
        "the operator's table is in force, whole - the project cannot unhide it"
    );
    assert_eq!(
        merged["local"].requires,
        ["bash"],
        "and the project's own apply"
    );
    let named: Vec<&str> = merged.keys().map(String::as_str).collect();
    assert_eq!(
        named,
        ["both", "dashboard", "local"],
        "every table either level wrote is in force, and no other"
    );
    assert!(
        merged["both"].unless.is_empty(),
        "the operator's own table is kept as written"
    );
}

/// `[github] post` needs its token and nothing is posted without both, and a project file
/// may stop the seat and may not start it: a repository a session can write must not begin
/// posting under the operator's identity.
#[test]
fn the_github_table_reads_both_keys_and_a_project_file_cannot_start_posting() {
    let dir = project_dir("github");
    let path = write_file(
        &dir,
        "[github]\npost = true\ntoken_path = \"~/.config/letibot/github.token\"\n",
    );
    let cfg = LeticodeConfig::load(&path).expect("both keys parse");
    assert!(cfg.github.posting());
    assert_eq!(
        cfg.github.token_path.as_deref(),
        Some("~/.config/letibot/github.token")
    );
    // Neither key alone is posting.
    assert!(!GithubKeys::default().posting());
    assert!(
        !GithubKeys {
            post: Some(true),
            token_path: None
        }
        .posting()
    );
    assert!(
        !GithubKeys {
            post: None,
            token_path: Some("/t".into())
        }
        .posting()
    );

    // The user's yes, then a project's no: the seat stops. A project's yes alone is nothing.
    let user = GithubKeys {
        post: Some(true),
        token_path: Some("/t".into()),
    };
    let project_off = GithubKeys {
        post: Some(false),
        token_path: None,
    };
    assert!(!user.overridden_by(&project_off).posting());
    assert!(user.overridden_by(&GithubKeys::default()).posting());
    let project_on = GithubKeys {
        post: Some(true),
        token_path: Some("/other".into()),
    };
    assert!(!GithubKeys::default().overridden_by(&project_on).posting());

    // The two facts may not come from two levels: a project that names only a token has
    // started the seat the user merely allowed.
    let user_yes = GithubKeys {
        post: Some(true),
        token_path: None,
    };
    let project_token = GithubKeys {
        post: None,
        token_path: Some("/other".into()),
    };
    assert!(!user_yes.overridden_by(&project_token).posting());
    assert_eq!(
        user.overridden_by(&project_token).token_path.as_deref(),
        Some("/t"),
        "a project names no token, so it can neither add one nor replace the user's"
    );

    let path = write_file(&dir, "[github]\npostt = true\n");
    let err = LeticodeConfig::load(&path).unwrap_err();
    assert!(err.contains("postt") && err.contains("[github]"), "{err}");
}

/// `[boundary]` is read with the file and line of each entry, a project adds to the user's
/// and a misshapen key is refused with its line rather than defaulted.
#[test]
fn the_boundary_table_keeps_its_lines_and_a_project_adds_to_the_users() {
    let dir = project_dir("boundary");
    let path = write_file(
        &dir,
        "main_model = \"x\"\n[boundary]\ngrants_ro = [\"~/a\", \"~/b\"]\nhide = [\"~/c\"]\n\
             [boundary.env]\nRUST_LOG = \"info\"\n",
    );
    let user = LeticodeConfig::load_as(&path, Level::User).expect("parses");
    let ro: Vec<(&str, usize)> = user
        .boundary
        .grants_ro
        .iter()
        .map(|e| (e.value.as_str(), e.from.line))
        .collect();
    assert_eq!(ro, vec![("~/a", 3), ("~/b", 3)]);
    assert!(user.boundary.grants_ro[0].from.user);
    assert_eq!(user.boundary.env[0].value.0, "RUST_LOG");
    assert_eq!(user.boundary.env[0].from.line, 6);

    let dir = project_dir("boundary-project");
    let path = write_file(&dir, "[boundary]\ngrants_ro = [\"~/b\", \"~/d\"]\n");
    let project = LeticodeConfig::load(&path).expect("parses");
    assert!(!project.boundary.grants_ro[0].from.user);
    let merged = user.overridden_by(&project);
    let all: Vec<&str> = merged
        .boundary
        .grants_ro
        .iter()
        .map(|e| e.value.as_str())
        .collect();
    assert_eq!(all, vec!["~/a", "~/b", "~/d"]);
    assert!(LeticodeConfig::default().boundary.is_empty());
}

/// `firecode_only` is a project key: it narrows what a session runs, and the levels union.
#[test]
fn firecode_only_is_read_from_a_project_and_unions_with_the_users() {
    let dir = project_dir("boundary-fconly");
    let path = write_file(&dir, "[boundary]\nfirecode_only = [\"npm\", \"node\"]\n");
    let project = LeticodeConfig::load(&path).expect("a project may set it");
    assert_eq!(project.boundary.firecode_only[0].value, "npm");
    assert_eq!(project.boundary.firecode_only[1].from.line, 2);
    let dir = project_dir("boundary-fconly-user");
    let path = write_file(&dir, "[boundary]\nfirecode_only = [\"node\", \"yarn\"]\n");
    let user = LeticodeConfig::load_as(&path, Level::User).expect("parses");
    let merged = user.boundary.overridden_by(&project.boundary);
    let all: Vec<&str> = merged
        .firecode_only
        .iter()
        .map(|e| e.value.as_str())
        .collect();
    assert_eq!(all, vec!["node", "yarn", "npm"]);
    let bad = write_file(
        &project_dir("boundary-fconly-bad"),
        "[boundary]\nfirecode_only = [\"/bin/npm\"]\n",
    );
    let err = LeticodeConfig::load(&bad).expect_err("a path is not a program name");
    assert!(err.contains("firecode_only") && err.contains(":2"), "{err}");
}

/// A file that does not parse as a boundary is a refusal with file and line, never an
/// empty config: an empty one is today's wide default.
#[test]
fn a_bad_boundary_table_is_refused_with_its_line() {
    let dir = project_dir("boundary-bad");
    for (body, want) in [
        ("[boundary]\ngrants = [\"~/a\"]\n", "`grants` is not a key"),
        ("[boundary]\ngrants_ro = \"~/a\"\n", "grants_ro is a list"),
        ("[boundary]\nhide = [1]\n", "hide is a list"),
        ("[boundary]\nenv = { A = 1 }\n", "env.A is a string"),
        ("[boundary\nhide = []\n", "line 1"),
    ] {
        let path = write_file(&dir, body);
        let err = LeticodeConfig::load(&path).unwrap_err();
        assert!(err.contains(want), "{body:?}: {err}");
        assert!(err.contains(&path.display().to_string()), "{err}");
    }
    let path = write_file(&dir, "[boundary]\nhide = []\ngrants = []\n");
    let err = LeticodeConfig::load(&path).unwrap_err();
    assert!(err.contains(":3:"), "{err}");
}

/// `[perimeter.<name>]` is read at both levels with each entry's file and line, the project's
/// table of one name adding to the user's; the three ways an env value is computed survive.
#[test]
fn the_perimeter_tables_keep_their_lines_and_a_project_adds_per_name() {
    let dir = project_dir("perimeter");
    let user_path = dir.join("user.toml");
    std::fs::write(
        &user_path,
        "[perimeter.subagent]\nhide = [\"~/models\"]\nsecrets = [\"github\"]\n\
             [perimeter.subagent.env]\nRUST_LOG = \"warn\"\nTOKEN = { file = \"~/tok\" }\n\
             STAMP = { command = \"date +%F\" }\n\
             [perimeter.reviewer]\ngrants_ro = [\"~/reviews\"]\n",
    )
    .unwrap();
    let user = LeticodeConfig::load_as(&user_path, Level::User).unwrap();
    let sub = user.perimeters.get("subagent").expect("the subagent table");
    assert_eq!(sub.hide[0].value, "~/models");
    assert!(sub.hide[0].from.user);
    assert!(sub.hide[0].from.line >= 2);
    assert_eq!(sub.secrets[0].value, Secret::Github);
    assert!(sub.env.iter().any(
        |e| e.value.0 == "TOKEN" && matches!(e.value.1, EnvValue::File(ref f) if f == "~/tok")
    ));
    assert!(sub.env.iter().any(|e| e.value.0 == "STAMP"
        && matches!(e.value.1, EnvValue::Command(ref c) if c == "date +%F")));
    assert!(sub.env.iter().any(|e| e.value.0 == "RUST_LOG"
        && matches!(e.value.1, EnvValue::Constant(ref v) if v == "warn")));
    assert_eq!(user.perimeters["reviewer"].grants_ro[0].value, "~/reviews");

    // The project's table of one name adds to the user's, and a name only the user file has
    // survives the merge untouched.
    let project_path = write_file(&dir, "[perimeter.subagent]\nhide = [\"/opt/x\"]\n");
    let project = LeticodeConfig::load(&project_path).unwrap();
    assert!(!project.perimeters["subagent"].hide[0].from.user);
    let merged = user.overridden_by(&project);
    let hides: Vec<&str> = merged.perimeters["subagent"]
        .hide
        .iter()
        .map(|e| e.value.as_str())
        .collect();
    assert!(
        hides.contains(&"~/models") && hides.contains(&"/opt/x"),
        "{hides:?}"
    );
    assert!(merged.perimeters.contains_key("reviewer"));
    assert!(LeticodeConfig::default().perimeters.is_empty());
}

/// A perimeter table that is misshapen is refused with its file and line, naming the key -
/// never silently read as the empty table, which would be a view nobody asked for.
#[test]
fn a_bad_perimeter_table_is_refused_with_its_line() {
    let dir = project_dir("perimeter-bad");
    for (body, want) in [
        (
            "[perimeter]\nsubagent = 3\n",
            "[perimeter.subagent] is a table",
        ),
        ("[perimeter.subagent]\nsize = 3\n", "`size` is not a key"),
        ("[perimeter.subagent]\nhide = \"~/a\"\n", "hide is a list"),
        (
            "[perimeter.subagent]\nsecrets = [\"ssh\"]\n",
            "not a secret this build knows",
        ),
        (
            "[perimeter.subagent]\n[perimeter.subagent.env]\nA = { file = 3 }\n",
            "env.A is a string",
        ),
        (
            "[perimeter.subagent]\n[perimeter.subagent.env]\nA = { file = \"x\", command = \"y\" }\n",
            "env.A is a string",
        ),
    ] {
        let path = write_file(&dir, body);
        let err = LeticodeConfig::load(&path).unwrap_err();
        assert!(err.contains(want), "{body:?}: {err}");
        assert!(err.contains(&path.display().to_string()), "{err}");
    }
}

/// A fallback list is replaced, not grown. A project that names two models must not
/// silently inherit a third from the user file: the list is one decision, not a set.
#[test]
fn a_project_fallback_list_replaces_the_users_rather_than_growing_it() {
    let dir = project_dir("fallback_merge");
    let user_path = dir.join("user.toml");
    std::fs::write(
        &user_path,
        "fallback = [\"local\", \"qwen-3.8-27b\"]\nfallback_after_timeouts = 5\n",
    )
    .unwrap();
    let project_path = dir.join("project.toml");
    std::fs::write(&project_path, "fallback = [\"deepseek\"]\n").unwrap();
    let user = LeticodeConfig::load_as(&user_path, Level::User).unwrap();
    let project = LeticodeConfig::load(&project_path).unwrap();
    let merged = user.overridden_by(&project);
    assert_eq!(
        merged.fallback,
        ["deepseek"],
        "the project's list replaced the user's whole list: {:?}",
        merged.fallback
    );
    assert_eq!(
        merged.fallback_after_timeouts,
        Some(5),
        "a key only the user file set survives the merge"
    );

    // A project that says nothing about a fallback keeps the user's list, and one
    // that names none is no fallback at all rather than the level above.
    let silent = dir.join("silent.toml");
    std::fs::write(&silent, "main_model = \"local\"\n").unwrap();
    let merged = user.overridden_by(&LeticodeConfig::load(&silent).unwrap());
    assert_eq!(merged.fallback, ["local", "qwen-3.8-27b"]);
    assert!(LeticodeConfig::default().fallback.is_empty());
    assert_eq!(LeticodeConfig::default().fallback_after_timeouts, None);
}

/// A budget of zero is refused by name: a move on the first timeout is `1`, and a file
/// that wrote `0` said something it did not mean.
#[test]
fn a_zero_timeout_budget_is_refused_by_name() {
    let dir = project_dir("fallback_zero");
    let path = write_file(&dir, "fallback_after_timeouts = 0\n");
    let err = LeticodeConfig::load(&path).expect_err("zero is refused");
    assert!(
        err.contains("fallback_after_timeouts"),
        "the key is named: {err}"
    );
    assert!(err.contains("`1`"), "and the number it meant: {err}");
    assert!(
        err.contains(&path.display().to_string()),
        "and the file: {err}"
    );
}

/// A fallback that is not a list of model names is refused by name with the file: a
/// string, a number in the list and an empty name are three different mistakes.
#[test]
fn a_fallback_that_is_not_a_list_of_model_names_is_refused_by_name() {
    let dir = project_dir("fallback_bad");
    for (text, wanted) in [
        ("fallback = \"deepseek\"\n", "not a list"),
        ("fallback = [\"deepseek\", 3]\n", "a whole number"),
        ("fallback = [\"\"]\n", "an empty name"),
    ] {
        let path = write_file(&dir, text);
        let err = LeticodeConfig::load(&path).expect_err("a bad list is Err");
        assert!(err.contains("fallback"), "the key is named: {err}");
        assert!(err.contains(wanted), "{wanted} must be said: {err}");
        assert!(
            err.contains(&path.display().to_string()),
            "and the file: {err}"
        );
    }
}

/// A budget that is not a whole number of timeouts is refused by name, so a negative or
/// a word never reaches the move as some number nobody wrote.
#[test]
fn a_timeout_budget_that_is_not_a_whole_number_is_refused_by_name() {
    let dir = project_dir("fallback_budget_bad");
    for (text, wanted) in [
        ("fallback_after_timeouts = -3\n", "-3"),
        ("fallback_after_timeouts = \"three\"\n", "a string"),
        ("fallback_after_timeouts = 3.5\n", "a number"),
    ] {
        let path = write_file(&dir, text);
        let err = LeticodeConfig::load(&path).expect_err("a bad budget is Err");
        assert!(
            err.contains("fallback_after_timeouts"),
            "the key is named: {err}"
        );
        assert!(err.contains(wanted), "{wanted} must be said: {err}");
    }
}

/// The fallback names are models the file set, so the disclosure says them in its own
/// style: where the model that answers after a timeout came from must be answerable.
#[test]
fn the_fallback_models_are_disclosed_as_fallback_lines() {
    let dir = project_dir("fallback_disclosure");
    let path = write_file(
        &dir,
        "fallback = [\"deepseek\", \"glm-coding/glm-5.3\"]\nfallback_after_timeouts = 3\n",
    );
    let cfg = LeticodeConfig::load(&path).expect("a fallback list is Ok");
    assert_eq!(cfg.fallback, ["deepseek", "glm-coding/glm-5.3"]);
    assert_eq!(cfg.fallback_after_timeouts, Some(3));
    let set = cfg.set_models();
    assert_eq!(
        set,
        ["fallback deepseek", "fallback glm-coding/glm-5.3"],
        "the list, in the order the file wrote it: {set:?}"
    );
    assert!(LeticodeConfig::default().set_models().is_empty());
}

/// The operator's ruling, in the file: what a session does when its provider puts a time on a
/// limit. `switch` when a fallback list exists, `wait` when nothing is set, and the file's own
/// word over both.
#[test]
fn the_provider_table_says_what_a_limit_does_and_its_defaults_are_the_rulings() {
    let dir = project_dir("provider_on_limit");
    let written = write_file(
        &dir,
        "[provider]\non_limit = \"switch\"\nfallback = [\"deepseek\", \"glm-coding/glm-5.3\"]\n",
    );
    let cfg = LeticodeConfig::load(&written).expect("[provider] is read");
    assert_eq!(cfg.provider.on_limit, Some(OnLimit::Switch));
    assert_eq!(
        cfg.fallback,
        ["deepseek", "glm-coding/glm-5.3"],
        "[provider] fallback is the one list, so the daemon's fallback reads it"
    );
    assert_eq!(cfg.limit_policy(), OnLimit::Switch);
    assert!(
        cfg.set_models().contains(&"on_limit switch".to_string()),
        "the disclosure says what the file decided: {:?}",
        cfg.set_models()
    );

    // The two defaults, and the file's word beats both.
    assert_eq!(LeticodeConfig::default().limit_policy(), OnLimit::Wait);
    let list_only = write_file(&sub(&dir, "list"), "fallback = [\"deepseek\"]\n");
    let cfg = LeticodeConfig::load(&list_only).unwrap();
    assert_eq!(cfg.limit_policy(), OnLimit::Switch, "a list means switch");
    let wait = write_file(
        &sub(&dir, "wait"),
        "[provider]\non_limit = \"wait\"\nfallback = [\"deepseek\"]\n",
    );
    assert_eq!(
        LeticodeConfig::load(&wait).unwrap().limit_policy(),
        OnLimit::Wait,
        "a file that says wait keeps the list and waits"
    );
}

/// A `[provider]` table is refused by name when it is malformed, and the two spellings of one
/// list are refused when they disagree.
#[test]
fn a_bad_provider_table_and_two_disagreeing_fallbacks_are_refused_by_name() {
    let dir = project_dir("provider_refusals");
    let bad = write_file(&sub(&dir, "bad"), "[provider]\non_limit = \"sleep\"\n");
    let err = LeticodeConfig::load(&bad).expect_err("sleep is not an answer");
    assert!(err.contains("on_limit"), "{err}");
    assert!(err.contains("wait"), "the two answers are named: {err}");

    let unknown = write_file(&sub(&dir, "unknown"), "[provider]\nfall_back = []\n");
    let err = LeticodeConfig::load(&unknown).expect_err("an unknown key");
    assert!(err.contains("provider"), "{err}");

    let two = write_file(
        &sub(&dir, "two"),
        "fallback = [\"deepseek\"]\n[provider]\nfallback = [\"glm\"]\n",
    );
    let err = LeticodeConfig::load(&two).expect_err("one list, two answers");
    assert!(err.contains("`[provider] fallback`"), "{err}");
    let same = write_file(
        &sub(&dir, "same"),
        "fallback = [\"deepseek\"]\n[provider]\nfallback = [\"deepseek\"]\n",
    );
    assert_eq!(
        LeticodeConfig::load(&same).unwrap().fallback,
        ["deepseek"],
        "the same list twice is one list"
    );
}

/// `wait_if_within` is a span next to `on_limit`: 45m when absent, `0` allowed, a bare number or
/// a stranger refused with its file and line, and the project's span beats the user's.
#[test]
fn wait_if_within_is_a_span_with_a_default_and_refusals_by_line() {
    let m = |n: u64| std::time::Duration::from_secs(n * 60);
    assert_eq!(LeticodeConfig::default().wait_if_within(), m(45));
    assert_eq!(DEFAULT_WAIT_IF_WITHIN, m(45));

    let dir = project_dir("wait_if_within");
    let set = write_file(
        &sub(&dir, "set"),
        "[provider]\non_limit = \"switch\"\nwait_if_within = \"1h30m\"\n",
    );
    let cfg = LeticodeConfig::load(&set).unwrap();
    assert_eq!(cfg.wait_if_within(), m(90));
    assert!(
        cfg.set_models()
            .contains(&"wait_if_within 1h30m".to_string()),
        "{:?}",
        cfg.set_models()
    );
    let zero = write_file(&sub(&dir, "zero"), "[provider]\nwait_if_within = \"0\"\n");
    assert_eq!(
        LeticodeConfig::load(&zero).unwrap().wait_if_within(),
        std::time::Duration::ZERO
    );

    for (tag, body) in [
        ("bare", "\n[provider]\nwait_if_within = \"45\"\n"),
        ("unit", "\n[provider]\nwait_if_within = \"45 minutes\"\n"),
        ("empty", "\n[provider]\nwait_if_within = \"\"\n"),
        ("int", "\n[provider]\nwait_if_within = 45\n"),
    ] {
        let path = write_file(&sub(&dir, tag), body);
        let err = LeticodeConfig::load(&path).expect_err(tag);
        assert!(err.contains("leticode.toml:3:"), "{tag}: {err}");
        assert!(err.contains("wait_if_within"), "{tag}: {err}");
    }
    let preset = write_file(
        &sub(&dir, "preset"),
        "[provider.glm]\nwait_if_within = \"5m\"\n",
    );
    assert!(
        LeticodeConfig::load(&preset).is_err(),
        "a [provider] key only"
    );

    assert_eq!(
        parse_span("2d3h"),
        Ok(std::time::Duration::from_secs(2 * 86_400 + 3 * 3600))
    );
    assert_eq!(parse_span(" 90s "), Ok(std::time::Duration::from_secs(90)));
    assert!(parse_span("m").is_err() && parse_span("5x").is_err() && parse_span("5m3").is_err());
    assert_eq!(span_words(m(45)), "45m");
    assert_eq!(span_words(m(90)), "1h30m");
    assert_eq!(span_words(std::time::Duration::ZERO), "0");

    // The project's span wins over the user's, and an absent one says nothing.
    let user = LeticodeConfig::load(&set).unwrap();
    let project = LeticodeConfig::load(&zero).unwrap();
    assert_eq!(
        user.overridden_by(&project).wait_if_within(),
        std::time::Duration::ZERO
    );
    assert_eq!(
        user.overridden_by(&LeticodeConfig::default())
            .wait_if_within(),
        m(90)
    );
}

/// The deadline keys parse, resolve model over preset over table over built-in, per key, and
/// the project's key beats the user's.
#[test]
fn provider_deadlines_resolve_by_model_then_preset_then_table_then_builtin() {
    let dir = project_dir("deadlines");
    let none = LeticodeConfig::default().deadlines("glm", Some("glm-5.3"));
    assert_eq!(
        (
            none.slow_notice.as_secs(),
            none.first_byte.as_secs(),
            none.answer.as_secs()
        ),
        (15, 60, 600)
    );
    let user = write_file(
        &sub(&dir, "user"),
        "[provider]\nslow_notice_seconds = 20\nanswer_seconds = 900\n\
             [provider.glm]\nslow_notice_seconds = 90\nfirst_byte_seconds = 100\n\
             [provider.\"glm/glm-5.3\"]\nfirst_byte_seconds = 120\n",
    );
    let project = write_file(
        &sub(&dir, "project"),
        "[provider.glm]\nslow_notice_seconds = 45\n",
    );
    let merged = LeticodeConfig::load_as(&user, Level::User)
        .unwrap()
        .overridden_by(&LeticodeConfig::load(&project).unwrap());
    let d = |name, model| {
        let d = merged.deadlines(name, model);
        (
            d.slow_notice.as_secs(),
            d.first_byte.as_secs(),
            d.answer.as_secs(),
        )
    };
    assert_eq!(
        d("glm", Some("glm-5.3")),
        (45, 120, 900),
        "model, preset, table"
    );
    assert_eq!(d("glm", Some("other")), (45, 100, 900), "the preset only");
    assert_eq!(
        d("deepseek", None),
        (20, 60, 900),
        "the table, then built-in"
    );
}

/// Nonsense deadlines are refused naming file, line and key.
#[test]
fn nonsense_deadlines_are_refused_with_file_line_and_key() {
    let dir = project_dir("deadline_refusals");
    for (tag, body, want, line) in [
        (
            "zero",
            "\n[provider]\nslow_notice_seconds = 0\n",
            "slow_notice_seconds",
            3,
        ),
        (
            "crossed",
            "\n[provider]\nfirst_byte_seconds = 90\nanswer_seconds = 30\n",
            "first_byte_seconds",
            3,
        ),
        (
            "preset_zero",
            "\n[provider.glm]\nanswer_seconds = 0\n",
            "[provider.glm] answer_seconds",
            3,
        ),
        (
            "preset_key",
            "\n[provider.glm]\non_limit = \"wait\"\n",
            "on_limit",
            3,
        ),
    ] {
        let path = write_file(&sub(&dir, tag), body);
        let err = LeticodeConfig::load(&path).expect_err(tag);
        assert!(
            err.contains(&format!("leticode.toml:{line}:")),
            "{tag}: {err}"
        );
        assert!(err.contains(want), "{tag}: {err}");
    }
}

/// `[ui] rooms_sidebar` is read, merged (the project over the user) and refused by name when
/// it is not a boolean or when the table holds another key.
#[test]
fn the_ui_table_reads_rooms_sidebar() {
    let dir = project_dir("ui_table");
    let on = write_file(&sub(&dir, "ui-on"), "[ui]\nrooms_sidebar = true\n");
    assert_eq!(
        LeticodeConfig::load(&on).unwrap().ui.rooms_sidebar,
        Some(true)
    );
    let none = write_file(&sub(&dir, "ui-none"), "");
    assert_eq!(LeticodeConfig::load(&none).unwrap().ui.rooms_sidebar, None);
    let bad = write_file(&sub(&dir, "ui-bad"), "[ui]\nrooms_sidebar = \"yes\"\n");
    let err = LeticodeConfig::load(&bad).unwrap_err();
    assert!(err.contains("rooms_sidebar"), "{err}");
    let wide = write_file(&sub(&dir, "ui-width"), "[ui]\nrooms_sidebar_width = 44\n");
    assert_eq!(
        LeticodeConfig::load(&wide).unwrap().ui.rooms_sidebar_width,
        Some(44)
    );
    let zero = write_file(&sub(&dir, "ui-zero"), "[ui]\nrooms_sidebar_width = 0\n");
    assert!(LeticodeConfig::load(&zero).is_err());
    let other = write_file(&sub(&dir, "ui-key"), "[ui]\nwidth = 3\n");
    let err = LeticodeConfig::load(&other).unwrap_err();
    assert!(err.contains("not a key"), "{err}");
}

// ---- K9: config, not a plugin (the plan's §4.3, §6 K9) ----

/// The six tables read, every key, and a file that says nothing is the daemon that shipped:
/// no `[nag]`/`[queue]` number at all, which is `NagTimings::default` and the queue's own
/// constants. This is the "byte-identical at the defaults" half of K9's target.
#[test]
fn the_six_tables_are_read_and_an_empty_file_is_the_shipped_daemon() {
    let dir = project_dir("k9_read");
    let empty = write_file(&sub(&dir, "empty"), "main_model = \"local\"\n");
    let none = LeticodeConfig::load(&empty).unwrap();
    assert_eq!(none.nag, NagKeys::default());
    assert_eq!(none.queue, QueueKeys::default());
    assert_eq!(none.room, RoomKeys::default());
    assert!(none.warnings.mute.is_empty());
    assert!(none.keys.rebind.is_empty());
    assert!(none.verbosity.profiles.is_empty());
    assert_eq!(
        none.nag_timings(),
        letibot_tools::builtins::todo::NagTimings::default(),
        "no key is the clock every daemon already had"
    );
    assert_eq!(none.stuck_after_seconds(), None);

    let full = write_file(
        &sub(&dir, "full"),
        "[queue]\nstuck_after_seconds = 900\nstuck_repeat_seconds = 7200\n\
             [nag]\nafter_seconds = 60\nfloor_seconds = 10\nrepeat_cap_seconds = 1800\n\
             [room]\ntie_breaker = \"glm-coding/glm-5.3\"\n\
             [warnings]\nmute = [\"model_slow_first_byte\", \"denied\"]\n\
             [keys]\n\"todos.d\" = \"f\"\n\
             [verbosity.mine]\nshow = [\"edits\", \"thinking=folded\"]\n",
    );
    let c = LeticodeConfig::load(&full).unwrap();
    assert_eq!(c.queue.stuck_after_seconds, Some(900));
    assert_eq!(c.queue.stuck_repeat_seconds, Some(7200));
    assert_eq!(c.nag.after_seconds, Some(60));
    assert_eq!(c.nag.floor_seconds, Some(10));
    assert_eq!(c.nag.repeat_cap_seconds, Some(1800));
    assert_eq!(c.room.tie_breaker.as_deref(), Some("glm-coding/glm-5.3"));
    assert_eq!(
        c.warnings
            .mute
            .iter()
            .map(|e| e.value.as_str())
            .collect::<Vec<_>>(),
        ["model_slow_first_byte", "denied"]
    );
    assert_eq!(c.keys.rebind.len(), 1);
    assert_eq!(c.keys.rebind[0].name, "todos.d");
    assert_eq!(c.keys.rebind[0].key, "f");
    assert_eq!(
        c.verbosity.profiles[0].name, "mine",
        "a profile is its name and the list of what it shows"
    );
    assert_eq!(c.verbosity.profiles[0].show, ["edits", "thinking=folded"]);

    // The clock's four, as the file and the shipped default make them: `[nag]`'s three and
    // the census's repeat from `[queue]`.
    let t = c.nag_timings();
    assert_eq!(t.window, std::time::Duration::from_secs(60));
    assert_eq!(t.floor, std::time::Duration::from_secs(10));
    assert_eq!(t.cap, std::time::Duration::from_secs(1800));
    assert_eq!(t.census_repeat, std::time::Duration::from_secs(7200));
    assert_eq!(c.stuck_after_seconds(), Some(900));
}

/// A bad value says its FILE AND LINE, table by table - the operator's target for K9, and the
/// reason these six tables are walked by hand instead of by serde, which names the table.
#[test]
fn a_bad_value_in_every_table_is_refused_with_its_file_and_line() {
    let dir = project_dir("k9_lines");
    // The line a refusal names is the one the value was WRITTEN on: a table's key is on the
    // file's third line here, and a `[verbosity.<name>]` profile's own line is the second -
    // its `show` list is refused against the profile, which is what the operator edits.
    for (tag, body, want, line) in [
        (
            "nag",
            "\n[nag]\nafter_seconds = \"60\"\n",
            "[nag] after_seconds",
            3,
        ),
        (
            "nagzero",
            "\n[nag]\nfloor_seconds = 0\n",
            "at least one second",
            3,
        ),
        (
            "queue",
            "\n[queue]\nstuck_after_seconds = -2\n",
            "[queue] stuck_after_seconds",
            3,
        ),
        ("qmute", "\n[queue]\nmute = \"a\"\n", "mute is a list", 3),
        (
            "qmute_entry",
            "\n[queue]\nmute = [\"a\", 3]\n",
            "mute holds a whole number",
            3,
        ),
        (
            "queue_kind",
            "\n[queue]\nstuck_repeat_seconds = true\n",
            "[queue]",
            3,
        ),
        (
            "warnings_key",
            "\n[warnings]\ncoalesce = \"yes\"\n",
            "`coalesce` is not a key",
            3,
        ),
        ("mute", "\n[warnings]\nmute = \"a\"\n", "mute is a list", 3),
        ("keys", "\n[keys]\nd = \"f\"\n", "names no surface", 3),
        (
            "keys_value",
            "\n[keys]\n\"todos.d\" = 3\n",
            "`todos.d` is one key",
            3,
        ),
        (
            "profile",
            "\n[verbosity.mine]\nshow = \"tools\"\n",
            "show is a list",
            2,
        ),
        (
            "profile_key",
            "\n[verbosity.mine]\nhide = []\n",
            "`hide` is not a key",
            2,
        ),
        (
            "room",
            "\n[room]\ntie_breaker = 3\n",
            "tie_breaker is a model name",
            3,
        ),
        (
            "room_key",
            "\n[room]\nmodel = \"x\"\n",
            "`model` is not a key",
            3,
        ),
    ] {
        let path = write_file(&sub(&dir, tag), body);
        let err = LeticodeConfig::load(&path).expect_err(tag);
        assert!(
            err.contains(&format!("leticode.toml:{line}:")),
            "{tag}: the refusal names the file and the line: {err}"
        );
        assert!(err.contains(want), "{tag}: {err}");
    }
}

/// `[queue] mute` reads from both levels and unions, each name once.
#[test]
fn a_queue_mute_unions_across_the_levels() {
    let dir = project_dir("qmute_levels");
    let user = write_file(
        &sub(&dir, "user"),
        "[queue]\nmute = [\"stroppy-rust\", \"x\"]\n",
    );
    let project = write_file(
        &sub(&dir, "project"),
        "[queue]\nmute = [\"x\", \"stroppy-cloud\"]\nstuck_after_seconds = 60\n",
    );
    let merged = LeticodeConfig::load_as(&user, Level::User)
        .unwrap()
        .overridden_by(&LeticodeConfig::load(&project).unwrap());
    assert_eq!(
        merged.queue_mute(),
        ["stroppy-rust", "x", "stroppy-cloud"],
        "both levels, the user's first, each once"
    );
    assert_eq!(merged.stuck_after_seconds(), Some(60));
}

/// A floor above the ceiling is a ladder with no room in it, refused by name.
#[test]
fn a_nag_floor_above_the_repeat_cap_is_refused() {
    let dir = project_dir("k9_floor");
    let bad = write_file(
        &sub(&dir, "crossed"),
        "[nag]\nfloor_seconds = 3600\nrepeat_cap_seconds = 60\n",
    );
    let err = LeticodeConfig::load(&bad).expect_err("the two ends crossed");
    assert!(err.contains("floor_seconds"), "{err}");
    assert!(err.contains("repeat_cap_seconds"), "{err}");
}

/// The project's value wins over the user's, key by key - the target's third clause - and the
/// two suppression lists (`mute`, and a profile of the same name) resolve as they should: a
/// mute unions, because a project must not unsay what the operator muted for every project.
#[test]
fn the_projects_value_wins_and_a_mute_unions_across_the_levels() {
    let dir = project_dir("k9_levels");
    let user = write_file(
        &sub(&dir, "user"),
        "[nag]\nafter_seconds = 90\nrepeat_cap_seconds = 900\n\
             [queue]\nstuck_after_seconds = 600\n\
             [room]\ntie_breaker = \"deepseek/deepseek-flash\"\n\
             [warnings]\nmute = [\"model_slow_first_byte\"]\n\
             [keys]\n\"todos.d\" = \"f\"\n\"rooms.r\" = \"u\"\n\
             [verbosity.mine]\nshow = [\"tools\"]\n",
    );
    let project = write_file(
        &sub(&dir, "project"),
        "[nag]\nafter_seconds = 30\n\
             [queue]\nstuck_after_seconds = 120\n\
             [room]\ntie_breaker = \"glm-coding/glm-5.3\"\n\
             [warnings]\nmute = [\"denied\"]\n\
             [keys]\n\"todos.d\" = \"x\"\n\
             [verbosity.mine]\nshow = [\"edits\", \"tools\"]\n",
    );
    let merged = LeticodeConfig::load_as(&user, Level::User)
        .unwrap()
        .overridden_by(&LeticodeConfig::load(&project).unwrap());
    assert_eq!(
        merged.nag.after_seconds,
        Some(30),
        "the project's window, not the user's"
    );
    assert_eq!(
        merged.nag.repeat_cap_seconds,
        Some(900),
        "and the user's cap stays where the project said nothing"
    );
    assert_eq!(merged.queue.stuck_after_seconds, Some(120));
    assert_eq!(
        merged.room.tie_breaker.as_deref(),
        Some("glm-coding/glm-5.3")
    );
    assert_eq!(
        merged
            .warnings
            .mute
            .iter()
            .map(|e| e.value.as_str())
            .collect::<Vec<_>>(),
        ["model_slow_first_byte", "denied"],
        "both levels' mutes, the user's first"
    );
    assert_eq!(merged.keys.rebind.len(), 2, "one replaced, one kept");
    assert_eq!(
        merged
            .keys
            .rebind
            .iter()
            .find(|r| r.name == "todos.d")
            .map(|r| r.key.as_str()),
        Some("x"),
        "the project's rebinding of one key replaces the user's"
    );
    assert_eq!(
        merged
            .verbosity
            .profiles
            .iter()
            .find(|p| p.name == "mine")
            .map(|p| p.show.clone()),
        Some(vec!["edits".to_string(), "tools".to_string()]),
        "a project's profile of the same name replaces the user's"
    );
    // And the mute keeps the LINE it came from, because the head refuses an unknown code there.
    let denied = merged
        .warnings
        .mute
        .iter()
        .find(|e| e.value == "denied")
        .expect("the project's mute");
    assert_eq!(denied.from.file, project);
    assert_eq!(denied.from.line, 8, "the line the code is written on");
}
