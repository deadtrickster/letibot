//! The memfd-backed, append-only token region.
//!
//! The plan (§4.3) says "one region per transcript, appended by the harness,
//! mapped read-only by the server" and stops there. `docs/workstreams.md` §7
//! item 3 names the gap: *"that is a policy, not a format"*. This module is the
//! format, and this comment is the written-down decision.
//!
//! # Layout
//!
//! ```text
//!   byte 0                                                        byte 64
//!   +--------+---------+------------+--------+----------+-----------------+
//!   | magic  | version | token_size |  len   | capacity |    head_hash    |
//!   |  u64   |   u32   |    u32     |  u64   |   u64    |    [u8; 32]     |
//!   +--------+---------+------------+--------+----------+-----------------+
//!   byte 64 .. 64 + capacity*4 : token ids, native-endian u32, index 0 first
//! ```
//!
//! `magic` is `LETITOK1`. `token_size` is 4 and exists so a reader that finds a
//! different one refuses rather than reading garbage at half stride. Header and
//! payload are in one file so that a reader needs one fd and one mapping.
//!
//! ## Why a header at all
//!
//! A bare array gives a reader no way to know how much of it is valid. The file
//! length cannot serve: it is rounded up to a page and grown *before* the tokens
//! that fill it are written, so at every growth there is a window in which the
//! file is larger than the data. `len` is the published length, and it is what
//! makes "the bytes the reader sees" a well-defined set.
//!
//! ## Publication order, and the seqlock on (len, head_hash)
//!
//! A writer appends in exactly this order: **tokens, then `head_hash`, then a
//! release store to `len`**. A reader acquire-loads `len` first, so any `len` it
//! observes is backed by tokens that were already written. Because the region is
//! append-only, a reader that saw `len = N` at any past instant can read
//! `[0, N)` at any future instant and get the same ids: the writer never goes
//! back.
//!
//! `head_hash` is the one field that is *not* monotone -- it is replaced on every
//! append -- so reading it together with `len` needs a seqlock, and
//! [`TokenRegion::read_published`] does the standard two-load version of one:
//! load `len`, read the hash, load `len` again, retry if they differ. Without it
//! a reader could pair length N with the hash of N+1 and conclude the two
//! processes were looking at different conversations, which is precisely the
//! alarm §4.3 clause 4 wants to be trustworthy.
//!
//! # Growth
//!
//! Capacity starts at 16 KiB of tokens (64 KiB + one page, ~1 % of a small
//! conversation) and doubles. Growth is `ftruncate` up, then
//! `mremap(MREMAP_MAYMOVE)` in the writer. A 200 k-token conversation reaches
//! its final size in seven growths.
//!
//! **Growth never invalidates a reader.** The first N pages of the file are
//! byte-identical before and after an `ftruncate` up, so a reader's existing
//! mapping stays valid and stays correct; it only needs to re-`mmap` when it
//! wants to read past what it mapped, and `capacity` tells it what to ask for.
//! In *this* process the mapping may move, which is why [`TokenRegion::append`]
//! takes `&mut self` and [`TokenRegion::as_slice`] borrows `&self`: the borrow
//! checker, not a convention, is what stops a slice from outliving a remap.
//!
//! A reserve-a-huge-`PROT_NONE`-range-up-front scheme was considered, which
//! would keep the writer's base pointer stable and let a reader map once. It was
//! rejected: it buys nothing here (no slice can outlive `&mut self` anyway), and
//! it puts a hard ceiling on conversation length in exchange.
//!
//! # Sealing: the kernel enforces the invariant, not just the API
//!
//! The memfd is created with `MFD_ALLOW_SEALING` and immediately sealed
//! `F_SEAL_SHRINK`. Measured on this box: after that seal, `ftruncate` **up**
//! still succeeds and `ftruncate` **down** returns `EPERM`. So the file
//! underlying a transcript cannot be shortened by this process, by a bug in this
//! process, or by anyone holding the fd -- the impossibility is in the kernel,
//! below every line of Rust in this crate.
//!
//! [`TokenRegion::readonly_fd`] reopens the memfd through `/proc/self/fd/N` with
//! `O_RDONLY`. Also measured: a `MAP_SHARED | PROT_WRITE` mapping of that fd
//! fails with `EACCES` and `ftruncate` on it fails with `EINVAL`. That is the fd
//! to send to the server over `SCM_RIGHTS`; the reader is then structurally a
//! reader.
//!
//! # Lifecycle, and what a restart rebuilds from
//!
//! **The region is deliberately volatile.** A memfd is anonymous: it dies with
//! the process, and there is no path to it on disk. The durable copy of the
//! tokens is the `transcript_item.tokens` column (`crate::store`), written in
//! the same transaction as the ledger row that describes it, so the two can
//! never disagree.
//!
//! The alternative -- a file-backed mapping that survives a restart -- was
//! rejected because it creates a second durable copy of the truth. A crash
//! between "tokens are in the file" and "row is in SQLite" leaves two stores
//! that disagree, and reconciling them means deciding which one is right, which
//! is a decision nobody can make correctly after the fact. pi's durable-harness
//! rule quoted in §4.4 -- *"there is no third place"* -- is the same argument.
//!
//! So on restart the region is recreated empty and refilled by **replaying the
//! ledger's spans** (§4.3 clause 6), never by re-rendering. Re-rendering would
//! reintroduce exactly the renderer non-determinism the hash chain exists to
//! catch, and it would do so during recovery, when nobody is looking.
//! [`crate::ledger::TokenLedger::restore`] re-verifies the whole chain as it
//! replays, so a store that has been edited underneath us is caught at startup.
//!
//! # Ownership
//!
//! One writer, created by whoever creates the `TokenLedger` (harnessd). The
//! region is not `Clone` and appending needs `&mut`, so a second writer would
//! have to be a second `TokenRegion` over a second memfd, which is a different
//! transcript. Readers get an fd, never a `&mut`.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::vocab::TokenId;

