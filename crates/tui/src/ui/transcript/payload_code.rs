//! A payload's code: which language its text is in, and its rows in rano's syntax roles. The rows
//! are built from ONE parse of the whole payload, so a construct spanning lines keeps its scope;
//! a payload with no language is drawn plain, which is what every payload was before this existed.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use rano::render::{Line, Span};
use rano::style::Role;
use rano::syntax::Lang;

/// Bytes of a payload that get a grid. A folded card shows one line and a window forty, so this is
/// every row anyone scrolls to — and a 418 KB log cannot spend a second of a frame on colours.
const COLOURED_BYTES: usize = 16 * 1024;

/// Lines the shape probe reads: a signature, a doc comment and a keyword is all it takes.
const PROBE_LINES: usize = 40;

/// Points a language needs before it is believed (see [`shape`]).
const ENOUGH: usize = 2;

/// Points a language needs when no line of the sample STARTS like it: a `sed -n` slice begins
/// mid-function, so `fn ` and `use ` are behind the cut. Measured on 6 622 real 21-line windows of
/// this tree's Rust — floor 2 admits 4 of them, floor 3 admits 185, and the 4 are two marks alone.
const ENOUGH_ANYWHERE: usize = 3;

/// Strong marks (see [`shape`]) two of which carry a language on their own.
const ENOUGH_STRONG: usize = 2;

/// Payloads whose grid is kept, so a resize or a fold re-draws a card without parsing it again.
const KEEP: usize = 8;

/// One role per character, one row per line, for `lines` read as one text — or `None`, which means
/// *draw this plain*: no palette, no language, or a grammar that failed under it. `named` is the
/// command the call ran (or the file it read), which decides the language where the lines cannot.
pub(crate) fn grid(
    lines: &[&str],
    colour: bool,
    named: Option<&str>,
) -> Option<Arc<Vec<Vec<Role>>>> {
    if !colour {
        return None;
    }
    let shown = within_budget(lines);
    let lang = lang_of(&sample(shown), named)?;
    cached(shown, lang)
}

/// One payload line in its roles, runs of one role merged into a span so a line of sixty
/// characters is a handful of spans and not sixty. A role the grid does not reach is plain: a grid
/// that came back short says *that row was not understood*, never *that row is gone*.
pub(crate) fn line(text: &str, roles: &[Role]) -> Line {
    let mut spans: Vec<Span> = Vec::new();
    let mut run = String::new();
    let mut open = Role::Plain;
    for (i, c) in text.chars().enumerate() {
        let r = roles.get(i).copied().unwrap_or(Role::Plain);
        if r != open {
            if !run.is_empty() {
                spans.push(span(std::mem::take(&mut run), open));
            }
            open = r;
        }
        run.push(c);
    }
    if !run.is_empty() || spans.is_empty() {
        spans.push(span(run, open));
    }
    Line::new(spans)
}

/// A plain run is a raw span, and that is load-bearing: the row reads a payload line whose spans are
/// all unstyled as *the text as it came*, and paints a command's diff or a tool's prose onto it.
fn span(text: String, role: Role) -> Span {
    match role {
        Role::Plain => Span::raw(text),
        r => Span::role(text, r),
    }
}

