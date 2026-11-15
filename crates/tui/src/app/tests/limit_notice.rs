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

#[test]
fn the_newest_limit_notice_counts_the_older_ones() {
    let (mut a, hub) = attached();
    warn(&mut a, &hub, "model_fallback", MOVED);
    warn(&mut a, &hub, "limit_returned", BACK);
    let s = a.screen(160, 30).join("\n");
    assert!(
        s.contains("back on glm-coding/glm-5.3 · 1 more in /notes"),
        "{s}"
    );
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
