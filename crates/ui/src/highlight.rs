//! Syntax colouring for fenced code, built for a fence that is still open.
//!
//! # Why not `syntect`
//!
//! `syntect` is the answer to "highlight a file". It is the wrong answer to
//! "highlight the 1,400 bytes of a Rust function that are arriving four
//! characters at a time, while the closing fence has not been written yet".
//! It wants a whole buffer, a theme file and an Oniguruma regex engine; its
//! `ClassedHTMLGenerator`-style incremental use is line-at-a-time but the
//! `ParseState` is heavy and the syntax set is a 2 MB binary blob. For a head
//! that is a lot of machinery to colour six token classes.
//!
//! # The property that actually matters
//!
//! §13.3 requires render cost independent of accumulated output length.
//! `letibot-tui`'s markdown lexer earns that with a frozen stable prefix; a
//! highlighter that re-scans the whole code block on every delta gives it
//! straight back — and a code block is the *longest* thing a coding model emits,
//! so it is the worst place to lose it.
//!
//! The mechanism here is the same shape, and it is cheaper than the markdown one
//! because code has a boundary markdown does not: **a newline**. A line-oriented
//! lexer carrying one small [`State`] across the boundary has the property
//!
//! ```text
//! highlight(state, line₁ ++ line₂) == highlight(state, line₁) ++ highlight(state', line₂)
//! ```
//!
//! by construction, where `state'` is the state after line₁. So a complete line
//! is highlighted exactly once, ever, and only the incomplete final line is
//! redone per frame — and that line is bounded by the terminal width, not by the
//! message. [`StreamingCode::bytes_highlighted`] is the instrumentation that
//! says so, kept in the shipping type for the same reason
//! `IncrementalMarkdown::bytes_lexed` is: "did the renderer go quadratic again"
//! is asked once a year and is unanswerable after the fact.
//!
//! # What it knows, and what it does not
//!
//! Six token classes — keyword, type, function name, string, number, comment —
//! for the languages a coding assistant actually emits. It is a lexer, not a
//! parser: it has no symbol table, so `Vec` is a type because it starts with a
//! capital letter, and `foo` is a function because a `(` follows it. Both
//! heuristics are wrong sometimes and the cost of being wrong is a word in the
//! wrong colour.
//!
//! # Provenance
//!
//! The resumable-state mechanism below is the one **grok-build** (xAI,
//! Apache-2.0) arrived at in
//! `crates/codegen/xai-grok-markdown/src/open_code_highlighter.rs`, and their
//! module header carries the measurement that justifies it: before the fix,
//! *"every push re-ran syntect over the whole growing block: O(N²) over the
//! stream (~35 ms/push near the end of a ~1000-line block)"*, and a memoisation
//! miss on a closed fence trapped in an open list *"cost ~50 to 100 ms; one
//! recorded UI freeze hit 4.5 s"*. That is the evidence this file is built on
//! and it is theirs, so it is cited rather than restated as if we had measured
//! it.
//!
//! What is the same: persist the lexer state **after the last
//! newline-terminated line**, keep the already-painted lines, and repaint only
//! the incomplete tail against a copy of that state.
//!
//! What is different, and why:
//!
//! - They wrap `syntect`, so their state is syntect's `ParseState` +
//!   `HighlightState` and a theme file. This is a hand-written lexer and the
//!   state is two fields ([`State`]), which is what makes it `Copy` and
//!   therefore free to clone per frame for the tail.
//! - They need a **second** strategy — a `(fence_info, body)` memo with a
//!   256 KiB cap — for a closed fence trapped inside an open list, because
//!   pulldown-cmark cannot checkpoint inside a list. letibot's markdown lexer
//!   (`letibot_tui::markdown`) hands a code block's body to this type directly
//!   rather than re-parsing containing blocks, so that case does not arise and
//!   there is no memo here.
//! - They discard state on a theme change; [`Palette`] is fixed for the life of
//!   a [`StreamingCode`] here, so a palette change is a new block.
//!
//! It does **not** know: nested-language embedding (JS inside HTML), Rust
//! multi-line string literals (a `"` unterminated at end of line is treated as
//! closed), or any macro or preprocessor structure. Each of those would need a
//! parser, and a parser is the point where this file should be deleted in favour
//! of `syntect` or a tree-sitter grammar rather than extended.

