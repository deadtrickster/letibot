use super::*;
use crate::Capability;
use crate::testing::{Act, Fake, TempDir, layer, script, write_wasm, write_wasm_bytes};
use serde_json::json;

fn stack(scopes: &TempDir) -> Stack {
    Stack::discover(
        Some(&scopes.path().join("session")),
        Some(&scopes.path().join("project")),
        Some(&scopes.path().join("user")),
        std::sync::Arc::new(crate::testing::FakeLoader::new()),
    )
}

fn answer_of(r: &Resolved) -> Value {
    r.answer.clone().expect("an answer")
}

/// A layer whose manifest names its module: a script, which this build cannot run.
fn script_layer(root: &Path, name: &str, module: &str, body: &[u8]) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("a layer directory");
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "name = \"{name}\"\npurpose = \"p\"\nhooks = [\"notice_fired\"]\ncapabilities = []\ncontract = 1\nmodule = \"{module}\"\n"
        ),
    )
    .expect("plugin.toml");
    std::fs::write(dir.join(module), body).expect("the named module");
    dir
}

// -----------------------------------------------------------------------------------------
// `module`: the file a layer answers from, which is not always plugin.wasm
// -----------------------------------------------------------------------------------------

/// A script layer is on the ladder and answers nothing. It is pinned, hashed and polled like any
/// other; what it cannot do is run, because no engine for it is built in. Saying so at load is what
/// keeps fail-down true.
#[test]
fn a_script_module_falls_down_and_says_no_engine_for_it_is_built_in() {
    let t = TempDir::new("script-module");
    let dir = script_layer(
        &t.path().join("session"),
        "fix",
        "fix.js",
        b"export function notice_fired() { return null }\n",
    );
    layer(
        &t.path().join("project/.letibot/plugins"),
        "below",
        &["notice_fired"],
        Act::answer(json!({"from": "below"})),
    );
    let s = stack(&t);

    // A layer, hashed, and the hash is the script's rather than a plugin.wasm that is not there.
    let placed = s.layer("s1").expect("the script layer is a layer");
    assert_eq!(placed.dir, dir);
    assert_eq!(
        placed.sha256,
        crate::sha256_hex(b"export function notice_fired() { return null }\n")
    );
    // And it says why it cannot answer, once, on the stack.
    assert_eq!(s.failures().len(), 1, "{:?}", s.failures());
    let why = &s.failures()[0].why;
    assert!(why.contains("fix.js"), "{why}");
    assert!(
        why.contains("no engine for it is built into this host"),
        "{why}"
    );
    // The layer below answers, which is fail-down and not a stopped resolution.
    let r = s.call("notice_fired", &json!({}));
    assert_eq!(r.answer, Some(json!({ "from": "below" })));
    assert_eq!(r.by.as_deref(), Some("p1"));
}

#[test]
fn poll_changes_watches_the_module_the_manifest_names() {
    let t = TempDir::new("poll-named");
    let dir = script_layer(&t.path().join("session"), "fix", "fix.js", b"v1\n");
    // A `plugin.wasm` beside it is not this layer's module, so it is not what the poll reads.
    write_wasm_bytes(
        &dir,
        &script(&[("notice_fired", Act::answer(json!({"v": 1})))]),
    );
    let mut s = stack(&t);
    assert!(
        s.poll_changes().is_empty(),
        "a quiet stack has no candidates"
    );

    std::fs::write(dir.join("plugin.wasm"), b"a build of something else").expect("a wasm");
    assert!(
        s.poll_changes().is_empty(),
        "the module the manifest names is the one watched"
    );

    std::fs::write(dir.join("fix.js"), b"v2, a second version\n").expect("an edit");
    let pending = s.poll_changes();
    assert_eq!(pending.len(), 1, "the named module moved");
    assert_eq!(pending[0].layer, "s1");
    assert_eq!(
        pending[0].sha256,
        crate::sha256_hex(b"v2, a second version\n")
    );
    // And a module this build cannot run never applies, in the words the loader would use.
    let why = s
        .apply(pending.into_iter().next().unwrap())
        .expect_err("a script cannot be applied");
    assert!(
        why.contains("no engine for it is built into this host"),
        "{why}"
    );
}

/// A script layer is pinnable, and the pin is about the script: the trust ladder admits it, which is
/// the whole of this package. Without the manifest field it would pin on an empty module hash, and
/// an edit to the script would never un-pin the layer.
#[test]
fn a_script_layer_pins_on_its_script_and_an_edit_moves_the_bytes_the_pin_holds() {
    let t = TempDir::new("script-pin");
    let dir = script_layer(
        &t.path().join("project/.letibot/plugins"),
        "fix",
        "fix.js",
        b"v1\n",
    );
    let book = Trust::in_memory();
    let mut s = stack(&t);
    s.set_trust(book.clone());
    let pin = book.pin("fix", &dir).expect("a pin");
    assert_eq!(pin.module, crate::sha256_hex(b"v1\n"));
    s.refresh_trust();
    assert!(s.layer("p1").unwrap().trusted, "pinned bytes run freely");

    std::fs::write(dir.join("fix.js"), b"v2\n").expect("an edit");
    let pending = s.poll_changes();
    assert_eq!(pending.len(), 1, "the script moved");
    assert_ne!(
        pending[0].sha256, pin.module,
        "what is on disk is no longer what was pinned"
    );
    assert!(
        !book.standing(&dir, &pending[0].sha256, &pin.manifest).free,
        "the pin is a statement about the old text"
    );
}

// -----------------------------------------------------------------------------------------
// The trust ladder - a built-in is free, a placed layer needs a pin, and a pin is about bytes
// -----------------------------------------------------------------------------------------

