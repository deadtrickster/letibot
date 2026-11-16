//! `letibot-mcp-fake`: the test server in [`fake`].

#[path = "../fake.rs"]
mod fake;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    fake::serve(&args);
}