use crate::style::{Painter, Palette, Role};

/// A language's lexical surface: the minimum needed to colour it.
#[derive(Debug, Clone, Copy)]
pub struct Syntax {
    pub name: &'static str,
    keywords: &'static [&'static str],
    /// Words that are types rather than keywords. Empty for languages where the
    /// capital-letter heuristic is enough.
    types: &'static [&'static str],
    line_comment: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// Quote characters that begin a single-line string.
    quotes: &'static [char],
    /// Multi-line string delimiters, opened and closed by the same token.
    multiline: &'static [&'static str],
}

const RUST_KW: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "union",
];
const RUST_TY: &[&str] = &[
    "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "str", "u8", "u16",
    "u32", "u64", "u128", "usize",
];
const PY_KW: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "False", "try", "while",
    "with", "yield", "match", "case",
];
const JS_KW: &[&str] = &[
    "as", "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger",
    "default", "delete", "do", "else", "enum", "export", "extends", "false", "finally", "for",
    "from", "function", "if", "implements", "import", "in", "instanceof", "interface", "let",
    "new", "null", "of", "return", "satisfies", "static", "super", "switch", "this", "throw",
    "true", "try", "type", "typeof", "undefined", "var", "void", "while", "yield",
];
const JS_TY: &[&str] = &[
    "any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "unknown",
];
const GO_KW: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for",
    "func", "go", "goto", "if", "import", "interface", "map", "package", "range", "return",
    "select", "struct", "switch", "type", "var", "nil", "true", "false",
];
const GO_TY: &[&str] = &[
    "bool", "byte", "complex64", "complex128", "error", "float32", "float64", "int", "int8",
    "int16", "int32", "int64", "rune", "string", "uint", "uint8", "uint16", "uint32", "uint64",
    "uintptr",
];
const C_KW: &[&str] = &[
    "auto", "bool", "break", "case", "catch", "char", "class", "const", "constexpr", "continue",
    "default", "delete", "do", "double", "else", "enum", "extern", "false", "float", "for",
    "goto", "if", "inline", "int", "long", "namespace", "new", "nullptr", "operator", "private",
    "protected", "public", "register", "return", "short", "signed", "sizeof", "static", "struct",
    "switch", "template", "this", "throw", "true", "try", "typedef", "typename", "union",
    "unsigned", "using", "virtual", "void", "volatile", "while",
];
const SH_KW: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "exit", "export", "fi", "for", "function", "if",
    "in", "local", "return", "set", "then", "unset", "until", "while",
];
const SQL_KW: &[&str] = &[
    "AND", "AS", "ASC", "BY", "CREATE", "DELETE", "DESC", "DISTINCT", "DROP", "FROM", "GROUP",
    "HAVING", "INDEX", "INNER", "INSERT", "INTO", "JOIN", "LEFT", "LIMIT", "NOT", "NULL", "ON",
    "OR", "ORDER", "SELECT", "SET", "TABLE", "UPDATE", "VALUES", "WHERE", "WITH",
];
const JSON_KW: &[&str] = &["true", "false", "null"];
const YAML_KW: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];

