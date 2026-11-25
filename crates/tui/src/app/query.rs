//! A filter's query: what a typed line matches by, parsed and matched in one place so every list
//! that narrows as you type agrees - the session list, the panes a `/` filter narrows, and the
//! model card.
//!
//! Three spellings, most explicit first. `"a phrase"` is those characters in that order, case
//! folded, and never the letters-in-order fallback. `re:<pattern>` is a regular expression, with
//! the pattern's own case rules; a pattern that does not compile matches nothing and says why.
//! Everything else is the fuzzy ladder, which is what the model card has always used: the whole
//! name, a prefix, a word start, anywhere inside, and last its letters in order.

use regex::Regex;

/// The ladder's last rung: the query's letters merely in order inside the field. It is a FALLBACK
/// and never a companion - once any row answers below it, the rows that only reached it are noise.
pub(crate) const SUBSEQUENCE: u8 = 4;

/// One query, from one typed line. `Empty` keeps every row.
#[derive(Debug, Clone, Default)]
pub(crate) enum Query {
    #[default]
    Empty,
    /// `"words"`: those characters, in that order and case-folded, somewhere in the field.
    Phrase(String),
    /// `re:<pattern>`: the compiled pattern, or the one-line reason it is not one.
    Regex(Result<Regex, String>),
    /// The ladder in [`rank`].
    Fuzzy(String),
}

impl Query {
    /// What a line asks for. The closing quote of a phrase is optional, so a phrase narrows while
    /// it is being typed; a `re:` whose pattern is empty asks for nothing.
    pub(crate) fn parse(line: &str) -> Query {
        let t = line.trim();
        if t.is_empty() {
            return Query::Empty;
        }
        if let Some(pattern) = t.strip_prefix("re:") {
            if pattern.is_empty() {
                return Query::Empty;
            }
            return Query::Regex(Regex::new(pattern).map_err(|e| one_line(&e.to_string())));
        }
        if let Some(rest) = t.strip_prefix('"') {
            let words = rest.strip_suffix('"').unwrap_or(rest).trim();
            return match words.is_empty() {
                true => Query::Empty,
                false => Query::Phrase(words.to_string()),
            };
        }
        Query::Fuzzy(t.to_string())
    }

    pub(crate) fn is_empty(&self) -> bool {
        matches!(self, Query::Empty)
    }

    /// The reason a line matches nothing because it cannot be read at all: a `re:` that does not
    /// compile says so in the line it was typed into. `None` for a query that can be answered.
    pub(crate) fn error(&self) -> Option<&str> {
        match self {
            Query::Regex(Err(e)) => Some(e.as_str()),
            _ => None,
        }
    }

    /// How well one field answers, lowest first, and `None` for no match. Every tier but the
    /// ladder's last is a real containment, so a number means the same thing across all three
    /// spellings: a phrase is ranked by where it sits in the field, and a regex answers the whole
    /// field or not at all.
    pub(crate) fn rank(&self, field: &str) -> Option<u8> {
        match self {
            Query::Empty => Some(0),
            Query::Phrase(words) => rank(field, words).filter(|t| *t < SUBSEQUENCE),
            Query::Regex(Ok(re)) => re.is_match(field).then_some(0),
            Query::Regex(Err(_)) => None,
            Query::Fuzzy(words) => rank(field, words),
        }
    }

    /// One row's tier: the best of its fields, and `None` when no field answers. An empty field is
    /// skipped, or every row would match a query the daemon left a column blank for.
    pub(crate) fn best(&self, fields: &[&str]) -> Option<u8> {
        fields
            .iter()
            .filter(|f| !f.is_empty())
            .filter_map(|f| self.rank(f))
            .min()
    }

    /// Whether a row at `tier` is kept. `strict` is the LIST's own fact, from [`Query::strict`].
    pub(crate) fn keeps(&self, tier: Option<u8>, strict: bool) -> bool {
        tier.is_some_and(|t| !strict || t < SUBSEQUENCE)
    }

    /// Whether a list holds a match that is not the letters-in-order fallback - the one rule the
    /// ladder adds, and the reason a scattered match never sits above a row that contains the line.
    pub(crate) fn strict<'a>(tiers: impl IntoIterator<Item = &'a Option<u8>>) -> bool {
        tiers.into_iter().flatten().any(|t| *t < SUBSEQUENCE)
    }
}

/// How well one field answers a query, lowest first: the whole name or the model part alone, a
/// prefix of either, the start of a word inside it, anywhere inside it, and last its letters in
/// order. `None` is no match at all.
pub(crate) fn rank(name: &str, query: &str) -> Option<u8> {
    let (name, query) = (name.to_lowercase(), query.to_lowercase());
    let model = name.rsplit('/').next().unwrap_or(&name);
    if name == query || model == query {
        return Some(0);
    }
    if name.starts_with(&query) || model.starts_with(&query) {
        return Some(1);
    }
    if word_start(&name, &query) {
        return Some(2);
    }
    if name.contains(&query) {
        return Some(3);
    }
    subsequence(&name, &query).then_some(SUBSEQUENCE)
}

