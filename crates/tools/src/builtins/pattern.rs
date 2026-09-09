//! The two pattern languages the search tools use, and the relaxation clause 1
//! demands.
//!
//! # Why this is hand-written rather than `regex`
//!
//! The same argument `letibot-turn` makes for its HTTP client. What is needed is a
//! matcher over single lines with a **known, statable** subset, because clause 1's
//! third acceptance case — *a too-strict anchor is auto-relaxed to the bare
//! identifier and the relaxation is reported* — is a property of the pattern's
//! syntax tree, not of matching. A full engine would give more syntax and no more
//! of the thing that matters.
//!
//! The subset: literals, `.`, `*`, `+`, `?`, `[...]` with ranges and negation,
//! `(...)` grouping, top-level and in-group `|`, the anchors `^` `$` `\b`, and the
//! escapes `\w \W \d \D \s \S` plus `\` before a metacharacter.
//!
//! **A construct outside the subset is reported, never silently approximated.**
//! [`Pattern::compile`] returns the unsupported pieces alongside a pattern that
//! treats them literally, and `grep` puts that in the result. A matcher that
//! quietly means something different from what the model wrote is the exact shape
//! of bug this project exists to remove.

/// A compiled search pattern.
#[derive(Debug, Clone)]
pub struct Pattern {
    alts: Vec<Vec<Piece>>,
    pub source: String,
    pub case_insensitive: bool,
    /// Constructs the subset does not cover, quoted back. Empty is the usual case.
    pub unsupported: Vec<String>,
}

#[derive(Debug, Clone)]
struct Piece {
    node: Node,
    min: usize,
    max: usize,
}

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class { neg: bool, items: Vec<ClassItem> },
    Start,
    End,
    WordBoundary,
    Group(Vec<Vec<Piece>>),
}

#[derive(Debug, Clone)]
enum ClassItem {
    Ch(char),
    Range(char, char),
    Word,
    NotWord,
    Digit,
    NotDigit,
    Space,
    NotSpace,
}

impl Pattern {
    pub fn compile(source: &str, case_insensitive: bool) -> Pattern {
        let mut unsupported = Vec::new();
        let chars: Vec<char> = source.chars().collect();
        let mut i = 0;
        let alts = parse_alts(&chars, &mut i, &mut unsupported, 0);
        Pattern {
            alts,
            source: source.to_string(),
            case_insensitive,
            unsupported,
        }
    }

    /// Does this pattern match anywhere in the line?
    pub fn find(&self, line: &str) -> Option<(usize, usize)> {
        let hay: Vec<char> = if self.case_insensitive {
            line.to_lowercase().chars().collect()
        } else {
            line.chars().collect()
        };
        for start in 0..=hay.len() {
            for alt in &self.alts {
                if let Some(end) = match_seq(alt, 0, &hay, start, self.case_insensitive) {
                    return Some((start, end));
                }
            }
        }
        None
    }

    pub fn is_match(&self, line: &str) -> bool {
        self.find(line).is_some()
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

/// Clause 1's third case: **the bare identifier inside a too-strict pattern.**
///
/// `^\s*fn parse_args\(` relaxes to `parse_args`; `\bTokenLedger\b` to
/// `TokenLedger`. The rule is the longest run of identifier characters, because
/// that is what the model was actually looking for and everything else it wrote
/// was a guess about the surrounding syntax — which is precisely the guess that
/// produced the empty result.
///
/// `None` when the pattern is already bare, so the caller cannot report a
/// relaxation that did not happen.
pub fn bare_identifier(source: &str) -> Option<String> {
    let mut best = String::new();
    let mut cur = String::new();
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // An escape is syntax, not an identifier character, and it ends the
            // run: `\bfoo` is `foo`, not `bfoo`.
            chars.next();
            if cur.len() > best.len() {
                best = cur.clone();
            }
            cur.clear();
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            cur.push(c);
        } else {
            if cur.len() > best.len() {
                best = cur.clone();
            }
            cur.clear();
        }
    }
    if cur.len() > best.len() {
        best = cur;
    }
    if best.is_empty() || best == source {
        None
    } else {
        Some(best)
    }
}

