//! **A heredoc through `bash` is a write** — the one shell shape where that is
//! decidable, and the line a near miss earns instead.
//!
//! # Why
//!
//! Measured in the operator's session: a model wrote a whole file with
//! `cd ~/Projects/x && cat > crates/workloads/src/banking.rs <<'RUST' … RUST`
//! instead of calling `write`. The file it produced was right, and four things
//! about how it was made were wrong:
//!
//! - the transcript had no **diff row**, so the operator could see the change
//!   only by running `git diff` in a shell of their own;
//! - the gate judged an **exec** and not a write to a path, so the rules that are
//!   about a path — §11.4's never-write list, a path resolving outside the
//!   workspace — were never asked about the file;
//! - nothing recorded it as an **edit**: the file ledger, the intent check and
//!   the files-changed accounting all had no record of a change the model had
//!   made;
//! - and the row said `bash`, which is what the model *called*, not what it did.
//!
//! # What is recognised, and what that costs
//!
//! Exactly one shape, and no near form of it:
//!
//! ```text
//! [cd <dir> &&] cat  > <path> <<[']TAG['] … TAG      (a file's whole contents)
//! [cd <dir> &&] cat >> <path> <<[']TAG['] … TAG      (appended to them)
//! [cd <dir> &&] tee  <path> <<[']TAG['] … TAG        (the same, through tee)
//! [cd <dir> &&] tee -a <path> <<[']TAG['] … TAG
//! ```
//!
//! Anything else is not this shape and is left alone: a pipeline, a second
//! redirection, a command after it, a heredoc feeding something other than the
//! program that writes, an operand on `cat` (whose content this cannot know), a
//! `<<-` whose body the shell rewrites, and any command containing a construct
//! the shell grammar could not resolve.
//!
//! **The conservatism is the design and it is not free.** A shape this refuses
//! runs as it always did — nothing is lost but the record. A shape this accepted
//! wrongly would write bytes the shell would not have, which is the one failure
//! mode worth paying to avoid, so every rule below refuses when it cannot tell.
//!
//! # It is a pure function, and the two facts it does not have
//!
//! Where the session sits and what the file already holds are the caller's, not
//! the command text's: [`recognise`] is a function of the string alone, and
//! [`as_a_write`] takes those two as arguments. That is what makes the whole
//! thing testable without a backend — and it is why the append case can be
//! *refused* rather than guessed when the file on disk is not text.

use letibot_code::shell::{self, Context, RedirectOp, RedirectTarget, Stage, Word};

/// What a recognised heredoc write amounts to, before anybody knows where the
/// session sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeredocWrite {
    /// The directory a leading `cd <dir> &&` moved to, when the command had one.
    pub cd: Option<String>,
    /// The file the command writes, exactly as the command spelled it.
    pub path: String,
    /// The bytes the heredoc hands the program: the body between the delimiter
    /// line and its terminator, with its own final newline.
    pub body: String,
    /// `cat >>` or `tee -a` — the file's existing contents have to be kept.
    pub append: bool,
}

/// The `write` call a recognised heredoc stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileWrite {
    /// The path, resolved against the directory the command runs in. Still
    /// relative when the command was: the tool resolves it against the session.
    pub path: String,
    /// The file's whole new contents.
    pub content: String,
}

/// **What is at the path an append would extend.** Three facts, not two: "there
/// is nothing there" and "there is something this cannot read" are answers to
/// different questions, and only the first of them means the body alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Existing {
    /// The file is there, and these are its bytes.
    Bytes(Vec<u8>),
    /// There is no file at that path, so an append creates one.
    Absent,
    /// The read failed some other way — outside the session's view, a directory, an
    /// I/O error. Nothing is assumed, so nothing is appended: the shell keeps this
    /// one.
    Unknown,
}

