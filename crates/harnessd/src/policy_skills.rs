//! Policy skills: standing rules for the box (`kind: policy`). Never offered as optional help. A
//! pinned one rides the prompt whole; any other is delivered once per session, at the first `bash`
//! call its `[skills.when.<name>] command` trigger matches, and that call is held until it is re-issued.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use letibot_tools::builtins::skill::{Skill, SkillRegistry, command_runs};
use letibot_tools::runtime::{CallHook, Patched, Standing};
use letibot_transcript::TranscriptItem;

/// The head of every delivery, so a restored session can tell which rules it was already given.
pub const MARK: &str = "[standing rules for this box - policy skill";

/// The layer name a held call is reported under.
pub const BY: &str = "policy";

/// The rules as the model is given them: worded as rules in force, never as a suggestion.
/// `at` says what brought them, e.g. "every `cargo` call".
pub fn rules(skill: &Skill, at: &str) -> String {
    format!(
        "{MARK} `{}`, the harness's own text, not the operator's message] These are rules for {at} \
         on this box. They are not a suggestion and not background help: follow them now and for \
         the rest of this session. They are given once.\n\n{}{}",
        skill.name,
        dir_line(skill),
        skill.body.trim_end()
    )
}

/// The skill's base directory as a line of its own, or nothing for a skill with no file.
pub fn dir_line(skill: &Skill) -> String {
    skill
        .base_dir_line()
        .map(|l| format!("{l}\n"))
        .unwrap_or_default()
}

/// The pinned policy skills as one block of the standing notes, or `None` when none is pinned.
pub fn pinned_block(registry: &SkillRegistry) -> Option<String> {
    let pinned = registry.pinned_policies();
    if pinned.is_empty() {
        return None;
    }
    let mut out = String::from(
        "### standing rules for this box\nThe skills below are policy, pinned by the operator: \
         rules to follow in every call they govern, not help to weigh.\n",
    );
    for s in pinned {
        out.push_str(&format!(
            "#### {}  [{}]\n{}{}\n\n",
            s.name,
            s.source.label(),
            dir_line(&s),
            s.body.trim_end()
        ));
    }
    Some(out)
}

/// The policy skills this session already holds: pinned or declared at open, loaded by the model,
/// or delivered. Shared by the call hook and the turn loop, so the rules arrive once.
#[derive(Clone, Default, Debug)]
pub struct Delivered(Arc<Mutex<BTreeSet<String>>>);

impl Delivered {
    pub fn has(&self, name: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(name)
    }

    /// Record `name`; `true` when it was not held before.
    pub fn mark(&self, name: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string())
    }

    /// Read back what the transcript shows: a `skill` load, or a delivery's own head. The
    /// transcript is the memory, so a restarted session is not given the rules twice.
    pub fn seed(&self, items: &[TranscriptItem]) {
        for item in items {
            if let TranscriptItem::Assistant { tool_calls, .. } = item {
                for call in tool_calls {
                    if let Some(name) = crate::skills_offer::loaded_of(call) {
                        self.mark(&name);
                    }
                }
            }
            let text = letibot_sessionlog::view::body_of(item);
            let mut rest = text.as_str();
            while let Some(at) = rest.find(MARK) {
                rest = &rest[at + MARK.len()..];
                if let Some(name) = rest
                    .strip_prefix(" `")
                    .and_then(|r| r.split_once('`'))
                    .map(|(n, _)| n)
                {
                    self.mark(name);
                }
            }
        }
    }
}

/// The call hook that delivers a policy skill at its trigger, in front of the plugin layer.
pub struct PolicyHook {
    inner: Option<Arc<dyn CallHook>>,
    skills: Arc<SkillRegistry>,
    delivered: Delivered,
}

impl PolicyHook {
    pub fn new(
        skills: Arc<SkillRegistry>,
        delivered: Delivered,
        inner: Option<Arc<dyn CallHook>>,
    ) -> PolicyHook {
        PolicyHook {
            inner,
            skills,
            delivered,
        }
    }

    /// The held call's answer, when this call is the first to run a policy skill's program.
    fn hold(&self, call: &letibot_plugin::Call) -> Option<Patched> {
        match call.name.as_str() {
            "skill" => {
                if call.arguments.get("action").and_then(|v| v.as_str()) == Some("load")
                    && let Some(name) = call.arguments.get("name").and_then(|v| v.as_str())
                {
                    self.delivered.mark(name);
                }
                None
            }
            "bash" => {
                let command = call.arguments.get("command")?.as_str()?;
                let mut texts = Vec::new();
                for s in self.skills.model_skills().iter().filter(|s| s.policy) {
                    let Some(word) = self
                        .skills
                        .triggers(s)
                        .iter()
                        .find(|w| command_runs(command, w))
                    else {
                        continue;
                    };
                    if self.delivered.mark(&s.name) {
                        texts.push(rules(s, &format!("every `{word}` call")));
                    }
                }
                if texts.is_empty() {
                    return None;
                }
                Some(Patched {
                    out: letibot_plugin::tool_call::ToolCallOut::answer(format!(
                        "{}\n\nThis call was held and did not run. Issue it again, following \
                         these rules.",
                        texts.join("\n\n")
                    )),
                    by: BY.to_string(),
                    note: Some(
                        "held by the harness to deliver this box's standing rules; the tool did \
                         not run"
                            .to_string(),
                    ),
                })
            }
            _ => None,
        }
    }
}

impl CallHook for PolicyHook {
    fn before(&self, call: &letibot_plugin::Call, standing: &Standing) -> Option<Patched> {
        if let Some(held) = self.hold(call) {
            return Some(held);
        }
        self.inner.as_ref()?.before(call, standing)
    }
}

#[cfg(test)]
mod tests;
