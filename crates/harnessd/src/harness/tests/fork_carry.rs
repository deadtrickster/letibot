//! A fork that continues a conversation takes what the session had not read yet: the prompts
//! queued for the turn that is ending, and the steers a refused round handed back.

use super::*;

use letibot_sessionlog::{CommandKind, PromptDelivery, protocol::Caps};

/// A scratch directory that removes itself.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn send(hub: &Arc<Hub>, id: &str, text: &str, delivery: PromptDelivery) {
    let head = hub.attach("tui", "dead@lab2x1", Caps::default(), 0);
    hub.submit(
        &head.head_id,
        id,
        0,
        CommandKind::Prompt {
            text: text.into(),
            attachments: Vec::new(),
            context: None,
            from: None,
            delivery,
        },
    );
}

fn queued(hub: &Arc<Hub>) -> Vec<(String, CommandKind)> {
    let mut out = Vec::new();
    while let Some(c) = hub.try_command() {
        let text = match &c.kind {
            CommandKind::Prompt { text, .. } => text.clone(),
            other => format!("{other:?}"),
        };
        out.push((text, c.kind));
    }
    out
}

/// The finding's own case: a limit hook forks mid-turn, and the prompts a person typed for that
/// turn, and the steers a refused round handed back, continue in the fork instead of running on
/// the model that refused.
#[test]
fn a_new_session_fork_takes_the_queue_with_it() {
    let dir = Scratch::new("fork-carry");
    let path = dir.0.join("sessions.db");
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(path);
    cfg.session_id = "s-src".into();
    // A root seat carries `flowy`, which needs an adjudicator; this fixture is about the queue that
    // follows a conversation, so it is a child like the one the hook forks.
    cfg.parent_session_id = Some("s-parent".into());
    // A search key on the box seats `web_search`, and a gated tool with nobody behind the gate
    // refuses the open.
    cfg.web_search = None;
    at_always_ask(&mut cfg);
    let parts = Parts::load(&cfg).expect("the tokeniser loads");
    let mut h = Harness::open_with(
        &parts,
        cfg,
        Hub::new("s-src"),
        Some(Box::new(letibot_tools::adjudicate::NoAdjudicator)),
        None,
    )
    .expect("the session opens");

    send(&h.hub, "q1", "second question", PromptDelivery::Queue);
    send(&h.hub, "q2", "third question", PromptDelivery::Queue);
    // Not a prompt, so it is not the fork's: a stop the operator typed beside them stays here.
    let head = h.hub.attach("tui", "dead@lab2x1", Caps::default(), 0);
    h.hub.submit(
        &head.head_id,
        "m1",
        0,
        CommandKind::Mode {
            name: "always-ask".into(),
            consented: false,
        },
    );
    // A steer a refused round handed back, and a monitor's firing held beside it.
    h.handed_back
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(letibot_turn::SteeringMessage::normal(
            "hold on, wait for me",
        ));
    h.handed_back
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(letibot_turn::SteeringMessage::notice("a monitor fired"));

    let fork = Hub::new("s-fork");
    h.limit_carry_the_unread(&fork, "s-src", "s-fork");

    let carried = queued(&fork);
    let words: Vec<&str> = carried.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        words,
        vec!["second question", "third question", "hold on, wait for me"],
        "the fork holds the queue and the steer, in that order"
    );
    assert!(
        carried
            .iter()
            .all(|(_, k)| matches!(k, CommandKind::Prompt { .. })),
        "every carried command is a prompt"
    );
    let left = queued(&h.hub);
    assert_eq!(left.len(), 1, "only the stop stayed: {left:?}");
    assert!(
        matches!(&left[0].1, CommandKind::Mode { .. }),
        "the stop is not carried: {left:?}"
    );
    let held = h.handed_back.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(held.len(), 1, "the notice stayed: {held:?}");
    assert!(held[0].notice.is_some(), "and it is the notice");
}