#[test]
fn a_builtin_is_free_and_a_placed_layer_is_not_until_the_book_pins_its_bytes() {
    let t = TempDir::new("trust-arms");
    layer(
        &t.path().join("project/.letibot/plugins"),
        "one",
        &["pane_dashboard"],
        Act::answer(json!({"from": "project"})),
    );
    let mut s = stack(&t);
    s.register_builtin_bytes(
        "builtin",
        "name = \"builtin\"\npurpose = \"p\"\nhooks = [\"pane_builtin\"]\ncapabilities = []\ncontract = 1\n",
        &script(&[("pane_builtin", Act::answer(json!({"from": "binary"})))]),
    )
    .expect("a built-in registers");
    let book = Trust::in_memory();
    s.set_trust(book.clone());

    // Arm 1: the binary's own bytes, free without a pin.
    assert!(s.layer("b-builtin").unwrap().trusted);
    assert!(s.layer("b-builtin").unwrap().pin.is_none());
    // Arm 2: a placed layer is not, and says so by name.
    let placed = s.layer("p1").expect("the project layer");
    assert!(!placed.trusted, "a placed layer needs a pin");
    let said = placed.untrusted_sentence();
    assert!(said.contains("`one`"), "{said}");
    assert!(said.contains("not pinned"), "{said}");

    // The other half: pinning the bytes it has now makes it free.
    let pin = book
        .pin("one", &t.path().join("project/.letibot/plugins/one"))
        .expect("a pin");
    assert!(s.refresh_trust().is_empty(), "nothing was pinned before");
    let placed = s.layer("p1").expect("the project layer");
    assert!(placed.trusted, "a pinned layer runs freely");
    assert_eq!(
        placed.pin.as_ref().map(|p| p.module.as_str()),
        Some(pin.module.as_str())
    );
}

#[test]
fn a_module_rebuilt_after_its_pin_un_pins_the_layer_and_the_stack_says_so_once() {
    let t = TempDir::new("trust-broken");
    let root = t.path().join("project/.letibot/plugins");
    layer(
        &root,
        "one",
        &["pane_dashboard"],
        Act::answer(json!({"from": "v1"})),
    );
    let book = Trust::in_memory();
    let mut s = stack(&t);
    s.set_trust(book.clone());
    let pin = book.pin("one", &root.join("one")).expect("a pin");
    s.refresh_trust();
    assert!(s.layer("p1").unwrap().trusted);

    // The module is rebuilt - the shape this rule exists for.
    write_wasm(
        &root.join("one"),
        &["pane_dashboard"],
        Act::answer(json!({"from": "v2, rebuilt"})),
    );
    let pending = s.poll_changes().into_iter().next().expect("a candidate");
    s.apply(pending).expect("the rebuilt layer applies");
    let placed = s.layer("p1").unwrap();
    assert!(!placed.trusted, "the pin was about the old bytes");
    let said = placed.untrusted_sentence();
    assert!(said.contains(&pin.module), "the pinned hash: {said}");
    assert!(
        said.contains(&placed.sha256),
        "and what is there now: {said}"
    );
    let rebuilt = placed.sha256.clone();

    // Said once, by name: the news a host turns into a `plugin_untrusted` warning.
    let news = s.refresh_trust();
    assert_eq!(news.len(), 1, "{news:?}");
    assert_eq!(news[0].layer, "p1");
    assert_eq!(news[0].pin.module, pin.module);
    assert_eq!(news[0].module, rebuilt);
    assert!(
        s.refresh_trust().is_empty(),
        "a poll every two seconds is not news twice"
    );
}

#[test]
fn a_layer_named_by_its_name_is_the_layer_a_person_meant() {
    let t = TempDir::new("trust-named");
    layer(
        &t.path().join("project/.letibot/plugins"),
        "one",
        &["pane_dashboard"],
        Act::answer(json!({})),
    );
    let s = stack(&t);
    assert_eq!(s.layer_named("one").map(|l| l.id.as_str()), Some("p1"));
    assert_eq!(s.layer_named("p1").map(|l| l.id.as_str()), Some("p1"));
    assert!(s.layer_named("nope").is_none());
    assert_eq!(s.placed().len(), 1);
}

/// A layer directory whose manifest declares one tool.
fn tool_layer(root: &Path, name: &str, tool: &str, caps: &[&str]) {
    let dir = tool_manifest(root, name, tool, caps);
    write_wasm(&dir, &[&format!("tool_{tool}")], Act::answer(json!({})));
}

/// The same directory with the manifest and no module - the documented install of a
/// built-in's tool, where the module that answers is one the host already carries.
fn tool_manifest(root: &Path, name: &str, tool: &str, caps: &[&str]) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("a layer directory");
    let caps: Vec<String> = caps.iter().map(|c| format!("\"{c}\"")).collect();
    std::fs::write(
        dir.join(MANIFEST),
        format!(
            "name = \"{name}\"\npurpose = \"p\"\nhooks = [\"tool_{tool}\"]\n\
                 capabilities = [{}]\ncontract = 1\n\n[[tools]]\nname = \"{tool}\"\n\
                 description = \"what {name} offers\"\n\nschema = {{ type = \"object\" }}\n",
            caps.join(", ")
        ),
    )
    .expect("plugin.toml");
    dir
}

#[test]
fn declared_tools_are_read_without_loading_a_module_and_in_precedence_order() {
    let t = TempDir::new("declared-tools");
    // Two layers in the project scope - one declaring a tool, one that is only a pane - and a
    // session layer that overrides the project's tool by name.
    tool_layer(
        &t.path().join("project/.letibot/plugins"),
        "from-project",
        "look",
        &["read"],
    );
    layer(
        &t.path().join("project/.letibot/plugins"),
        "a-pane",
        &["pane_dashboard"],
        Act::answer(json!({})),
    );
    tool_layer(
        &t.path().join("session"),
        "from-session",
        "look",
        &["read:~/.claude/projects"],
    );
    let declared = Stack::declared_tools(
        Some(&t.path().join("session")),
        Some(&t.path().join("project")),
        Some(&t.path().join("user")),
    );
    // The session layer first: it is the one on top, so it is the declaration the model is
    // offered. The pane layer contributes nothing, and nothing was compiled to find that out.
    assert_eq!(declared.len(), 2, "{declared:?}");
    assert_eq!(declared[0].layer, "from-session");
    assert_eq!(declared[0].scope, Scope::Session);
    assert_eq!(declared[0].tool.name, "look");
    assert_eq!(
        declared[0].capabilities,
        vec![Capability::ReadPath("~/.claude/projects".into())]
    );
    assert_eq!(declared[1].layer, "from-project");
    assert_eq!(declared[1].scope, Scope::Project);
    assert_eq!(declared[1].tool.description, "what from-project offers");
}

