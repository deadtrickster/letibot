//! The GitHub reviewer seat, as a built-in plugin - one line per scope, one row per open pull
//! request under it.
//!
//! Nothing here asks GitHub anything: the daemon's snapshot of the seat arrives in the `github`
//! feed and this layer only decides how it reads. `posting: false` is drawn as words and not as a
//! colour, because the operator has to know that nothing is being sent.
//! One feed, `github`, and no clock: `SystemTime::now()` is a panic on `wasm32-unknown-unknown`.

use letibot_guest_abi as abi;
use letibot_plugin::pane::{Act, Cell, Input, Role};
use serde::Deserialize;
use serde_json::{Value, json};

/// The feed's name: what `plugin.toml` declares and what `Input::feeds` is keyed by.
const FEED: &str = "github";

/// The sentence a scope holding nothing draws under its line, so an empty scope is a fact rather
/// than a gap.
const NO_PULLS: &str = "no open pull requests in this scope";

/// What a host that handed over nothing draws. The feed is ABSENT rather than empty, so this is a
/// fact about the host and not about GitHub.
const NO_SCOPE: &str = "no scope is attached - nothing is being watched for review";

/// How much room a title is always given, so a long state word cannot cut it to one glyph.
const MIN_TITLE: usize = 8;

/// The width a row is drawn at when the head did not say - a first frame, or a fixture.
const DEFAULT_W: usize = 80;

/// **The feed, as this layer reads it.** Every field is defaulted: the snapshot belongs to another
/// crate, and a field it gains or drops must not make this pane draw nothing.
#[derive(Debug, Default, Deserialize)]
struct Feed {
    /// Whether a drafted review is posted to GitHub. `false` is everything kept as a draft.
    #[serde(default)]
    posting: bool,
    #[serde(default)]
    identity: String,
    #[serde(default)]
    scopes: Vec<Scope>,
}

/// One organisation or owner being watched.
#[derive(Debug, Default, Deserialize)]
struct Scope {
    #[serde(default)]
    scope: String,
    /// `polling`, `rate-limited`, `token missing`, `paused` or `failed`.
    #[serde(default)]
    state: String,
    #[serde(default)]
    state_words: String,
    /// The sentence behind `failed`, and empty otherwise.
    #[serde(default)]
    why: String,
    #[serde(default)]
    pulls: Vec<Pull>,
}

/// One open pull request in a scope.
#[derive(Debug, Default, Deserialize)]
struct Pull {
    #[serde(default)]
    repo: String,
    #[serde(default)]
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    head_sha: String,
    /// `queued`, `reviewing`, `drafted`, `posted`, `re-review pending`, `answering a mention`.
    #[serde(default)]
    review: String,
    /// **A fork's diff is reviewed and nothing of it runs**, which is why the row says both words.
    #[serde(default)]
    fork: bool,
    #[serde(default)]
    last_action: String,
    #[serde(default)]
    age_ms: u64,
    /// The session working this pull request; empty for one nobody has picked up.
    #[serde(default)]
    session: String,
    /// **What would be sent.** Drawn in full, because the operator's decision is about these words.
    #[serde(default, deserialize_with = "drafts_of")]
    drafts: Vec<Draft>,
}

/// One draft as the seat sends it: a review of a head, or an answer to a comment.
#[derive(Debug, Default, Deserialize)]
struct Draft {
    /// `review` or `reply`.
    #[serde(default)]
    kind: String,
    #[serde(default)]
    for_sha: String,
    #[serde(default)]
    comment: u64,
    #[serde(default)]
    body: String,
}

/// **A count from an older host is read as that many drafts with nothing in them.** The snapshot is
/// another crate's, and a field that changed shape must cost the words, not the whole pane.
fn drafts_of<'de, D>(d: D) -> Result<Vec<Draft>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let feed = Value::deserialize(d)?;
    match feed {
        Value::Array(items) => items
            .into_iter()
            .map(|item| serde_json::from_value(item).map_err(serde::de::Error::custom))
            .collect(),
        Value::Number(n) => Ok((0..n.as_u64().unwrap_or(0))
            .map(|_| Draft::default())
            .collect()),
        _ => Ok(Vec::new()),
    }
}

/// **One row this layer is about to draw.**
struct Out {
    id: String,
    cells: Vec<Cell>,
    facts: Vec<Cell>,
    under: Vec<String>,
    enter: Option<Act>,
    detail: String,
    children: Vec<Out>,
}

