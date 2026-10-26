//! **A session's whole daemon, on another machine — and back** (`docs/remote-hosts.md` RH4).
//!
//! The operator: *"move load from laptops — I start doing something, it starts eating resources
//! and just stays on the hot laptop"*, then *"I want daemon migration to host thing now"*, and
//! widening it: *"back and forth lol or just midween hosts"*. So this is not *up* and *down*: it
//! is **one move with a source and a destination**, and `local` is an endpoint like any other.
//!
//! ```text
//!   letibot --migrate SESSION --to lab2x1     laptop  -> lab2x1
//!   letibot --migrate SESSION --to local      lab2x1  -> laptop
//!   letibot --migrate SESSION --to lubuntu1   lab2x1  -> lubuntu1   (relayed through here)
//! ```
//!
//! # What moves, and why it is four things
//!
//! | what | how |
//! |---|---|
//! | **the session** | the store's rows, out of `letibot_tokencore::store::transfer` into a file, carried, and read in — with the same hash-chain check a resume makes |
//! | **the tree** | `git bundle` of the source checkout's `HEAD`, cloned on the far side onto a branch `remote/<short id>`; nothing uncommitted is carried and the list of what was left behind is printed |
//! | **the daemon** | `harnessd --session ID` on the destination, built there from a bundle of this tree at this commit (`cargo build --release --offline`) |
//! | **the socket** | `ssh -N -L <local>:<remote>`, recorded beside the launcher's own daemon records so a head finds it where it finds any daemon |
//!
//! # Exactly one copy is live, and the marker says which
//!
//! A move does not delete the source: it marks it — `session.lives_on` — with the name of the
//! endpoint holding the live copy, so `--sessions` says *lives on lab2x1 since …* rather than
//! offering a conversation nobody is appending to. **The local store is the index of record**:
//! every move rewrites the row here too, whatever hop the session took, because this is where
//! the operator's `--sessions` and `--continue` are typed. That is what makes `lab2x1 ->
//! lubuntu1` possible at all — the row here names lab2x1, so that is where the live copy is read
//! from.
//!
//! # A still copy, and the two ways of making one
//!
//! Step one is quiesce: attach to the session's daemon and wait until
//! [`Activity`](letibot_sessionlog::activity::Activity) is quiet — nothing in flight, nothing
//! owed, no monitor about to fire. A session stopped on a **card** is refused rather than waited
//! out: the only thing that would clear it is a person, and a verb that silently waited for one
//! would look like a hang.
//!
//! Then the source is shut, and which verb that is depends on a real difference rather than on
//! taste: a **host's** daemon was started by a move, serves exactly this session, and is stopped
//! (`Stop`); a **local** daemon serves the operator's other conversations, so only this session
//! is closed (`CloseSession`, protocol 53). After that nothing can append and the export is the
//! last word.
//!
//! # Host to host is relayed through here
//!
//! `A -> B` reads A's rows over ssh into a file on this box and writes that file to B. A direct
//! A→B pipe would be one connection fewer and one more thing that can fail where neither store's
//! refusals can describe it; the relay costs a temp file and buys the same verification at both
//! ends. The tree is relayed the same way, for the same reason.
//!
//! # What this module deliberately is NOT
//!
//! RH1 (`docs/remote-hosts.md`) runs a session's TOOLS on a host with the daemon staying here;
//! this moves the daemon. They meet at two seams — `hosts.toml` and the git layout
//! `root/<repo>/<short id>` — and both are implemented here *small and replaceable*, against the
//! doc's spelling, so whichever lands second can delete the copy instead of negotiating with it.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use letibot_sessionlog::activity::Activity;
use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, NOTE_SESSION_CLOSED, ServerFrame};
use letibot_tokencore::store::{ImportInto, SessionTransfer, Store};
// **One `hosts.toml` reader and one ssh argv, shared with RH1.** This module used to spell both
// itself — `parse_hosts`, `hosts_path`, `ssh_argv` — as a deliberately small copy that "whichever
// landed second can delete". It landed: the copy is gone, and the only thing a migration reads
// about a host is what `crates/tools` reads about it.
use letibot_tools::remote::{Host, Hosts, Paths, Ssh};

use crate::launcher;

/// The name `local` has in this vocabulary — it is an endpoint like any other, and the marker
/// column spells it the way a host is spelled.
pub const LOCAL: &str = "local";

/// **How long a move waits for a session to go quiet before refusing.** A minute: a tool call or
/// a model round can be long, and a migration that gave up in ten seconds would refuse the
/// ordinary case of *"I asked it something and now I want it on the host"*. Past it the operator
/// is told what the session is still waiting on, rather than being left with a verb that never
/// returns.
pub const QUIESCE_PATIENCE: Duration = Duration::from_secs(60);

// -------------------------------------------------------------------------------------------
// hosts.toml
// -------------------------------------------------------------------------------------------
//
// The reader is `letibot_tools::remote::Hosts`'s — one file read one way on this box, whether it
// is a session's tools or its daemon that goes to the host.

// -------------------------------------------------------------------------------------------
// An endpoint: a store, a checkout and a daemon, reached directly or over ssh
// -------------------------------------------------------------------------------------------

/// **One place a session can live.** `local` is this box; anything else is a host from
/// `hosts.toml`, reached through the ssh connection in [`Endpoint::ssh`].
///
/// A move has exactly two, and every step below is written ONCE against this type — which is what
/// makes *host → host* the same code path as *laptop → host*, rather than a second one that would
/// have to be kept in step with the first.
///
/// **The connection is `letibot_tools::remote::Ssh` and not this module's own argv.** RH4 spelled
/// its own flags here, and the two spellings had already drifted from RH1's: neither passed
/// `-F`, so both read the operator's ssh client config, and neither checked the host key it was
/// about to trust. There is one ssh policy in this tree now, and this is the second of its two
/// callers.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// `local`, or the host's table name — the spelling the marker column stores.
    pub name: String,
    /// `None` for this box.
    pub ssh: Option<Ssh>,
    /// The store's path, as spelled on that box: absolute here, and on a host a path inside
    /// `root` that this process never opens directly.
    pub store: String,
    /// `root`, resolved to an absolute path on that box. Empty for local, whose checkouts are
    /// wherever the operator made them.
    pub root: String,
}

impl Endpoint {
    pub fn is_local(&self) -> bool {
        self.ssh.is_none()
    }

    /// The ssh destination, `user@hostname`.
    fn target(&self) -> String {
        self.ssh
            .as_ref()
            .expect("only a host is ssh'd to")
            .destination()
    }

    /// **The daemon's unix socket on that box.** For a host it is inside the migration root and
    /// named by the session, because a moved session's daemon serves exactly that session.
    pub fn daemon_socket(&self, session: &str) -> String {
        assert!(
            !self.is_local(),
            "a local daemon's socket is its checkout's"
        );
        format!("{}/.letibot/{session}.sock", self.root)
    }

    /// **The socket the session's daemon listens on**, for either kind of endpoint. A local
    /// daemon is the folder's — `<rundir>/<key(workspace)>.sock`, the launcher's own rule — so
    /// the workspace is an argument here and not a lookup.
    pub fn daemon_socket_for(&self, session: &str, workspace: &str) -> String {
        if self.is_local() {
            return launcher::locate(Path::new(workspace))
                .socket
                .display()
                .to_string();
        }
        self.daemon_socket(session)
    }

    /// **The binary directory a move builds on this endpoint.**
    pub fn bin_dir(&self) -> String {
        format!("{}/.letibot/bin", self.root)
    }

    /// A command that runs `script` on this endpoint, with stdin/stdout/stderr as the caller sets
    /// them. **The script is one argv element**, which is exactly what ssh hands to the far
    /// side's shell — so nothing in it is re-quoted on the way, and everything it names goes
    /// through [`sh_quote`] here.
    ///
    /// The host's `path` list goes in front of the script, because the move runs `cargo`, `git`
    /// and the daemon's own binary on the far side through a shell that is not a login shell.
    fn command(&self, script: &str) -> Command {
        if self.is_local() {
            let mut c = Command::new("sh");
            c.arg("-c").arg(script);
            return c;
        }
        let ssh = self.ssh.as_ref().expect("only a host is ssh'd to");
        let script = ssh.host().with_path(script);
        let mut c = Command::new(ssh.program());
        c.args(ssh.argv(&script));
        c
    }

