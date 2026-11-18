# Config reference

One page per file under `~/.config/letibot`, generated from `crates/config-ref` - the same
tables `letibot --config-reference [FILE]` prints and each parser's tests check. Regenerate
with `LETIBOT_BLESS=1 cargo test -p letibot-config-ref`.

`letibot --config-header FILE` writes the reference as a comment block at the top of a TOML
file (`FILE.bak` first). `head.toml` gets it on every save.

- [leticode.toml](leticode.toml.md)
- [providers.toml](providers.toml.md)
- [head.toml](head.toml.md)
- [hosts.toml](hosts.toml.md)
- [prompts.toml](prompts.toml.md)
- [theme.toml](theme.toml.md) (`themes/NAME.toml`)
- [permission.json](permission.json.md)
- [plugin-trust.json](plugin-trust.json.md)
- [modes.tsv](modes.tsv.md)
- [commands/*.md](commands.md) (Markdown slash commands)

## Changing one

A session does not edit these files: `~/.config/letibot` is masked inside a session so a model
cannot grant itself anything. The `config` tool is the door. `config explain` answers what a key
does from these pages, `config show {file}` prints one with every credential value redacted, and
`config propose {file, edit, why}` puts a diff on a card that **only the operator's own answer
applies**. The result says what a restart means for that file, and each page carries that line.
