//! The `config` tool from the model's side: what it refuses before anybody is asked, what it puts
//! on the card, and whose answer it takes.

use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use letibot_config_ref::edit::Edits;

use crate::runtime::{Gate, GateCall, GateDecision, Registry, ToolRuntime};
use letibot_transcript::ToolCall;

// ---------------------------------------------------------------- the desk a test can drive

/// A desk over a directory of its own: nothing touching the operator's real config.
struct DirDesk {
    dir: std::path::PathBuf,
    /// What `show` claims to have redacted, so a test can prove the tool prints the desk's bytes.
    shown: Mutex<usize>,
}

impl DirDesk {
    fn new() -> DirDesk {
        let dir = std::env::temp_dir().join(format!(
            "letibot-config-tool-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("a fixture directory");
        DirDesk {
            dir,
            shown: Mutex::new(0),
        }
    }

    fn path(&self, file: &str) -> std::path::PathBuf {
        self.dir.join(file)
    }
}

/// A unified diff, as the smallest thing a card needs: the line that changed, and what it became.
fn diff_of(before: &str, after: &str, name: &str) -> String {
    let mut out = format!("--- a/{name}\n+++ b/{name}\n");
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    out.push_str(&format!("@@ -1,{} +1,{} @@\n", a.len(), b.len()));
    for l in &a {
        if !b.contains(l) {
            out.push_str(&format!("-{l}\n"));
        }
    }
    for l in &b {
        if !a.contains(l) {
            out.push_str(&format!("+{l}\n"));
        }
    }
    out
}

impl ConfigDesk for DirDesk {
    fn ready(&self) -> Result<(), String> {
        Ok(())
    }

    fn explain(&self, file: Option<&str>, key: Option<&str>) -> Result<String, String> {
        if let Some(word) = key {
            return Ok(format!("[{file:?}] `{word}` decides something"));
        }
        let f = letibot_config_ref::find(file.expect("a file")).expect("a listed file");
        Ok(letibot_config_ref::text(f))
    }

    fn show(&self, file: &str) -> Result<Shown, String> {
        let text = std::fs::read_to_string(self.path(file)).map_err(|e| e.to_string())?;
        *self.shown.lock().unwrap() += 1;
        Ok(Shown {
            path: self.path(file).display().to_string(),
            text: text.replace("sk-secret", "\"<redacted>\""),
            note: "1 credential value replaced".into(),
            mode: Some(0o600),
        })
    }

    fn propose(&self, file: &str, edits: &Edits) -> Result<Proposal, String> {
        let before = std::fs::read_to_string(self.path(file)).map_err(|e| e.to_string())?;
        let text = letibot_config_ref::edit::apply(&before, edits)?;
        // The daemon's reader, standing in as one that refuses what it does not know.
        if text.contains("nonsense") {
            return Err(format!("{file}: `nonsense` is not a key this file reads"));
        }
        Ok(Proposal {
            path: self.path(file).display().to_string(),
            diff: diff_of(&before, &text, file),
            before,
            text,
            check: format!("read back by the real {file} reader"),
            preview: Vec::new(),
        })
    }

    fn write(&self, file: &str, before: &str, text: &str) -> Result<Written, String> {
        let now = std::fs::read_to_string(self.path(file)).map_err(|e| e.to_string())?;
        if now != before {
            return Err("the file changed while the card was up".into());
        }
        std::fs::write(self.path(file), text).map_err(|e| e.to_string())?;
        Ok(Written {
            path: self.path(file).display().to_string(),
            bytes: text.len(),
            mode: Some(0o600),
        })
    }
}

// ---------------------------------------------------------------- the gate a test can script

/// What the gate answers, and the cards it was shown.
struct Scripted {
    answer: ConfigAnswer,
    ask: Arc<Mutex<Vec<ConfigAsk>>>,
}

impl Gate for Scripted {
    fn admit(&mut self, _call: &GateCall<'_>) -> GateDecision {
        GateDecision::Admit
    }
    fn ask_config(&mut self, ask: &ConfigAsk) -> ConfigAnswer {
        self.ask.lock().unwrap().push(ask.clone());
        self.answer.clone()
    }
}

struct Harness {
    rt: ToolRuntime,
    desk: Arc<DirDesk>,
    ask: Arc<Mutex<Vec<ConfigAsk>>>,
    calls: Arc<AtomicUsize>,
}

/// A runtime over a fixture config dir whose gate answers `answer`.
fn harness(answer: ConfigAnswer) -> Harness {
    let desk = Arc::new(DirDesk::new());
    std::fs::write(
        desk.path("providers.toml"),
        "# mine\n[shadow]\nanswer_reads = false   # the guard only observes\n\n[deepseek]\nkey = \"sk-secret\"\n",
    )
    .expect("a fixture file");
    let ask = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut reg = Registry::new();
    reg.register(Box::new(ConfigTool::new(desk.clone()))).unwrap();
    let d = crate::backend::tempdir::TempDir::new();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    let rt = ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(Scripted {
        answer,
        ask: ask.clone(),
    }));
    Harness {
        rt,
        desk,
        ask,
        calls,
    }
}

fn invoke(h: &mut Harness, args: serde_json::Value) -> crate::ToolResult {
    let mut sink = crate::NullToolSink;
    h.calls.fetch_add(1, Ordering::SeqCst);
    h.rt.invoke(
        "t1",
        &ToolCall {
            id: "c1".into(),
            name: TOOL.into(),
            arguments: args.to_string(),
        },
        &mut sink,
    )
}

fn human() -> ConfigAnswer {
    ConfigAnswer::Granted {
        by: "human:dead".into(),
    }
}

// ---------------------------------------------------------------- the card

/// A proposal renders the diff, puts it on the card, and writes only on the operator's yes.
#[test]
fn a_proposal_renders_its_diff_and_waits_for_the_operator() {
    let mut h = harness(human());
    let r = invoke(
        &mut h,
        json!({
            "action": "propose",
            "file": "providers.toml",
            "edit": {"shadow.answer_reads": true},
            "why": "the guard's own reads are safe enough to answer"
        }),
    );
    assert!(matches!(r.outcome, letibot_transcript::ToolOutcome::Ok), "{:?}", r.outcome);
    // The card carried the diff and the why, so a person reads the change and not a summary.
    let cards = h.ask.lock().unwrap();
    assert_eq!(cards.len(), 1, "one card");
    match &cards[0] {
        ConfigAsk::Propose { file, diff, why, .. } => {
            assert_eq!(file, "providers.toml");
            assert!(diff.contains("+answer_reads = true"), "{diff}");
            assert!(diff.contains("+++ b/providers.toml"), "{diff}");
            assert_eq!(why, "the guard's own reads are safe enough to answer");
        }
        other => panic!("{other:?}"),
    }
    drop(cards);
    // And the write kept the operator's comment, which is what `toml_edit` is for.
    let after = std::fs::read_to_string(h.desk.path("providers.toml")).unwrap();
    assert!(after.contains("# the guard only observes"), "{after}");
    assert!(after.contains("answer_reads = true"), "{after}");
    assert!(r.payload.contains("human:dead"), "{}", r.payload);
    assert!(
        r.payload.contains("read back by the real providers.toml reader"),
        "{}",
        r.payload
    );
    // What a restart means for the change is the reference's own sentence.
    let reload = letibot_config_ref::find("providers.toml").unwrap().reload;
    assert!(r.payload.contains(reload), "{}", r.payload);
}

/// A result the file's own reader refuses is refused here, with that reader's words, and no card
/// is ever raised.
#[test]
fn a_broken_result_is_refused_with_the_readers_words() {
    let mut h = harness(human());
    let r = invoke(
        &mut h,
        json!({
            "action": "propose",
            "file": "providers.toml",
            "edit": {"nonsense.setting": true},
            "why": "x"
        }),
    );
    assert!(matches!(r.outcome, letibot_transcript::ToolOutcome::Failed { .. }), "{:?}", r.outcome);
    // The reference does not list the key, so it never reaches the reader: the refusal names it.
    assert!(r.payload.contains("nonsense.setting"), "{}", r.payload);
    assert!(h.ask.lock().unwrap().is_empty(), "nobody was asked");
    assert!(
        !std::fs::read_to_string(h.desk.path("providers.toml"))
            .unwrap()
            .contains("nonsense"),
        "nothing was written"
    );
}

// ---------------------------------------------------------------- who answers

/// Each answerer that is not the operator is refused, and the file is left alone.
#[test]
fn an_answer_that_is_not_the_operators_writes_nothing() {
    for who in ["model:gatekeeper", "agent:s-9", "gate:mode", "shadow:qwen"] {
        let mut h = harness(ConfigAnswer::Granted { by: who.into() });
        let r = invoke(
            &mut h,
            json!({
                "action": "propose",
                "file": "providers.toml",
                "edit": {"shadow.answer_reads": true},
                "why": "x"
            }),
        );
        assert!(matches!(r.outcome, letibot_transcript::ToolOutcome::Failed { .. }), "{who}");
        assert!(r.payload.contains(who), "{}", r.payload);
        let after = std::fs::read_to_string(h.desk.path("providers.toml")).unwrap();
        assert!(after.contains("answer_reads = false"), "{who}: {after}");
    }
}

/// The operator saying no, and nobody being asked at all, both write nothing - and they are
/// different sentences, because one is a decision and the other is silence.
#[test]
fn a_no_and_a_silence_are_both_said_plainly() {
    let mut h = harness(ConfigAnswer::Refused {
        by: "human:dead".into(),
        why: "not this week".into(),
    });
    let r = invoke(
        &mut h,
        json!({"action": "propose", "file": "providers.toml",
               "edit": {"shadow.answer_reads": true}, "why": "x"}),
    );
    assert!(r.payload.contains("refused by human:dead"), "{}", r.payload);
    assert!(r.payload.contains("not this week"), "{}", r.payload);

    let mut h = harness(ConfigAnswer::NotAsked("no head is attached".into()));
    let r = invoke(
        &mut h,
        json!({"action": "propose", "file": "providers.toml",
               "edit": {"shadow.answer_reads": true}, "why": "x"}),
    );
    assert!(r.payload.contains("nobody could be asked"), "{}", r.payload);
    assert!(r.payload.contains("no head is attached"), "{}", r.payload);
}

// ---------------------------------------------------------------- before any card

/// `keys/` and every other path-shaped name are refused before anybody is asked.
#[test]
fn a_path_is_refused_by_name_and_nobody_is_asked() {
    let mut h = harness(human());
    for name in [
        "keys/lab2x1",
        "keys",
        "../leticode.toml",
        "/etc/passwd",
        "id_rsa",
        "known_hosts",
    ] {
        let r = invoke(&mut h, json!({"action": "show", "file": name}));
        assert!(matches!(r.outcome, letibot_transcript::ToolOutcome::Failed { .. }), "{name}");
        assert!(
            r.payload.contains("keys/") || r.payload.contains("not one of letibot's config files"),
            "{name}: {}",
            r.payload
        );
    }
    assert!(
        h.ask.lock().unwrap().is_empty(),
        "a card was raised for a name the tool should never have offered"
    );
}

/// A credential is never written, with or without a reason — and the path is refused whole, so
/// an env or headers table cannot be reached by naming the key beside it.
#[test]
fn a_credential_edit_is_refused_and_nobody_is_asked() {
    let mut h = harness(human());
    for (file, edit) in [
        ("providers.toml", json!({"deepseek.key": "sk-new"})),
        ("providers.toml", json!({"provider.mine.key": "x"})),
        ("providers.toml", json!({"brave.key": "probe"})),
        (
            "leticode.toml",
            json!({"perimeter.top-level.env.GH_TOKEN": "x"}),
        ),
        (
            "leticode.toml",
            json!({"mcp.search.headers": {"Authorization": "Bearer x"}}),
        ),
    ] {
        let r = invoke(
            &mut h,
            json!({"action": "propose", "file": file, "edit": edit, "why": "needed"}),
        );
        assert!(matches!(r.outcome, letibot_transcript::ToolOutcome::Failed { .. }), "{edit}");
        assert!(
            r.payload.contains("sets a credential"),
            "{edit}: {}",
            r.payload
        );
    }
    // A key that NAMES a credential rather than holding one is an ordinary setting: the path to
    // the file a token lives in is not the token.
    assert!(credential_path("github.token_path").is_none());
    assert!(h.ask.lock().unwrap().is_empty(), "nobody was asked");
}

/// `why` is the card's own line, so a proposal without one is refused before it is made.
#[test]
fn a_proposal_needs_a_reason() {
    let mut h = harness(human());
    let r = invoke(
        &mut h,
        json!({"action": "propose", "file": "providers.toml",
               "edit": {"shadow.answer_reads": true}}),
    );
    assert!(matches!(r.outcome, letibot_transcript::ToolOutcome::Failed { .. }));
    assert!(r.payload.contains("`why` is required"), "{}", r.payload);
}

// ---------------------------------------------------------------- explain and show

/// `explain` answers from the reference and asks nobody: it opens no file at all.
#[test]
fn explain_answers_without_a_card() {
    let mut h = harness(human());
    let r = invoke(&mut h, json!({"action": "explain"}));
    assert!(matches!(r.outcome, letibot_transcript::ToolOutcome::Ok), "{:?}", r.outcome);
    assert!(r.payload.contains("providers.toml"), "{}", r.payload);
    let r = invoke(&mut h, json!({"action": "explain", "key": "answer_reads"}));
    assert!(r.payload.contains("answer_reads"), "{}", r.payload);
    assert!(h.ask.lock().unwrap().is_empty(), "explain asked somebody");
}

/// `show` prints the desk's redacted bytes, on the operator's word, and says what was taken out.
#[test]
fn show_prints_the_redacted_bytes() {
    let mut h = harness(human());
    let r = invoke(&mut h, json!({"action": "show", "file": "providers.toml"}));
    assert!(matches!(r.outcome, letibot_transcript::ToolOutcome::Ok), "{:?}", r.outcome);
    assert!(!r.payload.contains("sk-secret"), "{}", r.payload);
    assert!(r.payload.contains("<redacted>"), "{}", r.payload);
    assert!(r.payload.contains("mode 600"), "{}", r.payload);
    assert!(r.payload.contains("human:dead"), "{}", r.payload);
    assert_eq!(h.ask.lock().unwrap().len(), 1);
    // And a session that is not the operator sees nothing at all.
    let mut h = harness(ConfigAnswer::Granted {
        by: "agent:s-9".into(),
    });
    let r = invoke(&mut h, json!({"action": "show", "file": "providers.toml"}));
    assert!(matches!(r.outcome, letibot_transcript::ToolOutcome::Failed { .. }));
    assert!(!r.payload.contains("<redacted>"), "{}", r.payload);
}

/// With no config dir behind the tool, every verb that would touch one says so by name.
#[test]
fn no_desk_says_so_and_explain_still_answers() {
    let mut reg = Registry::new();
    reg.register(Box::new(ConfigTool::new(Arc::new(NoConfig))))
        .unwrap();
    let d = crate::backend::tempdir::TempDir::new();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    let mut rt = ToolRuntime::new(reg, Box::new(backend));
    let mut sink = crate::NullToolSink;
    let r = rt.invoke(
        "t1",
        &ToolCall {
            id: "c1".into(),
            name: TOOL.into(),
            arguments: json!({"action": "show", "file": "leticode.toml"}).to_string(),
        },
        &mut sink,
    );
    assert!(matches!(r.outcome, letibot_transcript::ToolOutcome::Failed { .. }), "{:?}", r.outcome);
    assert!(r.payload.contains("serves no config directory"), "{}", r.payload);
}

/// The values a proposal can name, as TOML: a string stays quoted, a number does not, and a null
/// is a removal rather than a value.
#[test]
fn json_becomes_toml_source() {
    assert_eq!(json_to_toml(&json!(true)).unwrap(), "true");
    assert_eq!(json_to_toml(&json!(90)).unwrap(), "90");
    assert_eq!(json_to_toml(&json!("45m")).unwrap(), "\"45m\"");
    assert_eq!(
        json_to_toml(&json!(["2m", "5m"])).unwrap(),
        "[\"2m\", \"5m\"]"
    );
    assert_eq!(
        json_to_toml(&json!({"log": "warn"})).unwrap(),
        "{ log = \"warn\" }"
    );
    assert!(json_to_toml(&json!(null)).is_err());
    let edits = edits_of(&json!({
        "edit": {"shadow.fresh_every": null, "shadow.mode": "stateless"}
    }))
    .unwrap();
    assert_eq!(edits.remove, vec!["shadow.fresh_every".to_string()]);
    assert_eq!(edits.set.get("shadow.mode").map(String::as_str), Some("\"stateless\""));
}
