//! **The four hook kinds and nothing else** — the guest behind `../plugin.wasm`.
//!
//! Every hook this host runs is a `(ptr, len) -> i32` call through rano's ABI v1: the input is
//! (ptr, len) in this module's memory, and the answer is the offset of an 8-byte result block —
//! `out_ptr` and `out_len` as little-endian `u32`s — or `-1` for *unchanged*. A hook that answers a
//! constant needs no allocator, no copy and no import, so this is the whole of it: four exports
//! over four static sentences.
//!
//! `condition_engine` is named rather than numbered because a condition's name is the word a
//! `monitor` call asks for (`{"plugin": "engine"}`), the same way `notice_job` names the notice
//! KIND it rewords (`docs/plugins-plan.md` §4.3–§4.4).
//!
//! # How `../plugin.wasm` was built, and how to build it again
//!
//! ```sh
//! rustc --edition 2021 --crate-type cdylib --target wasm32-unknown-unknown \
//!       -C opt-level=s -C panic=abort -C debuginfo=0 \
//!       -o plugin.wasm lib.rs
//! ```
//!
//! It is committed rather than built by the test for the reason `plugins/examples/plan-dashboard`'s
//! module is: a test that shells out to a compiler is a test that fails on a box without a wasm
//! target, and a module this small never changes. It has **no imports at all**, which is what rano's
//! linker grants by default (a plugin's capabilities are letibot's looks, enforced in Rust, not
//! rano capabilities).

#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// Where the host writes a hook's input. Nothing here reads it: each hook answers a constant, so
/// the arena only has to be an address `rano_alloc` may hand back.
const ARENA: i32 = 4096;

/// The result block a hook returns: `out_ptr`, `out_len`.
static mut BLOCK: [u32; 2] = [0, 0];

/// The answer to `notice_job` — a reworded sentence, in this layer's own words.
static NOTICE: &[u8] = br#"{"text":"the engine reworded this notice"}"#;

/// The answer to `on_tool_result` — a suffix, the shape `pr_watch`'s sentence has.
static RESULT: &[u8] = br#"{"append":" (the engine read this result)"}"#;

/// The answer to `on_tool_call` — an answer, so the tool does not run.
static CALL: &[u8] = br#"{"answer":"the engine answered this call"}"#;

/// The answer to `condition_engine` — armed, watching a file, once a minute.
static CONDITION: &[u8] = br#"{"look":{"read":"status.json"},"every_s":60,"state":{"armed":true}}"#;

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
pub extern "C" fn notice_job(_ptr: i32, _len: i32) -> i32 {
    answer(NOTICE)
}

#[no_mangle]
pub extern "C" fn on_tool_result(_ptr: i32, _len: i32) -> i32 {
    answer(RESULT)
}

#[no_mangle]
pub extern "C" fn on_tool_call(_ptr: i32, _len: i32) -> i32 {
    answer(CALL)
}

#[no_mangle]
pub extern "C" fn condition_engine(_ptr: i32, _len: i32) -> i32 {
    answer(CONDITION)
}
