//! The `link_` hook (X26, `docs/plugins-plan.md` §4.6): the words on the frame that name something
//! the head's own references do not know, asked of a layer ONCE per frame and cached by word.

use super::*;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// What the layer does with the words it is asked about.
#[derive(Clone, Copy)]
enum Mode {
    /// Link every `LET-<n>` word to a board that only the layer knows.
    Boards,
    /// Trap, which is the fail-safe's half of the contract.
    Trap,
    /// Answer a target a terminal must not be handed.
    Nonsense,
}

/// A module that records every input it is given, so a test can count the CALLS and not only the
/// links - batching is a claim about how often the host asks.
struct Links {
    hooks: Vec<String>,
    mode: Mode,
    asked: Arc<Mutex<Vec<Value>>>,
}

impl letibot_plugins::Module for Links {
    fn hooks(&self) -> Vec<String> {
        self.hooks.clone()
    }

    fn call(&self, _hook: &str, input: &str) -> Result<Option<String>, String> {
        let input: Value = serde_json::from_str(input).map_err(|e| e.to_string())?;
        self.asked.lock().unwrap().push(input.clone());
        let words: Vec<String> = input["words"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        match self.mode {
            Mode::Boards => {
                let urls: Vec<Option<String>> = words
                    .iter()
                    .map(|w| {
                        w.strip_prefix("LET-")
                            .map(|n| format!("https://board.example/t/{n}"))
                    })
                    .collect();
                Ok(Some(json!({"urls": urls}).to_string()))
            }
            Mode::Trap => Err("trapped: error while executing at wasm backtrace".to_string()),
            Mode::Nonsense => Ok(Some(
                json!({"urls": words.iter().map(|_| "javascript:alert(1)").collect::<Vec<_>>()})
                    .to_string(),
            )),
        }
    }

    fn fuel(&self) -> u64 {
        7
    }
}

struct LinksLoader {
    mode: Mode,
    asked: Arc<Mutex<Vec<Value>>>,
}

impl letibot_plugins::Loader for LinksLoader {
    fn load(
        &self,
        wasm: &Path,
        _caps: &[letibot_plugins::Capability],
    ) -> Result<Box<dyn letibot_plugins::Module>, String> {
        let _ = std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
        let dir = wasm.parent().unwrap_or(Path::new("."));
        let manifest = letibot_plugins::Manifest::read(dir)?;
        Ok(Box::new(Links {
            hooks: manifest.hooks.clone(),
            mode: self.mode,
            asked: self.asked.clone(),
        }))
    }
}

/// A project with one placed `link_gh` layer, and the temp directory it lives in.
fn a_project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "letibot-link-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let layer = dir.join(".letibot").join("plugins").join("board");
    std::fs::create_dir_all(&layer).expect("the layer's directory");
    std::fs::write(
        layer.join("plugin.toml"),
        "name = \"board\"\npurpose = \"link this project's ticket ids\"\nhooks = [\"link_gh\"]\n\
         capabilities = []\ncontract = 1\n",
    )
    .expect("a manifest");
    std::fs::write(layer.join("plugin.wasm"), b"\0asm").expect("a module");
    dir
}

/// A head over that project, with the layer in its stack, and the inputs the layer was handed.
fn a_head_over(project: &Path, mode: Mode) -> (App, Arc<Mutex<Vec<Value>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut stack = letibot_plugins::Stack::discover(
        None,
        Some(project),
        None,
        Arc::new(LinksLoader {
            mode,
            asked: asked.clone(),
        }),
    );
    stack.set_trust(letibot_plugins::Trust::open(
        project.join("plugin-trust.json"),
    ));
    let mut a = app();
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));
    // The disk is not read for the question *is a link layer here at all*: this head has one, and
    // `any_link_layer` is what a real head asks before it compiles a stack (its own test is below).
    a.link_layers = Some(true);
    // Resolved, so the head does not ask the disk again inside the test's frames.
    a.link_layers_at = u64::MAX;
    a.set_features(rano::term::Features::ALL);
    a.gh_refs = Some(std::sync::Arc::new(crate::app::ghlinks::GitRefs {
        slug: "stroppy-io/stroppy".into(),
        branches: Default::default(),
        commits: Vec::new(),
    }));
    (a, asked)
}

