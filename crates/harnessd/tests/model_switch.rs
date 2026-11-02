//! `/models local.glm` from a daemon started on other weights - a switch replaces the
//! vocabulary, the dialect and the address of a live session instead of needing a daemon
//! restarted on those weights. Three tiers, because the parts fail for different reasons:
//! the decision (`local_switch_decision`) against a stub `/props` is pure and needs no
//! GGUF; the session switch (`Harness::set_local_model`) needs the daemon's own vocabulary;
//! and the full cross-weights switch needs both GGUFs, being the one run that proves a
//! second `Vocab::load` in one process. The stub answers `/props` and nothing else - never
//! `/completion` - so nothing in this file can put load on a model server:

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;

use letibot_harnessd::config::Config;
use letibot_harnessd::harness::{LocalSwitch, local_switch_decision};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_provider::keys::{LocalModel, ModelProfile};
use letibot_sessionlog::hub::Hub;
use letibot_tokencore::apparatus;

/// The GLM this fleet hosts, as the gguf the live tier needs on this box. An explicit
/// `LETIBOT_GLM_GGUF` wins, matching `apparatus::gguf_path`'s rule that an override is not a hint.
fn glm_gguf() -> PathBuf {
    if let Ok(p) = std::env::var("LETIBOT_GLM_GGUF") {
        return PathBuf::from(p);
    }
    std::env::var_os("HOME")
        .map(|h| {
            PathBuf::from(h)
                .join("models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf")
        })
        .unwrap_or_else(|| {
            PathBuf::from(
                "/home/dead/models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf",
            )
        })
}

/// A `/props` that answers on a local socket.
fn props_stub(model_path: Option<&str>, chat_template: Option<&str>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback socket");
    let port = listener.local_addr().unwrap().port();
    let mut body = String::from(
        r#"{"modalities":{"vision":false},"default_generation_settings":{"n_ctx":57344}"#,
    );
    if let Some(p) = model_path {
        body.push_str(&format!(r#","model_path":"{}""#, json_escape(p)));
    }
    if let Some(t) = chat_template {
        body.push_str(&format!(r#","chat_template":"{}""#, json_escape(t)));
    }
    body.push('}');
    std::thread::spawn(move || {
        // Every request the switch can make is a GET /props with `Connection: close`, and a switch
        // makes at most three, so the loop is bounded and the thread cannot outlive a wedged test
        // by much.
        for _ in 0..8 {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf); // the request head; the client never sends a body
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes());
            let _ = sock.write_all(body.as_bytes());
            let _ = sock.shutdown(std::net::Shutdown::Both);
        }
    });
    port
}

/// The escape set a JSON encoder emits and `serving::json_string` decodes.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The GLM dialect's own shipped template, trailing newline and all - the one divergence the
/// comparison trims.
fn glm_template() -> String {
    Dialect::Glm
        .wiring(None)
        .spec()
        .template
        .clone()
        .into_owned()
}

fn qwen_template() -> String {
    Dialect::Qwen
        .wiring(None)
        .spec()
        .template
        .clone()
        .into_owned()
}

/// A declared local model, as `providers.toml` would have given it to us.
fn model(name: &str, port: u16, dialect: Option<&str>) -> LocalModel {
    LocalModel {
        name: name.to_string(),
        url: format!("http://127.0.0.1:{port}"),
        model: name.to_string(),
        profile: ModelProfile {
            dialect: dialect.map(str::to_string),
            ..ModelProfile::default()
        },
    }
}

/// A daemon's own vocabulary, as the decision sees it: a path whose BASENAME is what `/props` is
/// compared against, exactly as `local_switch_decision` does.
const OWN_GGUF: &str =
    "/home/dead/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf";

// --- the decision, offline ------------------------------------------------

/// The dialect is what the target SAYS it renders, not a table of names - a daemon asked for
/// the local GLM derives glm from the served template, not from the basename.
#[test]
fn the_dialect_comes_from_what_the_target_says_it_renders() {
    let port = props_stub(
        Some("/m/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf"),
        Some(&glm_template()),
    );
    let got = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", port),
        &model("local.glm", port, None),
        Some(std::path::Path::new(OWN_GGUF)),
    )
    .expect("other weights with a matching template switch");
    match got {
        LocalSwitch::OtherWeights { dialect, gguf, .. } => {
            assert_eq!(dialect, Dialect::Glm, "the served template IS glm's");
            assert_eq!(
                gguf,
                PathBuf::from("/m/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf"),
                "the GGUF is the one /props named, verbatim"
            );
        }
        other => panic!("a glm template on other weights is a switch, not {other:?}"),
    }
}

/// Same weights by basename need no dialect - the file name is the measurement, and same weights
/// carry the template this session already renders.
#[test]
fn same_weights_by_basename_switch_the_address_only() {
    // A DIFFERENT box serving the SAME first-shard file name, which is this fleet's own
    // arrangement: the 27B on two hosts.
    let port = props_stub(Some(OWN_GGUF), None);
    let got = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", port),
        &model("dense78", port, None),
        Some(std::path::Path::new(OWN_GGUF)),
    )
    .expect("same weights switch");
    assert_eq!(got, LocalSwitch::SameWeights);
}

