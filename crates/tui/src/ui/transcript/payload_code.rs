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

/// Payloads whose grid is kept, so a resize or a fold re-draws a card without parsing it again.
const KEEP: usize = 8;

/// One role per character, one row per line, for `lines` read as one text — or `None`, which means
/// *draw this plain*: no palette to paint a role in, no language the highlighter knows, or a
/// grammar that failed under it. The caller decides whether the text is code at all.
pub(crate) fn grid(lines: &[&str], colour: bool) -> Option<Arc<Vec<Vec<Role>>>> {
    if !colour {
        return None;
    }
    let shown = within_budget(lines);
    let lang = lang_of(&sample(shown))?;
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

/// The language a payload is in: rano's ladder first (a shebang, a diff header, a section), then the
/// payload's own shape. The ladder's last rung is not believed — it answers `Ini` for `let mut x =
/// 0;`, because `key = value` is what fires it — and a diff is left to rano's own painter.
fn lang_of(sample: &str) -> Option<Lang> {
    match rano::syntax::detect_sample(None, sample) {
        Some(Lang::Diff) => None,
        Some(Lang::Ini) => section(sample)
            .then_some(Lang::Ini)
            .or_else(|| shape(sample)),
        Some(lang) => Some(lang),
        None => shape(sample),
    }
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

/// What the payload's own shape says, when no name does. Every mark is SYNTAX (`let mut`, `:=`) and
/// never a word prose also uses, each counts at most twice, and a language needs [`ENOUGH`] points
/// and more than any other — so a wrong guess costs colour and never text.
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
    let score = |starts: &[&str], has: &[&str]| -> (usize, usize) {
        let structural: usize = starts.iter().map(|m| count(m, true) * 2).sum();
        let anywhere: usize = has.iter().map(|m| count(m, false)).sum();
        (structural, structural + anywhere)
    };
    let languages = [
        (
            Lang::Rust,
            score(
                &["fn ", "impl ", "let mut ", "#[", "pub fn ", "use "],
                &["&self", " -> ", "::", "Ok(", "Some(", "&mut ", "pub "],
            ),
        ),
        (
            Lang::Python,
            score(
                &["def ", "class ", "import ", "from ", "elif ", "__name__"],
                &["self.", "None", "print(", "__init__"],
            ),
        ),
        (
            Lang::Go,
            score(
                &["func ", "package ", "import (", "type "],
                &[":=", "fmt.", "nil"],
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
                &["=>", "console.log(", "require(", "==="],
            ),
        ),
    ];
    let mut scored: Vec<(Lang, usize, usize)> = languages
        .iter()
        .map(|(lang, (structural, total))| (*lang, *structural, *total))
        .collect();
    scored.sort_by_key(|s| std::cmp::Reverse(s.2));
    let (lang, structural, top) = scored[0];
    // A line that STARTS like the language is what makes it a guess rather than a coincidence: two
    // test names with `::` in them are a `cargo test` summary, not Rust.
    (structural > 0 && top >= ENOUGH && scored.get(1).is_none_or(|s| s.2 < top)).then_some(lang)
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

    fn drawn_of(text: &str) -> Vec<Line> {
        let lines: Vec<&str> = text.lines().collect();
        let grid = grid(&lines, true);
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
        assert!(grid(&lines, true).is_none());
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
        assert!(grid(&lines, false).is_none());
    }

    /// A directory of English is not an ini file, `[1, 2]` is not a section, and a sentence with the
    /// word `impl` in it is not Rust.
    #[test]
    fn the_ladders_weakest_rungs_are_not_believed() {
        assert!(section("[dependencies.rano]\nrano = { git = \"x\" }\n"));
        assert!(!section("let v: [u8; 4] = [1, 2, 3];\n"));
        assert_eq!(lang_of("[package]\nname = \"letibot\"\n"), Some(Lang::Ini));
        assert_eq!(
            lang_of("   Compiling letibot-tui v0.4.0\n    Finished in 12.3s\n"),
            None
        );
        assert_eq!(lang_of("we should impl this later\nand check it\n"), None);
        // A cargo test summary is full of `::` and is not Rust: a language needs a line that STARTS
        // like it.
        assert_eq!(
            lang_of(
                "running 2 tests\ntest store::write ... ok\ntest store::read ... ok\n\
                 test result: ok. 2 passed; 0 failed\n"
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
            assert_eq!(lang_of(text), Some(want), "{text}");
        }
    }

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
        let _ = grid(&lines, true);
        let cold = t.elapsed();
        let t = std::time::Instant::now();
        for _ in 0..10 {
            let _ = grid(&lines, true);
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
