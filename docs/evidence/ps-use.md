# `ps` usage by coding agents on this box

Scan of every bash tool call in the Claude Code transcripts
(`~/.claude/projects/*/*.jsonl`) and the opencode store
(`~/.local/share/opencode/opencode.db`, `part` table) for a command line
starting with `ps`. Window: 2026-08-23 → 2026-09-14.

**Totals: 294 calls — Claude Code 275, opencode 19.**

Per-day counts (Claude Code dominates; the opencode calls cluster in two
debugging sessions):

```
2026-08-23   8 (opencode)      2026-09-05  27
2026-08-25  21                 2026-09-06  49
2026-08-26  69  <- peak        2026-09-07  10
2026-08-27   2                 2026-09-08  11
2026-08-28  19                 2026-09-09  11
2026-08-29  31                 2026-09-10   9
2026-08-30   4                 2026-09-11   2
2026-08-31   1                 2026-09-13   4
2026-09-03   2                 2026-09-14   6
2026-09-04   3
```

## Categories

### 1. Pattern filter — "is X running, and what is it" (112 · claude 101, opencode 11)

The dominant shape by far: `ps -eo ...` piped into `grep <pattern>`, almost
always `grep -v grep` to drop the self-match. The question being answered is
nearly always "is llama-server up / which model / which port".

> `ps aux | grep -E 'llama-server|qwen' | grep -v grep`
> — opencode, "Llama/Qwen 3.8 dual AMD 9700 troubleshooting", 2026-08-23 20:31 UTC

### 2. Field extraction — awk over ps columns (104 · claude 101, opencode 3)

Same intent as (1) but the model wanted structured fields (pid, ppid, etimes)
or inline formatting, so it reached for awk instead of grep.

> `ps -eo pid,args | awk '/cmake|swap200k/ && !/awk/ {print "  ", $1, substr($0,index($0,$3),60)}'`
> — claude, 2026-08-28 22:09 UTC

### 3. Known-pid inspection (24 · claude 20, opencode 4)

A pid is already known (from a pidfile, a previous call, `$!`); ps answers
"how old is it, is it alive, what was its command line".

> `ps -o pid,etime,cmd= -p 619312 2>/dev/null | tail -1 | grep -oE "CUDA_VISIBLE|n-cpu-moe [0-9]+"`
> — claude, 2026-09-06 10:24 UTC

### 4. Boolean probe — is it running, yes/no (18 · claude 18, opencode 0)

`grep -q` inside a `&& … || …` branch. No output wanted, one bit back.

> `ps -eo args | grep -qE '[l]oophunt.py|[d]rysweep.py' && echo "  (running)" || echo "  (both finished)"`
> — claude, 2026-08-28 21:43 UTC

### 5. Find and kill (15 · claude 15, opencode 0)

ps as the enumeration half of a kill loop — the destructive pattern.

> `ps -eo pid,args | awk '/llama-server/ && /Qwen3.8/ && !/awk/ {print $1}' | while read p; do kill $p 2>/dev/null; done`
> — claude, 2026-08-26 21:56 UTC

### 6. Count (12 · claude 11, opencode 1)

`grep -c` or `| wc -l`: how many instances, not which.

> `ps -eo args= | grep -c '[p]ython3 -$' | sed 's/^/  A\/B process alive: /'`
> — claude, 2026-08-26 18:28 UTC

### 7. Resource ranking — top consumers (8 · claude 8, opencode 0)

`--sort=-pcpu | head`, i.e. ps used as `top` in batch mode.

> `ps -eo pcpu,pid,comm --sort=-pcpu | head -8`
> — claude, 2026-09-07 12:12 UTC

### 8. Process-tree inspection (1 · claude 1, opencode 0)

Parent/children questions via `--ppid`.

> `ps --ppid 4385 -o pid,etimes,args 2>/dev/null || echo "(no children)"`
> — claude, 2026-08-29 06:14 UTC

### 9. Bare inventory — naked `ps` / `ps aux` (0)

Notably absent. Neither agent ever ran a bare `ps` on its own: every one of
the 294 calls filters, selects or sorts. The models treat ps as a *query*
over process state, not as a page to read.

## Observations

- The demand behind ~80% of calls is one question: **"is my server/script
  running, since when, and with what arguments"** — categories 1, 2, 3, 4, 6
  are all phrasings of it.
- The pattern-filter shape (`ps … | grep -v grep`) is a ritual the models
  reproduce from training data, bracket-trick included (`[l]lama-server`).
- All 15 kill loops came from Claude Code during the VL sweep era
  (Aug 26–29) — the only category with destructive intent.
- opencode's calls are concentrated in two troubleshooting sessions on
  Aug 23 and Sep 12–14; Claude Code spread them across the whole window.

## Re-running the scan

```bash
# Claude Code: bash tool calls containing a ps line
jq -r 'select(.type=="assistant") | .timestamp as $t
       | .message.content[]? | select(.type=="tool_use" and .name=="Bash")
       | .input.command as $c | select($c | test("(?m)^ps( |$)"))
       | [$t, $c] | @tsv' ~/.claude/projects/*/*.jsonl

# opencode: bash tool parts in the store
sqlite3 ~/.local/share/opencode/opencode.db \
  "select p.time_created, p.data from part p where p.data like '%\"tool\":\"bash\"%'"
```