/// `same_vocab = true` asserts the weights and skips the probe - the proof that it skips is the
/// endpoint: port 9 answers nothing, and a decision that probed it would have refused.
#[test]
fn same_vocab_asserts_the_weights_and_skips_the_probe() {
    let mut m = model("proxy", 9, None);
    m.profile.unknown.push("same_vocab".to_string());
    let got = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", 9),
        &m,
        Some(std::path::Path::new(OWN_GGUF)),
    )
    .expect("the operator's assertion is an answer");
    assert_eq!(got, LocalSwitch::SameWeights);
}

/// A template nobody drives is refused by name, with the block key that asserts a dialect by
/// hand - not guessed, because a guessed dialect is valid ids meaning other words.
#[test]
fn a_template_no_dialect_drives_is_refused_by_name() {
    let alien = "{# a template this tree ships no dialect for #}\n";
    let port = props_stub(Some("/m/other.gguf"), Some(alien));
    let why = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", port),
        &model("local.other", port, None),
        Some(std::path::Path::new(OWN_GGUF)),
    )
    .expect_err("an alien template cannot be driven");
    assert!(
        why.contains("matches no dialect this tree drives") && why.contains("dialect ="),
        "the refusal names what it looked at and the key that answers it: {why}"
    );
}

/// A template nobody matches with an assertion switches - the escape the `dialect =` key
/// exists for, honoured rather than filed away.
#[test]
fn an_asserted_dialect_answers_a_template_nobody_matches() {
    let alien = "{# overridden on the launch line #}\n";
    let port = props_stub(Some("/m/glm.gguf"), Some(alien));
    let got = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", port),
        &model("local.glm", port, Some("glm")),
        Some(std::path::Path::new(OWN_GGUF)),
    )
    .expect("the assertion is the operator's answer");
    assert!(
        matches!(
            got,
            LocalSwitch::OtherWeights {
                dialect: Dialect::Glm,
                ..
            }
        ),
        "{got:?}"
    );
}

/// An assertion that contradicts the served template is refused - the server's own template is
/// the one it renders with, so a block that disagrees with it is somebody's mistake, not a tie for
/// this code to break.
#[test]
fn an_assertion_that_contradicts_the_served_template_is_refused() {
    let port = props_stub(Some("/m/glm.gguf"), Some(&glm_template()));
    let why = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", port),
        &model("local.glm", port, Some("qwen")),
        Some(std::path::Path::new(OWN_GGUF)),
    )
    .expect_err("a contradiction is not a switch");
    assert!(
        why.contains("reports the glm dialect's own chat template")
            && why.contains("asserts `dialect = \"qwen\"`"),
        "the refusal prints both sides: {why}"
    );
}

/// An assertion nobody can read is refused with its value - a key whose value was dropped could
/// never be honoured, silently.
#[test]
fn an_unreadable_dialect_assertion_is_refused_with_its_value() {
    let port = props_stub(Some("/m/glm.gguf"), Some(&glm_template()));
    let why = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", port),
        &model("local.glm", port, Some("gpt")),
        Some(std::path::Path::new(OWN_GGUF)),
    )
    .expect_err("an unknown dialect name is a refusal");
    assert!(
        why.contains("not a dialect this tree drives") && why.contains("\"gpt\""),
        "{why}"
    );
}