    /// Run a shell script and capture it.
    fn run(&self, script: &str) -> Result<std::process::Output, String> {
        self.command(script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("running a command on {}: {e}", self.name))
    }

    /// [`Endpoint::run`] requiring success, with stdout as text.
    fn run_ok(&self, script: &str) -> Result<String, String> {
        let out = self.run(script)?;
        if !out.status.success() {
            return Err(format!(
                "`{}` on {} failed: {}",
                first_line(script),
                self.name,
                tail(&String::from_utf8_lossy(&out.stderr))
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// **Write a local file's bytes to `path` on this endpoint**, streamed — a bundle and a
    /// transfer file run to hundreds of megabytes and neither belongs in memory.
    ///
    /// The bytes go over as base64 on stdin and are decoded on the far side: no scp, no second
    /// protocol, and nothing shaped like a path inside the data.
    fn write_from(&self, local: &Path, path: &str) -> Result<(), String> {
        if self.is_local() {
            return std::fs::copy(local, path)
                .map(|_| ())
                .map_err(|e| format!("writing {path}: {e}"));
        }
        let quoted = sh_quote(path)?;
        let input =
            std::fs::File::open(local).map_err(|e| format!("opening {}: {e}", local.display()))?;
        let mut child = self
            .command(&format!("base64 -d > {quoted}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("writing to {}: {e}", self.name))?;
        let stdin = child.stdin.take().expect("piped");
        let mut encoder = Command::new("base64")
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(stdin))
            .spawn()
            .map_err(|e| format!("base64: {e}"))?;
        let _ = encoder.wait();
        let out = child
            .wait_with_output()
            .map_err(|e| format!("finishing the write to {}: {e}", self.name))?;
        if !out.status.success() {
            return Err(format!(
                "writing {path} on {}: {}",
                self.name,
                tail(&String::from_utf8_lossy(&out.stderr))
            ));
        }
        Ok(())
    }

    /// Read `path` on this endpoint into a local file, streamed.
    fn read_to(&self, path: &str, local: &Path) -> Result<(), String> {
        if self.is_local() {
            return std::fs::copy(path, local)
                .map(|_| ())
                .map_err(|e| format!("reading {path}: {e}"));
        }
        let quoted = sh_quote(path)?;
        // **Into a `.part` file, then renamed over `local`.** A caller can hand the same path for
        // both ends — a bundle made on the source and pulled back through a temp file named the
        // same — and creating `local` first would truncate the very file being read.
        let part = local.with_extension("part");
        let out_file = std::fs::File::create(&part)
            .map_err(|e| format!("creating {}: {e}", part.display()))?;
        let mut child = self
            // **`base64 < file`, not `base64 file`** — BSD's base64 (a Mac, and a Mac is one of
            // the two ends of a move) takes no file argument at all.
            .command(&format!("base64 < {quoted}"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("reading {path} from {}: {e}", self.name))?;
        let stdout = child.stdout.take().expect("piped");
        let mut decoder = Command::new("base64")
            .arg("-d")
            .stdin(Stdio::from(stdout))
            .stdout(Stdio::from(out_file))
            .spawn()
            .map_err(|e| format!("base64 -d: {e}"))?;
        let _ = decoder.wait();
        let out = child
            .wait_with_output()
            .map_err(|e| format!("finishing the read from {}: {e}", self.name))?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&part);
            return Err(format!(
                "reading {path} on {}: {}",
                self.name,
                tail(&String::from_utf8_lossy(&out.stderr))
            ));
        }
        std::fs::rename(&part, local).map_err(|e| format!("{}: {e}", local.display()))?;
        Ok(())
    }

    /// **Start an ssh of ours that outlives this command.** A forward has to be here an hour
    /// later, when a head attaches from another terminal, so the child is left running: it ends
    /// when its ssh does, and that is a fact about a socket somebody else may be using.
    fn spawn_detached(&self, argv_extra: &[String], say: &mut Say<'_>) -> Result<(), String> {
        let ssh = self.ssh.as_ref().expect("only a host is ssh'd to");
        // The flags, then what makes this a forward, then the destination — a forward carries no
        // remote command, which is the one shape `Ssh::argv` does not spell.
        let mut full = vec![ssh.program().display().to_string()];
        full.extend(ssh.flags());
        full.extend_from_slice(argv_extra);
        full.push(self.target());
        let child = Command::new(&full[0])
            .args(&full[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("starting `{}`: {e}", full[0]))?;
        std::mem::forget(child);
        say(&format!(
            "{}: {} {}",
            self.name,
            full[0],
            full[1..].join(" ")
        ));
        Ok(())
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

/// Single-quote a string for a shell, refusing one that cannot be quoted — a path with a `'` in
/// it is not one this verb has to support, and mangling it silently is worse than saying so.
fn sh_quote(s: &str) -> Result<String, String> {
    if s.contains('\'') {
        return Err(format!(
            "`{s}` has a single quote in it, and nothing this verb runs may: quoting it safely \
             through ssh is not worth the hiding place. Nothing was moved."
        ));
    }
    Ok(format!("'{s}'"))
}

/// The first line of a script, for a refusal — the whole of it is noise and the first line names
/// the act.
fn first_line(script: &str) -> &str {
    script
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(script)
        .trim()
}

/// The last few lines of a command's stderr: the tail is where a failure is.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines
        .iter()
        .rev()
        .take(4)
        .rev()
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}

// -------------------------------------------------------------------------------------------
// The move
// -------------------------------------------------------------------------------------------

/// Everything the move needs to know about the session, read once from the source's store.
#[derive(Debug, Clone)]
pub struct Facts {
    pub id: String,
    pub workspace: String,
    pub items: u32,
    pub model: String,
    pub provider: Option<String>,
}

/// What a move landed: the checkout on the destination, and the branch in it.
#[derive(Debug, Clone)]
pub struct Landed {
    pub workspace: String,
    pub branch: String,
}

/// One line per thing that happened, in the operator's words.
type Say<'a> = dyn FnMut(&str) + 'a;

/// Where a move is going to and from, and the knobs.
#[derive(Debug, Clone)]
pub struct Move {
    pub session: String,
    pub from: Endpoint,
    pub to: Endpoint,
    /// **This box's endpoint, even when neither end of the move is here.** It is what makes a
    /// `lab2x1 -> lubuntu1` hop keep the laptop's row truthful: that row is the index of record
    /// every surface reads, and a hop that left it naming the first host would send the next
    /// attach and the next `--sessions` to a machine the session has left.
    pub local: Endpoint,
    pub patience: Duration,
    /// Copy `~/.config/letibot/providers.toml` to the destination when it has none.
    pub copy_keys: bool,
    /// The tree this build came from: what is bundled and built on the destination.
    pub src: PathBuf,
}

impl Move {
    /// **Do it.** Every step says what it did; every refusal says what it will not do.
    pub fn run(&self, say: &mut Say<'_>) -> Result<(), String> {
        if self.from.name == self.to.name {
            return Err(format!(
                "{} is already on `{}` — a move needs two endpoints. Nothing was moved.",
                self.session, self.to.name
            ));
        }
        // **The source's own build first, when the source is a host.** Everything below reads
        // that box's store through its own binary (this process cannot open a file over ssh), and
        // a host that was moved TO has one — built by the move that put it there.
        if !self.from.is_local() {
            self.ensure_build(&self.from, say)?;
        }
        let facts = self.facts(&self.from)?;
        say(&format!(
            "{} lives on `{}` — {} rows, workspace {}, model {}{}",
            self.session,
            self.from.name,
            facts.items,
            facts.workspace,
            facts.model,
            match &facts.provider {
                Some(p) => format!(" (via {p})"),
                None => " (local)".to_string(),
            }
        ));

        // **Quiesce and shut, before anything is copied.** What the copy is worth is exactly what
        // the quiet of this moment is worth.
        self.quiesce_and_shut(&facts, say)?;

        // **The destination's build before its rows**: the import on a host is run by that host's
        // own binary, so the binary has to be there before there is anything for it to read.
        if !self.to.is_local() {
            self.check_providers(say)?;
            self.ensure_build(&self.to, say)?;
        }

        // The tree, then the rows: a session whose workspace did not arrive would open onto a
        // folder nobody has, and its rows would describe work in a directory that is not there.
        let landed = self.move_tree(&facts, say)?;
        let transfer = self.export(&self.from)?;
        say(&format!(
            "{}: {} transcript(s), {} item(s), {} todo(s), {} job(s), {} decision(s)",
            self.session,
            rows(&transfer, "transcript"),
            rows(&transfer, "transcript_item"),
            rows(&transfer, "todo"),
            rows(&transfer, "job"),
            rows(&transfer, "adjudication"),
        ));
        self.import(&transfer, &landed)?;
        say(&format!(
            "read into {} ({})",
            self.to.name,
            if self.to.is_local() {
                self.to.store.clone()
            } else {
                format!("{}:{}", self.to.name, self.to.store)
            }
        ));

        // The daemon, and the socket a head reaches it on. A LOCAL daemon is the launcher's to
        // start — it is the folder's daemon, and starting one from here would be a second answer
        // to *which daemon does this folder have*.
        if !self.to.is_local() {
            self.start_daemon(&landed, say)?;
            self.forward(&self.to, &self.session, say)?;
        }
        self.record(&landed, say)?;

        // **Exactly one copy is live, and both stores say which.** The destination's copy is live
        // (`None`); the source's says where it went.
        self.mark(&self.to, None)?;
        self.mark(&self.from, Some(&self.to.name))?;
        // **And the laptop's row, when the move did not touch this box.** It is not one of the
        // two ends of a host-to-host hop, but it is the row `--sessions` and `--continue` read
        // here — so leaving it naming the first host would be a signpost to a machine the
        // session has left.
        if !self.from.is_local() && !self.to.is_local() {
            self.mark(&self.local, Some(&self.to.name))?;
        }
        say(&format!(
            "{} now lives on `{}`. The rows on `{}` stay, marked — a move back has something to \
             come back to.",
            self.session, self.to.name, self.from.name
        ));
        say(&format!("attach with `letibot --session {}`", self.session));
        Ok(())
    }

    /// The session's row, from whichever store it lives in.
    fn facts(&self, endpoint: &Endpoint) -> Result<Facts, String> {
        if endpoint.is_local() {
            let store = Store::open(Path::new(&endpoint.store))
                .map_err(|e| format!("opening {}: {e}", endpoint.store))?;
            let row = store
                .session(&self.session)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| self.nowhere(endpoint))?;
            return Ok(Facts {
                id: row.id,
                workspace: row.workspace_root,
                items: row.items,
                model: row.model_id,
                provider: row.provider_choice,
            });
        }
        let json = self.far(
            endpoint,
            &format!("--describe-session {}", sh_quote(&self.session)?),
        )?;
        let v: serde_json::Value =
            serde_json::from_str(json.trim()).map_err(|e| format!("reading the row: {e}"))?;
        if v.is_null() {
            return Err(self.nowhere(endpoint));
        }
        let s = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string()
        };
        Ok(Facts {
            id: s("id"),
            workspace: s("workspace_root"),
            items: v.get("items").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            model: s("model_id"),
            provider: v
                .get("provider_choice")
                .and_then(|x| x.as_str())
                .map(String::from),
        })
    }

    fn nowhere(&self, endpoint: &Endpoint) -> String {
        format!(
            "no session {} in {} ({}). Nothing was moved. `letibot --sessions` here prints where \
             a session lives, if it is somewhere else.",
            self.session, endpoint.name, endpoint.store
        )
    }

    /// **Wait until it is quiet, then make it still.**
    fn quiesce_and_shut(&self, facts: &Facts, say: &mut Say<'_>) -> Result<(), String> {
        // A host's daemon is reached through the forwarded socket, so the forward has to exist
        // before it can be talked to — and it is the same forward a head uses, so establishing it
        // here is not extra machinery.
        if !self.from.is_local() {
            self.forward(&self.from, &self.session, say)?;
        }
        let socket = PathBuf::from(self.from.daemon_socket_for(&self.session, &facts.workspace));
        if !socket_reachable(&socket) {
            say(&format!(
                "no daemon is holding {} on `{}` — nothing is appending, so nothing waits",
                self.session, self.from.name
            ));
            return Ok(());
        }
        let (client, frames) = quiesce(&socket, &self.session, self.patience, say)?;
        if self.from.is_local() {
            // The folder's daemon serves the operator's other conversations: close this one and
            // leave the rest (protocol 53).
            close_session(client, frames, &self.session, say)
        } else {
            // A host's daemon was started by a move and serves exactly this session, so the
            // daemon-level stop is the narrower act here.
            stop_daemon(client, frames, &self.session, say)
        }
    }

    // --- the tree -----------------------------------------------------------------------

    /// **The checkout moves as a git branch**, and what is uncommitted stays behind.
    fn move_tree(&self, facts: &Facts, say: &mut Say<'_>) -> Result<Landed, String> {
        let short = short_id(&self.session);
        // **The repository's name, not the checkout's directory name.** A session that has already
        // hopped once works in `<root>/<repo>/<short>`, so the last component is the SESSION's id
        // and taking it as the repository would nest every later hop one directory deeper.
        let repo = {
            let mut path = Path::new(&facts.workspace);
            if path.file_name().map(|n| n.to_string_lossy()) == Some(short.as_str().into())
                && let Some(parent) = path.parent()
            {
                path = parent;
            }
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "session".to_string())
        };
        let ws = sh_quote(&facts.workspace)?;

        // A folder that is not a repository has no HEAD to send, and copying it would be a
        // different product — `docs/remote-hosts.md` refuses that in this slice.
        let inside = self.from.run_ok(&format!(
            "cd {ws} 2>/dev/null && git rev-parse --is-inside-work-tree 2>/dev/null || true"
        ))?;
        if inside.trim() != "true" {
            return Err(format!(
                "{} is not a git repository, so there is no HEAD to carry. A remote placement in \
                 this slice is a CHECKOUT, not a copy of a folder. Nothing was moved.",
                facts.workspace
            ));
        }
        let dirty = self
            .from
            .run_ok(&format!("cd {ws} && git status --porcelain"))?;
        let dirty: Vec<&str> = dirty.lines().filter(|l| !l.trim().is_empty()).collect();
        if dirty.is_empty() {
            say(&format!(
                "{}: clean, HEAD carries everything",
                facts.workspace
            ));
        } else {
            say(&format!(
                "{}: {} uncommitted change(s) are NOT carried — commit them first if they belong \
                 on `{}`",
                facts.workspace,
                dirty.len(),
                self.to.name
            ));
            for line in dirty.iter().take(20) {
                say(&format!("    {line}"));
            }
            if dirty.len() > 20 {
                say(&format!("    … and {} more", dirty.len() - 20));
            }
        }

        // **The bundle, through a temp file on this box.** A session's HEAD can be hundreds of
        // megabytes, and holding one in memory to relay it would be a way to run out of it.
        let bundle = temp_path("tree")?;
        // **Two paths, and they are deliberately different.** A bundle made on THIS box is
        // already here. One made on a host is written into that host's own root and then pulled
        // back to a temp file here — the relay the module header owns up to — and naming the two
        // the same would make the cleanup below delete the copy just fetched on any machine whose
        // "remote" is this one.
        let plant = if self.from.is_local() {
            bundle.display().to_string()
        } else {
            format!("{}/.letibot/{}.bundle", self.from.root, self.session)
        };
        let at = sh_quote(&plant)?;
        self.from.run_ok(&format!(
            "cd {ws} && rm -f {at} && git bundle create {at} HEAD >/dev/null 2>&1"
        ))?;
        if !self.from.is_local() {
            self.from.read_to(&plant, &bundle)?;
            let _ = self.from.run(&format!("rm -f {at}"));
        }
        let bytes = std::fs::metadata(&bundle).map(|m| m.len()).unwrap_or(0);
        say(&format!("HEAD bundled: {} KiB", bytes.div_ceil(1024)));

        let landed = if self.to.is_local() {
            // **Back on a laptop, the branch is the result**: `remote/<host>/<short>`, and
            // whether it is checked out is the operator's business — the parent reviews and
            // merges, as with any worktree package.
            let dest = self.local_checkout()?;
            let refname = format!("remote/{}/{}", sanitize(&self.from.name), short);
            // **The LOCAL copy of the bundle**, which is the one `git fetch` can open here — `at`
            // is the path the bundle was made at, and on a host that is the host's.
            let here = sh_quote(&bundle.display().to_string())?;
            self.to.run_ok(&format!(
                "cd {} && git fetch -q {here} '+HEAD:refs/heads/{refname}'",
                sh_quote(&dest)?,
            ))?;
            say(&format!(
                "{dest}: the branch came back as `{refname}` (not checked out — merge what you \
                 want)"
            ));
            Landed {
                workspace: dest,
                branch: refname,
            }
        } else {
            let dest = format!("{}/{repo}/{short}", self.to.root);
            let arriving = format!("{}/.letibot/{}.bundle", self.to.root, self.session);
            self.to.run_ok(&format!(
                "mkdir -p {}",
                sh_quote(&format!("{}/{repo}", self.to.root))?
            ))?;
            self.to.write_from(&bundle, &arriving)?;
            self.to.run_ok(&format!(
                "rm -rf {dest} && git clone -q {arriving} {dest} && \
                 git -C {dest} checkout -q -B remote/{short}",
                dest = sh_quote(&dest)?,
                arriving = sh_quote(&arriving)?,
            ))?;
            let _ = self.to.run(&format!("rm -f {}", sh_quote(&arriving)?));
            say(&format!("{dest}: checked out on `remote/{short}`"));
            Landed {
                workspace: dest,
                branch: format!("remote/{short}"),
            }
        };
        let _ = std::fs::remove_file(&bundle);
        Ok(landed)
    }

    /// **Which local checkout a coming-home branch goes into.** The source's row still names the
    /// folder this session was opened in, and that is where the branch belongs.
    fn local_checkout(&self) -> Result<String, String> {
        let store = Store::open(Path::new(&self.to.store))
            .map_err(|e| format!("opening {}: {e}", self.to.store))?;
        match store
            .session_workspace(&self.session)
            .map_err(|e| e.to_string())?
        {
            Some(ws) if Path::new(&ws).is_dir() => Ok(ws),
            Some(ws) => Err(format!(
                "{ws} is where this session's checkout was and it is not here any more. Nothing \
                 was moved — check the folder out again, or move the session to a host."
            )),
            None => Err(format!(
                "this store has no row for {}, so there is nowhere for a coming-home branch to be \
                 fetched into. Nothing was moved.",
                self.session
            )),
        }
    }

    // --- the rows -----------------------------------------------------------------------

    /// **The session, out of the source's store.** Locally the store is opened here; on a host
    /// that box's own binary exports it and the file comes back over the same ssh.
    fn export(&self, from: &Endpoint) -> Result<SessionTransfer, String> {
        if from.is_local() {
            let store = Store::open(Path::new(&from.store))
                .map_err(|e| format!("opening {}: {e}", from.store))?;
            return store
                .export_session(&self.session)
                .map_err(|e| e.to_string());
        }
        let json = self.far(
            from,
            &format!("--export-session {}", sh_quote(&self.session)?),
        )?;
        serde_json::from_str(&json)
            .map_err(|e| format!("reading the transfer file from `{}`: {e}", from.name))
    }

    /// **And into the destination's**, through the same two doors.
    fn import(&self, transfer: &SessionTransfer, landed: &Landed) -> Result<(), String> {
        if self.to.is_local() {
            let store = Store::open(Path::new(&self.to.store))
                .map_err(|e| format!("opening {}: {e}", self.to.store))?;
            store
                .import_session(
                    transfer,
                    &ImportInto {
                        workspace: landed.workspace.clone(),
                    },
                )
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        let file = temp_path("transfer")?;
        std::fs::write(
            &file,
            serde_json::to_vec(transfer).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("writing {}: {e}", file.display()))?;
        // **The rows arrive on stdin**, which is what `--import-session -` reads: no quoting of a
        // JSON document into a command line, and no size limit a shell argument would have.
        let script = format!(
            "{} --import-session - --store {} --workspace {}",
            sh_quote(&format!("{}/letibot", self.to.bin_dir()))?,
            sh_quote(&self.to.store)?,
            sh_quote(&landed.workspace)?,
        );
        let answer = self
            .to
            .command(&script)
            .stdin(Stdio::from(
                std::fs::File::open(&file).map_err(|e| format!("{}: {e}", file.display()))?,
            ))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("importing on {}: {e}", self.to.name))?;
        let _ = std::fs::remove_file(&file);
        if !answer.status.success() {
            return Err(format!(
                "`{}` refused the import: {}",
                self.to.name,
                tail(&String::from_utf8_lossy(&answer.stderr))
            ));
        }
        Ok(())
    }

    /// **The marker, on one store.** `None` is *live here*; `Some(name)` is *its live copy is
    /// there*. Written on both ends of every move, which is what makes the local row the index.
    fn mark(&self, endpoint: &Endpoint, on: Option<&str>) -> Result<(), String> {
        if endpoint.is_local() {
            let store = Store::open(Path::new(&endpoint.store))
                .map_err(|e| format!("opening {}: {e}", endpoint.store))?;
            return store
                .set_lives_on(&self.session, on)
                .map_err(|e| e.to_string());
        }
        let args = match on {
            Some(name) => format!(
                "--mark {} --lives-on {}",
                sh_quote(&self.session)?,
                sh_quote(name)?
            ),
            None => format!("--mark {} --home", sh_quote(&self.session)?),
        };
        self.far(endpoint, &args).map(|_| ())
    }

    /// Run an endpoint's own `letibot` sub-verb against its own store.
    fn far(&self, endpoint: &Endpoint, args: &str) -> Result<String, String> {
        endpoint.run_ok(&format!(
            "{} {} --store {}",
            sh_quote(&format!("{}/letibot", endpoint.bin_dir()))?,
            args,
            sh_quote(&endpoint.store)?
        ))
    }

    // --- the destination's machine -------------------------------------------------------

    /// **A daemon on a host with no keys opens the session and refuses every turn**, which is a
    /// migration that looks like it worked until somebody types. So this is checked before a byte
    /// moves.
    fn check_providers(&self, say: &mut Say<'_>) -> Result<(), String> {
        let there = self
            .to
            .run("test -f \"$HOME/.config/letibot/providers.toml\" && echo yes || echo no")?;
        if String::from_utf8_lossy(&there.stdout).trim() == "yes" {
            say(&format!(
                "{}: ~/.config/letibot/providers.toml is there",
                self.to.name
            ));
            return Ok(());
        }
        if !self.copy_keys {
            return Err(format!(
                "`{}` has no ~/.config/letibot/providers.toml, so a daemon there would open this \
                 session and refuse every turn for want of a key. NOTHING WAS MOVED — the tree \
                 and the rows are still on `{}`.\n\
                 Fix it by putting that file there yourself (mode 600), or by re-running with \
                 `--copy-keys`, which copies THIS box's ~/.config/letibot/providers.toml and \
                 never prints it.",
                self.to.name, self.from.name
            ));
        }
        let local = Paths::from_env().config().join("providers.toml");
        let text = std::fs::read(&local).map_err(|e| {
            format!(
                "--copy-keys was asked for and {} cannot be read: {e}. Nothing was printed and \
                 nothing was copied.",
                local.display()
            )
        })?;
        let staged = temp_path("providers")?;
        std::fs::write(&staged, &text).map_err(|e| format!("{}: {e}", staged.display()))?;
        // The path is resolved ON THE HOST — `$HOME` is that machine's, and a `~` inside a
        // single-quoted argument would be a directory named `~`.
        let there = self.to.run_ok(
            "d=\"$HOME/.config/letibot\"; mkdir -p \"$d\" && chmod 700 \"$d\" && printf %s \
             \"$d/providers.toml\"",
        )?;
        let path = there.trim().to_string();
        if !path.starts_with('/') {
            return Err(format!(
                "`{}` did not say where its home is ({path:?}), so the file has nowhere to go. \
                 Nothing was copied.",
                self.to.name
            ));
        }
        self.to.write_from(&staged, &path)?;
        let _ = std::fs::remove_file(&staged);
        self.to.run_ok(&format!("chmod 600 {}", sh_quote(&path)?))?;
        say(&format!(
            "{}: copied providers.toml to {path} (mode 600, not printed)",
            self.to.name
        ));
        Ok(())
    }

    /// **The host must run the SAME build as this one.** A migrated session's rows were written
    /// by this build's schema; a daemon from another commit would refuse them, migrate them
    /// differently, or — worse — read them and write something this build cannot read back.
    ///
    /// Bundled and built there, **offline**: if the host has no cargo registry cache the build
    /// fails and that is said in the build's own words. Nothing is installed behind the operator's
    /// back and the network is never used.
    fn ensure_build(&self, endpoint: &Endpoint, say: &mut Say<'_>) -> Result<(), String> {
        let want = self.local_commit()?;
        let stamp = format!("{}/.letibot/build-commit", endpoint.root);
        let bin = format!("{}/harnessd", endpoint.bin_dir());
        let have = endpoint
            .run(&format!(
                "cat {} 2>/dev/null; [ -x {} ] && echo present",
                sh_quote(&stamp)?,
                sh_quote(&bin)?
            ))
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if have.contains(want.as_str()) && have.contains("present") {
            say(&format!(
                "{} runs this build ({})",
                endpoint.name,
                short_commit(&want)
            ));
            return Ok(());
        }

        let bundle = temp_path("source")?;
        let made = Command::new("git")
            .arg("-C")
            .arg(&self.src)
            .arg("bundle")
            .arg("create")
            .arg(&bundle)
            .arg("HEAD")
            .output()
            .map_err(|e| format!("bundling this tree: {e}"))?;
        if !made.status.success() {
            return Err(format!(
                "bundling {} at HEAD: {}",
                self.src.display(),
                tail(&String::from_utf8_lossy(&made.stderr))
            ));
        }
        endpoint.run_ok(&format!(
            "mkdir -p {}",
            sh_quote(&format!("{}/.letibot", endpoint.root))?
        ))?;
        let arriving = format!("{}/.letibot/source.bundle", endpoint.root);
        endpoint.write_from(&bundle, &arriving)?;
        let _ = std::fs::remove_file(&bundle);
        say(&format!(
            "building this tree ({}) on `{}` — cargo build --release --offline",
            short_commit(&want),
            endpoint.name
        ));
        let build = endpoint.run(&format!(
            "set -e\n\
             mkdir -p {bin} {src}\n\
             if [ ! -d {src}/.git ]; then git init -q {src}; fi\n\
             git -C {src} fetch -q --update-head-ok {bundle} '+refs/heads/*:refs/heads/*' \
                '+HEAD:refs/rh4/head'\n\
             git -C {src} checkout -q -f refs/rh4/head\n\
             cd {src} && cargo build --release --offline -p letibot\n\
             cp target/release/letibot {bin}/letibot\n\
             ln -sf letibot {bin}/harnessd\n\
             printf '%s' {want} > {stamp}\n",
            bin = sh_quote(&endpoint.bin_dir())?,
            src = sh_quote(&format!("{}/.letibot/src", endpoint.root))?,
            bundle = sh_quote(&arriving)?,
            stamp = sh_quote(&stamp)?,
            want = sh_quote(&want)?,
        ))?;
        if !build.status.success() {
            let err = String::from_utf8_lossy(&build.stderr);
            return Err(format!(
                "the build on `{}` failed. This verb never uses the network, so a host with no \
                 cargo registry cache cannot be built for:\n  {}\nNothing was moved; the rows and \
                 the tree are still on `{}`.",
                endpoint.name,
                tail(&err),
                self.from.name
            ));
        }
        say(&format!(
            "built {} on `{}` — {}/letibot, {}/harnessd",
            short_commit(&want),
            endpoint.name,
            endpoint.bin_dir(),
            endpoint.bin_dir()
        ));
        Ok(())
    }

    /// This tree's commit — what the far side must have been built from.
    fn local_commit(&self) -> Result<String, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.src)
            .args(["rev-parse", "HEAD"])
            .output()
            .map_err(|e| format!("asking {} for its commit: {e}", self.src.display()))?;
        if !out.status.success() {
            return Err(format!(
                "{} is not a git checkout, so there is no commit to build a host from. Pass --src.",
                self.src.display()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// **The session's daemon on the host** — the same session id, so a head that knows the id
    /// finds it wherever it is.
    fn start_daemon(&self, landed: &Landed, say: &mut Say<'_>) -> Result<(), String> {
        let socket = self.to.daemon_socket(&self.session);
        let log = format!("{}/.letibot/{}.log", self.to.root, self.session);
        let script = format!(
            "set -e\n\
             mkdir -p {dir}\n\
             if [ -S {sock} ]; then echo running; exit 0; fi\n\
             cd {ws}\n\
             SETSID=$(command -v setsid 2>/dev/null || true)\n\
             $SETSID nohup {bin}/harnessd --workspace {ws} --store {store} --socket {sock} \
                --session {session} --idle-exit 0 --no-queue \
                >>{log} 2>&1 </dev/null &\n\
             echo started\n",
            dir = sh_quote(&format!("{}/.letibot", self.to.root))?,
            sock = sh_quote(&socket)?,
            ws = sh_quote(&landed.workspace)?,
            bin = sh_quote(&self.to.bin_dir())?,
            store = sh_quote(&self.to.store)?,
            session = sh_quote(&self.session)?,
            log = sh_quote(&log)?,
        );
        let out = self.to.run(&script)?;
        if !out.status.success() {
            return Err(format!(
                "starting the daemon on `{}`: {}",
                self.to.name,
                tail(&String::from_utf8_lossy(&out.stderr))
            ));
        }
        say(&format!(
            "{} daemon on `{}` for {} — socket {socket}, log {log}. `--no-queue`: the merge queue \
             belongs to the folder's daemon on the box you work from, not to a moved session. \
             `setsid` where the box has one, so a closed lid does not take it",
            if String::from_utf8_lossy(&out.stdout).contains("running") {
                "already a"
            } else {
                "started a"
            },
            self.to.name,
            self.session,
        ));
        Ok(())
    }

    /// **The socket a head reaches that daemon on**, forwarded to this box over `ssh -L`
    /// (OpenSSH's unix-socket forwarding). A forward is a process, so it is started once and left:
    /// it does not belong to this verb's lifetime — a head attaches an hour later from a different
    /// terminal. A dropped one is re-established by the next attach ([`Move::reattach`]).
    fn forward(&self, endpoint: &Endpoint, session: &str, say: &mut Say<'_>) -> Result<(), String> {
        if endpoint.is_local() {
            return Ok(());
        }
        let local = forward_socket(session);
        if socket_reachable(&local) {
            return Ok(());
        }
        // A socket file left by a forward that died: the path exists, nothing answers on it, and
        // ssh refuses to bind over it.
        let _ = std::fs::remove_file(&local);
        if let Some(dir) = local.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let remote = endpoint.daemon_socket(session);
        endpoint.spawn_detached(
            &[
                "-N".to_string(),
                "-o".to_string(),
                "ExitOnForwardFailure=yes".to_string(),
                "-L".to_string(),
                format!("{}:{}", local.display(), remote),
            ],
            say,
        )?;
        say(&format!(
            "the session's socket is at {} (forwarded from {}:{})",
            local.display(),
            endpoint.name,
            remote
        ));
        Ok(())
    }

    /// **Where a moved session is recorded** — beside the launcher's own daemon records, so a head
    /// finds it where it finds any daemon: by looking in the rundir. Named by the SESSION and not
    /// by a workspace hash, because a forward is not a folder's daemon — the folder's daemon may
    /// well still be running for the sessions that stayed.
    fn record(&self, landed: &Landed, say: &mut Say<'_>) -> Result<(), String> {
        let path = record_path(&self.session);
        // **A session that came home has no forward to record.** The record is what makes a
        // HEAD go to the forwarded socket; leaving it behind after a move back would send the
        // next attach to a socket nothing listens on, and the re-establish would ask a host
        // that no longer holds the session. Removed, not rewritten: there is nothing to say.
        if self.to.is_local() {
            match std::fs::remove_file(&path) {
                Ok(()) => say(&format!(
                    "{}: removed — the session is back, so there is no forward to find",
                    path.display()
                )),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("removing {}: {e}", path.display())),
            }
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let body = serde_json::json!({
            "workspace": landed.workspace,
            "socket": forward_socket(&self.session).display().to_string(),
            "host": if self.to.is_local() { LOCAL } else { &self.to.name },
            "session": self.session,
            "branch": landed.branch,
            "role": "",
            "model": "",
            "answers": "",
            "dialect": "",
            "bash": false,
            "started": now_ms().to_string(),
        });
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&body).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
        say(&format!(
            "recorded {} — a head reads the socket from it",
            path.display()
        ));
        Ok(())
    }

    /// **Bring a dropped forward back, and the daemon with it** — what the launcher runs before it
    /// attaches to a session that lives elsewhere.
    pub fn reattach(&self, say: &mut Say<'_>) -> Result<(), String> {
        if self.to.is_local() {
            say(&format!(
                "{} lives here; there is nothing to re-establish",
                self.session
            ));
            return Ok(());
        }
        self.ensure_build(&self.to, say)?;
        let json = self.far(
            &self.to,
            &format!("--describe-session {}", sh_quote(&self.session)?),
        )?;
        let v: serde_json::Value =
            serde_json::from_str(json.trim()).map_err(|e| format!("reading the row: {e}"))?;
        let ws = v
            .get("workspace_root")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        if ws.is_empty() {
            return Err(format!(
                "{} is not in `{}`'s store any more. Nothing was re-established — run \
                 `letibot --sessions` here to see where it went.",
                self.session, self.to.name
            ));
        }
        let landed = Landed {
            workspace: ws,
            branch: format!("remote/{}", short_id(&self.session)),
        };
        self.start_daemon(&landed, say)?;
        self.forward(&self.to, &self.session, say)?;
        self.record(&landed, say)
    }
}

fn rows(transfer: &SessionTransfer, table: &str) -> usize {
    transfer
        .tables
        .iter()
        .find(|t| t.table == table)
        .map_or(0, |t| t.rows.len())
}

fn short_commit(commit: &str) -> &str {
    &commit[..12.min(commit.len())]
}

// -------------------------------------------------------------------------------------------
// The socket: quiesce, close, stop
// -------------------------------------------------------------------------------------------

/// `<rundir>/migrated-<session>.sock` — the local end of a moved session's forward.
pub fn forward_socket(session: &str) -> PathBuf {
    launcher::rundir().join(format!("migrated-{session}.sock"))
}

/// `<rundir>/migrated-<session>.json` — the record a head reads to find that socket, in the same
/// shape as the launcher's own (`workspace`, `socket`, …) so one reader serves both.
pub fn record_path(session: &str) -> PathBuf {
    launcher::rundir().join(format!("migrated-{session}.json"))
}

fn socket_reachable(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

/// **Wait until the session is quiet, or refuse by name**, leaving the connection attached so the
/// caller can shut the session on the same one.
///
/// Attaches as a head of kind `migration`, so the session's log says who was watching, and reads
/// [`Activity`] — the daemon's own answer to *will this act again by itself?* — rather than timing
/// anything. A session stopped on a card is refused at once: waiting for it would wait for a
/// person, which is the one wait a verb must not fake.
fn quiesce(
    socket: &Path,
    session: &str,
    patience: Duration,
    say: &mut Say<'_>,
) -> Result<(HeadClient, Receiver<ServerFrame>), String> {
    let (client, mut reader) =
        HeadClient::start_attach(socket, session, 0, "migration", "migrate", Caps::default())
            .map_err(|e| format!("attaching to {} to wait for it: {e}", socket.display()))?;
    let (tx, rx) = mpsc::channel::<ServerFrame>();
    std::thread::spawn(move || {
        while let Ok(frame) = reader.read() {
            let last = matches!(frame, ServerFrame::Bye { .. });
            if tx.send(frame).is_err() || last {
                return;
            }
        }
    });

    let deadline = Instant::now() + patience;
    let mut activity = Activity::Quiet;
    let mut said: Vec<String> = Vec::new();
    loop {
        // A frame that changes the answer, or 250 ms — whichever comes first. The timeout is not
        // the mechanism; it is what lets the deadline above be looked at.
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ServerFrame::Hello { snapshot, .. }) => {
                if let Some(snap) = snapshot {
                    activity = snap.activity;
                }
            }
            Ok(ServerFrame::Resync { snapshot, .. }) => activity = snapshot.activity,
            Ok(ServerFrame::Event(envelope)) => {
                if let letibot_sessionlog::SessionEvent::ActivityChanged { activity: a } =
                    envelope.event
                {
                    activity = a;
                }
            }
            Ok(ServerFrame::Bye { reason }) => {
                return Err(format!(
                    "the daemon closed the connection while this verb was waiting for {} to come \
                     to rest: {reason}. Nothing was moved.",
                    session
                ));
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!(
                    "the daemon's connection to {} ended while this verb was waiting for it to \
                     come to rest. Nothing was moved.",
                    session
                ));
            }
        }
        if let Some(card) = activity.ask() {
            return Err(format!(
                "{} is stopped on a card (`{card}`), so it is waiting for a person and this verb \
                 will not wait with it. Answer the card (or interrupt the turn) and run the \
                 migration again. Nothing was moved.",
                session
            ));
        }
        if !activity.moves_by_itself() {
            return Ok((client, rx));
        }
        let line = activity.line();
        if !line.is_empty() && !said.contains(&line) {
            say(&format!(
                "{session} is busy — {line}; waiting for it to come to rest"
            ));
            said.push(line);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} did not come to rest within {}s — it is still {}. The copy would be a \
                 conversation with a hole in it, so nothing was moved.",
                session,
                patience.as_secs(),
                if activity.line().is_empty() {
                    "running".to_string()
                } else {
                    activity.line()
                }
            ));
        }
    }
}

