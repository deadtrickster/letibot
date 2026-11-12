//! Every notice is ONE line with the whole of it one click away: the two lines that are not a
//! notice - a system row and a hub warning - and the frame the session row's notices are drawn in.
//!
//! The notices themselves are `letibot_plugin::card::readers`' now: one copy of the readers, which
//! the built-in `cards` layer is compiled from as well, so a card drawn by the layer and a card drawn
//! by this head are the same card (X22). What stays here is what is the head's: where a row splits
//! into notices, and the shapes only a head has the figures for:
//!
//! ```text
//! system · compacted summary (18.7k chars) · click for details
//! daemon_stopping · `deadtrickster` asked this daemon to stop · click for details
//! compacted · 958k → ≈ 100k deepseek tokens · kept a 13.5k-char summary · nothing dropped · click for details
//! ```
//!
//! A shape nothing has been taught falls back to a lead, the first clause and `click for details`.
//! The popup is the text verbatim.

use rano::style::Role;

#[cfg(test)]
use crate::ui::render::visible_width;
use crate::ui::transcript::line_cards::{CLICK, Card, first_clause, flat, sentences};

/// The longest a summarised part of a line is before it is cut with `…` - so the line's other
/// facts and its hint survive at an ordinary width.
pub(crate) const PART: usize = 60;

/// `s` cut to `n` characters with the head's `…`.
pub(crate) fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

/// `18.7k chars`, `912 chars`.
fn chars(n: usize) -> String {
    format!("{} chars", chars_words(n))
}

/// The number `chars` puts its unit on: `18.7k`, `912`. Shared so a compaction's line and the
/// `system · compacted summary` row beside it cannot disagree about how big a summary is.
fn chars_words(n: usize) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        format!("{:.1}k", n as f64 / 1000.0)
    }
}

/// `958k`, `1.2M`, `512` - a token count as a person reads it.
fn tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1000 => format!("{}k", n / 1000),
        n => n.to_string(),
    }
}

/// A line from its parts: the lead (drawn in `role`), the facts, and the hint last.
fn card(lead: &str, role: Role, parts: Vec<String>, title: String) -> Card {
    let mut all = vec![lead.to_string()];
    all.extend(parts.into_iter().filter(|p| !p.is_empty()));
    all.push(CLICK.to_string());
    Card::new(all.join(" · "), 0, lead, role, title)
}

/// The first sentence of a text, flattened, as its first clause. Not cut: a line whose one
/// fact this is is cut to its width by [`Card::cut`], which keeps the hint.
fn gist(text: &str) -> String {
    let flat = flat(text);
    let first = sentences(&flat).into_iter().next().unwrap_or_default();
    first_clause(first)
}

/// The one line for a notice with no facts and no layer: the readers, and the generic line for a
/// shape none of them knows. What a popup holding only a row's words draws, and what a test asks.
pub(crate) fn session_notice(text: &str) -> Card {
    crate::app::Cards::readers_only().lines(&[text]).remove(0)
}

/// Whether a `\n\n` group opens a notice of its own - where [`notices`] splits a row.
fn opens_notice(group: &str) -> bool {
    const OPENINGS: &[&str] = &[
        "Tool call ",
        "message from your parent session `",
        // And the other direction - a child answering, a peer, the session hosting a review.
        // Without it the row still drew (as the generic notice card) but its words were clipped
        // and its sender was not named.
        "message from session `",
        "The merge queue ",
        "The harness is watching what your tool calls return",
        "Your previous turn was stopped at the context wall",
        "You keep overthinking.",
    ];
    let tagged = group.starts_with('[')
        && group.find("] ").is_some_and(|e| {
            group[1..e]
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == ' ')
        });
    tagged
        || OPENINGS.iter().any(|o| group.starts_with(o))
        // A room message: `#41 [project] from dead (say) to all` and its text under it. It has a
        // reader of its own, and one that draws it as chat rather than as one more notice.
        || letibot_plugin::card::readers::is_room_line(group)
        || crate::ui::transcript::stale_card::stale_of(group).is_some()
        || crate::ui::transcript::line_cards::opens_card(group)
}

