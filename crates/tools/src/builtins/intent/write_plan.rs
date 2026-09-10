//! `write_plan` — the scoped write plan mode keeps (D9).
//!
//! # The absurdity this removes
//!
//! Plan mode's first cut dropped every write tool, which made it *"no writes"*. The
//! operator's correction: plan mode is **no writes to the work**, and writing the
//! plan is one of the two things a plan *is*. A planner that cannot record its plan
//! has to carry the plan in the context it is about to hand over — which is the
//! worst place to keep it, since a handover is exactly where that context is lost.
//!
//! # The boundary moved from the verb to the target
//!
//! Not *"the `write` tool is absent"* but *"a write whose target is a plan document
//! is reachable and nothing else is."* And it is a **capability boundary rather than
//! a check**, per brief §2.4: this tool takes a `name`, not a `path`.
//!
//! ```text
//!   write      path: "src/main.rs"        →  not seated in plan mode at all
//!   write_plan name: "rewrite-the-parser" →  plans/rewrite-the-parser.md
//!   write_plan name: "../src/main.rs"     →  refused: that is not a name
//! ```
//!
//! There is no argument in which a path to `src/` can be spelled. The tool composes
//! the path itself from [`PLAN_DIR`] and a single filename segment; a `name`
//! carrying `/`, `..`, a leading `.` or a NUL is refused as **not a name**, which is
//! a different refusal from "not allowed" and is the one that teaches the right
//! thing. That is a stronger property than a guard on `write`'s `path`, because a
//! guard has to be right about every spelling of an escape and this has to be right
//! about one character class.
//!
//! # It is still a write, and it is still adjudicated
//!
//! [`Access::Write`], so it goes to the gate like any other write, and the backend
//! must have been opened writable. A read-only session cannot write its plan — but
//! that is the session being read-only, not plan mode taking something away, and
//! the two are worth keeping distinguishable. What plan mode changes is that
//! `write` and `edit` are *not seated*, while this one is.

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// Where plan documents live, relative to the session root.
///
/// One directory, named once. A configurable location would put the boundary back
/// in configuration, where nothing checks it — and the whole point of D9's shape is
/// that the target set is knowable from the tool rather than from a setting.
pub const PLAN_DIR: &str = "plans";

/// The extension every plan document gets. Plans are prose for people to read, and
/// markdown is what the rest of this tree writes prose in.
pub const PLAN_EXT: &str = "md";

/// The most bytes one plan document may be. A plan longer than this is a design
/// document, and a design document is work — which is the thing plan mode is not
/// for.
const MAX_PLAN_BYTES: usize = 128 * 1024;

pub struct WritePlan;

/// Turn a model-supplied `name` into a path under [`PLAN_DIR`], or say why it is
/// not a name.
///
/// The rejections are deliberately about *shape*, not about permission: a name
/// with a slash in it is not a name that was refused, it is not a name.
fn plan_path(name: &str) -> Result<String, String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("a plan needs a name — one word or a few joined by `-`".into());
    }
    if n.len() > 120 {
        return Err(format!(
            "`{}…` is {} characters; a plan's name is a label, not its first paragraph",
            &n[..n.floor_char_boundary(40)],
            n.chars().count()
        ));
    }
    for bad in ['/', '\\', '\0'] {
        if n.contains(bad) {
            return Err(format!(
                "`{n}` contains `{}`, so it is a PATH and not a name. `write_plan` takes \
                 a name and puts the document under `{PLAN_DIR}/` itself — there is no \
                 argument here that can name a file anywhere else, which is why plan mode \
                 can write a plan without being able to touch the work.",
                if bad == '\0' { "NUL".into() } else { bad.to_string() }
            ));
        }
    }
    if n.contains("..") || n.starts_with('.') {
        return Err(format!(
            "`{n}` is not a name: a leading `.` or a `..` reads as a path. Use letters, \
             digits, `-` and `_`."
        ));
    }
    let stem = n.strip_suffix(&format!(".{PLAN_EXT}")).unwrap_or(n);
    Ok(format!("{PLAN_DIR}/{stem}.{PLAN_EXT}"))
}

