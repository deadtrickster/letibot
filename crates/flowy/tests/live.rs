//! Against the real node, with the real seat, from the usual path.
//!
//! Runs only with `FLOWY_LIVE=1`; everything else skips loudly (D10's rule: a
//! check that did not run is not a pass). It takes no message off the seat's
//! inbox unless the reader is free — and when the reader is held by a live
//! `flowy inbox`, the assertion is D2's: the second listener is REFUSED, naming
//! the pid, and nothing is killed.

use letibot_flowy::creds::{Onboarding, discover};
use letibot_flowy::seat::{PollOutcome, Seat, SeatState};

#[test]
fn the_usual_path_reaches_the_node_or_is_refused_by_the_holder() {
    if std::env::var("FLOWY_LIVE").ok().as_deref() != Some("1") {
        eprintln!("SKIPPED: FLOWY_LIVE is not 1 — the check did not run and this is not a pass");
        return;
    }
    let creds = discover(&Onboarding::usual()).expect("a seat on the usual path");
    eprintln!("credentials: {}", creds.describe());
    assert!(
        creds.token_file.is_some(),
        "the seat's token should come from its file"
    );

    match Seat::open(creds, None, None) {
        Err(e) => {
            let s = e.to_string();
            eprintln!("{s}");
            assert!(s.starts_with("LISTENER REFUSED"), "{s}");
            assert!(s.contains("pid "), "{s}");
        }
        Ok(seat) => {
            let me = seat.whoami().expect("whoami");
            eprintln!("whoami: {me:?}");
            assert!(!me.agent_id.is_empty());
            let reader = seat
                .reader()
                .expect("readers")
                .expect("the seat's reader exists");
            eprintln!("reader at {}", reader.cursor);
            let cond = seat.attach("live-test", Default::default());
            let outcome = seat.poll_once();
            eprintln!("poll: {outcome:?}; state {}", seat.state().word());
            assert!(matches!(outcome, PollOutcome::Delivered(_)), "{outcome:?}");
            assert!(matches!(seat.state(), SeatState::Listening { .. }));
            if let Some(why) = letibot_tools::exec::monitor::Condition::met(&*cond) {
                eprintln!("delivered:\n{why}");
            }
            seat.stop();
        }
    }
}

/// The read-only doors, which need no reader and take nothing off the inbox.
#[test]
fn the_node_client_reads_whoami_readers_and_a_room() {
    if std::env::var("FLOWY_LIVE").ok().as_deref() != Some("1") {
        eprintln!("SKIPPED: FLOWY_LIVE is not 1 — the check did not run and this is not a pass");
        return;
    }
    let creds = discover(&Onboarding::usual()).expect("a seat on the usual path");
    let node = letibot_flowy::Node::new(
        creds.endpoint.clone(),
        creds.addr.clone(),
        creds.token.clone(),
    );
    let me = node.whoami().expect("whoami");
    eprintln!(
        "whoami: agent {} kind {} project {}",
        me.agent, me.agent_kind, me.project
    );
    assert!(!me.agent.is_empty());
    let readers = node.readers().expect("readers");
    assert!(
        readers.iter().any(|r| r.reader == creds.agent),
        "{readers:?}"
    );
    let recent = node.room_read("general", 3).expect("room read");
    assert!(recent.len() <= 3);
    let id = letibot_flowy::attention::Identity {
        user_id: me.user.clone(),
        agent_id: me.agent.clone(),
        name: creds.agent.clone(),
    };
    for e in &recent {
        eprint!("{}", letibot_flowy::render::message(e, &id));
    }
    let build = node.node().expect("node");
    eprintln!(
        "node build {}",
        build.get("version").and_then(|v| v.as_str()).unwrap_or("?")
    );
}

/// The shelf, live: the same rows `flowy skills` lists, through the seat's
/// token. Takes no reader.
#[test]
fn the_fabric_shelf_lists_and_loads_a_skill() {
    if std::env::var("FLOWY_LIVE").ok().as_deref() != Some("1") {
        eprintln!("SKIPPED: FLOWY_LIVE is not 1 — the check did not run and this is not a pass");
        return;
    }
    use letibot_tools::builtins::skill::SkillShelf;
    let creds = discover(&Onboarding::usual()).expect("a seat on the usual path");
    let node = letibot_flowy::Node::new(
        creds.endpoint.clone(),
        creds.addr.clone(),
        creds.token.clone(),
    );
    let rows = node.artifacts_of_kind("skill", 50).expect("kind=skill");
    eprintln!("{} skills on the shelf", rows.len());
    assert!(!rows.is_empty());
    let first = &rows[0];
    let d = std::env::temp_dir().join(format!("letibot-shelf-live-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    // A seat with a private claim dir: this test holds no reader and polls nothing.
    let seat = Seat::open(creds, Some(&d), Some(&d)).expect("open without polling");
    let shelf = letibot_flowy::FabricShelf::new(seat);
    let listed = shelf.list().expect("list");
    assert!(listed.iter().any(|e| e.id == first.id));
    let by_id = shelf.load(&first.id).expect("by id");
    let by_title = shelf.load(&first.title).expect("by title");
    assert_eq!(by_id.body, by_title.body);
    eprintln!("loaded `{}`: {} bytes", by_id.name, by_id.body.len());
}
