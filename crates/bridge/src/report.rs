//! **The brief's closing report, parsed into fields.**
//!
//! `docs/claude-code-bridge.md`: *"the report as data … asked for in a fixed markdown
//! shape, and `leticode_status` parses it into fields when the last assistant message has
//! it, so the review starts from data."* A reviewer that reads four headings out of a
//! ten-thousand-line transcript by hand is doing the parsing this module exists to do
//! once.
//!
//! # What makes a message a report
//!
//! Headings, and their wording is the brief's rather than this file's — `## What I built`,
//! `## Test results`, `## What I could not do`, `## Contracts`. Two recognised sections is
//! the threshold: one heading is something a long answer may have for its own reasons
//! (*"## Test results"* is a heading an ordinary reply about a test run can carry), while
//! two of the brief's four is a shape nobody writes by accident.
//!
//! # Why the sections are matched by keyword and not by position
//!
//! The briefs are written by a person, and the operator's own reports in this tree move
//! between *"## What I built"* and *"## What was built"*, *"## Test results"* and *"## What
//! I tested"*, *"## Contracts"* and *"## Contract changes"*. A positional parser would
//! return the contract section as the tests the first time a brief added a heading, and a
//! wrong field that looks populated is worse than a missing one.
//!
//! **The one order that matters is `could not` before `test`**: *"## What I could not
//! test"* is the failure section, and a `contains("test")` tried first files it as results
//! — a report that says it tested nothing would read as a report of tests.

use serde_json::{Value, json};

/// The brief's report, section by section. A section the message did not have is empty
/// rather than absent: *"no tests section"* and *"a tests section with no words in it"*
/// are the same fact for a reviewer, and one field is one question to ask.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub built: String,
    pub tested: String,
    pub could_not: String,
    pub contracts: String,
    /// Short shas, in the order they appear, without duplicates.
    pub commits: Vec<String>,
}

impl Report {
    pub fn to_json(&self) -> Value {
        let mut out = json!({
            "built": self.built,
            "tested": self.tested,
            "could_not": self.could_not,
            "contracts": self.contracts,
        });
        // Omitted rather than empty, because `docs/claude-code-bridge.md`'s table writes
        // it `commits?`: a report that names no commits and a daemon that parsed none are
        // the same answer, and an empty list would invite a reader to distinguish them.
        if !self.commits.is_empty() {
            out["commits"] = json!(self.commits);
        }
        out
    }
}

/// Which of the report's sections a heading names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Built,
    Tested,
    CouldNot,
    Contracts,
    Commits,
}

/// The section a heading names, or `None` for a heading that is not the report's.
fn section_of(heading: &str) -> Option<Section> {
    let h = heading.trim().to_lowercase();
    let has = |needle: &str| h.contains(needle);
    if has("could not")
        || has("couldn't")
        || has("cannot")
        || has("can not")
        || has("unable")
        || has("limitation")
        || has("did not")
        || has("not done")
        || has("left undone")
        || has("not do")
    {
        Some(Section::CouldNot)
    } else if has("contract") {
        // Before `test` and `built`: *"the contracts I changed"* names neither, but
        // *"contract changes I could not make"* must not become a build section.
        Some(Section::Contracts)
    } else if has("test") {
        Some(Section::Tested)
    } else if has("built")
        || has("build")
        || has("implement")
        || has("delivered")
        || has("what i did")
        || has("changed")
    {
        Some(Section::Built)
    } else if has("commit") {
        Some(Section::Commits)
    } else {
        None
    }
}

/// **Parse the brief's report out of a message**, or `None` when the message is not one.
///
/// Every heading is a section boundary, whatever its level: a report written with `###`
/// under a `##` title is the same report, and a parser that only believed in `##` would
/// silently fold three sections into the fourth.
pub fn parse(message: &str) -> Option<Report> {
    let mut found: Vec<(Section, String)> = Vec::new();
    let mut current: Option<(Section, Vec<&str>)> = None;
    // **A fenced block is body, never structure.** A report that quotes a shell script
    // has `# a comment` lines in it, and one of those read as a heading would file half a
    // section under another heading — the text is still there, under the wrong name.
    let mut fenced = false;
    for line in message.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            if let Some((_, body)) = current.as_mut() {
                body.push(line);
            }
            continue;
        }
        let after = trimmed.trim_start_matches('#');
        let is_heading = !fenced && after.len() < trimmed.len() && after.starts_with(' ');
        if is_heading {
            if let Some((section, body)) = current.take() {
                found.push((section, lines_joined(&body)));
            }
            current = section_of(after).map(|s| (s, Vec::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push(line);
        }
    }
    if let Some((section, body)) = current.take() {
        found.push((section, lines_joined(&body)));
    }
    // One heading is an answer that happens to have a heading; the brief's report has at
    // least two of its four, and this is the line between them.
    if found.len() < 2 {
        return None;
    }
    let mut report = Report::default();
    let mut named: Option<String> = None;
    for (section, body) in found {
        match section {
            Section::Built => join_into(&mut report.built, body),
            Section::Tested => join_into(&mut report.tested, body),
            Section::CouldNot => join_into(&mut report.could_not, body),
            Section::Contracts => join_into(&mut report.contracts, body),
            Section::Commits => match &mut named {
                Some(text) => {
                    text.push_str("\n\n");
                    text.push_str(&body);
                }
                None => named = Some(body),
            },
        }
    }
    // **The shas come from the commits section when there is one**, and from the report
    // as a whole when there is not: the briefs ask for commits as a heading, and a model
    // that instead wrote *"commit `4621097e`"* inside its build section has still told a
    // reviewer the one thing that field is for.
    let scan = match &named {
        Some(body) => body.clone(),
        None => [
            &report.built,
            &report.tested,
            &report.could_not,
            &report.contracts,
        ]
        .map(String::as_str)
        .join("\n"),
    };
    report.commits = short_shas(&scan);
    Some(report)
}

