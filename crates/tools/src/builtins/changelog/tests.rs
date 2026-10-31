//! The `host_changelog` contract: the five facts, the two refusals that are the feature, the
//! revert that applies nothing, and the append two writers can share.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::*;
use crate::backend::tempdir::TempDir;

/// A changelog in a temp directory, with a chosen writer and box name — the whole scope, since
/// the file is the subject.
struct Fixture {
    dir: PathBuf,
    /// The temp directory, held by whichever fixture made it. A sibling over the same file does
    /// not hold it, so the directory outlives exactly as long as the test does.
    _owned: Option<TempDir>,
    who: String,
}

impl Fixture {
    fn new(who: &str) -> Self {
        let d = TempDir::new();
        Fixture {
            dir: d.path().to_path_buf(),
            _owned: Some(d),
            who: who.to_string(),
        }
    }

    /// A second writer over the **same file** — the shape two sessions on one box have.
    fn sibling(&self, who: &str) -> Self {
        Fixture {
            dir: self.dir.clone(),
            _owned: None,
            who: who.to_string(),
        }
    }

    fn call(&self, args: Value, now: u64) -> Result<String, Invocation> {
        dispatch(self, &args, now)
    }

    fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(self.path())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl ChangelogScope for Fixture {
    fn path(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    fn host(&self) -> String {
        "fixture-box".to_string()
    }

    fn who(&self) -> String {
        self.who.clone()
    }
}

/// A call that was meant to be refused, as the sentence the model reads.
fn refusal(r: Result<String, Invocation>) -> String {
    match r {
        Ok(said) => panic!("this call was meant to be refused, and it answered: {said}"),
        Err(f) => match &f.outcome {
            letibot_transcript::ToolOutcome::Failed { reason } => {
                format!("{reason} — {}", f.payload)
            }
            other => format!("{other:?} — {}", f.payload),
        },
    }
}

fn record_args(what: &str, before: &str, after: &str, why: &str, words: &str) -> Value {
    json!({
        "verb": "record",
        "what": what,
        "before": before,
        "after": after,
        "why": why,
        "words": words,
    })
}

const NOW: u64 = 1_700_000_000_000;

// ---------------------------------------------------------------------------
// record
// ---------------------------------------------------------------------------

/// **A record round-trips: five fields into the file, five fields and an age back out.**
#[test]
fn a_record_round_trips_with_all_five_fields_and_an_age() {
    let f = Fixture::new("s-1");
    let said = f
        .call(
            record_args(
                "cpu governor",
                "powersave",
                "performance",
                "the box slept through the test window",
                "make it fast, i don't care about the fans",
            ),
            NOW,
        )
        .expect("the record lands");
    assert!(said.contains("#1"), "the reply names the number: {said}");

    // The file is one JSON object per line, and it parses back to the change itself — not to
    // something that merely looks like it.
    let lines = f.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    let Line::Change(change) = serde_json::from_str(&lines[0]).expect("the line is a change")
    else {
        panic!("the line is a change, not a revert: {}", lines[0]);
    };
    assert_eq!(change.at_ms, NOW);
    assert_eq!(change.host, "fixture-box");
    assert_eq!(change.what, "cpu governor");
    assert_eq!(change.before, "powersave");
    assert_eq!(change.after, "performance");
    assert_eq!(change.why, "the box slept through the test window");
    assert_eq!(change.words, "make it fast, i don't care about the fans");
    assert_eq!(change.who, "s-1");

    // And `list`, three days later, shows every one of them with the age.
    let listed = f
        .call(json!({"verb": "list"}), NOW + 3 * 86_400_000)
        .expect("the list answers");
    for fact in [
        "#1",
        "3 d ago",
        "fixture-box",
        "cpu governor",
        "`powersave`",
        "`performance`",
        "the box slept through the test window",
        "\"make it fast, i don't care about the fans\"",
        "s-1",
    ] {
        assert!(listed.contains(fact), "the list must show {fact:?}: {listed}");
    }
}

/// **A record without `why` is refused by name, and nothing is written.** This is the feature: a
/// record of *what* with no reason is the complaint, written down.
#[test]
fn a_record_without_why_is_refused_by_name() {
    let f = Fixture::new("s-1");
    let mut args = record_args("cpu governor", "powersave", "performance", "", "do it");
    args.as_object_mut().unwrap().remove("why");
    let said = refusal(f.call(args, NOW));
    assert!(said.contains("`record` needs `why`"), "{said}");
    assert!(said.contains("not an answer a person can act on"), "{said}");
    assert!(!f.path().exists(), "nothing was written: {}", f.path().display());
}

/// **And so is one without `words`** — whose words a change rests on is the half that makes it
/// answerable.
#[test]
fn a_record_without_words_is_refused_by_name() {
    let f = Fixture::new("s-1");
    let said = refusal(f.call(
        json!({
            "verb": "record",
            "what": "cpu governor",
            "before": "powersave",
            "after": "performance",
            "why": "the box slept through the test window",
        }),
        NOW,
    ));
    assert!(said.contains("`record` needs `words`"), "{said}");
    assert!(!f.path().exists(), "nothing was written");
}

/// Both missing is one refusal naming both, so a model corrects itself in the same call.
#[test]
fn a_record_missing_two_fields_names_both() {
    let f = Fixture::new("s-1");
    let said = refusal(f.call(
        json!({"verb": "record", "what": "vm.swappiness", "after": "10"}),
        NOW,
    ));
    for field in ["`why`", "`words`", "`before`"] {
        assert!(said.contains(field), "the refusal names {field}: {said}");
    }
}

// ---------------------------------------------------------------------------
// revert
// ---------------------------------------------------------------------------

/// **A revert hands back the recorded `before`, marks the line, and applies nothing.**
#[test]
fn a_revert_returns_the_before_and_marks_the_line() {
    let f = Fixture::new("s-1");
    f.call(
        record_args(
            "cpu governor",
            "powersave",
            "performance",
            "the box slept through the test window",
            "make it fast",
        ),
        NOW,
    )
    .expect("the record lands");
    let before = f.lines();

    let said = f
        .call(
            json!({"verb": "revert", "change": 1, "words": "put it back, the fans are loud"}),
            NOW + 60_000,
        )
        .expect("the revert lands");
    assert!(said.contains("`powersave`"), "it hands back the before: {said}");
    assert!(said.contains("marked reverted"), "{said}");
    assert!(
        said.contains("NOTHING has been applied"),
        "a revert is a record, not an action: {said}"
    );

    // The change's own line is untouched — a record is evidence, and the mark is a second line.
    let after = f.lines();
    assert_eq!(after.len(), 2, "{after:?}");
    assert_eq!(after[0], before[0], "the change's line was not rewritten");
    let Line::Revert(revert) = serde_json::from_str(&after[1]).expect("a revert line") else {
        panic!("the second line is a revert: {}", after[1]);
    };
    assert_eq!(revert.change, 1);
    assert_eq!(revert.what, "cpu governor");
    assert_eq!(revert.who, "s-1");
    assert_eq!(revert.words, "put it back, the fans are loud");

    // And the fold says so.
    let log = read(&f.path());
    assert!(log.unreadable.is_empty(), "{:?}", log.unreadable);
    assert_eq!(log.entries.len(), 1);
    assert_eq!(log.entries[0].reverted.as_ref().map(|r| r.change), Some(1));
}

/// **A second revert of the same change is refused**, naming the first one.
#[test]
fn a_second_revert_of_the_same_change_is_refused() {
    let f = Fixture::new("s-1");
    f.call(
        record_args("cpu governor", "powersave", "performance", "why", "words"),
        NOW,
    )
    .expect("the record lands");
    f.call(json!({"verb": "revert", "change": 1}), NOW + 1000)
        .expect("the first revert lands");

    let said = refusal(f.call(json!({"verb": "revert", "change": 1}), NOW + 2000));
    assert!(said.contains("already reverted"), "{said}");
    assert!(said.contains("cpu governor"), "{said}");
    assert_eq!(f.lines().len(), 2, "nothing was appended: {:?}", f.lines());
}

/// **A reverted change still lists** — with the revert's age and who asked for it.
#[test]
fn a_reverted_change_still_lists() {
    let f = Fixture::new("s-1");
    f.call(
        record_args(
            "vm.swappiness",
            "60",
            "10",
            "the box thrashed under a build",
            "stop swapping the toolchain out",
        ),
        NOW,
    )
    .expect("the record lands");
    f.call(
        json!({"verb": "revert", "change": 1, "words": "it made the box sluggish"}),
        NOW + 3_600_000,
    )
    .expect("the revert lands");

    // Two hours after the revert, so the two ages are different words.
    let listed = f
        .call(json!({"verb": "list"}), NOW + 3 * 3_600_000)
        .expect("the list answers");
    assert!(listed.contains("#1"), "{listed}");
    assert!(listed.contains("vm.swappiness"), "it is still listed: {listed}");
    assert!(listed.contains("REVERTED"), "and marked reverted: {listed}");
    assert!(
        listed.contains("3 h ago") && listed.contains("REVERTED 2 h ago"),
        "the change's age and the revert's are both there: {listed}"
    );
    assert!(
        listed.contains("\"it made the box sluggish\""),
        "the words the revert was asked on: {listed}"
    );
    assert!(listed.contains("1 change(s)"), "the count is the file's: {listed}");
}

/// A revert of a number the file does not have is refused with the numbers it does have — the
/// miss a model can correct from.
#[test]
fn a_revert_of_a_change_the_file_does_not_have_lists_the_ones_it_does() {
    let f = Fixture::new("s-1");
    f.call(
        record_args("cpu governor", "powersave", "performance", "why", "words"),
        NOW,
    )
    .expect("the record lands");
    let said = refusal(f.call(json!({"verb": "revert", "change": 7}), NOW));
    assert!(said.contains("there is no change #7"), "{said}");
    assert!(said.contains("#1 `cpu governor`"), "it names what there is: {said}");
    assert_eq!(f.lines().len(), 1, "nothing was appended");
}

/// **A revert that names a different setting is not applied to the number it lands on.** The
/// number is a position, and a file somebody truncated would otherwise make an old revert undo
/// whatever now sits there.
#[test]
fn a_revert_naming_another_setting_is_reported_and_not_applied() {
    let f = Fixture::new("s-1");
    let first = json!({
        "kind": "change", "at_ms": NOW, "host": "fixture-box", "what": "cpu governor",
        "before": "powersave", "after": "performance", "why": "why", "words": "words", "who": "s-1"
    });
    let stray = json!({
        "kind": "revert", "at_ms": NOW + 1000, "change": 1, "what": "vm.swappiness", "who": "s-1"
    });
    std::fs::write(f.path(), format!("{first}\n{stray}\n")).expect("fixture file");

    let log = read(&f.path());
    assert_eq!(log.entries.len(), 1);
    assert!(
        log.entries[0].reverted.is_none(),
        "the revert names another setting and must not be applied"
    );
    assert_eq!(log.unreadable.len(), 1, "{:?}", log.unreadable);
    assert!(
        log.unreadable[0].1.contains("vm.swappiness")
            && log.unreadable[0].1.contains("cpu governor"),
        "{:?}",
        log.unreadable
    );
    let listed = f.call(json!({"verb": "list"}), NOW).expect("the list answers");
    assert!(listed.contains("could not be read"), "{listed}");
}

// ---------------------------------------------------------------------------
// the file
// ---------------------------------------------------------------------------

/// **Two writers appending at once: every line survives, and the order is the file's.**
///
/// The number is a position, so the test can assert the two facts an append-only file is for:
/// nothing was lost, and nothing was written over. Ten records each, from two threads that start
/// on a barrier, because a single pair can miss the interleaving by luck.
#[test]
fn two_writers_appending_at_once_keep_both_lines() {
    const EACH: usize = 10;
    let owner = Arc::new(Fixture::new("s-a"));
    let other = Arc::new(owner.sibling("s-b"));
    let gate = Arc::new(std::sync::Barrier::new(2));

    let threads: Vec<_> = [owner.clone(), other.clone()]
        .into_iter()
        .map(|f| {
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                for i in 0..EACH {
                    let who = f.who.clone();
                    f.call(
                        json!({
                            "verb": "record",
                            "what": format!("setting-{who}-{i}"),
                            "before": format!("before-{i}"),
                            "after": format!("after-{i}"),
                            "why": "a concurrent writer test",
                            "words": "both of these have to survive",
                        }),
                        NOW + i as u64,
                    )
                    .expect("the record lands");
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("no thread panicked");
    }

    // Every line is intact — a half-written line is what an append-only file must not produce.
    let lines = owner.lines();
    assert_eq!(lines.len(), 2 * EACH, "both writers' lines are there");
    let log = read(&owner.path());
    assert!(log.unreadable.is_empty(), "{:?}", log.unreadable);
    assert_eq!(log.entries.len(), 2 * EACH);

    // The numbers are the file's positions, 1..=N, and the fold's order is the file's order.
    let mut seen: Vec<String> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Line::Change(change) = serde_json::from_str(line).expect("every line parses") else {
            panic!("no revert was written: {line}");
        };
        assert_eq!(
            log.entries[i].n,
            i + 1,
            "the number is the line's position in the file"
        );
        assert_eq!(log.entries[i].change, change, "the fold reads the file's order");
        seen.push(change.what);
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 2 * EACH, "no record was lost: {seen:?}");
}

/// `list` on a file nobody has written says so, names the path, and names the verb that would
/// start it — a miss is more output than a hit, not less.
#[test]
fn an_empty_changelog_says_so_and_names_the_path() {
    let f = Fixture::new("s-1");
    let said = f.call(json!({"verb": "list"}), NOW).expect("the list answers");
    assert!(said.contains("no host config change has been recorded"), "{said}");
    assert!(said.contains(&f.path().display().to_string()), "{said}");
    assert!(said.contains("`record`"), "{said}");
}

/// The schema is the seat's contract: a name, a clean description, and `list` narrowing to a read
/// so a read-only backend is not asked a question about a fact.
#[test]
fn the_schema_is_clean_and_list_narrows_to_a_read() {
    let tool = HostChangelogTool::nowhere();
    let s = tool.schema();
    assert_eq!(s.name, TOOL);
    assert_eq!(crate::schema::lint_description(&s.description), vec![]);
    assert!(s.description.len() < 900, "{} bytes", s.description.len());
    assert_eq!(s.access, Access::Write);
    assert_eq!(tool.access_for(&json!({"verb": "list"})), Some(Access::Read));
    assert_eq!(tool.access_for(&json!({"verb": "record"})), None);
    assert_eq!(tool.access_for(&json!({"verb": "revert"})), None);
}

/// An unknown verb is refused with the three there are.
#[test]
fn an_unknown_verb_names_the_verbs_there_are() {
    let f = Fixture::new("s-1");
    let said = refusal(f.call(json!({"verb": "delete"}), NOW));
    assert!(said.contains("unknown `host_changelog` verb `delete`"), "{said}");
    assert!(said.contains("`record`, `list` and `revert`"), "{said}");
}

// ---------------------------------------------------------------------------
// through the runtime: the second gate, and a file nobody had to trust the tool about
// ---------------------------------------------------------------------------

/// **A record reaches the disk through the whole runtime** — the gate, the tool, the scope — and
/// the assertion reads the file itself rather than asking the tool what it did.
#[test]
fn a_record_reaches_the_file_through_the_runtime() {
    let mut h = crate::testing::writable_harness();
    let r = h.call(
        "host_changelog",
        r#"{"verb":"record","what":"cpu governor","before":"powersave","after":"performance","why":"the box slept through the test window","words":"make it fast"}"#,
    );
    assert!(r.is_grounded(), "{}", r.render());
    assert!(r.render().contains("#1"), "{}", r.render());

    let on_disk = std::fs::read_to_string(h.changelog_file()).expect("the file the tool wrote");
    assert_eq!(on_disk.lines().count(), 1, "{on_disk}");
    assert!(on_disk.contains("cpu governor"), "{on_disk}");
    assert!(on_disk.contains("the box slept through the test window"), "{on_disk}");
}

/// **A read-only session cannot record, and can still read.** The write verbs are refused by the
/// backend — the second, independent gate — and `list`, which narrows to `Read`, answers. That is
/// what makes seating the tool on the read-only default seat worth its entry.
#[test]
fn a_read_only_backend_refuses_to_record_and_still_lists() {
    let mut h = crate::testing::read_only_harness_with_gate(Some(crate::testing::allow_all()));
    let r = h.call(
        "host_changelog",
        r#"{"verb":"record","what":"cpu governor","before":"powersave","after":"performance","why":"w","words":"x"}"#,
    );
    assert!(!r.is_grounded(), "{}", r.render());
    assert!(r.render().contains("read-only"), "{}", r.render());
    assert!(
        r.render().contains("Nothing on disk changed"),
        "{}",
        r.render()
    );
    assert!(!h.changelog_file().exists(), "nothing was written");

    let r = h.call("host_changelog", r#"{"verb":"list"}"#);
    assert!(r.is_grounded(), "the read verb is not refused: {}", r.render());
    assert!(r.render().contains("no host config change"), "{}", r.render());
}
