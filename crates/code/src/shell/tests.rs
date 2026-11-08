use super::*;

fn n(s: &str) -> Normalised {
    normalise(s)
}

/// A `for` over a list the command spells is that many passes, not one hole: the shape from the
/// corpus is `for f in <literal> <literal>; do grep ... $f; done`, with the literals written here.
#[test]
fn a_for_over_a_literal_list_is_unrolled_into_its_passes() {
    let x = n("for f in /w/a.rs /w/b.rs; do grep -n x $f; done");
    assert!(x.is_resolved(), "{:?}", x.unresolved);
    assert_eq!(x.binaries(), vec!["grep", "grep"]);
    assert_eq!(
        x.stages[0].argv,
        vec!["-n".into(), "x".into(), "/w/a.rs".into()]
            .into_iter()
            .map(Word::Literal)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        x.stages[1].argv.last(),
        Some(&Word::Literal("/w/b.rs".into())),
        "the second pass carries the second value"
    );
    // A loop body is a loop body: nothing here is promoted to "always runs".
    assert!(
        x.stages
            .iter()
            .all(|s| s.certainty == Certainty::Conditional),
        "{:?}",
        x.stages.iter().map(|s| s.certainty).collect::<Vec<_>>()
    );
    // And the passes are in the shape, because they are what will run.
    assert_eq!(shape(&x), "grep -n <args> ; grep -n <args>");

    // A bound value is a word of the body wherever it appears - inside a
    // concatenation and inside double quotes as much as on its own.
    let x = n("for f in /w/a /w/b; do cp $f $f.bak; echo \"done $f\"; done");
    assert!(x.is_resolved(), "{:?}", x.unresolved);
    assert_eq!(
        x.stages[0].argv,
        vec!["/w/a".into(), "/w/a.bak".into()]
            .into_iter()
            .map(Word::Literal)
            .collect::<Vec<_>>()
    );
    // Four stages: two per pass, in pass order.
    assert_eq!(x.stages.len(), 4);
    assert_eq!(x.stages[3].argv[0], Word::Literal("done /w/b".into()));
}

#[test]
fn a_brace_range_at_the_integer_edge_ends_instead_of_overflowing() {
    let words = |inner: &str| match brace_range(inner) {
        Brace::Words(w) => w,
        _ => panic!("{inner} is a range"),
    };
    assert_eq!(
        words("9223372036854775800..9223372036854775807..5"),
        vec!["9223372036854775800", "9223372036854775805"]
    );
    assert_eq!(words("1..3..9223372036854775807"), vec!["1"]);
    assert_eq!(words("a..e..4294967297"), vec!["a"]);
}

/// A `for` over a brace list is the passes the shell makes of it: a brace is decided by the
/// text, so the body is walked once per word rather than left unread.
#[test]
fn a_for_over_a_brace_list_is_unrolled_into_the_words_it_makes() {
    let x = n("for i in {1..3}; do cat /w/f$i; done");
    assert!(x.is_resolved(), "{:?}", x.unresolved);
    let tails: Vec<Option<&str>> = x
        .stages
        .iter()
        .map(|s| s.argv.last().and_then(Word::literal))
        .collect();
    assert_eq!(tails, vec![Some("/w/f1"), Some("/w/f2"), Some("/w/f3")]);
    assert!(
        x.stages
            .iter()
            .all(|s| s.certainty == Certainty::Conditional),
        "a brace list does not make a loop body unconditional"
    );

    // A brace concatenated with text, and two brace words in one list.
    let x = n("for i in {a,b}.txt; do cat /w/$i; done");
    assert!(x.is_resolved(), "{:?}", x.unresolved);
    let tails: Vec<Option<&str>> = x
        .stages
        .iter()
        .map(|s| s.argv.last().and_then(Word::literal))
        .collect();
    assert_eq!(tails, vec![Some("/w/a.txt"), Some("/w/b.txt")]);
    let x = n("for i in {1..2} {8,9}; do echo $i; done");
    assert!(x.is_resolved(), "{:?}", x.unresolved);
    assert_eq!(x.stages.len(), 4);

    // Past the bound, and a brace alternative the shell would glob: unread, as before.
    assert!(!n("for i in {1..100}; do echo $i; done").is_resolved());
    assert!(!n("for i in {a,*}; do echo $i; done").is_resolved());
}

/// A list the command does not spell is not unrolled, and each of these is a real
/// `for` from the same corpus.
#[test]
fn a_for_whose_list_is_not_in_the_text_stays_one_unread_pass() {
    for cmd in [
        // The file set is not in the text.
        "for f in *.rs; do grep -n x $f; done",
        // The value is not.
        "for f in $FILES; do grep -n x $f; done",
        "for f in $(ls); do grep -n x $f; done",
        // Two words from one: a single binding cannot stand for both.
        "for f in \"a b\"; do grep -n x $f; done",
    ] {
        let x = n(cmd);
        assert!(!x.is_resolved(), "{cmd} was unrolled: {:?}", x.stages);
    }
    // The body is still read, once - unrolling is the extra, not the reading.
    let x = n("for f in *.rs; do grep -n x $f; done");
    assert_eq!(x.binaries(), vec!["grep"]);
}

