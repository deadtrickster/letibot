//! The daemon's MCP servers: one process per distinct server spec, shared by every session that
//! names it, so a failure is reported by the open that started it. `/mcp reload` and a changed
//! file at turn start reconcile a session's set without a daemon restart.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use crate::config::Config;
use crate::leticode_config::McpServerKeys;

/// Each server a session may start, or why it cannot, by name.
pub type Tables = BTreeMap<String, Result<McpServerKeys, String>>;

/// The user's `[mcp]` tables, joined by the workspace's `.mcp.json` when `[mcp_projects] pinned`
/// holds its path and bytes, and a warning for each project server left unstarted. The user's table wins a name.
pub fn servers(cfg: &Config) -> (Tables, Vec<String>) {
    let mut tables: Tables = cfg
        .leticode_user
        .mcp
        .iter()
        .map(|(n, k)| (n.clone(), Ok(k.clone())))
        .collect();
    let mut warnings = Vec::new();
    if !cfg.read_project_config {
        return (tables, warnings);
    }
    let read = match crate::leticode_config::mcp_json(&cfg.workspace) {
        Ok(Some(r)) if !r.servers.is_empty() => r,
        Ok(_) => return (tables, warnings),
        Err(why) => {
            warnings.push(format!("{why}. Its MCP servers are not started."));
            return (tables, warnings);
        }
    };
    let file = read.path.display().to_string();
    let names = read.servers.keys().cloned().collect::<Vec<_>>().join(", ");
    let pin_line = format!("[mcp_projects.pinned] \"{file}\" = \"{}\"", read.sha256);
    // Trusted by these bytes at this path: an edit, or the same file in another worktree, is
    // a file nobody read yet.
    use crate::leticode_config::McpPin;
    match crate::leticode_config::mcp_pin(&cfg.leticode_user.mcp_pinned, &read.path, &read.sha256) {
        McpPin::Held => {}
        McpPin::Moved { pinned } => {
            warnings.push(format!(
                "{file} changed since it was pinned (sha256 {pinned}, now {}), so its MCP \
                 server(s) {names} are not started. Read it, then pin it again in the user \
                 leticode.toml: {pin_line}.",
                read.sha256
            ));
            return (tables, warnings);
        }
        McpPin::Unpinned => {
            let under = if crate::leticode_config::mcp_trusts(
                &cfg.leticode_user.mcp_trusted,
                &cfg.workspace,
            ) {
                " [mcp_projects] trusted names its directory, and a directory is not a pin."
            } else {
                ""
            };
            warnings.push(format!(
                "{file} declares MCP server(s) {names} and a checkout starts no process until \
                 the user leticode.toml pins the file's bytes.{under} After reading it: \
                 {pin_line}."
            ));
            return (tables, warnings);
        }
    }
    let project = read.servers;
    for (name, k) in project {
        if tables.contains_key(&name) {
            warnings.push(format!(
                "{file} declares MCP server `{name}`, and the user leticode.toml's [mcp.{name}] \
                 is the one started."
            ));
            continue;
        }
        tables.insert(name, k.map(|k| in_checkout(k, &cfg.workspace)));
    }
    (tables, warnings)
}

/// A relative command path is the checkout's, not the daemon's working directory.
fn in_checkout(mut k: McpServerKeys, workspace: &std::path::Path) -> McpServerKeys {
    if let Some(c) = &k.command
        && c.contains('/')
        && std::path::Path::new(c).is_relative()
    {
        k.command = Some(workspace.join(c).display().to_string());
    }
    k
}

/// Whether any enabled server is configured: what seats the two doors.
pub fn configured(tables: &Tables) -> bool {
    tables
        .values()
        .any(|k| k.as_ref().map_or(true, |k| k.enabled))
}

