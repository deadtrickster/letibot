//! `letibot shadow replay <session|project>`: run the turn_end job over logged sessions, read-only,
//! and score it and the regex against what the operator did next.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use letibot_tokencore::store::{Store, StoredSession, split};
use letibot_transcript::{Speaker, TranscriptItem};

use super::Job;
use super::judge::Judge;
use super::runner::Outcome;
use super::verdict::{self, Reminder, Verdict};

/// What happened after a turn end, as the label it is scored against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    /// The operator's next words only told it to go on: the turn owed work.
    Owed,
    /// The operator moved on, or answered a question: nothing was owed.
    None,
}

/// The operator's words that only push a stopped turn on.
const GO_ON: [&str; 15] = [
    "continue",
    "go on",
    "go ahead",
    "proceed",
    "carry on",
    "keep going",
    "do it",
    "and",
    "so",
    "next",
    "resume",
    "please continue",
    "ok continue",
    "yes continue",
    "status",
];

fn is_go_on(text: &str) -> bool {
    let t: String = text
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '?' | '.' | '!' | ','))
        .collect();
    t.len() <= 40 && GO_ON.contains(&t.trim())
}

/// One turn end with its label and the regex's call.
#[derive(Debug, Clone)]
pub struct Case {
    pub at: usize,
    pub reply: String,
    pub operator: Option<String>,
    pub label: Label,
    pub regex_owed: bool,
}

/// Every labelled turn end in `items`: a finished text-only answer followed by an operator row.
pub fn cases(items: &[TranscriptItem]) -> Vec<Case> {
    let mut out = Vec::new();
    let mut operator: Option<String> = None;
    for (i, item) in items.iter().enumerate() {
        match item {
            TranscriptItem::User {
                speaker: Speaker::Operator,
                ..
            } => operator = Some(letibot_sessionlog::view::body_of(item)),
            TranscriptItem::Assistant {
                text,
                tool_calls,
                truncated: false,
            } if tool_calls.is_empty() && !text.trim().is_empty() => {
                let next = items[i + 1..].iter().find(|n| {
                    !matches!(
                        n,
                        TranscriptItem::Offer { .. }
                            | TranscriptItem::SegmentMark { .. }
                            | TranscriptItem::Reasoning { .. }
                    )
                });
                let Some(
                    n @ TranscriptItem::User {
                        speaker: Speaker::Operator,
                        ..
                    },
                ) = next
                else {
                    continue;
                };
                let said = letibot_sessionlog::view::body_of(n);
                let label = if is_go_on(&said) && !text.trim_end().ends_with('?') {
                    Label::Owed
                } else {
                    Label::None
                };
                out.push(Case {
                    at: i,
                    reply: text.clone(),
                    operator: operator.clone(),
                    label,
                    regex_owed: !letibot_tools::builtins::intent::commitments(text).is_empty(),
                });
            }
            _ => {}
        }
    }
    out
}

/// Hits and misses per predictor.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Score {
    pub owed_right: u32,
    pub owed_wrong: u32,
    pub none_right: u32,
    pub none_wrong: u32,
}

impl Score {
    pub fn add(&mut self, said_owed: bool, label: Label) {
        match (said_owed, label) {
            (true, Label::Owed) => self.owed_right += 1,
            (true, Label::None) => self.owed_wrong += 1,
            (false, Label::None) => self.none_right += 1,
            (false, Label::Owed) => self.none_wrong += 1,
        }
    }

    pub fn total(&self) -> u32 {
        self.owed_right + self.owed_wrong + self.none_right + self.none_wrong
    }

    pub fn accuracy(&self) -> f64 {
        match self.total() {
            0 => 0.0,
            n => f64::from(self.owed_right + self.none_right) / f64::from(n),
        }
    }

