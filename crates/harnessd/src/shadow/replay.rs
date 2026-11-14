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

/// The sessions `what` names: one session id, or every conversation of a project folder.
fn sessions_of(what: &str) -> Result<(Vec<Logged>, String), String> {
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
    let mut out = Vec::new();
    for s in ids {
        let Some(tid) = s.transcript_id.clone() else {
            continue;
        };
        if let Ok(t) = store.load_transcript(&tid) {
            out.push((s.id, t.items.into_iter().map(|(i, _, _)| i).collect()));
        }
    }
    Ok((out, model))
}

/// The verb. `--model M` names the model to judge with; else the `[shadow]` turn_end line for the
/// sessions' model, else `[default]`.
pub fn run(args: &[String]) -> ExitCode {
    let usage = "usage: letibot shadow replay <session-id|project-dir> [--model M] [--verbose]";
    let mut what = None;
    let mut model = None;
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
    let (sessions, watched) = match sessions_of(&what) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("letibot shadow replay: {e}");
            return ExitCode::FAILURE;
        }
    };
    let model = match model {
        Some(m) => m,
        None => match super::config::ShadowConfig::load(None) {
            Ok(c) => c.model_line(Job::TurnEnd, &watched),
            Err(e) => {
                eprintln!("letibot shadow replay: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let local = letibot_turn::http::Endpoint::new("127.0.0.1", 8080);
    let judge = match super::judge::build(&model, Job::TurnEnd, None, &local) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("letibot shadow replay: {model}: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "replaying turn_end on {} over {} session(s), read-only",
        judge.label(),
        sessions.len()
    );
    println!("{}", score(&sessions, judge.as_ref()).said(verbose));
    ExitCode::SUCCESS
}