/// A row's notices: split at each blank line that a notice opens after. A group that does
/// not open one belongs to the notice above it - its paragraph, which the popup shows.
pub(crate) fn notices(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (at, _) in text.match_indices("\n\n") {
        let next = &text[at + 2..];
        if at > start && opens_notice(next) {
            out.push(&text[start..at]);
            start = at + 2;
        }
    }
    out.push(&text[start..]);
    out
}

/// A lead and a whole short text - a note this head wrote as one sentence for one line
/// already (`not run`, an answer): flattened, not cut at a clause, and cut to the width by
/// [`Card::cut`].
pub(crate) fn whole_parts(lead: &str, role: Role, text: &str) -> Card {
    let said = flat(text);
    card(lead, role, vec![said.clone()], short(&said, PART))
}

/// A System row's one line - `system · compacted summary (18.7k chars)`, or its first clause.
pub(crate) fn system_notice(text: &str, bootstrap: bool) -> Card {
    let n = text.chars().count();
    let said = if bootstrap {
        format!("the prompt the session opened with ({})", chars(n))
    } else if text.starts_with("This conversation was compacted")
        || text.starts_with("Earlier conversation is replaced by this summary")
    {
        format!("compacted summary ({})", chars(n))
    } else if text.starts_with("This conversation was re-seated onto a new prompt") {
        "re-seated onto a new prompt".into()
    } else if text.starts_with("The conversation above is what this record stands in for") {
        format!("compaction instruction ({})", chars(n))
    } else {
        gist(text)
    };
    card(
        "system",
        Role::Faint,
        vec![said.clone()],
        format!("system · {said}"),
    )
}

/// A hub warning's one line when no card reads it: `<mark> <code> - <first clause>`, the mark and
/// the code in the code's register - rano's marks survive a pipe and a light theme where colour
/// does not - and a compaction's figures read rather than its sentence repeated.
pub(crate) fn warning_notice(code: &str, detail: &str, mark: &str, role: Role) -> Card {
    // The code once. A detail that opens with its own code (`compacted: 958397 → …`) would
    // say it twice on a line that leads with it.
    let body = detail
        .trim()
        .strip_prefix(code)
        .and_then(|r| r.strip_prefix(": "))
        .unwrap_or(detail.trim());
    // The facts are in the daemon's own sentence, which is the first paragraph: after a blank line
    // comes the summary body, and model prose that says "nothing dropped" or "CUT OFF" would
    // otherwise rewrite what the line draws.
    let sentence = body.split("\n\n").next().unwrap_or(body);
    let said = compaction_of(sentence)
        .map(|c| compaction_line(&c))
        .unwrap_or_else(|| gist(body));
    // The routine mark is the `·` the frame's own `session ·` already ends in: drawn twice it
    // reads as a stutter, so the quiet register is the code alone (and still not red).
    let lead = if mark == "·" {
        code.to_string()
    } else {
        format!("{mark} {code}")
    };
    // The second half of the line, and never the daemon's sentence: that sentence is cut to its
    // first clause (a reader scanning rows wants the head of it), which is where a remedy usually
    // is - so the remedy is the table's, and it is drawn whole.
    let mut parts = vec![said.clone()];
    if let letibot_sessionlog::warning::Action::Do(action) =
        letibot_sessionlog::warning::action(code)
    {
        parts.push(action.to_string());
    }
    card(
        &lead,
        role,
        parts,
        format!("{code} · {}", short(&said, PART)),
    )
}

/// A compaction's own sentence, read. `compaction_said` writes it and this is the only reader, so
/// the shape is a contract with two ends: the size in the daemon's unit, then what was kept and
/// what was dropped. Only the size is guaranteed, because logs written before this carry it alone.
struct Compaction {
    before: u64,
    after: u64,
    /// The after-count is an estimate until a round on the new base measures it.
    estimated: bool,
    /// The daemon's own word for what the numbers count - `deepseek tokens`, `tokens`.
    unit: String,
    /// Items of the newest exchange carried as they were written.
    verbatim: u64,
    /// How big the summary standing in for the rest of the history is.
    summary_chars: Option<usize>,
    /// Items dropped at the seam because the verbatim part begins inside an exchange.
    /// `None` is a sentence that did not say, which is not the same as `Some(0)`.
    dropped: Option<u64>,
    cut_off: bool,
}

