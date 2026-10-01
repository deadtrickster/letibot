#!/bin/sh
# Format-check the Rust files a change TOUCHES, and nothing else.
#
#     sh scripts/check-fmt.sh <base-rev>      e.g. origin/main, or a push's `before`
#
# # Why not `cargo fmt --all -- --check`
#
# MEASURED on 2026-10-01, and it is the whole reason this script exists: the tree is
# not rustfmt-clean and never has been. `cargo fmt --all -- --check` reports **26 files
# and 154 hunks**, none of them related to any one change — `crates/tui/src/app.rs`
# alone accounts for 94.
#
# So the whole-tree check cannot be the gate yet, for two separate reasons:
#
#   * It fails on arrival for somebody else's reason. A CI job that is red the moment
#     it is added, before anyone has changed anything, is a job that gets deleted — and
#     the person who deletes it takes the clippy step with it.
#   * Reformatting 26 files inside the push that adds the release machinery would bury
#     that machinery under 154 hunks of unrelated whitespace, which is the opposite of
#     what a reviewer needs.
#
# Holding the files a change touches is a real gate — new code cannot arrive
# unformatted — and it passes today. Formatting the rest is a deliberate act, in its
# own commit, and when it lands `cargo fmt --all -- --check` should replace this call;
# the workflow step says so where it invokes this.
#
# # The base
#
# `git diff` against the base the change is measured from: for a pull request that is
# the merge base with the target branch, and for a push it is the ref's previous tip.
# If the revision cannot be resolved this FAILS rather than passing, because a check
# that cannot find its own input is not a check — and a silent pass here would be
# indistinguishable from a formatted tree, which is the exact confusion this repository
# keeps closing elsewhere.
set -eu

base=${1:-}
if [ -z "$base" ]; then
    echo "usage: check-fmt.sh <base-rev>" >&2
    exit 2
fi

if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null 2>&1; then
    echo "check-fmt: cannot resolve $base as a commit." >&2
    echo "  Refusing to pass: a format check with no base has not run, and reporting" >&2
    echo "  success for a check that did not run is the failure mode this repository" >&2
    echo "  keeps paying for. Pass the revision the change is measured from." >&2
    exit 1
fi

# Added, Copied, Modified or Renamed, never Deleted — rustfmt cannot read a file that
# is gone, and `--diff-filter` is what makes a deletion not a crash.
files=$(git diff --name-only --diff-filter=ACMR "$base" HEAD -- '*.rs' | sort)

if [ -z "$files" ]; then
    echo "check-fmt: this change touches no Rust files"
    exit 0
fi

echo "check-fmt: checking $(printf '%s\n' "$files" | wc -l) Rust file(s) this change touches"
# `--edition 2024` is the workspace's, and it is stated here rather than inferred:
# rustfmt parses a file according to the edition it is TOLD, and edition 2024 parses
# some code (raw identifiers, `gen` blocks) that 2021 does not.
# shellcheck disable=SC2086 # the list is newline-separated paths, and they must split
rustfmt --check --edition 2024 $files
echo "check-fmt: all formatted"
