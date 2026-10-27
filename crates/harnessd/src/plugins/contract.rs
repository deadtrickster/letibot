//! **`plugin_hooks`: the contracts, written by the host that enforces them.**
//!
//! The operator: *"`plugin_hooks` — the contracts of §4 with a worked example each, read from the
//! host itself so it cannot drift from what the host accepts"* (`docs/plugins-plan.md` §5).
//!
//! # How "cannot drift" is kept, rather than promised
//!
//! The worked example below is **built out of the host's own types** ([`View`], [`Panel`],
//! [`Block`] …), not typed as a string: a field that is renamed, a `serde(rename)` that moves, a
//! block kind that gains a spelling — each of those breaks the *compilation* of this file rather
//! than leaving a document that says the old thing. The names a model reads are then read back
//! out of that example's JSON, and the constants it is told about (`HOST_KEYS`, `MIN_EVERY_S`) are
//! the host's own constants.
//!
//! The example is also **round-tripped and validated** by the test at the bottom: it parses into a
//! [`View`] and passes [`View::validate`], which is the same check the daemon runs over a plugin's
//! answer before publishing it. A worked example the host would refuse is worse than none, because
//! a model copies it.
//!
//! # One kind today
//!
//! `pane` is the only hook kind this host runs (`docs/plugins-plan.md` §7 step 2 is the next
//! package: `on_tool_call`, `on_tool_result`, `notice`, `condition`). A name this host does not
//! run is refused with the one it does, never answered with silence.

use std::collections::BTreeMap;

use letibot_sessionlog::pane::{
    Action, Badge, Bar, Block, Finding, HOST_KEYS, Line, LookSpec, MIN_EVERY_S, Panel, Role, Row,
    Severity, Table, View,
};
use serde_json::{Value, json};

/// The hook kinds this host runs, as `scaffold`'s `hook` names them.
pub const KINDS: &[&str] = &[
    "pane",
    "notice",
    "on_tool_call",
    "on_tool_result",
    "condition",
];

