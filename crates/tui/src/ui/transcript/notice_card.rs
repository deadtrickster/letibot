//! **Every notice is ONE line**, and the whole of it one click away — the fallback behind the
//! card readers, so no notice is drawn as a wall (R89).
//!
//! The operator's standing rule, said of every wall they met: *"make them the usual one liners"*,
//! *"one line + popup"*. The card readers (`red_card`, `review_card`, `asked_card`,
//! `restart_card`, `stale_card`) each read one notice the daemon writes; this module is what
//! draws EVERYTHING ELSE — a session row no reader and no fold accounts for, a hub warning with no
//! card, a System row. It reads the notices it knows well enough to say something better than
//! their first words (a length cap, a plan check, a parent's message, …) and falls back to
//! `<lead> · <first clause> · click for details` for the rest, so a shape nobody has taught this
//! head yet is still one line.
//!
//! # The line
//!
//! ```text
//! length cap · 6902 calls not executed (bash ×6901, read ×1) · click for details
//! todo · review-main (entry red-main-…) · queue's · 2 more open · click for details
//! parent s-gatekeeper · Two new constraints from the operator… · click for details
//! context check · 54% full · not the wall · click for details
//! system · compacted summary (18.7k chars) · click for details
//! daemon_stopping · `deadtrickster` asked this daemon to stop · click for details
//! ! ledger_chain_mismatch · row 41's head is not the one that was stored · click for details
//! ```
//!
//! **Nothing is lost.** The line is a summary; the popup is the text verbatim, and the model
//! reads every word of it as before. A reader that does not recognise a shape answers `None` and
//! the generic line is drawn — never a guess about which half of a text is the fact.

use rano::style::Role;

use crate::ui::transcript::line_cards::{CLICK, Card, first_clause, flat, sentences};

/// The longest a summarised part of a line is before it is cut with `…` — so the line's other
/// facts and its hint survive at an ordinary width.
const PART: usize = 60;

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
    if n < 1000 {
        format!("{n} chars")
    } else {
        format!("{:.1}k chars", n as f64 / 1000.0)
    }
}

/// `958k`, `1.2M`, `512` — a token count as a person reads it.
fn tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1000 => format!("{}k", n / 1000),
        n => n.to_string(),
    }
}

/// **A line from its parts**: the lead (drawn in `role`), the facts, and the hint last.
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

/// **The one line for a session row no card reader and no fold accounts for** — never `None`.
pub(crate) fn session_notice(text: &str) -> Card {
    const READERS: &[fn(&str) -> Option<Card>] = &[
        length_cap,
        todo_check,
        parent_message,
        intent_check,
        context_check,
        merge_queue,
        reseat,
        monitor,
        progress,
        deliverable,
        stale_session,
        answer,
        context_wall,
        overthinking,
    ];
    READERS
        .iter()
        .find_map(|r| r(text))
        .unwrap_or_else(|| generic(text))
}

