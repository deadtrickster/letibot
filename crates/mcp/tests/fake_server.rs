//! The client against a real child process: `letibot-mcp-fake`, a stdio MCP server.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use letibot_mcp_client::{Catalog, ServerSpec, Transport};
use letibot_tools::adjudicate::AdjudicatedGate;
use letibot_tools::builtins::external::mcp::McpCatalog;
use letibot_tools::builtins::external::scripted;
use letibot_tools::testing::external_harness_with_gate;
use letibot_tools::{ExternalBackends, MemoryStore, OutputCap, Spiller};
use letibot_transcript::ToolOutcome;

/// The fake server's path. Not `env!`: `cargo clippy` does not build the bin, so it is unset there.
fn fake_bin() -> String {
    if let Some(p) = option_env!("CARGO_BIN_EXE_letibot-mcp-fake") {
        return p.into();
    }
    let exe = std::env::current_exe().unwrap();
    let debug = exe.parent().and_then(|d| d.parent()).unwrap();
    debug.join("letibot-mcp-fake").display().to_string()
}

fn fake(name: &str, args: &[&str]) -> ServerSpec {
    ServerSpec {
        name: name.into(),
        transport: Transport::Stdio {
            command: fake_bin(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: BTreeMap::new(),
            cwd: None,
        },
        timeout: Duration::from_secs(20),
    }
}

fn harness(
    catalog: Arc<dyn McpCatalog>,
    rules: serde_json::Value,
) -> letibot_tools::testing::Harness {
    let rules = letibot_tools::permission::config_to_ruleset(rules.as_object().unwrap()).unwrap();
    let gate = AdjudicatedGate::closed().with_permission(rules);
    let mut h = external_harness_with_gate(
        ExternalBackends {
            mcp: catalog,
            ..scripted::attached()
        },
        Some(Box::new(gate)),
    );
    h.rt.spiller = Spiller::new(Box::new(OutputCap::default()), Box::new(MemoryStore::new()));
    h
}

fn allow_all_mcp() -> serde_json::Value {
    serde_json::json!({"mcp_search": "allow", "mcp_call": {"fake__*": "allow"}})
}

#[test]
fn tools_are_listed_searched_and_called() {
    let cat = Catalog::start(&[fake("fake", &[])]);
    assert!(cat.failures().is_empty(), "{:?}", cat.failures());
    let tools: Vec<String> = cat.servers()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(
        tools,
        [
            "echo", "big", "fail", "crash", "grow", "pwd", "env", "spawn"
        ]
    );
    let mut h = harness(Arc::new(cat), allow_all_mcp());

    let r = h.call("mcp_search", r#"{"query":"echo"}"#);
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    assert!(r.payload.contains("fake__echo"), "{}", r.payload);
    assert!(!r.payload.contains("fake__big"), "{}", r.payload);

    let r = h.call(
        "mcp_call",
        r#"{"tool":"fake__echo","arguments":{"text":"hello there"}}"#,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    assert!(r.payload.contains("hello there"), "{}", r.payload);
    assert!(r.payload.contains("UNTRUSTED"), "{}", r.payload);

    let r = h.call("mcp_call", r#"{"tool":"fake__fail"}"#);
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    assert!(r.payload.contains("it broke"), "{}", r.payload);

    let r = h.call("mcp_call", r#"{"tool":"fake__nope"}"#);
    assert!(
        matches!(r.outcome, ToolOutcome::NotRun { .. }),
        "{:?}",
        r.outcome
    );
}

#[test]
fn a_large_result_spills_under_the_generic_cap() {
    let mut h = harness(
        Arc::new(Catalog::start(&[fake("fake", &[])])),
        allow_all_mcp(),
    );
    let r = h.call(
        "mcp_call",
        r#"{"tool":"fake__big","arguments":{"bytes":200000}}"#,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    assert!(r.spill.is_some(), "200 KB must spill");
    assert!(r.payload.len() < 120_000, "{}", r.payload.len());
    assert!(
        r.payload.contains("read_spill"),
        "the seam names the locator"
    );
}

#[test]
fn a_server_that_fails_to_start_is_reported_and_the_rest_start() {
    let cat = Catalog::start(&[
        fake("dead", &["--die-at-start"]),
        fake("fake", &[]),
        ServerSpec {
            name: "missing".into(),
            transport: Transport::Stdio {
                command: "/nonexistent/letibot-mcp-server".into(),
                args: vec![],
                env: BTreeMap::new(),
                cwd: None,
            },
            timeout: Duration::from_secs(5),
        },
    ]);
    let failed: Vec<&str> = cat.failures().iter().map(|f| f.server.as_str()).collect();
    assert_eq!(failed, ["dead", "missing"]);
    assert!(
        cat.failures()[0].why.contains("refusing to start"),
        "{:?}",
        cat.failures()
    );
    assert!(
        cat.failures()[1].why.contains("did not start"),
        "{:?}",
        cat.failures()
    );
    assert_eq!(cat.servers().len(), 1);
    assert!(
        cat.describe().contains("failed to start: dead, missing"),
        "{}",
        cat.describe()
    );
}

#[test]
fn a_server_that_crashes_mid_call_is_reported_and_restarted_on_the_next_call() {
    let mut h = harness(
        Arc::new(Catalog::start(&[fake("fake", &[])])),
        allow_all_mcp(),
    );
    let r = h.call("mcp_call", r#"{"tool":"fake__crash"}"#);
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    let said = r.render();
    assert!(said.contains("exited"), "{said}");
    assert!(said.contains("crashing on purpose"), "{said}");
    assert!(said.contains("restarted on the next call"), "{said}");
    let r = h.call(
        "mcp_call",
        r#"{"tool":"fake__echo","arguments":{"text":"back"}}"#,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    assert!(r.payload.contains("back"), "{}", r.payload);
}

#[test]
fn a_restart_that_fails_waits_out_its_backoff_before_the_next() {
    let dir = std::env::temp_dir().join(format!("letibot-mcp-once-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("started");
    let _ = std::fs::remove_file(&marker);
    let cat = Catalog::start(&[fake("fake", &["--once", marker.to_str().unwrap()])]);
    assert!(cat.failures().is_empty(), "{:?}", cat.failures());
    assert!(cat.call("fake", "crash", &serde_json::json!({})).is_err());

    // The marker is still there, so the restart fails and arms the 500 ms rung.
    let e = cat
        .call("fake", "echo", &serde_json::json!({}))
        .unwrap_err();
    let said = format!("{e:?}");
    assert!(said.contains("did not restart"), "{said}");
    assert!(said.contains("started once already"), "{said}");
    assert!(said.contains("next restart is in 0.5s"), "{said}");
    std::fs::remove_file(&marker).unwrap();
    let e = cat
        .call("fake", "echo", &serde_json::json!({}))
        .unwrap_err();
    assert!(
        format!("{e:?}").contains("the next restart is in"),
        "inside the backoff nothing is spawned: {e:?}"
    );
    assert!(!marker.exists(), "no start was tried");

    std::thread::sleep(Duration::from_millis(600));
    let r = cat
        .call("fake", "echo", &serde_json::json!({"text": "up"}))
        .unwrap();
    assert_eq!(r.text, "up");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_list_changed_notice_lists_the_tools_again() {
    let cat = Arc::new(Catalog::start(&[fake("fake", &[])]));
    let names = |c: &Catalog| -> Vec<String> {
        c.servers()[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect()
    };
    assert!(!names(&cat).contains(&"extra".to_string()));
    cat.call("fake", "grow", &serde_json::json!({})).unwrap();
    // The notice follows the answer on the pipe, so it is read on a later listing.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !names(&cat).contains(&"extra".to_string()) {
        assert!(
            std::time::Instant::now() < deadline,
            "never relisted: {:?}",
            names(&cat)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut h = harness(cat, allow_all_mcp());
    let r = h.call("mcp_call", r#"{"tool":"fake__extra"}"#);
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    assert!(r.payload.contains("extra ran"), "{}", r.payload);
}

#[test]
fn a_stdio_server_runs_in_its_cwd() {
    let dir = std::env::temp_dir().join(format!("letibot-mcp-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut spec = fake("fake", &[]);
    if let Transport::Stdio { cwd, .. } = &mut spec.transport {
        *cwd = Some(dir.clone());
    }
    let cat = Catalog::start(&[spec]);
    let r = cat.call("fake", "pwd", &serde_json::json!({})).unwrap();
    assert_eq!(
        std::path::Path::new(&r.text).canonicalize().unwrap(),
        dir.canonicalize().unwrap()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_call_not_preapproved_goes_to_the_gate_and_does_not_run() {
    // Only `fake__echo` is preapproved; with nobody to ask, the closed gate refuses the rest.
    let rules = serde_json::json!({"mcp_search": "allow", "mcp_call": {"fake__echo": "allow"}});
    let mut h = harness(Arc::new(Catalog::start(&[fake("fake", &[])])), rules);
    let r = h.call(
        "mcp_call",
        r#"{"tool":"fake__echo","arguments":{"text":"ok"}}"#,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    let r = h.call("mcp_call", r#"{"tool":"fake__crash"}"#);
    assert!(
        matches!(r.outcome, ToolOutcome::NotRun { .. }),
        "{:?}",
        r.outcome
    );
    // The refused call never reached the server, so it is still up.
    let r = h.call(
        "mcp_call",
        r#"{"tool":"fake__echo","arguments":{"text":"still"}}"#,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
}