/// A server that reports no template needs the assertion - the refusal names the missing fact
/// and the key that supplies it.
#[test]
fn a_target_without_a_template_needs_the_asserted_dialect() {
    let port = props_stub(Some("/m/glm.gguf"), None);
    let why = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", port),
        &model("local.glm", port, None),
        Some(std::path::Path::new(OWN_GGUF)),
    )
    .expect_err("no template and no assertion is not a switch");
    assert!(
        why.contains("did not report a chat_template") && why.contains("dialect ="),
        "{why}"
    );
}

/// A silent endpoint is refused by name, with the block key that asserts the weights by hand -
/// port 9 is the traditional nothing-listens-here.
#[test]
fn a_silent_endpoint_is_refused_by_name() {
    let why = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", 9),
        &model("dead.port", 9, None),
        Some(std::path::Path::new(OWN_GGUF)),
    )
    .expect_err("nothing answering is not a switch");
    assert!(
        why.contains("did not answer /props") && why.contains("same_vocab = true"),
        "the refusal names the endpoint's silence and the key that answers it: {why}"
    );
}

// --- the byte-vocabulary daemon: `None` ------------------------------------

/// A daemon with no vocabulary of its own still switches - to the weights the target itself
/// reports. Nothing can be `SameWeights` there, so the decision is the ordinary cross-weights
/// switch with the GGUF taken from the target's own `/props`, not a blanket refusal.
#[test]
fn no_vocabulary_of_its_own_switches_to_the_targets_reported_weights() {
    let port = props_stub(
        Some("/m/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf"),
        Some(&glm_template()),
    );
    let got = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", port),
        &model("local.glm", port, None),
        None,
    )
    .expect("a byte-vocabulary daemon can still take the switch");
    match got {
        LocalSwitch::OtherWeights { dialect, gguf, .. } => {
            assert_eq!(dialect, Dialect::Glm);
            assert_eq!(
                gguf,
                PathBuf::from("/m/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf"),
                "the weights are the ones the TARGET reported — there are none here \
                 to compare against, and none were invented"
            );
        }
        other => panic!("no vocabulary of this daemon's own means no SameWeights: {other:?}"),
    }
}

/// Not even `same_vocab = true` can make weights the same as none - the assertion
/// short-circuits only where there is a vocabulary to be the same as, so the probe still runs and
/// the switch lands on the target's own reported weights.
#[test]
fn same_vocab_cannot_assert_weights_a_byte_daemon_does_not_have() {
    let mut m = model("local.glm", 0, None);
    m.profile.unknown.push("same_vocab".to_string());
    let port = props_stub(
        Some("/m/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf"),
        Some(&glm_template()),
    );
    m.url = format!("http://127.0.0.1:{port}");
    let got = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", port),
        &m,
        None,
    )
    .expect("the probe decides, and it has an answer");
    assert!(
        matches!(got, LocalSwitch::OtherWeights { .. }),
        "the assertion cannot be honoured against no vocabulary: {got:?}"
    );
}

/// A refusal under `None` names the GGUF the target reported - the one weights this switch could
/// ever have seated. The stub answers a model_path and a template this tree drives no dialect for
/// so the refusal fires after the weights were known and must carry them.
#[test]
fn no_vocabulary_refusals_name_the_targets_reported_gguf() {
    let alien = "{# a template this tree ships no dialect for #}\n";
    let port = props_stub(Some("/m/other.gguf"), Some(alien));
    let why = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", port),
        &model("local.other", port, None),
        None,
    )
    .expect_err("an alien template cannot be driven from any daemon");
    assert!(
        why.contains("/m/other.gguf") && why.contains("no vocabulary of its own"),
        "the refusal says what is true and names the reported GGUF: {why}"
    );
}

/// And a target that names no model at all is refused with both absences - no weights here to
/// keep, none reported there to load - rather than with the `same_vocab` remedy, which has no
/// vocabulary to assert against.
#[test]
fn no_vocabulary_and_no_model_path_names_both_absences() {
    let port = props_stub(None, Some(&glm_template()));
    let why = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", port),
        &model("proxy", port, None),
        None,
    )
    .expect_err("neither side can name the weights");
    assert!(
        why.contains("named no model_path") && why.contains("no vocabulary of its own"),
        "{why}"
    );
}

