# language.md

<!-- the head's own language: `[ui] language` in leticode.toml, and what it does and does not cover -->

The head draws its own interface in English or in Russian, chosen by `[ui] language` in
`leticode.toml`:

```toml
[ui]
language = "auto"       # the box's own: LC_ALL, then LC_MESSAGES, then LANG
# language = "en"       # English, whatever the environment says
# language = "ru"       # Russian
```

`auto` is the default. It reads the environment in the order every shell does - `LC_ALL`,
`LC_MESSAGES`, `LANG` - and takes the language's head off a locale (`ru_RU.UTF-8` is `ru`). `C` and
`POSIX` name no language, so a box that says `LC_ALL=C` reads English.

A code this build does not carry is said on the notice line by name, and English is drawn: a typo in
a language is not a head that will not start.

## What it covers, and what it does not

The catalogs are `crates/tui/messages/en.toml` (the source - every id is there) and `ru.toml`,
compiled into the binary. A message is `id = "text"`, with `{name}` for a value the caller supplies;
a language that does not carry an id reads the English one, never the id itself.

Translated today:

- the status row under the composer - `waiting on:`, the waiting kinds and their counts, the
  editor's `no file` / `view only` / `modified`;
- a severity's word in a plugin pane's findings (`tripped`, `missing`, `passed`);
- the permission card: its rungs, what a grant lasts, what silence does, what a typed line becomes,
  its hints and its keys row;
- the session list: its title, its group headers and legends, the `system` branch's groups, and a
  row's facts (`on disk`, `nags paused`, `N rows`, `N held`, the rollups);
- the setup cards: the model card's columns and states, the key card's hint row, and what the head
  says when a key is stored or the card is cancelled;
- `/help`: its title and footer, and every slash verb's one-line help.

Still English, deliberately: the `/help` screen's key rows, the session list's own hint and note
rows and its legend map, everything rano draws (the composer, the pickers, rano's own cards), every
sentence the daemon composes (a call's summary, an option label this head does not recognise, a
usage-limit's words), `--config-reference`, `/theme`'s listing, and the badges, errors and refusal
codes.

Never translated, in any build:

- anything sent to the model, and anything in a transcript row;
- store rows, log lines, warning codes and error codes;
- command names, config keys, and paths.

## Adding a language

Add `crates/tui/messages/xx.toml`, add the language to `Lang` in `crates/tui/src/i18n.rs` (its code,
its name, its file, and the plural forms it distinguishes), and run the tests: every id in the new
file must exist in English, every `{name}` in a translation must be one its English names, and no
translated string may be byte-identical to its English. Russian carries three plural forms where
English carries two, so a count's ids are `…one` / `…few` / `…many` / `…other` and `i18n::count`
picks one.