    fn line(&self, who: &str) -> String {
        format!(
            "{who:<8} {:>10} {:>10} {:>10} {:>10} {:>9.2}",
            self.owed_right,
            self.owed_wrong,
            self.none_right,
            self.none_wrong,
            self.accuracy()
        )
    }
}

/// The replay's result: the regex, the shadow raw, and the shadow as it would act (falling back to
/// the regex on unsure, void or a miss).
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub turns: usize,
    pub regex: Score,
    pub shadow: Score,
    pub acting: Score,
    pub fell_back: u32,
    pub lines: Vec<String>,
}

/// A session's id and its rows, as the replay reads them.
pub type Logged = (String, Vec<TranscriptItem>);

/// Judge every case with `judge` and score it. Read-only: nothing here writes anything.
pub fn score(sessions: &[Logged], judge: &dyn Judge) -> Report {
    let mut r = Report::default();
    for (id, items) in sessions {
        for c in cases(items) {
            r.turns += 1;
            r.regex.add(c.regex_owed, c.label);
            let shown = super::turn_end::shown(c.operator.as_deref(), &c.reply, &[], &[]);
            let outcome = match judge.judge(
                super::prompts::TURN_END,
                &super::prompts::fence(&shown.data),
            ) {
                Ok(reply) => match verdict::check(Job::TurnEnd, &reply, &shown).verdict {
                    Ok(v) => Outcome::Verdict(v),
                    Err(why) => Outcome::Void(why),
                },
                Err(e) => Outcome::Failed(e),
            };
            let said = match &outcome {
                Outcome::Verdict(Verdict::TurnEnd { reminder, .. }) => match reminder {
                    Reminder::Owed { .. } => Some(true),
                    Reminder::None => Some(false),
                    Reminder::Unsure => None,
                },
                _ => None,
            };
            match said {
                Some(owed) => {
                    r.shadow.add(owed, c.label);
                    r.acting.add(owed, c.label);
                }
                None => {
                    r.fell_back += 1;
                    r.acting.add(c.regex_owed, c.label);
                }
            }
            r.lines.push(format!(
                "{id} #{}: label {:?}, regex {}, shadow {}",
                c.at,
                c.label,
                if c.regex_owed { "owed" } else { "none" },
                match said {
                    Some(true) => "owed",
                    Some(false) => "none",
                    None => "fell back",
                }
            ));
        }
    }
    r
}

impl Report {
    pub fn said(&self, verbose: bool) -> String {
        let mut out = vec![
            format!(
                "turn_end replay: {} labelled turn ends; label = the operator's next words",
                self.turns
            ),
            format!(
                "{:<8} {:>10} {:>10} {:>10} {:>10} {:>9}",
                "", "owed-right", "owed-wrong", "none-right", "none-wrong", "accuracy"
            ),
            self.regex.line("regex"),
            self.shadow.line("shadow"),
            self.acting.line("acting"),
            format!(
                "shadow fell back to the regex on {} (unsure, void or no answer)",
                self.fell_back
            ),
        ];
        if verbose {
            out.extend(self.lines.iter().cloned());
        }
        out.join("\n")
    }
}

/// The permission replay's tally against the operator's answers. Today's mechanism asks every one
/// of these, so its row is all `asked`; a shadow allow the operator refused is the costly miss.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Permits {
    pub allow_right: u32,
    pub allow_wrong: u32,
    pub asked: u32,
    pub fell_back: u32,
}

/// One asked call as the store kept it: what was called, and whether the operator admitted it.
#[derive(Debug, Clone)]
pub struct Asked {
    pub request: String,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub admitted: bool,
}

