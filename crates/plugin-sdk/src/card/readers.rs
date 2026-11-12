//! The card readers every notice in the tree is drawn by: the text-prefix shapes a notice takes,
//! read into a [`Lines`]. The head and the built-in `cards` layer compile them from one source,
//! because two copies would be two drawings of one notice.

use super::Lines;
use crate::pane::Role;

/// What every card's line ends with: the row is a summary, and the popup is the text.
pub const CLICK: &str = "click for details";

/// The longest a summarised part of a line is before it is cut with `…` - so the line's other
/// facts and its hint survive at an ordinary width.
pub const PART: usize = 60;

/// `s` cut to `n` characters with the head's `…`.
pub fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

/// A text flattened to one line of single spaces.
pub fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The sentences of a flattened text: ended by `. `, `; `, `! ` or `? `, trimmed.
pub fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = text.as_bytes();
    for i in 0..b.len() {
        if matches!(b[i], b'.' | b';' | b'!' | b'?') && b.get(i + 1) == Some(&b' ') {
            let s = text[start..i].trim();
            if !s.is_empty() {
                out.push(s);
            }
            start = i + 2;
        }
    }
    let s = text[start.min(text.len())..]
        .trim()
        .trim_end_matches(['.', ';']);
    if !s.is_empty() {
        out.push(s);
    }
    out
}

/// A sentence's first clause - cut at the first `, and` / `, so` / `, which` / `: ` / ` - `
/// far enough in to be a clause, and a cut clause says so with `…`.
pub fn first_clause(sentence: &str) -> String {
    /// A clause shorter than this is not the reason yet (`BUT, as`, `So: `).
    const AT_LEAST: usize = 12;
    const MARKS: &[&str] = &[", and ", ", so ", ", which ", ": ", " — "];
    let cut = MARKS
        .iter()
        .flat_map(|m| sentence.match_indices(m).map(|(i, _)| i))
        .filter(|&i| i >= AT_LEAST)
        .min();
    match cut {
        Some(i) => format!("{}…", &sentence[..i]),
        None => sentence.to_string(),
    }
}

/// A line from its parts: the lead (drawn in `role`), the facts, and the hint last.
fn card(lead: &str, role: Role, parts: Vec<String>, title: String) -> Lines {
    let mut all = vec![lead.to_string()];
    all.extend(parts.into_iter().filter(|p| !p.is_empty()));
    all.push(CLICK.to_string());
    Lines {
        parts: all,
        accent: 0,
        role: Some(role),
        title,
    }
}

/// The first sentence of a text, flattened, as its first clause. Not cut: a line whose one
/// fact this is is cut to its width by the host, which keeps the hint.
pub fn gist(text: &str) -> String {
    let flat = flat(text);
    let first = sentences(&flat).into_iter().next().unwrap_or_default();
    first_clause(first)
}

/// The one line for a notice, read from its opening words - `None` for a shape no reader knows,
/// which is the host's cue to draw its generic line. The first reader that recognises a text is
/// the one that says what it is.
pub fn read(text: &str) -> Option<Lines> {
    const READERS: &[fn(&str) -> Option<Lines>] = &[
        length_cap,
        todo_check,
        from_the_parent,
        message_from_a_session,
        intent_check,
        context_check,
        merge_queue,
        reseat,
        monitor,
        progress,
        deliverable,
        answer,
        context_wall,
        overthinking,
    ];
    READERS.iter().find_map(|r| r(text))
}