/// A transform of the loop variable is not the variable - the same rule the
/// intent layer keeps for `${#X}` (the grammar reports the name
/// `f` for all of `$f`, `${f}`, `${#f}` and `${!f}`).
#[test]
fn a_transform_of_a_loop_variable_is_not_the_variable() {
    for cmd in [
        "for f in /w/a; do echo ${#f}; done",
        "for f in /w/a; do echo ${f%.*}; done",
        "for f in /w/a; do echo ${!f}; done",
        "for f in /w/a; do echo ${f[0]}; done",
    ] {
        assert!(!n(cmd).is_resolved(), "{cmd} was read as the variable");
    }
    // The braced spelling of the value itself is the value.
    assert!(n("for f in /w/a; do echo ${f}; done").is_resolved());
}

/// A binding lives for one body. `$f` after the `done` is the empty string, which
/// this module does not know and must not claim.
#[test]
fn a_loop_binding_does_not_leak_past_its_body() {
    let x = n("for f in /w/a; do echo $f; done; echo $f");
    assert!(!x.is_resolved());
    assert_eq!(x.unresolved.len(), 1);
    assert_eq!(x.unresolved[0].construct.as_str(), "parameter_expansion");
}

#[test]
fn a_plain_pipeline_resolves_to_its_stages_and_their_redirections() {
    let x = n("cargo test 2>&1 | tee out.log");
    assert!(x.is_resolved(), "{:?}", x.unresolved);
    assert_eq!(x.binaries(), vec!["cargo", "tee"]);
    assert_eq!(x.stages[0].argv, vec![Word::Literal("test".into())]);
    // `2>&1` is a descriptor dup, NOT a write to a file named `1`.
    assert_eq!(
        x.stages[0].redirects[0].target,
        RedirectTarget::Descriptor(1)
    );
    assert_eq!(x.stages[0].redirects[0].fd, Some(2));
    assert!(x.stages[0].pipe_out && !x.stages[0].pipe_in);
    assert!(x.stages[1].pipe_in && !x.stages[1].pipe_out);
    // The last stage's stdout is what becomes a tool result.
    assert!(!x.stages[0].stdout_surfaces());
    assert!(x.stages[1].stdout_surfaces());
}

#[test]
fn quote_removal_happens_before_the_decision_which_is_the_whole_point() {
    // GuardFall's class: the harness inspects `"c d.txt"` and the program
    // receives `c d.txt`. The normalised word is what `execve` gets.
    let x = n(r#"cat 'a b.txt' "plain" e\ f.txt"#);
    assert!(x.is_resolved(), "{:?}", x.unresolved);
    assert_eq!(
        x.stages[0].argv,
        vec![
            Word::Literal("a b.txt".into()),
            Word::Literal("plain".into()),
            Word::Literal("e f.txt".into()),
        ]
    );
}

#[test]
fn a_double_quoted_part_is_the_bytes_bash_makes_of_it() {
    // Measured with `set --`: inside `"…"` a backslash goes before `$`, a backquote, `"` and
    // itself, a backslash-newline is a line continuation, every other backslash stays, and a
    // `$` with nothing to expand is a dollar.
    let x = n(r#"rm -rf "/w/$"{a,b}"#);
    assert_eq!(
        x.stages[0].argv[1..].to_vec(),
        vec![Word::Literal("/w/$a".into()), Word::Literal("/w/$b".into())]
    );
    let y = n("rm -rf \"a\\$b\" \"c\\\\d\" \"e\\\"f\" \"g\\nh\" \"i\\\nj\"");
    assert_eq!(
        y.stages[0].argv[1..].to_vec(),
        vec![
            Word::Literal("a$b".into()),
            Word::Literal("c\\d".into()),
            Word::Literal("e\"f".into()),
            Word::Literal("g\\nh".into()),
            Word::Literal("ij".into()),
        ]
    );
}

#[test]
fn a_command_assembled_at_runtime_is_unresolved_at_the_program_position() {
    let x = n("X=$(cat f); $X --now");
    assert!(!x.is_resolved());
    // The inner `cat f` IS resolved and listed - this is what `predicate.rs`
    // could not do - while the value it produces is not.
    assert!(x.binaries().contains(&"cat"));
    let p = x.unresolved_programs();
    assert_eq!(p.len(), 1);
    assert!(x.unresolved.iter().any(|u| u.decides == Decides::Program
        && matches!(&u.construct, Construct::ParameterExpansion { name } if name == "X")));
    // And the assignment's value is unresolvable for a different reason.
    assert!(
        x.unresolved.iter().any(|u| u.decides == Decides::Assignment
            && u.construct == Construct::CommandSubstitutionValue)
    );
}

#[test]
fn eval_of_a_variable_is_unresolved_and_says_so_at_a_position() {
    let x = n("eval \"$CMD\"");
    assert!(!x.is_resolved());
    assert_eq!(x.binaries(), vec!["eval"]);
    let u = &x.unresolved[0];
    assert_eq!(u.decides, Decides::Argument);
    assert_eq!(u.span.line, 1);
    assert!(u.why.contains("$CMD"), "{}", u.why);
    assert!(x.unresolved_report().contains("parameter_expansion"));
}

#[test]
fn a_here_string_does_not_parse_and_the_recovered_tree_is_not_trusted() {
    // Why `partial` is fatal: tree-sitter-bash 0.25 cannot parse `<<<`, and its RECOVERY reads it
    // as `< 'here string'` - a read of a file named `here string`. A normaliser reporting that
    // data flow would be confidently wrong.
    let x = n("cat >&2 <<< 'here string'");
    assert!(x.partial);
    assert!(!x.is_resolved());
    assert_eq!(x.unresolved[0].construct, Construct::ParseError);
    assert_eq!(x.unresolved[0].decides, Decides::Structure);
    assert!(
        !x.unresolved[0].why.contains("here-document operator"),
        "a here-string is not the here-document shape: {}",
        x.unresolved[0].why
    );
}

/// `<>` is one operator this grammar has no token for: the `>` is recovered as an ERROR between
/// the descriptor and the destination, both of which land where bash puts them, so the one
/// redirect is read instead of the whole command refused.
#[test]
fn a_read_write_redirect_is_read_rather_than_refused() {
    let x = n("exec 3<>/dev/tcp/127.0.0.1/5432");
    assert!(x.is_resolved(), "{:?}", x.unresolved);
    assert_eq!(x.binaries(), vec!["exec"]);
    let r = &x.stages[0].redirects;
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].op, RedirectOp::ReadWrite);
    assert_eq!(r[0].fd, Some(3));
    assert_eq!(
        r[0].target,
        RedirectTarget::File(Word::Literal("/dev/tcp/127.0.0.1/5432".into()))
    );

    // The one command in this tree that writes the operator: `fleet`'s port probe.
    let probe = n(
        "if (exec 3<>/dev/tcp/127.0.0.1/5432) 2>/dev/null; then exec 3>&-; \
                       echo yes; else echo no; fi",
    );
    assert!(probe.is_resolved(), "{:?}", probe.unresolved);
    assert_eq!(probe.binaries(), vec!["exec", "exec", "echo", "echo"]);
}