/// `LETITOK1`, little-endian.
pub const REGION_MAGIC: u64 = u64::from_le_bytes(*b"LETITOK1");
pub const REGION_VERSION: u32 = 1;
/// Bytes of header before the first token.
pub const HEADER_BYTES: usize = 64;
/// Tokens the region can hold before its first growth.
const INITIAL_CAPACITY_TOKENS: usize = 16 * 1024;

#[repr(C)]
struct Header {
    magic: u64,
    version: u32,
    token_size: u32,
    len: AtomicU64,
    capacity: AtomicU64,
    head_hash: [u8; 32],
}

const _: () = assert!(size_of::<Header>() == HEADER_BYTES);

/// A growing, append-only vector of token ids in a sealed memfd.
///
/// The public surface is deliberately tiny. There is no `truncate`, no `clear`,
/// no `pop`, no `insert`, no `as_mut_slice`, no `IndexMut`, and
/// `crate::ledger`'s `no_rewrite_operation_exists` test fails the build if
/// anybody adds one without changing the test's allowlist on purpose.
pub struct TokenRegion {
    fd: OwnedFd,
    /// Base of the mapping, header first. Moves on growth.
    base: *mut u8,
    /// Bytes currently mapped.
    mapped: usize,
    /// Tokens the file can currently hold.
    capacity: usize,
    /// The writer's own count. Mirrors `Header::len`, which is the reader's.
    len: usize,
}

// The mapping belongs to the process, not to a thread, and every mutation goes
// through `&mut self`. Not `Sync`: two threads appending would be two writers.
unsafe impl Send for TokenRegion {}

