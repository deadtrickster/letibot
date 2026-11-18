//! The desk over a directory of its own: the reader that judges a proposal, the diff, the atomic
//! write, and the redaction. Nothing here reads the operator's real config dir.

use super::*;

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "letibot-config-desk-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a fixture directory");
    dir
}

fn edits(set: &[(&str, &str)], remove: &[&str]) -> cref::edit::Edits {
    cref::edit::Edits {
        set: set
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        remove: remove.iter().map(|s| s.to_string()).collect(),
    }
}

const PROVIDERS: &str = "\
# mine, hand-edited
[default]
provider = \"glm\"   # the one that works

[glm]
key = \"sk-do-not-show-me\"

[shadow]
answer_reads = false
";

/// A proposal is applied over the file as it is, so the operator's comments and order survive; the
/// result carries a unified diff and the reader's sentence.
#[test]
fn a_proposal_keeps_the_file_and_renders_a_diff() {
    let dir = temp("diff");
    std::fs::write(dir.join("providers.toml"), PROVIDERS).unwrap();
    let desk = SessionConfig::at(&dir);
    let p = desk
        .propose("providers.toml", &edits(&[("shadow.answer_reads", "true")], &[]))
        .expect("a proposal");
    assert!(p.text.starts_with("# mine, hand-edited\n[default]\n"), "{}", p.text);
    assert!(p.text.contains("# the one that works"), "{}", p.text);
    assert!(p.text.contains("answer_reads = true"), "{}", p.text);
    assert!(p.diff.starts_with("--- a/providers.toml\n+++ b/providers.toml\n"), "{}", p.diff);
    assert!(p.diff.contains("-answer_reads = false"), "{}", p.diff);
    assert!(p.diff.contains("+answer_reads = true"), "{}", p.diff);
    assert!(p.check.contains("providers.toml reader"), "{}", p.check);
    // The card's two panels are the file's own excerpt, so a head draws an edit card.
    assert_eq!(p.preview.len(), 1);
    assert!(p.preview[0].after.contains("answer_reads = true"));
    // And the file itself has not changed: a proposal writes nothing.
    assert_eq!(std::fs::read_to_string(dir.join("providers.toml")).unwrap(), PROVIDERS);
}

/// A result the file's own reader refuses is refused here, in the daemon's words, before any card.
#[test]
fn a_result_the_reader_refuses_is_refused_with_its_words() {
    let dir = temp("refuse");
    std::fs::write(dir.join("leticode.toml"), "[exec]\nforeground_seconds = 60\n").unwrap();
    let desk = SessionConfig::at(&dir);
    // `foreground_seconds` must be an integer, and a string is what the reader refuses by naming
    // the key and its type — the daemon's own sentence, not a second one written here.
    let e = desk
        .propose("leticode.toml", &edits(&[("exec.foreground_seconds", "\"sixty\"")], &[]))
        .expect_err("the reader refuses a string here");
    assert!(e.contains("leticode.toml"), "{e}");
    assert!(e.contains("foreground_seconds"), "the reader's words: {e}");
    // A key the file's parser does not know is refused too, and it names the key.
    let e = desk
        .propose("leticode.toml", &edits(&[("exec.no_such_key", "1")], &[]))
        .expect_err("the parser refuses an unknown key");
    assert!(e.contains("no_such_key"), "{e}");
}

/// The write is atomic and keeps the mode: a file at 0600 stays at 0600, and the temp file is gone.
#[test]
fn a_write_keeps_the_mode_and_leaves_no_temp_file() {
    let dir = temp("write");
    let path = dir.join("providers.toml");
    std::fs::write(&path, PROVIDERS).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let desk = SessionConfig::at(&dir);
    let p = desk
        .propose("providers.toml", &edits(&[("shadow.answer_reads", "true")], &[]))
        .unwrap();
    let w = desk.write("providers.toml", &p.before, &p.text).unwrap();
    assert_eq!(w.mode, Some(0o600));
    assert_eq!(w.bytes, p.text.len());
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(!dir.join(".providers.toml.tmp").exists(), "the temp file is gone");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), p.text);
    // A file that moved under the card is not overwritten by an answer about the old bytes.
    let e = desk
        .write("providers.toml", &p.before, &p.text)
        .expect_err("the file no longer holds those bytes");
    assert!(e.contains("changed while the card was up"), "{e}");
}