/// Whether what was typed begins a word of the name: `pro` starts the `pro` of `deepseek-v4-pro`,
/// and `deep` does not start the `deep` of `deepseek` by this test - a prefix already answered it.
fn word_start(name: &str, query: &str) -> bool {
    let mut from = 0;
    while let Some(at) = name[from..].find(query) {
        let at = from + at;
        let before = name[..at].chars().next_back();
        if before.is_none_or(|c| !c.is_alphanumeric()) {
            return true;
        }
        from = at + query.len().max(1);
    }
    false
}

/// The query's letters in order inside the name, which is how `dflash` reaches
/// `deepseek/deepseek-v4-flash`.
fn subsequence(name: &str, query: &str) -> bool {
    let mut letters = name.chars();
    query.chars().all(|c| letters.any(|x| x == c))
}

/// A regex error's first sentence. The crate's own text is a diagram of the pattern with the caret
/// under the mistake; a filter line is one row, and the sentence is the half of it that is read.
fn one_line(e: &str) -> String {
    match e.lines().map(str::trim).find(|l| l.starts_with("error:")) {
        Some(why) => why.trim_start_matches("error:").trim().to_string(),
        None => e.lines().next().unwrap_or(e).trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three spellings are told apart, and a line that is none of them is the ladder - the
    /// meaning every list had before there were tiers.
    #[test]
    fn a_line_is_read_as_one_of_the_three_tiers() {
        assert!(matches!(Query::parse("   "), Query::Empty));
        assert!(matches!(Query::parse("\"\""), Query::Empty));
        assert!(matches!(Query::parse("re:"), Query::Empty));
        assert!(matches!(Query::parse("rust"), Query::Fuzzy(w) if w == "rust"));
        assert!(matches!(Query::parse("  the bench  "), Query::Fuzzy(w) if w == "the bench"));
        assert!(matches!(Query::parse("\"the bench\""), Query::Phrase(w) if w == "the bench"));
        // An unterminated phrase still narrows: the quote is typed before the words are.
        assert!(matches!(Query::parse("\"the ben"), Query::Phrase(w) if w == "the ben"));
        assert!(matches!(
            Query::parse("re:^rust-\\d+$"),
            Query::Regex(Ok(_))
        ));
    }

    /// A phrase matches the characters in that order and does not care about case, and it never
    /// reaches the letters-in-order rung - that is what makes it a phrase.
    #[test]
    fn a_phrase_is_those_characters_and_never_the_ladder() {
        let q = Query::parse("\"Rust Bench\"");
        // Ranked by where in the field the phrase sits - the ladder's own order, minus its last
        // rung: one that begins a word is a better answer than one buried inside another.
        assert_eq!(q.rank("the rust bench"), Some(2));
        assert_eq!(q.rank("Rust Bench"), Some(0));
        // The letters of `rstb` ARE in order inside `the rust bench`, which is exactly what a
        // phrase must not trade on: the same letters unquoted are the ladder's last rung.
        assert_eq!(
            Query::parse("rstb").rank("the rust bench"),
            Some(SUBSEQUENCE)
        );
        assert_eq!(Query::parse("\"rstb\"").rank("the rust bench"), None);
        assert_eq!(q.error(), None);
    }

    /// A regex is the pattern's own business: it matches what it says, and `(?i)` is how it is
    /// asked to ignore case.
    #[test]
    fn a_regex_matches_what_it_says() {
        let q = Query::parse("re:^s-[0-9a-f]{8}$");
        assert_eq!(q.rank("s-0123abcd"), Some(0));
        assert_eq!(q.rank("s-0123abcd-2"), None);
        assert_eq!(q.rank("Rust"), None);
        let any_case = Query::parse("re:(?i)rust");
        assert_eq!(any_case.rank("RUSTY"), Some(0));
    }

    /// A pattern that does not compile matches nothing and keeps its reason, which is what the
    /// filter line draws: a head that panicked or that quietly matched everything would be two
    /// worse answers than "there is no such pattern".
    #[test]
    fn a_bad_regex_matches_nothing_and_says_why() {
        let q = Query::parse("re:a(");
        let why = q.error().expect("a bad pattern has a reason");
        assert!(!why.is_empty() && !why.contains('\n'), "{why:?}");
        assert!(
            why.contains("unclosed group") || why.contains("regex"),
            "{why}"
        );
        assert_eq!(q.rank("anything at all"), None);
        assert_eq!(q.best(&["a", "b"]), None);
        assert!(
            !q.keeps(q.rank("anything at all"), false),
            "a broken pattern keeps no row"
        );
    }

    /// The ladder's four rungs and its fallback, unchanged.
    #[test]
    fn the_ladder_orders_a_match_before_the_letters_in_order() {
        let q = Query::parse("pro");
        assert_eq!(q.rank("pro"), Some(0));
        assert_eq!(q.rank("pro-max"), Some(1));
        assert_eq!(q.rank("deepseek-v4-pro"), Some(2));
        assert_eq!(q.rank("improved"), Some(3));
        assert_eq!(q.rank("p-r-o"), Some(SUBSEQUENCE));
        // And the fallback is dropped once one row really contains the line.
        let tiers = vec![Some(3), Some(SUBSEQUENCE), None];
        assert!(Query::strict(&tiers));
        assert!(!q.keeps(Some(SUBSEQUENCE), true), "the fallback went");
        assert!(q.keeps(Some(3), true));
        assert!(!q.keeps(None, false), "no match is never kept");
        assert!(!Query::strict(&[Some(SUBSEQUENCE)]), "nothing contains it");
    }
}
