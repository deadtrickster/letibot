use crate::{File, Format, Table, k};

#[rustfmt::skip]
pub const HOSTS: File = File {
    name: "hosts.toml",
    format: Format::Toml,
    location: "$LETIBOT_HOSTS, else $XDG_CONFIG_HOME/letibot/hosts.toml, else ~/.config/letibot/hosts.toml",
    about: "machines letibot may place sessions, children and gates on over ssh (docs/remote-hosts.md)",
    writer: "",
    tables: &[
        Table {
            path: "host.<name>",
            note: "NAME is what --host and `host:NAME` take. Unknown keys and a key stated twice fail the whole file. Values are one line.",
            needs: "",
            keys: &[
                k("hostname", "string", "NAME", "", "what ssh connects to", "\"lab\""),
                k("user", "string", "ssh's own", "", "login user", "\"dead\""),
                k("key", "path", "~/.config/letibot/keys/NAME", "", "identity file; absolute, outside ~/.ssh", "\"/etc/letibot/keys/lab\""),
                k("path", "list of dirs", "[]", "", "put in front of the remote PATH", "[\"~/.cargo/bin\"]"),
                k("root", "path", "~/letibot-remote", "", "where remote worktrees live", "\"~/letibot-remote\""),
                k("mise", "false | \"program\"", "found once and cached", "", "mise wrapper: false = off, a string names it", "false"),
                k("reserve", "int", "0", "", "cores kept back for the owner; reported only", "2"),
                k("flags", "list of words", "[]", "", "free-form host words (heavy, tmpfs)", "[\"heavy\"]"),
                k("never_touch", "list of names", "[]", "", "processes, compose projects and containers letibot never stops there", "[\"postgres\"]"),
            ],
        },
        Table {
            path: "local",
            note: "The machine the daemon runs on.",
            needs: "",
            keys: &[k(
                "never_touch",
                "list of names",
                "[]",
                "",
                "processes, compose projects and containers letibot never stops here",
                "[\"postgres\"]",
            )],
        },
    ],
};