/// Without a descriptor the recovered tree holds TWO redirects (`<` and `>file`) where the shell
/// opens one, so the redirect is not read - and the refusal names the operator and says where a
/// descriptor makes it read.
#[test]
fn a_read_write_redirect_with_no_descriptor_names_the_writing_that_reads() {
    for cmd in ["cmd <>file", "cmd 0<>file"] {
        let x = n(cmd);
        assert!(x.partial, "{cmd}");
        assert_eq!(x.unresolved[0].construct, Construct::ParseError);
        let why = &x.unresolved[0].why;
        assert!(why.contains("<>"), "{cmd}: {why}");
        assert!(why.contains("3<>file"), "{cmd}: {why}");
    }

    let written = n("cmd 3<>file");
    assert!(written.is_resolved(), "{:?}", written.unresolved);
    assert_eq!(written.stages[0].redirects[0].op, RedirectOp::ReadWrite);
    assert_eq!(written.stages[0].redirects[0].fd, Some(3));

    // And a recovery that swallowed a word behind the destination is not credited: `exec 3<>file
    // cat`'s tree has `cat` as a second destination, so the `exec` of a command is refused.
    let swallowed = n("exec 3<>file cat");
    assert!(swallowed.partial, "{:?}", swallowed.unresolved);
}

/// `%(…)` in a word is not a command bash will run either, so the refusal says what the shell
/// says and which writing both the shell and the gate read.
#[test]
fn a_percent_before_a_bracket_is_refused_as_the_text_bash_refuses() {
    for cmd in [
        "git for-each-ref --format=%(refname)",
        "echo a%(x)b",
        "a=%(x)",
    ] {
        let x = n(cmd);
        assert!(x.partial, "{cmd}");
        let why = &x.unresolved[0].why;
        assert!(why.contains("%("), "{cmd}: {why}");
        assert!(why.contains("quote"), "{cmd}: {why}");
    }
    let written = n("git for-each-ref --format='%(refname)'");
    assert!(written.is_resolved(), "{:?}", written.unresolved);
}

