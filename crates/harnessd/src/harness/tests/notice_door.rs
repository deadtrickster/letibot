//! K2: one notice door. Every model-facing sentence the harness composes is offered to a
//! `notice_<kind>` layer before it is said, mid-turn included, and with no layer it reads
//! byte-for-byte as it did before the door existed.

use super::*;
use crate::harness::turns::{queue_mid_turn, say};
use letibot_plugin::notice::{self, Facts};
use letibot_plugins::{BuildError, Builder, Capability, Loader, Module};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// What a layer was handed, in the order the harness said it.
#[derive(Default)]
struct Heard(Mutex<Vec<Value>>);

/// A layer that records every notice and answers from a table keyed by hook name.
struct RecordingModule {
    heard: Arc<Heard>,
    answers: std::collections::BTreeMap<String, Value>,
}

impl Module for RecordingModule {
    fn hooks(&self) -> Vec<String> {
        notice::KINDS.iter().map(|k| notice::hook(k)).collect()
    }

    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String> {
        let input: Value = serde_json::from_str(input).map_err(|e| format!("not a notice: {e}"))?;
        self.heard
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(input);
        Ok(self.answers.get(hook).map(|a| a.to_string()))
    }

    fn fuel(&self) -> u64 {
        9
    }
}

struct RecordingLoader {
    heard: Arc<Heard>,
    answers: std::collections::BTreeMap<String, Value>,
}

impl Loader for RecordingLoader {
    fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
        Ok(Box::new(RecordingModule {
            heard: self.heard.clone(),
            answers: self.answers.clone(),
        }))
    }
}

/// Nothing is ever built here: the layers are written, not compiled.
struct NoBuild;

impl Builder for NoBuild {
    fn build(&self, dir: &Path) -> Result<String, BuildError> {
        Ok(format!("not built: {}", dir.display()))
    }
}

/// A scratch directory that removes itself.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).expect("the scratch directory");
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A host whose one layer declares every notice hook and answers from `answers`.
fn recording_host(
    dir: &Scratch,
    answers: &[(&str, Value)],
) -> (Arc<crate::plugins::PluginHost>, Arc<Heard>) {
    let layer = dir.0.join(".letibot/plugins/notice-recorder");
    std::fs::create_dir_all(&layer).expect("the layer directory");
    let hooks = notice::KINDS
        .iter()
        .map(|k| format!("\"{}\"", notice::hook(k)))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        layer.join("plugin.toml"),
        format!(
            "name = \"notice-recorder\"\npurpose = \"records every notice\"\nhooks = [{hooks}]\n\
             capabilities = []\ncontract = 1\n"
        ),
    )
    .expect("a manifest");
    std::fs::write(layer.join("plugin.wasm"), b"a module").expect("a module");
    let heard = Arc::new(Heard::default());
    let host = crate::plugins::PluginHost::discover(
        "s-notice",
        &dir.0,
        None,
        None,
        Arc::new(RecordingLoader {
            heard: heard.clone(),
            answers: answers
                .iter()
                .map(|(hook, answer)| (hook.to_string(), answer.clone()))
                .collect(),
        }),
        Arc::new(NoBuild),
    );
    (Arc::new(host), heard)
}

