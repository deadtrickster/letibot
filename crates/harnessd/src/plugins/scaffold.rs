//! **`plugin scaffold`: a pane plugin's source, written by the session that will use it.**
//!
//! The template is three files and nothing else, and each is load-bearing:
//!
//! | file | is |
//! |---|---|
//! | `Cargo.toml` | a `cdylib` for the host and an `rlib` for the tests, the rano SDK by path, and a release profile that keeps the module small |
//! | `plugin.toml` | the C2 manifest: the name, the one-line purpose a person consents to, the hooks, no capabilities by default, contract 1 |
//! | `src/lib.rs` | the pane hook (`pane_<ident>`), one pure `view` function, and a native test of it |
//!
//! # Why a native test in the template
//!
//! `docs/plugins-plan.md` §1 principle 1 makes a hook a pure function: JSON in, JSON out. That is
//! why the whole dashboard can be asserted without a wasm host — and why the template ships the
//! test that proves it. A pane whose logic only ran inside wasm would be a pane nobody could check
//! before loading it, and the measured rake of this slice is exactly that: the example's committed
//! `.wasm` had never been executed once and trapped on every call
//! (`.letibot/notes/the-pane-contracts-two-look-keys.md`).
//!
//! # What the template deliberately does not do
//!
//! **No clock.** `std::time::SystemTime::now()` on `wasm32-unknown-unknown` answers a panic, and a
//! panic in a guest is a trap: a pane that wants to say *how old* something is has to be handed
//! the time by its look. The comment in the template says so, in the place somebody would write it.
//!
//! **No I/O.** A plugin never reads a file or runs a program (`docs/plugins-plan.md` §1): it asks
//! for a `look` and the host runs it. The template's view is therefore a pure function of its
//! input, and that is what makes `plugin test` (and the shadow gate's replay) mean anything.

use std::path::{Path, PathBuf};

use letibot_plugins::MANIFEST;

/// The longest name a plugin may have. A pane id is `<name>/<hook>` and it is drawn in a title,
/// so the bound is about a title that fits rather than about a filesystem.
const MAX_NAME: usize = 48;

/// **The one hook kind this host runs today**, as `scaffold` names it: the kind without its
/// underscore. The plan's other four patch points are the next package
/// (`docs/plugins-plan.md` §7 step 2), and a scaffold that wrote a `condition_` module nobody
/// calls would be a directory that never answers.
pub const HOOK_KINDS: &[&str] = &["pane"];

/// Where the slice-1 Rust SDK lives on a box that has a rano checkout beside its projects — the
/// path `plugins/examples/plan-dashboard/Cargo.toml` uses, and the one a template has to write
/// because cargo cannot be told *any checkout of rano will do*.
pub const SDK_DEFAULT: &str = "Projects/rano/rano/sdks/rust";

/// What resolved as the SDK: the path the template writes, and whether it was found here.
#[derive(Debug, Clone)]
pub struct Sdk {
    pub path: String,
    pub found: bool,
}

/// **Where the SDK is on this box**, in the order the tree resolves such things: the environment
/// first (a box may have rano anywhere), then the conventional checkout under `$HOME`.
///
/// A path that does not exist is still WRITTEN, with `found: false` and the sentence the tool
/// result carries: scaffolding a plugin is useful on a box that cannot build it yet — the source
/// is what gets committed and reviewed — and refusing would make the first step of the loop need
/// a toolchain the last step is what checks for.
pub fn sdk() -> Sdk {
    let from_env = std::env::var("LETIBOT_RANO_SDK")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let home = std::env::var("HOME").ok();
    let conventional = home
        .as_ref()
        .map(|h| PathBuf::from(h).join(SDK_DEFAULT))
        .filter(|p| p.join("Cargo.toml").is_file());
    match (from_env, conventional) {
        (Some(path), _) => {
            let found = Path::new(&path).join("Cargo.toml").is_file();
            Sdk { path, found }
        }
        (None, Some(path)) => Sdk {
            path: path.display().to_string(),
            found: true,
        },
        (None, None) => Sdk {
            path: home
                .map(|h| PathBuf::from(h).join(SDK_DEFAULT).display().to_string())
                .unwrap_or_else(|| SDK_DEFAULT.to_string()),
            found: false,
        },
    }
}