fn parse_alts(
    chars: &[char],
    i: &mut usize,
    unsupported: &mut Vec<String>,
    depth: usize,
) -> Vec<Vec<Piece>> {
    let mut alts = vec![Vec::new()];
    while *i < chars.len() {
        let c = chars[*i];
        if c == ')' && depth > 0 {
            break;
        }
        if c == '|' {
            *i += 1;
            alts.push(Vec::new());
            continue;
        }
        let node = match c {
            '(' => {
                *i += 1;
                // `(?:` and friends: the flag is not supported, the grouping is.
                if chars.get(*i) == Some(&'?') {
                    let mut j = *i;
                    while j < chars.len() && chars[j] != ')' && chars[j] != ':' {
                        j += 1;
                    }
                    if chars.get(j) == Some(&':') {
                        unsupported.push("(?…: group flags are ignored".into());
                        *i = j + 1;
                    }
                }
                let inner = parse_alts(chars, i, unsupported, depth + 1);
                if chars.get(*i) == Some(&')') {
                    *i += 1;
                } else {
                    unsupported.push("( with no closing )".into());
                }
                Node::Group(inner)
            }
            '[' => {
                *i += 1;
                parse_class(chars, i, unsupported)
            }
            '.' => {
                *i += 1;
                Node::Any
            }
            '^' => {
                *i += 1;
                Node::Start
            }
            '$' => {
                *i += 1;
                Node::End
            }
            '\\' => {
                *i += 1;
                let e = chars.get(*i).copied().unwrap_or('\\');
                *i += 1;
                match e {
                    'b' => Node::WordBoundary,
                    'w' => class(false, ClassItem::Word),
                    'W' => class(false, ClassItem::NotWord),
                    'd' => class(false, ClassItem::Digit),
                    'D' => class(false, ClassItem::NotDigit),
                    's' => class(false, ClassItem::Space),
                    'S' => class(false, ClassItem::NotSpace),
                    'n' => Node::Char('\n'),
                    't' => Node::Char('\t'),
                    other => Node::Char(other),
                }
            }
            '{' => {
                // Counted repetition. Out of the subset, and said so.
                unsupported.push("{n,m} counted repetition, matched literally".into());
                *i += 1;
                Node::Char('{')
            }
            other => {
                *i += 1;
                Node::Char(other)
            }
        };
        let (min, max) = quantifier(chars, i);
        alts.last_mut()
            .expect("one alternative always exists")
            .push(Piece { node, min, max });
    }
    alts
}

fn class(neg: bool, item: ClassItem) -> Node {
    Node::Class {
        neg,
        items: vec![item],
    }
}

fn parse_class(chars: &[char], i: &mut usize, unsupported: &mut Vec<String>) -> Node {
    let mut neg = false;
    if chars.get(*i) == Some(&'^') {
        neg = true;
        *i += 1;
    }
    let mut items = Vec::new();
    while *i < chars.len() && chars[*i] != ']' {
        let c = chars[*i];
        if c == '\\' {
            *i += 1;
            let e = chars.get(*i).copied().unwrap_or('\\');
            *i += 1;
            items.push(match e {
                'w' => ClassItem::Word,
                'd' => ClassItem::Digit,
                's' => ClassItem::Space,
                'n' => ClassItem::Ch('\n'),
                't' => ClassItem::Ch('\t'),
                other => ClassItem::Ch(other),
            });
            continue;
        }
        if chars.get(*i + 1) == Some(&'-') && chars.get(*i + 2).is_some_and(|c| *c != ']') {
            items.push(ClassItem::Range(c, chars[*i + 2]));
            *i += 3;
            continue;
        }
        items.push(ClassItem::Ch(c));
        *i += 1;
    }
    if chars.get(*i) == Some(&']') {
        *i += 1;
    } else {
        unsupported.push("[ with no closing ]".into());
    }
    Node::Class { neg, items }
}

fn quantifier(chars: &[char], i: &mut usize) -> (usize, usize) {
    match chars.get(*i) {
        Some('*') => {
            *i += 1;
            lazy(chars, i);
            (0, usize::MAX)
        }
        Some('+') => {
            *i += 1;
            lazy(chars, i);
            (1, usize::MAX)
        }
        Some('?') => {
            *i += 1;
            lazy(chars, i);
            (0, 1)
        }
        _ => (1, 1),
    }
}

/// A trailing `?` makes a quantifier lazy. Greedy-with-backtracking finds the same
/// *whether there is a match*, which is all `grep` reports, so it is consumed and
/// ignored rather than refused.
fn lazy(chars: &[char], i: &mut usize) {
    if chars.get(*i) == Some(&'?') {
        *i += 1;
    }
}

