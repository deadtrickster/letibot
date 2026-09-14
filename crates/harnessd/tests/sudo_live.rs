//! The shim plus `SUDO_ASKPASS`, against the real `sudo` on this box — the one
//! link that is genuinely ours and needs a live sudo to prove: that the shim's
//! `-A` makes sudo run the askpass program instead of reaching for a tty, that
//! the program's stdout is fed to sudo as the password, and that a wrong one
//! comes back as sudo's own refusal (not a tty error, and not a hang).
//!
//! No model, no daemon: `letibot-askpass` talks to a daemon, and that path is
//! covered by `letibot-sessionlog`'s `askpass` socket test. Here the askpass
//! program is a one-line script that prints a deliberately wrong password, so
//! the assertion is sudo's *refusal*, which is nobody's secret.
//!
//! `SUDO_LIVE=1 cargo test -p letibot-harnessd --test sudo_live`.

use std::os::unix::fs::PermissionsExt;

use letibot_harnessd::sudo;

#[test]
fn the_shim_routes_sudo_to_askpass_and_a_wrong_password_is_refused_not_a_tty_error() {
    if std::env::var("SUDO_LIVE").as_deref() != Ok("1") {
        eprintln!("SUDO_LIVE=1 to run this against the box's real sudo");
        return;
    }
    // `install()` finds the helper beside the daemon binary; here we only need the
    // shim it writes, so point $LETIBOT_ASKPASS at a script of our own.
    let dir = std::env::temp_dir().join(format!("letibot-sudo-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake_askpass = dir.join("askpass.sh");
    std::fs::write(
        &fake_askpass,
        "#!/bin/sh\nprintf 'definitely-not-the-password\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake_askpass, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: single-threaded at this point.
    unsafe { std::env::set_var("LETIBOT_ASKPASS", &fake_askpass) };

    let plumbing = sudo::install().expect("the shim installs");
    let shim = plumbing.shims.join("sudo");
    assert!(shim.is_file(), "the shim was not written");

    // The shim, with SUDO_ASKPASS set the way the daemon sets it, running a
    // harmless command. Wrong password → sudo prints its own refusal and exits
    // non-zero. The one thing that must NOT happen is "a terminal is required".
    let out = std::process::Command::new(&shim)
        .arg("true")
        .env("SUDO_ASKPASS", &fake_askpass)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the shim runs");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("a terminal is required") && !err.contains("askpass helper"),
        "sudo did not use the askpass program:\n{err}"
    );
    assert!(
        !out.status.success(),
        "a wrong password must not authenticate (stderr: {err})"
    );
    assert!(
        err.contains("incorrect password") || err.contains("try again") || err.contains("Sorry"),
        "expected sudo's own wrong-password refusal, got:\n{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
