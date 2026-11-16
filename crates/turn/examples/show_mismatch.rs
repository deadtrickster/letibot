//! The two sentences a refused frame produces, side by side, readable without a server, since a
//! patched server cannot produce the frames. `cargo run --example show_mismatch -p letibot-turn`
fn main() {
    for (name, e) in [
        (
            "WITHHELD — the server counted more than it sent (an unpatched server)",
            letibot_turn::stream::StreamError::FrameMismatch {
                n_decoded: 3,
                previous: 1,
                ids: 1,
                reading: letibot_turn::stream::Mismatch::Withheld,
            },
        ),
        (
            "OVER-SENT — more ids than counted (NOT the UTF-8 gate)",
            letibot_turn::stream::StreamError::FrameMismatch {
                n_decoded: 1,
                previous: 0,
                ids: 2,
                reading: letibot_turn::stream::Mismatch::OverSent,
            },
        ),
    ] {
        println!("=== {name}\n{e}\n");
    }
    println!(
        "A grep for `withheld` finds the first and never the second, which is why the\n\
         vocabulary is what it is: the reading's name is the thing an operator searches a\n\
         session log for."
    );
}