#[test]
fn a_layer_whose_manifest_does_not_parse_is_skipped_by_the_scanner() {
    // `discover` reports it; this scanner is the prompt's half and has no failure list.
    let t = TempDir::new("declared-broken");
    let dir = t.path().join("project/.letibot/plugins/broken");
    std::fs::create_dir_all(&dir).expect("a directory");
    std::fs::write(dir.join(MANIFEST), "name = \"x\"\n").expect("a bad manifest");
    let declared = Stack::declared_tools(None, Some(&t.path().join("project")), None);
    assert!(declared.is_empty(), "{declared:?}");
}

#[test]
fn precedence_walks_session_project_user_builtin() {
    let t = TempDir::new("precedence");
    for (root, name, mark) in [
        ("project/.letibot/plugins", "a", "project"),
        ("user/plugins", "a", "user"),
        ("session", "a", "session"),
    ] {
        layer(
            &t.path().join(root),
            name,
            &["pane_dashboard"],
            Act::answer(json!({"from": mark})),
        );
    }
    let mut s = stack(&t);
    assert_eq!(
        s.layers().iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
        vec!["s1", "p1", "u1"],
    );
    let r = s.call("pane_dashboard", &json!({"v": 1}));
    assert_eq!(answer_of(&r), json!({"from": "session"}));
    assert_eq!(r.by.as_deref(), Some("s1"));

    // The project is next, then the user, then nothing at all.
    s.set_disabled("s1", true).unwrap();
    assert_eq!(
        answer_of(&s.call("pane_dashboard", &json!({}))),
        json!({"from": "project"})
    );
    s.set_disabled("p1", true).unwrap();
    assert_eq!(
        answer_of(&s.call("pane_dashboard", &json!({}))),
        json!({"from": "user"})
    );
    s.set_disabled("u1", true).unwrap();
    assert_eq!(s.call("pane_dashboard", &json!({})).answer, None);
    assert!(s.failures().is_empty());
}

#[test]
fn a_disabled_disk_layer_stays_off_after_a_restart_in_every_scope() {
    let t = TempDir::new("disabled-restart");
    for scope in ["session", "project/.letibot/plugins", "user/plugins"] {
        layer(
            &t.path().join(scope),
            "a",
            &["pane_dashboard"],
            Act::answer(json!({})),
        );
    }
    let mut s = stack(&t);
    for id in ["s1", "p1", "u1"] {
        s.set_disabled(id, true).unwrap();
    }
    let s = stack(&t);
    for id in ["s1", "p1", "u1"] {
        assert!(s.layer(id).unwrap().disabled, "{id} came back on");
    }
    let mut s = s;
    s.set_disabled("p1", false).unwrap();
    let s = stack(&t);
    assert!(!s.layer("p1").unwrap().disabled);
    assert!(s.layer("s1").unwrap().disabled);
}

#[test]
fn a_mask_is_not_saved_and_a_deleted_layer_takes_its_mark_along() {
    let t = TempDir::new("disabled-mask");
    layer(
        &t.path().join("session"),
        "a",
        &["pane_dashboard"],
        Act::answer(json!({})),
    );
    let mut s = stack(&t);
    s.mask("s1", true).unwrap();
    assert!(!stack(&t).layer("s1").unwrap().disabled);
    s.set_disabled("s1", true).unwrap();
    std::fs::remove_dir_all(t.path().join("session").join("a")).unwrap();
    layer(
        &t.path().join("session"),
        "a",
        &["pane_dashboard"],
        Act::answer(json!({})),
    );
    assert!(!stack(&t).layer("s1").unwrap().disabled);
}

#[test]
fn a_disabled_builtin_stays_off_after_a_restart() {
    let t = TempDir::new("disabled-builtin");
    let state = t.path().join("session").join(".disabled-builtins");
    let register = |s: &mut Stack| {
        let manifest = Manifest::parse(
            "name = \"b\"\npurpose = \"p\"\nhooks = [\"pane_dashboard\"]\n\
                 capabilities = []\ncontract = 1\n",
        )
        .unwrap();
        s.register_builtin(
            Layer {
                id: String::new(),
                scope: Scope::BuiltIn,
                dir: PathBuf::new(),
                manifest,
                sha256: String::new(),
                manifest_sha256: String::new(),
                disabled: false,
                trusted: true,
                pin: None,
            },
            Box::new(Fake::one("pane_dashboard", Act::answer(json!({})))),
        );
    };
    let mut s = stack(&t);
    s.keep_builtin_state(state.clone());
    register(&mut s);
    s.set_disabled("b-b", true).unwrap();
    let mut s = stack(&t);
    s.keep_builtin_state(state.clone());
    register(&mut s);
    assert!(s.layer("b-b").unwrap().disabled);
    s.set_disabled("b-b", false).unwrap();
    let mut s = stack(&t);
    s.keep_builtin_state(state);
    register(&mut s);
    assert!(!s.layer("b-b").unwrap().disabled);
}