/// **A scope's line**: its name, the words its state is, and how many pull requests it holds.
fn scope_row(s: &Scope, feed: &Feed, width: usize) -> Out {
    let mut cells = vec![Cell::role(s.scope.clone(), Role::Strong)];
    let mut facts = vec![Cell::role(count_words(s.pulls.len()), Role::Faint)];
    // The state words take what the name and the count leave: they are why the scope is where it
    // is, and the count is the one number a person scans for.
    let rest = joined(&cells) + 1 + 1 + joined(&facts);
    let words = fit(
        &one_line(&s.state_words),
        width.saturating_sub(rest).max(MIN_TITLE),
    );
    if !words.is_empty() {
        cells.push(Cell::role(words, state_role(&s.state)));
    }
    clip(&mut cells, &mut facts, width);

    // A scope with nothing in it says so under its line: a tree that draws only children would
    // leave the scope looking like a host that lost its data.
    let mut under: Vec<String> = Vec::new();
    if s.pulls.is_empty() {
        under.push(fit(NO_PULLS, width));
    }

    let posting = if feed.posting {
        "on - a drafted review is posted to GitHub"
    } else {
        "off - every review is kept as a draft, nothing is sent to GitHub"
    };
    let mut detail: Vec<String> = vec![
        field("scope", &s.scope),
        field("state", &s.state_words),
        field("why", &s.why),
        field("posting", posting),
        field("pulls", &pull_count(s.pulls.len())),
    ];
    detail.retain(|l| !l.is_empty());

    Out {
        id: format!("scope:{}", s.scope),
        cells,
        facts,
        under,
        enter: None,
        detail: detail.join("\n"),
        children: s.pulls.iter().map(|p| pull_row(p, width)).collect(),
    }
}

/// **A pull request's row**: `#12` and the title on the left, the review state and the age on the
/// right, the last action faint under it, and Enter into the session working it.
fn pull_row(p: &Pull, width: usize) -> Out {
    let mut facts: Vec<Cell> = Vec::new();
    if !p.review.is_empty() {
        facts.push(Cell::role(p.review.clone(), review_role(&p.review)));
    }
    if p.fork {
        facts.push(Cell::role("fork", Role::Attention));
        facts.push(Cell::role("read-only", Role::Faint));
    }
    facts.push(Cell::role(age(p.age_ms), Role::Faint));

    let mut cells = vec![Cell::role(format!("#{}", p.number), Role::Strong)];
    // The title is the only unbounded text on the row, so it is the only thing that gives way: the
    // state and the age stay whole, because they are what the person acts on.
    let rest = joined(&cells) + 1 + 1 + joined(&facts);
    let title = fit(
        &one_line(&p.title),
        width.saturating_sub(rest).max(MIN_TITLE),
    );
    cells.push(Cell::new(title));
    clip(&mut cells, &mut facts, width);

    let mut under: Vec<String> = Vec::new();
    if !p.last_action.is_empty() {
        under.push(fit(&one_line(&p.last_action), width));
    }
    // **Each draft on its own line**, because a count is not something a person can decide about.
    for draft in &p.drafts {
        under.push(draft_line(draft, width));
    }
    let mut detail: Vec<String> = vec![
        field("repo", &p.repo),
        field("head", &p.head_sha),
        field("review", &p.review),
        field("drafts", &drafts_words(p.drafts.len())),
        field("session", &p.session),
        field("fork", if p.fork { FORK_WHY } else { "" }),
    ];
    // The whole draft, never a cut one: this is what would be sent.
    for draft in &p.drafts {
        detail.push(format!("{:<10} {}", draft_what(draft), draft.body.trim()));
    }
    detail.retain(|l| !l.is_empty());

    Out {
        id: format!("pr:{}#{}", p.repo, p.number),
        cells,
        facts,
        under,
        // A row with no session has nothing to switch to, and an act the host cannot perform is a
        // key that lies.
        enter: (!p.session.is_empty()).then(|| Act::switch(p.session.clone())),
        detail: detail.join("\n"),
        children: Vec::new(),
    }
}

/// Why a fork's row says `read-only`.
const FORK_WHY: &str = "a fork's diff is reviewed and nothing of it runs";

/// **The seat itself**: whether posting is on, and whose token would send it.
fn chrome(feed: &Feed, width: usize) -> Vec<Value> {
    let room = width.saturating_sub(10);
    let (posting, role) = if feed.posting {
        (
            "on - a review this seat drafts is posted to GitHub",
            "success",
        )
    } else {
        (
            "off - every review is kept as a draft and nothing is sent to GitHub",
            "attention",
        )
    };
    let who = if feed.identity.is_empty() {
        "not set - the daemon has no identity for this seat".to_string()
    } else {
        format!("@{}", feed.identity)
    };
    vec![
        json!({"label": "posting", "value": fit(posting, room), "role": role}),
        json!({"label": "identity", "value": fit(&who, room), "role": "plain"}),
    ]
}

