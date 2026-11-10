//! The head's stack is built on a thread of its own (R175 item 5): the frame that first asks for
//! one draws the head's own list, and the layer's drawing appears on a later frame. Both halves are
//! here, and a real build is asked for rather than only the machinery, because a thread nobody ever
//! starts would leave the pane on the head's list for ever.

use super::*;
use crate::app::sessions_pane::{Host, HostBuild, RanoLoader};
use letibot_plugins::Stack;
use std::sync::Arc;
use std::time::Duration;

/// A head with one session of the operator's, and nothing on its stack yet. This file's head is
/// the one that does NOT build where it stands: the thread is what these two tests are about.
fn a_head_with_a_session() -> App {
    let mut a = app();
    a.host_inline.set(false);
    a.apply(hello(
        "s-head",
        vec![brief("s-head", "the head's own conversation", false)],
        Hub::new("s-head").snapshot(),
    ));
    a
}

/// The frame that asks for a stack does not wait for it, and draws the head's own list meanwhile;
/// the frame after the build lands asks the layer and draws its answer. The build is HELD open by
/// the test - a thread cannot be held still - and it is the real `Host`, with the binary's own
/// layers at the bottom, that is handed over.
#[test]
fn the_head_draws_its_first_frame_before_the_host_is_built() {
    let mut a = a_head_with_a_session();
    let workspace = a.workspace_path();
    let (tx, rx) = std::sync::mpsc::channel();
    *a.host_build.get_mut() = Some(HostBuild {
        workspace: workspace.clone(),
        settled: rx,
    });

    let first = a.picker_lines(100).join("\n");
    assert!(
        first.contains("the head's own conversation"),
        "the first frame is drawn, and from the head's own tree:\n{first}"
    );
    assert!(
        a.sessions_host.borrow().is_none(),
        "the frame did not wait for a stack to be built"
    );
    assert!(
        a.sessions_list(100).is_none(),
        "there is no layer to ask yet, so nothing plugin-driven is drawn"
    );

    // The build lands: a real stack, with the binary's layers at the bottom.
    let mut stack = Stack::discover(None, Some(&workspace), None, Arc::new(RanoLoader));
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "every built-in loads: {failed:?}");
    tx.send(Host::with_stack(stack, workspace))
        .expect("a receiver");
    let second = a.picker_lines(100).join("\n");
    assert!(
        a.sessions_host.borrow().is_some(),
        "the frame that followed took the stack the thread built"
    );
    assert!(
        a.sessions_list(100).is_some(),
        "and the pane is now the layer's drawing"
    );
    assert!(
        second.contains("the head's own conversation"),
        "which still names the session:\n{second}"
    );
}

/// And the build really is on another thread: the first frame returns with no stack and the work
/// in flight, and the stack it produces is the one the pane goes on to ask.
#[test]
fn the_head_asks_for_its_host_off_the_drawing_thread() {
    let mut a = a_head_with_a_session();
    // A folder of the test's own, so the build reads nothing of the operator's: absent directories
    // are empty scopes, and the binary's own layers are all this stack will hold.
    let workspace = std::env::temp_dir().join(format!(
        "letibot-head-build-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&workspace).expect("a folder");
    a.sessions[0].wiring.workspace = workspace.display().to_string();

    let _ = a.plugin_host();
    assert!(
        a.sessions_host.borrow().is_none(),
        "the first ask drew without a stack"
    );
    assert!(
        a.host_build.borrow().is_some(),
        "and the stack is being built somewhere else"
    );
    assert!(
        a.wait_for_host_build(Duration::from_secs(300)),
        "the build lands"
    );
    assert!(
        a.sessions_list(100).is_some(),
        "and the layer answers the pane"
    );
    std::fs::remove_dir_all(&workspace).ok();
}
