//! What a record kept, and what it dropped: compaction's fidelity gate, checkable without a
//! model. Facts are read from structure (`docs/compaction.md`) - tool names and arguments,
//! then plan rows compared by words. It claims what is missing, not whether the record is good.

use letibot_transcript::{TranscriptItem, UserPart};

/// The most facts of one class this reads from one conversation. A conversation longer than this
/// is measured on its first `MAX_FACTS` distinct facts of that class - a partial count rather than
/// an unbounded walk, since the walk happens on a turn's own thread.
pub const MAX_FACTS: usize = 400;

/// How many missing names a re-ask spells out before it says how many more there are.
pub const SHOWN: usize = 12;

/// The share of each level a record has to name, in per cent of that level's total. Files and
/// commands are exact strings the record either carries or does not; a plan row is prose and is
/// compared by its words, so it is held to a lower bar.
pub const FILES_FLOOR: usize = 80;
pub const COMMANDS_FLOOR: usize = 80;
pub const DECISIONS_FLOOR: usize = 50;

/// The words of a plan row that must appear for the row to count as carried: half of its words of
/// four characters or more, which lets *"decide the budget"* stand for *"decided about the budget"*
/// and does not let silence stand for either.
const ROW_WORDS_FLOOR: usize = 50;

/// The names of the levels, in the order they are reported.
pub const LEVELS: [&str; 3] = ["files", "commands", "decisions"];

/// What one class of fact came to: how many the conversation held, how many the record names, and
/// which ones it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Level {
    pub name: &'static str,
    pub total: usize,
    pub kept: usize,
    /// The facts this record does not name, in conversation order. The gap a re-ask is written
    /// from, and what a report shows when it says the record dropped something.
    pub missing: Vec<String>,
}

impl Level {
    /// The share this record names, in per cent, or `100` for a level the conversation never had -
    /// a record cannot drop what was never there.
    pub fn per_cent(&self) -> usize {
        if self.total == 0 {
            return 100;
        }
        self.kept * 100 / self.total
    }

    /// Is this level within its floor?
    pub fn passes(&self) -> bool {
        self.per_cent() >= floor_of(self.name)
    }

    /// `files 3 of 47`.
    pub fn line(&self) -> String {
        format!("{} {} of {}", self.name, self.kept, self.total)
    }
}

fn floor_of(name: &str) -> usize {
    match name {
        "files" => FILES_FLOOR,
        "commands" => COMMANDS_FLOOR,
        _ => DECISIONS_FLOOR,
    }
}

/// What a conversation held, as three lists of exact strings. Built once, before the summary turn,
/// because the turn appends its instruction and its answer to the session it is summarising.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// Paths named by a call's arguments, and files the operator attached.
    pub files: Vec<String>,
    /// Commands the conversation ran, each by its first line.
    pub commands: Vec<String>,
    /// The plan rows it wrote: what it said it decided to do.
    pub decisions: Vec<String>,
}