/// The seat panel's title.
fn seat_title(feed: &Feed, width: usize) -> String {
    let title = if feed.identity.is_empty() {
        "the reviewer seat".to_string()
    } else {
        format!("the reviewer seat · @{}", feed.identity)
    };
    fit(&title, width)
}

/// The scopes panel's title - how many scopes, and how many pull requests among them.
fn scopes_title(feed: &Option<Feed>, width: usize) -> String {
    let Some(feed) = feed else {
        return "scopes".to_string();
    };
    let open: usize = feed.scopes.iter().map(|s| s.pulls.len()).sum();
    fit(
        &format!("scopes ({}) · {}", feed.scopes.len(), pull_count(open)),
        width,
    )
}

/// The sentence every state word and mark this pane draws is explained by.
fn legend() -> Value {
    json!({
        "polling": "asking GitHub for this scope's open pull requests, on the daemon's own clock",
        "rate-limited": "the token has spent its quota; the wait is GitHub's, not the scope's",
        "token missing": "posting is on and there is no token file, so nothing can be sent",
        "paused": "the operator paused this scope; it is polled again when they resume it",
        "failed": "the last poll failed, and `why` on the scope carries GitHub's own sentence",
        "queued": "waiting for a reviewer to start on it",
        "reviewing": "a session is reading the diff now",
        "drafted": "a review is written and not posted - it waits for the operator",
        "posted": "the review is on the pull request, where its author can read it",
        "re-review pending": "the head moved since the review; the new commits are unread",
        "answering a mention": "somebody wrote on the pull request and a session is answering",
        "fork": "the branch is somebody else's, so its diff is reviewed and nothing of it runs",
        "read-only": "no code from this pull request is executed anywhere on this box",
        "draft": "a review or an answer that is written and not sent; `[github] post` sends it",
    })
}

/// The palette's role for a scope's state.
fn state_role(state: &str) -> Role {
    match state {
        "polling" => Role::Working,
        "rate-limited" => Role::Attention,
        "token missing" | "failed" => Role::Failure,
        "paused" => Role::Muted,
        _ => Role::Plain,
    }
}

/// The palette's role for a pull request's review state.
fn review_role(review: &str) -> Role {
    match review {
        "queued" => Role::Pending,
        "reviewing" | "answering a mention" => Role::Working,
        "drafted" | "re-review pending" => Role::Attention,
        "posted" => Role::Success,
        _ => Role::Plain,
    }
}

/// **`4m`** - an age, from the milliseconds the feed carries. The guest has no clock, so this is
/// arithmetic and nothing else.
fn age(ms: u64) -> String {
    if ms < 60_000 {
        format!("{}s", ms / 1000)
    } else if ms < 3_600_000 {
        format!("{}m", ms / 60_000)
    } else if ms < 86_400_000 {
        format!("{}h", ms / 3_600_000)
    } else {
        format!("{}d", ms / 86_400_000)
    }
}

/// How many open pull requests a scope holds, as the fact on its line. `0 open` is a fact and not a
/// gap - the sentence under the line is what says what a zero means.
fn count_words(n: usize) -> String {
    format!("{n} open")
}

/// The same count as prose, for a detail.
fn pull_count(n: usize) -> String {
    match n {
        0 => "no open pull requests".to_string(),
        1 => "1 open pull request".to_string(),
        n => format!("{n} open pull requests"),
    }
}

/// The drafts the seat holds for one pull request, in words - nothing when there are none.
fn drafts_words(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => "1 kept as a draft".to_string(),
        n => format!("{n} kept as drafts"),
    }
}

/// What one draft is, in the words its line opens with: a review of a head, or an answer.
fn draft_what(draft: &Draft) -> String {
    match draft.kind.as_str() {
        "reply" if draft.comment != 0 => format!("draft reply to comment {}", draft.comment),
        "reply" => "draft reply".to_string(),
        _ if draft.for_sha.is_empty() => "draft review".to_string(),
        _ => format!(
            "draft review of {}",
            &draft.for_sha[..draft.for_sha.len().min(10)]
        ),
    }
}

/// One draft's faint line under its row: what it is and the first line of what it says.
fn draft_line(draft: &Draft, width: usize) -> String {
    let what = draft_what(draft);
    let said = one_line(&draft.body);
    let line = if said.is_empty() {
        what
    } else {
        format!("{what}: {said}")
    };
    fit(&line, width)
}

/// The first line of a text, with control characters flattened - a title is somebody's prose and an
/// escape in it would be measured one way and drawn another.
fn one_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// One `label  value` line of a detail, the label padded so the prose reads as fields.
fn field(k: &str, v: &str) -> String {
    if v.is_empty() {
        String::new()
    } else {
        format!("{k:<10} {v}")
    }
}