/// **Close this session and wait for the daemon's answer** — the proof that it read the frame, not
/// merely that one was written.
fn close_session(
    mut client: HeadClient,
    frames: Receiver<ServerFrame>,
    session: &str,
    say: &mut Say<'_>,
) -> Result<(), String> {
    client
        .close_session(0, "migrate")
        .map_err(|e| format!("asking the daemon to close {session}: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "the daemon did not answer the close of {session} within 10s, so nothing is known \
                 about whether it is still open. Nothing was moved."
            ));
        }
        match frames.recv_timeout(left) {
            Ok(ServerFrame::Accepted { note, .. }) if note == NOTE_SESSION_CLOSED => {
                say(&format!(
                    "{session} is closed on its daemon — nothing can append to it while it moves"
                ));
                return Ok(());
            }
            Ok(ServerFrame::Rejected { reason, .. }) => {
                return Err(format!("the daemon refused to close {session}: {reason}"));
            }
            Ok(_) => continue,
            Err(_) => {
                // The session's hub closes as part of the close, so the connection ending IS the
                // answer for a session this daemon held — the frame's own doc says so.
                say(&format!(
                    "{session} is closed on its daemon — nothing can append to it while it moves"
                ));
                return Ok(());
            }
        }
    }
}

/// **Stop the daemon a move started.** It serves exactly this session, so the daemon-level stop is
/// the narrower act here — the opposite of the local case, and the module header says why.
fn stop_daemon(
    mut client: HeadClient,
    frames: Receiver<ServerFrame>,
    session: &str,
    say: &mut Say<'_>,
) -> Result<(), String> {
    client
        .stop(0, "migrate")
        .map_err(|e| format!("stopping the daemon holding {session}: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "the daemon holding {session} did not answer the stop within 10s. Its rows are \
                 exported and no marker has been written; stop it by hand before moving the \
                 session again."
            ));
        }
        match frames.recv_timeout(left) {
            Ok(ServerFrame::Accepted { .. }) => break,
            // The socket going is the stop, and it is the ordinary ending.
            Err(_) => break,
            Ok(_) => continue,
        }
    }
    say(&format!(
        "the daemon on the host is stopped — it serves only {session}, so nothing else goes with \
         it"
    ));
    Ok(())
}