const RUST: Syntax = Syntax {
    name: "rust",
    keywords: RUST_KW,
    types: RUST_TY,
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    multiline: &[],
};
const PYTHON: Syntax = Syntax {
    name: "python",
    keywords: PY_KW,
    types: &[],
    line_comment: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    multiline: &["\"\"\"", "'''"],
};
const JS: Syntax = Syntax {
    name: "typescript",
    keywords: JS_KW,
    types: JS_TY,
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    multiline: &["`"],
};
const GO: Syntax = Syntax {
    name: "go",
    keywords: GO_KW,
    types: GO_TY,
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    multiline: &["`"],
};
const C: Syntax = Syntax {
    name: "c",
    keywords: C_KW,
    types: &[],
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    multiline: &[],
};
const SHELL: Syntax = Syntax {
    name: "shell",
    keywords: SH_KW,
    types: &[],
    line_comment: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    multiline: &[],
};
const SQL: Syntax = Syntax {
    name: "sql",
    keywords: SQL_KW,
    types: &[],
    line_comment: &["--"],
    block_comment: Some(("/*", "*/")),
    quotes: &['\''],
    multiline: &[],
};
const JSON: Syntax = Syntax {
    name: "json",
    keywords: JSON_KW,
    types: &[],
    line_comment: &[],
    block_comment: None,
    quotes: &['"'],
    multiline: &[],
};
const YAML: Syntax = Syntax {
    name: "yaml",
    keywords: YAML_KW,
    types: &[],
    line_comment: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    multiline: &[],
};
const TOML: Syntax = Syntax {
    name: "toml",
    keywords: &["true", "false"],
    types: &[],
    line_comment: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    multiline: &["\"\"\"", "'''"],
};

/// Resolve a fence's info string to a syntax.
///
/// The info string is whatever the model typed after the backticks, so it is
/// lower-cased and the common aliases are accepted. An unknown language returns
/// `None`, and the caller renders the block uncoloured — which is right: a wrong
/// colouring of an unknown language is worse than none, because it invites the
/// reader to trust it.
pub fn syntax_for(lang: &str) -> Option<&'static Syntax> {
    let l = lang.trim().to_ascii_lowercase();
    let l = l.split_whitespace().next().unwrap_or("");
    Some(match l {
        "rust" | "rs" => &RUST,
        "python" | "py" | "python3" => &PYTHON,
        "js" | "javascript" | "jsx" | "ts" | "typescript" | "tsx" | "mjs" | "cjs" => &JS,
        "go" | "golang" => &GO,
        "c" | "h" | "cpp" | "c++" | "cc" | "hpp" | "java" | "cs" | "csharp" | "kotlin" | "kt"
        | "swift" | "scala" | "zig" => &C,
        "sh" | "bash" | "zsh" | "shell" | "console" | "fish" => &SHELL,
        "sql" | "postgres" | "postgresql" | "sqlite" | "mysql" => &SQL,
        "json" | "jsonc" | "json5" => &JSON,
        "yaml" | "yml" => &YAML,
        "toml" | "ini" | "cfg" => &TOML,
        _ => return None,
    })
}

/// What a line needs to know about the lines before it. Small on purpose: it is
/// cloned per frame to highlight the incomplete tail without committing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    in_block_comment: bool,
    /// Index into `Syntax::multiline` of the delimiter currently open.
    in_multiline: Option<u8>,
}

