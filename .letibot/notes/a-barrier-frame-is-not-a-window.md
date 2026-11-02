<!-- abstract: An absence assertion that collects frames up to a barrier frame can never fail, because the frame a defect would emit is the next one; the instrument for "and nothing else happened" is a window after the barrier. -->

# A barrier frame is not a window, and an absence assertion that stops at one has no teeth

An absence assertion that collects frames until a barrier frame measures nothing: `Link::tick` builds
one `actions` vector in tick order, so the frame a defect would emit is always the next one.

Prove it by mutation: push `Action::TermClose { pane }` back into `App::pane_keys`'s detach arm and
run `cargo test -p letibot-tui --test term_way_out`. With a collection that stops at the barrier, 4
pass and 3 fail, and the two tests about the absence are among the passes.

The instrument is `frames_until_and_after` - `frames_until` plus `arrived_meanwhile`, a 2 s window
after the barrier. A positive frame makes an absence exact only for what was written in front of it; a
non-blocking `try_recv` has the same hole from the other side.

And a probe's `None` is not a failure: `crates/bridge/tests/support/cpu.rs`'s `of_pid` answers `None`
where there is no `/proc`, so `unwrap_or_default()` there turns macOS into a 30 s wait and a red that
reads as a broken instrument. Split at the call site; `apparatus::present` panics under
`LETIBOT_REQUIRE_APPARATUS=1` for a facility macOS can never have.