/// `Tool call X was cut off and not run. …`, once a call - the length cap's salvage. Both wordings
/// are read because stored transcripts still carry the older one.
fn length_cap(text: &str) -> Option<Lines> {
    const AFTER: &[&str] = &[" was cut off and not run", " was not executed"];
    let mut by: Vec<(String, usize)> = Vec::new();
    let mut n = 0;
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        let rest = l.strip_prefix("Tool call ")?;
        let tool = AFTER.iter().find_map(|a| rest.split_once(a))?.0;
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

/// `[todo check] …` - the plan check, as one line whatever it carries: the first row of the plan,
/// whose row it is, and how many more are open. The whole check is the popup.
fn todo_check(text: &str) -> Option<Lines> {
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
        // Whose row it is is the bullet's own tail, ` - yours` / ` - the operator's` / ` -
        // Parent <id>` / ` - the merge queue's`; the advice under it is prose, and prose is
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
        // A notice that carries the count in its head says it; one that does not is counted by
        // the rows it names, which is what the card is drawn from.
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

/// `T2 · the launcher - yours` → the item, whose row it is, and whether it is in progress. The
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

/// `[parent] ...` - what the parent session said to this child. The relation
/// is the tag and the id is gone, so the card names the edge and not a session to act on.
fn from_the_parent(text: &str) -> Option<Lines> {
    let said = text.strip_prefix("[parent] ")?;
    Some(card(
        "parent",
        Role::Pending,
        vec![gist(said)],
        "a message from your parent session".into(),
    ))
}

/// `message from your parent session `ID`: ...` and `message from session `ID`: ...` - a parent
/// wrote to its child, or any other session did. Two openings and one card, because both are chat:
/// the words are the same and only the relation differs, so the lead says it (`parent` / `agent`).
fn message_from_a_session(text: &str) -> Option<Lines> {
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
fn intent_check(text: &str) -> Option<Lines> {
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
fn context_check(text: &str) -> Option<Lines> {
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
fn merge_queue(text: &str) -> Option<Lines> {
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
fn reseat(text: &str) -> Option<Lines> {
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
pub fn monitor_said(bullet: &str) -> Option<(String, String)> {
    let b = bullet.strip_prefix('`')?;
    let (name, rest) = b.split_once('`')?;
    let said = match rest.find(", declared by turn ") {
        Some(at) => rest[at..].split_once(": ").map_or(rest, |(_, s)| s),
        None => rest,
    };
    Some((name.to_string(), said.trim().to_string()))
}

/// One watch's news, as a card draws it: the composed firing sentence read down to its own words -
/// `[monitor] `ci` fired: the run exited 0.` -> `the run exited 0`. The facts carry the sentence
/// too, which is what makes the facts-drawn row and the text-drawn one the same line.
pub fn firing_said(said: &str) -> String {
    let said = said.strip_prefix("[monitor] ").unwrap_or(said);
    let said = match said.split_once("` ") {
        Some((_, rest)) => rest,
        None => said,
    };
    let said = said.strip_prefix("fired: ").unwrap_or(said);
    said.trim_end_matches('.').to_string()
}

/// `[monitor] N watch(es) fired:` and its bullets, when the fold did not take it - or
/// one `[monitor] \`name\` fired: why.` / `… expired without firing.` line per watch.
fn monitor(text: &str) -> Option<Lines> {
    let rest = text.strip_prefix("[monitor] ")?;
    let mut bullets: Vec<(String, String)> = rest
        .lines()
        .filter_map(|l| l.strip_prefix("  - "))
        .filter_map(monitor_said)
        .collect();
    if bullets.is_empty() {
        bullets = text
            .lines()
            .filter(|l| l.starts_with("[monitor] `"))
            .filter_map(|l| {
                let rest = l.strip_prefix("[monitor] `")?;
                let (name, _) = rest.split_once('`')?;
                Some((name.to_string(), firing_said(l)))
            })
            .collect();
    }
    monitor_card(&bullets)
}

/// The card a monitor's news draws, from the watches and what each said - one call whatever shape
/// the text came in, so a firing the head reads as one line and a firing the daemon hands over as
/// typed facts are the same card.
pub fn monitor_card(bullets: &[(String, String)]) -> Option<Lines> {
    let (name, said) = bullets.first()?;
    let mut parts = vec![format!("{name} {}", short(&first_clause(said), PART))];
    if bullets.len() > 1 {
        parts.push(format!("{} more", bullets.len() - 1));
    }
    // A watch firing or expiring is news and nothing is asked of the reader: `info`, which is the
    // severity for exactly that, rather than a dim that says the line is structure.
    Some(card(
        "monitor",
        Role::Info,
        parts,
        format!("monitor · {name}"),
    ))
}

/// `Your last 4 rounds returned nothing new. …` - the progress nudge. The sentence before
/// once opened `The harness is watching what your tool calls return` and is still in every
/// stored transcript.
fn progress(text: &str) -> Option<Lines> {
    let said = match text.strip_prefix("Your last ") {
        Some(rest) => {
            let rounds = rest.split(' ').next()?;
            format!("last {rounds} rounds: nothing new")
        }
        None => {
            let rest = text.strip_prefix("The harness is watching what your tool calls return")?;
            let (_, after) = rest.split_once("the last ")?;
            let rounds = after.split(' ').next()?;
            let calls = number_before(after, " calls and")?;
            let (_, nothing) = after.split_once(" calls and ")?;
            let fresh = nothing.split(' ').next()?;
            format!("last {rounds} rounds: {calls} calls, {fresh} returned anything new")
        }
    };
    Some(card(
        "progress",
        Role::Attention,
        vec![said.clone()],
        format!("progress · {said}"),
    ))
}

/// `[the deliverable] Your worktree `…` has uncommitted changes, so the branch `B` does not hold
/// them: FILES. …`.
fn deliverable(text: &str) -> Option<Lines> {
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

/// `[answer] on "ROW": you asked "…" - the operator chose "X"…`.
fn answer(text: &str) -> Option<Lines> {
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
fn context_wall(text: &str) -> Option<Lines> {
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
fn overthinking(text: &str) -> Option<Lines> {
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

/// The generic line a notice no reader accounts for draws: the tag (or `notice`), and the first
/// clause of what follows. It is `info`: a shape nobody has taught a reader is not a warning, and
/// drawing it yellow is how an untaught tag came to look like something to act on.
pub fn generic(text: &str) -> Lines {
    let (lead, body) = match tagged(text) {
        Some((tag, rest)) => (tag.to_string(), rest),
        None => ("notice".to_string(), text),
    };
    let said = gist(body);
    card(
        &lead,
        Role::Info,
        vec![said.clone()],
        format!("{lead} · {}", short(&said, PART)),
    )
}

/// `[tag] rest` → `(tag, rest)`.
fn tagged(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix('[')?;
    let (tag, rest) = rest.split_once("] ")?;
    tag.chars()
        .all(|c| c.is_ascii_lowercase() || c == ' ')
        .then_some((tag, rest))
}

/// The fixtures the readers are held to, and the ones the head's built-in `cards` test draws: every
/// shape a session row's notice takes, with the line it must come out as. Shared so a reader added
/// here without the layer's card is a failing test rather than a shape nobody noticed.
pub const FIXTURES: &[(&str, &str)] = &[
    (
        "message from your parent session `s-g`: Two new constraints from the operator, fold them \
         into the plan.\n\n1. REPLICATION",
        "parent s-g · Two new constraints from the operator, fold them into the plan · click for \
         details",
    ),
    (
        "[intent check] this turn declared 6 item(s); 0 tool call(s) succeeded and 0 ran without \
         producing a result.\n  - #1 \"x\" was declared\nboard: 0 of 6 complete — 0 in progress",
        "intent check · 6 declared, 0 ran · board 0/6 · click for details",
    ),
    (
        "[context check] you said \"my context is nearly out\". Your context is 54% full — 547,198 \
         of 999,999 tokens. That is not the wall, … Continue.",
        "context check · 54% full · not the wall · \"my context is nearly out\" · click for details",
    ),
    (
        "The harness is watching what your tool calls return, not how many you make, and the last \
         4 rounds ran 8 calls and 0 of 8 returned a result this turn had not already seen (8 ok).",
        "progress · last 4 rounds: 8 calls, 0 returned anything new · click for details",
    ),
    (
        "[the deliverable] Your worktree `/w` has uncommitted changes, so the branch `agent/x` does \
         not hold them: ?? a.jsonl. The branch is the deliverable.",
        "deliverable · agent/x · uncommitted: ?? a.jsonl · click for details",
    ),
    (
        "[monitor] 1 watch(es) fired:\n  - `ci` (command `gh run watch` exiting), declared by turn \
         s#t8#226: FIRED after 0.3s — the command exited 2\nOnly a FIRED watch is an answer about \
         the world.",
        "monitor · ci FIRED after 0.3s… · click for details",
    ),
    (
        "[answer] on \"GAP (latency)\": you asked \"Postpone?\" — the operator chose \"Postpone it\"",
        "answer · chose \"Postpone it\" · on \"GAP (latency)\" · click for details",
    ),
    (
        "[reseat] The person re-seated this conversation with `/reseat`: the conversation was \
         carried across as it was, nothing summarised. You can now call: web_fetch. The notice no \
         longer applies.",
        "reseated · verbatim · new: web_fetch · click for details",
    ),
    (
        "You keep overthinking. Cut it short. Your last turn spent its whole output on reasoning \
         (reasoning_only) and nothing was recorded.",
        "nudge · reasoning-only turn, told to answer first · click for details",
    ),
    (
        "[something new] the daemon learned to say this, and more. Second sentence.",
        "something new · the daemon learned to say this… · click for details",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn line(l: &Lines) -> String {
        l.parts.join(" · ")
    }

    /// The readers' own fixtures: every shape a notice takes, its one line. These are the strings
    /// the built-in layer is held to as well - a card drawn two ways is the defect this module
    /// exists to prevent.
    #[test]
    fn the_readers_say_the_facts_and_the_generic_says_the_first_clause() {
        for (text, want) in FIXTURES {
            let read = read(text).unwrap_or_else(|| generic(text));
            assert_eq!(&line(&read), want, "{text}");
        }
        assert_eq!(
            generic("[monitor] a shape no reader knows")
                .parts
                .join(" · "),
            "monitor · a shape no reader knows · click for details"
        );
    }

    /// A watch's news drawn from the bullets and from the one line per watch is one card: the
    /// daemon's fold and a stored transcript are two spellings of the same firing.
    #[test]
    fn a_monitor_line_per_watch_is_one_card() {
        let text =
            "[monitor] `ci` fired: the run exited 0.\n[monitor] `log` expired without firing.";
        let l = read(text).expect("the new shape is a card");
        assert_eq!(l.title, "monitor · ci");
        assert_eq!(
            line(&l),
            "monitor · ci the run exited 0 · 1 more · click for details"
        );
    }
}