/// The specs the tables resolve to, disabled ones left out, and a failure for each entry that
/// could not be read or names neither or both of `command` and `url`.
pub fn specs(
    tables: &Tables,
) -> (
    Vec<letibot_mcp_client::ServerSpec>,
    Vec<letibot_mcp_client::Failure>,
) {
    let mut specs = Vec::new();
    let mut bad = Vec::new();
    for (name, k) in tables {
        let k = match k {
            Ok(k) if !k.enabled => continue,
            Ok(k) => k,
            Err(why) => {
                bad.push(letibot_mcp_client::Failure {
                    server: name.clone(),
                    why: why.clone(),
                });
                continue;
            }
        };
        let transport = match (&k.command, &k.url) {
            (Some(command), None) => letibot_mcp_client::Transport::Stdio {
                command: command.clone(),
                args: k.args.clone(),
                env: k.env.clone(),
                cwd: k.cwd.clone(),
            },
            (None, Some(url)) => letibot_mcp_client::Transport::Http {
                url: url.clone(),
                headers: k.headers.clone(),
            },
            (c, _) => {
                bad.push(letibot_mcp_client::Failure {
                    server: name.clone(),
                    why: if c.is_some() {
                        "its table sets both `command` and `url`".into()
                    } else {
                        "its table sets neither `command` nor `url`".into()
                    },
                });
                continue;
            }
        };
        specs.push(letibot_mcp_client::ServerSpec {
            name: name.clone(),
            transport,
            timeout: k
                .timeout_seconds
                .map_or(letibot_mcp_client::DEFAULT_TIMEOUT, Duration::from_secs),
        });
    }
    (specs, bad)
}

/// The daemon's servers: one process per distinct spec, shared by every session naming it.
fn pool() -> &'static Arc<letibot_mcp_client::Pool> {
    static POOL: OnceLock<Arc<letibot_mcp_client::Pool>> = OnceLock::new();
    POOL.get_or_init(letibot_mcp_client::Pool::new)
}

/// The warning for each table refused before any start, such as a `.mcp.json` server naming an
/// unset variable. `/mcp restart` spawns nothing here: only a re-read of the table can fix it.
pub fn refused_lines(failures: &[letibot_mcp_client::Failure]) -> Vec<String> {
    failures
        .iter()
        .map(|f| {
            format!(
                "MCP server `{}` is not started: {}. `/mcp reload` reads it again once its table \
                 is fixed.",
                f.server, f.why
            )
        })
        .collect()
}

/// The warning for each server that did not start.
pub fn failure_lines(failures: &[letibot_mcp_client::Failure]) -> Vec<String> {
    failures
        .iter()
        .map(|f| {
            format!(
                "MCP server `{}` did not start: {}. The other servers serve; `/mcp restart {}` \
                 tries it again.",
                f.server, f.why, f.server
            )
        })
        .collect()
}

/// A file the servers were read from, as last seen.
#[derive(Debug, Clone, PartialEq)]
struct Mark {
    path: PathBuf,
    mtime: Option<SystemTime>,
    sha256: Option<String>,
}

impl Mark {
    fn read(path: &Path) -> Mark {
        use sha2::Digest;
        Mark {
            path: path.to_path_buf(),
            mtime: std::fs::metadata(path).and_then(|m| m.modified()).ok(),
            sha256: std::fs::read(path)
                .ok()
                .map(|b| format!("{:x}", sha2::Sha256::digest(&b))),
        }
    }

    /// Whether the bytes changed. The mtime is checked first; a touch with the same bytes is no change.
    fn moved(&mut self) -> bool {
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        if mtime == self.mtime {
            return false;
        }
        let now = Mark::read(&self.path);
        let moved = now.sha256 != self.sha256;
        *self = now;
        moved
    }
}

/// A reload's change lines and warnings, or why the config did not read.
pub type Read = Result<(Vec<String>, Vec<String>), String>;

/// What `/mcp` acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    List,
    Reload,
    Restart(String),
}

