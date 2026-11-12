//! A retried model call is ONE row: it moves in place as the ladder tries, and it settles to how
//! the call ended.
//!
//! The measurement is the frame at eighty columns, which is what a terminal actually is. Before this
//! row existed, three failed attempts were three of these and the count was not on the glass at all:
//!
//! ```text
//!   session · model_endpoint_retry · the model endpoint… · click for details
//!   session · model_endpoint_retry · the model endpoint… · click for details
//!   session · model_endpoint_retry · the model endpoint… · click for details
//! ```
//!
//! And after, one row that moves and then settles:
//!
//! ```text
//!   session · retrying: 2 attempts, 503 server … · click for details
//!   session · retrying: 3 attempts, 503 server … · click for details
//!   session · gave up after 3 attempts: 503 ser… · click for details
//! ```

use super::*;

/// One attempt, as the daemon's durable warning says it - the row a head drew per attempt before
/// the series had a row of its own, kept here as the before half of the comparison.
fn attempt_warning(n: u32) -> SessionEvent {
    SessionEvent::Warning {
        code: "model_endpoint_retry".into(),
        detail: format!(
            "the model endpoint at api.deepseek.com did not answer (attempt {n}, last at \
             10:31:{n:02}): http 503: bad gateway"
        ),
        compaction: None,
    }
}

/// One value of the series, as the daemon publishes it: the live line, then the line it settles to.
fn series(attempts: u32, ended: Option<&str>) -> SessionEvent {
    SessionEvent::ModelRetry {
        host: "api.deepseek.com".into(),
        attempts,
        said: format!(
            "retrying: {attempts} attempts, 503 server error, {}s",
            attempts * 2
        ),
        log: (1..=attempts)
            .map(|i| format!("10:31:{i:02} · 503 server error"))
            .collect(),
        ended: ended.map(str::to_string),
        gone: false,
    }
}

/// The provider's own trouble, as the daemon publishes it: the same fact as a type.
fn trouble(kind: &str, switched_to: Option<&str>) -> SessionEvent {
    SessionEvent::ProviderTrouble {
        kind: kind.into(),
        model: "deepseek/deepseek-chat".into(),
        detail: "http 429: rate limited".into(),
        reset_ms: None,
        count: 1,
        switched_to: switched_to.map(str::to_string),
    }
}

/// The retry line's own events are the whole of a wait: a rate limit, a timeout and a stall file NO
/// row, because the ladder drawing the line is the handling; a spent account is a failure with its
/// action, and a move is said once by `model_fallback` rather than twice.
#[test]
fn a_providers_trouble_is_the_line_and_one_row_at_most() {
    let mut a = app();
    for kind in ["rate_limit", "timeout", "stall"] {
        a.apply(ServerFrame::Event(env(1, trouble(kind, None))));
    }
    assert!(
        !a.notes.iter().any(|(_, n)| matches!(n, Note::Warned(_))),
        "a wait is not a row, whatever kind of wait it is: {:?}",
        a.notes
    );
    // The account cannot pay: one Failure row, whose sentence says what it is and whose action the
    // table supplies.
    a.apply(ServerFrame::Event(env(2, trouble("quota", None))));
    let quota: Vec<&Note> = a
        .notes
        .iter()
        .map(|(_, n)| n)
        .filter(|n| matches!(n, Note::Warned(w) if w.code == "provider_quota"))
        .collect();
    assert_eq!(quota.len(), 1, "a spent account is one row: {:?}", a.notes);
    assert_eq!(
        crate::ui::panes::notes::note_card(quota[0])
            .expect("a warning is a card")
            .0
            .role,
        Role::Failure,
        "and it is red"
    );
    // And a move: the daemon's `model_fallback` row is the one that says it, so a trouble that names
    // where the session went files nothing here.
    let mut b = app();
    b.apply(ServerFrame::Event(env(
        3,
        trouble("quota", Some("glm/glm-4.6")),
    )));
    assert!(
        !b.notes.iter().any(|(_, n)| matches!(n, Note::Warned(_))),
        "a move is one row, and it is `model_fallback`'s: {:?}",
        b.notes
    );
}

