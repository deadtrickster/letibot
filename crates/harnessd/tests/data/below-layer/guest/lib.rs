//! **The smallest layer that asks below** — the guest behind `../plugin.wasm`.
//!
//! Called the first time, it answers `{"below": {}}`: *resolve the layer below me with no input
//! and hand me its answer*. Called again — the stack asks the same layer a second time, with the
//! original input plus `"below": <the lower answer>` — it looks for the built-in's own title in
//! what it was handed and says whether it found it.
//!
//! Two facts and no JSON parser: this is a fixture for the STACK's protocol, and the fixture's
//! job is to be small enough to read at a glance. `plugins/examples/sessions-override` is the
//! same idea done properly, with serde.
//!
//! # How `../plugin.wasm` was built, and how to build it again
//!
//! ```sh
//! rustc --edition 2021 --crate-type cdylib --target wasm32-unknown-unknown \
//!       -C opt-level=s -C panic=abort -C debuginfo=0 \
//!       -o plugin.wasm lib.rs
//! ```
//!
//! It is committed for the reason `data/plugin-engine/plugin.wasm` is: a test that shells out to
//! a compiler fails on a box without a wasm target, and a module this small never changes. It has
//! **no imports**.

#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// Where the host writes the input — and where this module's own answers live, because they are
/// `&'static [u8]` already in linear memory.
const ARENA: i32 = 8192;

/// The result block a hook returns: `out_ptr`, `out_len`.
static mut BLOCK: [u32; 2] = [0, 0];

/// The first call's answer is built in this buffer: `{"below":` + the input it was given + `}`.
/// **The input is passed through**, because what a layer asks below with is the request it was
/// asked — a fixture that sent an empty object would be asking the layer below a different
/// question, and the built-in would have no feed to draw from.
static mut ASK: [u8; 4096] = [0; 4096];

/// The second call's answers — the built-in's title was in what came back, or it was not.
static FOUND: &[u8] = br#"{"title":"below found the sessions built-in"}"#;
static MISSED: &[u8] = br#"{"title":"below found nothing below it"}"#;

/// What the built-in answers, as bytes — the mark this fixture looks for.
static MARK: &[u8] = b"sessions in this daemon";

#[no_mangle]
pub extern "C" fn rano_abi() -> i32 {
    // ABI v1, which is what rano's host speaks.
    1
}

#[no_mangle]
pub extern "C" fn rano_alloc(_n: i32) -> i32 {
    ARENA
}

fn answer(bytes: &'static [u8]) -> i32 {
    unsafe {
        let block = core::ptr::addr_of_mut!(BLOCK) as *mut u32;
        core::ptr::write_unaligned(block, bytes.as_ptr() as u32);
        core::ptr::write_unaligned(block.add(1), bytes.len() as u32);
        core::ptr::addr_of_mut!(BLOCK) as i32
    }
}

/// `{"below": <input>}` — the request this layer was asked, handed to the layer below it.
fn ask_below(input: &[u8]) -> i32 {
    let head = b"{\"below\":";
    let room = 4096;
    if head.len() + input.len() + 1 > room {
        return -1;
    }
    unsafe {
        let buf = &mut *core::ptr::addr_of_mut!(ASK);
        buf[..head.len()].copy_from_slice(head);
        buf[head.len()..head.len() + input.len()].copy_from_slice(input);
        buf[head.len() + input.len()] = b'}';
        answer_bytes(&buf[..head.len() + input.len() + 1])
    }
}

/// [`answer`] for bytes that are not `'static` — they are in this module's own buffer, which
/// outlives the call.
fn answer_bytes(bytes: &[u8]) -> i32 {
    unsafe {
        let block = core::ptr::addr_of_mut!(BLOCK) as *mut u32;
        core::ptr::write_unaligned(block, bytes.as_ptr() as u32);
        core::ptr::write_unaligned(block.add(1), bytes.len() as u32);
        core::ptr::addr_of_mut!(BLOCK) as i32
    }
}

/// Whether `hay` holds `needle` — the whole of this module's parsing.
fn holds(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    (0..=hay.len() - needle.len()).any(|at| &hay[at..at + needle.len()] == needle)
}

#[no_mangle]
pub extern "C" fn pane_sessions(ptr: i32, len: i32) -> i32 {
    // The host wrote the input at the offset `rano_alloc` answered and told us how long it is —
    // and the SECOND call's input is this one plus `"below": …`, so the length is the whole of
    // what this fixture needs to read.
    let at = ptr as usize as *const u8;
    let len = len.max(0) as usize;
    let input = unsafe { core::slice::from_raw_parts(at, len) };
    if holds(input, b"\"below\"") {
        if holds(input, MARK) {
            answer(FOUND)
        } else {
            answer(MISSED)
        }
    } else {
        ask_below(input)
    }
}