/// **A plugin name**, checked where it becomes a directory, a crate and a hook.
///
/// Cargo's rule for a package name is the loosest of the three, so this is stricter on purpose: a
/// name is also half a pane id (`<name>/<hook>`) and a Rust function name, and a name that had to
/// be escaped differently in each of the three is a defect waiting for the second plugin.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a plugin needs a name".to_string());
    }
    if name.len() > MAX_NAME {
        return Err(format!(
            "`{name}` is {} characters: a name is at most {MAX_NAME}, because it is half a pane \
             id and the id is drawn in a title",
            name.len()
        ));
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        return Err(format!(
            "`{name}`: a plugin's name is lowercase letters, digits and `-` — it is a crate name \
             and half a pane id, so `{}` is what it would have to be",
            slug(name)
        ));
    }
    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return Err(format!(
            "`{name}`: a name does not start or end with `-`, and has no `--` in it"
        ));
    }
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Err(format!("`{name}`: a plugin's name starts with a letter"));
    }
    Ok(())
}

/// A name made to fit [`check_name`], for the refusal that quotes it back.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    match out.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => out,
        // A name that starts with a digit or is empty once the punctuation is gone: `p` in front,
        // because a crate name and a Rust identifier both have to start with a letter.
        _ => format!("p{out}"),
    }
}

/// The Rust identifier a name becomes: `deploy-status` → `deploy_status`.
pub fn ident(name: &str) -> String {
    name.replace('-', "_")
}

/// The hook this plugin answers: `pane_<ident>`.
pub fn hook_name(kind: &str, name: &str) -> String {
    format!("{kind}_{}", ident(name))
}

/// The pane id this plugin's hook is drawn under (C4): `<manifest name>/<hook without its kind>`.
pub fn pane_id(name: &str) -> String {
    format!("{name}/{}", ident(name))
}

/// What a scaffold wrote, for the sentence the model reads.
#[derive(Debug)]
pub struct Scaffolded {
    pub dir: PathBuf,
    pub files: Vec<String>,
    pub sdk: Sdk,
    pub manifest: String,
    pub pane: String,
}

/// **Write the template into `plugins_dir/<name>`** — refusing to touch a directory that already
/// has a manifest, because that is somebody's plugin and the scaffold's job is the first frame.
pub fn write(
    plugins_dir: &Path,
    name: &str,
    purpose: &str,
    kind: &str,
) -> Result<Scaffolded, String> {
    check_name(name)?;
    if !HOOK_KINDS.contains(&kind) {
        return Err(format!(
            "`{kind}` is not a hook kind this host runs ({}): the other patch points of \
             `docs/plugins-plan.md` §7 step 2 are the next package",
            HOOK_KINDS.join(", ")
        ));
    }
    let purpose = purpose.trim();
    if purpose.is_empty() {
        return Err(
            "a scaffold needs its one-line purpose: it is what a person reads in the stack view \
             and what a capability is consented to by, so an empty one cannot be written"
                .to_string(),
        );
    }
    let dir = plugins_dir.join(name);
    if dir.join(MANIFEST).is_file() {
        return Err(format!(
            "there is already a plugin at {} — open its `src/lib.rs` and edit it, or scaffold \
             another name",
            dir.display()
        ));
    }
    let sdk = sdk();
    let hook = hook_name(kind, name);
    std::fs::create_dir_all(dir.join("src")).map_err(|e| format!("{}: {e}", dir.display()))?;
    let files = [
        ("Cargo.toml", cargo_toml(name, kind, &sdk.path)),
        (MANIFEST, manifest(name, purpose, &hook)),
        ("src/lib.rs", lib_rs(name, purpose, &hook, kind)),
    ];
    let mut written = Vec::new();
    for (rel, body) in files {
        let path = dir.join(rel);
        std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(rel.to_string());
    }
    Ok(Scaffolded {
        manifest: dir.join(MANIFEST).display().to_string(),
        dir,
        files: written,
        sdk,
        pane: pane_id(name),
    })
}

/// The `plugin.toml` the template writes — through [`letibot_plugins::Manifest`]'s own parser at
/// load time, so a template that did not parse would be a scaffold whose every plugin fails at
/// discovery. The unit test below runs the real parser over it.
fn manifest(name: &str, purpose: &str, hook: &str) -> String {
    format!(
        "# What this layer declares: `docs/plugins-slice1.md` C2. A capability is a REQUEST the\n\
         # host enforces (`docs/plugins-plan.md` §3), and a session plugin starts with none.\n\
         name = \"{name}\"\n\
         purpose = \"{}\"\n\
         hooks = [\"{hook}\"]\n\
         capabilities = []\n\
         contract = 1\n",
        escape(purpose)
    )
}

