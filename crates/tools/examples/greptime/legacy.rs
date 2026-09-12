//! The matcher that `crates/tools/src/builtins/pattern.rs` used to hold, kept
//! HERE and only here.
//!
//! It is not dead code and it is not a second implementation to maintain: it is
//! the BEFORE half of the measurement that justified deleting it. A change
//! argued from a number has to end with the number, and a number you cannot
//! reproduce next month is an anecdote. Copied verbatim from e33cc1b, minus the
//! `unsupported` reporting, which is not on the timed path.
//!
//! If it ever stops compiling, delete it and say in the commit that the
//! comparison is now historical — do not repair it.
#![allow(dead_code)]

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
