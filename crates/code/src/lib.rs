//! The structural half of code search: **what is defined in this file, where.**
//!
//! # The measurement this exists for
//!
//! Read out of a real session on 2026-09-09: a model spent 5 of its 12 tool
//! rounds fighting `grep`, and every one of the five patterns was a STRUCTURAL
//! question written in a LEXICAL language.
//!
//! ```text
//! ^(pub )?(mod|fn|struct|enum|impl|const|static)\s
//! ^\s*(pub )?fn |^mod |^#\[cfg\(test\)\]|^impl
//! ```
//!
//! Those are not searches for text. They are a request for the file's outline,
//! spelled as a guess about how its text happens to be laid out — and that guess
//! is unfixable from inside `grep`, because **relaxing a structural query
//! lexically can only ever miss.** Drop the `^` and `fn` matches inside comments,
//! strings and the word "define"; keep it and a `    pub fn` indented under an
//! `impl` is invisible. There is no third pattern. The question needs a parser.
//!
//! # What this crate is, and what it deliberately is not
//!
//! It turns SOURCE TEXT into a list of definitions. It does not open files, list
//! directories, or know what a path is beyond the extension on the end of one.
//! That is not an accident of layering, it is the sandbox: `letibot-tools`
//! confines every read to an [`ExecBackend`], and a parser crate that walked the
//! filesystem itself would route around that confinement in the one place nobody
//! would think to look for it. The tool does the walking; this does the parsing.
//!
//! [`ExecBackend`]: https://docs.rs/letibot-tools
//!
//! # An unknown language says so
//!
//! [`Language::of_path`] returns an error naming the extension and listing what
//! IS supported. It never returns an empty outline for a `.tf` file, because an
//! empty outline and an unparsed file are different facts and this repo has just
//! spent a night on the class of bug that reports them identically.
//!
//! # The second reader of these grammars
//!
//! [`shell`] normalises a shell command through the same `tree-sitter-bash` that
//! `outline` uses, for `docs/boundary-and-adjudication.md` §4's layer 2: a
//! permission decision made on a string the shell has not expanded yet is a
//! decision about a meaning that does not exist. Same crate, same reason — the
//! grammars are C and this is where the `cc` build lives — and the same discipline:
//! a construct the grammar cannot resolve is *named*, never assumed harmless.

pub mod shell;

use std::fmt;

use rano::syntax::{Lang, Node, Stream};

/// The languages with a grammar compiled in.
///
/// Six, and the list is short on purpose: each one is a C grammar and a `cc`
/// build. Adding a seventh is a one-line change in [`Language::ALL`] plus its
/// kind table — see [`classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Python,
    Go,
    C,
    Bash,
    Json,
}