/// The facts in a compaction's sentence, or `None` when the text is not that shape.
fn compaction_of(body: &str) -> Option<Compaction> {
    let (before, rest) = body.split_once(" → ")?;
    let before: u64 = before.trim().parse().ok()?;
    let (estimated, rest) = match rest.strip_prefix('≈') {
        Some(r) => (true, r.trim_start()),
        None => (false, rest),
    };
    let (after, rest) = rest.split_once(' ')?;
    let after: u64 = after.trim().parse().ok()?;
    // The unit runs to the comma that opens the next fact, and its absence means this is not
    // the sentence a compaction writes.
    let (unit, _) = rest.split_once(", ")?;
    Some(Compaction {
        before,
        after,
        estimated,
        unit: unit.trim().to_string(),
        verbatim: count_before(body, " item(s) verbatim").unwrap_or(0),
        summary_chars: count_before(body, "-character summary").map(|n| n as usize),
        dropped: if body.contains("nothing dropped") {
            Some(0)
        } else {
            count_before(body, " item(s) dropped")
        },
        cut_off: body.contains("CUT OFF"),
    })
}

/// The one line a compaction draws: the size before and after in the daemon's unit, what the
/// summary stands in for, what was kept and what was dropped. Ordered by what a reader asks
/// first, and `Card::cut` keeps the size when a narrow terminal cuts the rest.
fn compaction_line(c: &Compaction) -> String {
    let mut parts = vec![format!(
        "{} → {}{} {}",
        tokens(c.before),
        if c.estimated { "≈ " } else { "" },
        tokens(c.after),
        c.unit
    )];
    let kept = match (c.verbatim, c.summary_chars) {
        (0, None) => None,
        (0, Some(chars)) => Some(format!("kept a {}-char summary", chars_words(chars))),
        (n, None) => Some(format!("kept {}", items(n))),
        (n, Some(chars)) => Some(format!(
            "kept {} + a {}-char summary",
            items(n),
            chars_words(chars)
        )),
    };
    parts.extend(kept);
    // `Some(0)` is the sentence saying nothing was dropped, which the line keeps: a reader
    // deciding whether a verbatim tail opens mid-exchange has to be told either way.
    if let Some(dropped) = c.dropped {
        parts.push(match dropped {
            0 => "nothing dropped".to_string(),
            n => format!("{n} dropped"),
        });
    }
    if c.cut_off {
        parts.push("summary CUT OFF".to_string());
    }
    parts.join(" · ")
}

/// `1 item`, `12 items` - a count and its noun, so a drawn line never says `1 item(s)`.
fn items(n: u64) -> String {
    if n == 1 {
        "1 item".to_string()
    } else {
        format!("{n} items")
    }
}

/// The number that ends where `marker` begins - `12` in `kept 12 item(s) verbatim`. A count is
/// only a fact when it is written as one, so a marker with no number in front of it is `None`
/// rather than a zero nobody said.
fn count_before(text: &str, marker: &str) -> Option<u64> {
    number_before(text, marker)?.parse().ok()
}