#[test]
fn a_builtin_sits_below_every_scope_and_the_last_registered_is_on_top() {
    let t = TempDir::new("builtin");
    layer(
        &t.path().join("session"),
        "a",
        &["pane_dashboard"],
        Act::answer(json!({"from": "session"})),
    );
    let mut s = stack(&t);
    for name in ["first", "second"] {
        let manifest = Manifest::parse(&format!(
            "name = \"{name}\"\npurpose = \"p\"\nhooks = [\"pane_dashboard\"]\ncapabilities = []\ncontract = 1\n"
        ))
        .unwrap();
        s.register_builtin(
            Layer {
                id: String::new(),
                scope: Scope::Session,
                dir: PathBuf::new(),
                manifest,
                sha256: String::new(),
                manifest_sha256: String::new(),
                disabled: false,
                trusted: false,
                pin: None,
            },
            Box::new(Fake::one(
                "pane_dashboard",
                Act::answer(json!({ "from": name })),
            )),
        );
    }
    assert_eq!(
        s.layers().iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
        vec!["s1", "b-second", "b-first"],
    );
    s.set_disabled("s1", true).unwrap();
    assert_eq!(
        answer_of(&s.call("pane_dashboard", &json!({}))),
        json!({"from": "second"})
    );
}

/// A built-in the binary carries as bytes, at the bottom of the stack: no directory, a manifest that
/// had to parse, a module that had to be admitted, and an id built from its name. The scope above it
/// still wins, because a built-in is a default and not an exception.
#[test]
fn a_builtin_from_bytes_sits_at_the_bottom_and_obeys_the_scopes_above_it() {
    let t = TempDir::new("builtin-bytes");
    layer(
        &t.path().join("session"),
        "a",
        &["pane_sessions"],
        Act::answer(json!({"from": "session"})),
    );
    let mut s = stack(&t);
    let manifest = "name = \"sessions\"\npurpose = \"p\"\nhooks = [\"pane_sessions\"]\n\
                        capabilities = [\"read\"]\ncontract = 1\n[contracts]\n\
                        pane_sessions = 3\n";
    s.register_builtin_bytes(
        "sessions",
        manifest,
        &script(&[("pane_sessions", Act::answer(json!({"from": "builtin"})))]),
    )
    .expect("a built-in registers");
    assert_eq!(
        s.layers().iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
        vec!["s1", "b-sessions"],
    );
    // It is the bottom layer: no directory, and the operator's by definition.
    let builtin = s.layer("b-sessions").expect("the built-in");
    assert!(builtin.dir.as_os_str().is_empty());
    assert_eq!(builtin.scope, Scope::BuiltIn);
    assert!(builtin.trusted);
    assert_eq!(
        builtin.sha256.len(),
        64,
        "the bytes are hashed like any layer's"
    );
    // With the session layer above it, the session answers.
    assert_eq!(
        answer_of(&s.call("pane_sessions", &json!({}))),
        json!({"from": "session"})
    );
    s.set_disabled("s1", true).unwrap();
    let r = s.call("pane_sessions", &json!({}));
    assert_eq!(r.by.as_deref(), Some("b-sessions"));
    assert_eq!(answer_of(&r), json!({"from": "builtin"}));
}

/// A loader with no memory path refuses a built-in, and the stack says so - a fake engine
/// must not be able to pretend it loaded something it never saw.
#[test]
fn a_loader_that_cannot_load_bytes_answers_a_sentence_and_records_it() {
    struct FilesOnly;
    impl Loader for FilesOnly {
        fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
            Err(format!("no engine here: {}", wasm.display()))
        }
    }
    let mut s = Stack::discover(None, None, None, Arc::new(FilesOnly));
    let manifest = "name = \"sessions\"\npurpose = \"p\"\nhooks = [\"pane_sessions\"]\n\
                        capabilities = []\ncontract = 1\n[contracts]\n\
                        pane_sessions = 3\n";
    let why = s
        .register_builtin_bytes("sessions", manifest, b"not a module")
        .expect_err("a loader without `load_bytes` cannot register a built-in");
    assert!(why.contains("load_bytes"), "{why}");
    assert!(s.layers().is_empty());
    let failure = s.failures().first().expect("the refusal is recorded");
    assert_eq!(failure.layer, "b-sessions", "the built-in is named");
    assert_eq!(failure.scope, Scope::BuiltIn);
}

/// A built-in whose manifest does not admit its module is refused by name, not left on the stack
/// half-registered - the same rule discovery follows, and the one the stack view draws as *installed
/// and exploding* rather than as absent.
#[test]
fn a_builtin_whose_exports_its_manifest_does_not_declare_is_refused() {
    let t = TempDir::new("builtin-admit");
    let mut s = stack(&t);
    let manifest = "name = \"sessions\"\npurpose = \"p\"\nhooks = [\"pane_other\"]\n\
                        capabilities = []\ncontract = 1\n";
    let why = s
        .register_builtin_bytes(
            "sessions",
            manifest,
            &script(&[("pane_sessions", Act::answer(json!({})))]),
        )
        .expect_err("the manifest and the exports disagree");
    assert!(why.contains("pane_sessions"), "{why}");
    assert!(s.layers().is_empty(), "nothing half-registered");
}

/// A layer written for the shape before a move is refused at load, by name, and the layer
/// below it answers. No wire can tell the two shapes apart, so the manifest is the witness.
#[test]
fn a_layer_written_for_the_old_shape_is_refused_and_the_layer_below_answers() {
    let t = TempDir::new("old-shape");
    let dir = t.path().join("session").join("sessions");
    std::fs::create_dir_all(&dir).expect("a layer directory");
    // What a person wrote before the session list became a tree: the hook, and no shape.
    std::fs::write(
        dir.join(MANIFEST),
        "name = \"sessions\"\npurpose = \"p\"\nhooks = [\"pane_sessions\"]\n\
             capabilities = []\ncontract = 1\n",
    )
    .expect("plugin.toml");
    write_wasm(
        &dir,
        &["pane_sessions"],
        Act::answer(json!({"from": "groups"})),
    );
    let mut s = stack(&t);
    s.register_builtin_bytes(
        "sessions",
        "name = \"sessions\"\npurpose = \"p\"\nhooks = [\"pane_sessions\"]\n\
             capabilities = []\ncontract = 1\n[contracts]\npane_sessions = 3\n",
        &script(&[("pane_sessions", Act::answer(json!({"from": "tree"})))]),
    )
    .expect("the built-in registers");

    // The refusal names the layer, the version it declares and the one this host speaks.
    let why = s
        .failures()
        .iter()
        .find(|f| f.layer == "s1")
        .map(|f| f.why.clone())
        .expect("the old-shape layer is recorded as refused");
    assert!(why.contains("`sessions`"), "{why}");
    assert!(why.contains("contract 1"), "{why}");
    assert!(why.contains("this host speaks 3"), "{why}");
    assert!(
        why.contains("pane_sessions = 3"),
        "and the line that fixes it: {why}"
    );

    // And it never answers: the built-in below it draws the list.
    let r = s.call("pane_sessions", &json!({}));
    assert_eq!(r.by.as_deref(), Some("b-sessions"), "{:?}", r.failures);
    assert_eq!(answer_of(&r), json!({"from": "tree"}));
}

