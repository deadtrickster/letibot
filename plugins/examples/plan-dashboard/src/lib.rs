//! **The first letibot pane plugin**: a project plan's tiers, stages, agents and hosts, drawn from
//! `<project>/.letibot/status.json` — the status file the agents keep as data rather than prose
//! (`docs/plugins-slice1.md` C5).
//!
//! Everything the plugin is lives in one pure function, [`view`]: an input in, a `View` out. It
//! has no memory but the `state` the host hands back, no clock but the one it is given (the status
//! file's `updated` is compared against it), and no capability but `read`. That is why the native
//! tests below can assert the whole dashboard without a wasm host — and why the `.wasm` is thin:
//! [`pane_dashboard`] parses the input, calls `view`, and serialises the answer.
//!
//! What is *not* here, deliberately: the layer stack's answer protocol (`{"pass": true}`,
//! `{"mask": true}`, `{"look": {"below": …}}`). A pane plugin in this slice is the bottom layer of
//! its own stack, and the protocol belongs to the host that resolves it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::status::{
    Agent, BLOCKED, DONE, Host, IN_PROGRESS, NOT_STARTED, RUNNING, Status, Tier, TierRow, WAITING,
    YOURS,
};
use crate::view::{
    Action, Badge, Bar, Block, Finding, Input, Line, Look, Panel, Role, Row, Severity, Unavailable,
    View,
};

mod status;
mod time;
mod view;

/// Where the dashboard reads its plan from, relative to the project root — which is what a
/// project-scope layer's look resolves against.
pub const STATUS_PATH: &str = ".letibot/status.json";

/// The status file is a snapshot the agents overwrite; a rewrite every half minute is soon enough
/// to notice one and slow enough to cost nothing. C3's floor for a repeating look is 5 seconds.
pub const EVERY_S: u32 = 30;

/// A status file this old is a claim about the past, and the dashboard says so rather than
/// drawing it as if it were now.
pub const STALE_AFTER_H: i64 = 24;

/// The one sentence every frame that cannot read the file ends with: it names the example a
/// project copies, because "write a status file" is not an instruction anyone can follow.
const FIX_STATUS: &str =
    "write .letibot/status.json (see plugins/examples/plan-dashboard/status.example.json)";

/// **The pane's whole logic.** An input in, a `View` out, with no I/O and no hidden state.
pub fn view(input: &Value) -> Value {
    view_before(input, now())
}

/// [`view`] with the clock given. The staleness finding is a comparison against now, so a test
/// that asserts on it must say what now is instead of hoping the box's clock agrees with a
/// fixture written months ago.
pub fn view_at(input: &Value, now: i64) -> Value {
    view_before(input, Some(now))
}

/// **The clock this pane has, or nothing at all.**
///
/// A wasm32 guest has no `SystemTime::now`: std's answer there is a PANIC, and a panic in this
/// module is a trap — which the host draws as one unavailable panel and no dashboard (measured:
/// the committed `.wasm` was never executed until the daemon loaded it, and every call trapped on
/// `time not implemented on this platform`). So the age of the status file is measured where a
/// clock exists and simply NOT measured where it does not — `None` is *could not ask*, never
/// *it is fresh*, and no finding claims a freshness nobody computed.
fn now() -> Option<i64> {
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
    }
}

/// [`view_at`]'s whole body, with the clock optional.
fn view_before(input: &Value, now: Option<i64>) -> Value {
    let Ok(input) = Input::deserialize(input) else {
        // Unreadable is not the same as refused: a host that hands this pane something which is
        // not a C3 input gets the first frame back — and the first frame asks for the file.
        return to_value(loading());
    };

    let mut state = input.state.and_then(|s| s.status);
    if let Some(look) = &input.look {
        if look.exit != 0 {
            return to_value(failed(look_error(look)));
        }
        match Status::parse(&look.stdout) {
            Ok(fresh) => state = Some(fresh),
            // The parser's message carries the line and column, which is the whole of what makes
            // this recoverable: a malformed file is a place to fix, not a mood.
            Err(why) => return to_value(failed(why)),
        }
    }

    match state {
        Some(status) => to_value(dashboard(&status, now)),
        None => to_value(loading()),
    }
}

/// The first frame: the read is asked for, and the panel says what it is doing instead of drawing
/// nothing.
fn loading() -> View {
    let panels = vec![Panel {
        id: "plan".into(),
        key: None,
        title: "plan".into(),
        denominator: None,
        summary: vec![Block::lines(vec![Line {
            label: "status".into(),
            value: format!("reading {STATUS_PATH}"),
            role: Role::Muted,
            ..Line::default()
        }])],
        view: Vec::new(),
        unavailable: Some(Unavailable {
            why: "not read yet".into(),
            fix: format!("wait: the read of {STATUS_PATH} follows this frame"),
        }),
    }];
    View {
        title: "plan".into(),
        badge: None,
        legend: legend(&panels),
        panels,
        findings: Vec::new(),
        look: Some(Look::read(STATUS_PATH)),
        every_s: 0,
        state: None,
    }
}

