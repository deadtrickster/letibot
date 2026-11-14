//! One module per config file. `ALL` is the order `--config-reference` lists them in.

mod head;
mod hosts;
mod leticode;
mod modes;
mod permission;
mod plugin_trust;
mod prompts;
mod providers;
mod theme;

pub use head::HEAD;
pub use hosts::HOSTS;
pub use leticode::LETICODE;
pub use modes::MODES;
pub use permission::PERMISSION;
pub use plugin_trust::PLUGIN_TRUST;
pub use prompts::PROMPTS;
pub use providers::PROVIDERS;
pub use theme::THEME;

pub static ALL: &[&crate::File] = &[
    &LETICODE,
    &PROVIDERS,
    &HEAD,
    &HOSTS,
    &PROMPTS,
    &THEME,
    &PERMISSION,
    &PLUGIN_TRUST,
    &MODES,
];

/// Files in the config dir that hold no options, for the overview.
pub const OTHERS: &[(&str, &str)] = &[
    (
        "known_hosts",
        "letibot's own ssh host keys; never written by letibot",
    ),
    ("keys/NAME", "default ssh identity per hosts.toml host"),
    (
        "browser.token",
        "the Chrome extension's token, created once, mode 0600",
    ),
    (
        "host-changelog.jsonl",
        "append-only record of host_config changes",
    ),
    (
        "todo-template.md",
        "starter todos, when head.toml todo_template = true",
    ),
    (
        "roles/*.md",
        "role profiles, under the project's .letibot/roles",
    ),
    ("skills/", "user skills"),
    ("plugins/", "user plugin layers"),
];
