//! The two pattern languages the search tools use, and the relaxation clause 1
//! demands.
//!
//! # Why this stopped being hand-written
//!
//! It used to be, and the argument for that was written down here: what is needed
//! is a matcher over single lines with a **known, statable** subset, because
//! clause 1's third acceptance case — *a too-strict anchor is auto-relaxed to the
//! bare identifier and the relaxation is reported* — is a property of the
//! pattern's syntax tree, not of matching. A full engine would give more syntax
//! and no more of the thing that matters.
//!
//! **The reasoning was right and the conclusion was wrong, and a measurement is
//! what separated them.** Searching `crates/` (137 files, 1.9 MB, 48,667 lines)
//! with `cargo run --release -p letibot-tools --example greptime`:
//!
//! ```text
//! walk     0.4 ms
//! read     0.9 ms
//! match   23.7 ms   <- 95% of the total
//! ```
//!
//! ripgrep answers the same question in 9-10 ms. So the hand-rolled matcher was
//! not paying for the syntax tree it gave us — `regex_syntax` gives the *same*
//! syntax tree, and a better one, as a by-product of parsing. What the hand-rolled
//! engine was actually costing was a backtracking `Vec<char>` scan per line, which
//! is where 95% of a search went.
//!
//! The part of the old argument that survives is the part that mattered: **a
//! construct this engine cannot compile is REPORTED, never silently
//! approximated.** What changed is the remedy. The old engine treated an
//! unsupported construct as a literal and put a note beside the result — which is
//! still a search for something other than what was asked, merely an annotated
//! one. [`Pattern::compile`] now returns the engine's own error, with the position
//! in the pattern, and `grep` hands that to the model instead of a result.
//!
//! Two limits are deliberate and stated rather than left at the crate's defaults:
//! [`SIZE_LIMIT`] and [`DFA_SIZE_LIMIT`]. A pattern arrives from a model, and a
//! model can write `(a|b|c|…){20}` without meaning to; the refusal is reported the
//! same way a syntax error is.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use regex::{Regex, RegexBuilder};

/// The compiled program's ceiling, in bytes.
///
/// The crate's default is 10 MiB. A model writing a search does not need a program
/// that large, and the tool runtime is answering a turn a human is waiting on, so
/// the trade is the other way round from a general-purpose library's.
pub const SIZE_LIMIT: usize = 1 << 20;

/// The lazy-DFA cache ceiling. Exceeding it is not an error at compile time — the
/// engine falls back to a slower strategy — so this bounds memory, not syntax.
pub const DFA_SIZE_LIMIT: usize = 1 << 21;

/// A compiled search pattern.
///
/// Matching is **per line**, which is what `grep` reports, and the two entry
/// points differ only in how much text they are handed at once:
///
/// - [`Pattern::is_match`] takes one line.
/// - [`Pattern::line_hits`] takes a whole file and returns the 1-based numbers of
///   the lines that match. It exists because scanning a buffer once lets the
///   engine's literal prefilters skip most of it, and calling `is_match` 48,667
///   times does not.
#[derive(Debug, Clone)]
pub struct Pattern {
    re: Arc<Regex>,
    /// The same pattern in multi-line mode, for [`Pattern::line_hits`].
    multi: Arc<Regex>,
    pub source: String,
    pub case_insensitive: bool,
}

/// Why a pattern could not be compiled, in the engine's own words.
///
/// Kept as a struct rather than collapsed to a `String` so the caller can tell a
/// refusal (too big) from a malformed pattern without matching on prose.
#[derive(Debug, Clone)]
pub struct PatternError {
    /// The pattern as written.
    pub source: String,
    /// `regex`'s own message. It names the offending span and, for a syntax
    /// error, underlines it.
    pub message: String,
    /// True when the pattern parsed but the compiled program exceeded
    /// [`SIZE_LIMIT`].
    pub too_large: bool,
}

impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for PatternError {}

