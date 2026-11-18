//! A session owns what its own commands create. A child ran `git worktree add` inside a container
//! and every subpath of the tree it had just built was refused, one card per path: the boundary was
//! asking about the session's own work. A tree that was not there before the call and is after it,
//! under a place the command could write, joins the session's read-write view with no card.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use letibot_tools::exec::confine::ViewSpec;
use letibot_tools::runtime::{Gate, GateCall, GateDecision, ViewGrant};
use letibot_tools::testing::carrying_view_harness;
use letibot_tools::view::ViewAccess;
use letibot_transcript::ToolOutcome;

/// A gate that admits every call and COUNTS the view questions it was asked: the claim this test
/// makes is that the session's own work asks nothing, so a count of what was asked is the assertion.
struct NeverAsked {
    views: Arc<AtomicUsize>,
}

impl Gate for NeverAsked {
    fn admit(&mut self, _call: &GateCall<'_>) -> GateDecision {
        GateDecision::Admit
    }

    fn grant_view(&mut self, _path: &Path, _tool: &str, _access: ViewAccess) -> ViewGrant {
        self.views.fetch_add(1, Ordering::SeqCst);
        ViewGrant::NotAsked
    }

    fn describe(&self) -> String {
        "a fixture gate that admits and counts".into()
    }
}

/// The tree a command creates is the session's own: `git worktree add` through a container client
/// that was handed the place with `-v` leaves a directory that did not exist before the call, and a
/// write inside it must not raise a card.
///
/// The container client is a stub this test writes, because what the command LINE says is the fact
/// the rule reads: a `-v <place>:` handed to a `docker`-named program makes `<place>` a place the
/// command could write. The stub makes the directory the real client would have made.
#[test]
fn a_tree_the_sessions_own_command_created_raises_no_card() {
    let views = Arc::new(AtomicUsize::new(0));
    let mut h = carrying_view_harness(
        Box::new(NeverAsked {
            views: Arc::clone(&views),
        }),
        // The view is the project alone: the place the tree is made in is OUTSIDE it, which is the
        // whole situation - a container wrote where the session's own view could not reach.
        // (`carrying_view_harness` puts the project in `project/` under its temp dir for this.)
        |root| ViewSpec::project_only(root),
    );
    let temp = h.root().to_path_buf();
    let root = temp.join("project");
    let share = temp.join("share-for-a-container");
    std::fs::create_dir_all(&share).expect("the place the command could write");
    // A container client under a name the parse reads as one, which makes the tree the real client
    // would have made. Written inside the view, so the command can run it.
    let stub = root.join("bin/docker");
    std::fs::create_dir_all(stub.parent().expect("a bin")).expect("a bin");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\n# a container client, as far as this call is concerned\nmkdir -p {}\n",
            share.join("leakidx-wt").display()
        ),
    )
    .expect("a stub client");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("a mode");
    }

    let command = format!(
        "{} run --rm -v {}:/h alpine",
        stub.display(),
        share.display()
    );
    let r = h.call(
        "bash",
        &serde_json::json!({ "command": command }).to_string(),
    );
    let body = r.render();
    assert_eq!(r.outcome, ToolOutcome::Ok, "{body}");
    let tree = share.join("leakidx-wt");
    assert!(
        tree.is_dir(),
        "the command has to have created the tree this test is about: {body}"
    );
    assert!(
        body.contains("leakidx-wt"),
        "the row must say what the session created: {body}"
    );
    assert_eq!(
        views.load(Ordering::SeqCst),
        0,
        "a tree the session's own command made is not a view question: {body}"
    );

    // The other half, where a boundary can be re-probed: the tree is in the view, so a write inside
    // it needs no card either. A box whose bwrap cannot make a namespace cannot bind a new path into
    // a view at all, and says so rather than passing quietly.
    if body.contains("could not be bound") {
        eprintln!(
            "a_tree_the_sessions_own_command_created_raises_no_card: this box cannot bind a path \
             into a view, so only the recognition half was measured: {body}"
        );
        letibot_tokencore::apparatus::absent("a boundary that can be re-probed with a wider view");
        return;
    }
    let inside = tree.join("README");
    let r = h.call(
        "write",
        &serde_json::json!({ "path": inside.display().to_string(), "content": "x\n" }).to_string(),
    );
    assert_eq!(
        r.outcome,
        ToolOutcome::Ok,
        "a write inside the tree the session created must not be refused: {}",
        r.render()
    );
    assert_eq!(
        views.load(Ordering::SeqCst),
        0,
        "and it must not raise a card either: {}",
        r.render()
    );
}