fn inputs(heard: &Heard) -> Vec<Value> {
    heard.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// One fixture per §1.3 row: the site that composes a model-facing sentence, the facts it composes
/// it from, and the sentence itself. The door must hand each to the layer for its own kind, with
/// the harness's own words, and answer them unchanged when the layer says nothing.
fn every_row() -> Vec<(&'static str, Facts, &'static str)> {
    vec![
        (
            "wake: the queue's verdicts and handovers (merge_queue)",
            Facts::MergeQueue { rows: 1 },
            "the merge queue cannot land anything in `o/r`",
        ),
        (
            "wake: the room's bells (room)",
            Facts::Room { items: 1 },
            "[room] a peer claimed the migration",
        ),
        (
            "wake: what a restart brought back (restart)",
            Facts::Restart { lines: 1 },
            "[restart] `s-child` was brought back",
        ),
        (
            "wake: a monitor firing (monitor)",
            Facts::Monitor {
                fired: vec![notice::Firing {
                    name: "pr-24".into(),
                    watch: Some("pr o/r#24".into()),
                    ended: None,
                    said: "[monitor] `pr-24` fired: its checks finished.".into(),
                    folded: 0,
                    ending: false,
                }],
            },
            "[monitor] `pr-24` fired: its checks finished.",
        ),
        (
            "wake: a job that settled (job)",
            Facts::Job {
                jobs: vec!["j7".into()],
            },
            "[job] `j7` exited 0",
        ),
        (
            "wake: a todo row's condition met (todo)",
            Facts::Todo {
                rows: vec!["land the release".into()],
            },
            "[todo] Due (its condition is met):\n  - land the release — `j7` exited 0",
        ),
        (
            "wake: the workspace gone (workspace)",
            Facts::Workspace {
                path: "/home/dead/Projects/x".into(),
            },
            "[workspace] /home/dead/Projects/x is not a directory any more.",
        ),
        (
            "a round the gate admitted without asking (gate)",
            Facts::Gate,
            "admitted without asking — `cargo test` · inside the measured boundary",
        ),
        (
            "a round salvaged (salvage)",
            Facts::Salvage,
            "[salvage] the batch was truncated mid-call",
        ),
        (
            "the harness's own reminder (nudge)",
            Facts::Nudge,
            "Your reply was all reasoning and nothing was kept.",
        ),
        (
            "the turn's own correction (steer)",
            Facts::Steer,
            "[intent check] you said you would ssh and did not",
        ),
        (
            "the stall steer, one round before the bound (stall)",
            Facts::Stall { rounds: 4 },
            "One more empty round ends the turn",
        ),
        (
            "a context claim the measurement contradicts (context_claim)",
            Facts::ContextClaim {
                resident: 120_000,
                window: 200_000,
            },
            "[context] 120,000 of 200,000 tokens are resident",
        ),
        (
            "a session reseated or moved to a new model (reseat)",
            Facts::Reseat,
            "[reseat] this conversation was re-seated onto a new prompt",
        ),
        (
            "a layer's own failure (plugin)",
            Facts::Plugin,
            "a plugin layer's answer was refused",
        ),
        (
            "the idle plan-check (nag)",
            Facts::Nag {
                rows: vec!["land the release".into()],
            },
            "[todo check] Not done: land the release",
        ),
        (
            "the queue census (census)",
            Facts::Census {
                rows: 2,
                states: vec![notice::Stuck {
                    state: "failed".into(),
                    rows: 2,
                }],
            },
            "[queue] 2 failed · the oldest has been stuck for 3h",
        ),
        (
            "the restart continuation (restart)",
            Facts::Restart { lines: 2 },
            "[restart] the daemon stopped 12 minutes ago",
        ),
        (
            "the fallback continuation (fallback)",
            Facts::Fallback {
                was: "glm-coding/glm-5.3".into(),
                now: "glm/glm-4.6".into(),
            },
            "[fallback] glm-coding/glm-5.3 refused (http 429); continuing this task on glm/glm-4.6.",
        ),
        (
            "a parent's message (parent)",
            Facts::Parent {
                from: "s-parent".into(),
            },
            "[parent] use the second branch",
        ),
        (
            "the operator's answer to a row (answer)",
            Facts::Answer {
                row: "land the release".into(),
                question: "which database?".into(),
            },
            "the operator answered `which database?` on `land the release`: postgres",
        ),
        (
            "a directory's AGENTS.md, handed to a subagent (guidance)",
            Facts::Guidance {
                path: "crates/tools/AGENTS.md".into(),
                lines: 2,
            },
            "[guidance] `crates/tools/AGENTS.md` — the operator's rules for that directory",
        ),
    ]
}

/// Every model-facing sentence the harness composes passes the notice hook, with its own kind and
/// the facts it was built from, and reads exactly as it was written when the layer says nothing.
#[test]
fn every_model_facing_sentence_passes_the_notice_hook() {
    let dir = Scratch::new("k2-notice-rows");
    let (host, heard) = recording_host(&dir, &[]);
    let hub = Hub::new("s-notice");
    let rows = every_row();
    for (site, facts, text) in &rows {
        assert_eq!(
            say(Some(&host), &hub, facts, text).as_deref(),
            Some(*text),
            "{site}: a layer that says nothing leaves the sentence alone"
        );
    }
    let seen = inputs(&heard);
    assert_eq!(seen.len(), rows.len(), "one notice per row: {seen:#?}");
    for ((site, facts, text), input) in rows.iter().zip(&seen) {
        assert_eq!(input["kind"], json!(facts.kind()), "{site}: {input}");
        assert_eq!(input["facts"], facts.to_value(), "{site}: {input}");
        assert_eq!(input["text"], json!(text), "{site}: {input}");
        assert_eq!(
            input["must_see"],
            json!(!notice::droppable(facts.kind())),
            "{site}: the rule the layer is under rides with the notice"
        );
    }
    // And the table covers the whole contract: a kind nothing composes is a hole in the door, and a
    // kind composed without a row here would be a sentence this test does not know about.
    let covered: Vec<&str> = rows.iter().map(|(_, facts, _)| facts.kind()).collect();
    for kind in notice::KINDS {
        assert!(covered.contains(kind), "no fixture composes a `{kind}`");
    }
    for kind in notice::KINDS {
        assert!(
            seen.iter().any(|i| i["kind"] == json!(kind)),
            "the host never asked for `{kind}`"
        );
    }
}

/// A room wake that arrives while a turn is RUNNING is the same kind as one that arrives between
/// turns, and the layer's words are what the model reads: the mid-turn door is not a second one.
#[test]
fn a_mid_turn_room_wake_is_reworded() {
    let dir = Scratch::new("k2-notice-midturn");
    let reworded = "[room] CodeRabbit answered your question on o/r#24 — read it before you go on";
    let (host, heard) = recording_host(
        &dir,
        &[(&notice::hook(notice::ROOM), json!({"text": reworded}))],
    );
    let hub = Hub::new("s-notice");
    // The bell is queued the way the room's reader thread queues it.
    crate::room::wake::push("s-notice", "[room] CodeRabbit answered you".to_string());

    let mut steering = HubSteering::new(hub.clone());
    steering.host = Some(host);
    let said = steering.try_next().expect("a bell is handed over");
    assert_eq!(
        said.text, reworded,
        "the layer's words are what the model reads"
    );
    // And the layer was handed the harness's own sentence, not its own answer back.
    let seen = inputs(&heard);
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert_eq!(seen[0]["kind"], json!(notice::ROOM));
    assert_eq!(seen[0]["text"], json!("[room] CodeRabbit answered you"));
    assert_eq!(seen[0]["facts"]["items"], json!(1));
}

/// A census cannot be dropped: it is the fact a quiet session is woken for, so a layer may reword
/// it and the harness's own sentence stands whatever it answers.
#[test]
fn a_census_cannot_be_dropped() {
    let dir = Scratch::new("k2-notice-census");
    let (host, heard) = recording_host(
        &dir,
        &[(&notice::hook(notice::CENSUS), json!({"drop": true}))],
    );
    let hub = Hub::new("s-notice");
    let facts = Facts::Census {
        rows: 2,
        states: vec![notice::Stuck {
            state: "failed".into(),
            rows: 2,
        }],
    };
    let text = "[queue] 2 failed · the oldest has been stuck for 3h";
    assert!(!notice::droppable(notice::CENSUS));
    assert_eq!(
        say(Some(&host), &hub, &facts, text).as_deref(),
        Some(text),
        "a drop of a must-see kind is refused and the sentence stands"
    );
    assert_eq!(inputs(&heard).len(), 1, "the layer was asked");
}

/// A stall steer can be dropped, and a dropped one never reaches the running turn: the harness's
/// own reminder is the one kind a project may switch off, and the queue is where that is honoured.
#[test]
fn a_stall_steer_can_be_dropped() {
    let dir = Scratch::new("k2-notice-stall");
    let (host, _) = recording_host(
        &dir,
        &[
            (&notice::hook(notice::STALL), json!({"drop": true})),
            (&notice::hook(notice::PARENT), json!({"drop": true})),
        ],
    );
    let hub = Hub::new("s-notice");
    let injected = Mutex::new(VecDeque::new());

    queue_mid_turn(
        Some(&host),
        &hub,
        &injected,
        &Facts::Stall { rounds: 4 },
        "One more empty round ends the turn",
    );
    assert!(
        injected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty(),
        "a dropped stall steer must not reach the running turn"
    );

    // The same answer on a kind the model is owed is refused: the sentence is queued.
    queue_mid_turn(
        Some(&host),
        &hub,
        &injected,
        &Facts::Parent {
            from: "s-parent".into(),
        },
        "[parent] use the second branch",
    );
    assert_eq!(
        injected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .front()
            .cloned(),
        Some("[parent] use the second branch".to_string()),
        "a parent's words are a fact the model is owed"
    );
}
