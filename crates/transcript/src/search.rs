//! One query syntax for searching conversation rows: the head's find bar, the session list's
//! filter and the daemon's cross-session scan all read it here, so no two of them can disagree
//! about what `"a phrase"`, `re:` or `tool:bash` means.
//!
//! A query is role tokens, then needles. `me:` keeps the operator's own turns, `agent:` the
//! model's words, `tool:` the calls and their results, and `tool:NAME` one tool's. With no role
//! token every row is searched. A `"quoted phrase"` is that text exactly, `re:PATTERN` is a
//! regular expression, and the words left over are one substring. Case is smart: a needle with an
//! upper-case letter is case-sensitive, a lower-case one is not - both across ASCII, so a byte
//! offset found here is a byte offset in the row.

use std::borrow::Cow;
use std::ops::Range;

use crate::{Speaker, TranscriptItem, UserPart};

/// Who a row's words belong to, as far as a search is concerned. Three, because that is what a
/// person filters by: their own turns, the model's, and what a tool did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Operator,
    Agent,
    Tool,
}

impl Role {
    /// The word the wire, a bar and a list row spell it as. One spelling, so a hit a daemon filed
    /// as `mine` cannot be drawn as `operator` by a head.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Operator => "mine",
            Role::Agent => "agent",
            Role::Tool => "tool",
        }
    }

    /// The role a wire word names. `None` for a word this build does not know: a row from a
    /// newer daemon is a row this head cannot place, and guessing `agent` would be a lie.
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "mine" | "operator" | "me" => Some(Role::Operator),
            "agent" | "assistant" => Some(Role::Agent),
            "tool" => Some(Role::Tool),
            _ => None,
        }
    }
}

/// Which rows a query wants, read off its prefix tokens. No token is every role; two tokens are
/// both, which is why this is a set of flags and not one word.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wants {
    operator: bool,
    agent: bool,
    tool: bool,
    /// The tool names `tool:NAME` named. Empty with `tool` set is any tool.
    tools: Vec<String>,
}

impl Wants {
    /// Are these the words of everything? True before any token, and the answer a bar reads to
    /// say `all rows`.
    pub fn everything(&self) -> bool {
        !self.operator && !self.agent && !self.tool
    }

    /// Does the query want a piece of text with this role - and, for a tool, this name?
    pub fn accepts(&self, role: Role, tool: &str) -> bool {
        match role {
            Role::Operator => self.everything() || self.operator,
            Role::Agent => self.everything() || self.agent,
            Role::Tool => {
                (self.everything() || self.tool)
                    && (self.tools.is_empty() || self.tools.iter().any(|t| t == tool))
            }
        }
    }

    /// The word each role is filtered by, for a bar's own sentence: `mine + tool:bash`, or `all`
    /// when nothing narrows the roles.
    pub fn words(&self) -> String {
        if self.everything() {
            return "all".to_string();
        }
        let mut out = Vec::new();
        if self.operator {
            out.push("mine".to_string());
        }
        if self.agent {
            out.push("agent".to_string());
        }
        if self.tool {
            out.push(match self.tools.is_empty() {
                true => "tool".to_string(),
                false => format!("tool:{}", self.tools.join(",")),
            });
        }
        out.join(" + ")
    }

    /// The same set as QUERY TOKENS, which is what a bar writes when it steps the cycle: prose and
    /// syntax are two spellings of one fact and a cycle that wrote the prose would search for it.
    pub fn tokens(&self) -> String {
        if self.everything() {
            return String::new();
        }
        let mut out = Vec::new();
        if self.operator {
            out.push("me:".to_string());
        }
        if self.agent {
            out.push("agent:".to_string());
        }
        if self.tool {
            out.push(match self.tools.is_empty() {
                true => "tool:".to_string(),
                false => format!("tool:{}", self.tools.join(" ")),
            });
        }
        out.join(" ")
    }

