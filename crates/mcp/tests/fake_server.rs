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

mod support;

use support::{PATIENCE, wait_until};

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

/// A stdio server is a process this box started, so its tools carry `Exec` into the gate: the
/// class of a command here, not of a reach outside. An HTTP server's stay `Network`, and
/// `tests/http_server.rs` says so.
#[test]
fn a_stdio_servers_tools_are_classed_exec() {
    let cat = Catalog::start(&[fake("fake", &[])]);
    assert!(cat.failures().is_empty(), "{:?}", cat.failures());
    assert_eq!(
        cat.servers()[0].access,
        letibot_tools::schema::Access::Exec,
        "a local server's call runs here"
    );
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
            "echo", "big", "fail", "crash", "grow", "pwd", "slow", "env", "spawn", "pid"
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
    let failures = cat.failures();
    let failed: Vec<&str> = failures.iter().map(|f| f.server.as_str()).collect();
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

    // The restart's backoff is 500 ms and doubles, so ask until it has passed rather than sleep
    // through it: a call inside the window spawns nothing and answers with the time left.
    let r = {
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            match cat.call("fake", "echo", &serde_json::json!({"text": "up"})) {
                Ok(r) => break r,
                Err(e) => assert!(
                    std::time::Instant::now() < deadline,
                    "never restarted: {e:?}"
                ),
            }
        }
    };
    assert_eq!(r.text, "up");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A server that did not start is retried on the ladder a died one uses, on the next call: the
/// alternative is tools missing until somebody restarts the daemon.
#[test]
fn a_server_that_failed_at_start_is_retried_without_a_restart() {
    let dir = std::env::temp_dir().join(format!("letibot-mcp-startretry-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("started");
    // `--once` refuses to run while the marker is there, which is a start that fails for a reason
    // the client can do nothing about and the test can remove.
    std::fs::write(&marker, b"").unwrap();
    let cat = Catalog::start(&[fake("fake", &["--once", marker.to_str().unwrap()])]);
    let failures = cat.failures();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].why.contains("started once already"),
        "{failures:?}"
    );
    assert!(cat.servers().is_empty(), "a failed server offers nothing");

    std::fs::remove_file(&marker).unwrap();
    // No reconcile, no restart verb: the call itself is what brings it back.
    let mut text = String::new();
    wait_until("the server that failed to start to come up", || {
        match cat.call("fake", "echo", &serde_json::json!({"text": "up"})) {
            Ok(r) => {
                text = r.text;
                true
            }
            Err(e) => {
                text = format!("{e:?}");
                false
            }
        }
    });
    assert_eq!(text, "up");
    assert!(cat.failures().is_empty(), "{:?}", cat.failures());
    // The server's own tools, and not a count: the fake grows a tool whenever a test needs one
    // (`write` arrived after this), so name the one this test calls.
    let listed: Vec<String> = cat.servers()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert!(
        listed.contains(&"echo".to_string()),
        "listed on the retry: {listed:?}"
    );
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
    wait_until("the tools to be listed again", || {
        names(&cat).contains(&"extra".to_string())
    });
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

#[test]
fn an_mcp_server_endless_line_is_dropped() {
    let mut spec = fake("fake", &["--endless"]);
    spec.timeout = Duration::from_secs(10);
    let started = std::time::Instant::now();
    let cat = Catalog::start(&[spec]);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "waited {:?} on a line with no end",
        started.elapsed()
    );
    let failures = cat.failures();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].why.contains("bytes with no newline"),
        "{failures:?}"
    );
}

/// A call whose answer does not come back is cancelled in the protocol's own words, and told to the
/// model as *may have run*: the call is out, so repeating it is a second run and not a retry.
#[test]
fn a_timed_out_mcp_call_is_cancelled_not_retried_blind() {
    let dir = std::env::temp_dir().join(format!("letibot-mcp-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("cancelled");
    let _ = std::fs::remove_file(&log);
    let mut spec = fake("slowpoke", &["--cancel-log", log.to_str().unwrap()]);
    spec.timeout = Duration::from_millis(300);
    let cat = Catalog::start(&[spec]);
    assert!(cat.failures().is_empty(), "{:?}", cat.failures());
    let why = match cat.call("slowpoke", "slow", &serde_json::json!({"ms": 900})) {
        Err(letibot_tools::builtins::external::mcp::McpError::MayHaveRun { why }) => why,
        other => panic!("not a may-have-run: {other:?}"),
    };
    // The server answers at 900 ms and reads the note after, so the id arrives a moment behind the
    // timeout; the pairing is that the id in the log is the one the error names.
    let mut logged = String::new();
    wait_until("the cancellation to reach the server", || {
        logged = std::fs::read_to_string(&log).unwrap_or_default();
        !logged.trim().is_empty()
    });
    let id = logged.trim();
    assert!(!id.is_empty(), "no cancellation reached the server: {why}");
    assert!(
        why.contains(&format!("request {id} ")),
        "the cancelled id `{id}` is the request the error names: {why}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// What the model is told: the tool may have run. `did not run` is the sentence a timeout may never
/// produce here, because the answer is what never came back and the call went out.
#[test]
fn a_timed_out_call_reaches_the_model_as_may_have_run() {
    let mut spec = fake("fake", &[]);
    spec.timeout = Duration::from_millis(300);
    let mut h = harness(Arc::new(Catalog::start(&[spec])), allow_all_mcp());
    let r = h.call(
        "mcp_call",
        r#"{"tool":"fake__slow","arguments":{"ms":900}}"#,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    let said = r.render();
    assert!(said.contains("may have run"), "{said}");
    assert!(!said.contains("did not run"), "{said}");
}
