//! Markdown command files through the daemon's own door: `/review foo` as a head sends it
//! becomes the operator's prompt with the file's body expanded, the settings row lists the
//! commands, and a verb with no file is refused naming where the file would be.

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::{CommandKind, Hub, QueuedCommand};
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{SessionRecord, Store};
use letibot_transcript::{Speaker, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

mod support;

struct Fixture {
    workspace: std::path::PathBuf,
    user: std::path::PathBuf,
    store: std::path::PathBuf,
    id: String,
}

fn fixture(tag: &str) -> Fixture {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "harnessd-command-files-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let workspace = dir.join("project");
    let user = dir.join("config");
    std::fs::create_dir_all(workspace.join(".letibot/commands")).unwrap();
    std::fs::create_dir_all(user.join("commands")).unwrap();
    let store = dir.join("sessions.db");
    let id = format!("command-files-{tag}");
    Store::open(&store)
        .expect("store")
        .put_session(&SessionRecord {
            id: id.clone(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: workspace.display().to_string(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .expect("the session row");
    Fixture {
        workspace,
        user,
        store,
        id,
    }
}

fn config(f: &Fixture) -> Config {
    let mut cfg = Config::for_this_box(f.workspace.to_str().unwrap());
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(f.store.clone());
    cfg.session_id = f.id.clone();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    // No model answers: the prompt is what is checked, and the turn fails at once.
    cfg.endpoint = Endpoint::new("127.0.0.1", 1);
    cfg.http_retries = 0;
    cfg.vocab_gguf = support::vocab::gguf();
    cfg.box_dirs.config = f.user.clone();
    cfg
}

fn command(line: &str) -> QueuedCommand {
    QueuedCommand {
        head_id: "test-head".into(),
        identity: "dead".into(),
        client_request_id: "req-1".into(),
        at_seq: 0,
        kind: CommandKind::Slash { line: line.into() },
        enqueued_ms: 0,
    }
}

fn operator_texts(hub: &Hub) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. }
            | SessionEvent::TranscriptAppended {
                item: Some(item), ..
            } => match &**item {
                TranscriptItem::User {
                    speaker: Speaker::Operator,
                    parts,
                    ..
                } => Some(
                    parts
                        .iter()
                        .filter_map(|p| match p {
                            UserPart::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn warnings(hub: &Hub, code: &str) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning {
                code: c, detail, ..
            } if c == code => Some(detail.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_project_command_is_sent_expanded_and_listed_and_a_missing_one_is_refused() {
    let f = fixture("e2e");
    std::fs::write(
        f.workspace.join(".letibot/commands/review.md"),
        "---\ndescription: review a change\nargument-hint: PATH\n---\nReview $ARGUMENTS and report.\n",
    )
    .unwrap();
    std::fs::write(f.user.join("commands/review.md"), "the user's review").unwrap();
    std::fs::write(f.user.join("commands/standup.md"), "Write the standup.").unwrap();

    let cfg = config(&f);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let registry = Registry::new();
    registry
        .create(&f.id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");
    let hub = registry.get(&f.id).expect("the hub");

    sessions.dispatch(&f.id, &command("review foo"));
    let sent = operator_texts(&hub);
    assert!(
        sent.iter().any(|t| t == "Review foo and report."),
        "the project's file, expanded, is the operator's prompt: {sent:?}"
    );
    assert!(
        !sent.iter().any(|t| t.contains("the user's review")),
        "the project's file wins over the user's: {sent:?}"
    );
    let said = warnings(&hub, "command_file");
    assert!(
        said.iter()
            .any(|w| w.contains(".letibot/commands/review.md")),
        "the row names the file: {said:?}"
    );

    let row = registry
        .settings(&f.id)
        .into_iter()
        .find(|r| r.key == letibot_sessionlog::protocol::DAEMON_COMMANDS_KEY)
        .expect("a commands row");
    assert_eq!(row.value, "review,standup");
    assert_eq!(
        row.choices,
        vec!["PATH - review a change", "Write the standup."]
    );

    sessions.dispatch(&f.id, &command("nonesuch x"));
    let refused = warnings(&hub, "slash_refused");
    assert!(
        refused
            .iter()
            .any(|w| w.contains("no command file defines it") && w.contains("nonesuch.md")),
        "a missing file is refused naming where it was looked for: {refused:?}"
    );
}
