//! A stdio server starts inside the boundary its session hands the client, so a write outside
//! that view has no path to land on. Its own binary: a boundary is a process, and the paths are
//! this test's.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use letibot_mcp_client::{Catalog, ServerSpec, Transport};
use letibot_tools::builtins::external::mcp::McpCatalog;
use letibot_tools::exec::{
    Bwrap, Confined, Confinement, Egress, Literal, NoConfinement, Spawn, ViewSpec,
};
use serde_json::json;

fn fake_bin() -> String {
    if let Some(p) = option_env!("CARGO_BIN_EXE_letibot-mcp-fake") {
        return p.into();
    }
    let exe = std::env::current_exe().unwrap();
    let debug = exe.parent().and_then(|d| d.parent()).unwrap();
    debug.join("letibot-mcp-fake").display().to_string()
}

/// The server as a `[mcp.NAME]` table would resolve it, run in `cwd` the way a `.mcp.json`
/// server runs in its checkout.
fn spec_in(cwd: &std::path::Path) -> ServerSpec {
    ServerSpec {
        name: "fake".into(),
        transport: Transport::Stdio {
            command: fake_bin(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: Some(cwd.to_path_buf()),
        },
        timeout: Duration::from_secs(30),
    }
}

fn scratch(test: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("letibot-mcp-sandbox-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_sandboxed_stdio_server_cannot_write_outside_its_view() {
    let root = scratch("writes");
    let project = root.join("project");
    let outside = root.join("outside");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    // Measured, not asked for: a boundary this kernel cannot build is one the server would run
    // outside of, and that is the whole fact this test is about.
    let bwrap = match Bwrap::probe(ViewSpec::project_only(&project), Egress::Denied) {
        Ok(b) => b,
        Err(why) => {
            eprintln!("skipped: no boundary can be measured on this box: {why}");
            return;
        }
    };
    assert!(
        bwrap.boundary().is_some_and(|b| b.encloses()),
        "a boundary that does not enclose is no boundary: {}",
        bwrap.describe()
    );
    let spawn: Arc<dyn Spawn> = Arc::new(Confined(Arc::new(bwrap)));
    let cat = Catalog::start_with(&[spec_in(&project)], spawn);
    assert!(cat.failures().is_empty(), "{:?}", cat.failures());

    let inside = project.join("inside.txt");
    let ok = cat
        .call(
            "fake",
            "write",
            &json!({"path": inside.display().to_string(), "text": "hello"}),
        )
        .expect("the server answers");
    assert!(!ok.is_error, "{}", ok.text);
    assert_eq!(std::fs::read_to_string(&inside).unwrap(), "hello");

    let out = outside.join("out.txt");
    let refused = cat
        .call(
            "fake",
            "write",
            &json!({"path": out.display().to_string(), "text": "hello"}),
        )
        .expect("the server answers");
    assert!(
        refused.is_error,
        "a write outside the view succeeded: {}",
        refused.text
    );
    assert!(
        !out.exists(),
        "the file is on this filesystem, so the server was not inside the view"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_sandboxed_server_outlives_the_thread_that_started_it() {
    let root = scratch("thread");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let bwrap = match Bwrap::probe(ViewSpec::project_only(&project), Egress::Denied) {
        Ok(b) => b,
        Err(why) => {
            eprintln!("skipped: no boundary can be measured on this box: {why}");
            return;
        }
    };
    let spawn: Arc<dyn Spawn> = Arc::new(Confined(Arc::new(bwrap)));
    // The pool's own shape: the server is started on a worker thread that ends as soon as the
    // start returns. A `--die-with-parent` argv SIGKILLs the boundary there, because the parent
    // `PR_SET_PDEATHSIG` names is that thread and not the client that holds the server.
    let started_in = project.clone();
    let cat = std::thread::scope(|s| {
        s.spawn(move || Catalog::start_with(&[spec_in(&started_in)], spawn))
            .join()
            .expect("the start thread")
    });
    assert!(cat.failures().is_empty(), "{:?}", cat.failures());
    std::thread::sleep(Duration::from_millis(300));

    let inside = project.join("inside.txt");
    let ok = cat
        .call(
            "fake",
            "write",
            &json!({"path": inside.display().to_string(), "text": "hello"}),
        )
        .expect("the server answers");
    assert!(!ok.is_error, "{}", ok.text);
    assert_eq!(std::fs::read_to_string(&inside).unwrap(), "hello");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_session_with_no_boundary_starts_literally_and_one_that_wanted_one_refuses() {
    // Nobody asked for a boundary: the command is the command, and the key is every such
    // session's, which is what keeps one server shared by the sessions that are alike.
    let project = scratch("literal");
    assert_eq!(Literal.key(), "literal");
    assert_eq!(
        Confined(Arc::new(NoConfinement::new("why"))).key(),
        "literal"
    );

    // Asked for and refused: fail closed. A server that ran here would be the boundary silently
    // not being there, which is worse than no boundary at all.
    let asked = Confined(Arc::new(NoConfinement::new(
        "this session's boundary is not there",
    )));
    let cat = Catalog::start_with(&[spec_in(&project)], Arc::new(asked));
    let failures = cat.failures();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].why.contains("boundary is not there"),
        "the refusal is the boundary's own words: {}",
        failures[0].why
    );
    assert!(
        cat.servers().is_empty(),
        "nothing ran unconfined in its place"
    );
    let _ = std::fs::remove_dir_all(&project);
}
