import { atom, read, update } from 'claude-code'
import type { Register } from 'claude-code'

import type { Board, Job, Pkg, Run, SeatTodo, Tail, TailEntry } from '../types'

const PANE = 'letibot-packages'
const SERVER = 'leticode'
const POLL_MS = 5000
const RUNS_PANE = 'letibot-runs'
const TAIL_PANE = 'letibot-tail'
/** How many ring entries the tail keeps; the ring itself holds 500. */
const TAIL_KEEP = 400
/** Rows per transcript poll, and how far back a new follow starts. */
const TAIL_BATCH = 12
const TAIL_START = 40
const tail = atom({ plugin: 'leticode', key: 'tail' } as const, {
  session: null,
  name: '',
  entries: [],
  next: null,
  error: null,
} as Tail)
const runs = atom({ plugin: 'leticode', key: 'runs' } as const, [] as Run[])
const jobs = atom({ plugin: 'leticode', key: 'jobs' } as const, [] as Job[])
/** remote.sh jobs: ssh to every host, so polled slower than the bridge. */
const JOBS_MS = 30000
const JOBS_CMD = 'Projects/stroppy-conformity-local/bin/jobs-json.sh'
const TODOS_PANE = 'leticode-todos'
const todos = atom({ plugin: 'leticode', key: 'todos' } as const, { rows: [], error: null } as { rows: SeatTodo[]; error: string | null })

/** This seat's board as the bridge stores it (the `todos` tool), the rows letibot nags about. */
async function refreshTodos($: any): Promise<void> {
  try {
    const got = json(await $.mcp.call(SERVER, 'todos', {}))
    const rows: SeatTodo[] = (got.rows ?? []).map((r: any) => ({
      content: String(r.content ?? r.title ?? ''),
      status: String(r.status ?? 'pending'),
      waits: (r.needs ?? []).map((n: any) => `${n.kind ?? ''} ${n.id ?? n.pid ?? ''}`.trim()),
    }))
    await update($, todos, () => ({ rows, error: null }))
  } catch (err) {
    const why = err instanceof Error ? err.message : String(err)
    await update($, todos, t => ({ ...t, error: why }))
  }
}

async function refreshJobs($: any): Promise<void> {
  try {
    const { stdout: home } = await $.process.run(['sh', '-c', 'printf %s "$HOME"'])
    const script = `${home.trim()}/${JOBS_CMD}`
    // Only a machine with the stroppy campaign checkout has the jobs script.
    if (!(await $.fs.exists(script))) return
    const { stdout } = await $.process.run([script])
    const got: Job[] = JSON.parse(stdout)
    await update($, jobs, () => got)
  } catch {
    // keep the last answer
  }
}
let runsDir: string | null = null

/** Where `rrun` keeps each run's status: ~/.local/share/letibot/remote-runs. */
async function runsDirOf($: any): Promise<string> {
  if (runsDir) return runsDir
  const { stdout } = await $.process.run(['sh', '-c', 'printf %s "$HOME"'])
  runsDir = `${stdout.trim()}/.local/share/letibot/remote-runs`
  return runsDir
}

/** Every run's status file, newest first. A file mid-write is skipped until the next poll. */
async function readRuns($: any): Promise<Run[]> {
  const dir = await runsDirOf($)
  if (!(await $.fs.exists(dir))) return []
  const entries = (await $.fs.list(dir)).filter(
    (e: any) => e.kind === 'file' && e.name.endsWith('.json'),
  )
  const out: Run[] = []
  for (const e of entries) {
    try {
      out.push(JSON.parse(await $.fs.read(`${dir}/${e.name}`)))
    } catch {
      // half-written or not a run: next poll
    }
  }
  return out.sort((a, b) => (b.updated ?? 0) - (a.updated ?? 0))
}

/** Seconds as `1h02m`, `3m05s`, `42s`. */
function took(s: number): string {
  if (s >= 3600) return `${Math.floor(s / 3600)}h${String(Math.floor((s % 3600) / 60)).padStart(2, '0')}m`
  if (s >= 60) return `${Math.floor(s / 60)}m${String(s % 60).padStart(2, '0')}s`
  return `${s}s`
}
const board = atom({ plugin: 'leticode', key: 'board' } as const, {
  pkgs: [],
  at: 0,
  error: null,
} as Board)

