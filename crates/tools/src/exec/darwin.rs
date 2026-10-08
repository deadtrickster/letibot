//! What `/proc` answers on Linux, asked of the Darwin kernel instead.
//!
//! macOS has no `/proc`. The same facts — which processes exist, a process's
//! parent, group, owner and state, its name and its command line — come from
//! `libproc` (`proc_listallpids`, `proc_pidinfo(PROC_PIDTBSDINFO)`) and from
//! `sysctl(KERN_PROCARGS2)`. This module is the one place those calls live, so the
//! callers that used to read `/proc` ask a function instead of a file.

/// One process, as `PROC_PIDTBSDINFO` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub uid: u32,
    /// `SZOMB` and friends.
    pub status: u32,
    /// The short name, at most 16 bytes — `/proc/<pid>/comm`'s counterpart.
    pub comm: String,
    /// Start time, microseconds since the epoch. With the pid, an identity: a
    /// reused pid has a different start.
    pub start_us: u64,
}

impl Info {
    pub fn zombie(&self) -> bool {
        self.status == libc::SZOMB
    }
}

/// Every pid the kernel will list for this user. Processes of other users are
/// listed too; `info` on one may fail, which callers treat as "not ours".
pub fn pids() -> Vec<u32> {
    // Ask for the count, then for the list with headroom: processes are created
    // between the two calls.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    let mut buf = vec![0 as libc::c_int; n as usize + 64];
    let got = unsafe {
        libc::proc_listallpids(
            buf.as_mut_ptr() as *mut libc::c_void,
            (buf.len() * size_of::<libc::c_int>()) as libc::c_int,
        )
    };
    if got <= 0 {
        return Vec::new();
    }
    buf.truncate(got as usize);
    buf.into_iter()
        .filter(|p| *p > 0)
        .map(|p| p as u32)
        .collect()
}

/// A process's parent, group, owner, state and name, or `None` if it is gone (or
/// not ours to ask about).
pub fn info(pid: u32) -> Option<Info> {
    let mut bi: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut bi as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if got != size {
        return None;
    }
    let comm = unsafe { std::ffi::CStr::from_ptr(bi.pbi_comm.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    Some(Info {
        pid: bi.pbi_pid,
        ppid: bi.pbi_ppid,
        pgid: bi.pbi_pgid,
        uid: bi.pbi_uid,
        status: bi.pbi_status,
        comm,
        start_us: bi.pbi_start_tvsec * 1_000_000 + bi.pbi_start_tvusec,
    })
}

/// CPU time (user + system, nanoseconds) and resident size (bytes), from
/// `PROC_PIDTASKINFO`. `None` for a zombie, which has no task left.
#[allow(deprecated)] // `mach_timebase_info` points at the `mach2` crate; one call is not worth a dependency.
pub fn task(pid: u32) -> Option<(u64, u64)> {
    let mut ti: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_taskinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            &mut ti as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if got != size {
        return None;
    }
    // The CPU times are in mach absolute-time units, which are nanoseconds on
    // Intel and not on Apple Silicon; the timebase converts either.
    let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
    let ok = unsafe { libc::mach_timebase_info(&mut tb) } == 0 && tb.denom != 0;
    let units = ti.pti_total_user + ti.pti_total_system;
    let ns = if ok {
        (units as u128 * tb.numer as u128 / tb.denom as u128) as u64
    } else {
        units
    };
    Some((ns, ti.pti_resident_size))
}

/// The live (non-zombie) members of the given process groups.
pub fn members_of_groups(pgids: &[u32]) -> Vec<u32> {
    if pgids.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<u32> = pids()
        .into_iter()
        .filter_map(info)
        .filter(|i| pgids.contains(&i.pgid) && !i.zombie())
        .map(|i| i.pid)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether a pid names a live, non-zombie process. `kill(pid, 0)` alone says yes
/// to a zombie, which is the mistake `/proc/<pid>` existing made on Linux.
pub fn alive(pid: u32) -> bool {
    info(pid).is_some_and(|i| !i.zombie())
}

/// The command line, space-joined — `/proc/<pid>/cmdline`'s counterpart.
///
/// `KERN_PROCARGS2` returns `argc` (an `int`), the executable path, NUL padding,
/// then `argc` NUL-terminated arguments, then the environment. Only the arguments
/// are taken. Empty when the process is gone or not ours.
pub fn cmdline(pid: u32) -> String {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size < size_of::<libc::c_int>()
    {
        return String::new();
    }
    let mut buf = vec![0u8; size];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return String::new();
    }
    buf.truncate(size);
    parse_procargs2(&buf)
}

fn parse_procargs2(buf: &[u8]) -> String {
    let n = size_of::<libc::c_int>();
    if buf.len() < n {
        return String::new();
    }
    let argc = libc::c_int::from_ne_bytes(buf[..n].try_into().unwrap()).max(0) as usize;
    let rest = &buf[n..];
    // Skip the executable path, then the NUL padding after it.
    let Some(end) = rest.iter().position(|b| *b == 0) else {
        return String::new();
    };
    let rest = &rest[end..];
    let Some(start) = rest.iter().position(|b| *b != 0) else {
        return String::new();
    };
    rest[start..]
        .split(|b| *b == 0)
        .take(argc)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_listed_and_described() {
        let me = std::process::id();
        assert!(pids().contains(&me));
        let i = info(me).expect("our own pid");
        assert_eq!(i.pid, me);
        assert_eq!(i.uid, unsafe { libc::getuid() });
        assert!(!i.zombie());
        assert!(!cmdline(me).is_empty());
    }

    #[test]
    fn procargs2_takes_the_arguments_and_not_the_environment() {
        let mut b = 2i32.to_ne_bytes().to_vec();
        b.extend_from_slice(b"/bin/sleep\0\0\0\0sleep\x0030\0HOME=/x\0");
        assert_eq!(parse_procargs2(&b), "sleep 30");
    }

    #[test]
    fn a_group_is_found_by_its_members() {
        let mut c = {
            use std::os::unix::process::CommandExt;
            let mut c = std::process::Command::new("/bin/sleep");
            c.arg("30").process_group(0);
            c.spawn().unwrap()
        };
        let pg = c.id();
        assert_eq!(members_of_groups(&[pg]), vec![pg]);
        let _ = c.kill();
        let _ = c.wait();
        assert!(members_of_groups(&[pg]).is_empty());
    }
}
