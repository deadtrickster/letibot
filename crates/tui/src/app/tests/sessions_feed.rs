//! What the pane is ASKED, and how often: the input the layer is handed, and the cache that keeps
//! an idle frame from re-running a guest. The drawing itself is pinned in `sessions_list`.

use super::*;

/// A head with one conversation, and the picker open.
fn a_head_with_one_session() -> App {
    let mut a = app();
    a.apply(hello(
        "s-here",
        vec![brief("s-here", "the habr article", true)],
        Hub::new("s-here").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    // A session that moved, so the feed has an age to call a bucket.
    let now = letibot_sessionlog::registry::now_ms();
    for s in &mut a.sessions {
        s.moved_ms = now.saturating_sub(20 * 60 * 1000);
    }
    a
}

/// **An idle frame does not re-run the guest.** The one-slot cache is keyed by the whole input, so
/// anything in it that moves on its own makes every frame a different question - and the clock
/// was: `now` in the feed meant the key changed every millisecond. An age is a BUCKET now
/// (`letibot_plugin::sessions::age_word`), so frames inside one bucket ask once.
#[test]
fn an_idle_frame_does_not_ask_the_guest_again() {
    let mut a = a_head_with_one_session();
    let first = a.picker_lines(100).join("\n");
    assert!(first.contains("the habr article"), "{first}");
    let asked = a
        .sessions_host
        .borrow()
        .as_ref()
        .expect("a host")
        .list_asks();
    assert_eq!(asked, 1, "the first frame asks once");
    // Two more frames, a few hundred milliseconds apart on the head's own clock: the same bucket,
    // so the same input, so nothing is asked again.
    for tick in [a.now_ms + 100, a.now_ms + 900] {
        a.clock(tick);
        let again = a.picker_lines(100).join("\n");
        assert_eq!(again, first, "the same picture");
    }
    assert_eq!(
        a.sessions_host
            .borrow()
            .as_ref()
            .expect("a host")
            .list_asks(),
        1,
        "three frames, one guest run"
    );
    // And the feed says nothing about a clock: the age is a word.
    let feed = a.sessions_feed();
    assert!(feed.get("now").is_none(), "no clock in the feed: {feed}");
    let age = feed["sessions"][0]["age"].as_str().unwrap_or_default();
    assert!(!age.is_empty(), "and the age is a word: {feed}");
}

/// The pane is asked about the height it is DRAWN at - a layer that fits its answer to a rectangle
/// was being told `40` whatever the screen said.
#[test]
fn the_pane_is_asked_about_the_height_it_is_drawn_at() {
    let a = a_head_with_one_session();
    a.picker_layout(96, 17);
    assert_eq!(
        a.sessions_host
            .borrow()
            .as_ref()
            .expect("a host")
            .asked_size(),
        Some((96, 17)),
        "the size the layer is handed is the pane's own"
    );
    // And a key asks about the same one, because the answer is cached by the input: a second size
    // would be a second answer, and a guest run on every press.
    let before = a
        .sessions_host
        .borrow()
        .as_ref()
        .expect("a host")
        .list_asks();
    let _ = a.picker_rows();
    assert_eq!(
        a.sessions_host
            .borrow()
            .as_ref()
            .expect("a host")
            .list_asks(),
        before,
        "a keypress reads the list the frame drew"
    );
}

/// The feed's own cap is STATED: a store the feed cannot hold in one answer says how many rows it
/// left out, and the layer draws that line. A list that stops at 250 in silence reads as a daemon
/// holding 250.
#[test]
fn a_feed_over_the_cap_says_how_many_rows_it_left_out() {
    use letibot_plugin::pane::SESSION_FEED_ROWS;
    let mut a = app();
    let sessions: Vec<_> = (0..SESSION_FEED_ROWS + 7)
        .map(|i| {
            let mut b = brief(&format!("s-{i:04}-aaaaaaaa"), &format!("row {i}"), false);
            b.moved_ms = 1 + i as u64;
            b
        })
        .collect();
    a.apply(hello(
        "s-0000-aaaaaaaa",
        sessions,
        Hub::new("s-0000-aaaaaaaa").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    assert_eq!(
        a.sessions_feed()["more"].as_u64(),
        Some(7),
        "the feed counts what it could not hold"
    );
    let drawn = a.picker_lines(100).join("\n");
    assert!(
        drawn.contains("7 more sessions not shown"),
        "and the layer says so on the list:\n{drawn}"
    );
    // And the line is not a stop: nothing can be entered from it, so it takes no number and no id.
    let last = a.picker_rows().last().cloned().expect("rows");
    assert!(
        !a.picker_rows().iter().any(|r| r.id.is_empty()),
        "a line with no id is not a row a key can enter: {last:?}"
    );
}