/// One session's MCP servers and the files they were read from. Reconciled by `/mcp reload`,
/// and at turn start or on `/mcp` when a file's bytes or a stdio binary changed.
pub struct Live {
    catalog: Arc<letibot_mcp_client::Catalog>,
    tables: Tables,
    /// Names the user file declares; the others are the checkout's `.mcp.json`.
    user_names: BTreeSet<String>,
    /// `None` for a config that names no box, as in a test that never read one.
    user_file: Option<PathBuf>,
    project_file: PathBuf,
    marks: Vec<Mark>,
    /// The last read's warnings, such as an unpinned `.mcp.json`.
    warnings: Vec<String>,
}

impl Live {
    fn user_source(&self) -> String {
        self.user_file
            .as_ref()
            .map_or("the user leticode.toml".into(), |f| f.display().to_string())
    }

    /// Start what `cfg` names, sharing processes the daemon already runs. The warnings are the
    /// read's, a table refused before any start, and the start failures of servers this open
    /// started.
    pub fn open(cfg: &Config) -> (Live, Vec<String>) {
        let (tables, warnings) = servers(cfg);
        let (specs, bad) = specs(&tables);
        let refused = refused_lines(&bad);
        let (catalog, failed) = pool().catalog(&specs, bad);
        let mut said = warnings.clone();
        said.extend(refused);
        said.extend(failure_lines(&failed));
        let live = Live {
            catalog: Arc::new(catalog),
            tables,
            user_names: cfg.leticode_user.mcp.keys().cloned().collect(),
            user_file: user_file(cfg),
            project_file: cfg.workspace.join(crate::leticode_config::MCP_JSON),
            marks: marks(cfg),
            warnings,
        };
        (live, said)
    }

    pub fn catalog(&self) -> Arc<letibot_mcp_client::Catalog> {
        self.catalog.clone()
    }

    /// Re-read the user file and the pinned `.mcp.json`, then reconcile: one line per change,
    /// and the read's warnings. A file that does not parse changes nothing.
    pub fn reload(&mut self, cfg: &mut Config) -> Read {
        self.marks = marks_at(self.user_file.as_deref(), cfg);
        if let Some(file) = &self.user_file {
            let user = crate::leticode_config::LeticodeConfig::load(file)?;
            cfg.leticode_user.mcp = user.mcp;
            cfg.leticode_user.mcp_pinned = user.mcp_pinned;
            cfg.leticode_user.mcp_trusted = user.mcp_trusted;
        }
        let (tables, warnings) = servers(cfg);
        let (specs, bad) = specs(&tables);
        let mut changes = self.catalog.reconcile(&specs, bad);
        for name in self.catalog.stale() {
            if !changes.iter().any(|c| c.name == name)
                && let Some(c) = self.catalog.restart(&name, "binary updated")
            {
                changes.push(c);
            }
        }
        self.tables = tables;
        self.user_names = cfg.leticode_user.mcp.keys().cloned().collect();
        self.warnings = warnings.clone();
        Ok((changes.iter().map(ToString::to_string).collect(), warnings))
    }

    /// A reload when a file's bytes or a stdio binary changed since the last look; `None`
    /// when nothing did. Checked at turn start and on `/mcp`, never on a timer.
    pub fn check(&mut self, cfg: &mut Config) -> Option<Read> {
        let mut moved = false;
        for m in &mut self.marks {
            moved |= m.moved();
        }
        if !moved && self.catalog.stale().is_empty() {
            return None;
        }
        Some(self.reload(cfg))
    }

    /// The one row a turn-start reload is said in, for the model and the operator, and the
    /// warnings for the hub.
    pub fn turn_start(&mut self, cfg: &mut Config) -> (Option<String>, Vec<String>) {
        let Some(read) = self.check(cfg) else {
            return (None, Vec::new());
        };
        match read {
            Ok((changes, warnings)) if changes.is_empty() => (None, warnings),
            Ok((changes, warnings)) => (Some(format!("mcp: {}", changes.join(", "))), warnings),
            Err(why) => (
                Some(format!(
                    "mcp: the config changed and did not read, so the servers are as they were: {why}"
                )),
                Vec::new(),
            ),
        }
    }