impl Language {
    pub const ALL: &'static [Language] = &[
        Language::Rust,
        Language::Python,
        Language::Go,
        Language::C,
        Language::Bash,
        Language::Json,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::Go => "go",
            Language::C => "c",
            Language::Bash => "bash",
            Language::Json => "json",
        }
    }

    /// The extensions that select this grammar, without the dot.
    ///
    /// `.h` is claimed for C. It is also C++ about half the time, and a C parser
    /// on a C++ header produces a partial tree rather than nothing — which
    /// [`Outline::partial`] reports, so the answer is "here is what parsed and
    /// the file did not parse cleanly" rather than a confident short list.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["rs"],
            Language::Python => &["py", "pyi"],
            Language::Go => &["go"],
            Language::C => &["c", "h"],
            Language::Bash => &["sh", "bash"],
            Language::Json => &["json"],
        }
    }

    /// The grammar's own name for a language, for callers that already know it.
    pub fn of_name(name: &str) -> Option<Language> {
        let n = name.trim().to_ascii_lowercase();
        Language::ALL.iter().copied().find(|l| l.name() == n)
    }

    /// Pick a grammar from a path, or say why none applies.
    ///
    /// The error is the point. A tool that answered "no definitions" for a
    /// `.tf` file would be making a claim about Terraform it has no parser for.
    pub fn of_path(path: &str) -> Result<Language, UnknownLanguage> {
        let name = path.rsplit('/').next().unwrap_or(path);
        // Extension-less scripts are real: `configure`, `bootstrap`. A shebang is
        // the only honest way to know, and the caller does not have the bytes
        // here, so those come back as unknown with `extension: None` and the
        // caller can offer `language` explicitly.
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        match &ext {
            Some(e) => Language::ALL
                .iter()
                .copied()
                .find(|l| l.extensions().contains(&e.as_str()))
                .ok_or_else(|| UnknownLanguage {
                    path: path.to_string(),
                    extension: ext.clone(),
                }),
            None => Err(UnknownLanguage {
                path: path.to_string(),
                extension: None,
            }),
        }
    }

    /// The rano language this outline knows how to walk.
    ///
    /// Outline's list is a real **subset** of rano's: `Language` here means
    /// "languages whose definitions this module knows how to name", which is a
    /// different question from "languages rano can parse". Rano has 28; naming the
    /// definitions in all of them is work, and doing it badly would print a symbol
    /// table that is quietly wrong, so the six stay six until each is checked
    /// against `examples/dump.rs`.
    pub fn lang(self) -> Lang {
        match self {
            Language::Rust => Lang::Rust,
            Language::Python => Lang::Python,
            Language::Go => Lang::Go,
            Language::C => Lang::C,
            Language::Bash => Lang::Bash,
            Language::Json => Lang::Json,
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// No grammar covers this path.
///
/// Carries the extension so the caller can name it, rather than saying "that file
/// type" and leaving the model to guess which part of the path was the problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownLanguage {
    pub path: String,
    /// `None` when the name has no extension at all.
    pub extension: Option<String>,
}

impl fmt::Display for UnknownLanguage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.extension {
            Some(e) => write!(f, "no grammar for `.{e}` (`{}`)", self.path),
            None => write!(f, "`{}` has no extension to select a grammar", self.path),
        }
    }
}

impl std::error::Error for UnknownLanguage {}

/// Every supported language and its extensions, for a caller writing a refusal.
pub fn supported() -> Vec<(&'static str, &'static [&'static str])> {
    Language::ALL
        .iter()
        .map(|l| (l.name(), l.extensions()))
        .collect()
}

/// One definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// A short word from a fixed set: `fn`, `struct`, `enum`, `trait`, `impl`,
    /// `mod`, `const`, `static`, `type`, `union`, `macro`, `class`, `method`,
    /// `interface`, `var`, `prototype`, `key`. Stable enough to filter on.
    pub kind: &'static str,
    pub name: String,
    /// The enclosing definition's name, when there is one: `Editor` for a method
    /// in `impl Editor`, the class for a Python method, the object key path for
    /// JSON. `impl Editor { fn insert }` is what the model asked for; `fn insert`
    /// on its own sends it looking for a free function that does not exist.
    pub container: Option<String>,
    /// The declaration as written, collapsed to one line and cut at the body.
    /// `pub fn insert(&mut self, c: char) -> bool` — enough to call it without a
    /// second `read`.
    pub signature: String,
    /// 1-based, inclusive.
    pub line: usize,
    pub end_line: usize,
    /// Nesting depth among definitions, not in the syntax tree. Top level is 0.
    pub depth: usize,
}

