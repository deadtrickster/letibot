//! `0x1c` is `QUIT` under `ISIG`, so a head whose terminal is not fully raw never receives the
//! byte - the kernel signals the group instead. This measures `Terminal::enter` clearing
//! `ISIG`, in a file of its own because it takes fd 0 and so needs a process of its own.

use std::io::Write;
use std::os::fd::AsRawFd;

/// The way-out byte is delivered and observable: had `enter` left `ISIG` on, the kernel would
/// turn this into `SIGQUIT` for the process group and there would be no byte to assert on.
#[test]
fn the_way_out_byte_reaches_the_reader_and_is_not_a_key() {
    let mut master = 0i32;
    let mut slave = 0i32;
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "a pty pair, so the byte has a terminal to come from");
    let saved0 = unsafe { libc::dup(0) };
    assert_eq!(unsafe { libc::dup2(slave, 0) }, 0, "stdin becomes the pty");

    let term = rano::term::Terminal::enter().expect("a terminal to enter");

    // The operator presses ctrl-\.
    let wrote = unsafe { libc::write(master, b"\x1c".as_ptr().cast(), 1) };
    assert_eq!(wrote, 1, "the keyboard's byte reached the pty");
    std::io::stdout().flush().ok();

    let keys: Vec<_> = term
        .events()
        .into_iter()
        .filter_map(letibot_tui::app::key_of)
        .collect();
    let raw = term.raw_input();
    assert!(
        raw.contains(&0x1c),
        "the way-out byte must be in the stream the pane's interception reads: raw={raw:?}"
    );
    assert!(
        keys.is_empty(),
        "and it must not be a `Key`: a byte this head understood is a byte a program could \
         be given, and then a program could trap it: {keys:?}"
    );

    // The carry is not a second way out: the next read is the next keystroke.
    let wrote = unsafe { libc::write(master, b"x".as_ptr().cast(), 1) };
    assert_eq!(wrote, 1);
    let keys: Vec<_> = term
        .events()
        .into_iter()
        .filter_map(letibot_tui::app::key_of)
        .collect();
    let raw = term.raw_input();
    assert_eq!(raw, b"x", "the stream is per read, not a stale carry");
    assert_eq!(keys, vec![letibot_tui::app::Key::Char('x')]);

    drop(term);
    unsafe {
        libc::dup2(saved0, 0);
        libc::close(saved0);
        libc::close(master);
        libc::close(slave);
    }
    let _ = slave.as_raw_fd();
}