/// One column of a row as the grid joins it: a space between, and nothing else.
fn joined(cells: &[Cell]) -> usize {
    cells.iter().map(|c| c.text.chars().count()).sum::<usize>() + cells.len().saturating_sub(1)
}

/// What one row's own line costs - both columns and the space between them.
fn line_width(cells: &[Cell], facts: &[Cell]) -> usize {
    if facts.is_empty() {
        joined(cells)
    } else {
        joined(cells) + 1 + joined(facts)
    }
}

/// **Cut to fit, with an ellipsis.** A guest has no width table, so a character is the unit it can
/// honestly count.
fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(width - 1).collect();
    out.push('…');
    out
}

/// **Nothing wider than the frame leaves this layer.** The title is fitted before this runs, so
/// what is left is a state word or a scope name on a frame too narrow for it: the longest text
/// gives up the difference, because a row drawn over the edge is a row the person cannot read.
fn clip(cells: &mut [Cell], facts: &mut [Cell], width: usize) {
    loop {
        let over = line_width(cells, facts).saturating_sub(width);
        if over == 0 {
            return;
        }
        let mut at = 0;
        let mut best = 0;
        for (i, c) in facts.iter().chain(cells.iter()).enumerate() {
            let n = c.text.chars().count();
            if n > best {
                best = n;
                at = i;
            }
        }
        // One glyph is the floor: a row of ellipses says less than a row that overflows.
        if best <= 1 {
            return;
        }
        let keep = best.saturating_sub(over).max(1);
        let facts_n = facts.len();
        if at < facts_n {
            facts[at].text = fit(&facts[at].text, keep);
        } else {
            let i = at - facts_n;
            cells[i].text = fit(&cells[i].text, keep);
        }
    }
}

/// One row as C3 spells it.
fn row_value(row: &Out) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("id".into(), json!(row.id));
    out.insert("cells".into(), json!(row.cells));
    if !row.facts.is_empty() {
        out.insert("facts".into(), json!(row.facts));
    }
    if !row.under.is_empty() {
        out.insert("under".into(), json!(row.under));
    }
    if let Some(act) = &row.enter {
        out.insert("enter".into(), json!(act));
    }
    if !row.detail.is_empty() {
        out.insert("detail".into(), json!(row.detail));
    }
    if !row.children.is_empty() {
        let kids: Vec<Value> = row.children.iter().map(row_value).collect();
        out.insert("children".into(), json!(kids));
    }
    Value::Object(out)
}

/// The pane's answer, or `None` for *unchanged*.
fn answer(bytes: &[u8]) -> Option<String> {
    let input: Input = serde_json::from_slice(bytes).ok()?;
    let width = match input.size.w {
        0 => DEFAULT_W,
        w => w as usize,
    };
    // **An absent feed is not an empty one.** The host hands over what the layer declared, so a
    // missing `github` means nothing is attached, and that is a sentence rather than a blank panel.
    let feed: Option<Feed> = input
        .feeds
        .get(FEED)
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let mut panels: Vec<Value> = Vec::new();
    let mut badge: Option<Value> = None;
    let mut rows: Vec<Value> = Vec::new();
    if let Some(feed) = &feed {
        badge = Some(if feed.posting {
            json!({"text": "posting on", "role": "success"})
        } else {
            json!({"text": "drafts only", "role": "attention"})
        });
        panels.push(json!({
            "id": "seat",
            "title": seat_title(feed, width),
            "summary": [{"lines": chrome(feed, width)}],
        }));
        rows = feed
            .scopes
            .iter()
            .map(|s| row_value(&scope_row(s, feed, width)))
            .collect();
    }
    // A tree this head draws only when it has rows, so the empty case is one line and not a frame
    // with nothing in it.
    let summary = if rows.is_empty() {
        json!([{"lines": [{"label": "scopes", "value": NO_SCOPE}]}])
    } else {
        json!([{"tree": {"rows": rows, "empty": NO_SCOPE}}])
    };
    panels.push(json!({
        "id": "scopes",
        "title": scopes_title(&feed, width),
        "summary": summary,
    }));

    let mut view = serde_json::Map::new();
    view.insert("title".into(), json!("the github reviewer"));
    if let Some(badge) = badge {
        view.insert("badge".into(), badge);
    }
    view.insert("panels".into(), json!(panels));
    view.insert("legend".into(), legend());
    serde_json::to_string(&Value::Object(view)).ok()
}

/// The layer's policy, tested natively - see `tests.rs`.
#[cfg(test)]
mod tests;

/// The pane hook - `pane_<name>`, the name a head opens by.
#[unsafe(no_mangle)]
pub extern "C" fn pane_github(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}
