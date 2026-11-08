//! Taking the listeners the manager holds. launchd hands them out by the `Sockets` key through
//! `launch_activate_socket`; systemd passes them from fd 3 with `LISTEN_FDS`/`LISTEN_PID`, in
//! `ListenStream` order. Each fd's family is checked against the spec, so a wrong order fails.

use std::net::TcpListener;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixListener;

use crate::{Manager, Socket};

/// `$SD_LISTEN_FDS_START`.
pub const LISTEN_FDS_START: RawFd = 3;

#[derive(Debug)]
pub enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener),
}

/// The listeners for `sockets`, in that order.
pub fn take(m: Manager, sockets: &[Socket]) -> Result<Vec<Listener>, String> {
    let fds = match m {
        Manager::Launchd => sockets
            .iter()
            .map(|s| launchd::fd(s.name()))
            .collect::<Result<Vec<_>, _>>()?,
        Manager::Systemd => {
            let env = |k: &str| std::env::var(k).ok();
            let got = parse_listen(
                env("LISTEN_PID").as_deref(),
                env("LISTEN_FDS").as_deref(),
                env("LISTEN_FDNAMES").as_deref(),
                std::process::id(),
            )?;
            let names: Vec<&str> = sockets.iter().map(Socket::name).collect();
            assign(&names, &got)?
        }
    };
    sockets
        .iter()
        .zip(fds)
        .map(|(s, fd)| adopt(s, fd))
        .collect()
}

/// `LISTEN_FDS` read as fds with their names. The pid must be this process: an inherited
/// variable names fds that belong to a parent.
pub fn parse_listen(
    pid: Option<&str>,
    fds: Option<&str>,
    names: Option<&str>,
    me: u32,
) -> Result<Vec<(RawFd, Option<String>)>, String> {
    let pid = pid.ok_or("LISTEN_PID is not set: systemd passed no sockets")?;
    if pid.trim().parse::<u32>().ok() != Some(me) {
        return Err(format!(
            "LISTEN_PID is {pid}, this process is {me}: the sockets are not this process's"
        ));
    }
    let n: RawFd = fds
        .ok_or("LISTEN_FDS is not set")?
        .trim()
        .parse()
        .map_err(|_| "LISTEN_FDS is not a number".to_string())?;
    if n <= 0 {
        return Err("LISTEN_FDS is 0: systemd passed no sockets".into());
    }
    let names: Vec<&str> = names.map(|s| s.split(':').collect()).unwrap_or_default();
    let named = names.len() == n as usize;
    Ok((0..n)
        .map(|i| {
            let name = named.then(|| names[i as usize].to_string());
            (LISTEN_FDS_START + i, name)
        })
        .collect())
}

/// One fd per wanted socket: by name when every name is passed once, else by order.
pub fn assign(wanted: &[&str], got: &[(RawFd, Option<String>)]) -> Result<Vec<RawFd>, String> {
    let by_name: Option<Vec<RawFd>> = wanted
        .iter()
        .map(|w| {
            let mut hits = got.iter().filter(|(_, n)| n.as_deref() == Some(*w));
            match (hits.next(), hits.next()) {
                (Some((fd, _)), None) => Some(*fd),
                _ => None,
            }
        })
        .collect();
    if let Some(fds) = by_name {
        return Ok(fds);
    }
    if got.len() != wanted.len() {
        return Err(format!(
            "systemd passed {} sockets, this helper takes {} ({})",
            got.len(),
            wanted.len(),
            wanted.join(", ")
        ));
    }
    Ok(got.iter().map(|(fd, _)| *fd).collect())
}

