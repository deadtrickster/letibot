use crate::{File, Format, Table, k};

#[rustfmt::skip]
pub const PLUGIN_TRUST: File = File {
    name: "plugin-trust.json",
    format: Format::Json,
    location: "$LETIBOT_PLUGIN_TRUST (empty = memory only), else $XDG_CONFIG_HOME/letibot/plugin-trust.json, else ~/.config/letibot/plugin-trust.json",
    about: "the plugin trust book: which layer directories run without asking, at which hashes",
    writer: "letibot, on /plugin trust, /plugin untrust, a kept rebuild and every tool write of a plugin file; merged under plugin-trust.lock. Unknown keys are dropped on save",
    tables: &[
        Table {
            path: "pins[]",
            note: "A layer runs freely when its module and manifest hashes match an in-force pin for its dir.",
            needs: "",
            keys: &[
                k("name", "string", "\"\"", "", "manifest name when pinned", "\"fuzzpane\""),
                k("dir", "string", "required", "", "canonical layer directory; the entry's identity", "\"/p\""),
                k("module", "sha256", "\"\"", "", "hash of the module file", "\"00\""),
                k("manifest", "sha256", "\"\"", "", "hash of plugin.toml", "\"00\""),
                k("capabilities", "list | null", "null", "", "capabilities when pinned; a rebuild keeps the pin only if unchanged", "null"),
                k("at_ms", "int", "0", "", "time of the pin; the newest per dir wins", "1"),
            ],
        },
        Table {
            path: "revoked[]",
            note: "",
            needs: "",
            keys: &[
                k("dir", "string", "required", "", "layer directory that was un-pinned", "\"/q\""),
                k("at_ms", "int", "0", "", "a pin counts only if newer", "1"),
            ],
        },
        Table {
            path: "writes[]",
            note: "",
            needs: "",
            keys: &[
                k("path", "string", "required", "", "plugin file a tool wrote", "\"/p/plugin.toml\""),
                k("sha256", "string", "\"\"", "", "hash of the bytes written", "\"00\""),
                k("at_ms", "int", "0", "", "time of the write", "1"),
                k("session", "string", "\"\"", "", "session whose tool wrote it", "\"s-1\""),
            ],
        },
    ],
};
