//! What a session list says, once: the sentences the head's pane draws and the built-in
//! `sessions` layer answers with, and the two rules both must spell the same way.
//!
//! The head draws the list through a pane, so a sentence one half says and the other half does not
//! is two pictures of one list - which is what this module exists to end. It is compiled into the
//! daemon, the head and every guest, so it may depend on nothing.

/// The list's title. The pane's title and the frame around the list are one line, so *which layer
/// drew this* is never a guess a reader has to make.
pub const TITLE: &str = "sessions in this daemon";

/// The keys that are the HEAD's, whatever a layer says. The row-dependent half of the hint
/// (`enter switches`) is the selected row's own act and comes from the answer.
pub const HINT: [&str; 5] = [
    "↑↓ moves",
    "or type a number or part of a name and press enter",
    "/new [title] makes one",
    "/rename NAME names this one",
    "esc closes",
];

/// The line under the list: what switching does to the session left behind.
pub const NOTE: &str = "switching does not stop anything: a turn keeps running in the session you \
                        left, and it is still there when you come back.";

/// An empty list's sentence - the daemon has not answered, or this head replays a recorded log and
/// has no daemon to ask.
pub const EMPTY: &str = "none listed yet — the daemon has not answered, or this head is replaying \
                         a recorded log and has no daemon to ask.";

/// What the pane draws while the head's stack is being built, which is the one frame it spends
/// before the layer answers. It is a sentence about the LIST, not a second drawing of it: the
/// native tree this replaced was a list that then changed shape under the reader.
pub const BUILDING: &str =
    "building the session list — this head's plugin layers are being loaded.";

/// What the pane draws when no layer answered the hook, or answered a document this head will not
/// draw. The built-in `sessions` layer ships in the binary, so this is a defect. It is one pane row,
/// cut at the frame, so it stays short and the reason goes on the head's notice line.
pub const UNANSWERED: &str =
    "no layer answered the session list, which is a defect; the reason is on the notice line.";

/// The last line of a capped list: how many sessions the feed could not hold. A number a reader
/// can act on is not the same fact as a list that silently stops.
pub fn more(count: usize) -> String {
    format!(
        "{count} more session{} not shown — the list carries {} at a time",
        if count == 1 { "" } else { "s" },
        crate::pane::SESSION_FEED_ROWS
    )
}

/// The list's title with where the cursor stands in it - `sessions in this daemon (3/14)`. A list
/// longer than the screen has to say how long it is and where on it a reader is, or a page key is
/// a key with no answer on the screen.
pub fn counter(at: usize, of: usize) -> String {
    format!("{TITLE} ({at}/{of})")
}

/// A session id cut to the tail a person recognises: `…` then the last eight characters, because
/// ids are minted `s-<nanos>` and two made the same afternoon share a prefix. Short ids are kept
/// whole.
pub fn short_id(id: &str) -> String {
    let n = id.chars().count();
    if n <= 10 {
        return id.to_string();
    }
    let tail: String = id.chars().skip(n - 8).collect();
    format!("…{tail}")
}

/// Control characters out of a string about to be drawn: a title and a workspace are somebody's,
/// and an escape in one would be measured one way and drawn another.
pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// How long ago a session moved, in one word - the right-hand column of a row, and what a feed
/// carries instead of a clock, so a frame that changed nothing asks a guest for nothing.
///
/// Coarse on purpose: the bucket is the cache key's own unit, so a value that moved every
/// millisecond would be a guest run every frame.
pub fn age_word(age_ms: u64) -> String {
    let s = age_ms / 1000;
    match s {
        0..=9 => "now".to_string(),
        10..=59 => format!("{}s", s / 10 * 10),
        60..=3_599 => format!("{}m", s / 60),
        3_600..=86_399 => format!("{}h", s / 3_600),
        86_400..=2_591_999 => format!("{}d", s / 86_400),
        _ => format!("{}mo", s / 2_592_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two drawings this module replaced agreed on nothing but the words they shared, so the
    /// rules are pinned here: an id keeps its tail, and a control character never reaches a screen.
    #[test]
    fn an_id_keeps_its_tail_and_a_title_loses_its_escapes() {
        assert_eq!(short_id("s-habr"), "s-habr");
        assert_eq!(short_id("s-1794526860418261650"), "…18261650");
        assert_eq!(clean("the\u{7}bell\u{1b}[31mred"), "the bell [31mred");
    }

    /// A bucket is what the cache key is allowed to see: two frames a few seconds apart are ONE
    /// value, and the value is readable as an age.
    #[test]
    fn an_age_is_a_bucket_and_not_a_clock() {
        assert_eq!(age_word(0), "now");
        assert_eq!(age_word(9_999), "now");
        assert_eq!(age_word(10_000), "10s");
        assert_eq!(age_word(59_000), "50s");
        assert_eq!(age_word(60_000), "1m");
        assert_eq!(age_word(3_599_000), "59m");
        assert_eq!(age_word(3_600_000), "1h");
        assert_eq!(age_word(86_399_000), "23h");
        assert_eq!(age_word(86_400_000), "1d");
        assert_eq!(age_word(2_592_000_000), "1mo");
        assert_eq!(age_word(15 * 86_400_000), "15d");
    }

    /// The counter is the title and the position together, so a reader knows how long the list is
    /// and where on it they are.
    #[test]
    fn the_counter_is_the_title_and_the_position() {
        assert_eq!(counter(3, 14), format!("{TITLE} (3/14)"));
        assert_eq!(counter(1, 1), format!("{TITLE} (1/1)"));
    }
}