// --- the session switch, offline but for the harness's own vocabulary ------

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "harnessd-model-switch-{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).expect("scratch");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    // The daemon's own vocabulary, named as a daemon would name it: `for_this_box` carries no
    // default, so the test states the GGUF its own gate (`vocab_gguf`, at each test) checked for -
    // the qwen one the harness must tokenize with.
    cfg.vocab_gguf = Some(apparatus::gguf_path());
    if let Ok(g) = std::env::var("LETIBOT_VOCAB_GGUF") {
        cfg.vocab_gguf = Some(g.into());
    }
    cfg
}

/// Other weights whose GGUF is not on this box are refused with the path - and the wording
/// proves the dialect step PASSED first: this stub serves glm's own template and a path no box has
/// so the refusal can only come from loading the vocabulary.
#[test]
fn other_weights_refuse_when_their_gguf_is_not_on_this_box() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("absent-gguf");
    let path = dir.path.join("sessions.db");
    let cfg = config(&path, "switch-absent-gguf");
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new("switch-absent-gguf");
    let mut h = Harness::open(&parts, cfg, hub).expect("the session must open");

    let port = props_stub(
        Some("/no/such/dir/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf"),
        Some(&glm_template()),
    );
    let why = h
        .set_local_model(&model("local.glm", port, None))
        .expect_err("the switch cannot tokenize for a file it cannot read");
    assert!(
        why.to_string().contains("not a file this box can read")
            && why
                .to_string()
                .contains("/no/such/dir/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf"),
        "the refusal names the GGUF /props reported: {why}"
    );
}

/// A same-weights switch moves the address, and `/models local` moves it back to the daemon's
/// own - the window always came home while the address stayed wherever the fleet switch put it.
#[test]
fn a_same_weights_switch_moves_the_address_and_local_moves_it_back() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("same-weights");
    let path = dir.path.join("sessions.db");
    let cfg = config(&path, "switch-same-weights");
    let own = format!("local — {} at {}", cfg.model, cfg.endpoint.authority());
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new("switch-same-weights");
    let mut h = Harness::open(&parts, cfg, hub).expect("the session must open");
    assert_eq!(
        h.provider_line(),
        own,
        "the session opens on the daemon's own server"
    );

    // The same first-shard basename this daemon tokenizes with, on another port.
    let gguf = apparatus::gguf_path().display().to_string();
    let port = props_stub(Some(&gguf), None);
    let said = h
        .set_local_model(&model("dense78", port, None))
        .expect("same weights switch");
    assert!(
        said.starts_with("turns go to `dense78` at 127.0.0.1:"),
        "{said}"
    );
    assert_eq!(
        h.provider_line(),
        format!("local — dense78 at 127.0.0.1:{port}"),
        "the address moved, and the line names where"
    );

    // And back: the daemon's own server, exactly as it was started.
    let back = h.set_provider(None).expect("the return");
    assert!(
        back.starts_with("turns go to the local server at "),
        "{back}"
    );
    assert_eq!(
        h.provider_line(),
        own,
        "the return is to the DAEMON's own address, not the last fleet stop"
    );
}

// --- the full cross-weights switch: live, gated on both GGUFs --------------