impl PatternError {
    /// One sentence naming what to do, which is different for the two kinds.
    pub fn remedy(&self) -> String {
        if self.too_large {
            format!(
                "`{}` compiles to a program larger than the {} KiB this tool allows. \
                 Search for a smaller piece of it — usually the identifier — and \
                 narrow with `path` or `glob` instead of with pattern syntax.",
                self.source,
                SIZE_LIMIT / 1024
            )
        } else {
            format!(
                "`{}` is not a valid regular expression. The engine is the Rust \
                 `regex` crate: literals, `.`, `*`, `+`, `?`, `{{n,m}}`, `[...]`, \
                 `(...)`, `|`, `^`, `$`, `\\b`, `\\w`, `\\d`, `\\s`, and inline flags \
                 like `(?i)`. It has NO backreferences and NO lookaround — if that \
                 is what you wrote, the question is structural and `outline` \
                 answers it directly.",
                self.source
            )
        }
    }
}

/// Compiled patterns, keyed by what was asked for.
///
/// The ladder in `grep` compiles the same source two to four times per call (bare
/// identifier, case-insensitive retry, widened scope), and a turn typically greps
/// for the same thing more than once. Compilation is the expensive half of a
/// search over a small tree, so it is worth not repeating; the cache is process-
/// wide because a `Pattern` is immutable and `Regex` is `Send + Sync`.
///
/// Bounded, and bounded by clearing rather than by an LRU: the entries are small,
/// the working set of a turn is a handful, and a wrong eviction costs a
/// recompile, not a wrong answer.
/// The key is everything that changes the compiled program: the source and the
/// case flag. Nothing else does — `multi_line` is set the same way for every
/// entry, and the two limits are constants.
type CacheKey = (String, bool);

static CACHE: LazyLock<Mutex<HashMap<CacheKey, Arc<CompiledPair>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const CACHE_CAP: usize = 256;

#[derive(Debug)]
struct CompiledPair {
    line: Arc<Regex>,
    multi: Arc<Regex>,
}

impl Pattern {
    /// Compile `source`, or say why it could not be.
    ///
    /// A pattern that does not compile is an **error the model is shown**, not a
    /// pattern quietly replaced by a literal one. Searching for something other
    /// than what was asked is the class of bug this crate exists to remove, and an
    /// approximation with a note attached is still that bug with a note attached.
    pub fn compile(source: &str, case_insensitive: bool) -> Result<Pattern, PatternError> {
        let key: CacheKey = (source.to_string(), case_insensitive);
        if let Some(hit) = CACHE.lock().ok().and_then(|c| c.get(&key).cloned()) {
            return Ok(Pattern {
                re: hit.line.clone(),
                multi: hit.multi.clone(),
                source: source.to_string(),
                case_insensitive,
            });
        }

        let line = RegexBuilder::new(source)
            .case_insensitive(case_insensitive)
            .size_limit(SIZE_LIMIT)
            .dfa_size_limit(DFA_SIZE_LIMIT)
            .build()
            .map_err(|e| classify(source, &e.to_string()))?;
        // The same pattern with `^` and `$` bound to line boundaries.
        // `line_hits` uses it to find candidate lines in one pass; `line` above is
        // what decides whether a candidate really matches, so the two never
        // disagree about the answer, only about how fast it is reached.
        let multi = RegexBuilder::new(source)
            .case_insensitive(case_insensitive)
            .multi_line(true)
            .size_limit(SIZE_LIMIT)
            .dfa_size_limit(DFA_SIZE_LIMIT)
            .build()
            .map_err(|e| classify(source, &e.to_string()))?;

        let pair = Arc::new(CompiledPair {
            line: Arc::new(line),
            multi: Arc::new(multi),
        });
        if let Ok(mut c) = CACHE.lock() {
            if c.len() >= CACHE_CAP {
                c.clear();
            }
            c.insert(key, pair.clone());
        }
        Ok(Pattern {
            re: pair.line.clone(),
            multi: pair.multi.clone(),
            source: source.to_string(),
            case_insensitive,
        })
    }

    /// Does this pattern match anywhere in the line?
    pub fn is_match(&self, line: &str) -> bool {
        self.re.is_match(line)
    }

    /// Where in the line, as byte offsets. `None` when it does not match.
    pub fn find(&self, line: &str) -> Option<(usize, usize)> {
        self.re.find(line).map(|m| (m.start(), m.end()))
    }

