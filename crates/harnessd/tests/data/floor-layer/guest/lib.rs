//! The `adjudicate_` floor and nothing else - the guest behind `../plugin.wasm`.
//!
//! One export over one static sentence: a project says *this action is a production host, so a
//! person decides it*. It exists so `crates/harnessd/src/harness/open.rs`'s test can prove a
//! daemon-opened session's gate asks its layers at a real wasm boundary, not through a table.
//!
//! A hook is a `(ptr, len) -> i32` call through rano's ABI v1: the answer is the offset of an
//! 8-byte result block - `out_ptr` and `out_len` as little-endian `u32`s - or `-1` for *unchanged*.
//! Nothing here reads the input, so there is no allocator and no copy.
//!
//! # How `../plugin.wasm` was built, and how to build it again
//!
//! ```sh
//! rustc --edition 2021 --crate-type cdylib --target wasm32-unknown-unknown \
//!       -C opt-level=s -C panic=abort -C debuginfo=0 \
//!       -o plugin.wasm lib.rs
//! ```
//!
//! Committed for the reason `data/plugin-engine/plugin.wasm` is: a test that shells out to a
//! compiler fails on a box without a wasm target, and this module never changes.

#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// Where the host writes a hook's input. The arena only has to be an address `rano_alloc` may hand
/// back: this hook answers a constant and reads nothing.
const ARENA: i32 = 4096;

/// The result block a hook returns: `out_ptr`, `out_len`.
static mut BLOCK: [u32; 2] = [0, 0];

/// The answer to `adjudicate_floor`: the plan's own example - a named production host, at `ask`.
static FLOOR: &[u8] = br#"{"intents":["prod-host"],"floor":"ask"}"#;

#[no_mangle]
pub extern "C" fn rano_abi() -> i32 {
    // ABI v1, which is what rano's host speaks.
    1
}

#[no_mangle]
pub extern "C" fn rano_alloc(_n: i32) -> i32 {
    ARENA
}

/// Point the result block at `bytes`, which lives in this module's linear memory already, and
/// answer its offset.
fn answer(bytes: &'static [u8]) -> i32 {
    unsafe {
        let block = core::ptr::addr_of_mut!(BLOCK) as *mut u32;
        core::ptr::write_unaligned(block, bytes.as_ptr() as u32);
        core::ptr::write_unaligned(block.add(1), bytes.len() as u32);
        core::ptr::addr_of_mut!(BLOCK) as i32
    }
}

#[no_mangle]
pub extern "C" fn adjudicate_floor(_ptr: i32, _len: i32) -> i32 {
    answer(FLOOR)
}
