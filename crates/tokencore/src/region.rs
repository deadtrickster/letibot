//! The memfd-backed, append-only token region.
//!
//! One region per transcript, appended by the harness and mapped read-only by the server. The
//! public surface is deliberately tiny: no `truncate`, no `clear`, no `pop`, no insert at
//! an index. Growth remaps, and a reader's `(len, head)` is read as a consistent pair, so an
//! append across a growth cannot be seen half-done. A non-Linux box gets the same contract
//! over a file unlinked as soon as it is open.
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
//! `magic` is `LETITOK1`, and `token_size` is 4 so a reader finding another refuses rather than
//! reading garbage at half stride. Header and payload are in one file, so a reader needs one fd.

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

/// A growing, append-only vector of token ids in a sealed memfd. The public surface is tiny by
/// design: no `truncate`, `clear`, `pop` or insert at an index.
pub struct TokenRegion {
    fd: OwnedFd,
    /// Off Linux: a read-only fd opened on the backing file before it was
    /// unlinked, which [`TokenRegion::readonly_fd`] duplicates.
    #[cfg(not(target_os = "linux"))]
    ro: OwnedFd,
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
    /// Create an empty region. `name` is cosmetic (it shows in `/proc/<pid>/fd` as `memfd:<name>`)
    /// and should carry the transcript id, so a `lsof` says which transcript it is.
    pub fn create(name: &str) -> io::Result<Self> {
        let cname = std::ffi::CString::new(format!("letibot-tokens-{name}"))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "region name has a NUL"))?;

        #[cfg(target_os = "linux")]
        let fd = {
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
            fd
        };
        #[cfg(not(target_os = "linux"))]
        let (fd, ro) = unlinked_file(&cname)?;

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
            #[cfg(not(target_os = "linux"))]
            ro,
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
        let want = capacity_tokens
            .saturating_mul(size_of::<TokenId>())
            .saturating_add(HEADER_BYTES);
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        want.div_ceil(page).saturating_mul(page)
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

    /// Bytes this region is mapped at: the file's length and the ceiling the kernel charges this
    /// process for it. What it is NOT is the bytes actually resident.
    pub fn bytes(&self) -> usize {
        self.mapped
    }

    /// Every token appended so far, in order. Borrows `&self`, so it cannot be alive across an
    /// append, which is what makes the remap on growth safe.
    pub fn as_slice(&self) -> &[TokenId] {
        unsafe { std::slice::from_raw_parts(self.tokens_ptr() as *const TokenId, self.len) }
    }

    /// Append tokens and publish the new chain head: the only mutation this type has. Appending zero
    /// tokens is legal and meaningful, because a `SegmentMark` renders to nothing.
    pub fn append(&mut self, tokens: &[TokenId], head_hash: &[u8; 32]) -> io::Result<()> {
        let needed = self.len.checked_add(tokens.len()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::OutOfMemory, "token region overflow")
        })?;
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
        #[cfg(target_os = "linux")]
        let base = unsafe {
            libc::mremap(
                self.base as *mut libc::c_void,
                self.mapped,
                bytes,
                libc::MREMAP_MAYMOVE,
            )
        };
        // No `mremap` here: map the grown file anew, then drop the old mapping.
        // The file is the shared state, so the new mapping sees every byte.
        #[cfg(not(target_os = "linux"))]
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.fd.as_raw_fd(),
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        #[cfg(not(target_os = "linux"))]
        unsafe {
            libc::munmap(self.base as *mut libc::c_void, self.mapped)
        };
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

    /// Read `(len, head_hash)` the way a reader must: as a consistent pair. In-process this never
    /// spins, since appending takes the same lock.
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

    /// A read-only duplicate of the fd, for the server: reopened through `/proc/self/fd/N` with
    /// `O_RDONLY`, which makes it genuinely read-only rather than by convention.
    #[cfg(not(target_os = "linux"))]
    pub fn readonly_fd(&self) -> io::Result<OwnedFd> {
        self.ro.try_clone()
    }

    #[cfg(target_os = "linux")]
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

/// The non-Linux stand-in for a memfd: a file in the temp dir, unlinked as soon as it is open,
/// so it has no path and dies with its last fd exactly as a memfd does.
#[cfg(not(target_os = "linux"))]
fn unlinked_file(name: &std::ffi::CStr) -> io::Result<(OwnedFd, OwnedFd)> {
    let name = name.to_string_lossy().replace('/', "_");
    let template = std::env::temp_dir().join(format!("{name}.XXXXXX"));
    let mut path = std::ffi::CString::new(template.into_os_string().into_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "temp dir has a NUL"))?
        .into_bytes_with_nul();

    let raw = unsafe { libc::mkstemp(path.as_mut_ptr() as *mut libc::c_char) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let ro = unsafe {
        libc::open(
            path.as_ptr() as *const libc::c_char,
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    let ro_err = io::Error::last_os_error();
    unsafe { libc::unlink(path.as_ptr() as *const libc::c_char) };
    if ro < 0 {
        return Err(ro_err);
    }
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((fd, unsafe { OwnedFd::from_raw_fd(ro) }))
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
    #[cfg(target_os = "linux")]
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

    /// The reader's side of the contract in one process: map read-only, let the writer append across
    /// a growth, and confirm the reader's old mapping still answers.
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