    /// The roles in the cycle a bar steps through: everything, then one at a time, then the pair
    /// a reader most often wants together. One list, so the cycle and the tokens cannot drift.
    pub fn cycled(&self) -> Wants {
        if self.everything() {
            return Wants {
                operator: true,
                ..Wants::default()
            };
        }
        if self.operator {
            return Wants {
                agent: true,
                ..Wants::default()
            };
        }
        if self.agent {
            return Wants {
                tool: true,
                ..Wants::default()
            };
        }
        Wants::default()
    }
}

/// One thing a row's text must contain. Every needle in a query must be found for the row to
/// match, which is what makes `me: "cache key" re:^fn` one answer rather than three.
#[derive(Debug, Clone)]
enum Needle {
    /// A phrase or a bare word list: literal text, case-sensitive when it carries a capital.
    Text {
        needle: String,
        case: bool,
    },
    Pattern(regex::Regex),
}

impl Needle {
    /// Every occurrence, as byte ranges into `text`.
    fn hits(&self, text: &str) -> Vec<Range<usize>> {
        match self {
            Needle::Pattern(re) => re.find_iter(text).map(|m| m.range()).collect(),
            Needle::Text { needle, case } => match case {
                true => occurrences(text, needle),
                false => occurrences(&text.to_ascii_lowercase(), &needle.to_ascii_lowercase()),
            },
        }
    }
}

/// The byte ranges `needle` occurs at, overlapping ones included: `aa` in `aaa` is two.
fn occurrences(hay: &str, needle: &str) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = hay[from..].find(needle) {
        let start = from + at;
        out.push(start..start + needle.len());
        // One byte on, so an overlapping occurrence is found: a search for `aa` in `aaaa` that
        // stepped a whole needle on would answer half of them.
        from = start + hay[start..].chars().next().map_or(1, char::len_utf8);
        if from >= hay.len() {
            break;
        }
    }
    out
}

/// A parsed query: what it wants, and what the text must contain.
#[derive(Debug, Clone, Default)]
pub struct Query {
    wants: Wants,
    needles: Vec<Needle>,
    /// Why a `re:` in this query could not be built, if one could not. A bar says it instead of
    /// drawing a count no pattern answered.
    bad: Option<String>,
    /// The needles as a person wrote them, without the role tokens: what a hit's excerpt is
    /// centred on and what a bar shows it is searching for.
    words: String,
}

