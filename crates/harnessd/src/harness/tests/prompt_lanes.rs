//! The two lanes a prompt typed mid-turn can take: steer reaches the running turn at its next step
//! boundary, queue is left for the worker, which opens a turn for it after this one.

use super::*;

use letibot_sessionlog::{CommandKind, PromptDelivery, protocol::Caps};

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

#[test]
fn the_running_turn_takes_a_steer_and_leaves_a_queued_prompt() {
    let hub = Hub::new("s-lanes");
    let mut s = HubSteering::new(hub.clone());
    send(
        &hub,
        "c1",
        "write the changelog next",
        PromptDelivery::Queue,
    );
    send(&hub, "c2", "use the short form", PromptDelivery::Steer);
    let m = s.try_next().expect("the steer reaches the turn");
    assert!(m.from_operator, "{m:?}");
    assert_eq!(m.text, "use the short form");
    assert!(
        s.try_next().is_none(),
        "the queued prompt is not this turn's"
    );
    let Some(cmd) = hub.try_command() else {
        panic!("the queued prompt waits for the worker");
    };
    assert!(
        matches!(&cmd.kind, CommandKind::Prompt { text, .. } if text == "write the changelog next"),
        "{cmd:?}"
    );
}
