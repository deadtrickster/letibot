//! Print the named-node skeleton of a sample file per language, with field names.
//!
//! This is how every node kind and field name in `classify` was chosen. Written
//! down from the grammar rather than from memory, because a kind string that is
//! subtly wrong -- `type_declaration` where the grammar says `type_spec` -- does
//! not fail, it silently drops that definition from every outline, which is the
//! empty-corpus bug with a smaller blast radius.
//!
//!     cargo run -p letibot-code --example dump
//!
//! Run it again after a grammar bump. A grammar is somebody else's tree and it
//! changes under you; the kinds `classify` matches on are a contract nobody
//! signed.
//!
//! It reads rano's `Node` — the same plain data `classify` walks — rather than
//! tree-sitter's, so what it prints is exactly what the outline sees. The field
//! names come from `Node::field`, which is the same source `classify` reads them
//! from.

use rano::syntax::{Lang, Node, Stream};

const MAX_DEPTH: usize = 4;

fn walk(n: &Node, src: &str, d: usize, out: &mut String) {
    if d > MAX_DEPTH {
        return;
    }
    if n.named {
        let fields: Vec<String> = n
            .children
            .iter()
            .filter_map(|c| {
                let f = c.field.as_deref()?;
                let head: String = src
                    .get(c.start..c.end)
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(30)
                    .collect();
                Some(format!("{f}={}:{head}", c.kind))
            })
            .collect();
        out.push_str(&format!(
            "{}{} [{}]\n",
            "  ".repeat(d),
            n.kind,
            fields.join(", ")
        ));
    }
    for c in &n.children {
        walk(c, src, d + 1, out);
    }
}

fn main() {
    // One sample per language, holding one of everything `classify` matches on.
    let cases: Vec<(&str, Lang, &str)> = vec![
        (
            "rust",
            Lang::Rust,
            r#"
pub mod thing;
pub const MAX: usize = 3;
static NAME: &str = "n";
pub type Alias = u8;
pub struct Editor { x: u8 }
pub enum Mode { A, B }
pub trait Draw { fn draw(&self); }
impl Editor { pub fn insert(&mut self, c: char) {} }
impl Draw for Editor { fn draw(&self) {} }
pub fn top(a: u8) -> u8 { 0 }
macro_rules! m { () => {} }
mod inner { pub fn nested() {} }
union U { a: u8 }
"#,
        ),
        (
            "python",
            Lang::Python,
            r#"
CONST = 3
def top(a, b):
    pass
class Editor:
    def insert(self, c):
        pass
    @property
    def mode(self):
        return 1
"#,
        ),
        (
            "go",
            Lang::Go,
            r#"
package main
const Max = 3
var Name = "n"
type Editor struct { X int }
type Draw interface { Draw() }
func Top(a int) int { return 0 }
func (e *Editor) Insert(c rune) {}
"#,
        ),
        (
            "c",
            Lang::C,
            r#"
#define MAX 3
#define SQ(x) ((x)*(x))
typedef struct Editor { int x; } Editor;
enum Mode { A, B };
union U { int a; };
static int helper(int a) { return a; }
int *make(void);
int global_var = 3;
"#,
        ),
        (
            "bash",
            Lang::Bash,
            "NAME=value\nfunction top() { echo hi; }\nother() { echo bye; }\n",
        ),
        (
            "json",
            Lang::Json,
            r#"{"a": {"b": 1}, "c": [1,2], "d": "x"}"#,
        ),
    ];
    for (name, lang, src) in cases {
        let mut stream = Stream::new(lang);
        stream.push(src);
        let root = stream.root().expect("parse");
        let mut out = String::new();
        walk(&root, src, 0, &mut out);
        println!("===== {name} =====\n{out}");
    }
}