impl Query {
    /// Read a line of query text. Never fails: a pattern that does not compile is kept as `bad`,
    /// so the bar can say what is wrong while every other needle still narrows the rows.
    pub fn parse(raw: &str) -> Query {
        let mut wants = Wants::default();
        let mut needles: Vec<Needle> = Vec::new();
        let mut bad = None;
        let mut words: Vec<String> = Vec::new();
        let mut bare = String::new();
        let chars: Vec<char> = raw.chars().collect();
        let mut i = 0;
        // A bare run is flushed when a token or a quoted needle interrupts it, so `foo "bar"`
        // is two needles in the order they were written.
        fn flush(bare: &mut String, needles: &mut Vec<Needle>, words: &mut Vec<String>) {
            let needle = bare.split_whitespace().collect::<Vec<_>>().join(" ");
            bare.clear();
            if needle.is_empty() {
                return;
            }
            words.push(needle.clone());
            needles.push(Needle::Text {
                case: has_upper(&needle),
                needle,
            });
        }
        while i < chars.len() {
            let c = chars[i];
            if c.is_whitespace() {
                bare.push(' ');
                i += 1;
                continue;
            }
            if c == '"' {
                flush(&mut bare, &mut needles, &mut words);
                let mut phrase = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    phrase.push(chars[i]);
                    i += 1;
                }
                // The closing quote, or the end of a line somebody is still typing.
                i += 1;
                if !phrase.is_empty() {
                    words.push(format!("\"{phrase}\""));
                    needles.push(Needle::Text {
                        case: has_upper(&phrase),
                        needle: phrase,
                    });
                }
                continue;
            }
            // A token runs to whitespace. `tool:bash` and `re:^fn` are tokens; anything else is a
            // bare word and stays in the run.
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            if let Some((role, name)) = role_token(&token) {
                flush(&mut bare, &mut needles, &mut words);
                match role {
                    Role::Operator => wants.operator = true,
                    Role::Agent => wants.agent = true,
                    Role::Tool => {
                        wants.tool = true;
                        if let Some(name) = name.filter(|n| !n.is_empty())
                            && !wants.tools.iter().any(|t| t.eq_ignore_ascii_case(name))
                        {
                            wants.tools.push(name.to_string());
                        }
                    }
                }
                continue;
            }
            if let Some(rest) = token.strip_prefix("re:") {
                flush(&mut bare, &mut needles, &mut words);
                // `re:"a b"` for a pattern with a space in it; the rest of the token otherwise.
                let (pattern, used) = match rest.starts_with('"') {
                    true => quoted_from(&chars, start + 3),
                    false => (rest.to_string(), 0),
                };
                if used > 0 {
                    i = used;
                }
                let case = !has_upper(&pattern);
                match regex::RegexBuilder::new(&pattern)
                    .case_insensitive(case)
                    .build()
                {
                    Ok(re) => {
                        words.push(format!("re:{pattern}"));
                        needles.push(Needle::Pattern(re));
                    }
                    Err(e) => {
                        bad = Some(e.to_string().lines().next().unwrap_or("bad pattern").into())
                    }
                }
                continue;
            }
            bare.push_str(&token);
        }
        flush(&mut bare, &mut needles, &mut words);
        Query {
            wants,
            needles,
            bad,
            words: words.join(" "),
        }
    }

    /// Nothing to look for yet: an empty bar, and a bar that should draw itself rather than a
    /// count of zero.
    pub fn is_empty(&self) -> bool {
        self.needles.is_empty()
    }

    /// Why this query cannot be run, when a `re:` did not compile.
    pub fn complaint(&self) -> Option<&str> {
        self.bad.as_deref()
    }

    /// Which roles the query wants.
    pub fn wants(&self) -> &Wants {
        &self.wants
    }

    /// The needles as written, role tokens left out - what a hit is centred on.
    pub fn words(&self) -> &str {
        &self.words
    }

    /// The first needle that answers `text`, for an excerpt's centre. `None` for an empty query.
    pub fn first_at(&self, text: &str) -> Option<usize> {
        self.needles
            .iter()
            .filter(|n| !matches!(n, Needle::Text { needle, .. } if needle.is_empty()))
            .filter_map(|n| n.hits(text).first().map(|r| r.start))
            .min()
    }

    /// Every occurrence of every needle in `text`, as byte ranges. A bar tints all of them; a
    /// scan that only needs a verdict asks [`Query::matches`].
    pub fn hits(&self, text: &str) -> Vec<Range<usize>> {
        let mut out: Vec<Range<usize>> = Vec::new();
        for n in &self.needles {
            out.extend(n.hits(text));
        }
        out.sort_by_key(|r| r.start);
        out
    }

    /// Does `text` carry every needle? An empty query matches nothing, so a bar waiting for its
    /// first letter counts no rows.
    pub fn matches(&self, text: &str) -> bool {
        !self.needles.is_empty()
            && self
                .needles
                .iter()
                .all(|n| !matches!(n, Needle::Text { needle, .. } if needle.is_empty()))
            && self.needles.iter().all(|n| !n.hits(text).is_empty())
    }
}

/// `me:` / `agent:` / `tool:` / `tool:bash` read as the role it names, and for a tool the name
/// as written. `None` for a word that merely starts like one: `tools:` is somebody's search
/// word, and reading it as a filter would silently narrow what they asked for.
fn role_token(token: &str) -> Option<(Role, Option<&str>)> {
    let lower = token.to_ascii_lowercase();
    for (prefix, role) in [
        ("me:", Role::Operator),
        ("agent:", Role::Agent),
        ("tool:", Role::Tool),
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            // The name as the searcher typed it, cut from the token and not from its lower case.
            let name = token.len().checked_sub(rest.len()).map(|at| &token[at..]);
            return Some((role, name));
        }
    }
    None
}

