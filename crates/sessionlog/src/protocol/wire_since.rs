//! Every variant on the head wire, and the protocol that first carried it, so a variant added
//! without its `compat::SINCE` row is refused by name rather than only failing a peer at parse
//! time. serde's own "expected one of" list is the live set; this table says WHEN, which serde
//! cannot. `0` is *carried at 78, when this table was written*, and is never right for a new one.

use super::*;

/// The variant names here, sorted, must be exactly what serde reports for the type.
const CLIENT: &[(&str, u32)] = &[
    ("ack", 0),
    ("answer", 0),
    ("answer_question", 0),
    ("answer_todo", 0),
    ("archive_session", 0),
    ("askpass", 0),
    ("attach", 0),
    ("close_session", 0),
    ("compact_session", 0),
    ("detach", 0),
    ("driver_todos", 0),
    ("feedback", 0),
    ("fetch_diagnostic", 0),
    ("fetch_row", 0),
    ("fetch_rows", 0),
    ("github", 0),
    ("interrupt", 0),
    ("list_jobs", 0),
    ("list_merge_queue", 0),
    ("list_notes", 0),
    ("list_sessions", 0),
    ("list_skills", 0),
    ("list_todos", 0),
    ("load_skill", 0),
    ("mode", 0),
    ("mode_confirm", 0),
    ("mode_consent", 0),
    ("new_session", 0),
    ("operator_call", 0),
    ("operator_result", 0),
    ("operator_shell", 0),
    ("pane_action", 0),
    ("pane_close", 0),
    ("pane_open", 0),
    ("peek", 0),
    ("poll_usage", USAGE_POLL_SINCE),
    ("promote", 0),
    ("prompt", 0),
    ("prompt_answer", 0),
    ("read_job_output", 0),
    ("rename_session", 0),
    ("reseat_session", 0),
    ("resume_session", 0),
    ("resync", 0),
    ("room_chat", 0),
    ("screen", 0),
    ("secret", 0),
    ("send_line", 0),
    ("set_operator_todos", 0),
    ("settings", 0),
    ("slash", 0),
    ("stop", 0),
    ("suggest_shell", 0),
    ("switch", 0),
    ("term_close", 0),
    ("term_input", 0),
    ("term_open", 0),
    ("term_resize", 0),
    ("term_status", 0),
    ("unarchive_session", 0),
    ("withdraw_prompts", 0),
];

const SERVER: &[(&str, u32)] = &[
    ("accepted", 0),
    ("bye", 0),
    ("diagnostic", 0),
    ("event", 0),
    ("features", QUEUE_PROMPT_SINCE),
    ("github", 0),
    ("hello", 0),
    ("jobs", 0),
    ("mode_confirm", 0),
    ("mode_settled", 0),
    ("peeked", 0),
    ("queues", 0),
    ("rejected", 0),
    ("resync", 0),
    ("room_chat", 0),
    ("room_chat_pushed", 0),
    ("row_fetched", 0),
    ("rows_fetched", 0),
    ("secret", 0),
    ("sessions", 0),
    ("settings", 0),
    ("shell_suggestions", 0),
    ("skills", 0),
    ("standing_notes", 0),
    ("term_attached", 0),
    ("term_ended", 0),
    ("term_output", 0),
    ("term_status", 0),
    ("todos", 0),
];