/// The number before `word` in `s`: `declared 6 item(s)` → `6` for `item`.
fn number_before(s: &str, word: &str) -> Option<String> {
    let at = s.find(word)?;
    let n = s[..at].trim_end().rsplit(' ').next()?;
    n.chars().all(|c| c.is_ascii_digit()).then(|| n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_check_names_its_item_whose_it_is_and_how_many_more() {
        let text = "[todo check] this turn is finished and one item is not done (1 more open):\n  \
                    - review-agent/a-pass (entry s-1): Your branch is cut from x\nthis row is the \
                    merge queue's, not your plan's: it is one entry.\n\nsession s-g was seated \
                    10-10 22:42 with a prompt this daemon would not compose now: the tool schemas \
                    have changed since. Say this.";
        let parts = notices(text);
        assert_eq!(
            parts.len(),
            2,
            "the stale paragraph is its own notice: {parts:?}"
        );
        let todo = session_notice(parts[0]);
        assert_eq!(
            todo.line,
            "todo · review-agent/a-pass (entry s-1)… · queue's · 1 more open · click for details"
        );
        let stale = session_notice(parts[1]);
        assert!(
            stale.line.starts_with("prompt stale · seated 10-10 22:42"),
            "{}",
            stale.line
        );
        let nothing = "[todo check] this turn is finished and NOTHING can start — every item left \
                       waits on another:\n  - `review-main` waits on the child `s-x`\nunblock one \
                       of these rather than starting something new: finish a row.";
        // And a notice from the daemon as it writes them now - no count in the head, because
        // the daemon writes none - is counted by the rows it names, which is all it has.
        let current = "[todo check] this turn is finished and one item is not done:\n  \
                       - review-agent/a-pass (entry s-1): Your branch is cut from x\n  \
                       - review-agent/b-pass (entry s-2): and this one\n  \
                       - review-main (entry red-main-1): and this\nthis row is the merge queue's, \
                       not your plan's: it is one entry.";
        assert_eq!(
            session_notice(current).line,
            "todo · review-agent/a-pass (entry s-1)… · queue's · 3 rows · click for details"
        );
        assert!(
            session_notice(nothing)
                .line
                .starts_with("todo · nothing can start · 1 waits · "),
            "{}",
            session_notice(nothing).line
        );
    }

    /// The one-line-per-watch shape draws one card, the first watch named - and the generic line a
    /// shape no reader knows falls to.
    #[test]
    fn a_monitor_line_per_watch_is_one_card_and_an_unknown_shape_is_generic() {
        let text =
            "[monitor] `ci` fired: the run exited 0.\n[monitor] `log` expired without firing.";
        let c = session_notice(text);
        assert_eq!(c.title, "monitor · ci");
        let line = &c.line;
        assert!(line.contains("ci the run exited 0"), "{line}");
        assert!(line.contains("1 more"), "{line}");
        assert_eq!(
            session_notice("A sentence no reader knows; it goes on.").line,
            "notice · A sentence no reader knows · click for details"
        );
    }

    /// One honest line per compaction: the size before and after in the daemon's unit, what the summary stands in
    /// for, what was kept verbatim and what was dropped at the seam, then the hint. The bodies are the shapes
    /// `compaction_said` (`crates/harnessd/src/sessions.rs`) writes; that sentence is the contract.
    #[test]
    fn a_compaction_draws_the_size_the_unit_and_what_it_kept_and_dropped() {
        let ones = |detail: &str| warning_notice("compacted", detail, "·", Role::Faint).line;
        // The unit is the daemon's word and it is carried whole: `deepseek tokens`, never the
        // bare `tokens` printed beside a provider's number.
        assert_eq!(
            ones(
                "compacted: 958397 → 100005 deepseek tokens, kept 12 item(s) verbatim plus a \
                 13532-character summary, 2 item(s) dropped at the seam, on transcript s#t25."
            ),
            "compacted · 958k → 100k deepseek tokens · kept 12 items + a 13.5k-char summary · \
             2 dropped · click for details"
        );
        // No tail: the summary IS what it kept, and `kept 0 items` would read as a compaction
        // that kept nothing at all.
        assert_eq!(
            ones(
                "compacted: 240000 → 9000 tokens, kept as a 17000-character summary, nothing \
                 dropped, on transcript s#t1."
            ),
            "compacted · 240k → 9k tokens · kept a 17.0k-char summary · nothing dropped · \
             click for details"
        );
        // An estimate keeps its `≈` in front of the number it is about, and says so.
        assert_eq!(
            ones(
                "compacted: 958397 → ≈ 100005 deepseek tokens, kept as a 13532-character \
                 summary, nothing dropped, estimated until the next round measures it, on \
                 transcript s#t25."
            ),
            "compacted · 958k → ≈ 100k deepseek tokens · kept a 13.5k-char summary · nothing \
             dropped · click for details"
        );
        // A summary that came back cut off says so on the one line: it is the fact that changes
        // what the reader can trust, and the popup is not where a reader learns it first.
        assert_eq!(
            ones(
                "compacted: 958397 → 100005 tokens, kept 12 item(s) verbatim plus a 13532-\
                 character summary, nothing dropped, on transcript s#t25. The summary was CUT \
                 OFF at the model's length limit — it is incomplete, and the base says so too."
            ),
            "compacted · 958k → 100k tokens · kept 12 items + a 13.5k-char summary · nothing \
             dropped · summary CUT OFF · click for details"
        );
        // A sentence this head cannot read is not guessed at: an old log's figures still
        // draw, and one with no figures at all falls back to the words it has.
        assert_eq!(
            ones("compacted: 940188 → 9181 tokens, on transcript s#t25."),
            "compacted · 940k → 9k tokens · click for details"
        );
        assert_eq!(
            ones("compacted: the model wrote a poem and nothing else happened."),
            "compacted · the model wrote a poem and nothing else happened · click for details"
        );
    }

    /// The facts a compaction's line draws are read from its own sentence, never from the summary under it. The detail
    /// ends with the summary body, which is model prose, and a summary saying "nothing dropped" or "CUT OFF" must not
    /// rewrite what the daemon reported about the fold.
    #[test]
    fn the_summary_body_under_a_compaction_cannot_rewrite_its_counts() {
        let detail = "compacted: 958397 → 100005 tokens, kept 12 item(s) verbatim plus a \
                      13532-character summary, 2 item(s) dropped at the seam, on transcript s#t25. \
                      Nothing of the summary turn reached this screen, so here is what the model \
                      now reads in place of everything before it:\n\nThe fold left nothing dropped \
                      and nothing was CUT OFF, the summary says, and it is 40 items long.";
        let line = warning_notice("compacted", detail, "·", Role::Faint).line;
        assert!(
            line.contains("2 dropped") && line.contains("12 items"),
            "the summary's prose rewrote the figures the sentence reported: {line}"
        );
        assert!(
            !line.contains("nothing dropped") && !line.contains("CUT OFF"),
            "the summary's prose rewrote the sentence's own facts: {line}"
        );
    }

    #[test]
    fn a_system_row_and_a_warning_are_one_line_and_say_the_code_once() {
        let summary = format!("This conversation was compacted: {}", "x".repeat(18_650));
        assert_eq!(
            system_notice(&summary, false).line,
            "system · compacted summary (18.7k chars) · click for details"
        );
        assert_eq!(
            warning_notice(
                "daemon_stopping",
                "`dead` asked this daemon to stop. Every head detaches.",
                "·",
                Role::Faint
            )
            .line,
            "daemon_stopping · `dead` asked this daemon to stop · `/continue` reopens the session · \
             click for details"
        );
        // The action is the table's and it is drawn WHOLE: the daemon's sentence is cut at its
        // first clause, and a remedy that stopped there would be no remedy at all.
        assert_eq!(
            warning_notice(
                "context_wall",
                "the context is full, and more. `/compact` next.",
                "!",
                Role::Failure
            )
            .line,
            "! context_wall · the context is full… · `/compact` makes the room · click for details"
        );
        // A code with nothing to ask is the sentence alone - no invented line after it.
        assert_eq!(
            warning_notice(
                "compacted",
                "240000 → 9000 tokens, nothing dropped.",
                "·",
                Role::Faint
            )
            .line,
            "compacted · 240k → 9k tokens · nothing dropped · click for details"
        );
    }

    /// A card cut to a width keeps its hint whenever the facts can leave it room, and gives it up
    /// whole when they cannot - the reader who cannot see anything else needs the door first.
    #[test]
    fn a_card_cuts_to_its_width_and_keeps_the_hint() {
        let c = session_notice("[answer] on \"the row\": the operator chose \"Postpone it\"");
        let cut = c.cut(40);
        assert!(cut.text.ends_with("click for details"), "{}", cut.text);
        assert!(visible_width(&cut.text) <= 40, "{}", cut.text);
        let narrow = c.cut(12);
        assert!(
            !narrow.text.contains("click for details"),
            "{}",
            narrow.text
        );
        assert!(visible_width(&narrow.text) <= 12, "{}", narrow.text);
    }
}