    /// `/mcp`, `/mcp reload`, `/mcp restart NAME`.
    pub fn verb(&mut self, cfg: &mut Config, verb: &Verb) -> crate::slash::SlashReply {
        use crate::slash::SlashReply;
        match verb {
            Verb::List => {
                let mut lines = match self.check(cfg) {
                    None => Vec::new(),
                    Some(Ok((changes, _))) if changes.is_empty() => Vec::new(),
                    Some(Ok((changes, _))) => vec![format!("reloaded: {}", changes.join(", "))],
                    Some(Err(why)) => vec![format!("the config changed and did not read: {why}")],
                };
                lines.extend(self.status_lines());
                SlashReply::listing(lines)
            }
            Verb::Reload => match self.reload(cfg) {
                Ok((changes, warnings)) => {
                    let mut lines = if changes.is_empty() {
                        vec!["mcp: no changes".to_string()]
                    } else {
                        changes.iter().map(|c| format!("mcp: {c}")).collect()
                    };
                    lines.extend(warnings);
                    SlashReply::listing(lines)
                }
                Err(why) => {
                    SlashReply::refused(vec![format!("mcp: {why}; the servers are as they were")])
                }
            },
            Verb::Restart(name) => match self.catalog.restart(name, "by hand") {
                Some(c) if c.failed.is_none() => SlashReply::listing(vec![format!("mcp: {c}")]),
                Some(c) => SlashReply::refused(vec![format!("mcp: {c}")]),
                None => SlashReply::refused(vec![format!(
                    "mcp: no running server `{name}`; /mcp lists them"
                )]),
            },
        }
    }

    /// One line per configured server: name, state, tools, pid or url, and where it was read.
    pub fn status_lines(&self) -> Vec<String> {
        if self.tables.is_empty() {
            let mut lines = vec![format!(
                "no MCP servers configured - [mcp.NAME] in {}",
                self.user_source()
            )];
            lines.extend(self.warnings.iter().cloned());
            return lines;
        }
        let status = self.catalog.status();
        let mut lines = vec![format!(
            "MCP servers ({}) - /mcp reload re-reads the config, /mcp restart NAME restarts one",
            self.tables.len()
        )];
        for (name, table) in &self.tables {
            let source = if self.user_names.contains(name) {
                self.user_source()
            } else {
                self.project_file.display().to_string()
            };
            let disabled = matches!(table, Ok(k) if !k.enabled);
            let line = match status.iter().find(|s| &s.name == name) {
                _ if disabled => format!("{name} · stopped (enabled = false) · {source}"),
                None => format!("{name} · stopped · {source}"),
                Some(s) => {
                    use letibot_mcp_client::State;
                    let state = match &s.state {
                        State::Running => "running".to_string(),
                        State::Down(why) => format!("stopped: {why}; restarts on its next call"),
                        State::Failed(why) => format!("failed: {why}"),
                    };
                    let at = match (&s.pid, &s.url) {
                        (Some(pid), _) => format!(" · pid {pid}"),
                        // The query can carry a key; the headers are never shown.
                        (_, Some(url)) => format!(" · {}", url.split('?').next().unwrap_or(url)),
                        _ => String::new(),
                    };
                    format!("{name} · {state} · {} tools{at} · {source}", s.tools)
                }
            };
            lines.push(line);
        }
        lines.extend(self.warnings.iter().cloned());
        lines
    }
}

/// The user leticode.toml this session's servers are read from: the one loaded, else the box's.
fn user_file(cfg: &Config) -> Option<PathBuf> {
    cfg.leticode_user.path.clone().or_else(|| {
        (!cfg.box_dirs.config.as_os_str().is_empty())
            .then(|| cfg.box_dirs.config.join(crate::leticode_config::FILE_NAME))
    })
}

fn marks(cfg: &Config) -> Vec<Mark> {
    marks_at(user_file(cfg).as_deref(), cfg)
}