/// Highlight one line, given the state left by the previous one.
///
/// Returns the painted line and the state for the next. Pure: the same
/// `(state, line)` always gives the same output, which is what lets a cache key
/// on line number alone.
pub fn line(syn: &Syntax, st: State, src: &str, p: Painter) -> (String, State) {
    let mut out = String::with_capacity(src.len() + 32);
    let mut st = st;
    let b = src.as_bytes();
    let mut i = 0usize;

    // Continuations first: a line that starts inside a comment or a multi-line
    // string is that thing until its terminator.
    if st.in_block_comment {
        let (open_, close) = syn.block_comment.expect("state set without a delimiter");
        let _ = open_;
        match src.find(close) {
            Some(k) => {
                let end = k + close.len();
                push(&mut out, p, Role::Comment, &src[..end]);
                st.in_block_comment = false;
                i = end;
            }
            None => {
                push(&mut out, p, Role::Comment, src);
                return (out, st);
            }
        }
    } else if let Some(idx) = st.in_multiline {
        let close = syn.multiline[idx as usize];
        match src.find(close) {
            Some(k) => {
                let end = k + close.len();
                push(&mut out, p, Role::StringLit, &src[..end]);
                st.in_multiline = None;
                i = end;
            }
            None => {
                push(&mut out, p, Role::StringLit, src);
                return (out, st);
            }
        }
    }

    while i < b.len() {
        let rest = &src[i..];
        let c = match rest.chars().next() {
            Some(c) => c,
            None => break,
        };

        // Comments.
        if let Some(lc) = syn.line_comment.iter().find(|lc| rest.starts_with(**lc)) {
            let _ = lc;
            push(&mut out, p, Role::Comment, rest);
            return (out, st);
        }
        if let Some((open_, close)) = syn.block_comment
            && rest.starts_with(open_)
        {
            match rest[open_.len()..].find(close) {
                Some(k) => {
                    let end = open_.len() + k + close.len();
                    push(&mut out, p, Role::Comment, &rest[..end]);
                    i += end;
                }
                None => {
                    push(&mut out, p, Role::Comment, rest);
                    st.in_block_comment = true;
                    return (out, st);
                }
            }
            continue;
        }

        // Multi-line strings, checked before single-line quotes so `"""` wins
        // over `"`.
        if let Some((idx, delim)) = syn
            .multiline
            .iter()
            .enumerate()
            .find(|(_, d)| rest.starts_with(**d))
        {
            match rest[delim.len()..].find(delim) {
                Some(k) => {
                    let end = delim.len() + k + delim.len();
                    push(&mut out, p, Role::StringLit, &rest[..end]);
                    i += end;
                }
                None => {
                    push(&mut out, p, Role::StringLit, rest);
                    st.in_multiline = Some(idx as u8);
                    return (out, st);
                }
            }
            continue;
        }

        // Single-line strings. An unterminated one ends at the newline: this is
        // wrong for a Rust literal spanning lines and right for the far more
        // common case of an apostrophe in a shell comment-free line. See the
        // module header.
        if syn.quotes.contains(&c) {
            let end = string_end(rest, c);
            push(&mut out, p, Role::StringLit, &rest[..end]);
            i += end;
            continue;
        }

        // Numbers. Leading digit only: `x2` is an identifier.
        if c.is_ascii_digit() {
            let end = rest
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_'))
                .unwrap_or(rest.len());
            push(&mut out, p, Role::NumberLit, &rest[..end]);
            i += end;
            continue;
        }

        // Identifiers.
        if c.is_alphabetic() || c == '_' {
            let end = rest
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .unwrap_or(rest.len());
            let word = &rest[..end];
            let after = rest[end..].trim_start();
            let role = if syn.keywords.contains(&word) {
                Role::Keyword
            } else if syn.types.contains(&word) || word.starts_with(|ch: char| ch.is_uppercase()) {
                // A named primitive, or the capital-letter heuristic. Same
                // answer, and pretending they are two rules costs a branch and
                // buys nothing.
                Role::TypeName
            } else if after.starts_with('(') {
                Role::FuncName
            } else {
                Role::Plain
            };
            push(&mut out, p, role, word);
            i += end;
            continue;
        }

        // Anything else is punctuation and stays plain. Consumed in a run so a
        // line of operators is not one paint call per character.
        let end = rest
            .find(|ch: char| {
                ch.is_alphanumeric() || ch == '_' || syn.quotes.contains(&ch) || ch == '\x1b'
            })
            .unwrap_or(rest.len())
            .max(c.len_utf8());
        out.push_str(&rest[..end]);
        i += end;
    }
    (out, st)
}

fn push(out: &mut String, p: Painter, r: Role, s: &str) {
    if s.is_empty() {
        return;
    }
    let o = p.open(r);
    if o.is_empty() {
        out.push_str(s);
    } else {
        out.push_str(o);
        out.push_str(s);
        // Not `RESET`. A fenced block inside the model's reasoning is painted
        // inside a themed block, and a keyword that closed to the terminal
        // default took the rest of the line with it.
        out.push_str(&p.close());
    }
}

