//! **`ctrl-\` is a byte, not a signal — measured on a real terminal.**
//!
//! The other half of the way out, and the half no `App` test can see: whether `0x1c` is
//! *observable* at all. `0x1c` is `QUIT` under `ISIG`, so a head whose terminal is not
//! fully raw never receives the byte — the kernel sends `SIGQUIT` to the process group and
//! the head dies instead of detaching from the pane. `Terminal::enter` calls `cfmakeraw` on the
//! fd it read, which clears `ISIG` (`term.rs`), and this file is the measurement of that
//! claim rather than a reading of it: a pty pair, a real `Terminal`, the byte written into
//! the master the way a keyboard would send it, and the assertion that
//! `Terminal::raw_input` has it.
//!
//! **It is in its own file because it takes fd 0.** `Terminal::enter` inspects stdin, so
//! the test points fd 0 at a pty slave — and an integration test is its own process, so
//! that redirect is contained here and cannot reach another test's stdin.
//!
//! # What this proves and what it does not
//!
//! Proved: the byte reaches the reader, and it survives `key_of`, which maps it to nothing,
//! because the reader keeps what it *consumed* rather than what it decoded.
//! Not proved here: that a particular terminal or multiplexer sends `0x1c` for the
//! operator's `ctrl-\` at all — that is a fact about their keyboard, their terminal and
//! their multiplexer, and it is the one thing a live head still has to answer.

use std::io::Write;
use std::os::fd::AsRawFd;

/// **The way-out byte is delivered and observable.**
///
/// One pty pair, one real `Terminal`, one `0x1c`. If `enter` left `ISIG` on anywhere, the
/// kernel would turn this into `SIGQUIT` for the process group and there would be no byte
/// to assert on — which is exactly the failure the assertion is written to distinguish.
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