#[test]
fn the_unit_of_override_is_a_hook_so_a_lower_layer_still_answers_the_others() {
    let t = TempDir::new("per-hook");
    layer(
        &t.path().join("session"),
        "a",
        &["condition_coderabbit"],
        Act::answer(json!({"condition": "session"})),
    );
    layer(
        &t.path().join("project/.letibot/plugins"),
        "b",
        &["condition_coderabbit", "condition_github_pr"],
        Act::answer(json!({"condition": "project"})),
    );
    let s = stack(&t);
    assert_eq!(
        answer_of(&s.call("condition_coderabbit", &json!({}))),
        json!({"condition": "session"})
    );
    let r = s.call("condition_github_pr", &json!({}));
    assert_eq!(answer_of(&r), json!({"condition": "project"}));
    assert_eq!(r.by.as_deref(), Some("p1"));
    // A hook nobody exports has no answer and no failure: falling off the bottom is not an error.
    let r = s.call("notice_whatever", &json!({}));
    assert_eq!(r.answer, None);
    assert!(r.failures.is_empty());
}

#[test]
fn pass_and_unchanged_both_fall_through() {
    let t = TempDir::new("pass");
    layer(&t.path().join("session"), "a", &["notice_fired"], Act::Pass);
    layer(
        &t.path().join("project/.letibot/plugins"),
        "b",
        &["notice_fired"],
        Act::unchanged(),
    );
    layer(
        &t.path().join("user/plugins"),
        "c",
        &["notice_fired"],
        Act::answer(json!({"text": "user"})),
    );
    let r = stack(&t).call("notice_fired", &json!({"v": 1}));
    assert_eq!(answer_of(&r), json!({"text": "user"}));
    assert_eq!(r.by.as_deref(), Some("u1"));
    assert!(r.failures.is_empty());
}

#[test]
fn a_mask_switches_the_hook_off_from_that_layer_down() {
    let t = TempDir::new("mask");
    layer(
        &t.path().join("session"),
        "a",
        &["notice_fired"],
        Act::mask(),
    );
    layer(
        &t.path().join("project/.letibot/plugins"),
        "b",
        &["notice_fired"],
        Act::answer(json!({"text": "project"})),
    );
    let r = stack(&t).call("notice_fired", &json!({"v": 1}));
    assert_eq!(r.answer, None);
    assert_eq!(r.by, None);
    assert!(r.failures.is_empty(), "a mask is an answer, not a failure");
}

#[test]
fn an_ask_below_round_trips_the_lower_answer_into_the_layer_that_asked() {
    let t = TempDir::new("below");
    // The session layer changes one field of whatever answers below: `{"below":…}` on the
    // first call, and on the second it answers with what it was handed.
    layer(
        &t.path().join("session"),
        "a",
        &["pane_dashboard"],
        Act::ask_below(json!({"v": 1, "edited": true})),
    );
    layer(
        &t.path().join("project/.letibot/plugins"),
        "b",
        &["pane_dashboard"],
        Act::answer(json!({"title": "below answered"})),
    );
    let r = stack(&t).call(
        "pane_dashboard",
        &json!({"v": 1, "size": {"w": 80, "h": 24}}),
    );
    assert_eq!(
        r.by.as_deref(),
        Some("s1"),
        "the layer that asked below is the one that answers"
    );
    assert_eq!(
        answer_of(&r),
        json!({"below_seen": {"title": "below answered"}, "input": {"v": 1, "size": {"w": 80, "h": 24}}}),
        "the below answer arrives under `below`, beside the original input",
    );
    assert!(r.failures.is_empty());
}

/// A pane's `look` is an answer, not the stack's ask-below. A pane that asks the host to read a file
/// answers a document with a top-level `look`; a stack that read that as *ask the layer below* drew
/// nothing at all. `below` is the marker, and `look` belongs to the pane.
#[test]
fn a_pane_answer_carrying_c3s_look_answers_without_asking_below() {
    let t = TempDir::new("pane-look");
    let asks = json!({
        "title": "plan",
        "look": {"read": ".letibot/status.json"},
        "every_s": 0,
    });
    layer(
        &t.path().join("session"),
        "a",
        &["pane_dashboard"],
        Act::answer(asks.clone()),
    );
    layer(
        &t.path().join("project/.letibot/plugins"),
        "b",
        &["pane_dashboard"],
        Act::answer(json!({"title": "never asked"})),
    );
    let r = stack(&t).call("pane_dashboard", &json!({"v": 1}));
    assert_eq!(r.by.as_deref(), Some("s1"), "the top layer answers");
    assert_eq!(answer_of(&r), asks, "and it is the pane's own document");
    assert!(
        r.failures.is_empty(),
        "a pane's look is not a broken ask-below: {:?}",
        r.failures
    );
}

#[test]
fn a_trapping_top_layer_falls_to_the_next_and_is_recorded() {
    let t = TempDir::new("trap");
    layer(
        &t.path().join("session"),
        "a",
        &["pane_dashboard"],
        Act::trap("unreachable: memory access out of bounds"),
    );
    layer(
        &t.path().join("project/.letibot/plugins"),
        "b",
        &["pane_dashboard"],
        Act::answer(json!({"from": "project"})),
    );
    let r = stack(&t).call("pane_dashboard", &json!({}));
    assert_eq!(answer_of(&r), json!({"from": "project"}));
    assert_eq!(r.by.as_deref(), Some("p1"));
    assert_eq!(r.failures.len(), 1);
    let f = &r.failures[0];
    assert_eq!(f.layer, "s1");
    assert_eq!(f.scope, Scope::Session);
    assert_eq!(f.hook.as_deref(), Some("pane_dashboard"));
    assert!(f.why.contains("out of bounds"), "{f}");
}