#[test]
fn a_quoted_heredoc_is_literal_and_an_unquoted_one_is_not() {
    let lit = n("cat <<'EOF' > /tmp/x\nbody $notexpanded\nEOF");
    assert!(lit.is_resolved(), "{:?}", lit.unresolved);
    let r = &lit.stages[0].redirects;
    assert!(matches!(
        r[0].target,
        RedirectTarget::HereDoc { literal: true, .. }
    ));
    // The `> /tmp/x` nested inside the heredoc redirect is still a write.
    assert_eq!(lit.redirect_writes(), vec![&Word::Literal("/tmp/x".into())]);

    let exp = n("cat <<EOF\n$expanded\nEOF");
    assert!(!exp.is_resolved());
    assert_eq!(exp.unresolved[0].construct, Construct::ExpandedHeredoc);

    // The shell expands a backquote in an unquoted body too, and the grammar hands that
    // back as body text rather than as a node: the bytes `cat` reads are not these.
    let back = n("cat <<EOF\n`date` x\nEOF");
    assert!(
        !back.is_resolved(),
        "a backquote in the body was read as text"
    );
    assert_eq!(back.unresolved[0].construct, Construct::ExpandedHeredoc);
    // A backslash only counts before what it would quote, so prose keeps its `\|`.
    assert!(!n("cat <<EOF\n\\$x\nEOF").is_resolved());
    assert!(n("cat <<EOF\n\\| x\nEOF").is_resolved());
    // Quoted, none of it expands.
    assert!(n("cat <<'EOF'\n`date` $x \\$y\nEOF").is_resolved());
    // The grammar hands a body whose first byte is a backslash back as an argument word, so
    // the bytes are read from the text: quoted, an empty body would claim a write of nothing.
    let q = n("cat > /w/f <<'EOF'\n\\x\nEOF");
    assert!(q.is_resolved(), "{:?}", q.unresolved);
    assert!(
        q.stages[0].redirects.iter().any(|r| matches!(
            &r.target,
            RedirectTarget::HereDoc { literal: true, body } if body == "\\x\n"
        )),
        "the body was lost: {:?}",
        q.stages[0].redirects
    );
}

#[test]
fn a_here_document_body_a_sibling_swallowed_is_read_from_the_text() {
    // Measured: a redirect or a pipeline between the delimiter and the body leaves
    // `heredoc_body` empty and the body's own text inside that sibling, so an empty body is
    // "not in this node" - and an empty body is a script nobody read, admitted as no code.
    for src in [
        "cat <<'EOF' > /w/f\n\\x\nEOF",
        "cat <<'EOF' | sh\n\\x\nEOF",
        "cat <<'EOF' 2>&1\n\\x\nEOF",
    ] {
        let x = n(src);
        assert!(
            x.stages.iter().flat_map(|s| &s.redirects).any(|r| matches!(
                &r.target,
                RedirectTarget::HereDoc { body, .. } if body == "\\x\n"
            )),
            "the body was lost in {src:?}: {:?}",
            x.stages
        );
    }
}

/// A here-document operator followed by another redirect and then a pipe is the grammar
/// misplacing the pipeline: `| tail` arrives inside the `2>&1` redirect and the text holds no
/// second stage. The rewrite keeps the redirect on the same command and reads.
#[test]
fn a_here_document_followed_by_a_redirect_and_a_pipe_names_the_writing_that_reads() {
    for src in [
        "cat <<'EOF' 2>&1 | tail -3\nx\nEOF",
        "cat <<'EOF' 2>/dev/null | tail -3\nx\nEOF",
    ] {
        let x = n(src);
        assert!(x.partial, "{src}");
        let why = &x.unresolved[0].why;
        assert!(why.contains("here-document operator"), "{src}: {why}");
        assert!(why.contains("before the `<<`"), "{src}: {why}");
    }

    // What the refusal recommends: the same redirect, written before the operator.
    let written = n("cat 2>&1 <<'EOF' | tail -3\nx\nEOF");
    assert!(written.is_resolved(), "{:?}", written.unresolved);
    assert_eq!(written.binaries(), vec!["cat", "tail"]);
    assert!(written.stages[0].pipe_out && written.stages[1].pipe_in);
    assert_eq!(
        written.stages[0].redirects[0].op,
        RedirectOp::Duplicate,
        "{:?}",
        written.stages[0].redirects
    );
}

#[test]
fn a_dash_here_document_hands_over_no_tabs() {
    // `<<-` strips the leading tabs of every line, terminator line included; the grammar
    // strips only the first line's and keeps the rest, so the body here is not the shell's.
    let x = n("cat 3<<-'EOF'\n\t\t\\x\n\t\ty\n\tEOF");
    assert!(
        x.stages.iter().flat_map(|s| &s.redirects).any(|r| matches!(
            &r.target,
            RedirectTarget::HereDoc { literal: true, body } if body == "\\x\ny\n"
        )),
        "the tabs were kept: {:?}",
        x.stages[0].redirects
    );
}

#[test]
fn a_glob_is_a_pattern_and_not_an_unresolvable() {
    // Refusing every `*.rs` would make the gate useless without making it safer.
    let x = n("wc -l *.rs");
    assert!(x.is_resolved());
    assert_eq!(x.stages[0].argv[1], Word::Glob("*.rs".into()));
    // But an escaped star is a literal star.
    let y = n(r"rm \*");
    assert_eq!(y.stages[0].argv[0], Word::Literal("*".into()));
}

#[test]
fn a_known_prefix_survives_an_unresolvable_suffix() {
    // `~/.ssh/$KEY` cannot be resolved, and the prefix is already enough to
    // place it in the secret store. Losing it would lose the decision.
    let x = n("cat ~/.ssh/$KEY");
    assert!(!x.is_resolved());
    assert_eq!(
        x.unresolved[0].known_prefix.as_deref(),
        Some("~/.ssh/"),
        "{:?}",
        x.unresolved
    );
}