/// Judge every asked call and score the allows against the operator's answers. Read-only.
pub fn score_permission(
    asked: &[Asked],
    rules: &[String],
    workspace: &Path,
    judge: &dyn Judge,
) -> (Permits, Vec<String>) {
    let mut p = Permits::default();
    let mut lines = Vec::new();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    for a in asked {
        let call = super::guard::Call {
            tool: a.tool.clone(),
            args: a.arguments.clone(),
            cwd: workspace.to_path_buf(),
            workspace: workspace.to_path_buf(),
            home: home.clone(),
        };
        let shown = super::permission::shown(&a.request, &a.tool, &a.arguments, rules, None, call);
        let word = match judge.judge(
            super::prompts::PERMISSION,
            &super::prompts::fence(&shown.data),
        ) {
            Ok(reply) => match verdict::check(Job::Permission, &reply, &shown).verdict {
                Ok(Verdict::Permission {
                    decision: verdict::Decision::Allow { .. },
                    ..
                }) => "allow",
                Ok(_) => "ask",
                Err(_) => "fell back",
            },
            Err(_) => "fell back",
        };
        match (word, a.admitted) {
            ("allow", true) => p.allow_right += 1,
            ("allow", false) => p.allow_wrong += 1,
            ("ask", _) => p.asked += 1,
            _ => p.fell_back += 1,
        }
        lines.push(format!(
            "{}: {} {} -> shadow {word}, operator {}",
            a.request,
            a.tool,
            super::during::cut(&a.arguments.to_string(), 80),
            if a.admitted { "admitted" } else { "refused" }
        ));
    }
    (p, lines)
}

/// One logged skill or note offer: when, to which session, what, and whether the round took it.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct LoggedOffer {
    pub at: u64,
    pub session: String,
    #[serde(alias = "note", alias = "path")]
    pub skill: String,
    #[serde(default)]
    pub taken: bool,
    /// `subject`, `prompt` or `shadow`. A shadow-made offer is not a baseline to score it against.
    #[serde(default)]
    pub by: String,
}

/// The during replay's tally against what the rounds took: an offer the shadow keeps and the
/// round loaded is right, one it keeps and the round ignored is noise.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Offers {
    pub kept_taken: u32,
    pub kept_ignored: u32,
    pub dropped_taken: u32,
    pub dropped_ignored: u32,
    pub fell_back: u32,
}

/// Judge every logged offer over the session as it stood when the offer was made. `rows` holds a
/// session's items with the time each was written. Read-only.
pub fn score_offers(
    offers: &[LoggedOffer],
    rows: &std::collections::BTreeMap<String, Vec<(u64, TranscriptItem)>>,
    judge: &dyn Judge,
) -> Offers {
    let mut o = Offers::default();
    for off in offers {
        let Some(all) = rows.get(&off.session) else {
            continue;
        };
        let items: Vec<TranscriptItem> = all
            .iter()
            .filter(|(at, _)| *at <= off.at)
            .map(|(_, i)| i.clone())
            .collect();
        let entry = super::during::Entry {
            name: off.skill.clone(),
            about: String::new(),
        };
        let shown = super::during::shown(&items, &[], &[entry], &Default::default());
        let kept = match judge.judge(super::prompts::DURING, &super::prompts::fence(&shown.data)) {
            Ok(reply) => match verdict::check(Job::During, &reply, &shown).verdict {
                Ok(Verdict::During { offers, .. }) => {
                    Some(offers.iter().any(|p| p.name == off.skill))
                }
                _ => None,
            },
            Err(_) => None,
        };
        match (kept, off.taken) {
            (Some(true), true) => o.kept_taken += 1,
            (Some(true), false) => o.kept_ignored += 1,
            (Some(false), true) => o.dropped_taken += 1,
            (Some(false), false) => o.dropped_ignored += 1,
            (None, _) => o.fell_back += 1,
        }
    }
    o
}

