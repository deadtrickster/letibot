//! **rano ABI v1 from the guest's side**: the input the host wrote, an arena to answer in, and
//! the result block.
//!
//! # The ABI in six lines
//!
//! A text hook is `(i32, i32) -> i32`: the host calls `rano_alloc(n)` first, writes the input's
//! JSON at the offset it answered, and then calls the hook with `(offset, len)`. The hook answers
//! the offset of an **8-byte result block** holding `out_ptr` and `out_len` as little-endian
//! `u32`s — or `-1` for *unchanged*, which is the layer passing and the stack asking the next one
//! down (`docs/plugins-plan.md` §2.1). The module must also export `rano_abi() -> 1`.
//!
//! # Why an arena and not the allocator
//!
//! A guest in this host is called in a fresh store each time and may be called every frame, so
//! what it answers has to be bounded work *and* bounded memory. Leaking a `Box` per answer works
//! once; a pane drawn twenty times a second would grow until the host's 64 MiB ceiling. So the
//! answer goes into a fixed arena that [`reset`] empties at the start of every hook, and the
//! input sits in a region of its own — the host may still be reading the request while the guest
//! writes the answer.
//!
//! # Single-threaded, and that is the target's own fact
//!
//! `wasm32-unknown-unknown` with no atomics is one thread of execution, which is what makes the
//! `UnsafeCell`s below sound with no lock: two hooks never overlap, because rano calls one at a
//! time on one store.

use std::cell::UnsafeCell;

/// The largest input a hook accepts. A daemon's session list is tens of kilobytes; the host
/// refuses a bigger one with a sentence rather than the guest trapping on a short arena.
pub const IN_CAP: usize = 1 << 20;

/// The largest answer a hook may write — a view of a few hundred rows is far under it.
pub const OUT_CAP: usize = 1 << 20;

/// The answer the ABI calls *unchanged*: the layer passed, and the stack asks the one below.
pub const UNCHANGED: i32 = -1;

/// A `static` the guest writes through. `Sync` is claimed by hand for the reason the module doc
/// gives: there is one thread, and a hook is called to completion before the next one starts.
struct Shared<T>(UnsafeCell<T>);

// SAFETY: `wasm32-unknown-unknown` without atomics runs one thread, and rano instantiates a
// store per call and joins it before returning — so no two hooks, and no hook and `rano_alloc`,
// ever run at once.
unsafe impl<T> Sync for Shared<T> {}

static INPUT: Shared<[u8; IN_CAP]> = Shared(UnsafeCell::new([0; IN_CAP]));

/// The answer arena, and how far into it the current answer has got.
struct Arena {
    buf: [u8; OUT_CAP],
    at: usize,
}

static OUT: Shared<Arena> = Shared(UnsafeCell::new(Arena {
    buf: [0; OUT_CAP],
    at: 0,
}));

/// The result block the host reads after a hook answers: `out_ptr`, `out_len`.
static BLOCK: Shared<[u32; 2]> = Shared(UnsafeCell::new([0, 0]));

/// **Where the host writes the input** — the address `rano_alloc` hands back.
///
/// `rano_alloc` is exported from here rather than from each guest so that every built-in module
/// answers the same way, and so that a guest's `Cargo.toml` cannot forget it.
#[unsafe(no_mangle)]
pub extern "C" fn rano_alloc(n: i32) -> i32 {
    match usize::try_from(n) {
        Ok(n) if n <= IN_CAP => INPUT.0.get() as usize as i32,
        // Over the arena is a refusal the host reports: `rano_alloc` answered outside the
        // guest's memory. Better a sentence than a trap in the middle of a copy.
        _ => -1,
    }
}

/// **ABI v1**, which is what this host speaks (`docs/plugins-plan.md` §0).
#[unsafe(no_mangle)]
pub extern "C" fn rano_abi() -> i32 {
    1
}

/// The input of the hook being called, as bytes — the region `rano_alloc` answered with.
///
/// # Safety
///
/// Called only from inside a hook, with the `(ptr, len)` rano handed that hook.
pub fn input(ptr: i32, len: i32) -> &'static [u8] {
    let data: &'static [u8] = unsafe { &*INPUT.0.get() };
    let at = usize::try_from(ptr).unwrap_or(usize::MAX);
    match at.checked_add(usize::try_from(len).unwrap_or(usize::MAX)) {
        Some(end) if end <= IN_CAP => &data[at..end],
        // A host that broke its own contract is answered *unchanged* rather than by a panic: a
        // trap in a pane must not be how the head finds out.
        _ => &[],
    }
}

/// **Empty the answer arena** — the first thing every hook does, so what it answers does not
/// depend on what was answered before.
pub fn reset() {
    // SAFETY: see the module doc — one thread, one call at a time.
    unsafe { (*OUT.0.get()).at = 0 };
}

/// **Answer `text`**: copy it into the arena, point the result block at it, and give the host the
/// block's offset. The copy is why the guest needs no allocator and leaves nothing to free.
pub fn answer(text: &str) -> i32 {
    // SAFETY: see the module doc.
    let arena = unsafe { &mut *OUT.0.get() };
    let start = arena.at;
    let end = match start.checked_add(text.len()) {
        Some(end) if end <= OUT_CAP => end,
        // An answer past the arena is a bug in the layer, and *unchanged* is the honest answer:
        // the stack records nothing and the layer below speaks.
        _ => return UNCHANGED,
    };
    arena.buf[start..end].copy_from_slice(text.as_bytes());
    arena.at = end;
    let ptr = arena.buf[start..].as_ptr() as usize as u32;
    // SAFETY: see the module doc.
    unsafe {
        let block = &mut *BLOCK.0.get();
        block[0] = ptr;
        block[1] = text.len() as u32;
    }
    BLOCK.0.get() as usize as i32
}

/// **Answer the hook with what `f` makes of its input** — the three lines every export in
/// `plugins/builtin` is, so a guest's `lib.rs` is its logic and nothing else.
///
/// A hook whose logic answers nothing is `-1`: *unchanged*, the layer passes, and the stack asks
/// the layer below — the fail-down rule of `docs/plugins-plan.md` §1, principle 4, which is what
/// makes a broken built-in a plain list rather than a broken screen.
pub fn hook(ptr: i32, len: i32, f: impl FnOnce(&[u8]) -> Option<String>) -> i32 {
    reset();
    let bytes = input(ptr, len);
    match f(bytes) {
        Some(text) => answer(&text),
        None => UNCHANGED,
    }
}
