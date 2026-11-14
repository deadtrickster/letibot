use crate::{File, Format, Table, k};

#[rustfmt::skip]
pub const PROMPTS: File = File {
    name: "prompts.toml",
    format: Format::Toml,
    location: "beside providers.toml: $XDG_CONFIG_HOME/letibot/prompts.toml, else ~/.config/letibot/prompts.toml",
    about: "system-prompt overrides: replace a built-in section or append text, for every model or one",
    writer: "",
    tables: &[
        Table {
            path: "base",
            note: "Every model. An unknown table or section name fails the file; the built-in prompt is used.",
            needs: "",
            keys: &[
                k("system_extra", "string", "none", "", "appended last to the system prompt", "\"x\""),
                k("identity", "string", "built-in", "", "replaces the `identity` section; empty = built-in", "\"x\""),
                k("language", "string", "built-in", "", "replaces the `language` section; empty = built-in", "\"x\""),
                k("tree", "string", "built-in", "", "replaces the `tree` section; empty = built-in", "\"x\""),
                k("find_and_read", "string", "built-in", "", "replaces the `find_and_read` section; empty = built-in", "\"x\""),
                k("edit_files", "string", "built-in", "", "replaces the `edit_files` section; empty = built-in", "\"x\""),
                k("view", "string", "built-in", "", "replaces the `view` section; empty = built-in", "\"x\""),
                k("scratch", "string", "built-in", "", "replaces the `scratch` section; empty = built-in", "\"x\""),
                k("notes", "string", "built-in", "", "replaces the `notes` section; empty = built-in", "\"x\""),
                k("working", "string", "built-in", "", "replaces the `working` section; empty = built-in", "\"x\""),
                k("tone", "string", "built-in", "", "replaces the `tone` section; empty = built-in", "\"x\""),
            ],
        },
        Table {
            path: "model.<name>",
            note: "One model, over [base].",
            needs: "",
            keys: &[
                k("system_extra", "string", "none", "", "appended last to the system prompt", "\"x\""),
                k("identity", "string", "built-in", "", "replaces the `identity` section; empty = built-in", "\"x\""),
                k("language", "string", "built-in", "", "replaces the `language` section; empty = built-in", "\"x\""),
                k("tree", "string", "built-in", "", "replaces the `tree` section; empty = built-in", "\"x\""),
                k("find_and_read", "string", "built-in", "", "replaces the `find_and_read` section; empty = built-in", "\"x\""),
                k("edit_files", "string", "built-in", "", "replaces the `edit_files` section; empty = built-in", "\"x\""),
                k("view", "string", "built-in", "", "replaces the `view` section; empty = built-in", "\"x\""),
                k("scratch", "string", "built-in", "", "replaces the `scratch` section; empty = built-in", "\"x\""),
                k("notes", "string", "built-in", "", "replaces the `notes` section; empty = built-in", "\"x\""),
                k("working", "string", "built-in", "", "replaces the `working` section; empty = built-in", "\"x\""),
                k("tone", "string", "built-in", "", "replaces the `tone` section; empty = built-in", "\"x\""),
            ],
        },
    ],
};
