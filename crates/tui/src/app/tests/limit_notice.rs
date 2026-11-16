//! A model move, park or return a usage limit caused stays above the composer until dismissed.

use super::*;

const MOVED: &str = "[fallback] glm-coding/glm-5.3 reset 07:15:00 UTC is past wait_if_within 45m - \
                     on deepseek/deepseek-v4-flash until then. The daemon opens the next turn \
                     there by itself.";
const BACK: &str =
    "the limit on glm-coding/glm-5.3 lifted: model glm-coding/glm-5.3 answers from the next turn";

fn warn(a: &mut App, hub: &Hub, code: &str, detail: &str) {
    a.apply(ServerFrame::Event(hub.publish(SessionEvent::Warning {
        code: code.into(),
        detail: detail.into(),
        compaction: None,
    })));
}

fn attached() -> (App, std::sync::Arc<Hub>) {
    let hub = Hub::new("s");
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    (a, hub)
}

const SWITCH_LINE: &str =
    "glm-coding/glm-5.3 hit its limit (resets 07:15:00 UTC) - now on deepseek/deepseek-v4-flash";

#[test]
fn a_limit_switch_raises_a_notice_that_stays_until_dismissed() {
    let (mut a, hub) = attached();
    warn(&mut a, &hub, "model_fallback", MOVED);
    let first = a.screen(140, 30).join("\n");
    assert!(first.contains(SWITCH_LINE), "{first}");
    // Frames pass and the transient clock runs out: the notice is still there.
    for t in [5_000, 60_000, 3_600_000] {
        a.clock(t);
        let s = a.screen(140, 30).join("\n");
        assert!(s.contains(SWITCH_LINE), "gone at {t}ms:\n{s}");
    }
    // Not a row of the conversation: the record stays the model's.
    assert!(
        !a.hist_lines.join("\n").contains("hit its limit"),
        "{}",
        a.hist_lines.join("\n")
    );
    assert_eq!(a.key(Key::CtrlN), None);
    let after = a.screen(140, 30).join("\n");
    assert!(!after.contains("hit its limit"), "{after}");
    // Dismissed, and still held in `/notes`.
    assert!(
        a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == "model_fallback")),
    );
}

#[test]
fn the_switch_back_raises_its_own_notice() {
    let (mut a, hub) = attached();
    warn(&mut a, &hub, "model_fallback", MOVED);
    let _ = a.screen(140, 30);
    assert_eq!(a.key(Key::CtrlN), None);
    warn(&mut a, &hub, "limit_returned", BACK);
    let s = a.screen(140, 30).join("\n");
    assert!(
        s.contains("glm-coding/glm-5.3's limit lifted - back on glm-coding/glm-5.3"),
        "{s}"
    );
    assert!(!s.contains("hit its limit"), "{s}");
    assert_eq!(a.key(Key::CtrlN), None);
    let s = a.screen(140, 30).join("\n");
    assert!(!s.contains("limit lifted"), "{s}");
}

/// A return is said once, by the limit notice: the status line does not say it a second time.
#[test]
fn limit_returned_is_drawn_once() {
    let (mut a, hub) = attached();
    warn(&mut a, &hub, "limit_returned", BACK);
    assert!(
        a.notice.is_none(),
        "the status line says the return too: {:?}",
        a.notice
    );
    let s = a.screen(140, 30).join("\n");
    assert_eq!(s.matches("limit lifted").count(), 1, "{s}");
}

/// One episode, one notice: each newer limit note replaces the one before in place, with no
/// pile counted beside it.
#[test]
fn a_limit_episode_is_one_notice_updated_in_place() {
    let (mut a, hub) = attached();
    warn(&mut a, &hub, "model_fallback", MOVED);
    warn(&mut a, &hub, "limit_returned", BACK);
    let s = a.screen(160, 30).join("\n");
    assert!(s.contains("back on glm-coding/glm-5.3 · ctrl-n"), "{s}");
    assert!(!s.contains("more in /notes"), "{s}");
    assert!(!s.contains("hit its limit"), "{s}");
}