fn cargo_toml(name: &str, kind: &str, sdk: &str) -> String {
    format!(
        "[package]\n\
         name = \"{name}\"\n\
         version = \"0.1.0\"\n\
         edition = \"2024\"\n\
         publish = false\n\
         description = \"a letibot {kind} plugin\"\n\
         \n\
         [lib]\n\
         # `cdylib` is the plugin the host loads; `rlib` is what the native test below links, so\n\
         # the view can be asserted without a wasm host.\n\
         crate-type = [\"cdylib\", \"rlib\"]\n\
         \n\
         [dependencies]\n\
         # The Rust SDK over rano's ABI: the memory, the allocator and the result block are its\n\
         # job, not this file's. A path, because slice 1's SDK lives in a rano checkout.\n\
         rano-plugin = {{ path = \"{sdk}\" }}\n\
         serde_json = \"1\"\n\
         \n\
         [profile.release]\n\
         # A guest module is read out of a directory and compiled per load: small is what matters.\n\
         opt-level = \"s\"\n\
         strip = true\n\
         lto = true\n\
         panic = \"abort\"\n\
         \n\
         # **Not a member of letibot's workspace**: this is a guest module for another host's ABI,\n\
         # living in a session's own directory.\n\
         [workspace]\n"
    )
}

/// The guest's source. Kept as a `format!` rather than as a file under `templates/` so that the
/// name, the purpose and the hook cannot drift out of the three files that must agree about them.
fn lib_rs(name: &str, purpose: &str, hook: &str, kind: &str) -> String {
    format!(
        r#"//! **{name}** — {purpose}
//!
//! One pure function, [`view`]: an input in, a pane out. The contract is
//! `docs/plugins-slice1.md` C3 and the host's own version of it is the `plugin_hooks` tool.
//!
//! # The loop this file is the start of
//!
//! ```text
//! plugin scaffold {name}   (done)
//!   → edit `view` below
//!   → plugin build {name}
//!   → plugin test  {name}
//!   → plugin load  {name}      (the shadow gate, then hot — no restart)
//!   → edit, build, load again: hot reload, so the pane changes under the reader
//! ```
//!
//! # Two things a pane may not do
//!
//! * **No I/O.** A plugin never opens a file or runs a program: it asks its answer for a `look`
//!   (`{{"read": "path"}}`) and the host runs it, under the capability `plugin.toml` declares.
//! * **No clock.** `std::time::SystemTime::now()` answers a panic on `wasm32-unknown-unknown`,
//!   and a panic in a guest is a trap — the host draws an unavailable panel instead of your view.
//!   A timestamp comes from the look's text or from the host's `state`, never from `now()`.

use serde_json::{{json, Value}};

/// The pane's title: what the head draws at the top of the frame.
const TITLE: &str = "{name}";

/// **The pane's whole logic** — the pure function the native test below asserts.
///
/// `input` is the host's C3 input: `state` (whatever this pane returned last time), `look` (the
/// result of the look it asked for), `event` (the key a person pressed, or null) and `size` (the
/// rectangle the head is drawing in). Every field is always present.
///
/// A `View` is `title`, `badge`, `panels`, `findings`, `legend`, `look`, `every_s` and `state`.
/// The keys are the HOST's: `q`, `j`, `k`, `f` and `l` are taken, and so are ↑/↓, PgUp/PgDn,
/// Enter and Esc. A panel's key opens its `view`; an action's key does the one thing it names.
pub fn view(input: &Value) -> Value {{
    // The rectangle the head is drawing in, which is why an answer can be laid out for a screen
    // somebody has instead of for a guess.
    let w = input["size"]["w"].as_u64().unwrap_or(0);
    let h = input["size"]["h"].as_u64().unwrap_or(0);
    // `state` is how a pane remembers anything: it is returned in the answer, kept by the host,
    // and handed back on the next call. A plugin is otherwise stateless between calls.
    let seen = input["state"]["seen"].as_u64().unwrap_or(0) + 1;
    json!({{
        "title": format!("{{TITLE}} · {{seen}}"),
        "panels": [
            {{
                "id": "pane",
                "key": "t",
                "title": "pane",
                "denominator": "what this pane draws",
                "summary": [
                    {{"kv": {{"rows": [
                        ["drawn at", format!("{{w}}×{{h}}")],
                        ["redraws", seen.to_string()],
                        ["source", "src/lib.rs".to_string()]
                    ]}}}}
                ],
                "view": [
                    {{"markdown": "- **This is the template's view.** Replace `view` in \
`src/lib.rs` with what the pane is for.\n- Every list item and heading is a stop: ↑/↓ move \
between them, Enter opens a stop's `detail`.\n- The `t` key opens this view; `q` closes the pane, \
and `f` and `l` are the host's findings and legend keys."}}
                ]
            }}
        ],
        // A pane that cannot get what it needs says WHY and HOW TO FIX IT here rather than
        // drawing an empty frame: an empty dashboard and a broken one look identical.
        "unavailable": null,
        // `{{"read": "path"}}` asks the host to read a file and call this pane again with the
        // answer in `look`; it needs `capabilities = ["read"]` in plugin.toml. `every_s` is how
        // often to ask again — 0 is never, and 5 is the host's floor.
        "every_s": 0,
        "state": {{"seen": seen}}
    }})
}}

// **`rano_abi` and `rano_alloc`**: the two exports every rano plugin has.
rano_plugin::abi!();

/// **The hook the host calls**: a `{kind}` hook named `{hook}` — the export name is the name, and
/// `plugin.toml`'s `hooks = ["{hook}"]` must be exactly what this crate exports.
///
/// `None` is the ABI's *leave it alone*: this pane answers a view for every input it can read, and
/// hands back nothing only for text that is not JSON at all (a host defect, not a pane state).
#[unsafe(no_mangle)]
pub extern "C" fn {hook}(ptr: u32, len: u32) -> u32 {{
    rano_plugin::hook(ptr, len, |text| {{
        let input: Value = serde_json::from_str(text).ok()?;
        serde_json::to_string(&view(&input)).ok()
    }})
}}

#[cfg(test)]
mod tests {{
    use super::*;

    /// The host's keys (C3): a panel or action key that collides with one of these is a view the
    /// host refuses, so the template checks its own answer here rather than at load.
    const HOST_KEYS: [&str; 5] = ['q', 'j', 'k', 'f', 'l'];

    /// A C3 input, exactly the shape the host hands a pane: every field present, and null where
    /// nothing has happened yet.
    fn input() -> Value {{
        json!({{"v": 1, "state": null, "look": null, "event": null,
                "size": {{"w": 100, "h": 30}}}})
    }}

    #[test]
    fn the_view_is_a_pane_the_host_can_drive() {{
        let out = view(&input());
        assert!(out["title"].as_str().is_some_and(|t| !t.is_empty()), "{{out}}");
        let panels = out["panels"].as_array().expect("a view has panels");
        assert!(!panels.is_empty(), "{{out}}");
        for panel in panels {{
            // A panel that has a view needs one lowercase letter to open it, and it may not be
            // one of the host's.
            if !panel["view"].is_null() {{
                let key = panel["key"].as_str().expect("a key on a panel with a view");
                let c = key.chars().next().expect("a key");
                assert_eq!(key.chars().count(), 1, "one letter: {{key}}");
                assert!(!HOST_KEYS.contains(&c), "`{{c}}` is one of the host's keys");
            }}
        }}
        // C3's floor for a repeating look is five seconds, and zero is *never*.
        let every = out["every_s"].as_u64().expect("every_s is a number");
        assert!(every == 0 || every >= 5, "every_s {{every}}");
    }}

    #[test]
    fn the_state_a_pane_returns_is_the_state_it_is_handed_back() {{
        // §1 principle 5: the host keeps the state, and a pane is stateless between calls. The
        // second frame is asked with what the first answered, and counts it.
        let first = view(&input());
        let second = view(&json!({{"v": 1, "state": first["state"], "look": null,
                                   "event": null, "size": {{"w": 100, "h": 30}}}}));
        assert_eq!(first["state"]["seen"], 1);
        assert_eq!(second["state"]["seen"], 2);
        assert!(
            second["title"].as_str().unwrap_or_default().ends_with("2"),
            "{{second}}"
        );
    }}

    #[test]
    fn the_rectangle_the_head_sends_reaches_the_view() {{
        let out = view(&json!({{"v": 1, "state": null, "look": null, "event": null,
                                "size": {{"w": 77, "h": 21}}}}));
        let drawn = serde_json::to_string(&out).expect("a view");
        assert!(drawn.contains("77×21"), "{{drawn}}");
    }}
}}
"#
    )
}