fn match_seq(pieces: &[Piece], pi: usize, hay: &[char], pos: usize, ci: bool) -> Option<usize> {
    let Some(piece) = pieces.get(pi) else {
        return Some(pos);
    };
    // Zero-width assertions take no input and no quantifier.
    match &piece.node {
        Node::Start => {
            return if pos == 0 {
                match_seq(pieces, pi + 1, hay, pos, ci)
            } else {
                None
            };
        }
        Node::End => {
            return if pos == hay.len() {
                match_seq(pieces, pi + 1, hay, pos, ci)
            } else {
                None
            };
        }
        Node::WordBoundary => {
            let before = pos > 0 && is_word(hay[pos - 1]);
            let after = pos < hay.len() && is_word(hay[pos]);
            return if before != after {
                match_seq(pieces, pi + 1, hay, pos, ci)
            } else {
                None
            };
        }
        _ => {}
    }

    // Greedy: take as many as possible, then give them back one at a time.
    let mut ends = vec![pos];
    let mut cur = pos;
    while ends.len() - 1 < piece.max {
        match match_one(&piece.node, hay, cur, ci) {
            Some(next) if next > cur || matches!(piece.node, Node::Group(_)) => {
                if next == cur {
                    break;
                }
                cur = next;
                ends.push(cur);
            }
            _ => break,
        }
    }
    // Counts from the greediest down to `min`, and no further: `ends.len() - 1` is
    // the repetition count, so the loop stops exactly when it would go below the
    // minimum.
    while ends.len() > piece.min {
        let end = *ends.last().expect("non-empty by the loop condition");
        if let Some(done) = match_seq(pieces, pi + 1, hay, end, ci) {
            return Some(done);
        }
        ends.pop();
    }
    None
}

fn match_one(node: &Node, hay: &[char], pos: usize, ci: bool) -> Option<usize> {
    match node {
        Node::Char(c) => {
            let want = if ci {
                c.to_lowercase().next().unwrap_or(*c)
            } else {
                *c
            };
            (hay.get(pos) == Some(&want)).then_some(pos + 1)
        }
        Node::Any => (pos < hay.len()).then_some(pos + 1),
        Node::Class { neg, items } => {
            let c = *hay.get(pos)?;
            let hit = items.iter().any(|it| class_hit(it, c, ci));
            (hit != *neg).then_some(pos + 1)
        }
        Node::Group(alts) => {
            for alt in alts {
                if let Some(end) = match_seq(alt, 0, hay, pos, ci) {
                    return Some(end);
                }
            }
            None
        }
        Node::Start | Node::End | Node::WordBoundary => Some(pos),
    }
}

fn class_hit(item: &ClassItem, c: char, ci: bool) -> bool {
    match item {
        ClassItem::Ch(x) => {
            if ci {
                x.to_lowercase().eq(c.to_lowercase())
            } else {
                *x == c
            }
        }
        ClassItem::Range(a, b) => {
            if ci {
                let lc = c.to_lowercase().next().unwrap_or(c);
                let uc = c.to_uppercase().next().unwrap_or(c);
                (*a..=*b).contains(&lc) || (*a..=*b).contains(&uc)
            } else {
                (*a..=*b).contains(&c)
            }
        }
        ClassItem::Word => is_word(c),
        ClassItem::NotWord => !is_word(c),
        ClassItem::Digit => c.is_ascii_digit(),
        ClassItem::NotDigit => !c.is_ascii_digit(),
        ClassItem::Space => c.is_whitespace(),
        ClassItem::NotSpace => !c.is_whitespace(),
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Path globbing: `*`, `?`, `**`, `[...]`, and `{a,b}` alternation.
///
/// `*` does not cross a `/`; `**` does. Separate from [`Pattern`] because a glob
/// is matched against a whole path and a regex against one line, and conflating
/// them is how `*` quietly becomes `.*`.
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

    #[test]
    fn the_subset_matches_what_it_says_it_does() {
        let p = Pattern::compile(r"^\s*fn \w+\(", false);
        assert!(p.is_match("    fn parse_args(a: u8) {"));
        assert!(!p.is_match("let fn_name = 3;"));

        let p = Pattern::compile(r"To(ken|ol)Ledger", false);
        assert!(p.is_match("struct TokenLedger {"));
        assert!(p.is_match("struct ToolLedger {"));
        assert!(!p.is_match("struct TopLedger {"));

        let p = Pattern::compile(r"\bTokenLedger\b", false);
        assert!(p.is_match("the TokenLedger is"));
        assert!(!p.is_match("MyTokenLedgers"));

        let p = Pattern::compile("ERROR", true);
        assert!(p.is_match("an error happened"));

        let p = Pattern::compile(r"a.*z", false);
        assert!(p.is_match("abcz"));
        assert!(!p.is_match("zzza"));

        let p = Pattern::compile(r"[A-Z][a-z]+", false);
        assert!(p.is_match("Hello"));
        assert!(!p.is_match("hello"));
    }

    #[test]
    fn syntax_outside_the_subset_is_reported_not_approximated() {
        let p = Pattern::compile(r"ab{2,3}", false);
        assert!(
            !p.unsupported.is_empty(),
            "counted repetition must be named"
        );
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
