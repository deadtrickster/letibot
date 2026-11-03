//! Every notice is ONE line with the whole of it one click away: the fallback behind the card
//! readers, so a session row or hub warning no reader accounts for still draws as a line. The
//! shapes it knows:
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
//! A shape it has not been taught falls back to a lead, the first clause and `click for details`.
//! Nothing is lost: the popup is the text verbatim, and a reader that does not recognise a shape
//! answers `None` rather than guessing which half of a text is the fact.

use rano::style::Role;

use crate::ui::transcript::line_cards::{CLICK, Card, first_clause, flat, sentences};

/// The longest a summarised part of a line is before it is cut with `…` - so the line's other
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

/// **The one line for a session row no card reader and no fold accounts for** - never `None`.
pub(crate) fn session_notice(text: &str) -> Card {
    const READERS: &[fn(&str) -> Option<Card>] = &[
        length_cap,
        todo_check,
        message_from_a_session,
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

/// **Whether a `\n\n` group opens a notice of its own** - where [`notices`] splits a row.
fn opens_notice(group: &str) -> bool {
    const OPENINGS: &[&str] = &[
        "Tool call ",
        "message from your parent session `",
        // **And the other direction** - a child answering, a peer, the session hosting a review.
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
        || crate::ui::transcript::stale_card::stale_of(group).is_some()
        || crate::ui::transcript::line_cards::opens_card(group)
}

/// **A row's notices**: split at each blank line that a notice opens after. A group that does
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

/// **A lead and a whole short text** - a note this head wrote as one sentence for one line
/// already (`not run`, an answer): flattened, not cut at a clause, and cut to the width by
/// [`Card::cut`].
pub(crate) fn whole_parts(lead: &str, role: Role, text: &str) -> Card {
    let said = flat(text);
    card(lead, role, vec![said.clone()], short(&said, PART))
}

/// `Tool call X was not executed: …`, once a call - the length cap's salvage.
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

/// `[todo check] …` - the plan check, as ONE line whatever it carries: the first row of the plan,
/// whose row it is, and how many more are open. The whole check is the popup.
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
        let bullet = items.first()?;
        // **Whose row it is is the bullet's own tail**, ` — yours` / ` — the operator's` / ` —
        // Parent <id>` / ` — the merge queue's`; the advice under it is prose, and prose is
        // reworded.
        let (item, owner, in_progress) = owner_of(bullet);
        parts.push(short(&first_clause(&flat(item)), PART));
        let whose = owner.unwrap_or_else(|| {
            // A row written before the tail was there is read from the advice under it.
            if body.contains("the operator asked for this one")
                || body.contains("operator asked for it")
            {
                "operator's"
            } else if body.contains("your parent asked for this one")
                || body.contains("Your parent asked for it")
            {
                "parent's"
            } else if body.contains("this row is the merge queue's")
                || body.contains("— the merge queue's")
            {
                "queue's"
            } else {
                "yours"
            }
        });
        parts.push(whose.to_string());
        if in_progress {
            parts.push("in progress".to_string());
        }
        // A notice written before R93.B1 carries the count in its head and still says it. The
        // daemon writes none now, so a later one is counted by the rows it names - which is what
        // the card is drawn from, and the only count that notice really has.
        match head.rsplit_once('(').and_then(|(_, m)| {
            m.strip_suffix("more open):")
                .or(m.strip_suffix("more open)"))
        }) {
            Some(more) => parts.push(format!("{}more open", more)),
            None if items.len() > 1 => parts.push(format!("{} rows", items.len())),
            None => {}
        }
    }
    let title = format!("todo · {}", parts.first().cloned().unwrap_or_default());
    Some(card("todo", Role::Attention, parts, title))
}

/// `T2 · the launcher — yours` → the item, whose row it is, and whether it is in progress. The
/// tail is the daemon's own word for the author, so it is read and not guessed at.
fn owner_of(bullet: &str) -> (&str, Option<&'static str>, bool) {
    let b = bullet.trim();
    let (b, in_progress) = match b.strip_suffix(" (in progress)") {
        Some(head) => (head, true),
        None => (b, false),
    };
    let Some((item, tail)) = b.rsplit_once(" — ") else {
        return (b, None, in_progress);
    };
    let whose = match tail.trim() {
        "yours" => Some("yours"),
        "the operator's" => Some("operator's"),
        "the merge queue's" => Some("queue's"),
        t if t.starts_with("Parent ") => Some("parent's"),
        _ => return (b, None, in_progress),
    };
    (item.trim_end(), whose, in_progress)
}

/// `message from your parent session `ID`: ...` and `message from session `ID`: ...` - a parent
/// wrote to its child, or any other session did. Two openings and one card, because both are chat:
/// the words are the same and only the relation differs, so the lead says it (`parent` / `agent`).
fn message_from_a_session(text: &str) -> Option<Card> {
    let (lead_word, rest) = text
        .strip_prefix("message from your parent session `")
        .map(|r| ("parent", r))
        .or_else(|| {
            text.strip_prefix("message from session `")
                .map(|r| ("agent", r))
        })?;
    let (id, said) = rest.split_once("`: ")?;
    let lead = format!("{lead_word} {id}");
    let g = gist(said);
    Some(card(
        &lead,
        Role::Pending,
        vec![g],
        format!("message from {lead_word} {id}"),
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

/// `[context check] you said "…". Your context is 54% full - 547,198 of 999,999 tokens. …`.
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

/// `The merge queue cannot …` / `is holding …` - the queue's older notices no card reads.
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

/// What a monitor's bullet says after the watch's own description: `FIRED after 0.3s - …`.
pub(crate) fn monitor_said(bullet: &str) -> Option<(String, String)> {
    let b = bullet.strip_prefix('`')?;
    let (name, rest) = b.split_once('`')?;
    let said = match rest.find(", declared by turn ") {
        Some(at) => rest[at..].split_once(": ").map_or(rest, |(_, s)| s),
        None => rest,
    };
    Some((name.to_string(), said.trim().to_string()))
}

/// `[monitor] N watch(es) fired:` and its bullets, when the fold did not take it - or, since
/// R93.C8, one `[monitor] \`name\` fired: why.` / `… expired without firing.` line per watch.
fn monitor(text: &str) -> Option<Card> {
    let rest = text.strip_prefix("[monitor] ")?;
    let mut bullets: Vec<(String, String)> = rest
        .lines()
        .filter_map(|l| l.strip_prefix("  - "))
        .filter_map(monitor_said)
        .collect();
    if bullets.is_empty() {
        bullets = text
            .lines()
            .filter_map(|l| l.strip_prefix("[monitor] `"))
            .filter_map(|l| {
                let (name, said) = l.split_once("` ")?;
                let said = said.strip_prefix("fired: ").unwrap_or(said);
                Some((name.to_string(), said.trim_end_matches('.').to_string()))
            })
            .collect();
    }
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

/// `session ID was seated … with a prompt this daemon would not compose now: …` - the stale
/// prompt's sentence, as the session row it is also written as: the `prefix_stale` card's line.
fn stale_session(text: &str) -> Option<Card> {
    crate::ui::transcript::stale_card::stale_card(text)
}

/// `[answer] on "ROW": you asked "…" - the operator chose "X"…`.
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

/// **A System row's one line** - `system · compacted summary (18.7k chars)`, or its first clause.
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

/// A hub warning's one line when no card reads it: `<mark> <code> - <first clause>`, the mark and
/// the code in the code's register - rano's marks survive a pipe and a light theme where colour
/// does not - and a compaction's figures read rather than its sentence repeated.
pub(crate) fn warning_notice(code: &str, detail: &str, mark: &str, role: Role) -> Card {
    // **The code once.** A detail that opens with its own code (`compacted: 958397 → …`) would
    // say it twice on a line that leads with it.
    let body = detail
        .trim()
        .strip_prefix(code)
        .and_then(|r| r.strip_prefix(": "))
        .unwrap_or(detail.trim());
    let said = compaction_of(body)
        .map(|c| compaction_line(&c))
        .unwrap_or_else(|| gist(body));
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

/// **The facts in a compaction's sentence**, or `None` when the text is not that shape.
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
        // **And a notice from the daemon as it writes them now** - no count in the head, because
        // R93.B1 took it out - is counted by the rows it names, which is all it has.
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

    /// R93.C8: the one-line-per-watch shape draws one card, the first watch named.
    #[test]
    fn a_monitor_line_per_watch_is_one_card() {
        let text =
            "[monitor] `ci` fired: the run exited 0.\n[monitor] `log` expired without firing.";
        let c = monitor(text).expect("the new shape is a card");
        assert_eq!(c.title, "monitor · ci");
        let line = &c.line;
        assert!(line.contains("ci the run exited 0"), "{line}");
        assert!(line.contains("1 more"), "{line}");
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

    /// **One honest line per compaction** (R73): the size before and after in the daemon's own
    /// unit, what the summary stands in for, what was kept verbatim and what was dropped at the
    /// seam - the four facts a reader needs from a compaction, and the hint last. The bodies here
    /// are the shapes `compaction_said` (`crates/harnessd/src/sessions.rs`) writes; that sentence
    /// is the contract, and its own tests hold the other end of it.
    #[test]
    fn a_compaction_draws_the_size_the_unit_and_what_it_kept_and_dropped() {
        let ones = |detail: &str| warning_notice("compacted", detail, "·", Role::Faint).line;
        // The unit is the daemon's word and it is carried whole: `deepseek tokens`, never the
        // bare `tokens` the line used to print beside a provider's number.
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
        // **A sentence this head cannot read is not guessed at**: an old log's figures still
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
            "daemon_stopping · `dead` asked this daemon to stop · click for details"
        );
    }
}