/// The last eight characters of a session id — the branch and worktree suffix
/// (`docs/remote-hosts.md`'s `session-short-id`).
pub fn short_id(session: &str) -> String {
    let mut chars: Vec<char> = session.chars().collect();
    let from = chars.len().saturating_sub(8);
    let tail: String = chars.drain(from..).collect();
    if tail.is_empty() {
        "session".to_string()
    } else {
        tail
    }
}

fn temp_path(what: &str) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("letibot-migrate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir.join(format!("{what}-{}", now_ns())))
}

fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn now_ms() -> u128 {
    now_ns() / 1_000_000
}

// -------------------------------------------------------------------------------------------
// The verb
// -------------------------------------------------------------------------------------------

/// **The sub-verbs the FAR SIDE of a move runs** — a host's own `letibot`, against its own
/// store. They are internal (no launcher passes them, nothing invokes them by a program name)
/// and they exist so that the half of a move that touches another machine's store is done by
/// THAT machine's binary at THAT machine's commit, rather than by a second SQL implementation
/// spoken over ssh.
pub const FAR_VERBS: [&str; 5] = [
    "--export-session",
    "--import-session",
    "--mark",
    "--describe-session",
    "--reattach",
];

/// Is this word one of [`FAR_VERBS`]? Asked by the dispatcher, which has no other way to tell a
/// migration flag from a mistyped role.
pub fn is_far_verb(word: &str) -> bool {
    FAR_VERBS.contains(&word)
}