#[test]
fn malformed_json_is_a_failure_and_the_layer_below_still_answers() {
    let t = TempDir::new("malformed");
    layer(
        &t.path().join("session"),
        "a",
        &["pane_dashboard"],
        Act::malformed("{not json"),
    );
    layer(
        &t.path().join("project/.letibot/plugins"),
        "b",
        &["pane_dashboard"],
        Act::answer(json!({"from": "project"})),
    );
    let r = stack(&t).call("pane_dashboard", &json!({}));
    assert_eq!(r.by.as_deref(), Some("p1"));
    assert!(r.failures[0].why.contains("not JSON"), "{}", r.failures[0]);
}

#[test]
fn a_second_ask_from_one_layer_is_a_recorded_failure_not_a_loop() {
    let t = TempDir::new("two-asks");
    // This fake answers `{"below":…}` on every call, including the one carrying `below`.
    layer(
        &t.path().join("session"),
        "a",
        &["pane_dashboard"],
        Act::always_ask(json!({"v": 1})),
    );
    layer(
        &t.path().join("project/.letibot/plugins"),
        "b",
        &["pane_dashboard"],
        Act::answer(json!({"from": "project"})),
    );
    let r = stack(&t).call("pane_dashboard", &json!({}));
    assert_eq!(r.by.as_deref(), Some("p1"));
    assert!(
        r.failures.iter().any(|f| f.why.contains("second `below`")),
        "{:?}",
        r.failures
    );
}

#[test]
fn a_disabled_layer_re_resolves_as_if_it_were_not_there() {
    let t = TempDir::new("disable");
    layer(
        &t.path().join("session"),
        "a",
        &["notice_fired"],
        Act::answer(json!({"text": "a"})),
    );
    layer(
        &t.path().join("session"),
        "b",
        &["notice_fired"],
        Act::mask(),
    );
    let mut s = stack(&t);
    assert_eq!(s.call("notice_fired", &json!({})).answer, None, "b masks");

    s.set_disabled("s2", true).unwrap();
    let r = s.call("notice_fired", &json!({}));
    assert_eq!(answer_of(&r), json!({"text": "a"}));
    assert_eq!(r.by.as_deref(), Some("s1"));
    assert_eq!(
        s.layers().len(),
        2,
        "disabled is not deleted: the stack still reads as history"
    );
    assert!(s.layer("s2").unwrap().disabled);

    s.set_disabled("s2", false).unwrap();
    assert_eq!(s.call("notice_fired", &json!({})).answer, None);
    assert!(
        s.set_disabled("s9", true).is_err(),
        "an id nobody has is refused"
    );
}

#[test]
fn layer_ids_are_creation_order_within_a_scope_and_newest_is_on_top() {
    let t = TempDir::new("order");
    let session = t.path().join("session");
    // Written in name order, so the mtime order and the name tie-break agree and the
    // assertion holds on a filesystem whose timestamps are coarse.
    for name in ["a", "b", "c"] {
        layer(
            &session,
            name,
            &["notice_fired"],
            Act::answer(json!({"text": name})),
        );
    }
    let s = stack(&t);
    assert_eq!(
        s.layers().iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
        vec!["s3", "s2", "s1"],
    );
    assert_eq!(
        answer_of(&s.call("notice_fired", &json!({}))),
        json!({"text": "c"})
    );
}

#[test]
fn a_directory_without_a_manifest_is_a_failure_and_a_directory_named_in_it_is_reported() {
    let t = TempDir::new("no-manifest");
    let session = t.path().join("session");
    let broken = session.join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("plugin.toml"), "name = ").unwrap();
    let s = stack(&t);
    assert!(s.layers().is_empty(), "no manifest is not a layer");
    assert_eq!(s.failures().len(), 1);
    assert_eq!(s.failures()[0].layer, "broken");
    assert_eq!(s.failures()[0].scope, Scope::Session);
    assert!(
        s.failures()[0].why.contains("plugin.toml"),
        "{}",
        s.failures()[0]
    );
}

#[test]
fn an_engine_that_refuses_a_module_is_a_recorded_failure_not_a_missing_stack() {
    let t = TempDir::new("engine-refuses");
    let session = t.path().join("session");
    layer(
        &session,
        "a",
        &["notice_fired"],
        Act::answer(json!({"text": "a"})),
    );
    let loader = std::sync::Arc::new(crate::testing::FakeLoader::failing(
        "import `wasi_snapshot_preview1` is not granted by [read]",
    ));
    let s = Stack::discover(Some(&session), None, None, loader);
    assert_eq!(s.layers().len(), 1, "the layer exists; its module does not");
    assert_eq!(s.failures().len(), 1);
    assert!(
        s.failures()[0].why.contains("wasi_snapshot_preview1"),
        "{}",
        s.failures()[0]
    );
    assert_eq!(s.call("notice_fired", &json!({})).answer, None);
}

#[test]
fn discovery_hands_the_manifest_capabilities_to_the_engine() {
    let t = TempDir::new("caps");
    let session = t.path().join("session");
    crate::testing::layer_with(
        &session,
        "a",
        &["condition_github_pr"],
        &["read", "exec:gh"],
        Act::answer(json!({"fired": null})),
    );
    let loader = std::sync::Arc::new(crate::testing::FakeLoader::new());
    let s = Stack::discover(Some(&session), None, None, loader.clone());
    assert_eq!(s.layers().len(), 1);
    let caps = loader
        .caps_of(&session.join("a").join("plugin.wasm"))
        .expect("the engine was asked to load it");
    assert_eq!(
        caps,
        vec![Capability::Read, Capability::Exec("gh".to_string())],
        "a module is handed exactly what its manifest declared and nothing more"
    );
}