/// A frame that could not read the plan: one panel that says why and how to fix it, and a finding
/// that is `missing` rather than an empty dashboard that looks like a finished one.
fn failed(why: String) -> View {
    let panels = vec![Panel {
        id: "plan".into(),
        key: None,
        title: "plan".into(),
        denominator: None,
        // The one line an unavailable panel is allowed: what it could not get, in the colour that
        // says the failure is the pane's and not the plan's.
        summary: vec![Block::lines(vec![Line {
            label: "status".into(),
            value: "not read".into(),
            role: Role::Failure,
            ..Line::default()
        }])],
        view: Vec::new(),
        unavailable: Some(Unavailable {
            why: why.clone(),
            fix: FIX_STATUS.into(),
        }),
    }];
    let findings = vec![Finding {
        id: "status-file".into(),
        what: "no plan: the status file could not be read".into(),
        panel: "plan".into(),
        severity: Severity::Missing,
        detail: Some(why),
        evidence: Some(evidence(ParsedEvidence {
            path: STATUS_PATH.into(),
            ..ParsedEvidence::default()
        })),
        fix: Some(FIX_STATUS.into()),
        verify: Some(format!("jq .updated {STATUS_PATH}")),
        actions: vec![refresh()],
    }];
    View {
        title: "plan".into(),
        badge: None,
        legend: legend(&panels),
        panels,
        findings,
        // Still a look, once a minute: a file that appears is a dashboard that starts working
        // without anyone pressing a key, and a minute is slower than `r` is.
        look: Some(Look::read(STATUS_PATH)),
        every_s: 60,
        state: None,
    }
}

/// Why a look failed, in the words the panel shows: the process's own stderr when it said
/// anything, and the exit when it did not.
fn look_error(look: &crate::view::LookResult) -> String {
    let stderr = look.stderr.trim();
    if stderr.is_empty() {
        format!("reading {STATUS_PATH} exited {}", look.exit)
    } else {
        stderr.to_string()
    }
}

/// The dashboard proper: the overview of panels, the findings, the legend and the look that keeps
/// it current.
fn dashboard(status: &Status, now: Option<i64>) -> View {
    let mut panels = vec![
        tiers_panel(status),
        stages_panel(status),
        agents_panel(status),
    ];
    if !status.hosts.is_empty() {
        panels.push(hosts_panel(status));
    }
    panels.push(yours_panel(status));

    View {
        title: if status.release.name.is_empty() {
            "plan".into()
        } else {
            format!("plan · {}", status.release.name)
        },
        badge: badge(status),
        legend: legend(&panels),
        panels,
        findings: findings(status, now),
        look: Some(Look::read(STATUS_PATH)),
        every_s: EVERY_S,
        state: state(status),
    }
}

/// The status row's one word: what is yours outranks what is merely running, because only one of
/// the two needs the operator.
fn badge(status: &Status) -> Option<Badge> {
    if !status.yours.is_empty() {
        return Some(Badge {
            text: format!("{} yours", status.yours.len()),
            role: Role::Attention,
        });
    }
    let running = status.agents.iter().filter(|a| a.status == RUNNING).count();
    if running > 0 {
        return Some(Badge {
            text: format!("{running} running"),
            role: Role::Working,
        });
    }
    None
}

/// `tiers`: one line per tier on the overview — the shape of the plan read down the frame — and a
/// table per tier behind the key, because the summary is a share of a row count and the rows are
/// what a person acts on.
fn tiers_panel(status: &Status) -> Panel {
    let summary = vec![Block::lines(status.tiers.iter().map(tier_line).collect())];
    let mut view = Vec::new();
    // The release's goal is the one sentence the whole plan is measured against, and nothing else
    // in the frame has a place for it. A paragraph, not a heading: a heading would be a stop, and
    // a stop with nothing to do is a key press that goes nowhere.
    if !status.release.goal.is_empty() {
        view.push(Block::Markdown {
            markdown: format!("**goal**: {}", status.release.goal),
        });
    }
    for tier in &status.tiers {
        let (done, total) = counts(tier);
        // A table does not carry a title, so the tier's name is a kv row above it: it names the
        // table without adding a navigation stop that has nothing to do.
        view.push(Block::kv(vec![(
            tier.title.clone(),
            format!("{done}/{total} rows done"),
        )]));
        view.push(Block::table(
            &["area", "item", "status"],
            tier.rows.iter().map(tier_row).collect(),
        ));
    }
    Panel {
        id: "tiers".into(),
        key: Some("t".into()),
        title: "tiers".into(),
        denominator: Some("rows in the tier".into()),
        summary,
        view,
        unavailable: None,
    }
}

/// One tier, in the grid: `done/total`, its share of its own rows, and what is moving in it.
fn tier_line(tier: &Tier) -> Line {
    let (done, total) = counts(tier);
    Line {
        label: tier.title.clone(),
        value: format!("{done}/{total}"),
        bar: Some(Bar {
            frac: frac(done, total),
        }),
        tail: tier_tail(tier),
        role: tier_role(tier),
        id: Some(tier.id.clone()),
        actions: vec![refresh()],
        ..Line::default()
    }
}