/// `letibot --migrate SESSION --to NAME`, `letibot migrate …`, `letibot --reattach SESSION`, and
/// the sub-verbs the far side of a move runs.
pub fn run(args: &[String]) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("letibot: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_inner(args: &[String]) -> Result<(), String> {
    let mut session: Option<String> = None;
    let mut to: Option<String> = None;
    let mut reattach = false;
    let mut copy_keys = false;
    let mut hosts: Option<PathBuf> = None;
    let mut store: Option<PathBuf> = None;
    let mut src: Option<PathBuf> = None;
    let mut patience = QUIESCE_PATIENCE;
    // The far side's sub-verbs — internal, and flags so a host's `letibot` reads its command line
    // like any other program.
    let mut export: Option<String> = None;
    let mut import: Option<String> = None;
    let mut workspace: Option<String> = None;
    let mut mark: Option<String> = None;
    let mut lives_on: Option<String> = None;
    let mut here = false;
    let mut describe: Option<String> = None;

    let mut it = args.iter().cloned();
    while let Some(arg) = it.next() {
        let mut next = |flag: &str| it.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--to" => to = Some(next("--to")?),
            "--reattach" => reattach = true,
            "--copy-keys" => copy_keys = true,
            "--hosts" => hosts = Some(PathBuf::from(next("--hosts")?)),
            "--store" => store = Some(PathBuf::from(next("--store")?)),
            "--src" => src = Some(PathBuf::from(next("--src")?)),
            "--patience" => {
                patience = Duration::from_secs(
                    next("--patience")?
                        .parse()
                        .map_err(|e| format!("--patience: {e}"))?,
                )
            }
            "--export-session" => export = Some(next("--export-session")?),
            "--import-session" => import = Some(next("--import-session")?),
            "--workspace" => workspace = Some(next("--workspace")?),
            "--mark" => mark = Some(next("--mark")?),
            "--lives-on" => lives_on = Some(next("--lives-on")?),
            "--home" => here = true,
            "--describe-session" => describe = Some(next("--describe-session")?),
            "--help" | "-h" => {
                println!("{}", usage());
                return Ok(());
            }
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("{other} is not a migration flag.\n{}", usage()));
            }
            other => {
                if session.is_some() {
                    return Err(format!(
                        "two sessions on one line (`{}` and `{other}`) — a move is about one.",
                        session.unwrap_or_default()
                    ));
                }
                session = Some(other.to_string());
            }
        }
    }

    let store_path = store.unwrap_or_else(|| {
        std::env::var_os("LETIBOT_STORE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Paths::from_env()
                    .home()
                    .join(".local/share/letibot/sessions.db")
            })
    });

    // --- the far side's sub-verbs ---------------------------------------------------------
    if let Some(id) = export {
        let store = Store::open(&store_path)
            .map_err(|e| format!("opening {}: {e}", store_path.display()))?;
        let transfer = store.export_session(&id).map_err(|e| e.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&transfer).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if let Some(file) = import {
        let text = if file == "-" {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                .map_err(|e| format!("reading the transfer file on stdin: {e}"))?;
            buf
        } else {
            std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?
        };
        let transfer: SessionTransfer =
            serde_json::from_str(&text).map_err(|e| format!("{file}: {e}"))?;
        let workspace = workspace.ok_or_else(|| {
            "--import-session needs --workspace: the checkout this session works in HERE"
                .to_string()
        })?;
        let store = Store::open(&store_path)
            .map_err(|e| format!("opening {}: {e}", store_path.display()))?;
        let report = store
            .import_session(&transfer, &ImportInto { workspace })
            .map_err(|e| e.to_string())?;
        println!(
            "{}: {} transcript(s), {} item(s), {} todo(s), {} job(s), {} monitor(s), {} \
             decision(s), {} citation(s)",
            report.session_id,
            report.transcripts,
            report.items,
            report.todos,
            report.jobs,
            report.monitors,
            report.decisions,
            report.citations
        );
        return Ok(());
    }
    if let Some(id) = mark {
        let store = Store::open(&store_path)
            .map_err(|e| format!("opening {}: {e}", store_path.display()))?;
        match (here, lives_on) {
            (true, None) => store.set_lives_on(&id, None).map_err(|e| e.to_string())?,
            (false, Some(name)) => store
                .set_lives_on(&id, Some(&name))
                .map_err(|e| e.to_string())?,
            _ => return Err("--mark takes exactly one of --lives-on NAME or --home".into()),
        }
        println!("{id}: marked");
        return Ok(());
    }
    if let Some(id) = describe {
        let store = Store::open(&store_path)
            .map_err(|e| format!("opening {}: {e}", store_path.display()))?;
        let row = store.session(&id).map_err(|e| e.to_string())?;
        let body = match row {
            None => serde_json::Value::Null,
            Some(s) => serde_json::json!({
                "id": s.id,
                "title": s.title,
                "model_id": s.model_id,
                "dialect_sha": s.dialect_sha,
                "workspace_root": s.workspace_root,
                "owner": s.owner,
                "role": s.role,
                "provider_choice": s.provider_choice,
                "items": s.items,
                "lives_on": s.lives_on,
            }),
        };
        println!(
            "{}",
            serde_json::to_string(&body).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    // --- the move -------------------------------------------------------------------------
    let session = session.ok_or_else(|| format!("which session?\n{}", usage()))?;
    let local = Endpoint {
        name: LOCAL.to_string(),
        ssh: None,
        store: store_path.display().to_string(),
        root: String::new(),
    };

    // **Where the session lives NOW** — the local row's marker, because the local store is the
    // index of record. This is what makes `--to lubuntu1` work for a session already on lab2x1.
    let local_store =
        Store::open(&store_path).map_err(|e| format!("opening {}: {e}", store_path.display()))?;
    let here_row = local_store
        .session(&session)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "no session {session} in {}. Nothing was moved. (A session that lives on a host \
                 has a row here saying where.)",
                store_path.display()
            )
        })?;
    let from_name = here_row
        .lives_on
        .clone()
        .unwrap_or_else(|| LOCAL.to_string());

    let hosts_file = hosts.unwrap_or_else(letibot_tools::remote::hosts_file);
    let hosts = Hosts::load_from(&hosts_file);
    let endpoint = |name: &str| -> Result<Endpoint, String> {
        if name == LOCAL {
            return Ok(local.clone());
        }
        let host = hosts.get(name).ok_or_else(|| {
            format!(
                "`{name}` is not a host in {}. Nothing was moved. {}",
                hosts_file.display(),
                match hosts.names().len() {
                    0 => "That file has no `[host.NAME]` tables at all.".to_string(),
                    _ => format!("It has: {}.", hosts.names().join(", ")),
                }
            )
        })?;
        remote_endpoint(host)
    };

    let from = endpoint(&from_name)?;
    let to = match (reattach, to) {
        (true, _) => from.clone(),
        (false, Some(name)) => endpoint(&name)?,
        (false, None) => {
            return Err(
                "--to names where the session goes: a host from hosts.toml, or `local`.".into(),
            );
        }
    };

    let src = src
        .or_else(|| std::env::var_os("LETIBOT_SRC").map(PathBuf::from))
        .or_else(default_src)
        .ok_or_else(|| {
            "cannot tell which letibot tree to build on a host: pass --src DIR (the checkout this \
             binary was built from), or set LETIBOT_SRC."
                .to_string()
        })?;

    let mv = Move {
        session,
        from,
        to,
        local,
        patience,
        copy_keys,
        src,
    };
    let mut say = |line: &str| println!("{line}");
    if reattach {
        mv.reattach(&mut say)
    } else {
        mv.run(&mut say)
    }
}

