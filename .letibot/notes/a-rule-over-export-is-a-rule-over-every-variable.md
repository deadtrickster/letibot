<!-- abstract: The card's *Always allow* over a build command derived `export*`, which admits `export LD_PRELOAD=…; git status` whole and for ever; `permission::SHELL_STATE` makes the derivation refuse a segment that changes what a later one means. -->

# A durable rule over `export*` is a rule about every variable a later program trusts

`permission::the_rule_that_admits` writes one pattern for the segment still asking. For `cd <dir> && export CARGO_TARGET_DIR=$PWD/target && cargo test …` that segment is the export, and the derivation gave `export*` - which is not a rule about `export` but about every export of anything. `bash_segments` splits a compound command and the whole is allowed when every segment is, and the seed allows `git status*`, so `export LD_PRELOAD=/tmp/evil.so; git status` was admitted whole, permanently, from one *Always allow* over a build command.

`permission::SHELL_STATE` and `sets_state_for_later` make the derivation return `None` for a segment that changes what a LATER segment means. `cd` stays skipped rather than refused, because it moves the shell and changes nothing a later segment reads.

The session rung the card offered could never fire: `Grant::covers` checks resolution before coverage, so a grant recorded over an unresolved command covers nothing, and the operator took it and was asked again for ever. The rung is offered only when `Grant::covers` answers `Ok` for the very grant the answer would record (`AdjudicatedGate::session_grant`).