impl TokenRegion {
    /// Create an empty region.
    ///
    /// `name` is cosmetic (it shows up in `/proc/<pid>/fd` as
    /// `memfd:<name>`) and should carry the transcript id so a `lsof` tells you
    /// which conversation a mapping belongs to.
    pub fn create(name: &str) -> io::Result<Self> {
        let cname = std::ffi::CString::new(format!("letibot-tokens-{name}"))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "region name has a NUL"))?;

        let raw: RawFd = unsafe {
            libc::memfd_create(
                cname.as_ptr(),
                (libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) as libc::c_uint,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };

        // Below every line of Rust in this crate: the kernel now refuses to
        // shorten this file. Growth is unaffected.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_SHRINK) } < 0 {
            return Err(io::Error::last_os_error());
        }

        let bytes = Self::bytes_for(INITIAL_CAPACITY_TOKENS);
        if unsafe { libc::ftruncate(fd.as_raw_fd(), bytes as libc::off_t) } < 0 {
            return Err(io::Error::last_os_error());
        }

        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let base = base as *mut u8;

        let capacity = (bytes - HEADER_BYTES) / size_of::<TokenId>();
        let region = TokenRegion {
            fd,
            base,
            mapped: bytes,
            capacity,
            len: 0,
        };
        // memfd pages start zeroed, so every field is written explicitly.
        unsafe {
            let h = &mut *(region.base as *mut Header);
            h.magic = REGION_MAGIC;
            h.version = REGION_VERSION;
            h.token_size = size_of::<TokenId>() as u32;
            h.head_hash = [0u8; 32];
        }
        region
            .header()
            .capacity
            .store(capacity as u64, Ordering::Release);
        region.header().len.store(0, Ordering::Release);
        Ok(region)
    }

    fn bytes_for(capacity_tokens: usize) -> usize {
        let want = HEADER_BYTES + capacity_tokens * size_of::<TokenId>();
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        want.div_ceil(page) * page
    }

    fn header(&self) -> &Header {
        // Safety: `base` is a live mapping of at least HEADER_BYTES, and the
        // header was initialised in `create`.
        unsafe { &*(self.base as *const Header) }
    }

    fn tokens_ptr(&self) -> *mut TokenId {
        unsafe { self.base.add(HEADER_BYTES) as *mut TokenId }
    }

    /// Number of tokens appended so far.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Tokens the file can hold before its next growth.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Every token appended so far, in order.
    ///
    /// Borrows `&self`, so it cannot be alive across an [`append`](Self::append)
    /// -- which is what makes the remap on growth safe without any convention
    /// for callers to remember.
    pub fn as_slice(&self) -> &[TokenId] {
        unsafe { std::slice::from_raw_parts(self.tokens_ptr() as *const TokenId, self.len) }
    }

    /// Append tokens and publish the new chain head.
    ///
    /// The **only** mutation this type has. Appending zero tokens is legal and
    /// meaningful: a `SegmentMark` renders to nothing but still takes a ledger
    /// row and still advances the hash chain.
    pub fn append(&mut self, tokens: &[TokenId], head_hash: &[u8; 32]) -> io::Result<()> {
        let needed = self.len + tokens.len();
        if needed > self.capacity {
            self.grow_to(needed)?;
        }

        // 1. tokens
        unsafe {
            std::ptr::copy_nonoverlapping(
                tokens.as_ptr(),
                self.tokens_ptr().add(self.len),
                tokens.len(),
            );
        }
        // 2. head hash
        unsafe {
            let h = &mut *(self.base as *mut Header);
            h.head_hash = *head_hash;
        }
        // 3. release-store the length. Everything above is visible to any reader
        //    that acquire-loads this value.
        self.len = needed;
        self.header().len.store(needed as u64, Ordering::Release);
        Ok(())
    }

    fn grow_to(&mut self, needed_tokens: usize) -> io::Result<()> {
        let mut capacity = self.capacity.max(INITIAL_CAPACITY_TOKENS);
        while capacity < needed_tokens {
            capacity = capacity.checked_mul(2).ok_or_else(|| {
                io::Error::new(io::ErrorKind::OutOfMemory, "token region overflow")
            })?;
        }
        let bytes = Self::bytes_for(capacity);

        if unsafe { libc::ftruncate(self.fd.as_raw_fd(), bytes as libc::off_t) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let base = unsafe {
            libc::mremap(
                self.base as *mut libc::c_void,
                self.mapped,
                bytes,
                libc::MREMAP_MAYMOVE,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        self.base = base as *mut u8;
        self.mapped = bytes;
        self.capacity = (bytes - HEADER_BYTES) / size_of::<TokenId>();
        self.header()
            .capacity
            .store(self.capacity as u64, Ordering::Release);
        Ok(())
    }

    /// The last published chain head.
    pub fn head_hash(&self) -> [u8; 32] {
        self.header().head_hash
    }

    /// Read `(len, head_hash)` the way a *reader* must: as a consistent pair.
    ///
    /// See the module docs on the seqlock. In-process this can never spin, since
    /// appending needs `&mut self`; it exists so the reader-side algorithm is
    /// written down in one place and testable here rather than reinvented in the
    /// server.
    pub fn read_published(&self) -> (usize, [u8; 32]) {
        loop {
            let first = self.header().len.load(Ordering::Acquire);
            let hash = self.header().head_hash;
            let second = self.header().len.load(Ordering::Acquire);
            if first == second {
                return (first as usize, hash);
            }
        }
    }

    /// A read-only duplicate of the fd, for the server.
    ///
    /// Reopened through `/proc/self/fd/N` with `O_RDONLY`, which is what makes
    /// it genuinely read-only rather than merely intended to be: measured, a
    /// `MAP_SHARED | PROT_WRITE` mapping of the result fails `EACCES` and
    /// `ftruncate` fails `EINVAL`.
    pub fn readonly_fd(&self) -> io::Result<OwnedFd> {
        let path = std::ffi::CString::new(format!("/proc/self/fd/{}", self.fd.as_raw_fd()))
            .expect("no NUL in a /proc path");
        let raw = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }

    /// The writable fd. Only for tests and for a caller that must `fstat` it.
    pub fn as_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl Drop for TokenRegion {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.base as *mut libc::c_void, self.mapped) };
    }
}