/// A host's endpoint: its root resolved to an absolute path ON that box, once, so that every later
/// path is absolute and a `~` never has to survive a quoting layer.
fn remote_endpoint(host: &Host) -> Result<Endpoint, String> {
    // **The connection first**, because the two refusals it gives — a key that is missing or
    // inside the operator's own ssh directory, and a host key nobody has accepted for letibot —
    // have to arrive before a byte moves, not after the store has been exported.
    let ssh = Ssh::connect(host.clone(), None, Paths::from_env())?;
    let probe = Endpoint {
        name: host.name.clone(),
        ssh: Some(ssh.clone()),
        store: String::new(),
        root: host.root.clone(),
    };
    // `~` is the shell's, and this is the one place it is allowed to be: after this the root is
    // an absolute path. **Expanded to `$HOME/…` rather than left as `~`** — a tilde inside double
    // quotes is a literal character, so `cd "~/letibot-remote"` creates a directory named `~`
    // beside wherever the command happened to run, and every path after it is nonsense.
    let root = match host.root.strip_prefix("~/") {
        Some(rest) => format!("\"$HOME/{rest}\""),
        None if host.root == "~" => "\"$HOME\"".to_string(),
        None => sh_quote(&host.root)?,
    };
    let out = probe.run_ok(&format!(
        "d={root}; mkdir -p \"$d\" 2>/dev/null; cd \"$d\" && pwd -P"
    ))?;
    let root = out.trim().to_string();
    if !root.starts_with('/') {
        return Err(format!(
            "`{}` did not answer with a path for its root ({root:?}), so nothing can be placed \
             there. Nothing was moved.",
            host.name
        ));
    }
    Ok(Endpoint {
        name: host.name.clone(),
        ssh: Some(ssh),
        store: format!("{root}/.letibot/sessions.db"),
        root,
    })
}

