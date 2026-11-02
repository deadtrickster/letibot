<!-- abstract: `plan_cut` is `Hopeless` when the prompt leaves less than 24 576 of the window, and the prompt is a fact about `$HOME`, so a fixture must measure its window through the daemon's door. -->

# A fixture's window is the box's prompt, and the prompt is the HOME's

`letibot_turn::compaction::plan_cut` returns `Hopeless` when `prefix_tokens + WRITE_ROOM + CUT_MARGIN
>= window` (16 384 + 8 192 = 24 576), and `Hopeless` comes back as a `Setup` error the caller never
sees as a fold. `Config::compose_for_seat` puts the standing notes (AGENTS.md, the seat's instruction
files, `<config>/notes/*.md`, `<workspace>/.letibot/notes/`) INTO the system prompt, so the prefix is a
fact about `$HOME`: the same binary and vocabulary measured 6 717 tokens with an empty home and about
8 209 with the gate's real home - over 32 768 with the margin, which is one box passing and the other
failing.

So measure the window. Do it through the daemon's door (`Sessions::open_first` then
`harness_of(id).prefix_tokens()`), because a bare `Harness::open` is 217 tokens smaller. Then derive: a
fold needs `prompt + 24 576 + cfg.headroom()`, and a wall by size needs a band above `prompt + 24 576`
and below `prompt + history`.

Rakes: a seeded stuck pair must be the window's, because `restore_auto_compact` refuses a pair whose
`measured > window`; and `cfg.ledger_scale` converts the window by its ratio, so a scripted count five
times the ledger goes `Hopeless` again.
