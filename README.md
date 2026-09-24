# letibot

A local-first LLM agent harness: **one authoritative reader**, a gate that answers before the
model acts, and a runtime that behaves like a normal data app.

![letibot's TUI, mid-turn](docs/screenshot.jpg)

Two things above, in the same frame: the model's work in flight — `Running "cargo build …" · 4.2s`
and a yellow `[3 tools, 2 thinking]` on the sentence it belongs to — and, on its own permanent row
above the composer, `Responded in 12.4s at 21:07`. The counters go yellow while the work is still
going and stop when it is; the row above the input never moves.

## What it is

`harnessd` is a daemon. `letibot-tui` is one of its heads. A head speaks the session protocol over
a unix socket; **the daemon is the only reader of the session log**, so two heads can be attached to
one conversation without either of them being a second writer. That single-reader rule is §13.2 and
most of the design follows from it.

The runtime does not try to be clever about the model. It tries to be *legible* about it:

- **Every tool call can be gated before it runs.** The gate is asked first, the tool second, and
  what the gate was shown is stored as the trail — not reconstructed later.
- **Compaction is graduated.** Context is summarised in levels rather than in one blocking pass, and
  each level discloses what it cost and what it lost.
- **The wire carries facts, not summaries of facts.** A head can attach to a session that has been
  running for hours and draw it correctly, because the protocol says what happened rather than what
  a client should think about it.
- **Nothing moves that the reader did not ask to move.** A queued message keeps its place; a row
  that has been drawn is not redrawn elsewhere; a counter that stopped is drawn differently from one
  that is counting. This is the constraint the UI work is tested against, and it is why the
  screenshots in the docs are compared byte for byte.

## The crates

| crate | |
|---|---|
| `letibot-transcript` | The conversation record: what the harness stores, and what a dialect renders. |
| `letibot-dialect`, `-qwen`, `-glm` | The seam between the turn engine and whatever actually runs the model. |
| `letibot-turn` | The turn engine: one turn, from a transcript to a transcript. |
| `letibot-sessionlog` | The session log and the head protocol. |
| `letibot-tools` | The tool runtime and the built-ins. |
| `letibot-code` | The structural half of code search: what is defined in this file, where. |
| `letibot-tui` | The terminal head. |
| `letibot-harnessd` | The daemon that assembles the parts into a working harness. |

## Build and run

```sh
cargo build --release

# the daemon; it prints the socket a head attaches to
./target/release/harnessd \
    --dialect qwen --model qwen-3.8-27b --endpoint 127.0.0.1:8080 \
    --vocab path/to/model.gguf \
    --workspace . --store ~/.local/share/letibot/sessions.db

# a head, in another terminal
./target/release/letibot-tui
```

Requires Rust 1.98 and edition 2024. The model endpoint is any OpenAI-shaped server; the dialects
exist because the local ones disagree about how a tool call is spelled.

## A sample of the code

The daemon has one worker and **one blocking call**, and that call takes the clock as an input.
That is what lets it act on a *pause* and not only on an event — which is the difference between a
check that runs after every turn and a check that runs when nobody is waiting:

```rust
pub enum WorkOrIdle {
    Work(Work),
    Idle,
    Closed,
}

/// The command queue is drained before the deadline is looked at, so a head that
/// pressed enter is served at once — the ordering `Bell::next_any` already keeps
/// between work and wakes, now kept between work and the clock.
pub fn next_work_until(&self, deadline: Option<Instant>) -> WorkOrIdle {
    loop {
        match self.bell.next_any_until(deadline) {
            RingWait::Closed => return WorkOrIdle::Closed,
            RingWait::Idle => return WorkOrIdle::Idle,
            RingWait::Ring(Ring::Open(id)) => return WorkOrIdle::Work(Work::Open(id)),
            // A wake for a session this registry does not hold is dropped, the
            // same way a command for one is: the session is gone and there is
            // nothing to wake.
            RingWait::Ring(Ring::Woken(id)) => {
                if self.get(&id).is_some() {
                    return WorkOrIdle::Work(Work::Woken(id));
                }
            }
            RingWait::Ring(Ring::Command(id)) => {
                let Some(hub) = self.get(&id) else { continue };
                if let Some(cmd) = hub.try_command() {
                    self.set_default(&id);
                    return WorkOrIdle::Work(Work::Command(id, cmd));
                }
            }
        }
    }
}
```

Three answers rather than the obvious two, because `None` already meant something: the registry
closed, so **stop the daemon**. A timeout that returned `None` would kill the process instead of
waking it.

## Documentation

`docs/` holds the design documents the code argues against — `design-brief.md` for the thesis,
`compaction.md` and `closed-loop.md` for the two subsystems with the most measurement behind them,
and `boundary-and-adjudication.md` for the gate. `agents.md` states the conventions a contribution
is held to.

## Licence

Apache-2.0. `crates/ui` contains material ported from upstream projects; see `NOTICE` for the
attribution and for the files that carry modification notices.