    /// The 1-based numbers of the lines of `text` that match, in order.
    ///
    /// # Why this is not `text.lines().filter(is_match)`
    ///
    /// Because that pays the engine's per-call setup 48,667 times over a 1.9 MB
    /// tree and never lets a literal prefilter skip a kilobyte at a time. This
    /// runs the multi-line form over the whole buffer, which memchr-skips to
    /// candidates, and confirms each candidate line with the single-line form.
    ///
    /// The confirmation is not belt-and-braces, it is the correctness of the
    /// thing. In multi-line mode `\s`, `.` under `(?s)` and negated classes can
    /// match a `\n` and so match ACROSS two lines — which per-line `grep`
    /// semantics never do. A candidate is therefore only ever a candidate, and
    /// the line-scoped regex is what decides. One hit per line, at most,
    /// matching what `grep` prints.
    pub fn line_hits(&self, text: &str, cap: usize, mut on_hit: impl FnMut(usize, &str)) -> bool {
        let bytes = text.as_bytes();
        // `line_start` is a byte offset that is always the first byte of a line,
        // which is what makes searching `&text[line_start..]` legitimate: `^` and
        // `\b` then see exactly the context they would have seen in the whole
        // buffer. Resuming mid-line would quietly change both.
        let mut line_start = 0usize;
        let mut line_no = 0usize;
        let mut found = 0usize;
        loop {
            // `line_start == len` means the buffer ended with a newline, and
            // `str::lines` yields no final empty line for that; `line_start >
            // len` means the last line had no newline and has been consumed.
            // Either way there is nothing left, and a zero-width pattern would
            // otherwise invent one more line here.
            if line_start >= bytes.len() {
                break;
            }
            let Some(m) = self.multi.find(&text[line_start..]) else {
                break;
            };
            let abs = line_start + m.start();
            // Walk forward to the line holding the match. Monotone in `abs`, so
            // the whole scan crosses the buffer's newlines once, not once per hit.
            loop {
                let end = memchr_nl(bytes, line_start).unwrap_or(bytes.len());
                line_no += 1;
                if abs <= end {
                    let line = &text[line_start..end];
                    if self.re.is_match(line) {
                        on_hit(line_no, line);
                        found += 1;
                        if found >= cap {
                            return true;
                        }
                    }
                    // Resume at the next line. The line-scoped regex has already
                    // answered for every position in this one, so nothing that
                    // could still have matched is skipped.
                    line_start = end + 1;
                    break;
                }
                line_start = end + 1;
            }
        }
        false
    }

    /// Is this pattern anything other than plain characters?
    pub fn has_syntax(&self) -> bool {
        self.source.chars().any(|c| {
            matches!(
                c,
                '^' | '$' | '\\' | '[' | ']' | '(' | ')' | '*' | '+' | '?' | '.' | '|' | '{' | '}'
            )
        })
    }
}

fn memchr_nl(bytes: &[u8], from: usize) -> Option<usize> {
    bytes[from..].iter().position(|b| *b == b'\n').map(|i| i + from)
}

/// Separate "this is not a regex" from "this regex is too big", because the two
/// have different remedies and the model needs to be told which one it hit.
fn classify(source: &str, message: &str) -> PatternError {
    PatternError {
        source: source.to_string(),
        message: message.to_string(),
        too_large: message.contains("exceeds size limit")
            || message.contains("Compiled regex exceeds"),
    }
}