/// A head whose transcript says `text`, and whose screen is drawn once.
fn a_frame_over(project: &Path, mode: Mode, text: &str) -> (App, Arc<Mutex<Vec<Value>>>) {
    let (mut a, asked) = a_head_over(project, mode);
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("r.1", "assistant"),
    )));
    a.record_item(
        "r.1",
        TranscriptItem::Assistant {
            text: text.into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    (a, asked)
}

/// Every URL on the frame, in order.
fn urls(screen: &str) -> Vec<String> {
    screen
        .split("\u{1b}]8;;")
        .skip(1)
        .filter_map(|rest| rest.split_once("\u{1b}\\").map(|(u, _)| u.to_string()))
        .filter(|u| !u.is_empty())
        .collect()
}

/// The words a call was asked about.
fn words_of(input: &Value) -> Vec<String> {
    input["words"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Three ticket ids on one frame are ONE question: the words are gathered across the frame and the
/// layer is asked once for all of them, and a word it has already been asked about is not asked
/// again on the next frame (§4.6: batched per frame, cached by word).
#[test]
fn link_words_are_batched_per_frame() {
    let project = a_project("batch");
    let (mut a, asked) = a_frame_over(&project, Mode::Boards, "see LET-1, LET-2 and LET-3.");
    let screen = a.screen(120, 20).join("\n");
    let linked = urls(&screen);
    for want in ["1", "2", "3"] {
        assert!(
            linked.contains(&format!("https://board.example/t/{want}")),
            "`LET-{want}` is linked: {linked:?}\n{screen}"
        );
    }
    let calls = asked.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "one call for the frame, not one per word");
    let words = words_of(&calls[0]);
    for want in ["LET-1", "LET-2", "LET-3"] {
        assert!(words.contains(&want.to_string()), "{want} in {words:?}");
    }
    assert_eq!(calls[0]["repo"]["slug"], json!("stroppy-io/stroppy"));

    // The same screen again: every word is known now, so nothing new is asked.
    let _ = a.screen(120, 20);
    assert_eq!(
        asked.lock().unwrap().len(),
        1,
        "a word already answered is cached, and a frame asks for news only"
    );
    let _ = std::fs::remove_dir_all(&project);
}

/// A layer that traps links nothing, and the words stay the text they were: the frame is drawn as
/// it was before the layer existed, and the head keeps the sentence why.
#[test]
fn a_trapping_link_layer_leaves_the_words_as_text() {
    let project = a_project("trap");
    let (mut a, asked) = a_frame_over(&project, Mode::Trap, "see LET-1 and LET-2.");
    let screen = a.screen(120, 20).join("\n");
    assert!(
        urls(&screen).is_empty(),
        "a trap links nothing: {:?}",
        urls(&screen)
    );
    assert!(screen.contains("LET-1"), "and the words are still text");
    assert_eq!(
        asked.lock().unwrap().len(),
        1,
        "it was asked, and the ask is what trapped"
    );
    let said = a.take_sessions_failure().unwrap_or_default();
    assert!(said.contains("link_gh"), "{said}");
    assert!(said.contains("trapped"), "{said}");
    let _ = std::fs::remove_dir_all(&project);
}

/// An answer the head will not draw - a scheme a terminal cannot be asked about - is refused whole,
/// so no word of that frame is linked by half an answer.
#[test]
fn a_link_answer_with_an_undrawable_scheme_links_nothing() {
    let project = a_project("nonsense");
    let (mut a, _asked) = a_frame_over(&project, Mode::Nonsense, "see LET-1.");
    let screen = a.screen(120, 20).join("\n");
    assert!(urls(&screen).is_empty(), "{:?}", urls(&screen));
    let said = a.take_sessions_failure().unwrap_or_default();
    assert!(
        said.contains("http"),
        "the refusal names the schemes: {said}"
    );
    let _ = std::fs::remove_dir_all(&project);
}

/// A head with no `link_` layer on disk reads manifests and no modules: the feature costs nothing
/// where nobody wrote a layer for it.
#[test]
fn a_head_with_no_link_layer_builds_no_stack_for_links() {
    let project = a_project("none");
    assert!(
        crate::app::sessions_pane::Host::any_link_layer("s-none", &project),
        "the fixture's layer is one"
    );
    // A project with only layers of other kinds is not one.
    let other = std::env::temp_dir().join(format!("letibot-link-other-{}", std::process::id()));
    let layer = other.join(".letibot").join("plugins").join("plan");
    std::fs::create_dir_all(&layer).expect("the layer's directory");
    std::fs::write(
        layer.join("plugin.toml"),
        "name = \"plan\"\npurpose = \"a dashboard\"\nhooks = [\"pane_plan\"]\n\
         capabilities = []\ncontract = 1\n",
    )
    .expect("a manifest");
    assert!(
        !crate::app::sessions_pane::Host::any_link_layer("s-none", &other),
        "a pane layer is not a link layer"
    );
    let _ = std::fs::remove_dir_all(&project);
    let _ = std::fs::remove_dir_all(&other);
}