#[test]
fn a_process_substitution_lists_the_inner_command_and_refuses_the_path() {
    let x = n("diff <(sort a) <(sort b)");
    assert!(!x.is_resolved());
    assert!(x.binaries().contains(&"sort"));
    assert_eq!(
        x.unresolved
            .iter()
            .filter(|u| u.construct == Construct::ProcessSubstitutionPath)
            .count(),
        2
    );
    assert!(
        x.stages
            .iter()
            .any(|s| s.context.contains(&Context::ProcessSubstitution))
    );
}

#[test]
fn a_stage_in_a_function_body_is_not_claimed_to_run() {
    let x = n("nuke() { rm -rf /; }\nls");
    let rm = x
        .stages
        .iter()
        .find(|s| s.program_name() == Some("rm"))
        .unwrap();
    assert_eq!(rm.certainty, Certainty::OnlyIfCalled);
    let ls = x
        .stages
        .iter()
        .find(|s| s.program_name() == Some("ls"))
        .unwrap();
    assert_eq!(ls.certainty, Certainty::Always);
}

#[test]
fn the_guarded_side_of_a_list_is_conditional() {
    let x = n("test -f x && rm x");
    let rm = x
        .stages
        .iter()
        .find(|s| s.program_name() == Some("rm"))
        .unwrap();
    assert_eq!(rm.certainty, Certainty::Conditional);
    assert!(rm.context.contains(&Context::AndThen));
}

#[test]
fn a_prefix_assignment_belongs_to_its_stage_and_a_statement_one_does_not() {
    let x = n("VAR=1 env; Y=2");
    let env = x
        .stages
        .iter()
        .find(|s| s.program_name() == Some("env"))
        .unwrap();
    assert_eq!(env.assignments.len(), 1);
    assert_eq!(env.assignments[0].name, "VAR");
    assert_eq!(x.assignments.len(), 1);
    assert_eq!(x.assignments[0].name, "Y");
}

#[test]
fn a_subshell_and_a_loop_are_recorded_as_context_not_flattened_away() {
    let x = n("(cd /x && make) ; for f in *.rs; do wc -l $f; done");
    let make = x
        .stages
        .iter()
        .find(|s| s.program_name() == Some("make"))
        .unwrap();
    assert!(make.context.contains(&Context::Subshell));
    let wc = x
        .stages
        .iter()
        .find(|s| s.program_name() == Some("wc"))
        .unwrap();
    assert!(wc.context.contains(&Context::Loop));
    // `$f` is unresolvable even though a human can see the loop's list.
    assert!(!x.is_resolved());
}

#[test]
fn an_empty_command_carries_its_denominator_and_does_not_read_as_clean() {
    let x = n("");
    assert_eq!(x.bytes, 0);
    assert!(x.stages.is_empty());
    // `is_resolved` is true - there is nothing unresolvable in nothing - and
    // that is why a caller must look at `stages`/`bytes` too. A zero with
    // no denominator is not a measurement.
    assert!(x.is_resolved());
}

#[test]
fn a_declaration_and_an_unset_are_stages_with_their_keyword_as_the_program() {
    let x = n("export Z=3; unset Q");
    assert_eq!(x.binaries(), vec!["export", "unset"]);
    let exp = &x.stages[0];
    assert_eq!(exp.assignments[0].name, "Z");
}

#[test]
fn a_write_redirect_stops_stdout_from_surfacing() {
    let a = n("printf hi > log");
    assert!(!a.stages[0].stdout_surfaces());
    // fd 2 to a file leaves stdout alone.
    let b = n("printf hi 2> log");
    assert!(b.stages[0].stdout_surfaces());
    let c = n("printf hi >> log");
    assert!(!c.stages[0].stdout_surfaces());
}

#[test]
fn backquotes_are_command_substitution_too() {
    let x = n("echo `id -u`");
    assert!(!x.is_resolved());
    assert!(x.binaries().contains(&"id"));
    assert!(
        x.unresolved
            .iter()
            .any(|u| u.construct == Construct::CommandSubstitutionValue)
    );
}

#[test]
fn an_expansion_with_a_default_is_still_unresolved() {
    // `${x:-def}` resolves to one of two values and the grammar cannot say
    // which, so claiming `def` would be a guess dressed as a fact.
    let x = n("cat ${x:-def}");
    assert!(!x.is_resolved());
    assert!(matches!(
        &x.unresolved[0].construct,
        Construct::ParameterExpansion { .. }
    ));
}

#[test]
fn nested_substitution_past_one_level_is_still_seen() {
    // `predicate.rs` names this as its own limit: "$(...) nesting past one level
    // are unhandled".
    let x = n("echo $(dirname $(readlink -f /proc/self/exe))");
    assert!(x.binaries().contains(&"dirname"));
    assert!(x.binaries().contains(&"readlink"));
}