/// The payload's lines that fit the budget — all of them for the window this head folds a result
/// to, and a prefix for a log nobody scrolled to. A first line bigger than the whole budget is no
/// prefix at all: it is not code anyone reads, and it is the one shape that could cost a second.
fn within_budget<'a>(lines: &'a [&'a str]) -> &'a [&'a str] {
    let mut bytes = 0;
    for (n, l) in lines.iter().enumerate() {
        bytes += l.len() + 1;
        if bytes > COLOURED_BYTES {
            return &lines[..n];
        }
    }
    lines
}

/// What [`lang_of`] reads: a short prefix, because the language of a file is in its first lines.
fn sample(lines: &[&str]) -> String {
    lines
        .iter()
        .take(PROBE_LINES)
        .copied()
        .collect::<Vec<&str>>()
        .join("\n")
}

/// The language a payload is in: what the command named, then rano's ladder, then the payload's
/// shape. The ladder's `Ini` rung is not believed (it fires on `let mut x = 0;`), and a diff keeps
/// rano's own painting whatever made it — `git diff a.rs` names Rust and prints a patch.
fn lang_of(sample: &str, named: Option<&str>) -> Option<Lang> {
    let ladder = rano::syntax::detect_sample(None, sample);
    if ladder == Some(Lang::Diff) {
        return None;
    }
    // A statement beats an argument: `sed -n 'A,Bp' src/x.rs` is Rust because of its `.rs`, and
    // nothing the cut lines look like outranks the file the command said it read.
    if let Some(lang) = named.and_then(named_lang) {
        return Some(lang);
    }
    match ladder {
        Some(Lang::Ini) => section(sample)
            .then_some(Lang::Ini)
            .or_else(|| shape(sample)),
        Some(lang) => Some(lang),
        None => shape(sample),
    }
}

/// The language a command names: the first word that is a file rano's table knows, by name
/// (`Makefile`) or extension (`.rs`). `detect` gets no sample, so it never reads contents; a
/// command naming nothing (`cargo test`) leaves the answer to the shape probe.
fn named_lang(named: &str) -> Option<Lang> {
    named_paths(named)
        .into_iter()
        .find_map(|p| rano::syntax::detect(Some(std::path::Path::new(p)), None))
        .filter(|lang| *lang != Lang::Diff)
}

/// The file names one command line gives, in order. A word is a run of the head's own path
/// characters (`app::links::path_char`) plus `*`, so no shell is parsed: a quote, `&&`, `,` and a
/// `:` all separate words, and `grep "fn " *.go` names `*.go`.
fn named_paths(command: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut start: Option<usize> = None;
    for (at, c) in command.char_indices() {
        let in_word = crate::app::links::path_char(c) || c == '*';
        match (in_word, start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                out.push(&command[from..at]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        out.push(&command[from..]);
    }
    out
}

/// `[Unit]`, `[package]`, `[dependencies.rano]`: a name in brackets on a line of its own. An array
/// literal (`[1, 2, 3]`, `[u8; 4]`) is not one, which the comma and the punctuation rule keep out.
fn section(sample: &str) -> bool {
    sample.lines().any(|l| {
        let t = l.trim();
        let Some(inner) = t.strip_prefix('[').and_then(|r| r.strip_suffix(']')) else {
            return false;
        };
        !inner.is_empty()
            && inner
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-. ".contains(c))
    })
}

/// What one language scored on a sample: `structural` is what its lines that START with a mark
/// say, `strong` is how many marks only it puts inside a line, and `total` is every point earned.
#[derive(Clone, Copy, Debug)]
struct Points {
    structural: usize,
    strong: usize,
    total: usize,
}

/// What the payload's own shape says: every mark is SYNTAX and never a word prose also uses, each
/// counts at most twice, and a language needs more points than any other and either a line that
/// STARTS like it or [`ENOUGH_STRONG`] marks inside the lines (a slice out of a function has none).
fn shape(sample: &str) -> Option<Lang> {
    let lines: Vec<&str> = sample
        .lines()
        .map(str::trim_start)
        .filter(|l| !l.is_empty())
        .take(PROBE_LINES)
        .collect();
    // A JSON document is braces around a quoted key, which is a shape no code sample has.
    if let Some(first) = lines.first()
        && (first.starts_with('{') || first.starts_with('['))
        && lines.iter().any(|l| l.contains("\": "))
    {
        return Some(Lang::Json);
    }
    let scored = points_of(&lines);
    let (lang, top) = scored[0];
    let unique = scored.get(1).is_none_or(|(_, next)| next.total < top.total);
    let shaped = top.structural > 0 && top.total >= ENOUGH;
    let spoken = top.strong >= ENOUGH_STRONG && top.total >= ENOUGH_ANYWHERE;
    (unique && (shaped || spoken)).then_some(lang)
}

/// Every language's points on a sample's non-empty lines, best first. The marks are the ones no
/// language shares: `weak` reads alone rarely (a path, a word prose uses), `strong` is syntax only
/// the language writes — and the second list is what a slice from inside a function is read by.
fn points_of(lines: &[&str]) -> Vec<(Lang, Points)> {
    let count = |needle: &str, at_start: bool| -> usize {
        lines
            .iter()
            .filter(|l| {
                if at_start {
                    l.starts_with(needle)
                } else {
                    l.contains(needle)
                }
            })
            .count()
            .min(2)
    };
    let score = |starts: &[&str], weak: &[&str], strong: &[&str]| -> Points {
        let structural: usize = starts.iter().map(|m| count(m, true) * 2).sum();
        let weak: usize = weak.iter().map(|m| count(m, false)).sum();
        let strong: usize = strong.iter().map(|m| count(m, false)).sum();
        Points {
            structural,
            strong,
            total: structural + weak + strong,
        }
    };
    let languages = [
        (
            Lang::Rust,
            score(
                &["fn ", "impl ", "let mut ", "#[", "pub fn ", "use "],
                &["::", "pub "],
                &[
                    "&self",
                    " -> ",
                    "Ok(",
                    "Err(",
                    "Some(",
                    "None",
                    "&mut ",
                    "unsafe ",
                    // **Rust's macro syntax is Rust's alone**, and it is what test code is made of:
                    // `assert!`, `assert_eq!` and `matches!` have no twin in Python, Go or JS, so a
                    // slice of a test file — which has no `fn`, no `&mut`, no `Ok(` in it — still
                    // says what it is. The operator's card, `sed -n '424,450p' …/render/tests.rs`,
                    // was six of these and nothing else recognisable.
                    "assert!",
                    "assert_eq!",
                    "assert_ne!",
                    "matches!",
                    "vec![",
                    "format!",
                    "unwrap()",
                ],
            ),
        ),
        (
            Lang::Python,
            score(
                &["def ", "class ", "import ", "from ", "elif ", "__name__"],
                &["None", "print("],
                &["self.", "__init__"],
            ),
        ),
        (
            Lang::Go,
            score(
                &["func ", "package ", "import (", "type "],
                &["nil"],
                &[":=", "fmt."],
            ),
        ),
        (
            Lang::JavaScript,
            score(
                &[
                    "const ",
                    "let ",
                    "function ",
                    "import ",
                    "export ",
                    "class ",
                ],
                &["===", "require("],
                &["=>", "console.log("],
            ),
        ),
    ];
    let mut scored: Vec<(Lang, Points)> = languages.to_vec();
    scored.sort_by_key(|(_, p)| std::cmp::Reverse(p.total));
    scored
}

/// One cached grid: the text's hash, its row count, the language, and the roles.
type Entry = (u64, usize, Lang, Arc<Vec<Vec<Role>>>);

/// The last [`KEEP`] payloads' grids. `role_grid` costs 3 ms on 4 KiB of Rust in a debug build and
/// 0.5 ms in release (the test `a_payload_is_highlighted_at_what_it_costs` prints both), and a row
/// is rendered again on a resize or a fold, which is what this keeps off the frame.
struct Cache {
    /// Oldest first. Keyed by the text's hash, its row count and the language: a hash alone would
    /// let two payloads wear each other's colours, and the row count makes that a longer bet.
    entries: Vec<Entry>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    entries: Vec::new(),
});

fn cached(lines: &[&str], lang: Lang) -> Option<Arc<Vec<Vec<Role>>>> {
    let mut hasher = DefaultHasher::new();
    lines.hash(&mut hasher);
    let key = hasher.finish();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((.., grid)) = cache
        .entries
        .iter()
        .find(|(h, n, l, _)| *h == key && *n == lines.len() && *l == lang)
    {
        return Some(grid.clone());
    }
    let grid = built(lines, lang);
    if grid.is_empty() {
        return None;
    }
    let grid = Arc::new(grid);
    if cache.entries.len() == KEEP {
        cache.entries.remove(0);
    }
    cache.entries.push((key, lines.len(), lang, grid.clone()));
    Some(grid)
}

/// The grid itself, and the one place a grammar can take more than its own row with it: a panic
/// under the highlighter leaves the payload plain, because the payload is the model's output and
/// the operator's evidence, and a frame is not worth losing it — never mind the head.
fn built(lines: &[&str], lang: Lang) -> Vec<Vec<Role>> {
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rano::highlight::role_grid(lines, Some(lang))
    }));
    ran.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A few lines of rano's own Rust, as a `sed -n` window hands them over: a signature, a
    /// statement, a comment, and a keyword next to a string on one line.
    const RUST: &str = r#"    pub fn spawn(&mut self, cmd: &mut Command) -> io::Result<Pty> {
        let mut master = 0;
        // openpty takes a winsize pointer on every target now
        let what = "openpty";