/// Clause 1's third case: **the bare identifier inside a too-strict pattern.**
///
/// `^\s*fn parse_args\(` relaxes to `parse_args`; `\bTokenLedger\b` to
/// `TokenLedger`. The rule is the longest run of identifier characters, because
/// that is what the model was actually looking for and everything else it wrote
/// was a guess about the surrounding syntax — which is precisely the guess that
/// produced the empty result.
///
/// # Why this reads the AST and not the characters
///
/// It used to scan the source string, and that made it a second, worse parser:
/// `\bfoo` had to be special-cased so it did not yield `bfoo`, `\d` looked like
/// the identifier `d`, and `a{2,3}` yielded `a23`. Every one of those is a
/// question about the pattern's SYNTAX TREE being answered by looking at its
/// bytes. `regex_syntax::ast` already has the tree, so the rule can be stated
/// once and correctly: the longest run of adjacent literal identifier characters
/// in a concatenation, over the whole tree.
///
/// Two things are deliberately not descended into:
///
/// - **Alternations.** A literal inside one branch is one possibility out of
///   several; promoting it to "what the model was looking for" would search for a
///   term the model never committed to.
/// - **Character classes.** `[abc]` is three alternatives spelled compactly, for
///   the same reason.
///
/// `None` when the pattern is already bare, or does not parse, so the caller
/// cannot report a relaxation that did not happen.
pub fn bare_identifier(source: &str) -> Option<String> {
    let ast = regex_syntax::ast::parse::Parser::new().parse(source).ok()?;
    let mut best = String::new();
    longest_literal_run(&ast, &mut best);
    if best.is_empty() || best == source {
        None
    } else {
        Some(best)
    }
}

fn longest_literal_run(ast: &regex_syntax::ast::Ast, best: &mut String) {
    use regex_syntax::ast::Ast;
    match ast {
        Ast::Concat(c) => {
            let mut cur = String::new();
            for item in &c.asts {
                match item {
                    Ast::Literal(l) if is_word(l.c) => cur.push(l.c),
                    _ => {
                        take(&mut cur, best);
                        // A group or a repetition can still hold the identifier:
                        // `(?:pub )?fn (parse_args)` keeps it one level down.
                        longest_literal_run(item, best);
                    }
                }
            }
            take(&mut cur, best);
        }
        Ast::Group(g) => longest_literal_run(&g.ast, best),
        Ast::Repetition(r) => longest_literal_run(&r.ast, best),
        Ast::Literal(l) if is_word(l.c) => {
            let mut one = l.c.to_string();
            take(&mut one, best);
        }
        // Alternation and ClassBracketed are choices, not the thing asked for.
        _ => {}
    }
}

fn take(cur: &mut String, best: &mut String) {
    if cur.chars().count() > best.chars().count() {
        *best = cur.clone();
    }
    cur.clear();
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Path globbing: `*`, `?`, `**`, `[...]`, and `{a,b}` alternation.
///
/// `*` does not cross a `/`; `**` does. Separate from [`Pattern`] because a glob
/// is matched against a whole path and a regex against one line, and conflating
/// them is how `*` quietly becomes `.*`. Still hand-written, and for the reason
/// the regex matcher no longer is: a glob is matched once per PATH, not once per
/// line, so it has never appeared in a measurement, and translating globs to
/// regexes is a well-known source of quiet semantic drift at exactly the `/`
/// boundary this cares about.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    for p in expand_braces(pattern) {
        if glob_one(
            &p.chars().collect::<Vec<_>>(),
            &path.chars().collect::<Vec<_>>(),
        ) {
            return true;
        }
    }
    false
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_string()];
    };
    let Some(close) = pattern[open..].find('}').map(|i| i + open) else {
        return vec![pattern.to_string()];
    };
    let head = &pattern[..open];
    let tail = &pattern[close + 1..];
    let mut out = Vec::new();
    for alt in pattern[open + 1..close].split(',') {
        out.extend(expand_braces(&format!("{head}{alt}{tail}")));
    }
    out
}