/// **Read a command as the one file write it might be**, or say that it is not
/// one.
///
/// Never panics and never guesses: every arm that cannot *know* the bytes returns
/// `None`, and the command runs through the shell exactly as it did before this
/// module existed.
pub fn recognise(command: &str) -> Option<HeredocWrite> {
    let normalised = shell::normalise(command);
    // **A command with anything unresolved is not this shape.** That covers
    // `$HOME` anywhere in the command, and — the case that matters — a heredoc
    // whose delimiter is unquoted and whose body the shell would therefore expand,
    // which the grammar reports as `Construct::ExpandedHeredoc`.
    if !normalised.is_resolved() {
        return None;
    }

    let (cd, stage) = match normalised.stages.as_slice() {
        // One stage, at the top level. A `(cat > f <<'E' …)` in a subshell is the
        // same bytes, but the subshell is a fact this does not model.
        [only] if only.context.is_empty() => (None, only),
        // `cd <dir> && <the write>`, and nothing else in between.
        [dir, only] if only.context.as_slice() == [Context::AndThen] => {
            (Some(cd_directory(dir)?), only)
        }
        _ => return None,
    };
    if stage.pipe_in || stage.pipe_out {
        return None;
    }

    // The program has to be one that copies its stdin to a file and does nothing
    // else. `sudo tee` is `sudo`, `cat | tee` has a pipe, and neither is read here.
    let (path, append) = match stage.program_name()? {
        // `cat` writes what it reads, so an operand is somebody else's content
        // going to a file this cannot see the end of.
        "cat" if stage.argv.is_empty() => file_redirect(stage)?,
        // `tee` names its file as an operand and copies stdin to it.
        "tee" => {
            let (append, path) = tee_operand(&stage.argv)?;
            // A `> f` as well would be a second file, and a second answer.
            if has_a_writing_redirect(stage) {
                return None;
            }
            (path, append)
        }
        _ => return None,
    };

    let body = heredoc_body(command, stage)?;
    Some(HeredocWrite {
        cd,
        path,
        body,
        append,
    })
}

/// The `write` call this heredoc stands for, or `None` when the bytes it would
/// leave are not certainly this command's.
///
/// `cwd` is where the session sits — `bash`'s own `cwd`, which is the workspace
/// unless the call named another. `existing` is asked only for an append, and only
/// about the path this resolves to.
pub fn as_a_write(
    recognised: &HeredocWrite,
    cwd: &str,
    existing: impl FnOnce(&str) -> Existing,
) -> Option<FileWrite> {
    // `cd <dir> &&` is relative to where the shell already was, exactly as the shell
    // would have it: the base is resolved in one hop, the file in a second.
    let base = match &recognised.cd {
        Some(dir) => join(cwd, dir),
        None => cwd.to_string(),
    };
    let path = join(&base, &recognised.path);
    let content = if recognised.append {
        match existing(&path) {
            // An append to a file that is not there creates it, which is what the
            // shell would have done.
            Existing::Absent => recognised.body.clone(),
            Existing::Bytes(bytes) => {
                // `write` replaces a file's whole contents and will not do that
                // over bytes it cannot show, so an append to a file that is not
                // text is not this: the shell keeps it.
                let mut whole = String::from_utf8(bytes).ok()?;
                whole.push_str(&recognised.body);
                whole
            }
            Existing::Unknown => return None,
        }
    } else {
        recognised.body.clone()
    };
    Some(FileWrite { path, content })
}

/// **The one line a command that writes a file through a heredoc gets when it is
/// not converted** — the near miss, which is the far more common case.
///
/// A question about the *shape* and not about what the runtime did with it: a
/// recognised command answers `Some` here too, because it does write a file
/// through a heredoc. The runtime asks this only on the arm that did not convert,
/// so one call never carries both the note and the nudge.
///
/// Conservative on purpose: a heredoc **and** a redirect into a file, both
/// present in one command. `cargo build 2>&1 | tee out.log` writes a file and is
/// not this — it is a program doing its job, not file contents written by hand.
pub fn nudge(command: &str) -> Option<&'static str> {
    let normalised = shell::normalise(command);
    let mut heredoc = false;
    let mut to_a_file = false;
    for stage in &normalised.stages {
        for r in &stage.redirects {
            match (&r.op, &r.target) {
                (RedirectOp::HereDoc, _) => heredoc = true,
                (RedirectOp::Write | RedirectOp::Append, RedirectTarget::File(_)) => {
                    to_a_file = true
                }
                _ => {}
            }
        }
    }
    (heredoc && to_a_file).then_some(
        "this command writes a file's contents through a heredoc, which leaves no diff, \
         no path for the gate to judge and no record of the edit — call `write` with the \
         contents (or `edit` for part of a file) so the change is adjudicated, recorded \
         and drawn.",
    )
}