"#;

    /// The slice the operator's card showed as plain: a `sed -n 'A,Bp'` window that begins inside
    /// a function, so not one line of it starts with `fn `, `use ` or `pub fn `.
    const SLICE: &str = r#"        // mutable form for both. `&mut` is what compiles on every target this builds for.
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            // pixel size is not what a caller sets here, so leave it to the kernel
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                &size as *const libc::winsize,
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((master, slave))
"#;

    /// The other card: a longer slice that happens to keep its anchor marks, and was highlighted.
    const SLICE_WITH_ANCHORS: &str = r#"use std::time::{Duration, Instant};
fn openpty() -> Option<(RawFd, RawFd)> {
    let mut master = 0i32;
    let mut slave = 0i32;
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            &mut size as *mut libc::winsize,
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return None;
    }
    Some((master, slave))
}

/// The bytes a pty write may lose before the reader drains it, measured once at open.
fn drain_window(fd: RawFd) -> Duration {
    let mut bytes = 0i32;
    let rc = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut bytes) };
    if rc != 0 {
        return Duration::from_millis(0);
    }
    Duration::from_millis(u64::from(bytes.max(0)) / 8)
}

/// A pair whose child is the process it was opened for, and when that happened.
pub struct Pty {
    pub master: RawFd,
    pub slave: RawFd,
    opened: Instant,
}

