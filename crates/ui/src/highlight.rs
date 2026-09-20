//! Syntax colouring: **rano's captures, this crate's roles.**
//!
//! The engine is rano's (`~/Projects/rano`, `syntax::Stream`): tree-sitter, 28
//! languages, one capture walk, no palette — the same engine [`crate::sidediff`] already
//! used for the two-panel diff. What lives here is the other half of that decision,
//! `capture name → Role`, because a colour is a statement about the terminal rather than
//! about the grammar. One table for the whole workspace: two copies would drift, and the
//! drift would be invisible — the same Rust coloured differently in two panes of one
//! screen.
//!
//! # What used to be here
//!
//! A hand-written, line-oriented lexer (`StreamingCode`) that coloured six token classes
//! in **ten** languages: a keyword list per language, a capital-letter heuristic for
//! types, a `(` lookahead for function names, and one resumable `State` carried across
//! newlines so a block comment spanning lines stayed a comment.
//!
//! That design was right about the thing it was built for — a code block arrives four
//! characters at a time, and re-scanning the block per delta is O(N²) (grok-build
//! measured ~35 ms/push near the end of a ~1000-line block, and the citation was in this
//! file) — and it paid for that with coverage. `tsx`, `lua`, `ruby`, `diff` and eighteen
//! other languages rano knows rendered plain, and the heuristics were wrong often enough
//! to be a known cost: `Vec` was a type because it is capitalised, `foo` was a function
//! because a `(` followed.
//!
//! This file said what to do about that: *"a parser is the point where this file should
//! be deleted in favour of `syntect` or a tree-sitter grammar rather than extended."*
//! This is that, with the tree-sitter engine already in the tree.
//!
//! The cost it brings is a capture walk per repaint — measured 2026-09-20 at ~0.1 µs per
//! byte and ~1.1 µs per line, so ~470 µs for a 5.4 KB Rust block, which is a frame's
//! budget at fence sizes and is recorded in rano's `TODO.md` §9. An embedder painting a
//! long block should bound what it hands over, which is what `crates/tui`'s window does
//! for the conversation.

use crate::style::Role;

/// rano's capture names onto this crate's six syntax roles.
///
/// The dotted fallback mirrors rano's own `theme`: a name the table does not list falls
/// back to its prefix (`type.builtin` → `type`), and only a name with no known prefix at
/// all (`variable`, `punctuation.bracket`) renders plain — which is honest, because
/// those are the tokens a reader does not need coloured.
///
/// # Why six, and not rano's dozen
///
/// A terminal has expensive colour and cheap structure, and six roles is what a reader
/// holds: a keyword, a type, a function name, a string, a number, a comment. Rano's
/// `theme` distinguishes a dozen more (`attribute`, `label`, `module`, `operator`, …) and
/// drawing all of them would spend the palette on distinctions nobody scans for. What is
/// not mapped is not lost: [`Role::Plain`] is the reader's own foreground, which is the
/// right colour for punctuation.
pub fn role_for_capture(name: &str) -> Role {
    match name {
        "comment" => Role::Comment,
        "string" | "escape" => Role::StringLit,
        "number" | "constant" | "property" => Role::NumberLit,
        "type" | "constructor" | "label" => Role::TypeName,
        "keyword" | "include" | "preproc" | "variable.builtin" => Role::Keyword,
        "function" => Role::FuncName,
        _ => match name.split_once('.') {
            Some((prefix, _)) => role_for_capture(prefix),
            None => Role::Plain,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The common captures land on a role. The *spellings* are checked against a real
    /// parse in `crates/tui/src/render.rs`'s code-block tests, which is where a name
    /// that no grammar emits would show up as an uncoloured token.
    #[test]
    fn the_common_captures_land_on_a_role() {
        for (name, want) in [
            ("keyword", Role::Keyword),
            ("string", Role::StringLit),
            ("comment", Role::Comment),
            ("number", Role::NumberLit),
            ("type", Role::TypeName),
            ("function", Role::FuncName),
            ("constructor", Role::TypeName),
            ("escape", Role::StringLit),
            ("property", Role::NumberLit),
            ("include", Role::Keyword),
        ] {
            assert_eq!(role_for_capture(name), want, "{name}");
        }
    }

    /// A dotted name falls back to its prefix, one level at a time — which is what keeps
    /// a grammar's new sub-capture from rendering plain the day it appears.
    #[test]
    fn a_dotted_name_falls_back_to_its_prefix() {
        assert_eq!(role_for_capture("type.builtin"), Role::TypeName);
        assert_eq!(role_for_capture("function.method"), Role::FuncName);
        assert_eq!(role_for_capture("keyword.control.conditional"), Role::Keyword);
        assert_eq!(role_for_capture("string.escape"), Role::StringLit);
    }

    /// What is not drawn is plain, not guessed at. `punctuation.bracket` on a keyword
    /// colour would make every bracket in a file shout.
    #[test]
    fn punctuation_and_variables_are_plain() {
        for name in [
            "punctuation.bracket",
            "punctuation.delimiter",
            "variable",
            "variable.parameter",
            "operator",
            "",
        ] {
            assert_eq!(role_for_capture(name), Role::Plain, "{name:?}");
        }
    }
}