/// The rest of a `re:"…"` from the opening quote, and the index just past the closing one.
fn quoted_from(chars: &[char], at: usize) -> (String, usize) {
    let mut pattern = String::new();
    let mut i = at + 1;
    while i < chars.len() && chars[i] != '"' {
        pattern.push(chars[i]);
        i += 1;
    }
    (pattern, i + 1)
}

/// Smart case: a capital in the needle means the searcher meant it.
fn has_upper(s: &str) -> bool {
    s.chars().any(char::is_uppercase)
}

/// One searchable piece of a row: who it belongs to, which tool when it is a tool's, and the
/// words. A row can carry several - an assistant row's prose and the arguments of the calls it
/// proposed are two - which is what lets `tool:bash` find a call and not the sentence around it.
#[derive(Debug, Clone)]
pub struct Seg<'a> {
    pub role: Role,
    pub tool: &'a str,
    pub text: Cow<'a, str>,
}

/// The pieces of a row a search reads, in the order a reader meets them.
pub fn segments(item: &TranscriptItem) -> Vec<Seg<'_>> {
    match item {
        TranscriptItem::System { text, .. } => vec![Seg {
            role: Role::Agent,
            tool: "",
            text: Cow::Borrowed(text),
        }],
        TranscriptItem::User { parts, speaker } => {
            let role = match speaker {
                Speaker::Operator => Role::Operator,
                // A steering line, a job's settlement, a peer's message: not the operator's
                // words, so `me:` must not find them.
                Speaker::Agent | Speaker::Peer { .. } => Role::Agent,
            };
            let text = user_text(parts);
            text.map(|text| Seg {
                role,
                tool: "",
                text,
            })
            .into_iter()
            .collect()
        }
        TranscriptItem::Reasoning { text, .. } => vec![Seg {
            role: Role::Agent,
            tool: "",
            text: Cow::Borrowed(text),
        }],
        TranscriptItem::Assistant {
            text, tool_calls, ..
        } => {
            let mut out = Vec::with_capacity(1 + tool_calls.len());
            if !text.is_empty() {
                out.push(Seg {
                    role: Role::Agent,
                    tool: "",
                    text: Cow::Borrowed(text),
                });
            }
            for c in tool_calls {
                out.push(Seg {
                    role: Role::Tool,
                    tool: c.name.as_str(),
                    text: Cow::Borrowed(c.arguments.as_str()),
                });
            }
            out
        }
        TranscriptItem::ToolResult { name, payload, .. } => vec![Seg {
            role: Role::Tool,
            tool: name.as_str(),
            text: Cow::Borrowed(payload),
        }],
        TranscriptItem::Notice { text, .. } => vec![Seg {
            role: Role::Agent,
            tool: "",
            text: Cow::Borrowed(text),
        }],
        TranscriptItem::Offer { title, body, .. } => vec![Seg {
            role: Role::Agent,
            tool: "",
            text: Cow::Owned(format!("{title}\n{body}")),
        }],
        // A delimiter that draws nothing, so there is nothing in it to find.
        TranscriptItem::SegmentMark { .. } => Vec::new(),
    }
}

/// The words of a `User` row: its text parts, joined. Pictures and documents carry no words a
/// person searched for - a filename is not a phrase the conversation said.
fn user_text(parts: &[UserPart]) -> Option<Cow<'_, str>> {
    let mut out: Option<Cow<'_, str>> = None;
    for part in parts {
        if let UserPart::Text { text } = part {
            out = Some(match out {
                None => Cow::Borrowed(text),
                Some(mut had) => {
                    had.to_mut().push('\n');
                    had.to_mut().push_str(text);
                    had
                }
            });
        }
    }
    out.filter(|t| !t.trim().is_empty())
}

/// What one row answered: which role matched, the tool's name when it was a tool's words, and
/// the one line a list shows. A row is one hit however many times a needle occurs in it - a
/// reader jumps by rows, and a count of occurrences would outnumber the things on the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowMatch {
    pub role: Role,
    pub tool: String,
    pub excerpt: String,
}

