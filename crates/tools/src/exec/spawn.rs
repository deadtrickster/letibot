//! The spawn seam for a process that is not a job: a stdio MCP server, started once by a client
//! that owns its pipes and ends it itself. The boundary is the session's, read at each launch, so
//! a grant made after the session opened is in the argv.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::confine::{ConfinePlan, Confinement};

/// A stdio server as its client would start it: the program, its arguments, the pairs it is handed
/// and where it runs. `cwd` is `None` for a server the config gives no directory.
#[derive(Debug, Clone, Copy)]
pub struct Want<'a> {
    pub command: &'a str,
    pub args: &'a [String],
    pub env: &'a BTreeMap<String, String>,
    pub cwd: Option<&'a Path>,
}

/// What is actually started. `argv` is the program and its arguments, so a boundary's helper comes
/// first; `secrets` are the pairs a boundary may not put on an argv and that ride a descriptor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Launch {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    pub secrets: Vec<(String, String)>,
}

/// How a client starts a stdio server. The default is [`Literal`], which is also what a session
/// that asked for no boundary gets: one `Spawn` per session, whose `key` is the boundary it runs
/// in, because one process started under one view is the wrong process for another.
pub trait Spawn: Send + Sync {
    /// The argv, pairs and directory to start, or why this server is not started here at all. A
    /// refusal is the answer when a boundary was asked for and is not there: fail closed.
    fn launch(&self, want: &Want<'_>) -> Result<Launch, String>;

    /// An identity for the boundary, so two sessions' pools never share a server one of them
    /// cannot reach. Equal keys mean the same view, and the same process is safe to share.
    fn key(&self) -> String;
}

/// A session with no boundary, and the client's own fallback: the command is the command.
pub struct Literal;

impl Spawn for Literal {
    fn launch(&self, want: &Want<'_>) -> Result<Launch, String> {
        Ok(Launch {
            argv: std::iter::once(want.command.to_string())
                .chain(want.args.iter().cloned())
                .collect(),
            env: pairs(want),
            cwd: want.cwd.map(Path::to_path_buf),
            secrets: Vec::new(),
        })
    }

    fn key(&self) -> String {
        "literal".into()
    }
}

/// A boundary held as a value, for a caller that has a [`Confinement`] in hand.
pub struct Confined(pub std::sync::Arc<dyn Confinement>);

impl Spawn for Confined {
    fn launch(&self, want: &Want<'_>) -> Result<Launch, String> {
        confined(&*self.0, want)
    }

    fn key(&self) -> String {
        boundary_key(&*self.0)
    }
}

/// One live session's boundary: read out of its process host at each launch, so a grant answered
/// after the session opened is in the argv. Weak, because a pool holding this must not hold the
/// session's host alive after the session is gone.
pub struct Bound {
    pub host: std::sync::Weak<super::host::HostProcesses>,
}

impl Spawn for Bound {
    fn launch(&self, want: &Want<'_>) -> Result<Launch, String> {
        let host = self
            .host
            .upgrade()
            .ok_or("this session's boundary is gone, so the server was not started")?;
        host.sandboxed(want)
    }