/// A run of operands is one hole and flags are kept, because the arity of an operand list is
/// not part of what kind of command this is. A flag IS: `sed -n` prints and `sed -i` rewrites,
/// so flags and their order stay in the key, and an unreadable word stays its own token.
#[test]
fn a_run_of_operands_is_one_hole_and_flags_are_kept() {
    let one = shape(&n("cargo test --offline -p letibot-tools"));
    assert_eq!(one, "cargo <args> --offline -p <args>");
    assert_eq!(
        one,
        shape(&n("cargo test --offline -p letibot-tools another-filter")),
        "an extra filter made a new shape"
    );
    // The brief's own example: `--lib -- <arg> <arg>` and `--lib -- <arg> <arg>
    // <arg>` were two shapes, so re-running tests with one more filter asked again.
    assert_eq!(
        shape(&n("cargo test --offline --lib -- a b")),
        shape(&n("cargo test --offline --lib -- a b c")),
        "one more filter after `--` made a new shape"
    );
    // The keeper, and the reason it is the keeper: a flag changes the kind of call.
    assert_ne!(shape(&n("sed -n 1p f")), shape(&n("sed -i 1p f")));
    assert_ne!(shape(&n("grep -A 3 x f")), shape(&n("grep x -A 3 f")));
    // An unreadable word is never pooled with an approved spelling, and never
    // swallows the operands on either side of it.
    assert_eq!(shape(&n("cat a $X b")), "cat <args> <?> <args>");
    // A count flag stays a count, as it did before the run was collapsed.
    assert_eq!(shape(&n("head -26 f")), "head -<n> <args>");
}