/// `/models local.glm` from a qwen daemon, end to end: weights, dialect, fork, and back.
#[test]
fn local_glm_from_a_qwen_daemon_switches_weights_dialect_and_comes_back() {
    let Some(qwen) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let glm = glm_gguf();
    let Some(_) = apparatus::present(
        &format!("the GLM vocabulary GGUF ({})", glm.display()),
        glm.is_file(),
    ) else {
        return;
    };

    let dir = TempDir::new("live-switch");
    let path = dir.path.join("sessions.db");
    let mut cfg = config(&path, "switch-live-glm");
    // The daemon's own weights are the qwen ones, so the switch really is cross-weights.
    cfg.vocab_gguf = Some(qwen);
    let parts = Parts::load(&cfg).expect("the qwen vocabulary must load");
    let hub = Hub::new("switch-live-glm");
    let mut h = Harness::open(&parts, cfg, hub).expect("the session must open");
    let own_line = h.provider_line();

    // A conversation with something in it, so the re-render is a measured fact and not a claim
    // about an empty prefix: one user item, appended through the fork machinery the way
    // `compact.rs` seeds its sessions, with no turn.
    use letibot_harnessd::harness::ForkTail;
    use letibot_transcript::{Speaker, TranscriptItem, UserPart};
    use letibot_turn::CompactionOutcome;
    let seeded = vec![TranscriptItem::User {
        speaker: Speaker::Operator,
        parts: vec![UserPart::Text {
            text: "switch me to the glm and back".into(),
        }],
    }];
    let outcome = CompactionOutcome {
        turn_id: "seed".into(),
        summary: String::new(),
        tool_calls: 0,
        truncated: false,
        cached_tokens: 0,
        reusable: 0,
        generated_tokens: 0,
    };
    h.fork_to_summary(
        &outcome,
        None,
        None,
        ForkTail {
            items: &seeded,
            split: None,
            because: "",
        },
    )
    .expect("seeding one item");

    // The switch: the stub serves the REAL gguf path on this box and glm's own template, so the
    // dialect is derived, the vocabulary loaded, the dialect fitted and the conversation forked -
    // all of it local, none of it a turn.
    let port = props_stub(Some(&glm.display().to_string()), Some(&glm_template()));
    let said = h
        .set_local_model(&model("glm-5.3-flash", port, None))
        .expect("the switch the operator asked for");
    // Printed as well as asserted: this is the line an operator reads at a picker.
    println!("AFTER: {said}");
    assert!(
        said.contains("turns go to `glm-5.3-flash` at 127.0.0.1:"),
        "{said}"
    );
    assert!(
        said.contains("Weights changed") && said.contains("qwen3.8 → glm-5.3-flash"),
        "the line says what moved, both axes: {said}"
    );
    assert!(
        said.contains("re-rendered") && said.contains("fork "),
        "the re-render is disclosed with its cost: {said}"
    );
    assert_eq!(
        h.provider_line(),
        format!("local — glm-5.3-flash at 127.0.0.1:{port}")
    );

    // The fork is in the store, and its prefix row is keyed by the GLM's own template sha - the
    // guarantee a resume depends on: another dialect's cache row is never reused.
    let store = letibot_tokencore::store::Store::open(&path).expect("the store reopens");
    let glm_sha = {
        use letibot_tokencore::ledger::hex as hex32;
        hex32(&Dialect::Glm.wiring(None).spec().template_sha)
    };
    let rows: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM stable_prefix WHERE dialect_sha = ?1",
            [&glm_sha],
            |r| r.get(0),
        )
        .expect("the store answers");
    assert!(
        rows > 0,
        "the switch wrote a prefix row under glm's own sha"
    );

    // And home again: the daemon's own weights, dialect and address, with the conversation
    // re-rendered once more under them.
    let back = h.set_provider(None).expect("the way home");
    println!("BACK: {back}");
    assert!(
        back.contains("turns go to the local server at ") && back.contains("Weights back"),
        "the return says what it restored: {back}"
    );
    assert_eq!(
        h.provider_line(),
        own_line,
        "home is the daemon's own server again"
    );
}

// A qwen template on other weights derives the qwen dialect by the same rule - pinned so the arm
// is exercised for both dialects this tree drives, not just the one the operator asked for.
#[test]
fn a_qwen_template_on_other_weights_derives_the_qwen_dialect() {
    let port = props_stub(
        Some("/m/qwen3.8-27b/Qwen3.8-27B-UD-Q6_K_XL-00001-of-00006.gguf"),
        Some(&qwen_template()),
    );
    let got = local_switch_decision(
        &letibot_turn::http::Endpoint::new("127.0.0.1", port),
        &model("dense78b", port, None),
        Some(std::path::Path::new(OWN_GGUF)),
    )
    .expect("a qwen template is as derivable as a glm one");
    assert!(
        matches!(
            got,
            LocalSwitch::OtherWeights {
                dialect: Dialect::Qwen,
                ..
            }
        ),
        "{got:?}"
    );
}
