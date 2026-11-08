/** One letibot session the leticode bridge holds, as the pane draws it. */
export type Pkg = {
  session: string
  worktree: string
  name: string
  state: string
  watchOnly: boolean
  closed: boolean
  asks: number
  last: string
  waitingOn: string[]
  todosDone: number
  todosTotal: number
  costUsd: number | null
  rounds: number | null
  /** The commits its closing report named, when it has reported: the package is DONE. */
  reported: string[] | null
}

/** What the pane knows: the packages, when it last asked, and why the last ask failed. */
export type Board = { pkgs: Pkg[]; at: number; error: string | null }

/** One step of a remote run, as the host's runner recorded it. */
export type RunStep = {
  cmd: string
  state: string
  started?: number
  ended?: number
  exit?: number
  failures?: string[]
}

/** A remote test/build run (`rrun`), read from ~/.local/share/letibot/remote-runs/NAME.json. */
export type Run = {
  name: string
  host: string
  branch: string
  sha: string
  state: string
  started: number
  updated: number
  ended?: number
  load?: number | null
  step?: number
  steps: RunStep[]
}

declare module 'claude-code' {
  interface PluginState {
    leticode: { board: Board; runs: Run[]; tail: Tail; jobs: Job[]; todos: { rows: SeatTodo[]; error: string | null } }
  }
}

/** One entry of a session's progress ring (`leticode_events`), as the tail draws it. */
export type TailEntry = { cursor: number; kind: string; state?: string; name?: string; text?: string }

/** The session the transcript pane follows, and what it has read of it so far. */
export type Tail = {
  session: string | null
  name: string
  entries: TailEntry[]
  next: number | null
  error: string | null
}

/** A remote.sh job on a campaign host, or a host's own row (hostLoad, reachable). */
export type Job = {
  host: string
  name?: string
  state?: string
  exit?: number | null
  started?: number
  updated?: number
  cmd?: string
  last?: string
  hostLoad?: number | null
  reachable?: boolean
}

/** One row of this seat's letibot todo board (the `todos` tool). */
export type SeatTodo = { content: string; status: string; waits: string[] }