impl Symbol {
    /// `Editor::insert`, or `insert` at the top level.
    pub fn qualified(&self) -> String {
        match &self.container {
            Some(c) => format!("{c}::{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// What a file's structure turned out to be.
#[derive(Debug, Clone)]
pub struct Outline {
    pub language: Language,
    pub symbols: Vec<Symbol>,
    /// Lines in the file, so a caller can say "61 definitions over 2114 lines"
    /// rather than a count with no denominator.
    pub lines: usize,
    /// The parse contained an ERROR node.
    ///
    /// Not fatal — tree-sitter recovers and the definitions it did find are
    /// real — but it must be said, because a short outline of a broken file and
    /// a short outline of a small file look identical otherwise. The usual
    /// causes are a genuine syntax error, a `.h` that is actually C++, and a
    /// macro-heavy file.
    pub partial: bool,
}

/// Parse `source` and list what it defines, in source order.
///
/// # What is deliberately not descended into
///
/// The body of a function. A closure, a helper defined inside a function, and a
/// struct declared inside a method are all real definitions and none of them is
/// what "what is the structure of this file" is asking for. Items that live
/// beside functions — Rust's `#[cfg(test)] mod tests`, a Python class at module
/// level — are unaffected, because they are not inside a function body.
pub fn outline(source: &str, language: Language) -> Outline {
    let lines = source.lines().count();
    let mut stream = Stream::new(language.lang());
    stream.push(source);
    let Some(root) = stream.root() else {
        return Outline {
            language,
            symbols: Vec::new(),
            lines,
            partial: true,
        };
    };
    let mut symbols = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    walk(&root, source, language, &mut stack, 0, &mut symbols);
    Outline {
        language,
        symbols,
        lines,
        partial: root.has_error,
    }
}

/// What a node turned out to define, if anything.
struct Def {
    kind: &'static str,
    name: String,
    /// This symbol's OWN container, when the language states it on the
    /// definition rather than by nesting. Go is the case: a method names its
    /// receiver and there is no enclosing block to inherit from. `None` means
    /// inherit whatever the enclosing definition put on the stack.
    container: Option<String>,
    /// What this contributes to its CHILDREN's container. `None` means children
    /// keep the container they already had — a free function does not want its
    /// name prefixed onto anything.
    scope: Option<String>,
    /// Whether definitions nested inside are worth listing.
    descend: bool,
}

fn walk(
    node: &Node,
    src: &str,
    lang: Language,
    stack: &mut Vec<String>,
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    let mut child_depth = depth;
    let mut pushed = false;
    let mut descend = true;

    if let Some(def) = classify(node, src, lang) {
        out.push(Symbol {
            kind: def.kind,
            name: def.name,
            container: def.container.or_else(|| stack.last().cloned()),
            signature: signature(node, src),
            line: node.start_point.row + 1,
            end_line: node.end_point.row + 1,
            depth,
        });
        child_depth = depth + 1;
        descend = def.descend;
        if let Some(scope) = def.scope {
            stack.push(scope);
            pushed = true;
        }
    }

    if descend {
        for child in &node.children {
            walk(child, src, lang, stack, child_depth, out);
        }
    }
    if pushed {
        stack.pop();
    }
}

fn text(node: &Node, src: &str) -> String {
    src.get(node.start..node.end)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The child in field `name`.
///
/// By **field**, not by kind or position, which is what rano's `Node::field` exists
/// for: a grammar states which child is a definition's name, and matching kinds in
/// the order they happen to appear is guessing at a fact the grammar already has.
fn child<'a>(node: &'a Node, name: &str) -> Option<&'a Node> {
    node.children
        .iter()
        .find(|c| c.field.as_deref() == Some(name))
}

fn field(node: &Node, name: &str, src: &str) -> Option<String> {
    child(node, name).map(|n| text(n, src))
}

/// Is this node a definition, and what kind?
///
/// The tables are written from the grammars rather than from memory — see
/// `examples/dump.rs`, which prints the named-node skeleton with field names for
/// one sample per language and is how every kind string below was chosen.
fn classify(node: &Node, src: &str, lang: Language) -> Option<Def> {
    let k = node.kind.as_str();
    // A leaf definition: it has a name and nothing inside it is worth listing.
    let named = |kind: &'static str, field_name: &str| -> Option<Def> {
        Some(Def {
            kind,
            name: field(node, field_name, src)?,
            container: None,
            scope: None,
            descend: false,
        })
    };
    // A definition that holds others, and lends them its name.
    let holder = |kind: &'static str, field_name: &str| -> Option<Def> {
        let name = field(node, field_name, src)?;
        Some(Def {
            kind,
            container: None,
            scope: Some(name.clone()),
            name,
            descend: true,
        })
    };

    match lang {
        Language::Rust => match k {
            "function_item" => named("fn", "name"),
            "function_signature_item" => named("fn", "name"),
            "struct_item" => holder("struct", "name"),
            "enum_item" => holder("enum", "name"),
            "union_item" => holder("union", "name"),
            "trait_item" => holder("trait", "name"),
            "mod_item" => holder("mod", "name"),
            "type_item" => named("type", "name"),
            "const_item" => named("const", "name"),
            "static_item" => named("static", "name"),
            "macro_definition" => named("macro", "name"),
            "impl_item" => {
                // `impl Draw for Editor` and `impl Editor` are different things
                // to read and the same thing to look under: the methods belong
                // to `Editor` either way, so that is the scope the children get.
                let ty = field(node, "type", src)?;
                let name = match field(node, "trait", src) {
                    Some(tr) => format!("{tr} for {ty}"),
                    None => ty.clone(),
                };
                Some(Def {
                    kind: "impl",
                    name,
                    container: None,
                    scope: Some(ty),
                    descend: true,
                })
            }
            _ => None,
        },
        Language::Python => match k {
            "function_definition" => named("fn", "name"),
            "class_definition" => holder("class", "name"),
            // `@decorator def f()` wraps the definition one level down. It is not
            // itself a definition, but it must be descended into, which the
            // default does.
            _ => None,
        },
        Language::Go => match k {
            "function_declaration" => named("fn", "name"),
            "method_declaration" => {
                let name = field(node, "name", src)?;
                let recv = child(node, "receiver").and_then(|r| receiver_type(r, src));
                Some(Def {
                    kind: "method",
                    name,
                    // Go has no `impl` block: the receiver is stated on the
                    // method itself, so it is this symbol's own container and
                    // there is no enclosing scope to inherit from.
                    container: recv,
                    scope: None,
                    descend: false,
                })
            }
            "type_spec" => {
                let name = field(node, "name", src)?;
                let kind = match child(node, "type").map(|t| t.kind.as_str()) {
                    Some("interface_type") => "interface",
                    Some("struct_type") => "struct",
                    _ => "type",
                };
                Some(Def {
                    kind,
                    container: None,
                    scope: Some(name.clone()),
                    name,
                    descend: true,
                })
            }
            "const_spec" => named("const", "name"),
            "var_spec" => named("var", "name"),
            "method_elem" => named("fn", "name"),
            _ => None,
        },
        Language::C => match k {
            "function_definition" => Some(Def {
                kind: "fn",
                name: declarator_name(node, src)?,
                container: None,
                scope: None,
                descend: false,
            }),
            "struct_specifier" => holder("struct", "name"),
            "union_specifier" => holder("union", "name"),
            "enum_specifier" => holder("enum", "name"),
            "type_definition" => named("type", "declarator"),
            "preproc_def" => named("const", "name"),
            "preproc_function_def" => named("macro", "name"),
            "declaration" => {
                // A top-level `declaration` is either a prototype or a global.
                // Both belong in a header's outline; conflating them does not.
                let name = declarator_name(node, src)?;
                let kind = if has_function_declarator(node) {
                    "prototype"
                } else {
                    "var"
                };
                Some(Def {
                    kind,
                    name,
                    container: None,
                    scope: None,
                    descend: false,
                })
            }
            _ => None,
        },
        Language::Bash => match k {
            "function_definition" => named("fn", "name"),
            _ => None,
        },
        Language::Json => match k {
            // A JSON file's structure IS its key tree, and the useful depth is
            // shallow: the top two or three levels of a config are what somebody
            // is looking for, and every leaf is the file itself re-printed.
            "pair" => {
                let key = field(node, "key", src)?;
                let key = key.trim_matches('"').to_string();
                let value_kind = child(node, "value").map(|v| v.kind.as_str());
                let nested = matches!(value_kind, Some("object") | Some("array"));
                Some(Def {
                    kind: "key",
                    container: None,
                    scope: nested.then(|| key.clone()),
                    name: key,
                    descend: nested,
                })
            }
            _ => None,
        },
    }
}

/// C's name is buried under a chain of declarators: `int *f(void)` is
/// `pointer_declarator > function_declarator > identifier`.
fn declarator_name(node: &Node, src: &str) -> Option<String> {
    let mut cur = child(node, "declarator")?;
    for _ in 0..16 {
        match cur.kind.as_str() {
            "identifier" | "field_identifier" | "type_identifier" => return Some(text(cur, src)),
            _ => cur = child(cur, "declarator")?,
        }
    }
    None
}

fn has_function_declarator(node: &Node) -> bool {
    let mut cur = child(node, "declarator");
    for _ in 0..16 {
        let Some(c) = cur else { return false };
        if c.kind == "function_declarator" {
            return true;
        }
        cur = child(c, "declarator");
    }
    false
}

/// `(e *Editor)` -> `Editor`.
fn receiver_type(list: &Node, src: &str) -> Option<String> {
    for c in &list.children {
        if c.kind == "parameter_declaration"
            && let Some(t) = child(c, "type")
        {
            return Some(text(t, src).trim_start_matches('*').to_string());
        }
    }
    None
}

/// The declaration as written, up to where the body starts.
///
/// Cut at the `body` field so a 200-line function contributes one line, and
/// collapsed so a signature broken across five lines — which is most of them, in
/// this workspace — still reads as one.
const SIGNATURE_CAP: usize = 160;

fn signature(node: &Node, src: &str) -> String {
    let start = node.start;
    let end = child(node, "body")
        .or_else(|| child(node, "value"))
        .map(|b| b.start)
        .filter(|b| *b > start)
        .unwrap_or(node.end);
    let raw = src.get(start..end).unwrap_or_default();
    let mut out = String::new();
    let mut space = false;
    for c in raw.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        // A signature broken across five lines -- which is most of them in this
        // workspace -- must read as one, and `fn f( &mut self, x: u8, )` is not
        // one. Whitespace that only existed because of the line break is
        // dropped rather than collapsed.
        let opens = out.ends_with(['(', '[', '<', '&', '!']);
        let closes = matches!(c, ')' | ']' | ',' | ';' | '>');
        if space && !opens && !closes {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    let out = out.trim_end_matches(['{', '=', ':', ' ']).trim_end();
    if out.chars().count() > SIGNATURE_CAP {
        let head: String = out.chars().take(SIGNATURE_CAP).collect();
        format!("{head}…")
    } else {
        out.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(o: &Outline) -> Vec<(&str, String)> {
        o.symbols.iter().map(|s| (s.kind, s.qualified())).collect()
    }

    #[test]
    fn an_extension_selects_a_grammar_and_an_unknown_one_says_so() {
        assert_eq!(Language::of_path("src/main.rs"), Ok(Language::Rust));
        assert_eq!(Language::of_path("a/b/c.py"), Ok(Language::Python));
        assert_eq!(Language::of_path("main.go"), Ok(Language::Go));
        assert_eq!(Language::of_path("vm.h"), Ok(Language::C));

        // The empty-corpus bug, in its structural form: a `.tf` file has no
        // grammar here, and the honest answer names the extension. An empty
        // outline would be a claim about Terraform.
        let e = Language::of_path("infra/main.tf").expect_err("no terraform grammar");
        assert_eq!(e.extension.as_deref(), Some("tf"));
        assert!(e.to_string().contains(".tf"), "{e}");

        // No extension at all is a different miss and reads differently.
        let e = Language::of_path("configure").expect_err("no extension");
        assert_eq!(e.extension, None);
        assert!(e.to_string().contains("no extension"), "{e}");
    }

    #[test]
    fn rust_methods_carry_the_impl_they_belong_to() {
        let src = "\
pub struct Editor { x: u8 }
impl Editor {
    pub fn insert(&mut self, c: char) -> bool { true }
    fn helper() {}
}
impl Draw for Editor {
    fn draw(&self) {}
}
pub fn free() {}
";
        let o = outline(src, Language::Rust);
        assert!(!o.partial);
        assert_eq!(
            kinds(&o),
            vec![
                ("struct", "Editor".to_string()),
                ("impl", "Editor".to_string()),
                ("fn", "Editor::insert".to_string()),
                ("fn", "Editor::helper".to_string()),
                ("impl", "Draw for Editor".to_string()),
                ("fn", "Editor::draw".to_string()),
                ("fn", "free".to_string()),
            ]
        );
        // The whole point of the container: `fn insert` alone sends a reader
        // looking for a free function that is not there.
        let insert = &o.symbols[2];
        assert_eq!(insert.container.as_deref(), Some("Editor"));
        assert_eq!(insert.line, 3);
        assert_eq!(
            insert.signature,
            "pub fn insert(&mut self, c: char) -> bool"
        );
    }

    /// The five patterns the rano session actually wrote, as one question.
    #[test]
    fn the_definitions_a_lexical_pattern_could_not_have_found() {
        let src = "\
// fn commented_out() {}
const NOTE: &str = \"fn in_a_string()\";
    pub fn indented_under_nothing() {}
#[cfg(test)]
mod tests {
    #[test]
    fn nested() {}
}
";
        let o = outline(src, Language::Rust);
        let names: Vec<String> = o.symbols.iter().map(|s| s.qualified()).collect();
        // `^(pub )?fn` misses this one; dropping the anchor finds the two above it.
        assert!(
            names.contains(&"indented_under_nothing".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"tests".to_string()), "{names:?}");
        assert!(names.contains(&"tests::nested".to_string()), "{names:?}");
        // And neither the comment nor the string is a definition.
        assert!(
            !names.iter().any(|n| n.contains("commented_out")),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("in_a_string")),
            "{names:?}"
        );
        assert_eq!(names.len(), 4, "{names:?}");
    }

    #[test]
    fn a_function_body_is_not_descended_into() {
        let src = "\
fn outer() {
    fn inner_helper() {}
    let f = |x| x;
}
";
        let o = outline(src, Language::Rust);
        assert_eq!(kinds(&o), vec![("fn", "outer".to_string())]);
    }

    #[test]
    fn python_methods_carry_their_class_and_decorators_do_not_hide_them() {
        let src = "\
CONST = 1
def top(a, b):
    pass
class Editor:
    def insert(self, c):
        pass
    @property
    def mode(self):
        return 1
";
        let o = outline(src, Language::Python);
        assert_eq!(
            kinds(&o),
            vec![
                ("fn", "top".to_string()),
                ("class", "Editor".to_string()),
                ("fn", "Editor::insert".to_string()),
                ("fn", "Editor::mode".to_string()),
            ]
        );
    }

    #[test]
    fn go_methods_carry_their_receiver() {
        let src = "\
package main
type Editor struct { X int }
type Draw interface { Draw() }
func Top(a int) int { return 0 }
func (e *Editor) Insert(c rune) {}
";
        let o = outline(src, Language::Go);
        assert_eq!(
            kinds(&o),
            vec![
                ("struct", "Editor".to_string()),
                ("interface", "Draw".to_string()),
                ("fn", "Draw::Draw".to_string()),
                ("fn", "Top".to_string()),
                ("method", "Editor::Insert".to_string()),
            ]
        );
    }

    #[test]
    fn c_names_come_out_from_under_the_declarator_chain() {
        let src = "\
#define MAX 3
#define SQ(x) ((x)*(x))
typedef struct Editor { int x; } Editor;
enum Mode { A, B };
static int helper(int a) { return a; }
int *make(void);
int global_var = 3;
";
        let o = outline(src, Language::C);
        let got = kinds(&o);
        assert!(got.contains(&("const", "MAX".to_string())), "{got:?}");
        assert!(got.contains(&("macro", "SQ".to_string())), "{got:?}");
        assert!(got.contains(&("enum", "Mode".to_string())), "{got:?}");
        assert!(got.contains(&("fn", "helper".to_string())), "{got:?}");
        // A prototype is not a definition and is not reported as one.
        assert!(got.contains(&("prototype", "make".to_string())), "{got:?}");
        assert!(got.contains(&("var", "global_var".to_string())), "{got:?}");
    }

    #[test]
    fn bash_and_json_have_a_structure_too() {
        let o = outline(
            "function top() { echo hi; }\nother() { echo bye; }\n",
            Language::Bash,
        );
        assert_eq!(
            kinds(&o),
            vec![("fn", "top".to_string()), ("fn", "other".to_string())]
        );

        let o = outline(r#"{"a": {"b": 1}, "c": [1,2], "d": "x"}"#, Language::Json);
        let got = kinds(&o);
        assert!(got.contains(&("key", "a".to_string())), "{got:?}");
        assert!(got.contains(&("key", "a::b".to_string())), "{got:?}");
        assert!(got.contains(&("key", "d".to_string())), "{got:?}");
    }

    /// A file that does not parse gives a PARTIAL outline and says so. A short
    /// outline of a broken file and a short outline of a small file are
    /// different facts.
    #[test]
    fn a_broken_file_is_reported_as_partial_not_as_small() {
        let src = "fn good() {}\nfn bad( { \nstruct After;\n";
        let o = outline(src, Language::Rust);
        assert!(o.partial, "the parse recovered but it did not succeed");
        assert!(
            o.symbols.iter().any(|s| s.name == "good"),
            "what parsed is still real: {:?}",
            o.symbols
        );
    }

    #[test]
    fn an_empty_file_is_empty_and_not_broken() {
        let o = outline("", Language::Rust);
        assert!(o.symbols.is_empty());
        assert!(!o.partial);
        assert_eq!(o.lines, 0);
    }
}