const EVENT: &[(&str, u32)] = &[
    ("activity_changed", 0),
    ("citations", 0),
    ("command_issued", 0),
    ("compaction_progress", 0),
    ("decision_answered", 0),
    ("decision_requested", 0),
    ("delta", 0),
    ("denial_raised", 0),
    ("explain", 0),
    ("filling", 0),
    ("head_attached", 0),
    ("head_detached", 0),
    ("held_results", 0),
    ("job_output", 0),
    ("job_settled", 0),
    ("model_retry", 0),
    ("operator_call_allowed", 0),
    ("plugin_pane", 0),
    ("plugin_pane_gone", 0),
    ("prompt_progress", 0),
    ("prompt_requested", 0),
    ("prompt_settled", 0),
    ("provider_trouble", 0),
    ("queue_moved", 0),
    ("screen_requested", 0),
    ("secret_requested", 0),
    ("secret_settled", 0),
    ("session_renamed", 0),
    ("subagent", 0),
    ("tokens_generated", 0),
    ("tool_call_proposed", 0),
    ("tool_finished", 0),
    ("tool_progress", 0),
    ("tool_started", 0),
    ("todos_updated", 0),
    ("transcript_appended", 0),
    ("transcript_content", 0),
    ("transcript_forked", 0),
    ("turn_ended", 0),
    ("turn_failed", 0),
    ("turn_finished", 0),
    ("turn_interrupted", 0),
    ("turn_started", 0),
    ("warning", 0),
];

/// The names serde reports for a type, read off the error an unknown variant produces: the
/// compiler's own list, so a variant this table has not heard of is a failure and not a guess.
fn live<T: serde::de::DeserializeOwned>(probe: &str) -> Vec<String> {
    let said = serde_json::from_str::<T>(probe)
        .err()
        .expect("an unknown variant is refused")
        .to_string();
    let start = said
        .find("expected one of ")
        .unwrap_or_else(|| panic!("{said}"))
        + "expected one of ".len();
    let rest = &said[start..];
    let end = rest.find(" at line ").unwrap_or(rest.len());
    rest[..end]
        .split(',')
        .map(|s| s.trim().trim_matches('`').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn listed(what: &str, table: &[(&str, u32)], names: Vec<String>) {
    let known: Vec<&str> = table.iter().map(|(n, _)| *n).collect();
    for name in &names {
        assert!(
            known.contains(&name.as_str()),
            "`{what}` has a variant `{name}` this table does not know. A new wire variant lands \
             with its version here and a `compat::SINCE` row at that version, because every older \
             head fails to decode it: add (\"{name}\", {PROTOCOL_VERSION}) to `{what}`."
        );
    }
    for name in known {
        assert!(
            names.iter().any(|n| n == name),
            "`{what}` lists `{name}`, which this build no longer has: a rename is a new variant \
             and a `SINCE` row too."
        );
    }
}

/// A variant's version and the `SINCE` row it owes. Two directions, because either half alone
/// passes: a table row without the compat row leaves an older head failing at parse time.
#[test]
fn a_wire_variant_lands_with_its_since_row() {
    listed(
        "client",
        CLIENT,
        live::<ClientFrame>(r#"{"frame":"bogus"}"#),
    );
    listed(
        "server",
        SERVER,
        live::<ServerFrame>(r#"{"frame":"bogus"}"#),
    );
    listed(
        "event",
        EVENT,
        live::<crate::SessionEvent>(r#"{"event":"bogus"}"#),
    );
    for (what, table) in [("client", CLIENT), ("server", SERVER), ("event", EVENT)] {
        for (name, since) in table {
            assert!(
                *since <= PROTOCOL_VERSION,
                "`{what}` says `{name}` arrived at {since}, which is past this build's \
                 {PROTOCOL_VERSION}"
            );
        }
    }
    // And the number itself: a bump is what tells the other half, so it is never free.
    assert!(
        crate::compat::SINCE
            .iter()
            .any(|s| s.version == PROTOCOL_VERSION),
        "protocol {PROTOCOL_VERSION} has no `compat::SINCE` row: something moved the number, and \
         the row is the only place that says what an older head is sent instead"
    );
    for (what, table) in [("client", CLIENT), ("server", SERVER), ("event", EVENT)] {
        for (name, since) in table {
            if *since == 0 {
                continue;
            }
            assert!(
                crate::compat::SINCE.iter().any(|s| s.version == *since),
                "`{what}` says `{name}` arrived at {since}, and `compat::SINCE` has no row there"
            );
        }
    }
}