#[test]
fn a_layer_whose_module_will_not_load_stays_listed_and_never_answers() {
    let t = TempDir::new("bad-wasm");
    let session = t.path().join("session");
    let dir = session.join("a");
    layer(
        &session,
        "a",
        &["notice_fired"],
        Act::answer(json!({"text": "a"})),
    );
    std::fs::write(dir.join("plugin.wasm"), "this is not a fake module").unwrap();
    let s = stack(&t);
    assert_eq!(s.layers().len(), 1);
    assert_eq!(s.layers()[0].id, "s1");
    assert_eq!(
        s.failures().len(),
        1,
        "the reason is on the stack, not in every call"
    );
    assert_eq!(s.call("notice_fired", &json!({})).answer, None);
}

/// A manifest and no module is a declaration, not a failure - the documented install of a
/// built-in's tool. Two failures here would be published as `plugin_layer` warnings on the one
/// code that means *this layer is exploding*.
#[test]
fn a_manifest_with_no_module_is_a_declaration_and_not_a_failure() {
    let t = TempDir::new("declared-only");
    let session = t.path().join("session");
    let dir = tool_manifest(
        &session,
        "from-builtin",
        "look",
        &["read:~/.claude/projects"],
    );
    let mut s = stack(&t);
    assert!(
        s.failures().is_empty(),
        "a correct install must not look broken: {:?}",
        s.failures()
    );
    assert_eq!(
        s.layers().len(),
        1,
        "it is a layer: the model is offered its tool"
    );
    assert_eq!(s.layers()[0].id, "s1");
    assert_eq!(s.layers()[0].sha256, "", "there are no bytes to hash");
    // Nothing answers it from here - the module that does is one the host puts below.
    assert_eq!(s.call("tool_look", &json!({})).answer, None);
    assert!(
        s.failures().is_empty(),
        "and resolving it is not a failure either"
    );
    assert!(
        s.poll_changes().is_empty(),
        "a poll of the same directory is silent"
    );

    // A module written beside the manifest is a change the poll sees, so a declaration is not
    // a layer that can never answer.
    write_wasm(&dir, &["tool_look"], Act::answer(json!({"ok": true})));
    let pending = s.poll_changes();
    assert_eq!(pending.len(), 1, "the module that arrived is a change");
    assert_eq!(pending[0].layer, "s1", "for the layer that already exists");
    s.apply(pending.into_iter().next().unwrap()).unwrap();
    assert_eq!(
        answer_of(&s.call("tool_look", &json!({}))),
        json!({"ok": true})
    );
}

#[test]
fn poll_changes_sees_a_rewritten_plugin_wasm_and_nothing_else() {
    let t = TempDir::new("poll");
    let session = t.path().join("session");
    layer(
        &session,
        "a",
        &["notice_fired"],
        Act::answer(json!({"text": "a"})),
    );
    layer(
        &session,
        "b",
        &["notice_fired"],
        Act::answer(json!({"text": "b"})),
    );
    let mut s = stack(&t);
    assert!(
        s.poll_changes().is_empty(),
        "a quiet stack has no candidates"
    );

    // Longer than the first script, so the poll's length check alone would catch the rewrite
    // on a filesystem whose mtimes are coarse.
    write_wasm(
        &session.join("a"),
        &["notice_fired"],
        Act::answer(json!({"text": "a, rebuilt and rather longer than it was"})),
    );
    let pending = s.poll_changes();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].layer, "s1");
    assert_eq!(pending[0].scope, Scope::Session);
    assert_ne!(pending[0].sha256, s.layer("s1").unwrap().sha256);

    // Still pending: a change that has not been applied has not happened.
    assert_eq!(s.poll_changes().len(), 1);

    s.apply(pending.into_iter().next().unwrap()).unwrap();
    // `b` is on top and answers first; turning it off is how the rebuilt `a` is heard.
    s.set_disabled("s2", true).unwrap();
    assert_eq!(
        answer_of(&s.call("notice_fired", &json!({}))),
        json!({"text": "a, rebuilt and rather longer than it was"})
    );
    assert!(s.poll_changes().is_empty(), "applied is settled");
}

#[test]
fn apply_refuses_a_module_that_moved_after_the_gate_replayed_it() {
    let t = TempDir::new("stale");
    let session = t.path().join("session");
    layer(
        &session,
        "a",
        &["notice_fired"],
        Act::answer(json!({"text": "a"})),
    );
    let mut s = stack(&t);
    write_wasm(
        &session.join("a"),
        &["notice_fired"],
        Act::answer(json!({"text": "rebuilt"})),
    );
    let pending = s.poll_changes().into_iter().next().unwrap();
    // What the gate does: it loads the bytes and remembers them on the candidate.
    let module: Box<dyn Module> = Box::new(Fake::one(
        "notice_fired",
        Act::answer(json!({"text": "rebuilt"})),
    ));
    pending.remember("a-different-sha", Arc::from(module));
    let why = s
        .apply(pending)
        .expect_err("not the bytes that were replayed");
    assert!(why.contains("gate it again"), "{why}");
}

#[test]
fn a_new_directory_becomes_a_layer_on_top_of_its_scope() {
    let t = TempDir::new("new-layer");
    let session = t.path().join("session");
    layer(
        &session,
        "a",
        &["notice_fired"],
        Act::answer(json!({"text": "a"})),
    );
    let mut s = stack(&t);
    let fresh = session.join("b");
    layer(
        &session,
        "b",
        &["notice_fired"],
        Act::answer(json!({"text": "b"})),
    );
    let c = s.candidate(&fresh, Scope::Session).unwrap();
    assert_eq!(
        c.layer, "",
        "a directory the stack has never seen has no id yet"
    );
    s.apply(c).unwrap();
    assert_eq!(s.layers()[0].id, "s2");
    assert_eq!(
        answer_of(&s.call("notice_fired", &json!({}))),
        json!({"text": "b"})
    );
}