/** The JSON a bridge tool answered with: its first text block, parsed. */
function json(result: { content: { type: string; text?: string }[]; isError: boolean }): any {
  const text = result.content.find(c => c.type === 'text')?.text ?? ''
  if (result.isError) throw new Error(text || 'the bridge refused')
  try {
    return JSON.parse(text)
  } catch {
    // the bridge answers a refusal (a session it no longer holds) as plain text
    throw new Error(text.replace(/^Error:\s*/, '') || 'the bridge answered nothing')
  }
}

/** A worktree's last path segment, without the `wt-` every package worktree starts with. */
function nameOf(worktree: string): string {
  const base = worktree.split('/').filter(Boolean).pop() ?? worktree
  return base.replace(/^wt-/, '')
}

/** One line, at most `n` characters. */
function line(s: string, n: number): string {
  const one = (s ?? '').replace(/\s+/g, ' ').trim()
  return one.length > n ? one.slice(0, n - 1) + '…' : one
}

/** The bridge's sessions, with each live one's status folded in. */
async function poll($: any): Promise<Board> {
  const listed = json(await $.mcp.call(SERVER, 'leticode_list', {}))
  const pkgs: Pkg[] = []
  for (const s of listed.sessions ?? []) {
    const pkg: Pkg = {
      session: s.session,
      worktree: s.worktree,
      name: nameOf(s.worktree),
      state: s.state,
      watchOnly: !!s.watch_only,
      closed: s.closed != null,
      asks: s.pending_asks ?? 0,
      last: s.last_message ?? '',
      waitingOn: [],
      todosDone: 0,
      todosTotal: 0,
      costUsd: null,
      rounds: null,
      reported: null,
    }
    if (!pkg.closed) {
      try {
        const st = json(await $.mcp.call(SERVER, 'leticode_status', { session: s.session }))
        const todos: { status: string }[] = st.todos ?? []
        pkg.state = st.state ?? pkg.state
        pkg.todosTotal = todos.length
        pkg.todosDone = todos.filter(t => t.status === 'completed').length
        pkg.costUsd = st.spent?.cost_usd ?? null
        pkg.rounds = st.spent?.rounds ?? null
        pkg.waitingOn = st.waiting_on ?? (st.jobs_open ?? []).map((j: string) => `job ${j}`)
        pkg.last = st.last_message ?? pkg.last
        pkg.asks = (st.pending_asks ?? []).length
        // **A package is done when it has reported**: the bridge parses the brief's closing
        // report into `report`, and keeps the newest one even when words came after it.
        if (st.report) pkg.reported = st.report.commits ?? []
      } catch {
        // A status that failed leaves the listing's own row: the pane says what it knows.
      }
    }
    pkgs.push(pkg)
  }
  return { pkgs, at: await $.clock.now(), error: null }
}

/** Arguments of a tool call as one short line: the path or command when there is one. */
function argLine(raw: string): string {
  try {
    const a = JSON.parse(raw)
    const v = a.path ?? a.command ?? a.cmd ?? a.pattern ?? a.query ?? a.url
    if (typeof v === 'string') return v
  } catch {
    // not JSON: show it as it came
  }
  return raw
}

/**
 * Read the followed session's conversation since the last read through `leticode_transcript`:
 * the operator's prompts, the model's answers in full, and each tool call and outcome folded to
 * one line. Reasoning is left out. New rows scroll the tail to its end, like `tail -f`.
 */