/// Bytes up to and including the closing quote, or the end of the line.
fn string_end(rest: &str, q: char) -> usize {
    let mut it = rest.char_indices();
    it.next();
    let mut escaped = false;
    for (k, ch) in it {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == q {
            return k + ch.len_utf8();
        }
    }
    rest.len()
}

/// A fenced code block that grows at the end, highlighted once per line.
///
/// This is the type a head holds per open fence. It mirrors
/// `letibot_tui::markdown::IncrementalMarkdown` deliberately: one mutator
/// ([`push`](StreamingCode::push)), a frozen prefix that is never recomputed,
/// and a counter that proves it.
#[derive(Debug)]
pub struct StreamingCode {
    syn: Option<&'static Syntax>,
    painter: Painter,
    state: State,
    /// Complete lines, already painted. Never revisited.
    done: Vec<String>,
    /// The line still arriving, raw.
    partial: String,
    bytes_highlighted: u64,
}

impl StreamingCode {
    /// `lang` is the fence's info string; an unknown one gives an uncoloured
    /// block rather than a guessed one.
    pub fn new(lang: &str, palette: Palette) -> StreamingCode {
        StreamingCode::inside(lang, Painter::new(palette))
    }

    /// The same, for a block that is itself inside a themed block — the model's
    /// reasoning being the one that exists. Every span the highlighter closes
    /// then restores that theme instead of the terminal default.
    pub fn inside(lang: &str, painter: Painter) -> StreamingCode {
        StreamingCode {
            syn: syntax_for(lang),
            painter,
            state: State::default(),
            done: Vec::new(),
            partial: String::new(),
            bytes_highlighted: 0,
        }
    }