/// A TOML basic string body: a purpose is prose, and prose has quotes and backslashes in it.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' | '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "letibot-scaffold-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a temp dir");
        path
    }

    #[test]
    fn names_are_crate_names_and_half_a_pane_id_at_once() {
        for good in ["deploy", "deploy-status", "plan2", "a-b-c"] {
            check_name(good).unwrap_or_else(|e| panic!("{good}: {e}"));
        }
        for (bad, why) in [
            ("", "a name"),
            ("Deploy", "lowercase"),
            ("deploy_status", "lowercase"),
            ("-deploy", "does not start"),
            ("deploy-", "does not start"),
            ("a--b", "no `--`"),
            ("9lives", "starts with a letter"),
        ] {
            let e = check_name(bad).expect_err(bad);
            assert!(e.contains(why), "`{bad}`: {e}");
        }
        assert!(check_name(&"x".repeat(MAX_NAME + 1)).is_err());
    }

    /// **The template parses with the host's own manifest parser** — a scaffold whose every
    /// plugin failed at discovery would be a loop that never gets past its first step.
    #[test]
    fn the_written_manifest_is_one_the_host_admits() {
        let root = temp("manifest");
        let out = write(
            &root,
            "deploy-status",
            "the deploy queue, per environment",
            "pane",
        )
        .expect("scaffolded");
        let manifest = letibot_plugins::Manifest::read(&out.dir).expect("the manifest parses");
        assert_eq!(manifest.name, "deploy-status");
        assert_eq!(manifest.purpose, "the deploy queue, per environment");
        assert_eq!(manifest.hooks, vec!["pane_deploy_status".to_string()]);
        assert!(manifest.capabilities.is_empty(), "no capability by default");
        assert_eq!(manifest.contract, letibot_plugins::CONTRACT);
        // The hook the manifest declares is the export the template defines.
        assert_eq!(out.pane, "deploy-status/deploy_status");
        let lib = std::fs::read_to_string(out.dir.join("src/lib.rs")).expect("the source");
        assert!(lib.contains("pub extern \"C\" fn pane_deploy_status(ptr: u32, len: u32)"));
        std::fs::remove_dir_all(&root).ok();
    }

    /// A purpose with a quote in it is still a TOML string — the manifest is prose in a value
    /// position, and a plugin whose purpose broke the file would be one whose scaffold wrote a
    /// syntax error.
    #[test]
    fn a_purpose_with_quotes_and_backslashes_survives_the_manifest() {
        let root = temp("purpose");
        let out = write(
            &root,
            "quotes",
            "what \"the operator\" calls a C:\\temp job",
            "pane",
        )
        .expect("scaffolded");
        let manifest = letibot_plugins::Manifest::read(&out.dir).expect("the manifest parses");
        assert_eq!(
            manifest.purpose,
            "what \"the operator\" calls a C:\\temp job"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// **The template's own view, as JSON** — the rules the host would refuse it for, checked
    /// where the template is written rather than only inside the guest. This is the half that
    /// keeps the template honest on a box with no wasm toolchain.
    #[test]
    fn the_templates_view_a_panel_with_a_view_has_one_unowned_key() {
        let body = lib_rs("deploy", "the deploy queue", "pane_deploy", "pane");
        // The template carries its own assertions, and they are compiled where the plugin lands;
        // what is checked here is that it HAS them (a template without its test is a template
        // teaching the loop without the step that catches the trap).
        assert!(body.contains("#[cfg(test)]"), "the template ships a test");
        assert!(body.contains("fn the_view_is_a_pane_the_host_can_drive"));
        assert!(body.contains("const HOST_KEYS: [&str; 5] = ['q', 'j', 'k', 'f', 'l'];"));
        // And it names the two rakes a guest meets first.
        assert!(body.contains("SystemTime::now()"));
        assert!(body.contains("\"every_s\": 0"));
    }

    #[test]
    fn a_scaffold_never_overwrites_a_plugin() {
        let root = temp("exists");
        write(&root, "twice", "one", "pane").expect("the first");
        let e = write(&root, "twice", "two", "pane").expect_err("the second");
        assert!(e.contains("already a plugin"), "{e}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_hook_kind_this_host_does_not_run_is_refused_with_the_one_it_does() {
        let root = temp("kind");
        let e = write(&root, "x", "p", "condition").expect_err("no such kind yet");
        assert!(e.contains("`condition` is not a hook kind"), "{e}");
        assert!(e.contains("pane"), "{e}");
        std::fs::remove_dir_all(&root).ok();
    }
}
