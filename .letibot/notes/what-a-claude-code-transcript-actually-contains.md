<!-- abstract: Claude Code's transcript is mostly not conversation - 15 of its 17 line types are never a row - and the reader's traps are one block per line, a declared speaker, a subagent as a FILE, and a cumulative spend. -->

# What a Claude Code transcript actually contains — measured, and the four rakes under it

Say a notice once per TYPE and keep the count: `attachment`, `api-request-blob`, `last-prompt`
and the rest are never a row, so one scrap per unknown LINE buries the conversation. `aiTitle`
is the title. The reader is `crates/claude/src/lib.rs`.

A message is one block per line and nothing repeats - 0 repeated `text` blocks and 0 repeated
`tool_use` ids - so an importer must not dedupe by message id.

The speaker is DECLARED: `origin.kind` and `promptSource`, with `isMeta: true` winning over
both, and absent meaning the operator. A subagent is a FILE (`<uuid>/subagents/agent-*.jsonl`,
its `.meta.json` naming the `toolUseId` of the `Task` call that spawned it), not inline
`isSidechain` lines.

`cost-state` is CUMULATIVE: take the last, never the sum.

Rakes: `<` is a byte, so a frame parser that strips it and reports offsets into the remainder
slices one short and hands the whole frame back as the operator's words; a "first line found"
flag set before the check reports "(no operator line)" for a transcript whose first `user` line
is machinery; and a `<pasted_content>` frame's closing tag carries its id, so matching `</tag>`
exactly misses every paste.