/// **The documentation for one hook kind** — or for every kind, each on its own page.
pub fn hooks_doc(kind: Option<&str>) -> Result<String, String> {
    match kind.map(str::trim).filter(|k| !k.is_empty()) {
        None => Ok(KINDS
            .iter()
            .map(|k| patch_doc(k).unwrap_or_else(pane_doc))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")),
        Some("pane") => Ok(pane_doc()),
        Some(other) => patch_doc(other).ok_or_else(|| {
            format!(
                "`{other}` is not a hook kind this host runs ({}). Each of them has a page here: \
                 `plugin_hooks` with no `hook` prints all of them.",
                KINDS.join(", ")
            )
        }),
    }
}

/// **One patch point's page**, or `None` for a kind that is not one of the four.
///
/// Built the same way the pane's is: the input is the SDK's own synthetic example SERIALISED, the
/// answer is an SDK VALUE serialised, and the field lists are read back out of those two documents.
/// A renamed field moves the page; a field the page claims and the type does not have cannot exist.
fn patch_doc(kind: &str) -> Option<String> {
    let (in_value, out_value, contract, rules): (Value, Value, &str, String) = match kind {
        "notice" => {
            let model = letibot_plugin::notice::Notice::synthetic();
            let out = letibot_plugin::notice::NoticeOut::reworded(format!(
                "[monitor] {} — read it",
                model.text
            ));
            let rules = format!(
                "* **Only a firing and a nudge may be dropped.** `drop` is honoured for `monitor` \
                 and `nudge`, and refused (with the reason said to the model) for `job`, `todo`, \
                 `reseat`, `merge_queue`, `workspace`, `salvage` and `plugin`: those are facts the \
                 model is owed, and a layer may reword them.\n\
                 * **The export is named after the notice's KIND** — `notice_monitor`, \
                 `notice_job`, … — so a fix to one firing's wording is not the answerer for the \
                 queue's verdicts. The kinds are: {}.",
                letibot_plugin::notice::KINDS.join(", ")
            );
            (
                json!(model),
                json!(out),
                "`docs/plugins-plan.md` §4.3 — what the harness says to the model",
                rules,
            )
        }
        "on_tool_call" => {
            let model = letibot_plugin::tool_call::ToolCallIn::synthetic();
            let out = letibot_plugin::tool_call::ToolCallOut::refuse(
                "already watched as pr-24; use that",
            );
            (
                json!(model),
                json!(out),
                "`docs/plugins-plan.md` §4.1 — the call, before the gate",
                "* **It runs before the gate and can never admit anything.** The rewritten call is \
                 what the gate then adjudicates, the tool's name is not a field of the answer, and \
                 there is no marker by which a layer could say *allow*.\n\
                 * **Absent fields are absent.** `{}` or `{\"pass\": true}` lets the call run as \
                 the model proposed it; an answer naming two of the four is read in the order \
                 refuse, answer, arguments, pass.\n\
                 * **`arguments` replaces the call's arguments entirely** — it is not a merge. A \
                 rewritten call goes through the same argument repair the model's own did."
                    .to_string(),
            )
        }
        "on_tool_result" => {
            let model = letibot_plugin::tool_result::ToolResultIn::synthetic();
            let out = letibot_plugin::tool_result::ToolResultOut {
                append: Some("\n[harness] watching it for its checks".into()),
                arm: vec![letibot_plugin::tool_result::Arm::new(
                    "github-pr",
                    json!({"pr": "o/r#24"}),
                )],
                ..Default::default()
            };
            (
                json!(model),
                json!(out),
                "`docs/plugins-plan.md` §4.2 — what happens after a call",
                "* **Successful results only** (`ok`, `backgrounded`). A call that failed is the \
                 failure's own words, and the model reads them exactly.\n\
                 * **`payload` replaces, `append` adds.** Both may appear: what it was, and what \
                 the layer noticed about it.\n\
                 * **`arm` goes through the operator's door** — the same `monitor` call the model \
                 would type — so the coalescing, the already-watched refusal and the TTL caps are \
                 the tool's rules, not a second set here."
                    .to_string(),
            )
        }
        "condition" => {
            let model = letibot_plugin::condition::ConditionIn::synthetic();
            let out = letibot_plugin::condition::ConditionOut {
                look: Some(letibot_plugin::condition::LookAsk::Exec(vec![
                    "gh".into(),
                    "pr".into(),
                    "view".into(),
                    "24".into(),
                    "--json".into(),
                    "state,reviews".into(),
                ])),
                every_s: Some(90),
                max_ttl_s: Some(604_800),
                fired: Some("CodeRabbit reviewed: 3 comments".into()),
                state: Some(json!({"seen": 3})),
                github: Some(letibot_plugin::condition::Github {
                    pr: "o/r#24".into(),
                    watches: vec!["coderabbit".into()],
                }),
            };
            let rules = format!(
                "* **The plugin never runs anything.** `look` in an answer is a DECLARATION: the \
                 host runs it (a `read` under the layer's `read`, an `exec` argv under \
                 `exec:<program>`, never a shell) and calls the hook again with the result.\n\
                 * **`look` means two things by direction**: what came back on the way in \
                 (`{{\"exit\", \"stdout\", \"stderr\"}}`), what to run next on the way out \
                 (`{{\"exec\": […]}}` or `{{\"read\": \"path\"}}`).\n\
                 * **`fired` is the news**, and `state` is handed back next time — a plugin is \
                 stateless between calls and the host keeps what it returns. `every_s` is at least \
                 {}; `max_ttl_s` caps the watch, inside the tree's week.",
                letibot_plugin::condition::EVERY_MIN_S
            );
            (
                json!(model),
                json!(out),
                "`docs/plugins-plan.md` §4.4 — what a monitor watches",
                rules,
            )
        }
        _ => return None,
    };
    let input = serde_json::to_string_pretty(&in_value).unwrap_or_default();
    let example = serde_json::to_string_pretty(&out_value).unwrap_or_default();
    Some(format!(
        r#"# The `{kind}` hook — {contract}

One pure function, JSON in and JSON out, resolved through the layer stack: the topmost layer that
answers wins, a layer below is asked only when the one above says `{{"below": …}}`, any layer may
say `{{"mask": true}}` to switch the hook off from there down, and a layer that traps falls to the
one below and is reported. A layer is a wasm export whose NAME is the hook, and `plugin.toml`'s
`hooks` must be exactly what the module exports.

## The input

```json
{input}
```

## The answer — a worked example

```json
{example}
```

{rules}

## What to do when it goes wrong

`plugin test <name>` calls the hook with a synthetic input of this kind (or one you pass) and shows
the answer, the layer that answered, the fuel it burned and any trap. `plugin load <name>` runs it
through the shadow gate — replaying the inputs this session has already asked — so a layer that
traps, or answers something the host refuses, never reaches a running session.
"#
    ))
}

/// **The worked example**, built from the host's types. Every field a model is told about appears
/// in it, so the document and the contract are the same object.
fn example() -> View {
    View {
        title: "deploy · v0.2.0".into(),
        badge: Some(Badge {
            text: "2 waiting".into(),
            role: Some(Role::Attention),
        }),
        panels: vec![
            Panel {
                id: "envs".into(),
                key: "t".into(),
                title: "environments".into(),
                denominator: Some("environments this release ships to".into()),
                summary: vec![Block::Lines(vec![
                    Line {
                        label: "staging".into(),
                        value: Some("2/2".into()),
                        bar: Some(Bar { frac: 1.0 }),
                        tail: Some("shipped".into()),
                        role: Some(Role::Success),
                        id: Some("staging".into()),
                        detail: Some("deployed at the tag, twice".into()),
                        ..Line::default()
                    },
                    Line {
                        label: "prod".into(),
                        value: Some("0/2".into()),
                        bar: Some(Bar { frac: 0.0 }),
                        tail: Some("waiting on you".into()),
                        role: Some(Role::Attention),
                        id: Some("prod".into()),
                        actions: vec![Action {
                            key: "s".into(),
                            label: "start".into(),
                            say: Some("start the prod deploy".into()),
                            ..Action::default()
                        }],
                        ..Line::default()
                    },
                ])],
                view: vec![Block::Table(Table {
                    columns: vec!["step".into(), "state".into()],
                    rows: vec![Row {
                        id: Some("prod/migrate".into()),
                        cells: vec!["migrate".into(), "not started".into()],
                        role: Some(Role::Muted),
                        detail: Some("`bin/deploy --env prod --step migrate`".into()),
                        ..Row::default()
                    }],
                })],
                unavailable: None,
            },
            Panel {
                id: "queue".into(),
                key: "e".into(),
                title: "queue".into(),
                summary: vec![Block::Kv(vec![
                    ("entries".into(), "3".into()),
                    ("oldest".into(), "6 min".into()),
                ])],
                ..Panel::default()
            },
        ],
        findings: vec![Finding {
            id: "stale".into(),
            what: "the status file is 3 days old".into(),
            panel: "envs".into(),
            severity: Severity::Tripped,
            detail: "the numbers above describe three days ago".into(),
            evidence: BTreeMap::from([("age_h".to_string(), json!(74))]),
            fix: "write .letibot/deploy.json from the deploy job".into(),
            verify: "jq -r .updated .letibot/deploy.json".into(),
            actions: vec![Action {
                key: "r".into(),
                label: "refresh".into(),
                look: true,
                ..Action::default()
            }],
        }],
        legend: BTreeMap::from([
            (
                "done/total".to_string(),
                "steps finished of steps in the environment".to_string(),
            ),
            (
                "waiting on you".to_string(),
                "a step the pipeline will not take by itself".to_string(),
            ),
        ]),
        look: Some(LookSpec::Read {
            read: ".letibot/deploy.json".into(),
        }),
        every_s: 30,
        state: Some(json!({"seen": 1})),
    }
}

/// The example as the JSON a plugin answers — the document, not a transcription of it.
fn example_json() -> Value {
    let view = example();
    // Through `serde_json` exactly as the host parses a plugin's answer: an example that did not
    // survive this round trip is an example the host would refuse.
    let text = serde_json::to_string(&view).expect("a view serialises");
    serde_json::from_str(&text).expect("and parses back")
}

/// The keys of a JSON object, in the order the host writes them, for the field list.
fn keys(v: &Value) -> String {
    match v {
        Value::Object(map) => map.keys().cloned().collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

fn pane_doc() -> String {
    let input = letibot_tools::builtins::plugin::synthetic_input();
    let input = serde_json::to_string_pretty(&input).unwrap_or_default();
    let example = example_json();
    let pretty = serde_json::to_string_pretty(&example).unwrap_or_default();
    // The field lists are read back out of the example, so a renamed field is a document that
    // changes with the type.
    let view_keys = keys(&example);
    let panel_keys = keys(&example["panels"][0]);
    let line_keys = keys(&example["panels"][0]["summary"][0]["lines"][0]);
    let row_keys = keys(&example["panels"][0]["view"][0]["table"]["rows"][0]);
    let finding_keys = keys(&example["findings"][0]);
    // And the block kinds, from the host's own serializer — a sixth kind added to `Block` would
    // not appear here, so this list is checked against the variants by the test at the bottom.
    let block_kinds = ["lines", "table", "tree", "markdown", "kv"]
        .iter()
        .map(|k| format!("`{k}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let host_keys = HOST_KEYS
        .iter()
        .map(|k| format!("`{k}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let roles = [
        Role::Muted,
        Role::Working,
        Role::Attention,
        Role::Success,
        Role::Failure,
        Role::Plain,
    ]
    .iter()
    .map(|r| {
        format!(
            "`{}`",
            serde_json::to_value(r)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        )
    })
    .collect::<Vec<_>>()
    .join(", ");

    format!(
        r#"# The `pane` hook — `pane_<name>`, contract 1

A pane is a wasm export named `pane_<name>` in a plugin whose `plugin.toml` declares
`hooks = ["pane_<name>"]`, `contract = 1`, and whatever capabilities it asks for. The host calls it
with one JSON object and reads one back, and **that is the whole conversation**: a pane never opens
a file and never runs a program. When it needs the world it asks for a *look* in its answer and the
host runs it — `{{"read": "path"}}` under the `read` capability; an `exec` look is refused in this
slice (`exit` 126, with the sentence in `stderr`). The pane id it is drawn under is
`<plugin.toml's name>/<hook name without `pane_`>`.

Why the shape is this shape: `docs/plugins-slice1.md` C3, the dashboard language the operator
pointed at. An overview of panels, every panel a summary with a full view behind its own key;
every number through one grid so it lines up down the frame; every bar says its denominator;
findings collect everything that tripped in one list, and Enter reads one whole — what was
measured, the numbers behind it, the fix and how to check it; a legend documents every label; and
a panel that cannot get its data says *why and how to fix it* rather than drawing off an empty
result.

## The input

```json
{input}
```

* `v` — the contract version, always `1`.
* `state` — what this pane returned in its last answer's `state`, or `null` on the first call. A
  plugin is stateless between calls: anything it must remember it returns, and the host keeps it
  (§1 principle 5).
* `look` — `null`, or the result of the look it asked for: `{{"exit": 0|1, "stdout": "…",
  "stderr": "…"}}`. A refused look is a RESULT, not an error: draw why and how to fix it.
* `event` — `null`, or `{{"item": "<the stop's id>", "action": "<the action's event word>"}}` when
  a person pressed one of the pane's own actions.
* `size` — `{{"w": <columns>, "h": <rows>}}`, the rectangle the head is drawing in. Lay out for
  it: a dashboard wrapped for a screen nobody has is a dashboard nobody reads.

## The output — a worked example

```json
{pretty}
```

Top level: {view_keys}.

* `title` — drawn first. The pane's name is the wire's, so do not repeat it here.
* `badge` — `{{"text": "3 running", "role": "attention"}}` or absent, in the status row.
* `panels` — {panel_keys}.
  * `key` — one lowercase letter that opens the panel's `view`; needed only by a panel that HAS a
    view (a panel with no view draws its summary and takes no key).
  * `denominator` — what every `bar` and `spark` in this panel is a share of, in words.
  * `summary` and `view` — lists of blocks; `view` absent means the summary IS the view.
  * `unavailable` — `{{"why": "…", "fix": "…"}}`: the panel could not get its data.
* `findings` — {finding_keys}. `severity` is REQUIRED and is `tripped`, `missing` or `passed`; a
  finding that does not say how bad it is is not counted as one that passed.
* `legend` — `{{"<label>": "<what it means>"}}`, the `l` view and the hover text.
* `look` — `{{"read": "<path relative to the workspace>"}}`, or absent for no look.
* `every_s` — how often to ask again: `0` (never) or at least {MIN_EVERY_S}; `every_s` must be a
  number even when the answer asks for no look.
* `state` — whatever this pane wants handed back next time, or absent.

The blocks, `{block_kinds}`:

* `{{"lines":[<line>…]}}` — the one grid: {line_keys}. `bar` is `{{"frac": 0.0..1.0}}` of the
  panel's denominator, `spark` is history of the same; a line with an `id` is a stop (Enter reads
  its `detail`).
* `{{"table":{{"columns":[…],"rows":[<row>…]}}}}` and `{{"tree":{{"rows":[<row>…]}}}}` — a row is
  {row_keys}; `children` nests a tree.
* `{{"markdown":"…"}}` — itemized: its headings and list items are stops.
* `{{"kv":{{"rows":[["label","value"],…]}}}}` — rows that carry no id, so no stop either.
* Any other kind draws one muted line naming it, so a newer plugin is legible on an older host.

`role` is one of {roles} — the palette, so a plugin's colour means what the rest of the head means
by it.

An action is `{{"key": "s", "label": "start", "say": "…"}}`, `{{"key": "r", "label": "refresh",
"look": true}}` or `{{"key": "x", "label": "expand", "event": "expand"}}` — exactly ONE of the
three. `say` types a line into the session's model as the operator; `look` re-runs the look now;
`event` calls the pane again with `{{"item": <the stop's id>, "action": <the word>}}`.

## What the host refuses

* **The host's keys are the host's**: {host_keys}, ↑/↓, PgUp/PgDn, Enter, Esc and the wheel. A
  panel key or an action key that collides with one of those, that is not one lowercase letter,
  that is another panel's, or that is a second action on the SAME stop — the whole view is refused
  and the pane draws why instead. A refused view never half-draws.
* `every_s` is `0` or at least {MIN_EVERY_S}.
* A finding with neither an `id` nor a `what` is refused.
* An answer that is not JSON, or not this document, is refused as a whole.

## What to do when it goes wrong

`plugin test <name>` calls this hook with an input and shows the view, the layer that answered, the
fuel it burned and any trap. `plugin load <name>` runs it through the shadow gate first, so a view
the host would refuse never reaches a screen.
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The worked example is one the host accepts** — the same parse and the same
    /// `View::validate` the daemon runs over a plugin's answer before publishing it. A document
    /// nobody can copy is worse than no document, and a model copies examples.
    #[test]
    fn the_worked_example_parses_and_validates() {
        let view: View = serde_json::from_value(example_json()).expect("the example is a View");
        view.validate()
            .expect("the example passes the host's own check");
    }

    /// **Every field the document lists comes from the example**, which is built from the host's
    /// types — so a renamed field is a document that moves with it, not a stale sentence.
    #[test]
    fn the_document_names_the_fields_the_hosts_types_have() {
        let doc = pane_doc();
        for field in [
            "title", "badge", "panels", "findings", "legend", "look", "every_s", "state",
        ] {
            let example = example_json();
            assert!(
                example.get(field).is_some(),
                "a view has no `{field}`, so the document cannot list it: {example}"
            );
            assert!(doc.contains(field), "the document never names `{field}`");
        }
        // The input's own four fields, as the host hands them over.
        let input = letibot_tools::builtins::plugin::synthetic_input();
        for field in ["v", "state", "look", "event", "size"] {
            assert!(input.get(field).is_some(), "the input has no `{field}`");
            assert!(doc.contains(field), "the document never names `{field}`");
        }
        // And the constants a well-formed answer is checked against are the host's, not typed.
        for key in HOST_KEYS {
            assert!(
                doc.contains(key),
                "the document never names the host key `{key}`"
            );
        }
        assert!(doc.contains(&format!("at least {MIN_EVERY_S}")));
    }

    /// **Every kind has a page, every page carries its own types' fields, and a name that is not a
    /// kind is refused with the ones there are.** The four patch points' pages are built the same
    /// way the pane's is, so a renamed field in `crates/plugin-sdk` moves the document.
    #[test]
    fn every_kind_is_documented_and_a_stranger_is_refused() {
        let pane = hooks_doc(Some("pane")).expect("the pane contract");
        assert!(pane.contains("pane_<name>"), "{pane}");
        let all = hooks_doc(None).expect("every kind");
        assert!(
            all.contains(&pane),
            "the whole document contains the pane's page"
        );
        for kind in KINDS {
            let doc = hooks_doc(Some(kind)).unwrap_or_else(|why| panic!("{kind}: {why}"));
            assert!(doc.contains(kind), "the {kind} page never names itself");
            assert!(
                doc.contains("The input") && doc.contains("worked example"),
                "the {kind} page has no worked example"
            );
            assert!(
                all.contains(&doc),
                "{kind} is missing from the whole document"
            );
        }
        // The fields a reader is promised come from the example, which comes from the SDK.
        let call = hooks_doc(Some("on_tool_call")).expect("the call page");
        for field in ["pass", "arguments", "refuse", "answer"] {
            assert!(call.contains(field), "the call page never names `{field}`");
        }
        let result = hooks_doc(Some("on_tool_result")).expect("the result page");
        for field in ["payload", "append", "arm"] {
            assert!(
                result.contains(field),
                "the result page never names `{field}`"
            );
        }
        let condition = hooks_doc(Some("condition")).expect("the condition page");
        for field in ["look", "every_s", "max_ttl_s", "fired", "state", "github"] {
            assert!(
                condition.contains(field),
                "the condition page never names `{field}`"
            );
        }
        let notice = hooks_doc(Some("notice")).expect("the notice page");
        for field in ["kind", "text", "facts", "drop"] {
            assert!(
                notice.contains(field),
                "the notice page never names `{field}`"
            );
        }
        let why = hooks_doc(Some("digest")).expect_err("not a kind this host runs");
        assert!(why.contains("`digest` is not a hook kind"), "{why}");
        assert!(why.contains("condition"), "{why}");
    }

    /// The block kinds the document lists are the variants the host draws: a sixth would be a
    /// document that forgot it, so the list is asserted against the type's own serializer.
    #[test]
    fn every_block_kind_the_host_serializes_is_in_the_document() {
        let doc = pane_doc();
        for block in [
            Block::Lines(vec![]),
            Block::Table(Table::default()),
            Block::Tree(Default::default()),
            Block::Markdown(String::new()),
            Block::Kv(Vec::new()),
        ] {
            let json = serde_json::to_value(&block).expect("a block serialises");
            let kind = json
                .as_object()
                .and_then(|m| m.keys().next())
                .cloned()
                .expect("one key per block");
            assert_eq!(kind, block.kind());
            assert!(
                doc.contains(&format!("`{kind}`")),
                "the document never names the `{kind}` block"
            );
        }
    }
}
