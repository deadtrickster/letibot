<!-- abstract: MEASURED 2026-10-29 on the operator's Mac: Claude Code's transcript line census (17 types, 15 of them never rows), a subagent's conversation is a FILE beside the session (`<uuid>/subagents/agent-*.jsonl`) while `isSidechain` inline is the older spelling, `origin.kind`/`promptSource` is a declared speaker, and `cost-state` is cumulative — summing it multiplies the bill. -->

# What a Claude Code transcript actually contains — measured, and the four rakes under it

MEASURED 2026-10-29, branch `agent/claude-transcript-resume` (Package H), on the Mac,
against the operator's 13 transcripts under `~/.claude/projects` — the biggest of them
111.4 MB / 57,900 lines / `e90320f4-b550-4aa0-aa11-1c2673df88e8`. Every number below is
from that file unless it says otherwise.

## The line census, and how little of it is conversation

`{"type": …}` over the whole file:

| type | count | what it is |
|---|---|---|
| `assistant` | 14,736 | the model's blocks, one block per line |
| `attachment` | 10,358 | Claude Code's own context: 32 kinds (`total_tokens_reminder` 8,678, `deferred_tools_record` 4,607, `environment`, `skill_listing`, `file`, `diagnostics`, plans…) |
| `user` | 7,685 | the operator's words, tool results, and frames |
| `api-request-blob` | 4,076 | the request bodies it sent — 76,103 lines of wire capture in one file |
| `api-request` / `-shape` | 2,685 / 21 | the same, by hash |
| `last-prompt` / `ai-title` / `agent-name` | 2,465 each | its own index; **`aiTitle` is the session's title** and is worth reading for a picker |
| `mode` / `permission-mode` / `atis-latch` | 2,454 each | bookkeeping |
| `queue-operation` | 1,569 | the operator's queued prompts, enqueued and removed |
| `system` | 903 | `turn_duration` 719, **`away_summary` 156**, `informational` 19, `compact_boundary` 9 |
| `file-history-snapshot` / `-delta` | 383 / 58 | Claude's own file backups |
| `cost-state` | 25 | **the spend, and it is CUMULATIVE** |

So 15 of the 17 types are never a transcript row. A reader that emits one scrap per
unknown LINE is a reader that buries a conversation under 10,358 notices: say it once per
TYPE, at its first line, and keep the count in the report.

## A message is one block per line, and nothing repeats

4,456 of the 14,736 assistant lines share a `message.id` with another line (up to 8), and
across them there are **0 repeated `text` blocks and 0 repeated `tool_use` ids**. So an
importer must NOT dedupe by message id — a block appears once, on the line that carries
it — and a writer may put a text block and a call in ONE content array (the reader will
hand them back as two rows).

## The speaker is DECLARED, and the two fields are the whole of it

`origin.kind`: `human` 938, `task-notification` 655, `peer` 439, `auto-continuation` 3,
absent 11,487. `promptSource`: `typed` 840, `queued` 54, `suggestion_accepted` 44,
`system` 1,097, absent 11,487. `isMeta: true` wins over both (a `<local-command-caveat>`,
a subagent's hand-back). Absent means the operator — the reading an old row gets anyway.

Frames seen in `user` text: `<task-notification>` (655), `<agent-message from=…>` (431),
`<pasted_content id="…">` (28), `<bash-input>`/`<bash-stdout>` (4 each),
`<local-command-stdout>`, `<local-command-caveat>`, `<command-name|message|args>`,
`<system-reminder>`. The closing tag of a paste carries its id too —
`</pasted_content id="17da">` — so a frame parser that matches `</tag>` exactly misses
every paste.

## A subagent is a FILE, not inline lines

`<uuid>/subagents/agent-<id>.jsonl` beside the transcript (21 of them next to the 111 MB
one), each with a `.meta.json` naming `agentType`, `description` and the **`toolUseId` of
the `Task` call that spawned it**. Its first line is `{"isSidechain": true, "agentId": …}`.
`isSidechain` INLINE in the parent is the older spelling and there were **0** of those
here — which is why the skip is defensible: the parent already holds the `Task` call row
and its report, and the branch is the child's own conversation.

## The spend field is cumulative

`cost-state` carries `totalCostUSD` and `modelUsage`, and the file's own 25 lines run
$425.53 → $1,213.99. **Take the last, never the sum**: summing multiplies the bill by the
number of times Claude Code wrote it down.

# The rakes

* **`<` is a byte.** A frame parser that strips the leading `<` and then reports an `end`
  offset into the remainder slices one byte short, so `classify()` returns the WHOLE frame
  as the operator's words. Measured: `<bash-input> gh pr merge 1</bash-input>` came out
  verbatim instead of `! gh pr merge 1`.
* **`have_first = true` before the check** made a listing row say *(no operator line)* for
  2 of 13 transcripts: their first `user` line is machinery (a `<command-name>`, a
  `<local-command-stdout>`), and the flag then stopped the search. Set it only when the
  words were actually found — the two rows now show their real first asks.
* **`cargo build -p letibot` does not build `harnessd`** — it is a bin of
  `letibot-harnessd`, and a stale binary makes a format change look like it did not
  happen (`--list-claude` printed epoch seconds twice while the source said a civil date).
* **The worktree's binaries live in the MAIN tree's `target/`** (`cargo metadata`'s
  `target_directory` is `/Users/dead/Projects/letibot/letibot/target`), so `./target/debug`
  in a worktree is not a path — `cargo metadata | python3 -c …` is how to find one.
