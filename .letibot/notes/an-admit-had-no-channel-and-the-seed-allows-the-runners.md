<!-- abstract: A gate admit had no operator-facing channel (only `DenialSink`), so the seam is an `AdmissionSink` drained at a round boundary; and the shipped `config/permission.json` allows exactly the runners `ask_unconfined_code` would make ask. -->

# An admit had no channel, and the shipped seed allows exactly the runners

`AdjudicatedGate` has had a `DenialSink` since denials were made visible, and an admit had only the
audit row in `self.log`, which `/gate` and the corpus read and nothing that scrolls. A mode that stops
asking had no way to say so.

The seam is `AdmissionSink`/`AdmissionNotice` on the gate, `HubAdmissions` in harnessd, and
`Harness::flush_gate_admissions` writing the rows. Two shape facts: the gate runs inside the runtime,
where `&mut Harness` is not in hand, so a sentence is handed over and spoken later; and it is drained
at the round boundary, after `append_items(&results)` and never between a tool call and its result,
because `pair_tool_calls` reads the first non-`tool` row as the end of an answering window.

`config/permission.json`'s `bash` block ships `cargo test*`, `cargo build*`, `pytest*`, `go test*`,
`make test*` and their kin - exactly the runners `ask_unconfined_code` would make ask on an
unconfined host. That rule is `[permissions] ask_unconfined_code` in a `leticode.toml` and is OFF by
default, because asking on every build pushes an operator to allow-all.