impl Pty {
    /// Open a pair and remember when: a read window measured from here is honest.
    pub fn open(rows: u16, cols: u16) -> Option<(RawFd, RawFd)> {
        let (master, slave) = openpty()?;
        Some((master, slave))
    }
}
"#;

    /// The marks [`shape`] reads at the START of a line: none of these may begin a line of
    /// [`SLICE`], which is what makes it the case the structural gate could not see.
    const ANCHORS: [&str; 6] = ["fn ", "impl ", "let mut ", "#[", "pub fn ", "use "];

    fn drawn_of(text: &str) -> Vec<Line> {
        drawn_named(text, None)
    }

    fn drawn_named(text: &str, named: Option<&str>) -> Vec<Line> {
        let lines: Vec<&str> = text.lines().collect();
        let grid = grid(&lines, true, named);
        lines
            .iter()
            .enumerate()
            .map(|(n, l)| {
                line(
                    l,
                    grid.as_ref()
                        .and_then(|g| g.get(n))
                        .map_or(&[][..], Vec::as_slice),
                )
            })
            .collect()
    }

    fn roles(line: &Line) -> Vec<Role> {
        line.spans.iter().map(|s| s.style.top()).collect()
    }

    #[test]
    fn a_rust_payload_is_a_keyword_and_a_string_on_one_line() {
        let drawn = drawn_of(RUST);
        assert_eq!(drawn.len(), RUST.lines().count(), "one row per line");
        assert_eq!(drawn[3].plain(), "        let what = \"openpty\";");
        let got = roles(&drawn[3]);
        assert!(
            got.contains(&Role::Keyword) && got.contains(&Role::StringLit),
            "a keyword and a string, and not one role for the whole row: {got:?}"
        );
        assert!(got.len() > 2, "punctuation stays its own run: {got:?}");
        assert_eq!(
            roles(&drawn[2]).last(),
            Some(&Role::Comment),
            "and the comment is a comment"
        );
    }

    /// A run of one role is one span, and the text is untouched: the payload is the evidence.
    #[test]
    fn the_runs_are_merged_and_the_bytes_are_the_payload_s() {
        let drawn = drawn_of(RUST);
        for (n, l) in drawn.iter().enumerate() {
            assert_eq!(l.plain(), RUST.lines().nth(n).unwrap());
        }
        assert!(
            drawn[3].spans.len() < 12,
            "six runs, not one span a character: {:?}",
            drawn[3].spans
        );
    }

    #[test]
    fn prose_has_no_language_and_is_drawn_plain() {
        let out = "   Compiling letibot-tui v0.6.0\n    Finished dev profile in 12.3s\n";
        let lines: Vec<&str> = out.lines().collect();
        assert!(grid(&lines, true, None).is_none());
        for l in drawn_of(out) {
            assert!(
                l.spans
                    .iter()
                    .all(|s| s.style == rano::render::Style::new()),
                "not one role on {:?}: {:?}",
                l.plain(),
                l.spans
            );
        }
    }

    /// The colour decision is the palette's, and a terminal that cannot paint a role gets the bytes
    /// it always got — no parse, and nothing the row can mistake for a styled line.
    #[test]
    fn a_head_with_no_palette_parses_nothing() {
        let lines: Vec<&str> = RUST.lines().collect();
        assert!(grid(&lines, false, None).is_none());
    }

    /// A directory of English is not an ini file, `[1, 2]` is not a section, and a sentence with the
    /// word `impl` in it is not Rust.
    #[test]
    fn the_ladders_weakest_rungs_are_not_believed() {
        assert!(section("[dependencies.rano]\nrano = { git = \"x\" }\n"));
        assert!(!section("let v: [u8; 4] = [1, 2, 3];\n"));
        assert_eq!(
            lang_of("[package]\nname = \"letibot\"\n", None),
            Some(Lang::Ini)
        );
        assert_eq!(
            lang_of(
                "   Compiling letibot-tui v0.4.0\n    Finished in 12.3s\n",
                None
            ),
            None
        );
        assert_eq!(
            lang_of("we should impl this later\nand check it\n", None),
            None
        );
        // A cargo test summary is full of `::` and is not Rust: a language needs a line that STARTS
        // like it.
        assert_eq!(
            lang_of(
                "running 2 tests\ntest store::write ... ok\ntest store::read ... ok\n\
                 test result: ok. 2 passed; 0 failed\n",
                None
            ),
            None
        );
    }

    #[test]
    fn the_shape_probe_knows_four_languages_and_guesses_no_others() {
        for (want, text) in [
            (Lang::Rust, RUST),
            (
                Lang::Python,
                "def main() -> int:\n    self.x = 1\n    print(\"hi\")\n",
            ),
            (
                Lang::Go,
                "package main\n\nfunc main() {\n\tx := 1\n\tfmt.Println(x)\n}\n",
            ),
            (
                Lang::JavaScript,
                "const x = 1;\nfunction f() {\n  console.log(x);\n}\n",
            ),
            (Lang::Json, "{\n  \"name\": \"letibot\",\n  \"n\": 3\n}\n"),
        ] {
            assert_eq!(lang_of(text, None), Some(want), "{text}");
        }
    }

    /// The regression the operator found, on the card it was found on: a `sed -n 'A,Bp'` window
    /// starts inside a function, so not one line of it STARTS like Rust — and it is Rust, on the
    /// marks inside its lines.
    #[test]
    fn a_slice_from_inside_a_function_is_still_rust() {
        assert_eq!(SLICE.lines().count(), 21, "the card's twenty-one lines");
        for l in SLICE.lines() {
            assert!(
                !ANCHORS.iter().any(|m| l.trim_start().starts_with(m)),
                "no line starts like Rust, so the structural gate cannot answer: {l:?}"
            );
        }
        let rows: Vec<&str> = SLICE.lines().map(str::trim_start).collect();
        let (lang, points) = points_of(&rows)[0];
        assert_eq!(lang, Lang::Rust);
        assert_eq!(
            points.structural, 0,
            "the anywhere path is what decides this one"
        );
        assert!(
            points.strong >= ENOUGH_STRONG && points.total >= ENOUGH_ANYWHERE,
            "`&mut `, `unsafe `, `Ok(` and `Err(` are the evidence: {points:?}"
        );
        assert_eq!(lang_of(SLICE, None), Some(Lang::Rust));
        let drawn = drawn_of(SLICE);
        assert_eq!(drawn.len(), 21, "one row per line");
        assert!(
            roles(&drawn[8]).contains(&Role::Keyword),
            "`let rc = unsafe {{` is drawn in roles, not plain: {:?}",
            roles(&drawn[8])
        );
    }

    /// **A slice of test code is Rust by its macros alone** — no `fn`, no `&mut`, no `Ok(`; the
    /// operator's card from `src/markdown/render/tests.rs` had six assertions and nothing else.
    #[test]
    fn a_slice_of_test_code_is_rust_by_its_macros() {
        for l in TEST_SLICE.lines() {
            assert!(
                !ANCHORS.iter().any(|m| l.trim_start().starts_with(m)),
                "no line starts like Rust, so the structural gate cannot answer: {l:?}"
            );
        }
        assert_eq!(lang_of(TEST_SLICE, None), Some(Lang::Rust), "{TEST_SLICE}");
    }

    /// The card the slice was compared with keeps its answer, and so do the payloads the structural
    /// gate was built to refuse: a run of test names, a build log, a shell transcript.
    #[test]
    fn an_anchored_slice_is_rust_and_a_summary_of_test_names_is_not() {
        assert_eq!(SLICE_WITH_ANCHORS.lines().count(), 49);
        assert_eq!(lang_of(SLICE_WITH_ANCHORS, None), Some(Lang::Rust));
        for text in [SUMMARY, LONG_SUMMARY, BUILD_LOG, SHELL] {
            assert_eq!(lang_of(text, None), None, "{text}");
        }
    }

    /// A `cargo test` summary is full of `::` and is not Rust: `::` is a weak mark, and no strong
    /// mark of any language is in these.
    /// **The operator's second card**: a slice of a test file — `sed -n '424,450p'
    /// src/markdown/render/tests.rs` — which starts mid-function, has no anchored Rust mark, and whose
    /// only Rust-only signal is its macros (`assert!`, `assert_eq!`). It was drawn plain.
    const TEST_SLICE: &str = r#"        assert!(
            first_row.starts_with('│') && first_row.ends_with('│'),
            "{screen}"
        );
        assert_eq!(
            bar(header),
            bar(first_row),
            "header and first row disagree:\n{screen}"
        );
        for l in &lines {
            assert!(l.width() <= 120, "{} columns: {:?}", l.width(), l.plain());
        }
        // The header text is strong, and the box has two weights: the frame's edges and
        // the rule under the header are bold, the separators between cells are plain.
        assert_eq!(span_of(&lines, "branch")[0].style.top(), Role::Strong);
        assert_eq!(
            lines[0].spans[0].style.top(),
            Role::Strong,
            "the top rule is the frame's edge: {}",
            lines[0].plain()
        );"#;

    const SUMMARY: &str = "running 2 tests\ntest store::write ... ok\ntest store::read ... ok\n\
                          test result: ok. 2 passed; 0 failed\n";

    /// The same, longer: `count` stops at two marks of one kind, so the floor has to hold past that.
    const LONG_SUMMARY: &str = "running 8 tests\n\
        test mergequeue::serve::tests::a_row_is_taken ... ok\n\
        test mergequeue::serve::tests::a_row_is_judged ... ok\n\
        test mergequeue::serve::tests::a_row_is_landed ... ok\n\
        test mergequeue::git::tests::a_ref_is_proved ... ok\n\
        test mergequeue::git::tests::a_tree_is_reset ... ok\n\
        test mergequeue::desk::tests::a_status_is_one_line ... ok\n\
        test mergequeue::lease::tests::a_lease_is_taken ... ok\n\
        test result: ok. 8 passed; 0 failed\n";

    /// A build's own words: no syntax of any language, and no strong mark either.
    const BUILD_LOG: &str =
        "   Compiling letibot-tui v0.6.0\n    Finished `dev` profile in 12.34s\n";

    /// A shell transcript: a command line, what it printed, and a path. Not a language.
    const SHELL: &str = "$ cargo test -p letibot-tui --lib payload_code\n\
        running 4 tests\ntest result: ok. 4 passed; 0 failed\n\
        $ git status --short\n M crates/tui/src/ui/transcript/payload_code.rs\n";

    /// A grid shorter than the payload is *the rest was not understood*, never a lost line.
    #[test]
    fn a_row_past_the_grid_is_plain_and_still_there() {
        let drawn = line("let x = 1;", &[Role::Keyword]);
        assert_eq!(drawn.plain(), "let x = 1;");
        assert_eq!(
            drawn.spans.first().map(|s| s.style.top()),
            Some(Role::Keyword),
            "the role the grid had reaches, up to where it stops"
        );
        assert_eq!(line("let x = 1;", &[]).plain(), "let x = 1;");
    }

    /// A `sed -n` window cut out of a file has only the shape of the lines it happened to slice,
    /// and `let n = 0;` twice is the JavaScript probe's OWN structural mark — so the shape answers
    /// JavaScript here, which is the wrong answer a command's `.rs` corrects.
    const CUT: &str = "        let n = 0;\n        let m = n + 1;\n";

    /// The command the operator's card drew, as the head receives it: `display_target` puts a
    /// debug-quoted label round a command with spaces in it, and the quotes are just separators.
    const SED: &str = "\"cd rano && sed -n '424,450p' src/markdown/render/tests.rs\"";

    /// A few lines of a Go file, which is what `grep "func " *.go` hands back.
    const GO: &str = "package main\n\nfunc main() {\n\tx := 1\n}\n";

    /// A unified diff, whose own header rano's ladder reads.
    const PATCH: &str =
        "--- a/src/x.rs\n+++ b/src/x.rs\n@@ -1,2 +1,2 @@\n-let n = 0;\n+let n = 1;\n";

    /// The operator's insight, on the card that made it: the command says `…/tests.rs`, so the
    /// payload is Rust although forty lines of it read as JavaScript.
    #[test]
    fn the_command_names_the_language_of_a_slice() {
        assert_eq!(
            lang_of(CUT, None),
            Some(Lang::JavaScript),
            "the premise: the shape probe is WRONG about this slice without the command"
        );
        assert_eq!(named_lang(SED), Some(Lang::Rust));
        assert_eq!(lang_of(CUT, Some(SED)), Some(Lang::Rust));
        let drawn = drawn_named(CUT, Some(SED));
        assert_eq!(drawn.len(), 2, "one row per line");
        assert!(
            roles(&drawn[0]).contains(&Role::Keyword),
            "`let` is drawn as Rust's keyword, not as plain text: {:?}",
            roles(&drawn[0])
        );
        assert_eq!(
            drawn[0].plain(),
            "        let n = 0;",
            "and not a byte moved"
        );
    }

    /// The words fall out of the command where a shell puts its punctuation, and a glob names the
    /// language of the files it stands for.
    #[test]
    fn a_glob_and_a_quoted_pattern_name_the_language_of_their_files() {
        assert_eq!(
            named_paths(SED),
            [
                "cd",
                "rano",
                "sed",
                "-n",
                "424",
                "450p",
                "src/markdown/render/tests.rs"
            ],
            "a quote, a comma and an `&&` are all separators"
        );
        assert_eq!(
            named_paths("head -3 ../src/x.rs:120"),
            ["head", "-3", "../src/x.rs", "120"],
            "a `:NN` rides beside its path, and `../` is part of it"
        );
        assert_eq!(named_lang("grep -n \"func \" *.go"), Some(Lang::Go));
        assert_eq!(
            lang_of(BUILD_LOG, Some("grep -n \"func \" *.go")),
            Some(Lang::Go)
        );
        assert!(
            roles(&drawn_named(GO, Some("grep -n \"func \" *.go"))[2]).contains(&Role::Keyword),
            "`func` is drawn in Go's own roles"
        );
    }

    /// The first name rano knows wins, and a name it does not know is passed over — so a command
    /// naming two files of different languages answers with the one it read first.
    #[test]
    fn the_first_name_rano_knows_wins() {
        assert_eq!(named_lang("cat a.py b.rs"), Some(Lang::Python));
        assert_eq!(
            named_lang("cat notes.txt b.rs"),
            Some(Lang::Rust),
            "`.txt` is no grammar's, so the next word answers"
        );
        assert_eq!(
            named_lang("cat Makefile"),
            Some(Lang::Make),
            "a name is a statement too, and not only an extension"
        );
    }

    /// A command naming no file leaves the payload to its shape, exactly as before this rung
    /// existed: `cargo test` and `echo hello` name nothing a grammar knows.
    #[test]
    fn a_command_naming_no_file_leaves_the_shape_probe_to_answer() {
        assert_eq!(named_lang("cargo test -p letibot-tui --lib"), None);
        assert_eq!(named_lang("echo hello"), None);
        assert_eq!(named_lang("cat notes.txt"), None);
        assert_eq!(
            lang_of(CUT, Some("echo hello")),
            lang_of(CUT, None),
            "the shape probe's answer, unchanged"
        );
        assert_eq!(
            lang_of(RUST, Some("cargo test -p letibot-tui --lib")),
            Some(Lang::Rust),
            "a payload with a shape of its own is still read by its shape"
        );
        assert_eq!(
            lang_of(SUMMARY, Some("cargo test")),
            None,
            "a summary is not code"
        );
        // `.yaml` is a name rano's own table carries (`syntax.rs`'s hand-written extensions), so
        // this answers Yaml: the rung reports what rano knows, and the shape probe only speaks
        // when rano does not.
        assert_eq!(named_lang("cat config.yaml"), Some(Lang::Yaml));
        assert_eq!(
            lang_of("a: 1\nb: 2\n", Some("cat config.yaml")),
            Some(Lang::Yaml)
        );
    }

    /// The one rule the command cannot overrule: a payload that IS a diff keeps rano's own
    /// painting, because `git diff src/x.rs` names Rust and prints a patch.
    #[test]
    fn a_diff_is_not_code_whatever_command_made_it() {
        assert_eq!(
            rano::syntax::detect_sample(None, PATCH),
            Some(Lang::Diff),
            "the premise: the payload says what it is"
        );
        assert_eq!(lang_of(PATCH, Some("git diff src/x.rs")), None);
        assert_eq!(
            lang_of(PATCH, None),
            None,
            "and it was None before this rung"
        );
    }

    /// The budget, which is what bounds a payload's cost: a prefix of a log, and no grid at all for
    /// a single line bigger than the whole of it.
    #[test]
    fn the_budget_takes_a_prefix_and_refuses_one_huge_line() {
        let rows: Vec<String> = (0..3000).map(|n| format!("let row = {n};")).collect();
        let lines: Vec<&str> = rows.iter().map(String::as_str).collect();
        let shown = within_budget(&lines);
        let bytes: usize = shown.iter().map(|l| l.len() + 1).sum();
        assert!(shown.len() < 3000, "a log is cut: {} rows", shown.len());
        assert!(bytes <= COLOURED_BYTES, "cut inside the budget: {bytes}");
        let blob = format!("{{\"blob\": \"{}\"}}", "x".repeat(COLOURED_BYTES));
        assert!(
            within_budget(&[blob.as_str()]).is_empty(),
            "one line bigger than the budget gets no grid"
        );
    }

    /// The number the cache is sized on: `role_grid` on the tail window a payload is folded to, 4 KiB
    /// of Rust. Not an assertion about this box's speed — a ceiling four hundred times the
    /// measurement, so a grammar turning quadratic is caught and a loaded gate is not.
    #[test]
    fn a_payload_is_highlighted_at_what_it_costs() {
        let mut body = String::new();
        while body.len() < 4096 {
            body.push_str(RUST);
        }
        let lines: Vec<&str> = body.lines().collect();
        let t = std::time::Instant::now();
        let g = rano::highlight::role_grid(&lines, Some(Lang::Rust));
        let first = t.elapsed();
        assert_eq!(g.len(), lines.len());
        let t = std::time::Instant::now();
        let _ = grid(&lines, true, None);
        let cold = t.elapsed();
        let t = std::time::Instant::now();
        for _ in 0..10 {
            let _ = grid(&lines, true, None);
        }
        let warm = t.elapsed() / 10;
        println!(
            "payload {} bytes, {} lines: role_grid {first:?} (the grammar's query compiled in this \
             call), first grid {cold:?}, cached {warm:?}",
            body.len(),
            lines.len()
        );
        assert!(
            cold < std::time::Duration::from_secs(1),
            "4 KiB must not cost a second: {cold:?}"
        );
    }
}
