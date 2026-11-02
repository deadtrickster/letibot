//! Cutting text to a bound, once - a tool payload, a queue row, a card and a status line all
//! cut text, and a second copy of the arithmetic is a second place a multibyte character can
//! be split. Every function here walks boundaries, so no cut lands inside a character.
//!
//! It is in this crate for the reason [`crate::sanitize`] is: the cutting is needed on both
//! sides of a boundary that cannot see across itself, and this is the crate both already see.

/// The longest prefix of `s` of at most `max` characters.
pub fn chars_head(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((at, _)) => &s[..at],
        None => s,
    }
}

/// The longest prefix of `s` that is at most `max` **bytes** and ends on a character boundary.
pub fn bytes_head(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The first `max` characters, then `…` - so the answer is one character over the cap, and a
/// caller that needs the ellipsis inside its width wants [`clip_chars_within`].
pub fn clip_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_string(),
    }
}

/// The first `width` characters **including** the ellipsis: the shape a field that must line up
/// with its neighbours takes, where the marker is spent from the width rather than added to it.
pub fn clip_chars_within(s: &str, width: usize) -> String {
    if s.char_indices().nth(width).is_none() {
        return s.to_string();
    }
    format!("{}…", chars_head(s, width.saturating_sub(1)))
}

/// At most `max` **bytes**, then `…`. The cap is the payload's, and the marker is not counted in
/// it, so a caller whose cap is a hard one wants [`bytes_head`] and its own arithmetic.
pub fn clip_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    format!("{}…", bytes_head(s, max))
}

/// The END of `s`: its last `max` characters, with `…` in front of what was dropped. The tail is
/// what a prompt's answer lives in (`Continue? [Y/n]`), and a line cut from the front keeps it.
pub fn clip_front_chars(s: &str, max: usize) -> String {
    let skip = s.chars().count().saturating_sub(max);
    if skip == 0 {
        return s.to_string();
    }
    let at = s.char_indices().nth(skip).map(|(i, _)| i).unwrap_or(0);
    format!("…{}", &s[at..])
}

/// The last `max_lines` lines of `s`, and at most `max_bytes` of them, plus whether anything was
/// dropped. Output keeps its tail because a command that failed says why last, and the cut is on a
/// line boundary because a head joined to a tail reads as one line that never existed.
pub fn tail_lines(s: &str, max_lines: usize, max_bytes: usize) -> (String, bool) {
    let lines: Vec<&str> = s.lines().collect();
    let mut kept: Vec<&str> = if lines.len() > max_lines {
        lines[lines.len() - max_lines..].to_vec()
    } else {
        lines.clone()
    };
    let mut capped = kept.len() < lines.len();
    while kept.iter().map(|l| l.len() + 1).sum::<usize>() > max_bytes && !kept.is_empty() {
        kept.remove(0);
        capped = true;
    }
    (kept.join("\n"), capped)
}

/// The first `max` characters of one line, then how many the line had left. The counted form is
/// what a search answer uses: a minified file is one line of forty kilobytes, and the number is
/// what tells the reader a `read` of that line is worth it.
pub fn clip_line_counted(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.trim_end().to_string();
    }
    format!(
        "{}… (+{} more characters on this line)",
        chars_head(s, max),
        total - max
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every cut is measured in characters, so a two- and a three-byte character behave alike.
    const MULTIBYTE: &str = "héllo 日本語 🎉 end";

    #[test]
    fn a_head_is_a_prefix_of_at_most_the_bound() {
        assert_eq!(chars_head("abcdef", 3), "abc");
        assert_eq!(chars_head("abc", 3), "abc");
        assert_eq!(chars_head("abc", 9), "abc");
        assert_eq!(chars_head("abcdef", 0), "");
        assert_eq!(chars_head("日本語です", 2), "日本");
        assert_eq!(chars_head("日", 1), "日");
    }

    #[test]
    fn a_byte_head_never_splits_a_character() {
        // Each of these is three bytes, so a cap of 4 lands inside the second one.
        assert_eq!(bytes_head("日本語", 4), "日");
        assert_eq!(bytes_head("日本語", 3), "日");
        assert_eq!(bytes_head("日本語", 2), "");
        assert_eq!(bytes_head("日本語", 0), "");
        assert_eq!(bytes_head("日本語", 9), "日本語");
        assert_eq!(bytes_head(MULTIBYTE, 1), "h");
        assert_eq!(bytes_head(MULTIBYTE, 2), "h");
        assert_eq!(bytes_head(MULTIBYTE, 3), "hé");
    }

    #[test]
    fn clipping_by_characters_adds_the_marker_outside_the_cap() {
        assert_eq!(clip_chars("abcdef", 3), "abc…");
        assert_eq!(clip_chars("abc", 3), "abc");
        assert_eq!(clip_chars("日本語です", 2), "日本…");
        assert_eq!(clip_chars(MULTIBYTE, 1), "h…");
    }

    #[test]
    fn clipping_within_a_width_spends_the_marker_from_it() {
        assert_eq!(clip_chars_within("abcdef", 4), "abc…");
        assert_eq!(clip_chars_within("abcd", 4), "abcd");
        assert_eq!(clip_chars_within("日本語です", 3), "日本…");
        // A width of zero has no room for the text, and the marker is still said.
        assert_eq!(clip_chars_within("日本語", 0), "…");
    }

    #[test]
    fn clipping_by_bytes_is_short_of_the_cap_and_whole_characters() {
        assert_eq!(clip_bytes("日本語", 4), "日…");
        assert_eq!(clip_bytes("日本語", 3), "日…");
        assert_eq!(clip_bytes("abc", 3), "abc");
        assert_eq!(clip_bytes("ab", 3), "ab");
    }

    #[test]
    fn a_front_clip_keeps_the_end_and_says_a_front_went() {
        assert_eq!(clip_front_chars("abcdef", 3), "…def");
        assert_eq!(clip_front_chars("abc", 3), "abc");
        assert_eq!(clip_front_chars("日本語です", 2), "…です");
        assert_eq!(clip_front_chars(MULTIBYTE, 3), "…end");
    }

    #[test]
    fn a_tail_is_whole_lines_and_says_so() {
        let text = "one\ntwo\nthree\nfour";
        assert_eq!(tail_lines(text, 2, 10_000), ("three\nfour".into(), true));
        assert_eq!(tail_lines(text, 9, 10_000), (text.into(), false));
        // The byte cap eats from the front, one whole line at a time, and each line costs its
        // bytes plus the newline that joined it to the next.
        assert_eq!(tail_lines(text, 9, 5), ("four".into(), true));
        assert_eq!(tail_lines(text, 9, 4), (String::new(), true));
        assert_eq!(tail_lines("", 3, 3), (String::new(), false));
    }

    #[test]
    fn a_counted_line_carries_what_it_left_behind() {
        assert_eq!(
            clip_line_counted("abcdef", 3),
            "abc… (+3 more characters on this line)"
        );
        assert_eq!(clip_line_counted("abc", 3), "abc");
        assert_eq!(clip_line_counted("abcdef   ", 9), "abcdef");
        assert_eq!(
            clip_line_counted("日本語です", 2),
            "日本… (+3 more characters on this line)"
        );
    }
}
