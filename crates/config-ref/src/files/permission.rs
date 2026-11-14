use crate::{File, Format, Table, k};

#[rustfmt::skip]
pub const PERMISSION: File = File {
    name: "permission.json",
    format: Format::Json,
    location: "$XDG_CONFIG_HOME/letibot/permission.json, else ~/.config/letibot/permission.json; $LETIBOT_PERMISSION (JSON) is appended",
    about: "what never asks: per tool, a pattern over its path or one simple bash command, to allow, deny or ask. The last matching rule wins; no match asks",
    writer: "the daemon: installs config/permission.json once when absent, and appends each \"Always allow\" (whole file re-serialised)",
    tables: &[Table {
        path: "",
        note: "Patterns: * any run (including /), ? one character; a trailing ` *` makes the space optional. A bad entry fails the whole file.",
        needs: "",
        keys: &[
            k("$comment", "string", "none", "", "free text; ignored", "\"mine\""),
            k("<tool>", "\"allow\" | \"deny\" | \"ask\"", "ask", "", "one action for every call of the tool (pattern *)", "\"allow\""),
            k("<tool>.<pattern>", "\"allow\" | \"deny\" | \"ask\"", "ask", "", "action for calls matching the pattern; bash matches each simple command", "\"allow\""),
        ],
    }],
};
