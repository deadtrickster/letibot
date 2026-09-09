//! MCP passthrough — a **seam and a mounting rule**, not a stubbed tool.
//!
//! # Why there is no `mcp` tool here
//!
//! The other three in this module are tools whose schemas we chose. An MCP tool's
//! schema is the server's: dsh passes *"the server's own description and
//! `inputSchema` through verbatim"* (`docs/tool-survey.md` §1.5), and
//! [`Registry::register_foreign`](crate::runtime::Registry::register_foreign)
//! already exists in this crate for exactly that — it runs the description lint and
//! **records** the findings instead of refusing, *"because refusing somebody else's
//! server over its prose would be a harness making a policy nobody asked for"*.
//!
//! So with no server connected there is nothing to stub. A generic
//! `mcp(server, tool, args)` tool would be a schema whose arguments cannot be
//! validated against anything, describing tools that may not exist, and it is a
//! different architecture besides — grok-build's `search_tool` + `use_tool` over a
//! BM25 index of tool descriptions (§1.4). What is honest with nothing connected is
//! **zero tools mounted**, plus a startup line saying so, which is
//! [`super::ExternalWiring::startup_disclosures`].
//!
//! `TODO.md` T16.6 is why that is the state: checked rather than recalled, **no MCP
//! server is running anywhere on this fleet**. The detail that makes it worth
//! re-reading is that a TCP connect to the published port *succeeds while nothing is
//! behind it*, so the naive check reports a connection and not an absence — the same
//! shape as the embed wedge.
//!
//! # What is built: the mount, and §8.4's refusal
//!
//! > MCP servers are attached **per role**, not globally, and an MCP server that
//! > advertises more tools than the role's remaining budget is refused with its tool
//! > list, not silently truncated.
//!
//! That is §8.4, and [`mount`] implements it literally. A server that does not fit
//! is refused **whole**, its tools are named in the report so an operator can see
//! what was on offer, and the budget is untouched — a half-mounted server is a role
//! whose behaviour depends on the order the servers answered in.
//!
//! # What a mounted tool returns is still not ours
//!
//! Mounting a server is the operator's decision and this module does not second
//! guess it. But *"the operator trusts this server"* and *"everything this server
//! returns was written by somebody the operator trusts"* are different claims: a
//! retrieval server returns other people's documents, an issue tracker returns other
//! people's issues. So a result is quarantined like any other foreign text
//! ([`super::quarantine`]), and the operator's decision is about the *server*, which
//! is where it belongs.

use serde_json::Value;

use crate::attach::NotAttached;
use crate::runtime::{Invocation, InvokeCtx, RegisterError, Registry, Tool};
use crate::schema::{Access, ToolSchema};

/// The prefix a mounted tool's name carries, so that a name in a transcript says
/// where it came from without a lookup.
pub const PREFIX: &str = "mcp__";

/// The declared name of one server's tool, before it is mounted.
pub fn mounted_name(server: &str, tool: &str) -> String {
    format!("{PREFIX}{server}__{tool}")
}

/// One tool as a server advertises it. Description and schema are the server's own
/// words and are not rewritten here.
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// One connected server.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServer {
    pub name: String,
    pub tools: Vec<McpToolSpec>,
}

/// What a server answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCallResult {
    pub text: String,
    /// MCP's own `isError`. **Never widened**: a server saying it did not do the
    /// thing must not reach the model as a success (F5).
    pub is_error: bool,
}

#[derive(Debug)]
pub enum McpError {
    NotAttached(NotAttached),
    /// The server is known but is not answering.
    Transport(String),
    /// The server answered with a protocol-level error.
    Protocol(String),
}

/// The seam an MCP client attaches through.
///
/// Deliberately not an MCP *client*: this crate has no async runtime and no HTTP,
/// and `crate::builtins::retrieval`'s module docs give the other half of the reason
/// — a transport written blind, against a server nobody can reach, is the kind of
/// component that reports success while delivering nothing.
pub trait McpCatalog: Send + Sync {
    /// What is connected, right now. An empty list is a complete answer.
    fn servers(&self) -> Vec<McpServer>;

    fn call(&self, server: &str, tool: &str, args: &Value) -> Result<McpCallResult, McpError>;

    fn describe(&self) -> String;
}

/// Nothing connected: no servers, and any call that somehow reaches it refuses.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unavailable;

impl McpCatalog for Unavailable {
    fn servers(&self) -> Vec<McpServer> {
        Vec::new()
    }

    fn call(&self, server: &str, tool: &str, _args: &Value) -> Result<McpCallResult, McpError> {
        Err(McpError::NotAttached(
            NotAttached::new(
                mounted_name(server, tool),
                format!("no MCP server called `{server}`"),
                "nothing was called and no request left this box",
                "connect a server and mount its tools into this session's role; none is \
                 running on this fleet, which was checked rather than assumed",
            )
            .instead("the tools this session already has"),
        ))
    }

    fn describe(&self) -> String {
        "none connected".into()
    }
}

