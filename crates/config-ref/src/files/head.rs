use crate::{File, Format, Table, k};

#[rustfmt::skip]
pub const HEAD: File = File {
    name: "head.toml",
    format: Format::Toml,
    location: "$XDG_CONFIG_HOME/letibot/head.toml, else ~/.config/letibot/head.toml",
    about: "the head's own preferences: flat `key = value` lines, no tables",
    writer: "the head, from the /config pane and on every saved preference; unknown lines are kept",
    tables: &[Table {
        path: "",
        note: "Booleans take true/false, yes/no or on/off. Shared by every head on the box.",
        needs: "",
        keys: &[
            k("diff", "\"split\" | \"unified\"", "\"split\"", "", "diff layout: two panels or one", "\"unified\""),
            k("thinking", "\"open\" | \"folded\"", "\"folded\"", "", "thinking blocks start open or folded", "\"open\""),
            k("tools", "\"open\" | \"folded\"", "\"folded\"", "", "tool calls start open or folded", "\"open\""),
            k("raw_calls", "bool", "false", "", "show the model's raw call markup under each call", "true"),
            k("todos_hide_done", "bool", "true", "", "the todos pane folds the done group", "false"),
            k("verbosity", "string", "\"normal\"", "", "transcript rung: a profile name, a switch, or `custom ...`", "\"normal\""),
            k("retired", "comma list", "\"\"", "", "note keys this reader retired; written by the head", "\"w|a|1|ff\""),
            k("todo_template", "bool | path", "false", "", "starter todos: true = todo-template.md beside this file, or a path", "true"),
            k("todo_seed", "comma list", "\"\"", "", "hashed projects already seeded; written by the head", "\"abc\""),
            k("git_format", "string | false", "false", "", "git field template; false = built-in", "\"{branch}\""),
            k("rooms_sidebar_width", "columns | false", "false", "", "rooms sidebar width; false = configured or default", "30"),
            k("editor_width", "columns | false", "false", "", "editor column width; false = half the terminal", "80"),
            k("theme", "string", "terminal", "", "themes/NAME.toml beside this file; terminal = terminal palette", "\"gray\""),
            k("color.<role>", "string", "theme's", "", "one role's look over the theme, e.g. \"bg:#2f363b\"", "\"bg:#2f363b\""),
        ],
    }],
};