/// The checkout this binary was built from: walk up from the executable and take the first
/// directory that looks like this tree.
fn default_src() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    for _ in 0..4 {
        if dir.join("crates/harnessd").is_dir() && dir.join("Cargo.toml").is_file() {
            return Some(dir);
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

fn usage() -> String {
    "letibot --migrate SESSION --to NAME [--copy-keys] [--hosts FILE] [--store PATH]\n\
     \x20                             [--src DIR] [--patience SECS]\n\
     \x20 --to local|NAME   where the session goes: `local`, or a `[host.NAME]` in\n\
     \x20                  ~/.config/letibot/hosts.toml\n\
     \x20 --copy-keys       copy this box's providers.toml to the host (mode 600, never printed)\n\
     letibot --reattach SESSION    re-establish a moved session's daemon and forwarded socket"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The file's grammar, and that a typo is refused rather than ignored** — through the one
    /// reader this box has, which is `crates/tools`'s since RH5: a migration and a remote session
    /// must not read one file two ways.
    #[test]
    fn a_hosts_file_is_read_and_a_key_it_does_not_have_is_refused() {
        let hosts = Hosts::parse(
            "[host.lab2x1]\nhostname = \"lab2x1\"\nuser = \"dead\"\n\
             key = \"~/.config/letibot/keys/headless\"\npath = [\"~/.cargo/bin\"]\n\
             root = \"~/letibot-remote\"\n",
        )
        .expect("a well-formed file");
        let host = hosts.get("lab2x1").expect("the host");
        assert_eq!(host.hostname, "lab2x1");
        assert_eq!(host.user.as_deref(), Some("dead"));
        assert!(
            host.key
                .as_ref()
                .is_some_and(|k| k.ends_with("keys/headless")),
            "a `~` in a key is this machine's home: {:?}",
            host.key
        );
        assert_eq!(host.path, vec!["~/.cargo/bin".to_string()]);
        assert_eq!(host.root, "~/letibot-remote");
        // `root` is the one key with a default: a host that omits it places work where one that
        // says so does.
        let defaulted = Hosts::parse("[host.a]\nhostname = \"a\"\nuser = \"u\"\n").unwrap();
        assert_eq!(defaulted.get("a").unwrap().root, "~/letibot-remote");

        let why = Hosts::parse("[host.a]\nhostname = \"a\"\nuser = \"u\"\nkeyy = \"x\"\n")
            .expect_err("a key this file does not have");
        assert!(why.contains("keyy"), "{why}");
    }

    /// An empty or absent `[host]` table is no hosts, not an error: the file is the operator's and
    /// a fresh box has none.
    #[test]
    fn a_file_with_no_hosts_is_not_an_error() {
        assert_eq!(Hosts::parse("").unwrap().names().len(), 0);
        assert_eq!(Hosts::parse("[other]\nx = 1\n").unwrap().names().len(), 0);
    }

    /// **The branch suffix is the id's tail**, and a short id is still itself.
    #[test]
    fn the_short_id_is_the_tail_of_the_session_id() {
        assert_eq!(short_id("s-1789919514688401228"), "88401228");
        assert_eq!(short_id("s-1"), "s-1");
        assert_eq!(short_id(""), "session");
    }

    /// A path with a quote in it is refused rather than mangled — the one thing this module's
    /// quoting cannot carry.
    #[test]
    fn a_path_with_a_quote_cannot_be_quoted_and_says_so() {
        assert_eq!(sh_quote("/a/b c").unwrap(), "'/a/b c'");
        let why = sh_quote("/a/it's").expect_err("a quote cannot be quoted");
        assert!(why.contains("single quote"), "{why}");
    }

    /// The tail of a failure is what a refusal shows, and it is the LAST lines, not the first.
    #[test]
    fn a_failure_tail_is_the_end_of_it() {
        assert_eq!(tail("a\nb\nc\nd\ne\nf\n"), "c · d · e · f");
        assert_eq!(tail("\n\n"), "");
    }
}
