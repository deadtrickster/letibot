//! A session's NAME over the real daemon: one live main session of one folder to a name, refused
//! where a person typed it, and carried into the store row so `-r NAME` finds it tomorrow.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::wire::FrameReader;
use letibot_tokencore::migrate_split::project_key;
use letibot_tokencore::store::Store;

struct Daemon(std::process::Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One connection to the daemon, seated in the session it opened itself: a head of this daemon
/// creates and names sessions from there, which is what a person's `/new` does.
struct Control {
    head: HeadClient,
    reader: FrameReader<UnixStream>,
}

impl Control {
    fn open(socket: &Path) -> Control {
        let caps = Caps {
            can_decide: true,
            ..Caps::default()
        };
        let (head, hello, reader) =
            HeadClient::attach(socket, "s-only", 0, "tui", "dead", caps).expect("the attach");
        assert!(
            matches!(&hello, ServerFrame::Hello { session_id, .. } if session_id == "s-only"),
            "the daemon opens its named session: {hello:?}"
        );
        Control { head, reader }
    }

    /// Ask for a session named `title` in `workspace`, and answer what the daemon said: the id it
    /// made, or the sentence it refused with.
    fn ask(&mut self, title: &str, workspace: &str) -> Result<String, String> {
        self.head
            .new_session(title, workspace)
            .expect("the new-session frame goes");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(
                Instant::now() < deadline,
                "the daemon never answered the new-session frame"
            );
            match self.reader.read::<ServerFrame>() {
                Ok(ServerFrame::Sessions {
                    created: Some(id), ..
                }) => return Ok(id),
                Ok(ServerFrame::Rejected { reason, .. }) => return Err(reason),
                Ok(_) => {}
                Err(e) => panic!("the new-session frame went unanswered: {e}"),
            }
        }
    }
}

fn scratch() -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = letibot_sessionlog::testing::scratch_root().join(format!(
        "lb-names-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("ws")).unwrap();
    std::fs::create_dir_all(d.join(".config/letibot")).unwrap();
    std::fs::create_dir_all(d.join("run")).unwrap();
    d
}

fn start(home: &Path) -> Daemon {
    let socket = home.join("d.sock");
    let _ = std::fs::remove_file(&socket);
    let child = letibot_testenv::daemon(env!("CARGO_BIN_EXE_harnessd"))
        .args([
            "--provider",
            "deepseek",
            "--model",
            "deepseek-flash",
            "--role",
            "leticode",
            // No queue: the daemon's own gatekeeper seat would be a second session in this folder,
            // and this test is about the names of the sessions it was ASKED for.
            "--no-queue",
        ])
        .args(["--session", "s-only", "--workspace"])
        .arg(home.join("ws"))
        .arg("--store")
        .arg(home.join("store"))
        .arg("--socket")
        .arg(&socket)
        .env("HOME", home)
        .env("LETIBOT_LANDING_ROOT", home.join("landing"))
        .env("LETIBOT_NOTES_REPO", home.join("notes"))
        .env("LETIBOT_LOGS_ROOT", home.join("logs"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env_remove("DEEPSEEK_API_KEY")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(home.join("daemon.log")).expect("the daemon's log"))
        .spawn()
        .expect("harnessd starts");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !socket.exists() {
        assert!(
            Instant::now() < deadline,
            "the daemon never bound its socket"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Daemon(child)
}

/// The title the daemon's store holds for `id`, waiting for it: the row is written at the OPEN,
/// which the daemon does on its own worker after the head has been answered, so a read on the
/// instant of the attach is a read of a store that has not been written yet.
fn stored_title(home: &Path, id: &str) -> Option<String> {
    let key = project_key(&home.join("ws").display().to_string());
    let store = Store::open_set(&home.join("store"), &key).expect("the store");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let title = store
            .session(id)
            .expect("the row's read")
            .and_then(|s| s.title);
        if title.is_some() || Instant::now() > deadline {
            return title;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The same title twice in one folder: the first session keeps it, the second is refused BY THE
/// FRAME - before a row exists - and the refusal names the session that has it.
#[test]
fn a_new_session_named_twice_is_refused_and_names_the_holder() {
    let home = scratch();
    let _daemon = start(&home);
    let ws = home.join("ws").display().to_string();
    let mut control = Control::open(&home.join("d.sock"));
    let first = control.ask("the work", &ws).expect("the first is named");
    let second = control
        .ask("The Work", &ws)
        .expect_err("the second is refused");
    assert!(
        second.contains(&first) && second.contains("taken in this folder"),
        "the refusal names the holder: {second}"
    );

    // And the name reached the STORE - a session created with a title is listed under it, not
    // `(unnamed)` until someone renames it. The row is written when the session OPENS, so a head
    // attaches to it first, which is exactly what opening it means.
    {
        let (mut head, hello, _reader) = HeadClient::attach(
            home.join("d.sock"),
            &first,
            0,
            "tui",
            "dead",
            Caps {
                can_decide: true,
                ..Caps::default()
            },
        )
        .expect("attach to the new one");
        assert!(
            matches!(&hello, ServerFrame::Hello { session_id, .. } if *session_id == first),
            "the daemon opened the session it was asked for: {hello:?}"
        );
        let _ = head.detach();
    }
    let listed = stored_title(&home, &first);
    assert_eq!(
        listed.as_deref(),
        Some("the work"),
        "the head's title is on the row, not `(unnamed)` until someone renames it"
    );
}

/// The folder is the canonical path: a trailing slash and the path it names are one folder, and a
/// symlink to it is that folder too.
#[test]
fn a_workspace_spelled_another_way_is_the_same_folder() {
    let home = scratch();
    let _daemon = start(&home);
    let ws = home.join("ws");
    std::os::unix::fs::symlink(&ws, home.join("through-a-link")).unwrap();
    let mut control = Control::open(&home.join("d.sock"));
    let first = control
        .ask("one name", &ws.display().to_string())
        .expect("the first is named");
    let trailing = control
        .ask("one name", &format!("{}/", ws.display()))
        .expect_err("a trailing slash is the same folder");
    assert!(trailing.contains(&first), "{trailing}");
    let through_a_link = control
        .ask(
            "one name",
            &home.join("through-a-link").display().to_string(),
        )
        .expect_err("a symlink is the same folder");
    assert!(through_a_link.contains(&first), "{through_a_link}");
}

/// A name that reads as an address is refused at the frame, so `--session NAME` and `--session ID`
/// can never be two different answers to one word.
#[test]
fn a_name_that_reads_as_an_id_is_refused() {
    let home = scratch();
    let _daemon = start(&home);
    let ws = home.join("ws").display().to_string();
    let mut control = Control::open(&home.join("d.sock"));
    let why = control
        .ask("s-20260101", &ws)
        .expect_err("an address is not a name");
    assert!(why.contains("not a session name"), "{why}");
}