/// The words stage 0 hands its program, for a test that is about an expansion.
fn argv(cmd: &str) -> Vec<String> {
    n(cmd)
        .stages
        .first()
        .map(|s| {
            s.argv
                .iter()
                .map(|w| match w {
                    Word::Literal(l) => l.clone(),
                    Word::Glob(g) => format!("{g}*"),
                    other => format!("{other:?}"),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A brace expansion is the words bash makes of it, in bash's order, and every expectation below
/// is what `printf '<%s>' <the word>` printed rather than what the manual says: an empty
/// alternative makes no word, a nested pair is the one that expands, and zero padding counts the sign.
#[test]
fn a_brace_expansion_is_the_words_the_shell_makes() {
    assert_eq!(argv("echo {a,b}"), ["a", "b"]);
    assert_eq!(argv("echo {1..3}"), ["1", "2", "3"]);
    assert_eq!(argv("echo {a..e}"), ["a", "b", "c", "d", "e"]);
    // Prefix and suffix land on every word.
    assert_eq!(argv("echo f{1,2}.rs"), ["f1.rs", "f2.rs"]);
    // Leftmost outermost, rightmost fastest.
    assert_eq!(
        argv("echo x{1,2}{3,4}"),
        ["x13", "x14", "x23", "x24"],
        "the order is bash's product order"
    );
    // A nested pair expands inside its alternative.
    assert_eq!(
        argv("echo pre{a,{b,c}}post"),
        ["preapost", "prebpost", "precpost"]
    );
    // `{{a,b}}` is `{a}` and `{b}`: the inner pair is the one bash expands.
    assert_eq!(argv("echo {{a,b}}"), ["{a}", "{b}"]);
    // Ranges: descending, stepped, zero-padded, and signed.
    assert_eq!(argv("echo {5..1}"), ["5", "4", "3", "2", "1"]);
    assert_eq!(argv("echo {a..e..2}"), ["a", "c", "e"]);
    assert_eq!(argv("echo {01..03..2}"), ["01", "03"]);
    assert_eq!(argv("echo {001..3}"), ["001", "002", "003"]);
    assert_eq!(
        argv("echo {-01..3}"),
        ["-01", "000", "001", "002", "003"],
        "the padding width counts the minus sign"
    );
    // A step's SIGN is dropped and its magnitude kept: measured, both of these ascend.
    assert_eq!(argv("echo {1..3..-1}"), ["1", "2", "3"]);
    // The empty alternative makes no word, and a comma pair can still be one word.
    assert_eq!(argv("echo {a,}"), ["a"]);
    assert_eq!(argv("echo {a,,b}"), ["a", "b"]);
    assert_eq!(argv("echo a{,}b"), ["ab", "ab"]);
    // A glob the braces produce is a PATTERN, exactly as `*.rs` is.
    assert_eq!(argv("echo {*,x}"), ["**", "x"]);
    // And the whole thing is a reading, not a hole.
    assert!(n("echo f{1,2}.rs").is_resolved());
    assert_eq!(shape(&n("rm -f f{1,2}.rs")), "rm -f <args>");
}

/// The braces bash leaves alone stay one literal word - and each of these is a word
/// that really occurs (a `find -exec` terminator, a `{}` in a commit message).
#[test]
fn a_brace_the_shell_leaves_alone_stays_one_literal_word() {
    assert_eq!(argv("echo {a}"), ["{a}"]);
    assert_eq!(argv("echo {}"), ["{}"]);
    // A quoted brace is not expanded, so it is one literal - and never refused.
    assert_eq!(argv("echo \"{a,b}\""), ["{a,b}"]);
    assert_eq!(argv("echo '{a,b}'"), ["{a,b}"]);
    // A brace with a quoted literal beside it is still bare, so it expands: the quote
    // is not an expansion and the brace is outside it.
    assert_eq!(argv("echo \"x\"{a,b}"), ["xa", "xb"]);
    assert_eq!(argv("echo {a,b}\"x\""), ["ax", "bx"]);
    assert_eq!(argv("echo '/w/'{1,2}.txt"), ["/w/1.txt", "/w/2.txt"]);
    // Which brace is which cannot be said when the quotes hold one of them, and a
    // backslash or an expansion beside a brace leaves the word unread as it did.
    for cmd in ["echo \"{a,b}\"{c,d}", "echo a\\{b,c\\}", "echo \"$X\"{a,b}"] {
        assert!(!n(cmd).is_resolved(), "{cmd} was read: {:?}", n(cmd).stages);
    }
    // A mixed pair and a step that is not a number are not ranges.
    assert_eq!(argv("echo {1..a}"), ["{1..a}"]);
    assert_eq!(argv("echo {1..3..x}"), ["{1..3..x}"]);
    // No closing brace, no expansion.
    assert_eq!(argv("echo a{b,c"), ["a{b,c"]);
    // The `find -exec` terminator, which is why `{}` had to stay literal.
    assert!(n("find . -name x -exec rm -f {} ;").is_resolved());
    // `${a,b}` is a parameter expansion and not a brace expansion: the `$` decides it.
    assert!(!n("echo ${a,b}").is_resolved());
    // Neither half of a split brace is suspicious on its own: `M-}` is a tmux key name and
    // `ls {corpus` is a half-typed line, and bash hands both on as literals. Refusing either
    // alone leaves commands readable and then unreadable.
    assert_eq!(
        argv("tmux send-keys -t nav M-}"),
        ["send-keys", "-t", "nav", "M-}"]
    );
    assert_eq!(argv("ls {corpus"), ["{corpus"]);
    // Both halves in ONE command is the wreckage of `{a b,c}`: bash delivers three
    // words and the grammar delivers two, so neither may be called a literal.
    assert!(!n("echo {a b,c}").is_resolved());
    // A brace inside quotes is not an expansion, and the merge must not forget it: a
    // `gh api graphql query='{repository(...)}'` is one concatenation of `query=` and a quoted
    // string, and read on the MERGED text its braces look like an expansion.
    assert_eq!(
        argv("gh api graphql query='{repository(owner:\"x\", name:\"y\")}' -f a=b"),
        [
            "api",
            "graphql",
            "query={repository(owner:\"x\", name:\"y\")}",
            "-f",
            "a=b"
        ]
    );
    // A quoted `{` is not a brace: measured, `echo x'{'a,b'}'` prints ONE word `x{a,b}`.
    assert_eq!(argv("echo x'{'a,b'}'"), ["x{a,b}"]);
    // A quoted ALTERNATIVE inside unquoted braces is still an expansion (measured:
    // `echo {a,'b'}` is two words), which this refuses rather than expands - an
    // alternative holding a quote is a word whose bytes this does not model.
    assert!(!n("echo {a,'b'}").is_resolved());
}

/// A whole command in the argument position is walked, not swallowed: `time ( ... )` hangs a
/// `subshell` off the `command` node with no field, and reading it as one opaque argument meant
/// `time (cargo test ...)` produced no stage for `cargo`.
#[test]
fn a_compound_in_the_argument_position_is_walked_and_named() {
    let x = n("time (timeout 900 cargo test -p x 2>&1 | tail -3); echo done");
    assert!(!x.is_resolved(), "a compound argument was read as a word");
    // The commands inside RUN, so they are stages.
    for p in ["timeout", "tail", "echo"] {
        assert!(
            x.binaries().contains(&p),
            "{p} was not walked: {:?}",
            x.binaries()
        );
    }
    // And the refusal says what is wrong rather than calling a whole tree a parse error.
    assert!(
        x.unresolved
            .iter()
            .any(|u| u.construct == Construct::CompoundArgument),
        "{:?}",
        x.unresolved
    );
}

/// A brace expansion the text does not decide stays unread - the direction that
/// costs a question rather than giving a wrong answer.
#[test]
fn a_brace_no_longer_readable_than_it_is_stays_unread() {
    for cmd in [
        // An alternative whose value is not in the text.
        "echo {$a,$b}",
        "echo {a,$(x)}",
        // Field splitting would make more words than the braces did.
        "echo {a b,c}",
        // More words than the bound, and the bound is the point: without it the word
        // would have to be called a literal, which claims the program gets `{1..100}`.
        "echo {1..100}",
        "echo f{1..33}",
        // An expansion that makes no word at all: `echo {,}` is `echo` with nothing.
        "echo {,}",
    ] {
        assert!(!n(cmd).is_resolved(), "{cmd} was read: {:?}", argv(cmd));
    }
    // 32 values is inside the bound and 33 is not.
    assert!(n("echo f{1..32}").is_resolved());
    // The refusal names the construct it is about, not "the grammar could not read this".
    assert!(
        n("echo {1..100}")
            .unresolved
            .iter()
            .any(|u| u.construct == Construct::BraceExpansion),
        "{:?}",
        n("echo {1..100}").unresolved
    );
}

/// `$'...'` is decoded into the bytes the program receives, one escape per row against bash 5.2
/// and `od -tx1` of what the shell delivered: `\x` with no digits keeps its backslash, `\x4` is
/// `0x04`, `\1011` is `A1`, and `\c?` is DEL rather than arithmetic.
#[test]
fn an_ansi_c_string_is_decoded_into_the_bytes_the_program_gets() {
    assert_eq!(argv("echo $'a\\tb'"), ["a\tb"]);
    assert_eq!(argv("echo $'a\\nb'"), ["a\nb"]);
    assert_eq!(argv("echo $'\\r'"), ["\r"]);
    assert_eq!(argv("echo $'\\a\\b\\f\\v'"), ["\u{7}\u{8}\u{c}\u{b}"]);
    assert_eq!(argv("echo $'\\e'"), ["\u{1b}"]);
    assert_eq!(argv("echo $'\\E'"), ["\u{1b}"]);
    assert_eq!(argv("echo $'\\\\'"), ["\\"]);
    assert_eq!(argv("echo $'\\''"), ["'"]);
    assert_eq!(argv("echo $'\\\"'"), ["\""]);
    // Hex: two digits, one digit, and none at all.
    assert_eq!(argv("echo $'\\x41'"), ["A"]);
    assert_eq!(argv("echo $'\\x4'"), ["\u{4}"]);
    assert_eq!(argv("echo $'\\x'"), ["\\x"]);
    // Octal, at most three digits: `\0123` is a newline and a `3`.
    assert_eq!(argv("echo $'\\101'"), ["A"]);
    assert_eq!(argv("echo $'\\0123'"), ["\n3"]);
    // A code point is decoded to UTF-8, and a byte is one byte.
    assert_eq!(argv("echo $'\\u00e9'"), ["é"]);
    assert_eq!(argv("echo $'\\U0001F600'"), ["😀"]);
    assert_eq!(argv("echo $'\\x41\\102'"), ["AB"]);
    // Control characters.
    assert_eq!(argv("echo $'\\cA'"), ["\u{1}"]);
    assert_eq!(argv("echo $'\\cZ'"), ["\u{1a}"]);
    assert_eq!(argv("echo $'\\c?'"), ["\u{7f}"]);
    // An escape bash does not know keeps its backslash AND the character.
    assert_eq!(argv("echo $'a\\zb'"), ["a\\zb"]);
    // A decoded quote is a QUOTE: the delimiter cannot reappear in the word, and a
    // decoded tab is a tab inside one argument rather than a field break.
    assert_eq!(argv("echo $'a\\'b'"), ["a'b"]);
    assert_eq!(n("echo pre$'\\t'post").stages[0].argv.len(), 1);
    // A `$'…'` next to ordinary text is still one word.
    assert_eq!(argv("echo pre$'\\t'post"), ["pre\tpost"]);
    assert!(n("echo $'a\\tb'").is_resolved());
}

/// An escape whose bytes are not a string this can carry stays unread. Each one is a real
/// `$'...'` a shell accepts and none has a [`Word`] to be, and a replacement character or the
/// undecoded text would both be wrong claims.
#[test]
fn an_ansi_c_string_that_is_not_a_string_stays_unread() {
    for cmd in [
        "echo $'\\xff'",     // a byte that is not UTF-8
        "echo $'\\0'",       // a NUL, which no argv can hold
        "echo $'\\U110000'", // past the last code point
    ] {
        let x = n(cmd);
        assert!(!x.is_resolved(), "{cmd} was read as {:?}", argv(cmd));
        assert!(
            x.unresolved
                .iter()
                .any(|u| u.construct == Construct::AnsiCEscapes),
            "{cmd}: {:?}",
            x.unresolved
        );
    }
}

/// A `$` at the end of a word is not a parse error. This grammar wraps `$?`, `$#` and even `$$`
/// in a `simple_expansion` when something follows them, but a `$` in the LAST position of a
/// word comes back as the bare token, and the tree is whole - bash's own pid, not a parse error.
#[test]
fn a_dollar_that_ends_a_word_is_the_pid_or_a_literal_and_not_a_parse_error() {
    for cmd in ["echo x$$", "echo a$$b", "echo $$", "echo x$ y$$"] {
        let x = n(cmd);
        assert!(!x.partial, "{cmd}");
        assert!(
            !x.unresolved
                .iter()
                .any(|u| u.construct == Construct::ParseError),
            "{cmd} was reported as a parse error: {:?}",
            x.unresolved
        );
    }
    // A bare `$` is a literal dollar to the shell - `--run=^$` is a regex, not a hole  -
    // and a `$$` is a pid, which the intent layer reads as one of the shell's numbers.
    assert_eq!(argv("echo x$"), ["x$"]);
    assert_eq!(argv("cargo nextest -E run=^$"), ["nextest", "-E", "run=^$"]);
    assert_eq!(n("echo $$").stages[0].argv.len(), 1);
    // The `$` is not silently dropped from the argument list either.
    assert_eq!(n("echo x$$").stages[0].argv.len(), 1);
}