/// One row of a tier: the three cells C5 asks for, the colours its status earns, and a detail a
/// person can read without opening the file.
fn tier_row(row: &TierRow) -> Row {
    let mut actions = vec![refresh()];
    if row.status == NOT_STARTED {
        actions.insert(
            0,
            Action::say(
                "s",
                "start",
                format!("start work on {} (status row {})", name(row), row.id),
            ),
        );
    }
    Row {
        id: row.id.clone(),
        cells: vec![row.area.clone(), row.item.clone(), row.status.clone()],
        role: row_role(&row.status),
        detail: row_detail(row),
        actions,
    }
}

/// The note, the branch and the agent, as markdown: what the file says about this row that does
/// not fit in three columns.
fn row_detail(row: &TierRow) -> Option<String> {
    let mut parts = Vec::new();
    if !row.note.is_empty() {
        parts.push(format!("**note**: {}", row.note));
    }
    if !row.branch.is_empty() {
        parts.push(format!("**branch**: `{}`", row.branch));
    }
    if !row.agent.is_empty() {
        parts.push(format!("**agent**: {}", row.agent));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// `stages`: the checkpoints the plan passes through, one line each, the status as the value.
fn stages_panel(status: &Status) -> Panel {
    let lines = status
        .stages
        .iter()
        .map(|stage| Line {
            label: stage.title.clone(),
            value: stage.status.clone(),
            tail: stage.note.clone(),
            role: row_role(&stage.status),
            id: Some(stage.id.clone()),
            actions: vec![refresh()],
            ..Line::default()
        })
        .collect();
    Panel {
        id: "stages".into(),
        key: Some("g".into()),
        title: "stages".into(),
        denominator: Some("stages in the plan".into()),
        summary: vec![Block::lines(lines)],
        view: Vec::new(),
        unavailable: None,
    }
}

/// `agents`: who is working, on what, on which branch.
fn agents_panel(status: &Status) -> Panel {
    let rows = status
        .agents
        .iter()
        .map(|agent: &Agent| Row {
            id: agent.id.clone(),
            cells: vec![
                agent.kind.clone(),
                agent.task.clone(),
                agent.branch.clone(),
                agent.status.clone(),
            ],
            role: agent_role(&agent.status),
            detail: None,
            actions: vec![refresh()],
        })
        .collect();
    Panel {
        id: "agents".into(),
        key: Some("a".into()),
        title: "agents".into(),
        denominator: Some("agents tracked".into()),
        summary: vec![Block::table(&["kind", "task", "branch", "status"], rows)],
        view: Vec::new(),
        unavailable: None,
    }
}

/// `hosts`: only when the file names any, because a `h` key that opens an empty panel is a key
/// that teaches people not to press it.
fn hosts_panel(status: &Status) -> Panel {
    let lines = status
        .hosts
        .iter()
        .map(|host: &Host| Line {
            label: host.id.clone(),
            value: host.state.clone(),
            tail: jobs(host.jobs),
            role: host_role(&host.state),
            id: Some(host.id.clone()),
            detail: if host.note.is_empty() {
                None
            } else {
                Some(host.note.clone())
            },
            actions: vec![refresh()],
            ..Line::default()
        })
        .collect();
    Panel {
        id: "hosts".into(),
        key: Some("h".into()),
        title: "hosts".into(),
        denominator: Some("hosts in the plan".into()),
        summary: vec![Block::lines(lines)],
        view: Vec::new(),
        unavailable: None,
    }
}

/// `yours`: the rows the operator themselves has to do, which is the reason the badge is red.
fn yours_panel(status: &Status) -> Panel {
    let lines = status
        .yours
        .iter()
        .map(|item| Line {
            label: item.what.clone(),
            value: item.id.clone(),
            role: Role::Attention,
            id: Some(item.id.clone()),
            actions: vec![refresh()],
            ..Line::default()
        })
        .collect();
    Panel {
        id: "yours".into(),
        key: Some("y".into()),
        title: "yours".into(),
        denominator: Some("items waiting on you".into()),
        summary: vec![Block::lines(lines)],
        view: Vec::new(),
        unavailable: None,
    }
}

/// **Everything that tripped, in one list.** A snapshot that is old is named with its age; a
/// blocked row is its own finding, because "one of twelve rows is stuck" is not something the
/// overview can say; and a file that parsed clean is a `passed` finding rather than silence, so
/// the list distinguishes "nothing is wrong" from "nothing was read".
fn findings(status: &Status, now: Option<i64>) -> Vec<Finding> {
    let mut out = Vec::new();

    // **An age is a measurement, and one needs a clock.** A guest has none (see [`now`]), so the
    // staleness finding exists exactly where it could have been computed.
    if let (Some(now), Some(at)) = (now, time::epoch(&status.updated)) {
        let age = now - at;
        if age > STALE_AFTER_H * 3_600 {
            let age_h = age / 3_600;
            out.push(Finding {
                id: "stale-status".into(),
                what: format!("status file is {age_h} hours old"),
                panel: "tiers".into(),
                severity: Severity::Tripped,
                detail: Some(format!(
                    "the plan below was written {age_h} hours ago; a row that moved since then \
                     still reads as it did then"
                )),
                evidence: Some(evidence(StaleEvidence {
                    updated: status.updated.clone(),
                    age_h,
                })),
                fix: Some("ask the agents to update .letibot/status.json".into()),
                verify: Some("jq .updated .letibot/status.json".into()),
                actions: vec![refresh()],
            });
        }
    }

    for tier in &status.tiers {
        for row in &tier.rows {
            if row.status == BLOCKED {
                out.push(blocked(tier, row));
            }
        }
    }

    if out.is_empty() {
        out.push(Finding {
            id: "status-parsed".into(),
            what: "status file parsed".into(),
            panel: "tiers".into(),
            severity: Severity::Passed,
            detail: Some(format!(
                "{} tiers, {} rows and {} stages read from {STATUS_PATH}",
                status.tiers.len(),
                status.rows(),
                status.stages.len()
            )),
            evidence: Some(evidence(ParsedEvidence {
                path: STATUS_PATH.into(),
                updated: status.updated.clone(),
                tiers: status.tiers.len(),
                rows: status.rows(),
                stages: status.stages.len(),
            })),
            fix: None,
            verify: Some(format!("jq .updated {STATUS_PATH}")),
            actions: vec![refresh()],
        });
    }
    out
}

/// A blocked row, as a finding of its own: the row's note is the detail when there is one, and
/// `verify` is the query that lists every blocked row rather than the one.
fn blocked(tier: &Tier, row: &TierRow) -> Finding {
    Finding {
        id: format!("blocked-{}", slug(&row.id)),
        what: format!("{} is blocked", name(row)),
        panel: "tiers".into(),
        severity: Severity::Tripped,
        detail: Some(if row.note.is_empty() {
            format!(
                "the row {} in {} is blocked and the file does not say why",
                row.id, tier.title
            )
        } else {
            row.note.clone()
        }),
        evidence: Some(evidence(BlockedEvidence {
            tier: tier.id.clone(),
            row: row.id.clone(),
            status: row.status.clone(),
        })),
        fix: Some("clear what blocks it, then set the row's status in .letibot/status.json".into()),
        verify: Some(format!(
            "jq '.tiers[].rows[] | select(.status==\"blocked\")' {STATUS_PATH}"
        )),
        actions: vec![refresh()],
    }
}

/// The legend documents every label the frame actually uses — the tier line's `done/total`, each
/// role's meaning, and every panel's denominator — so nothing in the frame is a word a reader has
/// to infer.
fn legend(panels: &[Panel]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if panels.iter().any(|p| p.id == "tiers") {
        out.insert(
            "done/total".into(),
            "rows of that tier that are done, over all of its rows".into(),
        );
    }
    for role in roles(panels) {
        out.insert(role.as_str().into(), role.meaning().into());
    }
    for panel in panels {
        if let Some(denominator) = &panel.denominator {
            out.insert(
                denominator.clone(),
                format!(
                    "the \"{}\" panel: a bar there is a share of {denominator}",
                    panel.title
                ),
            );
        }
    }
    out
}

/// The roles a frame actually paints with, over every block of every panel.
fn roles(panels: &[Panel]) -> BTreeSet<Role> {
    let mut out = BTreeSet::new();
    for panel in panels {
        for blocks in [&panel.summary, &panel.view] {
            for block in blocks {
                match block {
                    Block::Lines { lines } => out.extend(lines.iter().map(|l| l.role)),
                    Block::Table { table } => out.extend(table.rows.iter().map(|r| r.role)),
                    Block::Kv { .. } | Block::Markdown { .. } => {}
                }
            }
        }
    }
    out
}

/// Rows that are done, over rows there are.
fn counts(tier: &Tier) -> (usize, usize) {
    (
        tier.rows.iter().filter(|r| r.status == DONE).count(),
        tier.rows.len(),
    )
}

fn frac(done: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        done as f64 / total as f64
    }
}

/// A tier is green when it is finished, working when something in it is, and quiet otherwise —
/// including a tier whose rows are all blocked, because "nothing is running" is what that means.
fn tier_role(tier: &Tier) -> Role {
    let (done, total) = counts(tier);
    if total > 0 && done == total {
        Role::Success
    } else if tier.rows.iter().any(|r| r.status == IN_PROGRESS) {
        Role::Working
    } else {
        Role::Muted
    }
}

/// The tail of a tier's line: what is moving in it, in the file's own words, and a plain answer
/// when nothing is.
fn tier_tail(tier: &Tier) -> String {
    let mut parts = Vec::new();
    for (status, label) in [
        (IN_PROGRESS, "in progress"),
        (BLOCKED, "blocked"),
        (WAITING, "waiting"),
        (YOURS, "yours"),
    ] {
        let n = tier.rows.iter().filter(|r| r.status == status).count();
        if n > 0 {
            parts.push(format!("{n} {label}"));
        }
    }
    if !parts.is_empty() {
        return parts.join(", ");
    }
    let (done, total) = counts(tier);
    if total > 0 && done == total {
        "all done".into()
    } else {
        "not started".into()
    }
}

/// Row status to colour, which is C5's vocabulary and nothing else: an unfamiliar status is
/// `plain`, not a guess.
fn row_role(status: &str) -> Role {
    match status {
        DONE => Role::Success,
        IN_PROGRESS => Role::Working,
        WAITING | BLOCKED | YOURS => Role::Attention,
        NOT_STARTED => Role::Muted,
        _ => Role::Plain,
    }
}

/// An agent's colour: running is work, done is done, waiting or blocked is somebody else's.
fn agent_role(status: &str) -> Role {
    match status {
        RUNNING => Role::Working,
        DONE => Role::Success,
        WAITING | BLOCKED => Role::Attention,
        _ => Role::Muted,
    }
}

/// A host's colour: busy is working, a host that is down is the operator's problem, and everything
/// else is quiet.
fn host_role(state: &str) -> Role {
    match state {
        "busy" => Role::Working,
        "down" | "offline" => Role::Attention,
        _ => Role::Muted,
    }
}

/// The name a person reads for a row, falling back to its id when the file left the item out.
fn name(row: &TierRow) -> &str {
    if row.item.is_empty() {
        &row.id
    } else {
        &row.item
    }
}

fn jobs(n: u32) -> String {
    if n == 1 {
        "1 job".into()
    } else {
        format!("{n} jobs")
    }
}

/// A finding id is a handle, so a row id's `/` becomes `-`: an id with a slash in it is a path, and
/// a path in an id invites someone to split it.
fn slug(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The one action every stop has: look again now.
fn refresh() -> Action {
    Action::look("r", "refresh")
}

/// What the pane keeps between frames: the parsed status file, which is what makes a key press
/// (a frame with no look) draw the same plan instead of asking for the file again.
fn state(status: &Status) -> Option<Value> {
    serde_json::to_value(crate::view::State {
        status: Some(status.clone()),
    })
    .ok()
}

#[derive(Debug, Default, Serialize)]
struct StaleEvidence {
    updated: String,
    age_h: i64,
}

#[derive(Debug, Default, Serialize)]
struct BlockedEvidence {
    tier: String,
    row: String,
    status: String,
}

#[derive(Debug, Default, Serialize)]
struct ParsedEvidence {
    path: String,
    updated: String,
    tiers: usize,
    rows: usize,
    stages: usize,
}

fn evidence<T: Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// The `View` as the host reads it. A `View` is strings, numbers and vectors, so serialising one
/// cannot fail; if it somehow did, `null` is an answer the host records as a failed layer rather
/// than a trap it has to explain.
fn to_value(view: View) -> Value {
    serde_json::to_value(view).unwrap_or(Value::Null)
}

// `rano_abi` and `rano_alloc`: the two exports every rano plugin has.
rano_plugin::abi!();

/// **`pane_dashboard`, the hook the host calls**: a C3 input in, a C3 `View` out.
///
/// `None` — the ABI's "leave it alone" — is for text this plugin cannot read as JSON at all, which
/// is a host defect rather than a dashboard state; every state the plan can be in is answered with
/// a view, because a trap shows the operator nothing.
#[unsafe(no_mangle)]
pub extern "C" fn pane_dashboard(ptr: u32, len: u32) -> u32 {
    rano_plugin::hook(ptr, len, |text| {
        let input: Value = serde_json::from_str(text).ok()?;
        serde_json::to_string(&view(&input)).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The fixture a project copies, read here exactly as the plugin reads it over a look — an
    /// example that the crate beside it could not parse would be an example teaching the wrong
    /// shape.
    const EXAMPLE: &str = include_str!("../status.example.json");

    /// The fixture's own clock. Every test that asks about staleness pins `now` relative to the
    /// file, so a fixture that ages does not turn a test red.
    fn fixture_now() -> i64 {
        time::epoch("2026-10-25T22:00:00Z").expect("the fixture's clock")
    }

    fn look_input(stdout: &str) -> Value {
        json!({"v": 1, "state": null, "look": {"exit": 0, "stdout": stdout, "stderr": ""},
               "event": null, "size": {"w": 120, "h": 40}})
    }

    fn example_view() -> Value {
        view_at(&look_input(EXAMPLE), fixture_now())
    }

    fn panels_of(v: &Value) -> Vec<&Value> {
        v["panels"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    }

    fn panel<'a>(v: &'a Value, id: &str) -> &'a Value {
        panels_of(v)
            .into_iter()
            .find(|p| p["id"] == id)
            .unwrap_or_else(|| panic!("no panel {id}"))
    }

    fn blocks<'a>(panel: &'a Value, which: &str) -> Vec<&'a Value> {
        panel[which]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    }

    fn lines<'a>(panel: &'a Value, which: &str) -> Vec<&'a Value> {
        blocks(panel, which)
            .into_iter()
            .filter_map(|b| b.get("lines").and_then(|l| l.as_array()))
            .flatten()
            .collect()
    }

    fn tables<'a>(panel: &'a Value, which: &str) -> Vec<&'a Value> {
        blocks(panel, which)
            .into_iter()
            .filter_map(|b| b.get("table"))
            .collect()
    }

    fn rows(table: &Value) -> Vec<&Value> {
        table["rows"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    }

    /// Every stop in a view — lines and query-visible rows — because the contract's key rules are
    /// about stops, not about panels.
    fn stops(v: &Value) -> Vec<&Value> {
        let mut out = Vec::new();
        for panel in panels_of(v) {
            for which in ["summary", "view"] {
                out.extend(
                    lines(panel, which)
                        .into_iter()
                        .filter(|l| l.get("id").is_some()),
                );
                for table in tables(panel, which) {
                    out.extend(rows(table));
                }
            }
        }
        out.extend(v["findings"].as_array().into_iter().flatten());
        out
    }

    fn action_keys(stop: &Value) -> Vec<String> {
        stop["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a["key"].as_str().map(str::to_string))
            .collect()
    }

    #[test]
    fn the_first_frame_asks_for_the_read_and_says_so() {
        let v = view(&json!({"v": 1, "state": null, "look": null, "event": null,
                             "size": {"w": 120, "h": 40}}));
        assert_eq!(v["look"], json!({"read": ".letibot/status.json"}));
        assert_eq!(v["every_s"], 0);
        assert_eq!(v["state"], Value::Null);
        let panels = panels_of(&v);
        assert_eq!(panels.len(), 1);
        assert_eq!(panels[0]["unavailable"]["why"], "not read yet");
        // The frame says what it is doing rather than drawing an empty plan.
        assert_eq!(
            lines(panels[0], "summary")[0]["value"],
            "reading .letibot/status.json"
        );
    }

    #[test]
    fn an_input_that_is_not_a_c3_input_still_answers_a_first_frame() {
        // No panic, and the read is still asked for: unreadable is not refused.
        for odd in [
            json!({}),
            json!(null),
            json!([]),
            json!({"look": {"exit": "nine"}}),
        ] {
            let v = view_at(&odd, fixture_now());
            assert_eq!(v["look"], json!({"read": ".letibot/status.json"}), "{odd}");
            assert_eq!(panels_of(&v).len(), 1);
        }
    }

    #[test]
    fn the_example_gives_the_counts_fracs_roles_and_badge() {
        let v = example_view();
        assert_eq!(v["title"], "plan · v0.2.0");
        // One item of yours outranks two running agents, because only one of the two needs you.
        assert_eq!(v["badge"], json!({"text": "1 yours", "role": "attention"}));

        let tiers = panel(&v, "tiers");
        assert_eq!(tiers["key"], "t");
        assert_eq!(tiers["denominator"], "rows in the tier");
        let lines = lines(tiers, "summary");
        assert_eq!(lines.len(), 4);

        // A tier that is finished: green, a full bar, and no work named.
        assert_eq!(lines[0]["value"], "3/3");
        assert_eq!(lines[0]["bar"]["frac"], 1.0);
        assert_eq!(lines[0]["role"], "success");
        assert_eq!(lines[0]["tail"], "all done");

        // A mixed tier: its share of its own rows, and what is moving in it.
        assert_eq!(lines[1]["value"], "1/4");
        assert_eq!(lines[1]["bar"]["frac"], 0.25);
        assert_eq!(lines[1]["role"], "working");
        assert_eq!(lines[1]["tail"], "2 in progress");

        // The blocked tier: still working (something is running), with the block named in the tail.
        assert_eq!(lines[2]["value"], "1/3");
        assert_eq!(lines[2]["role"], "working");
        assert_eq!(lines[2]["tail"], "1 in progress, 1 blocked");

        // A tier nobody has touched is quiet rather than a zero-length bar with an opinion.
        assert_eq!(lines[3]["value"], "0/2");
        assert_eq!(lines[3]["bar"]["frac"], 0.0);
        assert_eq!(lines[3]["role"], "muted");
        assert_eq!(lines[3]["tail"], "not started");

        // The view behind the key has one table per tier, with the statuses' own colours.
        let tables = tables(tiers, "view");
        assert_eq!(tables.len(), 4);
        assert_eq!(tables[0]["columns"], json!(["area", "item", "status"]));
        let roles: Vec<&str> = rows(tables[2])
            .iter()
            .filter_map(|r| r["role"].as_str())
            .collect();
        assert_eq!(roles, vec!["working", "attention", "success"]);
        assert_eq!(rows(tables[0])[0]["cells"][2], "done");
    }

    #[test]
    fn the_panels_are_the_plans_own_shape() {
        let v = example_view();
        let ids: Vec<&str> = panels_of(&v)
            .iter()
            .filter_map(|p| p["id"].as_str())
            .collect();
        assert_eq!(ids, vec!["tiers", "stages", "agents", "hosts", "yours"]);
        let keys: Vec<&str> = panels_of(&v)
            .iter()
            .filter_map(|p| p["key"].as_str())
            .collect();
        assert_eq!(keys, vec!["t", "g", "a", "h", "y"]);

        assert_eq!(lines(panel(&v, "stages"), "summary").len(), 3);
        assert_eq!(lines(panel(&v, "stages"), "summary")[0]["value"], "done");
        assert_eq!(rows(tables(panel(&v, "agents"), "summary")[0]).len(), 2);
        assert_eq!(lines(panel(&v, "hosts"), "summary").len(), 3);
        assert_eq!(lines(panel(&v, "hosts"), "summary")[0]["value"], "busy");
        assert_eq!(lines(panel(&v, "hosts"), "summary")[0]["tail"], "3 jobs");
        assert_eq!(lines(panel(&v, "hosts"), "summary")[2]["role"], "attention");
        assert_eq!(lines(panel(&v, "yours"), "summary").len(), 1);
        assert_eq!(lines(panel(&v, "yours"), "summary")[0]["role"], "attention");

        // A file with no hosts has no `h`, because a key that opens nothing teaches people not to
        // press keys.
        let mut sparse: Value = serde_json::from_str(EXAMPLE).expect("json");
        sparse["hosts"] = json!([]);
        let v = view_at(&look_input(&sparse.to_string()), fixture_now());
        assert!(panels_of(&v).iter().all(|p| p["id"] != "hosts"));
    }

    #[test]
    fn s_is_only_on_rows_that_have_not_started() {
        let v = example_view();
        let tiers = panel(&v, "tiers");
        let mut not_started = 0;
        let mut started_says = Vec::new();
        for table in tables(tiers, "view") {
            for row in rows(table) {
                let keys = action_keys(row);
                if row["cells"][2] == "not started" {
                    not_started += 1;
                    assert!(keys.contains(&"s".to_string()), "no start on {}", row["id"]);
                    started_says.push(
                        row["actions"][0]["say"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    );
                } else {
                    assert!(
                        !keys.contains(&"s".to_string()),
                        "s on {}: {}",
                        row["id"],
                        row["cells"]
                    );
                }
                assert!(
                    keys.contains(&"r".to_string()),
                    "no refresh on {}",
                    row["id"]
                );
            }
        }
        assert_eq!(not_started, 3);
        // The line an operator's key press types for them, naming the row the file has to change.
        assert_eq!(
            started_says[0],
            "start work on fixture JSON for every block kind (status row tier1/sixty)"
        );
    }

    #[test]
    fn every_stop_has_the_refresh_and_it_asks_for_a_look() {
        let v = example_view();
        let stops = stops(&v);
        assert!(!stops.is_empty());
        for stop in &stops {
            let refresh = stop["actions"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|a| a["key"] == "r")
                .unwrap_or_else(|| panic!("no refresh on {}", stop["id"]));
            assert_eq!(refresh["look"], true);
            assert_eq!(refresh["label"], "refresh");
        }
        // And every stop has an id, since a stop is what the arrows land on.
        for stop in &stops {
            assert!(stop["id"].as_str().is_some_and(|id| !id.is_empty()));
        }
    }

    #[test]
    fn a_malformed_file_says_why_and_never_panics() {
        for bad in [
            "",
            "{\"tiers\": [}",
            "not json at all",
            "[1, 2, 3]",
            "{}",
            "null",
        ] {
            let v = view_at(&look_input(bad), fixture_now());
            let panels = panels_of(&v);
            assert_eq!(panels.len(), 1, "{bad}");
            assert_eq!(panels[0]["id"], "plan");
            let why = panels[0]["unavailable"]["why"].as_str().unwrap_or_default();
            assert!(!why.is_empty(), "{bad}");
            assert_eq!(panels[0]["unavailable"]["fix"], FIX_STATUS);
            let finding = &v["findings"][0];
            assert_eq!(finding["severity"], "missing");
            assert_eq!(finding["fix"], FIX_STATUS);
        }
        // serde_json names the place, and the panel shows its message verbatim.
        let v = view_at(&look_input("{\"tiers\": [}"), fixture_now());
        let why = v["panels"][0]["unavailable"]["why"].as_str().unwrap();
        assert!(why.contains("line 1 column"), "{why}");
    }

    #[test]
    fn a_failed_look_carries_its_stderr_and_an_empty_one_carries_the_exit() {
        let v = view_at(
            &json!({"v": 1, "look": {"exit": 2, "stdout": "",
                "stderr": "cat: .letibot/status.json: No such file or directory\n"}}),
            fixture_now(),
        );
        assert_eq!(
            v["panels"][0]["unavailable"]["why"],
            "cat: .letibot/status.json: No such file or directory"
        );
        assert_eq!(v["findings"][0]["severity"], "missing");

        let v = view_at(
            &json!({"v": 1, "look": {"exit": 2, "stdout": "", "stderr": ""}}),
            fixture_now(),
        );
        let why = v["panels"][0]["unavailable"]["why"].as_str().unwrap();
        assert!(why.contains('2'), "{why}");
        // It keeps looking, once a minute, so a file that appears fixes the pane by itself.
        assert_eq!(v["look"], json!({"read": ".letibot/status.json"}));
        assert_eq!(v["every_s"], 60);
    }

    #[test]
    fn an_old_updated_trips_the_stale_finding_and_a_clean_file_passes() {
        let old = EXAMPLE.replace("2026-10-25T21:55:00Z", "2026-10-22T21:55:00Z");
        let v = view_at(&look_input(&old), fixture_now());
        let stale = v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == "stale-status")
            .expect("the stale finding");
        assert_eq!(stale["severity"], "tripped");
        assert_eq!(stale["what"], "status file is 72 hours old");
        assert_eq!(
            stale["evidence"],
            json!({"updated": "2026-10-22T21:55:00Z", "age_h": 72})
        );
        assert_eq!(
            stale["fix"],
            "ask the agents to update .letibot/status.json"
        );
        assert_eq!(stale["verify"], "jq .updated .letibot/status.json");
        assert_eq!(stale["actions"][0]["look"], true);

        // A fresh file, and the blocked row cleared: nothing tripped, so the clean read is said
        // out loud instead of the list being empty.
        let mut cleared: Value = serde_json::from_str(EXAMPLE).expect("json");
        for tier in cleared["tiers"].as_array_mut().unwrap() {
            for row in tier["rows"].as_array_mut().unwrap() {
                if row["status"] == "blocked" {
                    row["status"] = json!("in progress");
                }
            }
        }
        let v = view_at(&look_input(&cleared.to_string()), fixture_now());
        let severities: Vec<&str> = v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|f| f["severity"].as_str())
            .collect();
        assert_eq!(severities, vec!["passed"]);
        assert_eq!(v["findings"][0]["what"], "status file parsed");

        // And the example itself has one blocked row, which is one finding of its own.
        let v = example_view();
        let blocked: Vec<&Value> = v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["severity"] == "tripped")
            .collect();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0]["severity"], "tripped");
        assert_eq!(blocked[0]["panel"], "tiers");
        assert!(blocked[0]["detail"].as_str().unwrap().contains("GiB"));
    }

    #[test]
    fn the_state_carries_the_plan_so_a_key_press_draws_it_again() {
        let first = example_view();
        assert_eq!(first["every_s"], 30);
        assert_eq!(first["look"], json!({"read": ".letibot/status.json"}));
        assert_eq!(
            first["state"]["status"]["tiers"].as_array().unwrap().len(),
            4
        );

        // A frame with the state and no look — what a key press is — is the same dashboard.
        let again = view_at(
            &json!({"v": 1, "state": first["state"], "look": null,
                    "event": {"item": "tier1/sixty", "action": "look"}}),
            fixture_now(),
        );
        assert_eq!(again["title"], first["title"]);
        assert_eq!(again["badge"], first["badge"]);
        assert_eq!(lines(panel(&again, "tiers"), "summary").len(), 4);
        assert_eq!(again["state"], first["state"]);
    }

    #[test]
    fn the_legend_documents_every_label_the_frame_uses() {
        let v = example_view();
        let legend = v["legend"].as_object().expect("a legend");
        assert!(legend.contains_key("done/total"));
        for role in ["success", "working", "attention", "muted"] {
            assert!(legend.contains_key(role), "no legend for {role}");
        }
        for panel in panels_of(&v) {
            let denominator = panel["denominator"].as_str().expect("a denominator");
            assert!(
                legend.contains_key(denominator),
                "no legend for {denominator}"
            );
        }
    }

    #[test]
    fn keys_never_take_the_hosts_own() {
        let v = example_view();
        let host = ['q', 'j', 'k', 'f', 'l'];
        let panel_keys: Vec<String> = panels_of(&v)
            .iter()
            .filter_map(|p| p["key"].as_str().map(str::to_string))
            .collect();
        for key in &panel_keys {
            let c = key.chars().next().expect("a panel key");
            assert!(!host.contains(&c), "panel key {key} is the host's");
        }
        let mut unique = panel_keys.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            panel_keys.len(),
            "two panels share a key: {panel_keys:?}"
        );

        for stop in stops(&v) {
            let keys = action_keys(stop);
            for key in &keys {
                assert_eq!(key.chars().count(), 1, "{key} is not a single letter");
                let c = key.chars().next().unwrap();
                assert!(!host.contains(&c), "action {key} takes a host key");
                assert!(
                    !panel_keys.contains(key),
                    "action {key} takes panel key {key}"
                );
            }
            let mut unique = keys.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(unique.len(), keys.len(), "one stop lists {keys:?}");
        }
    }

    #[test]
    fn the_fixture_is_the_file_c5_describes() {
        let status = Status::parse(EXAMPLE).expect("the fixture parses");
        assert_eq!(status.tiers.len(), 4);
        assert_eq!(status.rows(), 12);
        assert_eq!(status.stages.len(), 3);
        assert_eq!(status.agents.len(), 2);
        assert_eq!(status.hosts.len(), 3);
        assert_eq!(status.yours.len(), 1);
        // One tier of each shape the brief asks for: done, mixed, blocked, untouched.
        let shapes: Vec<(&str, usize)> = status
            .tiers
            .iter()
            .map(|t| {
                (
                    tier_role(t).as_str(),
                    t.rows.iter().filter(|r| r.status == BLOCKED).count(),
                )
            })
            .collect();
        assert_eq!(
            shapes,
            vec![("success", 0), ("working", 0), ("working", 1), ("muted", 0)]
        );
    }
}