fn marks_at(user: Option<&Path>, cfg: &Config) -> Vec<Mark> {
    let mut v: Vec<Mark> = user.into_iter().map(Mark::read).collect();
    if cfg.read_project_config {
        v.push(Mark::read(
            &cfg.workspace.join(crate::leticode_config::MCP_JSON),
        ));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout(test: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("letibot-mcp-json-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join(".mcp.json"),
            r#"{"mcpServers": {"local": {"command": "./bin/srv"}, "fs": {"command": "npx"}}}"#,
        )
        .unwrap();
        d
    }

    fn cfg_at(ws: &std::path::Path) -> Config {
        let mut cfg = Config::for_this_box(ws.to_str().unwrap());
        cfg.workspace = ws.to_path_buf();
        cfg.leticode_user = Default::default();
        cfg.read_project_config = true;
        cfg
    }

    #[test]
    fn an_untrusted_checkouts_servers_are_named_and_not_started() {
        let ws = checkout("untrusted");
        let (tables, warnings) = servers(&cfg_at(&ws));
        assert!(tables.is_empty(), "{tables:?}");
        assert!(
            !configured(&tables),
            "no doors for a repository's own say-so"
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("fs, local"), "{}", warnings[0]);
        assert!(
            warnings[0].contains("[mcp_projects.pinned]"),
            "{}",
            warnings[0]
        );
    }

    /// A user config pinning `ws`'s `.mcp.json` at the bytes on disk now.
    fn pin(cfg: &mut Config, ws: &std::path::Path) {
        let read = crate::leticode_config::mcp_json(ws).unwrap().unwrap();
        cfg.leticode_user.mcp_pinned.insert(read.path, read.sha256);
    }

    #[test]
    fn a_trusted_checkouts_servers_run_in_it_and_the_user_table_wins_a_name() {
        let ws = checkout("trusted");
        let mut cfg = cfg_at(&ws);
        pin(&mut cfg, &ws);
        cfg.leticode_user.mcp.insert(
            "fs".into(),
            McpServerKeys {
                command: Some("mine".into()),
                enabled: true,
                ..Default::default()
            },
        );
        let (tables, warnings) = servers(&cfg);
        assert!(configured(&tables));
        assert_eq!(
            tables["fs"].as_ref().unwrap().command.as_deref(),
            Some("mine")
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("[mcp.fs]"), "{}", warnings[0]);
        let (specs, bad) = specs(&tables);
        assert!(bad.is_empty(), "{bad:?}");
        let local = specs.iter().find(|s| s.name == "local").unwrap();
        let letibot_mcp_client::Transport::Stdio { command, cwd, .. } = &local.transport else {
            panic!("{local:?}");
        };
        assert_eq!(command, &ws.join("./bin/srv").display().to_string());
        assert_eq!(cwd.as_deref(), Some(ws.as_path()));
    }

    /// A `.mcp.json` server whose command names a variable nobody set is refused by name, and the
    /// open's warning carries that name: a session that never runs `/mcp` has no other place to
    /// learn why a tool it was promised is not there.
    #[test]
    fn a_mcp_json_server_naming_an_unset_variable_is_a_warning_that_names_it() {
        let ws = checkout("var");
        std::fs::write(
            ws.join(".mcp.json"),
            r#"{"mcpServers": {"fs": {"command": "npx",
                 "args": ["${LETIBOT_MCP_TEST_UNSET}"], "env": {"A": "1"}}}}"#,
        )
        .unwrap();
        let mut cfg = cfg_at(&ws);
        pin(&mut cfg, &ws);
        let (tables, warnings) = servers(&cfg);
        assert!(warnings.is_empty(), "a held pin says nothing: {warnings:?}");
        let why = tables["fs"].as_ref().unwrap_err();
        assert!(
            why.contains("`${LETIBOT_MCP_TEST_UNSET}`") && why.contains("is not set"),
            "{why}"
        );
        let (specs, bad) = specs(&tables);
        assert!(specs.is_empty(), "nothing to start: {specs:?}");
        assert!(refused_lines(&bad)[0].contains("LETIBOT_MCP_TEST_UNSET"));
        // And what the session is actually told: one `mcp_server` warning at open.
        let (_live, said) = Live::open(&cfg);
        assert!(
            said.iter()
                .any(|s| s.contains("`fs`") && s.contains("LETIBOT_MCP_TEST_UNSET")),
            "{said:?}"
        );
    }

    #[test]
    fn an_edited_mcp_json_under_a_trusted_dir_is_not_started() {
        let ws = checkout("edited");
        let mut cfg = cfg_at(&ws);
        cfg.leticode_user.mcp_trusted = vec![ws.parent().unwrap().to_path_buf()];
        pin(&mut cfg, &ws);
        assert!(configured(&servers(&cfg).0), "the pinned bytes start");
        std::fs::write(
            ws.join(".mcp.json"),
            r#"{"mcpServers": {"evil": {"command": "sh", "args": ["-c", "id"]}}}"#,
        )
        .unwrap();
        let (tables, warnings) = servers(&cfg);
        assert!(
            tables.is_empty(),
            "an edit under a trusted dir started: {tables:?}"
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("changed since it was pinned"),
            "{}",
            warnings[0]
        );
        assert!(warnings[0].contains("evil"), "{}", warnings[0]);
    }

    #[test]
    fn no_project_config_reads_no_mcp_json() {
        let ws = checkout("noproject");
        let mut cfg = cfg_at(&ws);
        cfg.read_project_config = false;
        pin(&mut cfg, &ws);
        let (tables, warnings) = servers(&cfg);
        assert!(tables.is_empty() && warnings.is_empty());
    }

    /// The fake MCP server, when this test binary is started as one; a no-op test otherwise.
    #[test]
    fn fake_mcp_server() {
        if std::env::var_os("LETIBOT_FAKE_MCP").is_some() {
            letibot_mcp_client::fake::serve(&[]);
        }
    }

    /// A `[mcp.NAME]` table that runs this test binary as the fake server.
    fn fake_table(name: &str, tag: &str) -> String {
        let exe = std::env::current_exe().unwrap();
        format!(
            "[mcp.{name}]\ncommand = {:?}\nargs = [\"mcp::tests::fake_mcp_server\", \"--exact\", \
             \"--nocapture\", \"--test-threads=1\"]\nenv = {{ LETIBOT_FAKE_MCP = \"1\", TAG = \"{tag}\" }}\n",
            exe.display().to_string()
        )
    }

    /// A session config whose user file is `file` under a fresh directory.
    fn live_cfg(test: &str, body: &str) -> (Config, PathBuf) {
        let d =
            std::env::temp_dir().join(format!("letibot-mcp-live-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let file = d.join("leticode.toml");
        std::fs::write(&file, body).unwrap();
        let mut cfg = cfg_at(&d);
        cfg.read_project_config = false;
        cfg.leticode_user = crate::leticode_config::LeticodeConfig::load(&file).unwrap();
        (cfg, file)
    }

    /// Rewrite the file so its mtime moves even within the filesystem's timestamp granularity.
    fn rewrite(file: &Path, body: &str) {
        std::fs::write(file, body).unwrap();
        let later = SystemTime::now() + Duration::from_secs(2);
        std::fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(later)
            .unwrap();
    }

    fn pid_of(live: &Live, name: &str) -> u32 {
        let s = live.catalog().status();
        let s = s.iter().find(|s| s.name == name).unwrap();
        assert_eq!(s.state, letibot_mcp_client::State::Running, "{s:?}");
        s.pid.unwrap()
    }

    #[test]
    fn mcp_lists_each_server_with_its_state_and_source() {
        let body = format!(
            "{}[mcp.lsoff]\ncommand = \"x\"\nenabled = false\n\n[mcp.lsbad]\nargs = [\"x\"]\n",
            fake_table("lsup", "ls")
        );
        let (mut cfg, file) = live_cfg("list", &body);
        let (mut live, _) = Live::open(&cfg);
        let reply = live.verb(&mut cfg, &Verb::List);
        assert!(reply.ok);
        let text = reply.lines.join("\n");
        let pid = pid_of(&live, "lsup");
        let src = file.display().to_string();
        assert!(
            text.contains(&format!("lsup · running · 10 tools · pid {pid} · {src}")),
            "{text}"
        );
        assert!(
            text.contains(&format!("lsoff · stopped (enabled = false) · {src}")),
            "{text}"
        );
        assert!(
            text.contains("lsbad · failed: its table sets neither"),
            "{text}"
        );
    }

    #[test]
    fn a_reload_restarts_only_the_changed_server_and_adds_and_removes() {
        let (mut cfg, file) = live_cfg(
            "reload",
            &format!(
                "{}{}{}",
                fake_table("rla", "1"),
                fake_table("rlb", "1"),
                fake_table("rlc", "1")
            ),
        );
        let (mut live, _) = Live::open(&cfg);
        let (a, b) = (pid_of(&live, "rla"), pid_of(&live, "rlb"));
        rewrite(
            &file,
            &format!(
                "{}{}{}",
                fake_table("rla", "2"),
                fake_table("rlb", "1"),
                fake_table("rlc", "1")
            ),
        );
        let reply = live.verb(&mut cfg, &Verb::Reload);
        assert_eq!(reply.lines, ["mcp: rla restarted (config changed)"]);
        assert_ne!(pid_of(&live, "rla"), a);
        assert_eq!(
            pid_of(&live, "rlb"),
            b,
            "the unchanged server kept its process"
        );

        rewrite(
            &file,
            &format!(
                "{}{}{}",
                fake_table("rla", "2"),
                fake_table("rlb", "1"),
                fake_table("rld", "1")
            ),
        );
        let reply = live.verb(&mut cfg, &Verb::Reload);
        assert_eq!(
            reply.lines,
            ["mcp: rlc stopped (removed)", "mcp: rld added"]
        );
        pid_of(&live, "rld");
        assert!(live.catalog().status().iter().all(|s| s.name != "rlc"));
        assert_eq!(
            live.verb(&mut cfg, &Verb::Reload).lines,
            ["mcp: no changes"]
        );
    }

    #[test]
    fn a_changed_file_reloads_once_at_turn_start() {
        let (mut cfg, file) = live_cfg("auto", &fake_table("auto", "1"));
        let (mut live, _) = Live::open(&cfg);
        assert_eq!(live.turn_start(&mut cfg), (None, Vec::new()));
        // Touched with the same bytes: the mtime moved and the hash did not.
        rewrite(&file, &fake_table("auto", "1"));
        assert_eq!(live.turn_start(&mut cfg), (None, Vec::new()));
        rewrite(
            &file,
            &format!("{}{}", fake_table("auto", "2"), fake_table("auto2", "1")),
        );
        assert_eq!(
            live.turn_start(&mut cfg).0.as_deref(),
            Some("mcp: auto restarted (config changed), auto2 added")
        );
        assert_eq!(
            live.turn_start(&mut cfg),
            (None, Vec::new()),
            "once per change"
        );
        rewrite(&file, "[mcp.broken\n");
        let (row, _) = live.turn_start(&mut cfg);
        assert!(row.unwrap().contains("did not read"));
        assert_eq!(live.turn_start(&mut cfg), (None, Vec::new()));
        pid_of(&live, "auto2");
    }

    #[test]
    fn mcp_restart_names_one_server() {
        let (mut cfg, _) = live_cfg(
            "restart",
            &format!("{}{}", fake_table("rsa", "1"), fake_table("rsb", "1")),
        );
        let (mut live, _) = Live::open(&cfg);
        let (a, b) = (pid_of(&live, "rsa"), pid_of(&live, "rsb"));
        let reply = live.verb(&mut cfg, &Verb::Restart("rsa".into()));
        assert!(reply.ok);
        assert_eq!(reply.lines, ["mcp: rsa restarted (by hand)"]);
        assert_ne!(pid_of(&live, "rsa"), a);
        assert_eq!(pid_of(&live, "rsb"), b);
        let reply = live.verb(&mut cfg, &Verb::Restart("nobody".into()));
        assert!(!reply.ok);
    }
}
