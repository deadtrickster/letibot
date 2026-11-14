use crate::{File, Format, Table, k};

#[rustfmt::skip]
pub const MODES: File = File {
    name: "modes.tsv",
    format: Format::Tsv,
    location: "$XDG_CONFIG_HOME/letibot/modes.tsv, else ~/.config/letibot/modes.tsv",
    about: "legacy: where each project sits. Imported once into the session store; later edits are not read. `/mode` changes the store",
    writer: "",
    tables: &[Table {
        path: "",
        note: "One TAB-separated row per project root; `#` lines are comments. The longest matching ancestor wins; a root with no row is always-ask.",
        needs: "",
        keys: &[
            k("1: mode", "read-only | always-ask | writes-allowed | automode | automode-edits | allow-all", "always-ask", "", "the mode; also plan, default, acceptEdits, bypassPermissions", "allow-all"),
            k("2: root", "absolute path", "required", "", "project root", "/p"),
            k("3: consented_at", "timestamp", "none", "", "when the operator consented to allow-all on a bare host", "2026-10-10T16:50:34Z"),
        ],
    }],
};
