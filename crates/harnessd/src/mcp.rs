//! The daemon's MCP servers: started once per distinct `[mcp]` config and shared by every
//! session that opens under it, so a failure is reported by the open that started the pool.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::leticode_config::McpServerKeys;

/// Whether any enabled server is configured: what seats the two doors.
pub fn configured(tables: &std::collections::BTreeMap<String, McpServerKeys>) -> bool {
    tables.values().any(|k| k.enabled)
}

/// The specs a user file's `[mcp]` tables resolve to, disabled ones left out, and a failure for
/// each table that names neither or both of `command` and `url`.
pub fn specs(
    tables: &std::collections::BTreeMap<String, McpServerKeys>,
) -> (Vec<letibot_mcp_client::ServerSpec>, Vec<letibot_mcp_client::Failure>) {
    let mut specs = Vec::new();
    let mut bad = Vec::new();
    for (name, k) in tables.iter().filter(|(_, k)| k.enabled) {
        let transport = match (&k.command, &k.url) {
            (Some(command), None) => letibot_mcp_client::Transport::Stdio {
                command: command.clone(),
                args: k.args.clone(),
                env: k.env.clone(),
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