impl std::fmt::Debug for TokenRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenRegion")
            .field("len", &self.len)
            .field("capacity", &self.capacity)
            .field("mapped_bytes", &self.mapped)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    /// The region's central claim, tested against the kernel rather than
    /// against this crate: the file backing a transcript cannot be shortened,
    /// by us or by anybody else holding the descriptor.
    #[test]
    fn the_kernel_refuses_to_shrink_the_region() {
        let mut r = TokenRegion::create("seal").unwrap();
        r.append(&[1, 2, 3], &[0; 32]).unwrap();

        let smaller = (HEADER_BYTES + 4) as libc::off_t;
        let rc = unsafe { libc::ftruncate(r.as_fd(), smaller) };
        assert_eq!(rc, -1, "F_SEAL_SHRINK must refuse a shrink");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );

        // ...and growth is unaffected, which is what makes the seal usable.
        let bigger = (HEADER_BYTES + 1024 * 1024) as libc::off_t;
        assert_eq!(unsafe { libc::ftruncate(r.as_fd(), bigger) }, 0);
        assert_eq!(r.as_slice(), &[1, 2, 3]);
    }

    /// The fd handed to the server is a reader, structurally.
    #[test]
    fn a_readonly_fd_can_be_read_but_not_written_or_truncated() {
        let mut r = TokenRegion::create("ro").unwrap();
        r.append(&[10, 20, 30], &[7; 32]).unwrap();
        let ro = r.readonly_fd().unwrap();
        let bytes = HEADER_BYTES + 3 * 4;

        let w = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                ro.as_raw_fd(),
                0,
            )
        };
        assert_eq!(
            w,
            libc::MAP_FAILED,
            "a writable shared mapping must be refused"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EACCES)
        );

        assert_eq!(
            unsafe { libc::ftruncate(ro.as_raw_fd(), 0) },
            -1,
            "a read-only fd must not resize the region"
        );

        let ro_map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ,
                libc::MAP_SHARED,
                ro.as_raw_fd(),
                0,
            )
        };
        assert_ne!(ro_map, libc::MAP_FAILED, "a read-only mapping must work");
        let seen = unsafe {
            std::slice::from_raw_parts((ro_map as *const u8).add(HEADER_BYTES) as *const TokenId, 3)
        };
        assert_eq!(seen, &[10, 20, 30]);
        unsafe { libc::munmap(ro_map, bytes) };
    }

    /// The reader's side of the contract, played out in one process: map the
    /// region read-only, let the writer append across a growth, and confirm the
    /// reader's *old* mapping is still correct for its old length while a fresh
    /// mapping sees the new one. This is what "growth never invalidates a
    /// reader" means, and it is the property that lets the server hold a
    /// mapping across a turn.
    #[test]
    fn growth_never_invalidates_a_readers_existing_mapping() {
        let mut r = TokenRegion::create("reader").unwrap();
        let first: Vec<TokenId> = (0..1000).collect();
        r.append(&first, &[1; 32]).unwrap();

        let ro = r.readonly_fd().unwrap();
        let old_bytes = HEADER_BYTES + first.len() * 4;
        let old_map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                old_bytes,
                libc::PROT_READ,
                libc::MAP_SHARED,
                ro.as_raw_fd(),
                0,
            )
        };
        assert_ne!(old_map, libc::MAP_FAILED);
        let (published_len, _) = r.read_published();
        assert_eq!(published_len, 1000);

        // Force several growths past the initial 16 Ki capacity.
        let more: Vec<TokenId> = (0..50_000).map(|i| 100_000 + i).collect();
        r.append(&more, &[2; 32]).unwrap();
        assert!(r.capacity() > INITIAL_CAPACITY_TOKENS, "must have grown");

        // The reader's old mapping still reads its old prefix, unchanged.
        let old_view = unsafe {
            std::slice::from_raw_parts(
                (old_map as *const u8).add(HEADER_BYTES) as *const TokenId,
                1000,
            )
        };
        assert_eq!(old_view, first.as_slice());

        // A fresh mapping, sized from the header, sees everything.
        let (len, head) = r.read_published();
        assert_eq!(len, 51_000);
        assert_eq!(head, [2; 32]);
        let new_bytes = HEADER_BYTES + len * 4;
        let new_map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                new_bytes,
                libc::PROT_READ,
                libc::MAP_SHARED,
                ro.as_raw_fd(),
                0,
            )
        };
        assert_ne!(new_map, libc::MAP_FAILED);
        let new_view = unsafe {
            std::slice::from_raw_parts(
                (new_map as *const u8).add(HEADER_BYTES) as *const TokenId,
                len,
            )
        };
        assert_eq!(&new_view[..1000], first.as_slice());
        assert_eq!(&new_view[1000..], more.as_slice());

        unsafe {
            libc::munmap(old_map, old_bytes);
            libc::munmap(new_map, new_bytes);
        }
    }

    #[test]
    fn the_header_says_what_it_is() {
        let r = TokenRegion::create("hdr").unwrap();
        let h = r.header();
        assert_eq!(h.magic, REGION_MAGIC);
        assert_eq!(h.version, REGION_VERSION);
        assert_eq!(h.token_size, 4);
        assert_eq!(h.len.load(Ordering::Acquire), 0);
        assert!(h.capacity.load(Ordering::Acquire) >= INITIAL_CAPACITY_TOKENS as u64);
    }

    #[test]
    fn an_empty_append_publishes_a_new_head_and_no_tokens() {
        let mut r = TokenRegion::create("empty").unwrap();
        r.append(&[5], &[1; 32]).unwrap();
        r.append(&[], &[9; 32]).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r.read_published(), (1, [9; 32]));
    }
}