    fn key(&self) -> String {
        match self.host.upgrade() {
            Some(host) => host.sandbox_key(),
            None => "gone".into(),
        }
    }
}

/// The spec's pairs, as the list a launch carries.
pub fn pairs(want: &Want<'_>) -> Vec<(String, String)> {
    want.env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The program a launch runs, as the host path a boundary can bind: an absolute one as spelled, a
/// bare name looked up on `PATH` (`execvp`'s own rule), a relative one against the launch's
/// directory. `None` when nothing is there, and then the exec is the kernel's to refuse.
pub fn program_of(command: &str, cwd: &Path) -> Option<PathBuf> {
    let spelled = Path::new(command);
    let found = if spelled.is_absolute() {
        spelled.to_path_buf()
    } else if command.contains('/') {
        cwd.join(spelled)
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join(command))
            .find(|p| p.is_file())?
    };
    found.exists().then_some(found)
}

/// [`Spawn::launch`] for a boundary: an empty argv is *nothing wraps this* and means the command
/// runs as itself, while a refusal is a server that is not started. A pair a boundary may not put
/// on an argv is handed over separately, for the descriptor the argv names.
pub fn confined(confinement: &dyn Confinement, want: &Want<'_>) -> Result<Launch, String> {
    let env = pairs(want);
    // The boundary's own `--chdir`, and this process's too: a helper started somewhere the
    // namespace does not hold is a launcher that fails before the command runs.
    let cwd = match want.cwd {
        Some(d) => d.to_path_buf(),
        None => std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?,
    };
    // A stdio server is not a job: nothing else has put it in the view, so the launch names its
    // own program and the boundary binds it read-only when the view does not hold it.
    let program = program_of(want.command, &cwd);
    let argv = confinement
        .wrap_program(
            &ConfinePlan {
                cwd: &cwd,
                env: &env,
            },
            program.as_deref(),
        )
        .map_err(|e| e.to_string())?;
    if argv.is_empty() {
        return Literal.launch(want);
    }
    let secrets = env
        .iter()
        .filter(|(k, _)| super::boundary::is_secret_name(k))
        .cloned()
        .collect();
    Ok(Launch {
        // `wrap` answers the argv in FRONT of the command, and its own `--` ends the options:
        // the program and its arguments go after it. Without them bwrap is handed no command,
        // prints its usage text and exits, so the server never starts.
        argv: argv
            .into_iter()
            .chain(std::iter::once(want.command.to_string()))
            .chain(want.args.iter().cloned())
            .collect(),
        // Under `--clearenv` the pairs are on the argv (`--setenv`), so the helper itself is
        // handed none: the process a server sees is the one the boundary describes.
        env: Vec::new(),
        cwd: Some(cwd),
        secrets,
    })
}

/// [`Spawn::key`] for a boundary: nobody asked for one is every session's `literal`, which is what
/// keeps the daemon's one server per spec shared by the sessions that are alike.
pub fn boundary_key(confinement: &dyn Confinement) -> String {
    match confinement.boundary() {
        None => "literal".into(),
        Some(b) => format!(
            "{} | {} | egress {}",
            b.mechanism,
            b.view.summary(),
            match b.egress.allowed() {
                true => "host",
                false => "denied",
            }
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::ExecError;
    use crate::exec::confine::Boundary;

    /// A boundary that answers a prefix and nothing else, so the launch is readable on a box where
    /// no bwrap can make a namespace - which is most seats.
    struct Prefix;

    impl Confinement for Prefix {
        fn describe(&self) -> String {
            "a prefix".into()
        }
        fn boundary(&self) -> Option<&Boundary> {
            None
        }
        fn wrap(&self, _p: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
            Ok(["/usr/bin/bwrap", "--die-with-parent", "--"]
                .map(String::from)
                .to_vec())
        }
    }

    /// `wrap` answers the argv that goes IN FRONT of the command, so the launch has to put the
    /// server's own program and arguments after its `--`: handed only the prefix, bwrap prints its
    /// usage text and exits, and the server never starts.
    #[test]
    fn a_launch_under_a_boundary_carries_the_command() {
        let args = ["--once".to_string(), "MARK".to_string()];
        let env = BTreeMap::new();
        let want = Want {
            command: "letibot-mcp-fake",
            args: &args,
            env: &env,
            cwd: Some(Path::new("/tmp")),
        };
        let launch = confined(&Prefix, &want).expect("a boundary that wraps");
        assert_eq!(
            launch.argv,
            [
                "/usr/bin/bwrap",
                "--die-with-parent",
                "--",
                "letibot-mcp-fake",
                "--once",
                "MARK"
            ]
        );
    }

    /// The program is resolved the way `execvp` resolves it, so a boundary can bind the file it is
    /// about to exec; a name nothing answers to resolves to nothing, and the exec is the kernel's.
    #[test]
    fn a_programs_path_is_the_one_exec_would_use() {
        let cwd = Path::new("/tmp");
        let sh = program_of("sh", cwd).expect("`sh` is on this PATH");
        assert!(sh.is_absolute() && sh.is_file(), "{sh:?}");
        assert_eq!(
            program_of("/bin/sh", cwd).as_deref(),
            Some(Path::new("/bin/sh"))
        );
        assert_eq!(program_of("/nonexistent/server", cwd), None);
        assert_eq!(program_of("nobody-has-this-name", cwd), None);
        // A relative one is the launch's own directory, as `exec` treats it.
        let here = Path::new(".");
        assert!(program_of("./Cargo.toml", here).is_some_and(|p| p.ends_with("Cargo.toml")));
    }
}
