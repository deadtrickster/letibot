//! A manager call that cannot hang its caller. A user manager pegged at 100% CPU answers nothing,
//! and a launcher blocked on `systemctl` looks hung to the operator.

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The bound on a manager query the launcher waits on.
pub const QUERY: Duration = Duration::from_secs(2);
/// The bound on a control verb's step: `daemon-reload` on a busy manager is slow, not hung.
pub const STEP: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum Error {
    Spawn(std::io::Error),
    /// The command was killed at the bound.
    TimedOut(Duration),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Spawn(e) => write!(f, "{e}"),
            Error::TimedOut(d) => write!(f, "did not answer within {}s", d.as_secs()),
        }
    }
}

/// `cmd.output()`, killed at `limit`. The pipes are drained on their own threads so a chatty
/// child cannot block on a full pipe while we wait.
pub fn output(mut cmd: Command, limit: Duration) -> Result<Output, Error> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::Spawn)?;
    let drain = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let until = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(Output {
                    status,
                    stdout: out.join().unwrap_or_default(),
                    stderr: err.join().unwrap_or_default(),
                });
            }
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                // The drain threads end when the pipes close; a grandchild holding one is not
                // waited for.
                return Err(Error::TimedOut(limit));
            }
        }
    }
}

/// The argv's stdout when it exits 0 within `limit`.
pub fn text(argv: &[&str], limit: Duration) -> Result<Option<String>, Error> {
    let mut cmd = Command::new(argv[0]);
    cmd.args(&argv[1..]);
    let o = output(cmd, limit)?;
    Ok(o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hung_command_is_killed_at_the_bound() {
        let t = Instant::now();
        let r = text(&["sleep", "30"], Duration::from_millis(300));
        assert!(matches!(r, Err(Error::TimedOut(_))), "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn an_answer_inside_the_bound_is_read() {
        let r = text(&["echo", "7"], QUERY).unwrap();
        assert_eq!(r.as_deref(), Some("7\n"));
        assert_eq!(text(&["false"], QUERY).unwrap(), None);
    }
}