/// A file that does not exist is created private, because a config file can hold a key.
#[test]
fn a_new_file_is_written_at_0600() {
    let dir = temp("new");
    let desk = SessionConfig::at(&dir);
    let p = desk
        .propose("providers.toml", &edits(&[("default.provider", "\"glm\"")], &[]))
        .expect("a new file's proposal");
    assert!(p.preview[0].created);
    let w = desk.write("providers.toml", &p.before, &p.text).unwrap();
    assert_eq!(w.mode, Some(0o600));
    let mode = std::fs::metadata(dir.join("providers.toml")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

/// `show` prints the file with every credential value replaced, and says what it took out.
#[test]
fn show_redacts_and_says_what_it_took_out() {
    let dir = temp("show");
    std::fs::write(dir.join("providers.toml"), PROVIDERS).unwrap();
    std::fs::set_permissions(
        dir.join("providers.toml"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let desk = SessionConfig::at(&dir);
    let shown = desk.show("providers.toml").unwrap();
    assert!(!shown.text.contains("sk-do-not-show-me"), "{}", shown.text);
    assert!(shown.text.contains("<redacted>"), "{}", shown.text);
    assert!(shown.text.contains("# mine, hand-edited"), "{}", shown.text);
    assert!(shown.text.contains("provider = \"glm\""), "{}", shown.text);
    assert_eq!(shown.mode, Some(0o600));
    assert!(shown.note.contains("1 credential value"), "{}", shown.note);
    // A file that is not there is said so rather than shown as empty.
    let e = desk.show("hosts.toml").expect_err("no file");
    assert!(e.contains("does not exist"), "{e}");
}

/// A palette is a name and not a file; a path is not a config file; and a JSON file is not an
/// editable TOML one. Each is refused by name, before anybody is asked anything.
#[test]
fn the_files_this_tool_does_not_open_are_refused_by_name() {
    let dir = temp("names");
    let desk = SessionConfig::at(&dir);
    let e = desk.show("theme.toml").expect_err("a palette is not a file");
    assert!(e.contains("themes/NAME.toml"), "{e}");
    let e = desk.show("keys/lab2x1").expect_err("a key is not a config file");
    assert!(e.contains("is not one of letibot's config files"), "{e}");
    let e = desk
        .propose("permission.json", &edits(&[("bash.ls", "allow")], &[]))
        .expect_err("json is not edited here");
    assert!(e.contains("TOML edits only"), "{e}");
    // And a removal of a key that is not there is not a proposal.
    std::fs::write(dir.join("providers.toml"), PROVIDERS).unwrap();
    let e = desk
        .propose("providers.toml", &edits(&[], &["shadow.not_here"]))
        .expect_err("nothing to remove");
    assert!(e.contains("nothing to remove"), "{e}");
}

/// `explain` answers from the reference alone: a dotted path, a bare word, and a file.
#[test]
fn explain_reads_the_reference_and_not_a_file() {
    let dir = temp("explain");
    let desk = SessionConfig::at(&dir);
    let said = desk.explain(None, Some("shadow.answer_reads")).unwrap();
    assert!(said.contains("providers.toml"), "{said}");
    assert!(said.contains("answer_reads"), "{said}");
    assert!(said.contains("permission job"), "{said}");
    let said = desk.explain(None, Some("answer_reads")).unwrap();
    assert!(said.contains("[shadow]"), "{said}");
    let said = desk.explain(Some("leticode.toml"), None).unwrap();
    assert!(said.contains("leticode.toml - "), "{said}");
    assert!(said.contains("main_model"), "{said}");
    let e = desk.explain(None, Some("no_such_key_anywhere")).expect_err("no such key");
    assert!(e.contains("no key of"), "{e}");
}