impl Facts {
    /// The facts of a conversation, read off its tool calls and its attachments. A call whose
    /// arguments are not JSON contributes nothing: a fact this cannot name would be a gap the
    /// model is told to close and cannot.
    pub fn of(items: &[TranscriptItem]) -> Facts {
        let mut facts = Facts::default();
        for item in items {
            match item {
                TranscriptItem::Assistant { tool_calls, .. } => {
                    for call in tool_calls {
                        let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.arguments)
                        else {
                            continue;
                        };
                        match call.name.as_str() {
                            // Every tool that names a file names it under one of these keys.
                            "bash" => {
                                if let Some(command) = args.get("command").and_then(|v| v.as_str())
                                {
                                    push(&mut facts.commands, command);
                                }
                            }
                            "todo_write" => {
                                for row in args
                                    .get("todos")
                                    .and_then(|v| v.as_array())
                                    .into_iter()
                                    .flatten()
                                {
                                    if let Some(content) =
                                        row.get("content").and_then(|v| v.as_str())
                                    {
                                        push(&mut facts.decisions, content);
                                    }
                                }
                            }
                            _ => {
                                for key in ["path", "file_path", "file", "paths"] {
                                    match args.get(key) {
                                        Some(serde_json::Value::String(s)) => {
                                            push(&mut facts.files, s)
                                        }
                                        Some(serde_json::Value::Array(vs)) => {
                                            for v in vs.iter().filter_map(|v| v.as_str()) {
                                                push(&mut facts.files, v);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                TranscriptItem::User { parts, .. } => {
                    for part in parts {
                        if let UserPart::FileRef { path, .. } = part {
                            push(&mut facts.files, path);
                        }
                    }
                }
                _ => {}
            }
        }
        facts
    }

    /// What a record names, measured against these facts. `record` is the summary AND whatever the
    /// compaction carries verbatim beside it: a fact in the tail is not lost, and counting it as
    /// lost would re-ask for something already in the base.
    pub fn measure(&self, record: &str) -> Fidelity {
        let hay = flat(record);
        let mut levels = Vec::with_capacity(LEVELS.len());
        for (name, facts) in [
            ("files", &self.files),
            ("commands", &self.commands),
            ("decisions", &self.decisions),
        ] {
            let mut kept = 0usize;
            let mut missing = Vec::new();
            for fact in facts {
                let found = if name == "decisions" {
                    row_covered(&hay, fact)
                } else {
                    hay.contains(&flat(fact))
                };
                if found {
                    kept += 1;
                } else {
                    missing.push(fact.clone());
                }
            }
            levels.push(Level {
                name,
                total: facts.len(),
                kept,
                missing,
            });
        }
        Fidelity { levels }
    }
}

/// One distinct fact, appended in the order it was first seen: the count is of what the
/// conversation touched, and a command run forty times was touched once.
fn push(into: &mut Vec<String>, raw: &str) {
    let Some(fact) = fact_of(raw) else {
        return;
    };
    if into.len() >= MAX_FACTS || into.contains(&fact) {
        return;
    }
    into.push(fact);
}

/// A fact, normalised for comparison and small enough to put in a sentence: one line, no runs of
/// whitespace, bounded to `FACT_BYTES` characters so a pasted log is not a fact.
fn fact_of(raw: &str) -> Option<String> {
    const FACT_CHARS: usize = 160;
    let first = raw.lines().next().unwrap_or("").trim();
    let fact = first.split_whitespace().collect::<Vec<_>>().join(" ");
    if fact.is_empty() || fact == "." {
        return None;
    }
    Some(match fact.char_indices().nth(FACT_CHARS) {
        Some((at, _)) => fact[..at].trim_end().to_string(),
        None => fact,
    })
}

/// Text as the comparison wants it: lower case, every run of whitespace one space. A record that
/// wraps a path across a line break still names the path.
fn flat(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Whether a record carries a plan row: its own bytes, or half of its words of four characters or
/// more. A row of one or two words is compared by its bytes alone, because one shared word is not
/// a paraphrase.
fn row_covered(hay: &str, row: &str) -> bool {
    let needle = flat(row);
    if needle.is_empty() || hay.contains(&needle) {
        return true;
    }
    let words: Vec<String> = needle
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(str::to_string)
        .collect();
    if words.len() < 2 {
        return false;
    }
    let found = words.iter().filter(|w| hay.contains(w.as_str())).count();
    found * 100 >= words.len() * ROW_WORDS_FLOOR
}

/// What a record kept, level by level.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fidelity {
    pub levels: Vec<Level>,
}

impl Fidelity {
    /// The level of that name, for a caller reporting one number.
    pub fn level(&self, name: &str) -> Option<&Level> {
        self.levels.iter().find(|l| l.name == name)
    }

    /// Is every level within its floor? A record nothing could be counted against passes: this
    /// gate refuses a record that DROPPED things, never one with nothing to drop.
    pub fn passed(&self) -> bool {
        self.levels.iter().all(Level::passes)
    }

    /// `files 3 of 47, commands 20 of 21, decisions 6 of 7` - the line a report and a re-ask are
    /// both built from, so the sentence a person reads and the sentence the model reads cannot
    /// disagree.
    pub fn line(&self) -> String {
        self.levels
            .iter()
            .map(Level::line)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// What to tell the model when this record is not good enough, or `None` when it is. Only the
    /// levels that fell short are listed: a level it already carries needs no sentence, and a
    /// model asked to add what it already wrote learns nothing.
    pub fn ask_again(&self) -> Option<String> {
        let short: Vec<&Level> = self.levels.iter().filter(|l| !l.passes()).collect();
        if short.is_empty() {
            return None;
        }
        let mut said = format!(
            "That record is not the record this needs: it carries {}. Write it again now, under \
             the same headings, and name every one of these:",
            self.line()
        );
        for level in short {
            let mut names: Vec<String> = level
                .missing
                .iter()
                .take(SHOWN)
                .map(|m| format!("`{m}`"))
                .collect();
            if level.missing.len() > SHOWN {
                names.push(format!("and {} more", level.missing.len() - SHOWN));
            }
            said.push_str(&format!("\n- {}: {}", level.name, names.join(", ")));
        }
        said.push_str(
            "\nOne line each. Do not merge entries, do not summarise across items, and do not \
             drop a row for being small.",
        );
        Some(said)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_transcript::{Speaker, SystemOrigin, ToolCall, ToolOutcome};

    fn user(text: &str) -> TranscriptItem {
        TranscriptItem::User {
            speaker: Speaker::Operator,
            parts: vec![UserPart::Text { text: text.into() }],
        }
    }

    fn told(text: &str) -> TranscriptItem {
        TranscriptItem::Assistant {
            text: text.into(),
            tool_calls: vec![],
            truncated: false,
        }
    }

    fn called(id: &str, name: &str, arguments: &str) -> TranscriptItem {
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: arguments.into(),
            }],
            truncated: false,
        }
    }

    fn answered(id: &str, name: &str) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: id.into(),
            name: name.into(),
            outcome: ToolOutcome::Ok,
            payload: "…".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        }
    }

    /// A conversation shaped like a real long one: 82 operator turns, 3590 rows' worth of tool
    /// calls, 47 files touched, 21 commands run, 7 plan rows written. The facts are in the
    /// calls, where the model read them, and nowhere else.
    fn a_long_conversation() -> Vec<TranscriptItem> {
        let mut items = vec![TranscriptItem::System {
            text: "be terse".into(),
            origin: SystemOrigin::Bootstrap,
        }];
        for i in 0..82u32 {
            items.push(user(&format!("step {i}: carry on")));
            items.push(told("looking at it"));
            if i < 47 {
                let args = format!(r#"{{"path":"crates/turn/src/file_{i}.rs"}}"#);
                items.push(called(&format!("r{i}"), "read", &args));
                items.push(answered(&format!("r{i}"), "read"));
            }
            if i < 21 {
                let args = format!(r#"{{"command":"cargo test -p pkg{i} --lib"}}"#);
                items.push(called(&format!("b{i}"), "bash", &args));
                items.push(answered(&format!("b{i}"), "bash"));
            }
            if i % 12 == 0 {
                let args = format!(
                    r#"{{"todos":[{{"content":"decide about the budget at step {i}","status":"pending"}}]}}"#
                );
                items.push(called(&format!("t{i}"), "todo_write", &args));
            }
        }
        items
    }

    /// Every fact named, as a good record names them: one line each, under the template's own
    /// headings.
    fn the_whole_record(facts: &Facts) -> String {
        let mut said = String::from("## Objective\n- carry the conversation across\n");
        said.push_str("## Relevant Files\n");
        for f in &facts.files {
            said.push_str(&format!("- {f}: read for the budget\n"));
        }
        said.push_str("## Important Details\n");
        for c in &facts.commands {
            said.push_str(&format!("- `{c}` → passed\n"));
        }
        said.push_str("## Next Move\n");
        for d in &facts.decisions {
            said.push_str(&format!("1. {d}\n"));
        }
        said
    }

    #[test]
    fn a_long_conversation_is_counted_off_its_calls() {
        let facts = Facts::of(&a_long_conversation());
        assert_eq!(facts.files.len(), 47, "{:?}", facts.files);
        assert_eq!(facts.files[0], "crates/turn/src/file_0.rs");
        assert_eq!(facts.commands.len(), 21);
        assert_eq!(facts.commands[0], "cargo test -p pkg0 --lib");
        assert_eq!(facts.decisions.len(), 7, "{:?}", facts.decisions);
    }

    /// The measured case, as a number: a record naming three files out of forty-seven has failed,
    /// and the gate can say so without a model.
    #[test]
    fn a_record_naming_three_of_forty_seven_files_has_failed() {
        let facts = Facts::of(&a_long_conversation());
        let record = "## Objective\n- carry on\n## Relevant Files\n\
                      - crates/turn/src/file_0.rs: first\n\
                      - crates/turn/src/file_1.rs: second\n\
                      - crates/turn/src/file_2.rs: third\n";
        let f = facts.measure(record);
        assert!(!f.passed());
        let files = f.level("files").expect("the files level");
        assert_eq!((files.kept, files.total), (3, 47));
        assert!(f.line().starts_with("files 3 of 47"), "{}", f.line());
    }

    /// A record that names what the conversation held passes, and it passes whole: this gate is a
    /// floor, not a score.
    #[test]
    fn a_record_naming_everything_passes() {
        let facts = Facts::of(&a_long_conversation());
        let f = facts.measure(&the_whole_record(&facts));
        assert!(f.passed(), "{}", f.line());
        assert_eq!(f.levels.len(), 3);
        assert_eq!(f.level("files").unwrap().kept, 47);
        assert!(f.ask_again().is_none());
    }

    /// The gap names the things, not only the count: a model told *"44 files are missing"* cannot
    /// do anything with it, and one told which can.
    #[test]
    fn the_gap_names_what_the_record_dropped() {
        let facts = Facts::of(&a_long_conversation());
        let f = facts.measure("## Objective\n- carry on\n");
        let ask = f.ask_again().expect("a record this thin is re-asked");
        assert!(ask.contains("file_0.rs"), "{ask}");
        assert!(
            ask.contains("and 35 more"),
            "twelve names, then the count of the rest: {ask}"
        );
        assert!(ask.contains("cargo test -p pkg0 --lib"), "{ask}");
    }

    /// A fact the verbatim tail carries is in the new base and is not a fact this record dropped.
    #[test]
    fn a_fact_the_verbatim_tail_carries_is_not_dropped() {
        let facts = Facts::of(&a_long_conversation());
        let mut record = String::from("## Objective\n- carry on\n");
        for f in facts.files.iter().take(20) {
            record.push_str(&format!("- {f}\n"));
        }
        for c in &facts.commands {
            record.push_str(&format!("- `{c}`\n"));
        }
        for d in &facts.decisions {
            record.push_str(&format!("1. {d}\n"));
        }
        assert!(!facts.measure(&record).passed(), "20 of 47 files is short");
        let tail: String = facts
            .files
            .iter()
            .skip(20)
            .map(|f| format!("read {f}\n"))
            .collect();
        let with_tail = facts.measure(&format!("{record}{tail}"));
        assert!(with_tail.passed(), "{}", with_tail.line());
    }

    /// A plan row is prose, so a record may reword it - but not lose it.
    #[test]
    fn a_plan_row_may_be_reworded_and_not_vanished() {
        let row = "decide about the budget at step 0";
        let covered = Facts {
            decisions: vec![row.into()],
            ..Facts::default()
        };
        assert!(
            covered
                .measure("## Next Move\n1. the budget decision at step 0\n")
                .passed(),
            "half a row's own words is the row"
        );
        assert!(
            !covered
                .measure("## Next Move\n1. carry on with the work\n")
                .passed(),
            "a record that says nothing about it dropped it"
        );
    }

    /// A command is compared by its first line, and a command run twice is one thing the
    /// conversation did - the same way the store's rows are 3,590 and the commands are 21.
    #[test]
    fn a_command_is_its_first_line_and_counts_once() {
        let items = vec![
            called("a", "bash", r#"{"command":"cargo build\n  && echo done"}"#),
            called("b", "bash", r#"{"command":"cargo build"}"#),
            called("c", "bash", r#"{"command":"cargo build\n"}"#),
        ];
        let facts = Facts::of(&items);
        assert_eq!(facts.commands, vec!["cargo build".to_string()]);
    }

    /// A call this cannot read contributes nothing, and nothing is not a gap: a model told to
    /// close a gap nobody can name is being sent after a fact that does not exist.
    #[test]
    fn a_call_whose_arguments_are_not_json_is_skipped() {
        let items = vec![
            called("a", "read", "not json at all"),
            called("b", "bash", ""),
            called("c", "write", r#"{"path":""}"#),
        ];
        let facts = Facts::of(&items);
        assert!(facts.files.is_empty(), "{:?}", facts.files);
        assert!(facts.commands.is_empty(), "{:?}", facts.commands);
        assert!(facts.decisions.is_empty());
        assert!(
            facts.measure("## Objective\n- nothing to carry\n").passed(),
            "a conversation with no facts cannot have dropped one"
        );
    }

    /// The attachments the operator made are files the conversation touched, and they survive in
    /// no call at all.
    #[test]
    fn a_file_the_operator_attached_is_a_file_the_conversation_touched() {
        let items = vec![TranscriptItem::User {
            speaker: Speaker::Operator,
            parts: vec![UserPart::FileRef {
                path: "docs/compaction.md".into(),
                sha256: "0".repeat(64),
            }],
        }];
        assert_eq!(
            Facts::of(&items).files,
            vec!["docs/compaction.md".to_string()]
        );
    }
}
