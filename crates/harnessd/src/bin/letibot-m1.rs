//! `letibot-m1` — the M1 exit measurement, as its own executable.
//!
//! The work lives in `letibot_harnessd::m1::run` so that this binary and the
//! `letibot` multicall are the same implementation rather than two copies.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(letibot_harnessd::m1::run(&args));
}