/// The directory `cd <dir>` names, when that stage is nothing but that.
fn cd_directory(stage: &Stage) -> Option<String> {
    if stage.program_name() != Some("cd") || !stage.redirects.is_empty() {
        return None;
    }
    // `cd` with no operand goes home and `cd a b` is `$CDPATH` arithmetic; one
    // literal directory is the only form this reads.
    match stage.argv.as_slice() {
        [Word::Literal(dir)] => Some(dir.clone()),
        _ => None,
    }
}

/// The `> <path>` / `>> <path>` a `cat` writes through.
fn file_redirect(stage: &Stage) -> Option<(String, bool)> {
    let mut found: Option<(String, bool)> = None;
    for r in &stage.redirects {
        match (&r.op, &r.target) {
            (RedirectOp::Write | RedirectOp::Append, RedirectTarget::File(w)) => {
                // `> f` is stdout unless it says otherwise, and one of them is
                // the whole of what this reads.
                if !matches!(r.fd, None | Some(1)) || found.is_some() {
                    return None;
                }
                found = Some((w.literal()?.to_string(), r.op == RedirectOp::Append));
            }
            // A heredoc is the body, not a place bytes go: `heredoc_body` reads it
            // and refuses a second one. Skipped here rather than validated twice.
            (RedirectOp::HereDoc, _) => {}
            // `2>&1` moves stderr and nothing else: the bytes that reach the file
            // are the heredoc's either way, and it is the one extra redirection
            // people actually write beside this shape.
            (RedirectOp::Duplicate, RedirectTarget::Descriptor(1)) if r.fd == Some(2) => {}
            // Everything else moves bytes: `2> err` is a second file, `< in` is
            // another read, `> …` on any other fd is none of this, and `>&2` can
            // take stdout off the file entirely.
            _ => return None,
        }
    }
    found
}

/// Whether the command has any redirection into a file, ignoring the heredoc.
fn has_a_writing_redirect(stage: &Stage) -> bool {
    stage
        .redirects
        .iter()
        .any(|r| r.op.writes() && matches!(r.target, RedirectTarget::File(_)))
}

/// `tee`'s one file, and whether it was asked to append.
fn tee_operand(argv: &[Word]) -> Option<(bool, String)> {
    match argv {
        [only] => Some((false, only.literal()?.to_string())),
        [flag, only] if flag.literal() == Some("-a") => Some((true, only.literal()?.to_string())),
        // Two files is a copy to both, and a `-i` or `--output-error` is a tee
        // this does not model.
        _ => None,
    }
}

/// The heredoc's body, when the shell would hand its bytes over unchanged.
fn heredoc_body(command: &str, stage: &Stage) -> Option<String> {
    let mut found: Option<(String, bool)> = None;
    for r in &stage.redirects {
        let (RedirectOp::HereDoc, RedirectTarget::HereDoc { literal, body }) = (&r.op, &r.target)
        else {
            continue;
        };
        // stdin is the heredoc unless it says otherwise, and one heredoc is the
        // whole of what this reads.
        if !matches!(r.fd, None | Some(0)) || found.is_some() {
            return None;
        }
        // **`<<-` strips leading tabs from the body, and the body this grammar
        // hands back still carries the tab that ended it** — measured:
        // `cat > f <<-'EOF'\n\ta\n\tEOF` comes back as `"a\n\t"`. So the bytes are
        // not the bytes, and a body that is off by a tab is a file that was
        // rewritten rather than written.
        if command
            .get(r.span.start..r.span.end)
            .is_some_and(|text| text.starts_with("<<-"))
        {
            return None;
        }
        found = Some((body.clone(), *literal));
    }
    let (body, literal) = found?;
    if !literal {
        // **An unquoted delimiter means the shell expands the body before the
        // program reads it**: `$x` becomes its value, backticks and `$(…)` their
        // output, and a backslash is an escape or a line continuation. Any of the
        // three and the bytes `cat` reads are not the bytes here — which is why a
        // body with none of them is still recognised, and one with any is not.
        if body.contains(['$', '`', '\\']) {
            return None;
        }
    }
    Some(body)
}