/// One mounted tool: the server's schema, this crate's outcome discipline.
pub struct McpTool {
    pub server: String,
    pub spec: McpToolSpec,
    pub catalog: std::sync::Arc<dyn McpCatalog>,
}

impl Tool for McpTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            mounted_name(&self.server, &self.spec.name),
            // Verbatim. The lint runs at registration and its findings are
            // recorded; rewriting somebody's description would make the model's
            // idea of the tool disagree with the server's.
            self.spec.description.clone(),
            self.spec.parameters.clone(),
            // A tool on another process, usually on another machine. `Network` is
            // the widest thing it can do, and a tool declares the widest.
            Access::Network,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        ctx.progress(format!("calling {} on {}", self.spec.name, self.server));
        match self.catalog.call(&self.server, &self.spec.name, args) {
            Err(McpError::NotAttached(na)) => na.invocation(),
            Err(McpError::Transport(e)) => Invocation::failed(
                format!("`{}` could not be reached: {e}", self.server),
                format!(
                    "`{}` did not run. A connect that succeeds is not a server that \
                     answers, so treat neither as evidence about the other.",
                    self.spec.name
                ),
            ),
            Err(McpError::Protocol(e)) => Invocation::failed(
                format!("`{}` answered with an error: {e}", self.server),
                "the call reached the server and it did not produce a result."
                    .to_string(),
            ),
            Ok(r) if r.is_error => {
                // F5, in the one place it is easiest to get wrong: the transport
                // succeeded, so everything here looks like success.
                Invocation::failed(
                    format!("`{}` reported an error", self.spec.name),
                    format!(
                        "the server answered, and its answer is that the call did not \
                         succeed:\n{}",
                        r.text.trim()
                    ),
                )
            }
            Ok(r) if r.text.trim().is_empty() => Invocation::abstained(
                format!("`{}` returned no content", self.spec.name),
                format!(
                    "`{}` on `{}` ran and produced nothing. It ran — this is its \
                     answer, not a failure to call it.",
                    self.spec.name, self.server
                ),
            ),
            Ok(r) => {
                let (quarantined, note) = super::quarantine(
                    ctx.call_id(),
                    &format!("{} on {}", self.spec.name, self.server),
                    &r.text,
                );
                let mut inv = Invocation::ok(quarantined);
                if let Some(n) = note {
                    inv.notes.push(n);
                }
                inv
            }
        }
    }
}

/// Why one server was not mounted.
#[derive(Debug, Clone, PartialEq)]
pub struct MountRefusal {
    pub server: String,
    pub why: String,
    /// What it was offering, so the refusal is reviewable rather than merely a
    /// number.
    pub tools: Vec<String>,
}

/// What a mount did, in full.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MountReport {
    pub mounted: Vec<String>,
    pub refused: Vec<MountRefusal>,
    /// Descriptions that failed the lint and were mounted anyway, per
    /// [`Registry::register_foreign`].
    pub findings: Vec<(String, Vec<crate::schema::DescriptionFinding>)>,
}

impl MountReport {
    /// One line for the startup banner and for `EXPLAIN`.
    pub fn summary(&self) -> String {
        let mut s = format!("mounted {} MCP tool(s)", self.mounted.len());
        for r in &self.refused {
            s.push_str(&format!(
                "; refused `{}` ({}) offering {}",
                r.server,
                r.why,
                r.tools.join(", ")
            ));
        }
        if !self.findings.is_empty() {
            s.push_str(&format!(
                "; {} description(s) fail the staleness lint and were mounted anyway",
                self.findings.len()
            ));
        }
        s
    }
}