    /// The language actually resolved, for the block's title bar. `None` when
    /// the fence carried no info string or an unrecognised one.
    pub fn language(&self) -> Option<&'static str> {
        self.syn.map(|s| s.name)
    }

    /// Append raw code. Complete lines are highlighted and frozen here; the
    /// incomplete tail is kept raw and painted on demand.
    pub fn push(&mut self, delta: &str) {
        self.partial.push_str(delta);
        while let Some(nl) = self.partial.find('\n') {
            let l: String = self.partial[..nl].to_string();
            self.partial.drain(..nl + 1);
            self.freeze(&l);
        }
    }

    fn freeze(&mut self, l: &str) {
        self.bytes_highlighted += l.len() as u64;
        match self.syn {
            Some(s) => {
                let (painted, st) = line(s, self.state, l, self.painter);
                self.state = st;
                self.done.push(painted);
            }
            None => self.done.push(l.to_string()),
        }
    }

    /// Every line, painted. The frozen prefix is cloned; only the tail is
    /// highlighted, and only against a *copy* of the state, so nothing is
    /// committed until the newline arrives.
    pub fn lines(&mut self) -> Vec<String> {
        let mut out = self.done.clone();
        if !self.partial.is_empty() {
            self.bytes_highlighted += self.partial.len() as u64;
            match self.syn {
                Some(s) => {
                    let (painted, _) = line(s, self.state, &self.partial, self.painter);
                    out.push(painted);
                }
                None => out.push(self.partial.clone()),
            }
        }
        out
    }

    /// How many complete lines are frozen. A caller that keeps its own rendered
    /// buffer appends exactly the new ones.
    pub fn frozen_lines(&self) -> usize {
        self.done.len()
    }

    /// Total bytes handed to the lexer over this block's life. The number a
    /// regression test watches: a full re-highlight per delta makes it
    /// quadratic.
    pub fn bytes_highlighted(&self) -> u64 {
        self.bytes_highlighted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_SRC: &str = "\
/// Doc comment.
pub fn parse(input: &str) -> Result<Vec<u8>, Error> {
    let n = 42; // the answer
    let s = \"a string with a } brace\";
    /* a block
       comment */
    Ok(vec![n])
}
";

    fn stream(src: &str, chunk: usize) -> StreamingCode {
        let mut sc = StreamingCode::new("rust", Palette::Colour);
        let mut buf = String::new();
        for c in src.chars() {
            buf.push(c);
            if buf.chars().count() >= chunk {
                sc.push(&buf);
                buf.clear();
                let _ = sc.lines();
            }
        }
        if !buf.is_empty() {
            sc.push(&buf);
        }
        sc
    }

    #[test]
    fn painting_never_changes_the_visible_text() {
        // The invariant that makes a highlighter safe to put under a wrapper:
        // strip the escapes and you must have the source back.
        for (lang, src) in [
            ("rust", RUST_SRC),
            ("python", "def f(x):\n    return '''a\nb'''\n"),
            ("shell", "for f in *.rs; do echo \"$f\" # note\ndone\n"),
            ("json", "{\"a\": 1, \"b\": [true, null]}\n"),
        ] {
            let mut sc = StreamingCode::new(lang, Palette::Colour);
            sc.push(src);
            let painted = sc.lines().join("\n");
            let stripped: String = crate::width::cells(&painted)
                .iter()
                .map(|c| c.text)
                .collect();
            assert_eq!(stripped, src.trim_end_matches('\n'), "{lang}");
        }
    }

    #[test]
    fn a_block_comment_spanning_lines_stays_a_comment() {
        let mut sc = StreamingCode::new("rust", Palette::Colour);
        sc.push("/* one\ntwo\nthree */ let x = 1;\n");
        let ls = sc.lines();
        let comment = Palette::Colour.open(Role::Comment);
        assert!(ls[1].starts_with(comment), "{:?}", ls[1]);
        assert!(ls[2].contains(Palette::Colour.open(Role::Keyword)), "{:?}", ls[2]);
    }

    #[test]
    fn an_unterminated_fence_highlights_what_has_arrived() {
        let mut sc = StreamingCode::new("rust", Palette::Colour);
        sc.push("fn main() {\n    let x = ");
        let ls = sc.lines();
        assert_eq!(ls.len(), 2);
        assert!(ls[1].contains(Palette::Colour.open(Role::Keyword)));
    }

    #[test]
    fn a_complete_line_is_highlighted_exactly_once() {
        // §13.3, one layer down. Streaming a block in 3-byte deltas must not
        // cost more than a constant factor over highlighting it whole, and the
        // constant comes only from repainting the incomplete tail.
        let doc = RUST_SRC.repeat(60);
        let sc = stream(&doc, 3);
        let n = doc.len() as u64;
        let naive = n * n / (2 * 3);
        assert!(
            sc.bytes_highlighted() < naive / 50,
            "highlighted {} bytes; a full repaint per delta is about {naive}",
            sc.bytes_highlighted()
        );
        // And linear in the input, not merely sub-quadratic.
        let small = stream(&RUST_SRC.repeat(30), 3).bytes_highlighted();
        let large = stream(&RUST_SRC.repeat(60), 3).bytes_highlighted();
        assert!(
            large < small * 3,
            "doubling the block multiplied the work by {:.1}",
            large as f64 / small as f64
        );
    }

    #[test]
    fn streaming_gives_the_same_output_as_one_shot() {
        for chunk in [1usize, 5, 64] {
            let streamed = stream(RUST_SRC, chunk).lines();
            let mut whole = StreamingCode::new("rust", Palette::Colour);
            whole.push(RUST_SRC);
            assert_eq!(streamed, whole.lines(), "chunk {chunk}");
        }
    }

    #[test]
    fn an_unknown_language_is_left_alone() {
        let mut sc = StreamingCode::new("brainfuck", Palette::Colour);
        sc.push("+[->+<]\n");
        assert_eq!(sc.lines(), vec!["+[->+<]".to_string()]);
        assert_eq!(sc.language(), None);
    }

    #[test]
    fn the_none_palette_returns_the_source_verbatim() {
        let mut sc = StreamingCode::new("rust", Palette::None);
        sc.push(RUST_SRC);
        assert_eq!(sc.lines().join("\n"), RUST_SRC.trim_end_matches('\n'));
    }
}