/// A path joined to the directory it is relative to, lexically.
///
/// Not `Path::join`: the two halves are the *command's* spelling of a place, and
/// nothing here may touch the disk or collapse a `~` the backend expands itself.
fn join(base: &str, path: &str) -> String {
    // Absolute, or a `~` the backend expands: the base has no say in either.
    if path.starts_with('/') || path == "~" || path.starts_with("~/") {
        return path.to_string();
    }
    let path = path.trim_start_matches("./");
    let trimmed = base.trim_end_matches('/');
    if trimmed.is_empty() {
        // `cd / &&` — the base is the filesystem root, not the session's directory.
        return if base.starts_with('/') {
            format!("/{path}")
        } else {
            path.to_string()
        };
    }
    if trimmed == "." {
        return path.to_string();
    }
    format!("{trimmed}/{path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The operator's own command, from the session this exists for.
    const THE_OPERATORS_EXAMPLE: &str = "cd ~/Projects/x && cat > crates/workloads/src/banking.rs \
                                         <<'RUST'\npub fn balance() -> i64 { 0 }\nRUST";

    fn nothing(_: &str) -> Existing {
        Existing::Absent
    }

    #[test]
    fn the_operators_own_example_is_recognised() {
        let w = recognise(THE_OPERATORS_EXAMPLE).expect("recognised");
        assert_eq!(w.cd.as_deref(), Some("~/Projects/x"));
        assert_eq!(w.path, "crates/workloads/src/banking.rs");
        assert_eq!(w.body, "pub fn balance() -> i64 { 0 }\n");
        assert!(!w.append);
    }

    #[test]
    fn a_body_is_the_bytes_between_the_delimiter_and_its_terminator() {
        // Three shapes of the same text, and all three land on it exactly: the
        // same delimiter quoted with `'` and with `"`, and the here-document
        // operator written after the redirect rather than before it.
        for command in [
            "cat > f.rs <<'EOF'\nfn a() {}\nfn b() {}\nEOF",
            "cat > f.rs <<\"EOF\"\nfn a() {}\nfn b() {}\nEOF",
            "cat <<'EOF' > f.rs\nfn a() {}\nfn b() {}\nEOF",
            "cat>f.rs<<'EOF'\nfn a() {}\nfn b() {}\nEOF\n",
        ] {
            let w = recognise(command).unwrap_or_else(|| panic!("not recognised: {command:?}"));
            assert_eq!(w.body, "fn a() {}\nfn b() {}\n", "{command:?}");
            assert_eq!(w.path, "f.rs", "{command:?}");
        }
    }

    #[test]
    fn an_empty_body_is_an_empty_file_and_not_a_refusal() {
        let w = recognise("cat > f <<'EOF'\nEOF\n").expect("recognised");
        assert_eq!(w.body, "");
    }

    #[test]
    fn a_leading_cd_is_carried_and_not_resolved() {
        let w = recognise("cd crates/tools && cat > src/lib.rs <<'EOF'\nx\nEOF").unwrap();
        assert_eq!(w.cd.as_deref(), Some("crates/tools"));
        assert_eq!(w.path, "src/lib.rs");
    }

    #[test]
    fn append_is_read_from_the_operator_and_not_from_the_body() {
        for (command, program) in [
            ("cat >> f <<'EOF'\nx\nEOF", "cat"),
            ("tee -a f <<'EOF'\nx\nEOF", "tee"),
        ] {
            let w = recognise(command).unwrap_or_else(|| panic!("not recognised: {program}"));
            assert!(w.append, "{command:?}");
            assert_eq!(w.path, "f");
        }
        for (command, program) in [
            ("cat > f <<'EOF'\nx\nEOF", "cat"),
            ("tee f <<'EOF'\nx\nEOF", "tee"),
        ] {
            let w = recognise(command).unwrap_or_else(|| panic!("not recognised: {program}"));
            assert!(!w.append, "{command:?}");
        }
    }

    #[test]
    fn a_quoted_delimiter_makes_a_body_literal_and_an_unquoted_one_does_not_have_to() {
        // Quoted: `$` and backslashes are the file's, and are kept.
        let w = recognise("cat > f <<'EOF'\n$x \\n\nEOF").expect("recognised");
        assert_eq!(w.body, "$x \\n\n");
        // Unquoted with nothing for the shell to act on: the same bytes.
        let w = recognise("cat > f <<EOF\nplain text\nEOF").expect("recognised");
        assert_eq!(w.body, "plain text\n");
    }

    #[test]
    fn an_unquoted_delimiter_with_anything_the_shell_would_expand_is_refused() {
        for body in [
            "$HOME\n",
            "${X}\n",
            "$(date)\n",
            "`date`\n",
            "a\\tb\n",
            "one \\\ntwo\n",
        ] {
            let command = format!("cat > f <<EOF\n{body}EOF");
            assert!(
                recognise(&command).is_none(),
                "an expanded body was read as literal bytes: {command:?}"
            );
        }
    }

    #[test]
    fn the_near_shapes_are_all_left_to_the_shell() {
        for (command, why) in [
            // A second file, a second read, or a pipe on either side.
            ("cat > f 2>err <<'EOF'\nx\nEOF", "two files"),
            ("cat > f <in <<'EOF'\nx\nEOF", "a second read"),
            ("cat > f <<'EOF' | head -3\nx\nEOF", "a pipe out"),
            ("head -3 in | tee f <<'EOF'\nx\nEOF", "a pipe in"),
            // A heredoc feeding something other than the program that writes.
            ("cat <<'EOF' | bash\nx\nEOF", "the body is a program"),
            ("tee /dev/null <<'EOF' > f\nx\nEOF", "tee and a redirect"),
            // More than the write.
            ("cat > f <<'EOF'\nx\nEOF\nchmod +x f", "a command after it"),
            (
                "mkdir -p d && cat > d/f <<'EOF'\nx\nEOF",
                "a command before it",
            ),
            ("cat x > f <<'EOF'\nx\nEOF", "an operand on cat"),
            ("sudo tee f <<'EOF'\nx\nEOF", "sudo is the program"),
            ("tee f g <<'EOF'\nx\nEOF", "two files for tee"),
            // The body is not the body.
            ("cat > f <<-'EOF'\n\tx\n\tEOF", "a tab-stripping heredoc"),
            ("cat > f <<EOF\n$x\nEOF", "an expanded body"),
            // Not a write at all.
            ("echo hi > f", "a redirect of no heredoc"),
            ("cat <<'EOF'\nx\nEOF", "no file at all"),
            ("cat f", "a read"),
        ] {
            assert!(
                recognise(command).is_none(),
                "{why} was read as a heredoc write: {command:?}"
            );
        }
    }

    #[test]
    fn the_converted_shapes_are_recognised() {
        for command in [
            "cat > f <<'EOF'\nx\nEOF\n",
            "tee f <<'EOF'\nx\nEOF",
            "cd d && cat > f <<'EOF'\nx\nEOF",
        ] {
            assert!(recognise(command).is_some(), "{command:?}");
        }
    }

    #[test]
    fn a_subshell_a_background_run_and_a_second_write_are_not_this_shape() {
        for command in [
            "(cat > f <<'EOF'\nx\nEOF\n)",
            "cat > f <<'EOF' &\nx\nEOF",
            "cat > a <<'EOF'\nx\nEOF\ncat > b <<'EOF'\ny\nEOF",
        ] {
            assert!(recognise(command).is_none(), "{command:?}");
        }
    }

    #[test]
    fn stderr_to_stdout_beside_the_heredoc_is_still_the_same_file() {
        for command in [
            "cat > f <<'EOF' 2>&1\nx\nEOF",
            "cat > f 2>&1 <<'EOF'\nx\nEOF",
        ] {
            let w = recognise(command).unwrap_or_else(|| panic!("not recognised: {command:?}"));
            assert_eq!(w.body, "x\n");
        }
    }

    #[test]
    fn a_write_is_the_path_resolved_against_where_the_shell_would_have_been() {
        let w = recognise("cat > f <<'EOF'\nx\nEOF").unwrap();
        assert_eq!(as_a_write(&w, ".", nothing).unwrap().path, "f");
        assert_eq!(
            as_a_write(&w, "crates/tools", nothing).unwrap().path,
            "crates/tools/f"
        );

        let w = recognise("cd sub && cat > f <<'EOF'\nx\nEOF").unwrap();
        assert_eq!(as_a_write(&w, ".", nothing).unwrap().path, "sub/f");
        assert_eq!(
            as_a_write(&w, "crates/tools", nothing).unwrap().path,
            "crates/tools/sub/f"
        );

        // Absolute, tilde and a root `cd` are each already a place.
        let w = recognise("cd /tmp/x && cat > /etc/hosts <<'EOF'\nx\nEOF").unwrap();
        assert_eq!(as_a_write(&w, ".", nothing).unwrap().path, "/etc/hosts");
        let w = recognise("cd ~/x && cat > ~/notes.md <<'EOF'\nx\nEOF").unwrap();
        assert_eq!(as_a_write(&w, ".", nothing).unwrap().path, "~/notes.md");
        let w = recognise("cd / && cat > etc/hosts <<'EOF'\nx\nEOF").unwrap();
        assert_eq!(as_a_write(&w, ".", nothing).unwrap().path, "/etc/hosts");
    }

    #[test]
    fn an_overwrite_is_the_body_and_asks_nothing_about_the_file() {
        let w = recognise("cat > f <<'EOF'\nnew\nEOF").unwrap();
        let write = as_a_write(&w, ".", |_| Existing::Bytes(b"old\n".to_vec())).unwrap();
        assert_eq!(write.content, "new\n");
    }

    #[test]
    fn an_append_is_what_is_there_plus_the_body() {
        let w = recognise("cat >> f <<'EOF'\nnew\nEOF").unwrap();
        assert_eq!(
            as_a_write(&w, ".", |_| Existing::Bytes(b"old\n".to_vec()))
                .unwrap()
                .content,
            "old\nnew\n"
        );
        // A file that is not there is created with the body alone, which is what
        // the shell would have done.
        assert_eq!(
            as_a_write(&w, ".", |_| Existing::Absent).unwrap().content,
            "new\n"
        );
        // A file whose bytes are not text is left to the shell: `write` would
        // refuse the overwrite and say so, which is not what the command did.
        assert!(as_a_write(&w, ".", |_| Existing::Bytes(vec![0xff, 0xfe])).is_none());
        assert!(as_a_write(&w, ".", |_| Existing::Unknown).is_none());
    }

    #[test]
    fn the_nudge_is_for_a_heredoc_that_reaches_a_file() {
        assert!(nudge("mkdir -p d && cat > d/f <<'EOF'\nx\nEOF").is_some());
        assert!(nudge("cat > f <<EOF\n$x\nEOF").is_some());
        // Converted, but still the same shape: the runtime is what declines to say
        // both, and it can only do that if this answers about the shape.
        assert!(nudge("cat > f <<'EOF'\nx\nEOF").is_some());
        // Nothing was written through a heredoc and a redirect together.
        assert!(nudge("cat <<'EOF' | bash\nx\nEOF").is_none());
        assert!(nudge("echo hi > f").is_none());
        assert!(nudge("cargo build 2>&1 | tee out.log").is_none());
        assert!(nudge("git status").is_none());
    }

    #[test]
    fn the_nudge_is_one_line_and_names_both_tools() {
        let line = nudge("cat > f <<'EOF'\nx\nEOF").unwrap();
        assert!(!line.contains('\n'), "{line:?}");
        assert!(
            line.contains("`write`") && line.contains("`edit`"),
            "{line}"
        );
    }

    #[test]
    fn a_command_that_is_not_a_shell_command_at_all_is_not_a_write() {
        for command in ["", "   ", "cat > f <<'EOF'", "npm run build"] {
            assert!(recognise(command).is_none(), "{command:?}");
        }
    }
}