impl Tool for WritePlan {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "write_plan",
            "Write the plan you are working out to a document. Give `name` — a label, \
             not a path — and `content`. The document goes under the session's plan \
             directory and this tool cannot write anywhere else: there is no path \
             argument, so a name with a slash in it is refused as not being a name. \
             Use `append: true` to add to a plan you already started. This is the one \
             write that stays available while you are planning.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "A label for this plan: letters, digits, `-` and `_`. Not a path — the directory is not yours to choose."},
                    "content": {"type": "string", "description": "The plan, in markdown."},
                    "append": {"type": "boolean", "description": "Add to the end of an existing plan instead of replacing it."}
                },
                "required": ["name", "content"]
            }),
            // It writes bytes to the operator's disk, so it declares what it does.
            // The *scope* is what makes it cheap to approve, not a smaller class.
            Access::Write,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(name) = args.get("name").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "write_plan needs a name",
                format!(
                    "call `write_plan` again with `name` set to a label for this plan and \
                     `content` set to the plan. The document lands under `{PLAN_DIR}/`; \
                     you do not choose the directory."
                ),
            );
        };
        let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "write_plan needs content",
                "call `write_plan` again with `content` set to the plan, in markdown. An \
                 empty plan document is worse than none: it reads as a plan that exists.",
            );
        };
        let path = match plan_path(name) {
            Ok(p) => p,
            Err(why) => {
                return Invocation::failed(
                    format!("`{name}` is not a plan name"),
                    format!(
                        "{why}\n\nplans are written as `{PLAN_DIR}/<name>.{PLAN_EXT}` and \
                         this tool composes that itself."
                    ),
                );
            }
        };
        let append = args
            .get("append")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let existing = ctx
            .backend
            .read(&path)
            .ok()
            .map(|b| super::super::text_of(&b).0);
        let body = match (&existing, append) {
            (Some(prior), true) => {
                let mut s = prior.clone();
                if !s.ends_with('\n') {
                    s.push('\n');
                }
                s.push('\n');
                s.push_str(content);
                s
            }
            _ => content.to_string(),
        };
        if body.len() > MAX_PLAN_BYTES {
            return Invocation::failed(
                format!("this plan is {} bytes", body.len()),
                format!(
                    "a plan document is capped at {MAX_PLAN_BYTES} bytes and nothing was \
                     written. A plan this long is a design document, and a design document \
                     is work — leave plan mode and write it with `write`, or split it."
                ),
            );
        }

        if let Err(e) = ctx.backend.write(&path, body.as_bytes()) {
            let extra = if !ctx.backend.is_writable() {
                format!(
                    " This session's backend was opened read-only ({}), so no write can \
                     land however the gate answers. That is the session being read-only \
                     and not plan mode: plan mode seats this tool, it does not open the \
                     disk.",
                    ctx.backend.describe()
                )
            } else {
                String::new()
            };
            return Invocation::failed(format!("could not write `{path}`: {e}"), extra);
        }
        ctx.files.record(&path, body.as_bytes(), true);

        let verb = if existing.is_some() {
            if append { "appended to" } else { "replaced" }
        } else {
            "wrote"
        };
        Invocation::ok(format!(
            "{verb} `{path}` ({} bytes, {} line(s))",
            body.len(),
            body.lines().count()
        ))
        .with_note(format!(
            "the plan is on disk, so it does not have to survive in context to survive a \
             handover. `write` and `edit` are still not seated: this tool reaches \
             `{PLAN_DIR}/` and nothing else."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_transcript::ToolOutcome;

    fn harness() -> crate::testing::Harness {
        let mut h = crate::testing::writable_harness();
        h.rt.registry.register(Box::new(WritePlan)).unwrap();
        h
    }

    #[test]
    fn a_plan_lands_under_the_plan_directory_and_nowhere_else() {
        let mut h = harness();
        let r = h.call(
            "write_plan",
            r##"{"name":"rewrite-the-parser","content":"# Plan\n\nread it first.\n"}"##,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok, "{r:?}");
        assert!(r.render().contains("plans/rewrite-the-parser.md"), "{}", r.render());
        assert!(h.read_file("plans/rewrite-the-parser.md").contains("read it first"));
    }

    #[test]
    fn a_write_to_the_work_is_not_expressible_here() {
        // Not "refused because it is not allowed" — refused because it is not a
        // name. There is no argument on this tool that names a file outside the
        // plan directory, which is the whole of D9's mechanism.
        let mut h = harness();
        for attempt in [
            r#"{"name":"../src/main.rs","content":"x"}"#,
            r#"{"name":"/etc/passwd","content":"x"}"#,
            r#"{"name":"sub/dir","content":"x"}"#,
        ] {
            let r = h.call("write_plan", attempt);
            assert!(
                matches!(r.outcome, ToolOutcome::Failed { .. }),
                "{attempt} was accepted: {r:?}"
            );
            assert!(
                r.render().contains("not a name") || r.render().contains("is a PATH"),
                "{}",
                r.render()
            );
        }
        // `.ssh` never reaches the tool at all: the gate's NEVER_WRITE list stops
        // it first, with `Denied` rather than `Failed`. Two independent mechanisms
        // arriving at the same answer is the shape this crate wants, and asserting
        // only on one of them would have hidden the other.
        let r = h.call("write_plan", r#"{"name":".ssh","content":"x"}"#);
        assert!(
            matches!(
                r.outcome,
                ToolOutcome::Failed { .. } | ToolOutcome::Denied { .. }
            ),
            "{r:?}"
        );
        assert!(h.read_file("plans/.ssh.md").is_empty());
        assert!(h.read_file("src/main.rs").is_empty());
        assert!(h.read_file("src/lib.rs").contains("parse_args"));
    }

    #[test]
    fn appending_keeps_what_was_there() {
        let mut h = harness();
        h.call("write_plan", r#"{"name":"p","content":"first"}"#);
        let r = h.call("write_plan", r#"{"name":"p","content":"second","append":true}"#);
        assert_eq!(r.outcome, ToolOutcome::Ok);
        let on_disk = h.read_file("plans/p.md");
        assert!(on_disk.contains("first") && on_disk.contains("second"), "{on_disk}");
        assert!(r.render().contains("appended to"), "{}", r.render());
    }

    #[test]
    fn a_name_that_already_carries_the_extension_does_not_get_two() {
        assert_eq!(plan_path("p.md").unwrap(), "plans/p.md");
        assert_eq!(plan_path("p").unwrap(), "plans/p.md");
    }

    #[test]
    fn a_plan_over_the_cap_writes_nothing() {
        let mut h = harness();
        let big = "x".repeat(MAX_PLAN_BYTES + 1);
        let args = serde_json::json!({"name": "big", "content": big}).to_string();
        let r = h.call("write_plan", &args);
        assert!(matches!(r.outcome, ToolOutcome::Failed { .. }));
        assert!(h.read_file("plans/big.md").is_empty(), "nothing was written");
        assert!(r.render().contains("is work"), "{}", r.render());
    }

    #[test]
    fn a_read_only_session_says_it_is_the_session_and_not_plan_mode() {
        let mut h = crate::testing::harness();
        h.rt.registry.register(Box::new(WritePlan)).unwrap();
        h.rt = h.rt.with_gate(crate::testing::allow_all());
        let r = h.call("write_plan", r#"{"name":"p","content":"x"}"#);
        assert!(matches!(r.outcome, ToolOutcome::Failed { .. }), "{r:?}");
        assert!(r.render().contains("not plan mode"), "{}", r.render());
    }

    #[test]
    fn the_description_lints_clean() {
        let s = WritePlan.schema();
        assert_eq!(crate::schema::lint_description(&s.description), vec![]);
        assert!(s.description.len() < 800);
    }
}