/// A section body with the blank lines a heading leaves at its edges taken off.
fn lines_joined(lines: &[&str]) -> String {
    lines.join("\n").trim().to_string()
}

/// Two sections of the same class — a report that wrote *"## Test results"* twice — are
/// one field with both bodies, in the order they were written.
fn join_into(field: &mut String, body: String) {
    if body.is_empty() {
        return;
    }
    if !field.is_empty() {
        field.push_str("\n\n");
    }
    field.push_str(&body);
}

/// **The commit shas a report names**, in order, without duplicates.
///
/// A token has to be a whole word of 7 to 40 hexadecimal characters with **at least one
/// digit in it**. The digit is what keeps English out: `deadbeef`, `facade`, `acceded`
/// are hex-only words, and a `cafe`-shaped word inside a sentence is prose rather than a
/// commit. It costs the one-in-a-million sha that is all letters, which is the right way
/// round — a field that sometimes names a word is worse than one that occasionally misses.
fn short_shas(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if token.len() < 7 || token.len() > 40 {
            continue;
        }
        if !token.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if !token.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        let sha = token.to_ascii_lowercase();
        if !out.contains(&sha) {
            out.push(sha);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A report as this tree's briefs ask for it**, headings and all — the shape a
    /// reviewer's `leticode_status` call has to turn into four fields and a sha.
    const REAL_SHAPED: &str = "\
## What I built

- `crates/bridge/src/report.rs` — the parser, with its section rules.
- `crates/bridge/src/tools.rs` — `leticode_events`, eight tools plus one.

## Test results

```
cargo test --offline -p letibot-bridge
test result: ok. 24 passed; 0 failed
```

## What I could not do

The started-daemon branch has no end-to-end test: no test may spawn a daemon.

## Contracts

None: `docs/plugins-slice1.md` was not touched.

Commit `4621097e`.
";

    /// **The same report in other words**, at another heading level — which is what a
    /// brief that says *"headings like …"* has to tolerate.
    const OTHER_WORDING: &str = "\
# Package B2 report

### Built

`head.rs` folds the ring.

### Testing

`cargo test -p letibot-bridge`: 24 passed.

### Limitation: no daemon spawn

The `started` branch is untested here.

### Contract changes

None.

Commits: 7a1c0de and 4621097E, plus 7a1c0de again.
";

    #[test]
    fn a_real_shaped_report_parses_into_four_sections_and_a_sha() {
        let r = parse(REAL_SHAPED).expect("this is the brief's own report shape");
        assert!(r.built.contains("report.rs"), "{}", r.built);
        assert!(r.tested.contains("24 passed"), "{}", r.tested);
        assert!(r.could_not.contains("no test may spawn"), "{}", r.could_not);
        assert!(r.contracts.contains("plugins-slice1"), "{}", r.contracts);
        assert_eq!(r.commits, vec!["4621097e".to_string()]);
        // The heading itself is not part of the section: a reviewer reading `built` should
        // not be told a second time what the section is called.
        assert!(!r.built.contains("What I built"), "{}", r.built);
    }

    #[test]
    fn the_same_report_in_other_words_at_another_heading_level_parses_too() {
        let r = parse(OTHER_WORDING).expect("two of the four, in other words");
        assert!(r.built.contains("folds the ring"), "{}", r.built);
        assert!(r.tested.contains("24 passed"), "{}", r.tested);
        assert!(r.could_not.contains("untested here"), "{}", r.could_not);
        assert!(r.contracts.contains("None."), "{}", r.contracts);
        // From the commits section, lowercased, and deduplicated: two spellings of one
        // sha are one commit.
        assert_eq!(
            r.commits,
            vec!["7a1c0de".to_string(), "4621097e".to_string()]
        );
        assert_eq!(
            r.to_json()["commits"],
            json!(["7a1c0de", "4621097e"]),
            "the field a reviewer reads"
        );
    }

    #[test]
    fn a_message_with_no_report_parses_to_nothing() {
        assert_eq!(parse("I built the thing. All the tests pass."), None);
        assert_eq!(parse(""), None);
        // **One heading is not a report** — an ordinary answer about a test run has one.
        assert_eq!(
            parse("## Test results\n\nEvery test passed, nothing to report."),
            None
        );
        // And the report's own field is absent from a status answer rather than empty.
        let none: Option<Report> = parse("no headings here");
        assert!(none.is_none());
    }

    /// The order rule the module header states, as a test: the failure section wins over
    /// the word `test` inside it.
    #[test]
    fn a_could_not_section_is_not_read_as_the_tests() {
        let r =
            parse("## What I could not test\n\nThe VM path.\n\n## What I built\n\nThe parser.\n")
                .expect("two sections");
        assert!(r.built.contains("parser"), "{:?}", r);
        assert!(r.could_not.contains("VM path"), "{:?}", r);
        assert_eq!(r.tested, "", "nothing was tested, and the report says so");
    }

    /// A sha has to be a whole token with a digit in it; hex-only English is not a commit.
    #[test]
    fn a_word_that_is_all_hex_letters_is_not_a_commit() {
        assert_eq!(short_shas("deadbeef facade acceded"), Vec::<String>::new());
        assert_eq!(
            short_shas("see 4621097e, and (7a1c0de)."),
            vec!["4621097e".to_string(), "7a1c0de".to_string()]
        );
        // Too short to be a git abbreviation is not a sha; long enough and with a digit
        // is one, whatever the letters are.
        assert_eq!(short_shas("abcdef zz acceded"), Vec::<String>::new());
        assert_eq!(short_shas("abcdef12"), vec!["abcdef12".to_string()]);
    }
}
