//! `host`: one command on the machine itself, for a session whose tools run somewhere else.
//!
//! A VM seat is the whole point of the seat default - nothing inside it can reach this box - and
//! that is exactly what makes "help me set up ssh" impossible from there. This is the way back:
//! ONE command, run here, under the host seat's own adjudication (so a card is raised where a host
//! seat would raise one), with no lifetime of its own beyond the call. The file tools stay where
//! the work is; a whole session of host work is `leticode_start { vm: false }`.

use serde_json::Value;

use letibot_transcript::text::tail_lines;

use crate::backend::Command;
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// How much of the output is drawn inline, keeping the tail: the same cap `bash` uses, because a
/// reader of both would otherwise learn two different numbers.
const MAX_INLINE_LINES: usize = 400;
const MAX_INLINE_BYTES: usize = 16 * 1024;

pub struct Host;

/// The tool's name, spelled once: the registry, the role and the gate's host-seat point all
/// name it, and three spellings would be a seat that offers a door nothing judges.
pub const HOST_TOOL: &str = "host";

impl Tool for Host {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "host",
            "Run ONE command on this machine, outside the VM this session works in, under the \
             host seat's own adjudication (so it asks you where a host seat would) - the way to \
             set up ssh, a shell profile or anything else that must exist on the host; `command`, \
             optionally `cwd`.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The shell command, run on this machine with `sh -c`."},
                    "cwd": {"type": "string", "description": "Where it runs, relative to this session's workspace on the host. Default `.`."}
                },
                "required": ["command"]
            }),
            Access::Exec,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(host) = ctx.host_seat else {
            return Invocation::not_run(
                "this session's tools already run on the machine itself",
                "`host` is the door out of a VM: a session whose tools run here needs no second \
                 one, and `bash` is the shell. Nothing was run.",
            );
        };
        let Some(command) = args
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|c| !c.is_empty())
        else {
            return Invocation::failed(
                "`host` needs a `command`",
                "call `host` with `command` set to the one shell line to run on this machine.",
            );
        };
        let cwd = args.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");
        let cwd = match host.workdir(cwd) {
            Ok(cwd) => cwd,
            Err(e) => {
                return Invocation::failed(
                    "that `cwd` is not one this machine can run in",
                    format!("{e}. Nothing was run."),
                );
            }
        };
        // `sh -c` and not an argv split: this is a shell line, and the parser that reads it is the
        // shell's. Nothing of this session's environment or secrets is carried across the door.
        let run = host.run(&Command {
            argv: vec!["sh".into(), "-c".into(), command.to_string()],
            cwd,
            env: Vec::new(),
        });
        let output = match run {
            Ok(output) => output,
            Err(e) => {
                return Invocation::failed(
                    "`host` could not run a command on this machine",
                    format!(
                        "{e}. Nothing was run. A machine whose daemon has no process host of its \
                         own cannot carry this door, and `bash` inside the VM is unaffected."
                    ),
                );
            }
        };
        let mut full = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.stderr.is_empty() {
            full.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        let (body, capped) = tail_lines(&full, MAX_INLINE_LINES, MAX_INLINE_BYTES);
        let note = if capped {
            format!("\n\n[the output was longer than {MAX_INLINE_LINES} lines, keeping the TAIL]")
        } else {
            String::new()
        };
        // A non-zero exit is the COMMAND's answer and not a harness failure, as `bash` reads it.
        let head = if output.exit == 0 {
            String::new()
        } else {
            format!("[exit {}] ", output.exit)
        };
        Invocation::ok(format!("{head}{body}{note}"))
    }
}

#[cfg(test)]
mod tests;