/// The first piece of `item` this query wants and finds, with an excerpt around the match.
/// `None` for a row it does not match, and for a query with nothing to look for.
pub fn row_match(query: &Query, item: &TranscriptItem) -> Option<RowMatch> {
    if query.is_empty() {
        return None;
    }
    for seg in segments(item) {
        if !query.wants().accepts(seg.role, seg.tool) {
            continue;
        }
        if !query.matches(&seg.text) {
            continue;
        }
        let at = query.first_at(&seg.text).unwrap_or(0);
        return Some(RowMatch {
            role: seg.role,
            tool: seg.tool.to_string(),
            excerpt: excerpt(&seg.text, at, EXCERPT),
        });
    }
    None
}

/// How much of a row a list row shows. Wide enough to read a sentence, narrow enough that a
/// page of them is one screen.
pub const EXCERPT: usize = 120;

/// One line around `at`, with the whitespace of a multi-line body flattened: a list row is one
/// row, so a newline in an excerpt would break the answer into rows nobody placed.
pub fn excerpt(text: &str, at: usize, width: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(seen) = text.get(..at) else {
        return cut(&flat, width);
    };
    // Where the match lands once the whitespace runs are one space each: the same walk over the
    // flat line, so the excerpt is cut around the words rather than around a byte nobody sees.
    let before = seen.split_whitespace().count();
    let words: Vec<&str> = flat.split(' ').collect();
    if words.is_empty() || width == 0 {
        return String::new();
    }
    let keep = width.max(16) / 6;
    let from = before.saturating_sub(keep);
    let mut out = words[from..].join(" ");
    let cut_off = out.chars().count() > width;
    out = cut(&out, width);
    let mut said = String::new();
    if from > 0 {
        said.push('…');
    }
    said.push_str(&out);
    if cut_off {
        said.push('…');
    }
    said
}