/// The line the daemon withdraws: the endpoint came back, or a row of its own says what happened.
fn withdrawn(attempts: u32) -> SessionEvent {
    let SessionEvent::ModelRetry {
        host, said, log, ..
    } = series(attempts, None)
    else {
        unreachable!("`series` answers a retry")
    };
    SessionEvent::ModelRetry {
        host,
        attempts,
        said,
        log,
        ended: None,
        gone: true,
    }
}

/// A row as the glass has it, with the frame's trailing clock taken off: the clock is the machine's
/// and the time zone's, and it is not what this file measures.
fn glass(line: &str) -> &str {
    let row = line.trim_end();
    match row.rsplit_once("  ") {
        Some((text, clock)) if is_clock(clock) => text,
        _ => row,
    }
}

/// `22:13:20`, which is what the frame puts at the end of a row that has a clock.
fn is_clock(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 8
        && b[2] == b':'
        && b[5] == b':'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 2 || i == 5 || c.is_ascii_digit())
}

/// The row the series draws, in any of its three values: the live line, the two endings.
fn retry_rows(frame: &[String]) -> Vec<String> {
    frame
        .iter()
        .map(|l| glass(l).to_string())
        .filter(|l| {
            l.contains("retrying:") || l.contains("gave up after") || l.contains("answered after")
        })
        .collect()
}

#[test]
fn one_retried_call_is_one_row_at_eighty_columns() {
    // ── BEFORE: three attempts, and no row at all ────────────────────────────────────────
    // A warning is not a row of the conversation any more, so three attempts used to be three
    // lines and are now three sentences in `/notes` - which is why the LIVE line below is the
    // one place a retry can be watched.
    let mut before = app();
    for n in 1..=3 {
        before.apply(ServerFrame::Event(env(n as u64, attempt_warning(n))));
    }
    let wall = before.screen(80, 24);
    let had: Vec<&String> = wall
        .iter()
        .filter(|l| l.contains("model_endpoint_retry"))
        .collect();
    assert_eq!(
        had.len(),
        0,
        "an attempt drew a row of the conversation:\n{}",
        wall.join("\n")
    );
    assert_eq!(
        before.notes.len(),
        3,
        "and every attempt is held in `/notes`"
    );

    // ── AFTER: one row, and it MOVES ─────────────────────────────────────────────────────
    let mut after = app();
    after.apply(ServerFrame::Event(env_at(
        1,
        1_700_000_000_000,
        series(2, None),
    )));
    let at_two = after.screen(80, 24);
    let two = retry_rows(&at_two);
    assert_eq!(
        two.len(),
        1,
        "a series is one row from its first frame:\n{}",
        at_two.join("\n")
    );
    assert_eq!(
        two[0], "    session · retrying: 2 attempts, 503 server … · click for details",
        "the row names the count and which failure, which is what this frame keeps"
    );

    // The third attempt moves the SAME row: no second row, and the count on the first one moved.
    after.apply(ServerFrame::Event(env_at(
        2,
        1_700_000_002_000,
        series(3, None),
    )));
    let at_three = after.screen(80, 24);
    let three = retry_rows(&at_three);
    assert_eq!(
        three.len(),
        1,
        "the second publish rewrote the row rather than adding one:\n{}",
        at_three.join("\n")
    );
    assert_eq!(
        three[0], "    session · retrying: 3 attempts, 503 server … · click for details",
        "the same row, at the count the ladder is at"
    );

    // ── And it SETTLES, which is the last value of the same row ──────────────────────────
    after.apply(ServerFrame::Event(env_at(
        3,
        1_700_000_004_000,
        series(3, Some("gave up after 3 attempts: 503 server error")),
    )));
    let settled = after.screen(80, 24);
    let one = retry_rows(&settled);
    assert_eq!(
        one.len(),
        1,
        "a settled series is still one row and still only one:\n{}",
        settled.join("\n")
    );
    assert_eq!(
        one[0], "    session · gave up after 3 attempts: 503 ser… · click for details",
        "the row's last value is how the call ended, as far as this frame keeps it"
    );
    assert!(
        crate::ui::render::visible_width(settled.iter().find(|l| l.contains("gave up")).unwrap())
            <= 80,
        "and it fits the frame it is drawn in: {:?}",
        one[0]
    );

    // ── The same one row, on a frame with room for the whole sentence ─────────────────────
    let wide = after.screen(120, 24);
    let whole = retry_rows(&wide);
    assert_eq!(whole.len(), 1, "one row at any width:\n{}", wide.join("\n"));
    assert_eq!(
        whole[0], "    session · gave up after 3 attempts: 503 server error · click for details",
        "at a hundred and twenty columns nothing of the row is cut"
    );
}