/// The store read-only: `$LETIBOT_STORE` else this box's data directory, the project file of `key`.
fn open_reader(key: &str) -> Result<Store, String> {
    let target = std::env::var_os("LETIBOT_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(letibot_tools::lifecycle::data_dir);
    if target.is_file() {
        return Store::open_reader(&target).map_err(|e| format!("{}: {e}", target.display()));
    }
    Store::open_set_reader(&target, key).map_err(|e| format!("{}: {e}", target.display()))
}

fn shared_reader() -> Result<Store, String> {
    let target = std::env::var_os("LETIBOT_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(letibot_tools::lifecycle::data_dir);
    let path = if target.is_dir() {
        split::shared_path(&target)
    } else {
        target
    };
    Store::open_reader(&path).map_err(|e| format!("{}: {e}", path.display()))
}

fn is_conversation(s: &StoredSession) -> bool {
    s.parent_session_id.is_none()
        && matches!(s.origin.as_deref(), None | Some("operator"))
        && !letibot_sessionlog::is_gatekeeper_session(&s.id)
}

/// What a replay reads: the sessions in scope with their rows, the watched model, the project, and
/// the project's store, all read-only.
struct Scope {
    sessions: Vec<Logged>,
    model: String,
    workspace: String,
    store: Store,
}

/// The sessions `what` names: one session id, or every conversation of a project folder.
fn scope_of(what: &str) -> Result<Scope, String> {
    let shared = shared_reader()?;
    let (workspace, ids): (String, Vec<StoredSession>) = if Path::new(what).is_dir() {
        let ws = std::fs::canonicalize(what)
            .map_err(|e| format!("{what}: {e}"))?
            .to_string_lossy()
            .into_owned();
        let all = shared.list_sessions().map_err(|e| e.to_string())?;
        let mine = all
            .into_iter()
            .filter(|s| s.workspace_root == ws && is_conversation(s))
            .collect();
        (ws, mine)
    } else {
        let s = shared
            .session(what)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no session `{what}` in the store"))?;
        (s.workspace_root.clone(), vec![s])
    };
    let store = open_reader(&letibot_tokencore::migrate_split::project_key(&workspace))?;
    let model = ids.first().map(|s| s.model_id.clone()).unwrap_or_default();
    let mut sessions = Vec::new();
    for s in ids {
        let Some(tid) = s.transcript_id.clone() else {
            continue;
        };
        if let Ok(t) = store.load_transcript(&tid) {
            sessions.push((s.id, t.items.into_iter().map(|(i, _, _)| i).collect()));
        }
    }
    Ok(Scope {
        sessions,
        model,
        workspace,
        store,
    })
}

/// Each session's rows with the time each was written, for an offer judged as of its own moment.
fn timed_rows(scope: &Scope) -> std::collections::BTreeMap<String, Vec<(u64, TranscriptItem)>> {
    let mut out = std::collections::BTreeMap::new();
    for (id, _) in &scope.sessions {
        let Ok(Some(tid)) = scope.store.current_transcript_id(id) else {
            continue;
        };
        let Ok(rows) = scope.store.rows_json_from(&tid, 0, u32::MAX) else {
            continue;
        };
        let items = rows
            .into_iter()
            .filter_map(|(_, _, _, json, at)| Some((at, serde_json::from_str(&json).ok()?)))
            .collect();
        out.insert(id.clone(), items);
    }
    out
}

/// The offers the sessions in scope were made, from their take-up logs.
fn logged_offers(scope: &Scope) -> Vec<LoggedOffer> {
    let root = std::env::var_os("LETIBOT_LOGS_ROOT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::mergequeue::git::letibot_data_dir().map(|d| d.join("logs")));
    let mut out = Vec::new();
    for (id, _) in &scope.sessions {
        let dirs = [
            root.as_ref().map(|r| r.join(id)),
            Some(Path::new(&scope.workspace).join(".letibot")),
        ];
        for dir in dirs.into_iter().flatten() {
            for log in [crate::skills_offer::LOG, crate::notes_offer::LOG] {
                let Ok(text) = std::fs::read_to_string(dir.join(log)) else {
                    continue;
                };
                out.extend(
                    text.lines()
                        .filter_map(|l| serde_json::from_str::<LoggedOffer>(l).ok())
                        .filter(|o| &o.session == id && o.by != "shadow"),
                );
            }
        }
    }
    out
}

/// The calls the operator was asked about in the sessions in scope, with their answers.
fn asked_calls(scope: &Scope) -> Vec<Asked> {
    let ids: std::collections::BTreeSet<&str> =
        scope.sessions.iter().map(|(id, _)| id.as_str()).collect();
    scope
        .store
        .corpus(false, 100_000)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.asked && ids.contains(r.session_id.as_str()))
        .map(|r| Asked {
            request: r.request_id,
            tool: r.tool,
            arguments: serde_json::from_str(&r.arguments_json).unwrap_or_default(),
            admitted: r.effect == "admit",
        })
        .collect()
}

/// The verb. `--job` picks what is replayed (turn_end by default); `--model M` names the model to
/// judge with, else the job's `[shadow]` line for the sessions' model, else `[default]`.
pub fn run(args: &[String]) -> ExitCode {
    let usage = "usage: letibot shadow replay <session-id|project-dir> \
                 [--job turn_end|during|permission] [--model M] [--verbose]";
    let mut what = None;
    let mut model = None;
    let mut job = Job::TurnEnd;
    let mut verbose = false;
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("replay") => {}
        _ => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "--model" => model = it.next().cloned(),
            "--job" => match it.next().and_then(|j| Job::parse(j)) {
                Some(j) => job = j,
                None => {
                    eprintln!("{usage}");
                    return ExitCode::from(2);
                }
            },
            "--verbose" | "-v" => verbose = true,
            other if what.is_none() => what = Some(other.to_string()),
            _ => {
                eprintln!("{usage}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(what) = what else {
        eprintln!("{usage}");
        return ExitCode::from(2);
    };
    let scope = match scope_of(&what) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("letibot shadow replay: {e}");
            return ExitCode::FAILURE;
        }
    };
    let model = match model {
        Some(m) => m,
        None => match super::config::ShadowConfig::load(None) {
            Ok(c) => c.model_line(job, &scope.model),
            Err(e) => {
                eprintln!("letibot shadow replay: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let local = letibot_turn::http::Endpoint::new("127.0.0.1", 8080);
    let judge = match super::judge::build(&model, job, None, &local) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("letibot shadow replay: {model}: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "replaying {} on {} over {} session(s), read-only",
        job.as_str(),
        judge.label(),
        scope.sessions.len()
    );
    match job {
        Job::TurnEnd => println!("{}", score(&scope.sessions, judge.as_ref()).said(verbose)),
        Job::Permission => {
            let rules = letibot_tools::permission::file_path()
                .and_then(|p| letibot_tools::permission::load_file(&p).ok())
                .map(|r| super::permission::rules_of(&r))
                .unwrap_or_default();
            let asked = asked_calls(&scope);
            let (p, lines) =
                score_permission(&asked, &rules, Path::new(&scope.workspace), judge.as_ref());
            println!(
                "permission replay: {} asked calls; today every one is asked\n\
                 allow, operator admitted: {}\nallow, operator refused: {}\nask: {}\nfell back: {}",
                asked.len(),
                p.allow_right,
                p.allow_wrong,
                p.asked,
                p.fell_back
            );
            if verbose {
                println!("{}", lines.join("\n"));
            }
        }
        Job::During => {
            let offers = logged_offers(&scope);
            let o = score_offers(&offers, &timed_rows(&scope), judge.as_ref());
            println!(
                "during replay: {} logged offers; today every one is made\n\
                 kept, taken: {}\nkept, ignored: {}\ndropped, taken: {}\ndropped, ignored: {}\n\
                 fell back: {}",
                offers.len(),
                o.kept_taken,
                o.kept_ignored,
                o.dropped_taken,
                o.dropped_ignored,
                o.fell_back
            );
        }
    }
    ExitCode::SUCCESS
}