/// `s` cut to `width` columns, never inside a character.
fn cut(s: &str, width: usize) -> String {
    match s.char_indices().nth(width) {
        Some((at, _)) => s[..at].to_string(),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(text: &str) -> TranscriptItem {
        TranscriptItem::Assistant {
            text: text.into(),
            tool_calls: Vec::new(),
            truncated: false,
        }
    }

    /// Smart case is the whole tier: a needle in lower case finds either spelling, one with a
    /// capital in it means what it says.
    #[test]
    fn a_needle_is_case_insensitive_until_it_carries_a_capital() {
        let q = Query::parse("cache");
        assert!(q.matches("The CACHE key is warm"));
        let q = Query::parse("CACHE");
        assert!(q.matches("the CACHE key"));
        assert!(!q.matches("the cache key"));
    }

    /// Each tier survives a round trip: the words, the quoted phrase, and the pattern; and every
    /// needle of a query must be found, so the tiers narrow together instead of widening.
    #[test]
    fn the_three_tiers_each_narrow_the_row() {
        assert!(Query::parse("cache key").matches("a cache key here"));
        assert!(!Query::parse("\"cache key\"").matches("cache, and a key"));
        assert!(Query::parse("\"cache key\"").matches("a cache key here"));
        assert!(Query::parse("re:^fn").matches("fn main() {}"));
        assert!(!Query::parse("re:^fn").matches("pub fn main() {}"));
        // A pattern with a space in it is quoted, the way a phrase is.
        assert!(Query::parse("re:\"^fn [a-z]+\"").matches("fn main() {}"));
        // Two needles: both must hold.
        assert!(!Query::parse("cache re:^fn").matches("a cache key"));
        assert!(Query::parse("cache re:^fn").matches("fn cache() {}"));
    }

    /// A bad pattern is kept as a complaint and the rest of the query still answers, so a bar
    /// says what is wrong without going blank.
    #[test]
    fn a_pattern_that_does_not_compile_is_a_complaint_and_not_a_panic() {
        let q = Query::parse("re:(unclosed");
        assert!(q.complaint().is_some(), "no complaint: {q:?}");
        assert!(!q.matches("anything"));
        let q = Query::parse("keep re:(unclosed");
        assert!(!q.matches("nothing here"));
    }

    /// Role tokens are a set: `me:` alone finds the operator's turn, `me: agent:` finds both, and
    /// no token finds everything, tool arguments included.
    #[test]
    fn role_tokens_choose_whose_words_are_searched() {
        let user = TranscriptItem::User {
            parts: vec![UserPart::Text {
                text: "read the cache".into(),
            }],
            speaker: Speaker::Operator,
        };
        let called = TranscriptItem::Assistant {
            text: "reading".into(),
            tool_calls: vec![crate::ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                arguments: r#"{"command":"cat cache"}"#.into(),
            }],
            truncated: false,
        };
        let q = Query::parse("me: cache");
        assert!(row_match(&q, &user).is_some());
        assert!(row_match(&q, &called).is_none());
        let q = Query::parse("agent: cache");
        assert!(row_match(&q, &user).is_none());
        let q = Query::parse("tool: cache");
        assert!(row_match(&q, &called).is_some_and(|m| m.tool == "bash"));
        let q = Query::parse("tool:zsh cache");
        assert!(row_match(&q, &called).is_none());
        let q = Query::parse("cache");
        assert!(row_match(&q, &user).is_some());
        assert!(row_match(&q, &called).is_some());
    }

    /// A row that is not the operator's prose is not `me:`, however it was appended: a steering
    /// line and a peer's message read as the agent's words.
    #[test]
    fn an_appended_user_row_is_not_the_operators() {
        let peer = TranscriptItem::User {
            parts: vec![UserPart::Text {
                text: "deploy now".into(),
            }],
            speaker: Speaker::Peer {
                from: "cc@rano".into(),
            },
        };
        assert!(row_match(&Query::parse("me: deploy"), &peer).is_none());
        assert!(row_match(&Query::parse("agent: deploy"), &peer).is_some());
    }

    /// Every occurrence is found, overlapping ones included, and the ranges are byte offsets into
    /// the text that was searched - which is what a tint slices with.
    #[test]
    fn every_occurrence_is_a_range_into_the_text() {
        let q = Query::parse("aa");
        assert_eq!(q.hits("aaa"), vec![0..2, 1..3]);
        let q = Query::parse("warm");
        let text = "a warm day and a warm night";
        let hits = q.hits(text);
        assert_eq!(hits.len(), 2);
        for r in hits {
            assert_eq!(&text[r], "warm");
        }
    }

    /// The bar's cycle steps through the roles and comes back to everything, so one key cannot
    /// strand a reader on a filter they cannot reach.
    #[test]
    fn the_role_cycle_returns_to_everything() {
        let mut w = Wants::default();
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(w.tokens());
            w = w.cycled();
        }
        assert_eq!(seen, vec!["", "me:", "agent:", "tool:"]);
        assert!(w.everything());
    }

    /// An excerpt is one line around the match, cut to the width, and it says when it was cut.
    #[test]
    fn a_excerpt_is_one_line_around_the_match() {
        let body = "first line\nsecond line with the cache in it\nthird line";
        let at = body.find("cache").expect("the fixture has the word");
        let line = excerpt(body, at, 40);
        assert!(line.contains("cache"), "{line:?}");
        assert!(!line.contains('\n'), "{line:?}");
        let long = "x".repeat(400);
        let at = long.find("xxxx").unwrap_or(0);
        let line = excerpt(&long, at, 40);
        assert!(line.chars().count() <= 42, "{line:?}");
    }

    /// An empty query matches nothing, so a bar waiting for a letter counts no rows.
    #[test]
    fn an_empty_query_matches_nothing() {
        let q = Query::parse("");
        assert!(q.is_empty());
        assert!(!q.matches(""));
        assert!(!q.matches("anything"));
        assert!(row_match(&q, &assistant("anything")).is_none());
    }
}