/// **Whether a `\n\n` group opens a notice of its own** — where [`notices`] splits a row.
fn opens_notice(group: &str) -> bool {
    const OPENINGS: &[&str] = &[
        "Tool call ",
        "message from your parent session `",
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
        || crate::ui::transcript::stale_card::stale_of(group).is_some()
        || crate::ui::transcript::line_cards::opens_card(group)
}

/// **A row's notices**: split at each blank line that a notice opens after. A group that does
/// not open one belongs to the notice above it — its paragraph, which the popup shows.
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

/// `[tag] rest` → `(tag, rest)`.
fn tagged(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix('[')?;
    let (tag, rest) = rest.split_once("] ")?;
    tag.chars()
        .all(|c| c.is_ascii_lowercase() || c == ' ')
        .then_some((tag, rest))
}

/// **The fallback**: the tag (or `notice`), and the first clause of what follows.
pub(crate) fn generic(text: &str) -> Card {
    let (lead, body) = match tagged(text) {
        Some((tag, rest)) => (tag.to_string(), rest),
        None => ("notice".to_string(), text),
    };
    let said = gist(body);
    card(
        &lead,
        Role::Attention,
        vec![said.clone()],
        format!("{lead} · {}", short(&said, PART)),
    )
}

/// **A lead and a whole short text** — a note this head wrote as one sentence for one line
/// already (`not run`, an answer): flattened, not cut at a clause, and cut to the width by
/// [`Card::cut`].
pub(crate) fn whole_parts(lead: &str, role: Role, text: &str) -> Card {
    let said = flat(text);
    card(lead, role, vec![said.clone()], short(&said, PART))
}

/// `Tool call X was not executed: …`, once a call — the length cap's salvage.
fn length_cap(text: &str) -> Option<Card> {
    let mut by: Vec<(String, usize)> = Vec::new();
    let mut n = 0;
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        let tool = l
            .strip_prefix("Tool call ")?
            .split_once(" was not executed")?
            .0;
        n += 1;
        match by.iter_mut().find(|(t, _)| t == tool) {
            Some((_, c)) => *c += 1,
            None => by.push((tool.to_string(), 1)),
        }
    }
    if n == 0 {
        return None;
    }
    by.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let tools = by
        .iter()
        .map(|(t, c)| format!("{t} ×{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let said = format!(
        "{n} call{} not executed ({tools})",
        if n == 1 { "" } else { "s" }
    );
    Some(card(
        "length cap",
        Role::Attention,
        vec![said.clone()],
        format!("length cap · {said}"),
    ))
}

/// `[todo check] …` — the plan check that did not fold to its items.
fn todo_check(text: &str) -> Option<Card> {
    let rest = text.strip_prefix("[todo check] ")?;
    let (head, body) = rest.split_once('\n').unwrap_or((rest, ""));
    let items: Vec<&str> = body
        .lines()
        .filter_map(|l| l.strip_prefix("  - "))
        .collect();
    let mut parts = Vec::new();
    if head.contains("NOTHING can start") {
        parts.push("nothing can start".to_string());
        parts.push(format!(
            "{} wait{}",
            items.len(),
            if items.len() == 1 { "s" } else { "" }
        ));
        if let Some(first) = items.first() {
            parts.push(short(&first_clause(&flat(first)), PART));
        }
    } else {
        let item = items.first()?;
        parts.push(short(&first_clause(&flat(item)), PART));
        let whose = if body.contains("the operator asked for this one")
            || head.contains("the operator added")
        {
            "operator's"
        } else if body.contains("your parent asked for this one") {
            "parent's"
        } else if body.contains("this row is the merge queue's") {
            "queue's"
        } else {
            "yours"
        };
        parts.push(whose.to_string());
        if let Some(more) = head.rsplit_once('(').and_then(|(_, m)| {
            m.strip_suffix("more open):")
                .or(m.strip_suffix("more open)"))
        }) {
            parts.push(format!("{}more open", more));
        }
    }
    let title = format!("todo · {}", parts.first().cloned().unwrap_or_default());
    Some(card("todo", Role::Attention, parts, title))
}

/// `message from your parent session `ID`: …` — what a parent wrote to its child.
fn parent_message(text: &str) -> Option<Card> {
    let rest = text.strip_prefix("message from your parent session `")?;
    let (id, said) = rest.split_once("`: ")?;
    let lead = format!("parent {id}");
    let g = gist(said);
    Some(card(
        &lead,
        Role::Pending,
        vec![g],
        format!("message from parent {id}"),
    ))
}

/// The number before `word` in `s`: `declared 6 item(s)` → `6` for `item`.
fn number_before(s: &str, word: &str) -> Option<String> {
    let at = s.find(word)?;
    let n = s[..at].trim_end().rsplit(' ').next()?;
    n.chars().all(|c| c.is_ascii_digit()).then(|| n.to_string())
}

/// `[intent check] this turn declared N item(s); M tool call(s) succeeded …`.
fn intent_check(text: &str) -> Option<Card> {
    let rest = text.strip_prefix("[intent check] ")?;
    let head = rest.lines().next()?;
    let declared = number_before(head, " item(s)")?;
    let ran = number_before(head, " tool call(s) succeeded").unwrap_or_else(|| "?".into());
    let mut parts = vec![format!("{declared} declared, {ran} ran")];
    if let Some(board) = rest.lines().find_map(|l| l.strip_prefix("board: ")) {
        let (done, of) = board.split_once(" of ")?;
        let of = of.split(' ').next()?;
        parts.push(format!("board {done}/{of}"));
    }
    let title = format!("intent check · {}", parts[0]);
    Some(card("intent check", Role::Attention, parts, title))
}

/// `[context check] you said "…". Your context is 54% full — 547,198 of 999,999 tokens. …`.
fn context_check(text: &str) -> Option<Card> {
    let rest = text.strip_prefix("[context check] ")?;
    let mut parts = Vec::new();
    if let Some((_, after)) = rest.split_once("Your context is ")
        && let Some((pct, _)) = after.split_once(" full")
    {
        parts.push(format!("{pct} full"));
        if rest.contains("That is not the wall") {
            parts.push("not the wall".into());
        }
    } else {
        parts.push(gist(rest));
    }
    if let Some((_, q)) = rest.split_once('"')
        && let Some((quote, _)) = q.split_once('"')
    {
        parts.push(format!("\"{}\"", short(&flat(quote), 40)));
    }
    let title = format!("context check · {}", parts[0]);
    Some(card("context check", Role::Attention, parts, title))
}

/// `The merge queue cannot …` / `is holding …` — the queue's older notices no card reads.
fn merge_queue(text: &str) -> Option<Card> {
    let rest = text.strip_prefix("The merge queue ")?;
    let first = flat(rest.split("\n\n").next()?);
    let first = sentences(&first).into_iter().next()?.to_string();
    // The entry id is the popup's: `(entry `s-…`)` says nothing on a line.
    let mut said = first.clone();
    while let Some(at) = said.find(" (entry ") {
        match said[at..].find(')') {
            Some(end) => said.replace_range(at..at + end + 1, ""),
            None => break,
        }
    }
    let said = short(&first_clause(&said), PART + 20);
    Some(card(
        "‼ queue",
        Role::Failure,
        vec![said.clone()],
        format!("merge queue · {}", short(&said, PART)),
    ))
}

/// `[reseat] The person re-seated this conversation with `/reseat`: …`.
fn reseat(text: &str) -> Option<Card> {
    let rest = text.strip_prefix("[reseat] ")?;
    let flat = flat(rest);
    let mut parts = vec![if flat.contains("carried across as it was") {
        "verbatim".to_string()
    } else if flat.contains("summar") {
        "summarised".to_string()
    } else {
        String::new()
    }];
    let list = |lead: &str| -> Option<String> {
        let (_, after) = flat.split_once(lead)?;
        Some(after.split(". ").next()?.trim_end_matches('.').to_string())
    };
    if let Some(l) = list("You can now call: ") {
        parts.push(format!("new: {l}"));
    }
    if let Some(l) = list("read them again before calling: ") {
        parts.push(format!("changed: {l}"));
    }
    Some(card(
        "reseated",
        Role::Pending,
        parts,
        "reseated · what the model was told".into(),
    ))
}

/// What a monitor's bullet says after the watch's own description: `FIRED after 0.3s — …`.
pub(crate) fn monitor_said(bullet: &str) -> Option<(String, String)> {
    let b = bullet.strip_prefix('`')?;
    let (name, rest) = b.split_once('`')?;
    let said = match rest.find(", declared by turn ") {
        Some(at) => rest[at..].split_once(": ").map_or(rest, |(_, s)| s),
        None => rest,
    };
    Some((name.to_string(), said.trim().to_string()))
}

/// `[monitor] N watch(es) fired:` and its bullets, when the fold did not take it.
fn monitor(text: &str) -> Option<Card> {
    let rest = text.strip_prefix("[monitor] ")?;
    let bullets: Vec<(String, String)> = rest
        .lines()
        .filter_map(|l| l.strip_prefix("  - "))
        .filter_map(monitor_said)
        .collect();
    let (name, said) = bullets.first()?;
    let mut parts = vec![format!("{name} {}", short(&first_clause(said), PART))];
    if bullets.len() > 1 {
        parts.push(format!("{} more", bullets.len() - 1));
    }
    Some(card(
        "monitor",
        Role::Attention,
        parts,
        format!("monitor · {name}"),
    ))
}

/// `The harness is watching what your tool calls return, … the last 4 rounds ran 4 calls and 0
/// of 4 returned a result this turn had not already seen …`.
fn progress(text: &str) -> Option<Card> {
    let rest = text.strip_prefix("The harness is watching what your tool calls return")?;
    let (_, after) = rest.split_once("the last ")?;
    let rounds = after.split(' ').next()?;
    let calls = number_before(after, " calls and")?;
    let (_, nothing) = after.split_once(" calls and ")?;
    let fresh = nothing.split(' ').next()?;
    let said = format!("last {rounds} rounds: {calls} calls, {fresh} returned anything new");
    Some(card(
        "progress",
        Role::Attention,
        vec![said.clone()],
        format!("progress · {said}"),
    ))
}

/// `[the deliverable] Your worktree `…` has uncommitted changes, so the branch `B` does not hold
/// them: FILES. …`.
fn deliverable(text: &str) -> Option<Card> {
    let rest = text.strip_prefix("[the deliverable] ")?;
    let mut parts = Vec::new();
    if let Some((_, b)) = rest.split_once("the branch `")
        && let Some((branch, _)) = b.split_once('`')
    {
        parts.push(branch.to_string());
    }
    match rest.split_once("does not hold them: ") {
        Some((_, files)) => {
            let files = files.split(". ").next().unwrap_or(files);
            parts.push(format!("uncommitted: {}", short(files, 40)));
        }
        None => parts.push(gist(rest)),
    }
    Some(card(
        "deliverable",
        Role::Attention,
        parts,
        "the deliverable".into(),
    ))
}

/// `session ID was seated … with a prompt this daemon would not compose now: …` — the stale
/// prompt's sentence, as the session row it is also written as: the `prefix_stale` card's line.
fn stale_session(text: &str) -> Option<Card> {
    crate::ui::transcript::stale_card::stale_card(text)
}

/// `[answer] on "ROW": you asked "…" — the operator chose "X"…`.
fn answer(text: &str) -> Option<Card> {
    let rest = text.strip_prefix("[answer] on \"")?;
    let (row, after) = rest.split_once("\": ")?;
    let mut parts = Vec::new();
    if let Some((_, c)) = after.rsplit_once("the operator chose \"") {
        let chose = c.split('"').next().unwrap_or(c);
        parts.push(format!("chose \"{}\"", short(chose, 30)));
    }
    parts.push(format!("on \"{}\"", short(&flat(row), 40)));
    Some(card(
        "answer",
        Role::Pending,
        parts,
        "the operator's answer".into(),
    ))
}

/// `Your previous turn was stopped at the context wall and the conversation was then compacted…`.
fn context_wall(text: &str) -> Option<Card> {
    text.starts_with("Your previous turn was stopped at the context wall")
        .then(|| {
            card(
                "context wall",
                Role::Pending,
                vec!["compacted, continuing from the summary".into()],
                "context wall".into(),
            )
        })
}

/// `You keep overthinking. … (reasoning_only) …`.
fn overthinking(text: &str) -> Option<Card> {
    let rest = text.strip_prefix("You keep overthinking.")?;
    let why = rest
        .split_once('(')
        .and_then(|(_, w)| w.split_once(')'))
        .map(|(w, _)| w.replace('_', "-"))
        .unwrap_or_else(|| "reasoning only".into());
    Some(card(
        "nudge",
        Role::Attention,
        vec![format!("{why} turn, told to answer first")],
        "nudge · overthinking".into(),
    ))
}

/// **A System row's one line** — `system · compacted summary (18.7k chars)`, or its first clause.
pub(crate) fn system_notice(text: &str, bootstrap: bool) -> Card {
    let n = text.chars().count();
    let said = if bootstrap {
        format!("the prompt the session opened with ({})", chars(n))
    } else if text.starts_with("This conversation was compacted") {
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

/// **A hub warning's one line** when no card reads it: `<mark> <code> · <first clause>`, the mark
/// and the code in the code's register (rano's marks: `×` refused, `!` failure — which survive a
/// pipe and a light theme where colour does not — and the routine `·`, which is the frame's own)
/// — and a compaction's figures read rather than its sentence repeated.
pub(crate) fn warning_notice(code: &str, detail: &str, mark: &str, role: Role) -> Card {
    // **The code once.** A detail that opens with its own code (`compacted: 958397 → …`) would
    // say it twice on a line that leads with it.
    let body = detail
        .trim()
        .strip_prefix(code)
        .and_then(|r| r.strip_prefix(": "))
        .unwrap_or(detail.trim());
    let said = compaction_figures(body).unwrap_or_else(|| gist(body));
    // The routine mark is the `·` the frame's own `session ·` already ends in: drawn twice it
    // reads as a stutter, so the quiet register is the code alone (and still not red).
    let lead = if mark == "·" {
        code.to_string()
    } else {
        format!("{mark} {code}")
    };
    card(
        &lead,
        role,
        vec![said.clone()],
        format!("{code} · {}", short(&said, PART)),
    )
}

/// `958397 → 100005 tokens, …` → `958k → 100k tokens`.
fn compaction_figures(body: &str) -> Option<String> {
    let (before, rest) = body.split_once(" → ")?;
    let (after, _) = rest.split_once(" tokens")?;
    let before: u64 = before.trim().parse().ok()?;
    let after: u64 = after.trim().parse().ok()?;
    Some(format!("{} → {} tokens", tokens(before), tokens(after)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(c: &Card) -> &str {
        &c.line
    }

    #[test]
    fn a_length_cap_salvage_is_one_line_with_its_counts() {
        let mut text = String::from(
            "Tool call read was not executed: the response hit the output token limit, so its \
             arguments may be truncated. Re-issue the tool call with complete arguments.",
        );
        for _ in 0..6901 {
            text.push_str(
                "\nTool call bash was not executed: the response hit the output token limit, so \
                 its arguments may be truncated. Re-issue the tool call with complete arguments.",
            );
        }
        assert_eq!(
            line(&session_notice(&text)),
            "length cap · 6902 calls not executed (bash ×6901, read ×1) · click for details"
        );
    }

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
        assert!(
            session_notice(nothing)
                .line
                .starts_with("todo · nothing can start · 1 waits · "),
            "{}",
            session_notice(nothing).line
        );
    }

    #[test]
    fn the_readers_say_the_facts_and_the_fallback_says_the_first_clause() {
        let cases = [
            (
                "message from your parent session `s-g`: Two new constraints from the operator, \
                 fold them into the plan.\n\n1. REPLICATION",
                "parent s-g · Two new constraints from the operator, fold them into the plan · \
                 click for details",
            ),
            (
                "[intent check] this turn declared 6 item(s); 0 tool call(s) succeeded and 0 ran \
                 without producing a result.\n  - #1 \"x\" was declared\nboard: 0 of 6 complete — \
                 0 in progress",
                "intent check · 6 declared, 0 ran · board 0/6 · click for details",
            ),
            (
                "[context check] you said \"my context is nearly out\". Your context is 54% full — \
                 547,198 of 999,999 tokens. That is not the wall, … Continue.",
                "context check · 54% full · not the wall · \"my context is nearly out\" · click \
                 for details",
            ),
            (
                "The harness is watching what your tool calls return, not how many you make, and \
                 the last 4 rounds ran 8 calls and 0 of 8 returned a result this turn had not \
                 already seen (8 ok).",
                "progress · last 4 rounds: 8 calls, 0 returned anything new · click for details",
            ),
            (
                "[the deliverable] Your worktree `/w` has uncommitted changes, so the branch \
                 `agent/x` does not hold them: ?? a.jsonl. The branch is the deliverable.",
                "deliverable · agent/x · uncommitted: ?? a.jsonl · click for details",
            ),
            (
                "[monitor] 1 watch(es) fired:\n  - `ci` (command `gh run watch` exiting), \
                 declared by turn s#t8#226: FIRED after 0.3s — the command exited 2\nOnly a FIRED \
                 watch is an answer about the world.",
                "monitor · ci FIRED after 0.3s… · click for details",
            ),
            (
                "[answer] on \"GAP (latency)\": you asked \"Postpone?\" — the operator chose \
                 \"Postpone it\"",
                "answer · chose \"Postpone it\" · on \"GAP (latency)\" · click for details",
            ),
            (
                "[reseat] The person re-seated this conversation with `/reseat`: the conversation \
                 was carried across as it was, nothing summarised. You can now call: web_fetch. \
                 The notice no longer applies.",
                "reseated · verbatim · new: web_fetch · click for details",
            ),
            (
                "You keep overthinking. Cut it short. Your last turn spent its whole output on \
                 reasoning (reasoning_only) and nothing was recorded.",
                "nudge · reasoning-only turn, told to answer first · click for details",
            ),
            (
                "[something new] the daemon learned to say this, and more. Second sentence.",
                "something new · the daemon learned to say this… · click for details",
            ),
            (
                "A sentence no reader knows; it goes on.",
                "notice · A sentence no reader knows · click for details",
            ),
        ];
        for (text, want) in cases {
            assert_eq!(session_notice(text).line, want, "{text}");
        }
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
                "compacted",
                "compacted: 958397 → 100005 tokens, on transcript s#t25. The rest.",
                "·",
                Role::Faint
            )
            .line,
            "compacted · 958k → 100k tokens · click for details"
        );
        assert_eq!(
            warning_notice(
                "daemon_stopping",
                "`dead` asked this daemon to stop. Every head detaches.",
                "·",
                Role::Faint
            )
            .line,
            "daemon_stopping · `dead` asked this daemon to stop · click for details"
        );
    }
}