/// A second outage is a second row. The count going back down is the one signal a head has that
/// the daemon has started another series, and rewriting the settled row would hide that the endpoint
/// failed twice rather than for longer.
#[test]
fn a_second_series_is_a_second_row() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        series(2, Some("gave up after 2 attempts: 503 server error")),
    )));
    a.apply(ServerFrame::Event(env(2, series(2, None))));
    let frame = a.screen(80, 24);
    let rows = retry_rows(&frame);
    assert_eq!(
        rows.len(),
        2,
        "the settled row and the new series are two rows:\n{}",
        frame.join("\n")
    );
    assert!(rows[0].contains("gave up after 2 attempts"));
    assert!(rows[1].contains("retrying: 2 attempts"));
}

/// The operator's own words - *deepseek replied on the third try, why should I focus on that* - as a
/// measurement: the line the ladder drew is TAKEN OFF the glass when the daemon withdraws it, and it
/// leaves no row behind. `/notes` does not keep it either, because it happened and nothing came of it.
#[test]
fn a_withdrawn_line_leaves_nothing_on_the_glass_and_nothing_in_notes() {
    let mut a = app();
    a.apply(ServerFrame::Event(env_at(
        1,
        1_700_000_000_000,
        series(2, None),
    )));
    assert_eq!(
        retry_rows(&a.screen(80, 24)).len(),
        1,
        "the premise: the ladder is drawing one line\n{}",
        a.screen(80, 24).join("\n")
    );
    a.apply(ServerFrame::Event(env_at(
        2,
        1_700_000_003_000,
        withdrawn(2),
    )));
    let after = a.screen(80, 24);
    assert!(
        retry_rows(&after).is_empty(),
        "the line is gone and nothing took its place:\n{}",
        after.join("\n")
    );
    assert!(
        !a.notes.iter().any(|(_, n)| matches!(n, Note::Retrying(_))),
        "and it is not filed either: a recovery is not a row anywhere, not even in `/notes`"
    );
}

/// The line is the Warning role, whatever the ladder ends in: something went wrong and is being
/// handled, which is neither the red of a failure nor the quiet of a routine row.
#[test]
fn the_live_line_is_drawn_in_the_warning_role() {
    let mut a = app();
    a.apply(ServerFrame::Event(env_at(
        1,
        1_700_000_000_000,
        series(3, None),
    )));
    let card = a
        .notes
        .iter()
        .find_map(|(_, n)| {
            matches!(n, Note::Retrying(_))
                .then(|| crate::ui::panes::notes::note_card(n))
                .flatten()
        })
        .expect("the line is a card")
        .0;
    assert_eq!(card.role, Role::Warning, "the live line");
}

/// The attempts are behind the row, in the popup a click opens: the row is the one line a frame
/// keeps, and the shape of the outage - every attempt with its clock - is what a reader asking *how
/// bad was it* wants, without six rows of it in the conversation.
#[test]
fn the_attempts_of_the_series_are_one_click_away() {
    let mut a = app();
    a.apply(ServerFrame::Event(env_at(
        1,
        1_700_000_000_000,
        series(3, Some("gave up after 3 attempts: 503 server error")),
    )));
    let popup = newest_notice(&mut a);
    assert!(
        popup.contains("gave up after 3 attempts: 503 server error"),
        "the popup starts with the row's own line, whole: {popup:?}"
    );
    assert!(
        popup.contains("api.deepseek.com"),
        "and it names the endpoint being retried: {popup:?}"
    );
    for n in 1..=3 {
        assert!(
            popup.contains(&format!("10:31:{n:02} · 503 server error")),
            "every failed attempt is listed with its clock, and attempt {n} is missing: {popup:?}"
        );
    }
}