fn glob_one(p: &[char], s: &[char]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    match p[0] {
        '*' => {
            // `**` crosses separators; `*` does not.
            let doubled = p.get(1) == Some(&'*');
            let rest = if doubled {
                // `**/` also matches zero directories.
                if p.get(2) == Some(&'/') && glob_one(&p[3..], s) {
                    return true;
                }
                &p[2..]
            } else {
                &p[1..]
            };
            for i in 0..=s.len() {
                if !doubled && s[..i].contains(&'/') {
                    break;
                }
                if glob_one(rest, &s[i..]) {
                    return true;
                }
            }
            false
        }
        '?' => !s.is_empty() && s[0] != '/' && glob_one(&p[1..], &s[1..]),
        '[' => {
            let Some(close) = p.iter().position(|c| *c == ']') else {
                return false;
            };
            if s.is_empty() {
                return false;
            }
            let (neg, body) = if p.get(1) == Some(&'^') || p.get(1) == Some(&'!') {
                (true, &p[2..close])
            } else {
                (false, &p[1..close])
            };
            let mut hit = false;
            let mut i = 0;
            while i < body.len() {
                if i + 2 < body.len() && body[i + 1] == '-' {
                    if (body[i]..=body[i + 2]).contains(&s[0]) {
                        hit = true;
                    }
                    i += 3;
                } else {
                    if body[i] == s[0] {
                        hit = true;
                    }
                    i += 1;
                }
            }
            hit != neg && glob_one(&p[close + 1..], &s[1..])
        }
        c => !s.is_empty() && s[0] == c && glob_one(&p[1..], &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(src: &str, ci: bool) -> Pattern {
        Pattern::compile(src, ci).expect("compiles")
    }

    #[test]
    fn the_subset_matches_what_it_says_it_does() {
        let pat = p(r"^\s*fn \w+\(", false);
        assert!(pat.is_match("    fn parse_args(a: u8) {"));
        assert!(!pat.is_match("let fn_name = 3;"));

        let pat = p(r"To(ken|ol)Ledger", false);
        assert!(pat.is_match("struct TokenLedger {"));
        assert!(pat.is_match("struct ToolLedger {"));
        assert!(!pat.is_match("struct TopLedger {"));

        let pat = p(r"\bTokenLedger\b", false);
        assert!(pat.is_match("the TokenLedger is"));
        assert!(!pat.is_match("MyTokenLedgers"));

        let pat = p("ERROR", true);
        assert!(pat.is_match("an error happened"));

        let pat = p(r"a.*z", false);
        assert!(pat.is_match("abcz"));
        assert!(!pat.is_match("zzza"));

        let pat = p(r"[A-Z][a-z]+", false);
        assert!(pat.is_match("Hello"));
        assert!(!pat.is_match("hello"));
    }

    /// The whole point of the swap: the constructs the old subset had to refuse
    /// are now ordinary. Counted repetition was the one it named out loud.
    #[test]
    fn what_the_old_subset_refused_now_simply_works() {
        let pat = p(r"ab{2,3}", false);
        assert!(pat.is_match("xabbz"));
        assert!(!pat.is_match("xabz"));

        // Non-greedy, which the old engine consumed and ignored.
        let pat = p(r"<(.+?)>", false);
        assert_eq!(pat.find("<a><b>"), Some((0, 3)));

        // Inline flags, which the old engine reported as unsupported.
        let pat = p(r"(?i)tokenledger", false);
        assert!(pat.is_match("struct TokenLedger {"));

        // Unicode classes, which the old engine had no notion of at all.
        let pat = p(r"\p{Greek}+", false);
        assert!(pat.is_match("let x = λαμβδα;"));
    }

    /// Clause 3 of the brief, and the reason the old fallback had to go: a
    /// pattern that does not compile is an ERROR, with the engine's own message
    /// naming the position. It is not a literal search wearing a note.
    #[test]
    fn a_pattern_that_does_not_compile_is_an_error_not_a_literal() {
        let err = Pattern::compile(r"a(b", false).expect_err("unbalanced paren");
        assert!(!err.too_large);
        assert!(
            err.message.contains("unclosed group") || err.message.contains("unopened"),
            "the engine's own words, not ours: {}",
            err.message
        );
        // The position, which is the part a model can act on.
        assert!(err.message.contains('^'), "{}", err.message);

        // Lookaround and backreferences: valid PCRE, refused here, and the remedy
        // names the tool that answers the question instead.
        let err = Pattern::compile(r"(?<=fn )\w+", false).expect_err("no lookaround");
        assert!(err.remedy().contains("outline"), "{}", err.remedy());
    }

    #[test]
    fn a_pathological_pattern_is_refused_rather_than_compiled() {
        // Nested counted repetition is the cheap way to a huge program. The
        // refusal is reported; the tool does not sit there building it.
        let src = r"((((a{100}){100}){100}){100})";
        let err = Pattern::compile(src, false).expect_err("over the size limit");
        assert!(err.too_large, "{}", err.message);
        assert!(err.remedy().contains("KiB"), "{}", err.remedy());
    }

    #[test]
    fn the_bare_identifier_is_what_the_model_was_looking_for() {
        assert_eq!(
            bare_identifier(r"^\s*fn parse_args\("),
            Some("parse_args".into())
        );
        assert_eq!(
            bare_identifier(r"\bTokenLedger\b"),
            Some("TokenLedger".into())
        );
        assert_eq!(bare_identifier("impl .*for Ledger"), Some("Ledger".into()));
        // Already bare: there is no relaxation to report.
        assert_eq!(bare_identifier("parse_args"), None);
    }

    /// The three the character scanner got wrong, and could only ever get wrong,
    /// because they are questions about the tree.
    #[test]
    fn the_ast_answers_what_the_character_scan_could_not() {
        // `\d` is a class. The old scan saw the letter `d` and, on a short
        // pattern, could return it as the identifier.
        assert_eq!(bare_identifier(r"\d+"), None);
        // Counted repetition: the old scan spliced the bounds into the run.
        assert_eq!(bare_identifier(r"Ledger{2,3}"), Some("Ledge".into()));
        // A group is not a barrier to the thing being looked for.
        assert_eq!(
            bare_identifier(r"(?:pub )?fn (parse_args)"),
            Some("parse_args".into())
        );
        // A pattern that does not parse has no tree, so it has no relaxation.
        assert_eq!(bare_identifier(r"a(b"), None);
    }

    /// `line_hits` and per-line `is_match` must agree on every line, including
    /// the ones where multi-line mode could match across a newline.
    #[test]
    fn scanning_a_buffer_agrees_with_scanning_its_lines() {
        let text = "fn a() {\n    let x = 1;\n}\nfn b() {\n}\nlast line no newline";
        for src in [
            r"fn \w+",
            r"^fn ",
            r"^\s*let",
            r"\}$",
            r"line",
            r"^$",
            // The one that separates the two modes: `\s*` will happily eat a
            // newline over a buffer, and never does over a line.
            r"\{\s*\}",
            r"1;\s*\}",
        ] {
            let pat = p(src, false);
            let want: Vec<usize> = text
                .lines()
                .enumerate()
                .filter(|(_, l)| pat.is_match(l))
                .map(|(i, _)| i + 1)
                .collect();
            let mut got = Vec::new();
            pat.line_hits(text, usize::MAX, |n, _| got.push(n));
            assert_eq!(got, want, "pattern `{src}` disagreed over the buffer");
        }
    }

    #[test]
    fn line_hits_stops_at_the_cap_and_says_so() {
        let text = "a\na\na\na\na\n";
        let pat = p("a", false);
        let mut got = Vec::new();
        let truncated = pat.line_hits(text, 2, |n, _| got.push(n));
        assert!(truncated);
        assert_eq!(got, vec![1, 2]);
    }

    #[test]
    fn compiling_the_same_pattern_twice_reuses_the_program() {
        let a = Pattern::compile(r"cache_hit_probe_\w+", false).expect("compiles");
        let b = Pattern::compile(r"cache_hit_probe_\w+", false).expect("compiles");
        assert!(
            Arc::ptr_eq(&a.re, &b.re),
            "the ladder compiles the same source several times per call"
        );
        // Case sensitivity is part of the key: these are different programs.
        let c = Pattern::compile(r"cache_hit_probe_\w+", true).expect("compiles");
        assert!(!Arc::ptr_eq(&a.re, &c.re));
    }

    #[test]
    fn globs_respect_the_separator() {
        assert!(glob_match("src/*.rs", "src/lib.rs"));
        assert!(!glob_match("src/*.rs", "src/a/lib.rs"));
        assert!(glob_match("src/**/*.rs", "src/a/b/lib.rs"));
        assert!(glob_match("**/*.rs", "src/lib.rs"));
        assert!(glob_match("src/**/*.rs", "src/lib.rs"));
        assert!(glob_match("*.{rs,toml}", "Cargo.toml"));
        assert!(!glob_match("*.{rs,toml}", "README.md"));
        assert!(glob_match("src/lib.?s", "src/lib.rs"));
    }
}
