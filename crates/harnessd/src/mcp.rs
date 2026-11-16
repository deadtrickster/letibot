//! The daemon's MCP servers: started once per distinct `[mcp]` config and shared by every
//! session that opens under it, so a failure is reported by the open that started the pool.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

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

type Pools = Mutex<HashMap<String, Arc<letibot_mcp_client::Catalog>>>;

/// The catalogue for `specs`, and whether this call started it. Keyed by the specs themselves,
/// so two daemons in one test process with different files never share servers.
pub fn pool(
    specs: &[letibot_mcp_client::ServerSpec],
    bad: Vec<letibot_mcp_client::Failure>,
) -> (Arc<letibot_mcp_client::Catalog>, bool) {
    static POOLS: OnceLock<Pools> = OnceLock::new();
    let key = format!("{specs:?}{bad:?}");
    let mut pools = POOLS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(c) = pools.get(&key) {
        return (c.clone(), false);
    }
    let c = Arc::new(letibot_mcp_client::Catalog::start(specs).with_failures(bad));
    pools.insert(key, c.clone());
    (c, true)
}

/// The warning for each server that did not start.
pub fn failure_lines(catalog: &letibot_mcp_client::Catalog) -> Vec<String> {
    catalog
        .failures()
        .iter()
        .map(|f| {
            format!(
                "MCP server `{}` did not start: {}. The other servers serve; its tools are \
                 not offered until the daemon restarts.",
                f.server, f.why
            )
        })
        .collect()
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
}