fn family(fd: RawFd) -> Result<libc::c_int, String> {
    // SAFETY: `ss` is large enough for any address and `len` says so.
    unsafe {
        let mut ss: libc::sockaddr_storage = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        if libc::getsockname(fd, &mut ss as *mut _ as *mut libc::sockaddr, &mut len) != 0 {
            return Err(format!(
                "fd {fd} is not a socket: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(ss.ss_family as libc::c_int)
    }
}

fn adopt(s: &Socket, fd: RawFd) -> Result<Listener, String> {
    let fam = family(fd)?;
    let ok = match s {
        Socket::Tcp { .. } => fam == libc::AF_INET || fam == libc::AF_INET6,
        Socket::Unix { .. } => fam == libc::AF_UNIX,
    };
    if !ok {
        return Err(format!(
            "socket `{}` came as fd {fd} of the wrong family ({fam})",
            s.name()
        ));
    }
    // SAFETY: close-on-exec on an fd the manager handed to this process, which owns it alone.
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    Ok(match s {
        // SAFETY: as above.
        Socket::Tcp { .. } => Listener::Tcp(unsafe { TcpListener::from_raw_fd(fd) }),
        // SAFETY: as above.
        Socket::Unix { .. } => Listener::Unix(unsafe { UnixListener::from_raw_fd(fd) }),
    })
}

#[cfg(target_os = "macos")]
mod launchd {
    use std::ffi::CString;
    use std::os::fd::RawFd;

    unsafe extern "C" {
        // <launch.h>: the fds launchd holds under one `Sockets` key, in a malloc'd array.
        fn launch_activate_socket(
            name: *const libc::c_char,
            fds: *mut *mut libc::c_int,
            cnt: *mut libc::size_t,
        ) -> libc::c_int;
    }

    pub fn fd(name: &str) -> Result<RawFd, String> {
        let c = CString::new(name).map_err(|_| format!("socket name `{name}` holds a NUL"))?;
        let mut fds: *mut libc::c_int = std::ptr::null_mut();
        let mut cnt: libc::size_t = 0;
        // SAFETY: launchd fills `fds` with `cnt` ints the caller frees.
        let err = unsafe { launch_activate_socket(c.as_ptr(), &mut fds, &mut cnt) };
        if err != 0 {
            return Err(format!(
                "launchd has no socket `{name}` for this process: {}",
                std::io::Error::from_raw_os_error(err)
            ));
        }
        // SAFETY: `fds` holds `cnt` ints; it is freed once, here.
        let all: Vec<RawFd> = unsafe {
            let v = std::slice::from_raw_parts(fds, cnt).to_vec();
            libc::free(fds as *mut libc::c_void);
            v
        };
        let Some((&first, rest)) = all.split_first() else {
            return Err(format!("launchd passed no fd for `{name}`"));
        };
        for fd in rest {
            // SAFETY: an extra fd this process owns and never uses.
            unsafe { libc::close(*fd) };
        }
        Ok(first)
    }
}

#[cfg(not(target_os = "macos"))]
mod launchd {
    use std::os::fd::RawFd;

    pub fn fd(name: &str) -> Result<RawFd, String> {
        Err(format!(
            "launchd activation (socket `{name}`) exists on macOS only"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_fds_belong_to_this_pid_only() {
        assert_eq!(
            parse_listen(Some("42"), Some("2"), None, 42).unwrap(),
            vec![(3, None), (4, None)]
        );
        assert_eq!(
            parse_listen(Some("42"), Some("2"), Some("a:b"), 42).unwrap(),
            vec![(3, Some("a".into())), (4, Some("b".into()))]
        );
        assert_eq!(
            parse_listen(Some("42"), Some("2"), Some("a"), 42).unwrap(),
            vec![(3, None), (4, None)],
            "a name list of the wrong length is ignored"
        );
        assert!(
            parse_listen(Some("41"), Some("2"), None, 42)
                .unwrap_err()
                .contains("not this process")
        );
        assert!(parse_listen(None, Some("2"), None, 42).is_err());
        assert!(parse_listen(Some("42"), Some("0"), None, 42).is_err());
        assert!(parse_listen(Some("42"), Some("x"), None, 42).is_err());
    }

    #[test]
    fn fds_match_by_name_else_by_order() {
        let named = vec![(3, Some("sock".into())), (4, Some("http".into()))];
        assert_eq!(assign(&["http", "sock"], &named).unwrap(), vec![4, 3]);
        let unit = vec![
            (3, Some("letibot-browser".into())),
            (4, Some("letibot-browser".into())),
        ];
        assert_eq!(assign(&["http", "sock"], &unit).unwrap(), vec![3, 4]);
        assert!(
            assign(&["http", "sock"], &[(3, None)])
                .unwrap_err()
                .contains("passed 1 sockets")
        );
    }

    #[test]
    fn a_socket_of_the_wrong_family_is_refused() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let fd = std::os::fd::IntoRawFd::into_raw_fd(l);
        let s = Socket::Unix {
            name: "sock".into(),
            path: "/x".into(),
        };
        assert!(adopt(&s, fd).unwrap_err().contains("wrong family"));
        // SAFETY: the refused fd is still this test's.
        unsafe { libc::close(fd) };
    }
}