const PARK: &str = "kimi-code/k2 said `429 usage limit`, so this session is paused until \
                    13:07:09 UTC. Nothing is lost - the daemon opens the turn by itself and a \
                    prompt sent meanwhile waits with it.";
const RESUMED: &str = "the limit on kimi-code/k2 lifted, so the turn it stopped goes on by itself \
                       - nothing was sent to make that happen";

/// One event at `ts` on the daemon's clock, in the order the log gives it.
fn at(a: &mut App, ts: u64, event: SessionEvent) {
    let seq = a.seq + 1;
    a.apply(ServerFrame::Event(env_at(seq, ts, event)));
}

fn said(code: &str, detail: &str) -> SessionEvent {
    SessionEvent::Warning {
        code: code.into(),
        detail: detail.into(),
        compaction: None,
    }
}

fn parked_turn(a: &mut App) {
    at(a, 10, testing::turn_started("t1"));
    at(
        a,
        20,
        SessionEvent::ActivityChanged {
            activity: letibot_sessionlog::activity::Activity::of(vec![
                letibot_sessionlog::activity::Waiting::Limit(
                    "parked on kimi-code/k2's limit until 13:07:09 UTC - esc ends the turn, \
                     /models switches"
                        .into(),
                ),
            ]),
        },
    );
    at(a, 30, said("limit_park", PARK));
    at(
        a,
        30,
        SessionEvent::TurnInterrupted {
            turn_id: "t1".into(),
            reason: "paused until 13:07:09 UTC".into(),
            partial_kept: true,
            park: Some(letibot_sessionlog::event::TurnPark {
                model: "kimi-code/k2".into(),
                until_ms: letibot_sessionlog::event::now_ms() + 3_600_000,
                retry_at_ms: None,
            }),
        },
    );
}

/// rano showed one park three times, in two zones. The parked turn's line is the one.
#[test]
fn one_park_is_said_once_in_local_time() {
    let (mut a, _hub) = attached();
    parked_turn(&mut a);
    let s = a.screen(160, 30);
    let said: Vec<&String> = s
        .iter()
        .filter(|l| l.contains("kimi-code/k2") || l.contains("paused until"))
        .collect();
    assert_eq!(said.len(), 1, "{}", s.join("\n"));
    assert!(said[0].contains("(local time)"), "{}", s.join("\n"));
    assert!(!s.join("\n").contains("UTC"), "{}", s.join("\n"));
}

/// The lift says the stopped turn goes on, and closes itself once that turn has ended.
#[test]
fn the_lift_notice_closes_once_the_stopped_turn_has_ended() {
    let (mut a, _hub) = attached();
    parked_turn(&mut a);
    at(&mut a, 40, said("limit_resumed", RESUMED));
    at(&mut a, 50, testing::turn_started("t2"));
    let s = a.screen(160, 30).join("\n");
    assert!(
        s.contains("kimi-code/k2's limit lifted - the turn goes on"),
        "{s}"
    );
    at(
        &mut a,
        60,
        SessionEvent::TurnEnded {
            turn_id: "t2".into(),
        },
    );
    let s = a.screen(160, 30).join("\n");
    assert!(!s.contains("limit lifted"), "{s}");
    assert!(!s.contains("goes on"), "{s}");
}

#[test]
fn a_park_on_a_limit_reads_as_one_line() {
    let detail = "glm-coding/glm-5.3 said `429 usage limit, resets at 2026-11-15 07:15:00`, so \
                  this session is paused until 07:15:02 UTC. Nothing is lost - the daemon opens \
                  the turn by itself and a prompt sent meanwhile waits with it.";
    assert_eq!(
        crate::app::limit_notice::limit_words("limit_park", detail),
        "glm-coding/glm-5.3 hit its limit - paused until 07:15:02 UTC"
    );
    let outage = "the model at glm/glm-5.3 is out — 503 — and the retry ladder has nothing left \
                  to wait for, so this turn ends here and the session moves to deepseek/x, the \
                  first name in `[fallback]` that answers.";
    assert_eq!(
        crate::app::limit_notice::limit_words("model_fallback", outage),
        "glm/glm-5.3 is out - now on deepseek/x"
    );
}