async function pollTail($: any): Promise<void> {
  const t = await read($, tail)
  if (!t.session) return
  try {
    // Tool payloads run to 8000 characters a row, so a poll asks for a few rows only: a large
    // answer is refused by the MCP result limit. A new follow starts near the end, not at row 0.
    let since = t.next
    if (since == null) {
      const probe = json(await $.mcp.call(SERVER, 'leticode_transcript', { session: t.session, limit: 1 }))
      since = Math.max(0, (probe.rows_total ?? 0) - TAIL_START)
    }
    const args: Record<string, unknown> = { session: t.session, limit: TAIL_BATCH, since }
    let got: any
    try {
      got = json(await $.mcp.call(SERVER, 'leticode_transcript', args))
    } catch (err) {
      // A stopped session is not held any more: say so once instead of every poll.
      const why = err instanceof Error ? err.message : String(err)
      if (/not held|no session/.test(why)) {
        await update($, tail, cur => ({ ...cur, error: 'the session has ended; its transcript is no longer served' }))
        return
      }
      throw err
    }
    const fresh: TailEntry[] = []
    for (const r of got.rows ?? []) {
      if (r.kind === 'user') fresh.push({ cursor: r.i, kind: 'user', text: r.text ?? '' })
      else if (r.kind === 'assistant') {
        if (r.text) fresh.push({ cursor: r.i, kind: 'message', text: r.text })
        for (const c of r.calls ?? [])
          fresh.push({ cursor: r.i + 0.5, kind: 'tool', name: c.name, text: argLine(c.arguments ?? '') })
      } else if (r.kind === 'tool_result' && r.outcome !== 'ok')
        fresh.push({ cursor: r.i, kind: 'result', name: r.name, state: r.outcome, text: (r.payload ?? '').split('\n')[0] })
    }
    await update($, tail, cur =>
      cur.session !== t.session
        ? cur
        : {
            ...cur,
            entries: [...cur.entries, ...fresh].slice(-TAIL_KEEP),
            next: got.next ?? cur.next,
            error: null,
          },
    )
    if (fresh.length > 0) void $.ui.scroll({ in: TAIL_PANE, to: 'end' })
  } catch (err) {
    const why = err instanceof Error ? err.message : String(err)
    await update($, tail, cur => ({ ...cur, error: why }))
  }
}

/** Follow a package's session in the tail pane, from what the ring still holds. */
async function follow($: any, p: Pkg): Promise<void> {
  await update($, tail, () => ({ session: p.session, name: p.name, entries: [], next: null, error: null }))
  await $.ui.open({ id: TAIL_PANE, title: `${p.name} — transcript` })
  await pollTail($)
}