/// A guard layer that traps, returns malformed text or asks below twice refuses the call and
/// names itself. An advisory hook falls through to the layer below as before.
#[test]
fn a_failed_guard_refuses_and_a_failed_advisory_layer_falls_through() {
    for (tag, act) in [
        ("trap", Act::trap("out of fuel")),
        ("malformed", Act::malformed("not json")),
        ("twice", Act::always_ask(json!({"v": 1}))),
    ] {
        let t = TempDir::new(&format!("guard-{tag}"));
        layer(
            &t.path().join("session"),
            "a",
            &["on_tool_call", "notice_fired"],
            act,
        );
        layer(
            &t.path().join("project/.letibot/plugins"),
            "b",
            &["on_tool_call", "notice_fired"],
            Act::answer(json!({"from": "below"})),
        );
        let s = stack(&t);
        let r = s.call("on_tool_call", &json!({}));
        let (by, why) = r.refused.clone().unwrap_or_else(|| panic!("{tag}: {r:?}"));
        assert_eq!(by, "s1", "{tag}");
        assert!(!why.is_empty(), "{tag}");
        assert_eq!(
            r.answer, None,
            "{tag}: the layer below does not answer for a failed guard"
        );
    }
    let t = TempDir::new("advisory-trap");
    layer(
        &t.path().join("session"),
        "a",
        &["notice_fired"],
        Act::trap("boom"),
    );
    layer(
        &t.path().join("project/.letibot/plugins"),
        "b",
        &["notice_fired"],
        Act::answer(json!({"from": "below"})),
    );
    let r = stack(&t).call("notice_fired", &json!({}));
    assert_eq!(r.refused, None);
    assert_eq!(r.answer, Some(json!({"from": "below"})));
}

// -----------------------------------------------------------------------------------------
// The gate's floor: reviewed policy, and a session's own layer is not a reviewer
// -----------------------------------------------------------------------------------------

/// The scope rules are decided where a layer ENTERS the stack, not only when a hook is resolved: a
/// session's own layer answering the review of its own branch is refused at `apply`, the door every
/// new or rebuilt layer takes, and the project's copy of it is exactly where it belongs.
#[test]
fn a_session_layer_answering_a_review_hook_is_refused_where_it_is_applied() {
    let hook = letibot_plugin::review::VERDICT;
    let t = TempDir::new("apply-scope");
    let session = t.path().join("session");
    layer(
        &session,
        "self-review",
        &[hook],
        Act::answer(json!({"floor": "accept", "why": "it is my own branch"})),
    );
    let mut s = stack(&t);
    let c = s
        .candidate(&session.join("self-review"), Scope::Session)
        .expect("the layer is a candidate like any other");
    let why = s
        .apply(c)
        .expect_err("a session layer never answers the review of its own branch");
    assert!(why.contains(hook), "the hook is named: {why}");
    assert!(why.contains("session"), "and the scope: {why}");

    // The same manifest one scope up is admitted, so the refusal is about who wrote it.
    let project = t.path().join("project/.letibot/plugins");
    layer(
        &project,
        "our-review",
        &[hook],
        Act::answer(json!({"floor": "accept"})),
    );
    let c = s
        .candidate(&project.join("our-review"), Scope::Project)
        .expect("a candidate");
    s.apply(c)
        .expect("a project's review policy is the project's");
    assert_eq!(s.call(hook, &json!({})).by.as_deref(), Some("p1"));
}

/// A session's layer never answers the floor: a layer could raise its own limit to `auto`, and
/// the answer that stands is the project's. Two doors - this and `Manifest::admit_for` - because
/// a stack is discovered before anything asks a manifest about its scope.
#[test]
fn a_session_scope_layer_never_answers_the_gates_floor() {
    let hook = letibot_plugin::adjudicate::HOOK;
    let t = TempDir::new("floor-scope");
    layer(
        &t.path().join("session"),
        "own",
        &[hook],
        Act::answer(json!({"floor": "auto"})),
    );
    layer(
        &t.path().join("project/.letibot/plugins"),
        "prod",
        &[hook],
        Act::answer(json!({"intents": ["prod-host"], "floor": "ask"})),
    );
    let s = stack(&t);
    assert!(s.layer("s1").is_some(), "the session layer is on the stack");
    let r = s.call(hook, &json!({"v": 1}));
    assert_eq!(r.by.as_deref(), Some("p1"), "{r:?}");
    assert_eq!(
        answer_of(&r),
        json!({"intents": ["prod-host"], "floor": "ask"})
    );
    assert!(r.failures.is_empty(), "nobody failed: {:?}", r.failures);

    // And with nothing below it, a session's floor is no answer at all - not a silent `auto`.
    let alone = TempDir::new("floor-scope-alone");
    layer(
        &alone.path().join("session"),
        "own",
        &[hook],
        Act::answer(json!({"floor": "auto"})),
    );
    let s = Stack::discover(
        Some(&alone.path().join("session")),
        None,
        None,
        std::sync::Arc::new(crate::testing::FakeLoader::new()),
    );
    let r = s.call(hook, &json!({"v": 1}));
    assert_eq!(r.answer, None, "{r:?}");
    assert_eq!(r.by, None);
}

/// A floor that traps is a guard that failed, not a layer with nothing to say: the stack holds
/// this hook closed, so a trap reaches the caller as a refusal instead of a pass.
#[test]
fn a_trapping_floor_layer_is_a_refusal_and_not_a_pass() {
    let hook = letibot_plugin::adjudicate::HOOK;
    let t = TempDir::new("floor-trap");
    layer(
        &t.path().join("project/.letibot/plugins"),
        "prod",
        &[hook],
        Act::trap("its fuel ran out"),
    );
    let r = stack(&t).call(hook, &json!({"v": 1}));
    assert_eq!(r.answer, None);
    let (by, why) = r
        .refused
        .clone()
        .unwrap_or_else(|| panic!("a failed floor is a refusal: {r:?}"));
    assert_eq!(by, "p1");
    assert!(why.contains("fuel ran out"), "{why}");
}
