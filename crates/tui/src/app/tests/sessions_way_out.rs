//! **The way out** — `docs/plugins-plan.md` §7 step 10's last paragraph: *"the session picker is
//! the way out of a broken session, so it must not depend on a plugin"*.
//!
//! Every other pane may fail into a sentence on the screen. This one cannot: a person whose
//! session is wedged has exactly one keystroke left, and it opens this list. So the list is drawn
//! from three sources, and each one catches the one above:
//!
//! | the stack says | what is drawn |
//! |---|---|
//! | a layer answers | the layer's list (`sessions_list`) |
//! | nothing answers — no layer, a trap, fuel, a view the host refuses | the head's own list (`session_picker`) |
//!
//! The tests here stand each failure in, and each of them also presses the keys: a list that draws
//! and does not switch is not a way out.

use super::*;
use letibot_plugins::{Capability, Loader, Module, Stack};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A module that fails every call — a layer that traps, or runs out of fuel: from the stack's side
/// of the seam the two are one thing (`docs/plugins-plan.md` §1, principle 4).
struct Trap;

impl Module for Trap {
    fn hooks(&self) -> Vec<String> {
        vec!["pane_sessions".to_string()]
    }

    fn call(&self, _hook: &str, _input: &str) -> Result<Option<String>, String> {
        Err("the layer trapped".to_string())
    }

    fn fuel(&self) -> u64 {
        0
    }
}

/// **A loader that traps anything on disk and answers nothing at all**: the built-ins come through
/// the real engine (so the list below the broken layer is the real one), and every file-backed
/// layer fails. That is the measured shape of a layer whose Rust does not compile into a working
/// module.
struct TrapOnDisk {
    real: crate::app::sessions_pane::RanoLoader,
}

impl Loader for TrapOnDisk {
    fn load(&self, _wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        Ok(Box::new(Trap))
    }

    fn load_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        caps: &[Capability],
    ) -> Result<Box<dyn Module>, String> {
        self.real.load_bytes(name, bytes, caps)
    }
}

/// A project layer directory, as a person's `edit` would leave it: a `plugin.toml` that declares
/// the feed and the acts, and a `plugin.wasm` the loader refuses.
fn project_layer(root: &Path) {
    let dir = root.join(".letibot").join("plugins").join("sessions");
    std::fs::create_dir_all(&dir).expect("the project's plugin dir");
    std::fs::write(
        dir.join("plugin.toml"),
        "name = \"sessions\"\n\
         purpose = \"a project's own session list, which is broken on purpose here\"\n\
         hooks = [\"pane_sessions\"]\n\
         capabilities = []\n\
         feeds = [\"sessions\"]\n\
         actions = [\"switch\", \"fold\"]\n\
         contract = 1\n",
    )
    .expect("a manifest");
    std::fs::write(dir.join("plugin.wasm"), b"not a module").expect("a module");
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "letibot-way-out-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// A daemon with one conversation the head is standing in and one it could go to.
fn a_head_with_two_sessions() -> App {
    let mut a = app();
    let mut other = brief("s-other", "the rust rewrite", false);
    other.live = true;
    a.apply(hello(
        "s-here",
        vec![brief("s-here", "the habr article", false), other],
        Hub::new("s-here").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    a
}

/// **A layer that traps is skipped and the built-in below it answers** — the list is still the
/// list, and the person is TOLD what happened to their layer when they open it.
#[test]
fn a_layer_that_traps_falls_down_to_the_builtin() {
    let tmp = tempdir("trap");
    project_layer(&tmp);
    let mut stack = Stack::discover(
        None,
        Some(&tmp),
        None,
        Arc::new(TrapOnDisk {
            real: crate::app::sessions_pane::RanoLoader,
        }),
    );
    let failed = letibot_builtins::install(&mut stack);
    assert!(
        failed.is_empty(),
        "the built-ins themselves load: {failed:?}"
    );

    let mut a = a_head_with_two_sessions();
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));

    // The list is drawn — by the built-in, below the layer that failed.
    let list = a
        .sessions_list(100)
        .expect("the built-in answers under the trap");
    let drawn = crate::ui::render::row_strings(&list.content.lines, a.cfg.palette());
    assert!(
        drawn.iter().any(|l| l.contains("the habr article")),
        "{drawn:?}"
    );
    assert!(
        drawn.iter().any(|l| l.contains("the rust rewrite")),
        "{drawn:?}"
    );

    // **And the failure is said**, once, in the words of the layer that met it.
    let said = a
        .take_sessions_failure()
        .expect("a refused layer is news the person is owed");
    assert!(said.contains("the layer trapped"), "{said}");
    assert!(said.contains("drawn from below it"), "{said}");
    assert!(a.take_sessions_failure().is_none(), "and only once");

    // **Switching still works**: the second session, which is the third row.
    a.picker_sel = 2;
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Switch("s-other".into())),
        "a broken layer must not cost the list its one job"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// **No layer at all — a head whose built-in table could not be built — draws the head's own
/// list**, through the same rano primitive, and the keys act on the same rows.
#[test]
fn a_broken_builtin_falls_to_the_plain_list() {
    let tmp = tempdir("empty");
    // An empty stack: no project, no session, no user scope, and no built-in installed — the
    // binary that somehow carries nothing, which is the shape this fallback exists for.
    let stack = Stack::discover(
        None,
        Some(&tmp),
        None,
        Arc::new(crate::app::sessions_pane::RanoLoader),
    );
    let mut a = a_head_with_two_sessions();
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));

    assert!(
        a.sessions_list(100).is_none(),
        "there is no layer to answer, which is what the way out is for"
    );
    let plain = a.picker_lines(100);
    assert!(
        plain.iter().any(|l| l.contains("the habr article")),
        "the head's own list is drawn: {plain:?}"
    );
    // And it is the SAME drawing the head would draw with no plugin at all.
    let own =
        crate::ui::render::row_strings(&a.session_picker().content(100).lines, a.cfg.palette());
    assert_eq!(plain, own);

    // **The keys act on the rows that were drawn.** One line per session, two sessions: the
    // second row is the second session.
    a.picker_sel = 1;
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Switch("s-other".into())),
        "the plain list switches like any other"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// **A layer that answers something the host cannot draw is treated as no answer**, and the head's
/// own list is drawn — the same rule as a trap, one step later in the pipeline.
#[test]
fn an_answer_that_is_not_a_list_falls_to_the_plain_list() {
    /// A layer that answers a document with no tree in it: a view, but not one this pane can use.
    struct NotAList;
    impl Module for NotAList {
        fn hooks(&self) -> Vec<String> {
            vec!["pane_sessions".to_string()]
        }
        fn call(&self, _hook: &str, _input: &str) -> Result<Option<String>, String> {
            let answer: Value = serde_json::json!({"title": "a summary and no rows"});
            Ok(Some(answer.to_string()))
        }
        fn fuel(&self) -> u64 {
            0
        }
    }
    struct Layers;
    impl Loader for Layers {
        fn load(&self, _wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
            Ok(Box::new(NotAList))
        }
    }
    let tmp = tempdir("not-a-list");
    project_layer(&tmp);
    let stack = Stack::discover(None, Some(&tmp), None, Arc::new(Layers));

    let mut a = a_head_with_two_sessions();
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));
    assert!(a.sessions_list(100).is_none(), "no tree, so no list");
    assert!(
        a.picker_lines(100)
            .iter()
            .any(|l| l.contains("the habr article")),
        "and the head's own list is what is drawn"
    );
    a.picker_sel = 1;
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-other".into())));
    std::fs::remove_dir_all(&tmp).ok();
}