async function refresh($: any): Promise<void> {
  void pollTail($)
  void refreshTodos($)
  try {
    const r = await readRuns($)
    await update($, runs, () => r)
  } catch {
    // the runs view keeps its last answer
  }
  try {
    const next = await poll($)
    await update($, board, () => next)
  } catch (err) {
    const why = err instanceof Error ? err.message : String(err)
    await update($, board, b => ({ ...b, error: why }))
  }
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'packages',
      description: 'Show the letibot packages this session runs, beside the chat',
    })
    await $.command.register({
      name: 'leticode-todos',
      description: "Show this seat's letibot todo board beside the chat",
    })
    await $.command.register({
      name: 'runs',
      description: 'Show remote test/build runs (rrun) beside the chat',
    })
    void $.ui.open({ id: PANE, title: 'letibot packages' })
    void $.ui.open({ id: RUNS_PANE, title: 'Runs' })
    void refresh($)
    $.clock.every(POLL_MS, () => {
      void refresh($)
    })
    void refreshJobs($)
    $.clock.every(JOBS_MS, () => {
      void refreshJobs($)
    })
    return next(e)
  })

  on('command.run', { command: 'packages' }, async $ => {
    await refresh($)
    await $.ui.open({ id: PANE, title: 'letibot packages' })
    return { text: 'letibot packages pane opened.' }
  })

  on('command.run', { command: 'leticode-todos' }, async $ => {
    await refreshTodos($)
    await $.ui.open({ id: TODOS_PANE, title: 'Todos' })
    return { text: 'Todos pane opened.' }
  })

  on('ui.render', { component: 'Pane', requestId: TODOS_PANE }, async ($, e) => {
    const { Box, Text } = $.ui.resolve(e)
    const t = await read($, todos)
    const width = Math.max(30, (e.viewport?.columns ?? 60) - 2)
    const mark = (s: string) => (s === 'completed' ? '✓' : s === 'in_progress' ? '▸' : '·')
    const color = (s: string) => (s === 'completed' ? 'green' : s === 'in_progress' ? 'yellow' : undefined)
    return (
      <Box flexDirection="column">
        {t.error && <Text color="red">{line(`todos: ${t.error}`, width)}</Text>}
        {!t.error && t.rows.length === 0 && <Text dimColor>The board is empty.</Text>}
        {t.rows.map(r => (
          <Box flexDirection="column">
            <Text color={color(r.status)} dimColor={r.status === 'completed'}>
              {line(`${mark(r.status)} ${r.content}`, width)}
            </Text>
            {r.waits.length > 0 && <Text dimColor>{line(`  waits on ${r.waits.join(' · ')}`, width)}</Text>}
          </Box>
        ))}
      </Box>
    )
  })

  on('command.run', { command: 'runs' }, async $ => {
    await refresh($)
    await refreshJobs($)
    await $.ui.open({ id: RUNS_PANE, title: 'Runs' })
    return { text: 'Runs pane opened.' }
  })

  on('ui.render', { component: 'Pane', requestId: RUNS_PANE }, async ($, e) => {
    const { Box, Text } = $.ui.resolve(e)
    const list = await read($, runs)
    const js = await read($, jobs)
    const now = Math.floor((await $.clock.now()) / 1000)
    const hostRows = js.filter(j => j.name == null)
    const jobRows = js
      .filter(j => j.name != null)
      .sort((a, b) => (a.state === 'running' ? 0 : 1) - (b.state === 'running' ? 0 : 1) || (b.updated ?? 0) - (a.updated ?? 0))
    const jobColor = (j: Job) =>
      j.state === 'running' ? 'yellow' : j.state === 'done' && j.exit === 0 ? 'green' : 'red'
    const width = Math.max(30, (e.viewport?.columns ?? 60) - 2)
    const color = (state: string) =>
      state === 'passed' ? 'green' : state === 'failed' ? 'red' : state === 'running' ? 'yellow' : undefined
    return (
      <Box flexDirection="column">
        <Text bold>remote.sh jobs</Text>
        <Text dimColor>
          {line(
            hostRows
              .map(h => `${h.host} ${h.reachable ? (h.hostLoad != null ? h.hostLoad.toFixed(1) : '?') : 'down'}`)
              .join(' · '),
            width,
          )}
        </Text>
        {jobRows.length === 0 && <Text dimColor>No recent jobs.</Text>}
        {jobRows.slice(0, 14).map(j => (
          <Box flexDirection="column">
            <Text>
              <Text color={jobColor(j)}>{j.state === 'running' ? '▸' : j.state === 'done' && j.exit === 0 ? '✓' : '✗'} </Text>
              <Text bold>{j.name}</Text>
              <Text dimColor>
                {' '}
                {j.host} {took(Math.max(0, (j.state === 'running' ? now : j.updated ?? now) - (j.started ?? now)))}
                {j.state !== 'running' && j.state !== 'done' ? ` ${j.state}` : ''}
                {j.exit != null && j.exit !== 0 ? ` exit ${j.exit}` : ''}
              </Text>
            </Text>
            {j.state === 'running' && j.last ? <Text dimColor>{line(`  ${j.last}`, width)}</Text> : null}
          </Box>
        ))}
        <Text> </Text>
        <Text bold>rrun runs</Text>
        {list.length === 0 && <Text dimColor>No remote runs yet.</Text>}
        {list.slice(0, 12).map(r => {
          const end = r.ended ?? (r.state === 'passed' || r.state === 'failed' ? r.updated : now)
          const cur = r.steps[r.step ?? 0]
          const failed = r.steps.find(s => s.state === 'failed')
          return (
            <Box flexDirection="column" marginBottom={1}>
              <Text>
                <Text bold>{r.name}</Text>
                <Text color={color(r.state)}> {r.state}</Text>
                <Text dimColor>
                  {' '}
                  {took(Math.max(0, end - r.started))} on {r.host}
                  {r.load != null && ` (load ${r.load.toFixed(1)})`}
                </Text>
              </Text>
              <Text dimColor>{line(`${r.branch} @ ${r.sha}`, width)}</Text>
              {r.steps.map((s, i) => (
                <Text color={color(s.state)} dimColor={s.state === 'waiting'}>
                  {line(
                    `${s.state === 'passed' ? '✓' : s.state === 'failed' ? '✗' : s.state === 'running' ? '▸' : '·'} ${s.cmd}` +
                      (s.started ? `  ${took((s.ended ?? now) - s.started)}` : ''),
                    width,
                  )}
                </Text>
              ))}
              {failed?.failures?.slice(0, 6).map(f => (
                <Text color="red">{line(`  ${f}`, width)}</Text>
              ))}
              {r.state === 'running' && cur && <Text dimColor>{line(`now: ${cur.cmd}`, width)}</Text>}
            </Box>
          )
        })}
      </Box>
    )
  })

  on('ui.render', { component: 'Pane', requestId: TAIL_PANE }, async ($, e) => {
    const { Box, Text } = $.ui.resolve(e)
    const t = await read($, tail)
    const width = Math.max(30, (e.viewport?.columns ?? 80) - 2)
    return (
      <Box flexDirection="column">
        {!t.session && <Text dimColor>Click a package's name in the packages pane to follow it here.</Text>}
        {t.error && <Text color="red">{line(`transcript: ${t.error}`, width)}</Text>}
        {t.entries.map(x =>
          x.kind === 'message' ? (
            <Box key={String(x.cursor)} marginTop={1}>
              <Text wrap="wrap">{x.text ?? ''}</Text>
            </Box>
          ) : x.kind === 'user' ? (
            <Box key={String(x.cursor)} marginTop={1}>
              <Text color="cyan" wrap="wrap">{`› ${line(x.text ?? '', 4 * width)}`}</Text>
            </Box>
          ) : x.kind === 'tool' ? (
            <Text key={String(x.cursor)} dimColor>
              {line(`▸ ${x.name ?? ''}  ${x.text ?? ''}`, width)}
            </Text>
          ) : (
            <Text key={String(x.cursor)} color="red">
              {line(`✗ ${x.name ?? ''} ${x.state ?? ''}  ${x.text ?? ''}`, width)}
            </Text>
          ),
        )}
      </Box>
    )
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Text, Button } = $.ui.resolve(e)
    const b = await read($, board)
    const width = Math.max(30, (e.viewport?.columns ?? 60) - 2)
    const live = b.pkgs.filter(p => !p.closed)
    const done = b.pkgs.filter(p => p.closed)
    const color = (state: string) =>
      state === 'running'
        ? 'green'
        : state === 'asking'
          ? 'yellow'
          : state === 'failed'
            ? 'red'
            : state === 'done'
              ? 'cyan'
              : undefined
    // `idle` with a report is DONE; `idle` without one is a package that stopped and may want you.
    const shown = (p: Pkg) => (p.state === 'idle' && p.reported ? 'done' : p.state)

    return (
      <Box flexDirection="column">
        {b.error && <Text color="red">{line(`bridge: ${b.error}`, width)}</Text>}
        {live.length === 0 && <Text dimColor>No package running.</Text>}
        {live.map(p => (
          <Box flexDirection="column" marginBottom={1}>
            {/* A Button may not sit inside Text: the row is a Box, the name its pressable. */}
            <Box flexDirection="row">
              <Button key={`tail-${p.session}`} plain onPress={() => void follow($, p)}>
                <Text bold>{p.name}</Text>
              </Button>
              <Text>
                <Text color={color(shown(p))}> {shown(p)}</Text>
                {p.reported && p.reported.length > 0 && (
                  <Text dimColor> · {p.reported.slice(0, 3).join(' ')}</Text>
                )}
                {p.watchOnly && <Text dimColor> (watching)</Text>}
                {p.asks > 0 && <Text color="yellow"> · {p.asks} ask{p.asks > 1 ? 's' : ''}</Text>}
              </Text>
            </Box>
            {p.todosTotal > 0 && (
              <Text dimColor>
                todos {p.todosDone}/{p.todosTotal}
                {p.costUsd != null && ` · $${p.costUsd.toFixed(3)}`}
                {p.rounds != null && ` · ${p.rounds} rounds`}
              </Text>
            )}
            {p.waitingOn.length > 0 && (
              <Text dimColor>{line(`waiting on: ${p.waitingOn.join(' · ')}`, width)}</Text>
            )}
            {p.last && <Text>{line(p.last, width)}</Text>}
            <Text dimColor>{line(`attach: cd ${p.worktree} && leticode --session ${p.session}`, width)}</Text>
          </Box>
        ))}
        {done.length > 0 && (
          <Box flexDirection="column">
            <Text dimColor>finished ({done.length}):</Text>
            {done.slice(-8).map(p => (
              <Text dimColor>{line(`  ${p.name}: ${p.last}`, width)}</Text>
            ))}
          </Box>
        )}
      </Box>
    )
  })
}
