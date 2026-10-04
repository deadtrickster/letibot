//! Fetch one address through the crate's fetcher, printing exactly what the
//! model would see — the debug aid for this crate's policies.
//!
//! ```sh
//! cargo run -q -p letibot-webfetch --example fetch -- https://github.com/romkatv/gitstatus
//! ```
//!
//! Exists because every policy in this crate is meant to be checkable from a
//! shell: the github rewrite, the gh door, the cap, the notes. A policy you can
//! only observe through a running daemon is a policy nobody re-checks.
use letibot_tools::builtins::external::web::{FetchRequest, Fetcher, PageFormat};

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args.next().expect("one url");
    let format = match args.next().as_deref() {
        Some("text") => PageFormat::Text,
        Some("html") => PageFormat::Html,
        _ => PageFormat::Markdown,
    };
    let f = letibot_webfetch::CurlFetcher::attach().expect("curl");
    println!("describe: {}", f.describe());
    println!("asked:    {url}");
    match f.fetch(&FetchRequest { url, format }) {
        Ok(p) => {
            println!(
                "answered: {} — {} {}, {} bytes{}",
                p.final_url,
                p.status,
                p.content_type,
                p.bytes,
                if p.truncated { " TRUNCATED" } else { "" }
            );
            for n in &p.notes {
                println!("note: {n}");
            }
            println!("---");
            println!("{}", p.body);
        }
        Err(e) => println!("error: {e:?}"),
    }
}
