# Session names: one rule, and the file enforces it

A session's name is an ADDRESS. `-r NAME`, `--session NAME`, `/rename` in a head, `@name` on the
fabric and a bridge tool call all reach a conversation by it, so two sessions answering to one name
is two ways to open the wrong one. This page is the whole rule; the code is
`crates/tokencore/src/store/names.rs` and the schema-62 arm of `store.rs`.

## The rule

**One name, per canonical folder, among the LIVE MAIN sessions.**

- **The folder is the canonical path.** `canonical_folder` resolves symlinks and strips a trailing
  slash, so `/w/p`, `/w/p/` and a link to it are one folder. A path that no longer exists on disk
  is taken as written, minus a trailing slash: a session whose folder was deleted is still a
  session.
- **A name is its slug.** `slug_of` lowercases and turns every run of other characters into one
  `-`, so `Stroppy Conformity` and `stroppy-conformity` are one name. A title with no letters or
  digits has no slug and is filed under its own lowercase spelling.
- **Only live main sessions count.** A child (`parent_session_id`), a shadow and a reviewer are not
  addressed by name and may share one with anybody. An ARCHIVED session frees its name the moment
  it is put away; if it comes back to a name a live session has taken, the RETURNING one takes the
  counter (`night shift-2`) and the door that brought it back says which name it came back as.
- **A name that reads as an id is refused.** Every id this tree mints begins `s-` or `cc-`, so a
  name with that prefix would be a second spelling of one address. Refused where a person types a
  name (`/rename`, `session rename`, `--new TITLE`, `--title`, a head's NewSession title), never at a
  row that already has one.
- **Only a person's name is refused; every other title is DASHED.** A name a person just typed is a
  claim on an address, and taking a different one than they asked for would be a quiet answer to a
  question they asked. A title nothing typed - a package's, a daemon seat's, a reviewer's, a
  shadow's, a test's, a stored row's on the way back in - is the tree's own guess at a label, and a
  session that cannot open is worse than a `-2`. So `Store::put_session` dashes rather than fails,
  and opening or reopening a session never fails on its own name.

## Where it is enforced

The FILE, not a read before a write. `session.workspace_canon` and `session.name_slug` carry the
key, and

```sql
CREATE UNIQUE INDEX session_live_name
    ON session (workspace_canon, name_slug)
    WHERE name_slug IS NOT NULL AND parent_session_id IS NULL AND archived_ms IS NULL;
```

is the rule's scope, written once. Everything else is a door onto it:

| door | what it does on a clash |
|---|---|
| `Store::rename_unique` (`/rename`, `session rename`) | refused, holder named - and an archived session that used to answer to it is named too |
| `Store::set_title` (a raw write) | the index refuses; the sentence is read back so it names the holder |
| `Store::put_session` (a package, a shadow, a reviewer, a restore, a test) | the `-2`, `-3` counter, retried on the index's refusal - so the dash is decided by the same write that the file guards, and never fails the open |
| `Store::name_default` (the auto title) | `title IS NULL` on the write, then the `-2`, `-3` counter; `None` when a rename landed first, so a person's name is never overwritten |
| `Store::free_derived_name` (a fork) | the counter, by the same rule |
| a head's `NewSession` title, `--new TITLE` | refused BY THE FRAME, before a row exists, with the holder named (`SessionSource::name_taken`) |
| `--title` at a daemon start | refused before the socket binds (`Registry::name_ok`), with the holder named: the name came off the command line, so a person typed it |

`schema 62` fills both columns for the rows already there, dashes the duplicates it finds onto the
oldest row that had the name, and builds the index. `repair_name_keys` runs the same pass at every
open, because a build older than 62 writes rows with no key: cheap when the file already says what
it should, and the only thing that keeps a mixed box clean.

## Looking a name up

One resolver, `names::meaning_of`, answers with a `Meaning`:

- `One` - exactly one live session of these folders has the name.
- `Ambiguous` - several do. A store that predates the rule can hold this, and nobody picks one.
- `Prefixes` - nothing is called that; these BEGIN with it. **Offered, never opened silently.**
- `Archived` - nothing live is called that, and an archived session is. Offered with the word, so
  opening it by id brings it back.
- `Nothing`.

`-r NAME` / `--continue NAME` (`find_session`) applies it to this folder's conversations and, only
when nothing there answers, to the operator's own in every folder. The launcher (`scripts/letibot`)
asks on a tty - a numbered list for prefixes, `[y/N]` for an archived session - and with no
terminal every offer is a refusal that names the id and the command to run. `Store::meaning` is the
same rule for a caller that has folders rather than rows, and the Claude Code bridge
(`crates/bridge/src/names.rs`) uses it, refusing an ambiguous, prefix-only or archived name with
the candidates: a tool call has nobody to ask.

## After a rename

- The room's nickname follows: the flowy seat's alias is read from the registry each time it is
  asked (`InboxCondition::set_alias_source`), so `@seat/title` moves with a `/rename` instead of
  keeping the title it was attached with.
- The bridge's label follows the same way, and a label the room REFUSES is not recorded as sent: it
  is retried on a doubling wait (30 s, 1 min, ... six tries) and the last attempt is said aloud,
  with the holder the room named. A Claude Code `/rename` typed while the bridge is running
  outranks a nick file written before it; a title the transcript already held does not, so an
  operator's chosen nick keeps the seat.

## Tests

- `crates/tokencore/src/store/tests.rs`, `mod names`: `a_folder_is_its_canonical_path`,
  `an_archived_session_frees_its_name_and_says_so`, `a_subagent_does_not_hold_a_name`,
  `two_renames_at_once_cannot_both_win`, `a_rename_that_lands_first_beats_the_auto_title`,
  `a_name_that_reads_as_an_id_is_refused`, `a_typed_name_means_exact_then_prefix_then_archived`,
  `a_title_somebody_else_holds_is_dashed_and_never_refused`,
  `a_title_is_written_as_it_was_given`,
  `the_name_migration_dashes_the_duplicates_it_finds`.
- `crates/harnessd/tests/session_names.rs`: `a_new_session_named_twice_is_refused_and_names_the_holder`,
  `a_workspace_spelled_another_way_is_the_same_folder`, `a_name_that_reads_as_an_id_is_refused`,
  `a_title_typed_at_the_start_is_refused_and_names_the_holder`.
- `crates/bridge/src/names.rs`: `a_name_resolves_to_its_id_within_the_folder` (the prefix),
  `an_archived_session_is_refused_as_archived`.
- `crates/bridge/src/rooms.rs`: `a_refused_label_is_waited_out_and_then_given_up_on_aloud`,
  `the_retry_wait_doubles`, `a_rename_after_a_nick_file_wins`.
- `crates/harnessd/src/cli/tests.rs`: `a_name_resolves_exact_and_an_id_is_unchanged_and_a_prefix_is_offered`,
  and the folder tests beside it.