/// Mount every connected server's tools into `registry`, up to `budget`.
///
/// `budget` is the role's **remaining** seats — §8.4's ceiling minus what the role
/// already names. A server whose tool count exceeds what is left is refused whole,
/// with its tool list, and the next server is still considered: refusing the big
/// one is not a reason to drop the small one behind it.
///
/// A name that is already taken is also a refusal and not a rename. Renaming would
/// mean the model calls a tool by a name its own server has never heard of, and the
/// first person to debug that would have no thread to pull.
pub fn mount(
    registry: &mut Registry,
    catalog: std::sync::Arc<dyn McpCatalog>,
    budget: usize,
) -> MountReport {
    let mut report = MountReport::default();
    let mut left = budget;
    for server in catalog.servers() {
        let names: Vec<String> = server.tools.iter().map(|t| t.name.clone()).collect();
        if server.tools.len() > left {
            report.refused.push(MountRefusal {
                server: server.name.clone(),
                why: format!(
                    "it advertises {} tool(s) and {left} seat(s) are left under §8.4's \
                     ceiling; it is refused whole rather than truncated",
                    server.tools.len()
                ),
                tools: names,
            });
            continue;
        }
        let taken: Vec<String> = names
            .iter()
            .map(|n| mounted_name(&server.name, n))
            .filter(|n| registry.get(n).is_some())
            .collect();
        if !taken.is_empty() {
            report.refused.push(MountRefusal {
                server: server.name.clone(),
                why: format!("these names are already registered: {}", taken.join(", ")),
                tools: names,
            });
            continue;
        }
        let before = registry.foreign_findings.len();
        let mut mounted_here = Vec::new();
        for spec in server.tools {
            let name = mounted_name(&server.name, &spec.name);
            let tool = McpTool {
                server: server.name.clone(),
                spec,
                catalog: catalog.clone(),
            };
            match registry.register_foreign(Box::new(tool)) {
                Ok(()) => mounted_here.push(name),
                // Unreachable given the duplicate check above; a refusal rather
                // than an `expect`, because the check and the registry are two
                // mechanisms and this is what happens when they disagree.
                Err(RegisterError::Duplicate(n)) => report.refused.push(MountRefusal {
                    server: server.name.clone(),
                    why: format!("`{n}` was taken between the check and the mount"),
                    tools: vec![n],
                }),
                Err(e) => report.refused.push(MountRefusal {
                    server: server.name.clone(),
                    why: e.to_string(),
                    tools: vec![name],
                }),
            }
        }
        left -= mounted_here.len();
        report.mounted.extend(mounted_here);
        report
            .findings
            .extend(registry.foreign_findings[before..].iter().cloned());
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Fake {
        servers: Vec<McpServer>,
    }

    impl McpCatalog for Fake {
        fn servers(&self) -> Vec<McpServer> {
            self.servers.clone()
        }
        fn call(&self, _s: &str, _t: &str, _a: &Value) -> Result<McpCallResult, McpError> {
            Ok(McpCallResult {
                text: "answered".into(),
                is_error: false,
            })
        }
        fn describe(&self) -> String {
            "fake".into()
        }
    }

    fn spec(name: &str) -> McpToolSpec {
        McpToolSpec {
            name: name.into(),
            description: "Do the thing. Give it a subject.".into(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn catalog(server: &str, tools: &[&str]) -> Arc<dyn McpCatalog> {
        Arc::new(Fake {
            servers: vec![McpServer {
                name: server.into(),
                tools: tools.iter().map(|t| spec(t)).collect(),
            }],
        })
    }

    #[test]
    fn nothing_connected_mounts_nothing_and_says_so() {
        let mut reg = Registry::new();
        let r = mount(&mut reg, Arc::new(Unavailable), 8);
        assert_eq!(reg.len(), 0);
        assert!(r.mounted.is_empty() && r.refused.is_empty());
        assert!(r.summary().contains("mounted 0"), "{}", r.summary());
    }

    #[test]
    fn a_mounted_tool_carries_the_servers_own_name_description_and_schema() {
        let mut reg = Registry::new();
        mount(&mut reg, catalog("oracle", &["ask_code"]), 8);
        let s = reg.schemas();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "mcp__oracle__ask_code");
        assert_eq!(s[0].description, "Do the thing. Give it a subject.");
        assert_eq!(s[0].access, Access::Network);
    }

    #[test]
    fn a_server_over_the_remaining_budget_is_refused_with_its_tool_list() {
        // §8.4, verbatim: refused with its tool list, not silently truncated.
        let mut reg = Registry::new();
        let r = mount(&mut reg, catalog("big", &["a", "b", "c"]), 2);
        assert_eq!(reg.len(), 0, "nothing may be mounted from a refused server");
        assert_eq!(r.refused.len(), 1);
        assert_eq!(r.refused[0].tools, vec!["a", "b", "c"]);
        assert!(r.refused[0].why.contains('3'), "{}", r.refused[0].why);
    }

    #[test]
    fn a_refused_server_does_not_stop_the_next_one() {
        let mut reg = Registry::new();
        let cat: Arc<dyn McpCatalog> = Arc::new(Fake {
            servers: vec![
                McpServer {
                    name: "big".into(),
                    tools: vec![spec("a"), spec("b"), spec("c")],
                },
                McpServer {
                    name: "small".into(),
                    tools: vec![spec("d")],
                },
            ],
        });
        let r = mount(&mut reg, cat, 2);
        assert_eq!(r.mounted, vec!["mcp__small__d"]);
        assert_eq!(r.refused.len(), 1);
    }

    #[test]
    fn a_stale_description_is_mounted_and_the_finding_is_kept() {
        // Somebody else's prose is not ours to refuse — but it is ours to report.
        let mut reg = Registry::new();
        let cat: Arc<dyn McpCatalog> = Arc::new(Fake {
            servers: vec![McpServer {
                name: "s".into(),
                tools: vec![McpToolSpec {
                    name: "t".into(),
                    description: "Search it. The corpus contains the Rust book.".into(),
                    parameters: serde_json::json!({"type": "object"}),
                }],
            }],
        });
        let r = mount(&mut reg, cat, 8);
        assert_eq!(r.mounted.len(), 1);
        assert_eq!(r.findings.len(), 1);
        assert!(r.summary().contains("staleness lint"), "{}", r.summary());
    }
}
