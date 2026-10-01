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

# **Absolute, because rustfmt reports absolute paths and `git diff` gives relative
# ones.** A comparison that mixed the two would silently match nothing, so every file
# rustfmt complained about would look like "not ours" and this check would pass
# everything — the can-only-pass shape, arrived at by a path-prefix mistake.
repo_root=$(git rev-parse --show-toplevel)
ours=""
for f in $files; do
    ours="$ours $repo_root/$f"
done

# `--edition 2024` is the workspace's, and it is stated rather than inferred: rustfmt
# parses a file according to the edition it is TOLD, and edition 2024 accepts code
# that 2021 does not.
#
# # Why this judges rustfmt's OUTPUT instead of its exit code
#
# **rustfmt follows `mod` declarations**, so handing it `lib.rs` also formats that
# crate's children — MEASURED: `rustfmt --check crates/dialect-qwen/src/lib.rs`
# reports a hunk in `src/render.rs`, a file it was never given. That made this script
# fail on PRE-EXISTING debt in files a change did not touch, which is exactly the
# failure mode it exists to avoid: CI went red on a commit whose own five files were
# all clean, because one of them was a `lib.rs`.
#
# `--skip-children` would fix it and is NOT available on stable (`Unrecognized
# option`), so the widening is filtered: only diffs naming a file this change touches
# are fatal. What rustfmt says about other files is a NOTE — visible, not fatal, the
# same posture the tree takes for the 26 files of pre-existing whole-tree debt.
# shellcheck disable=SC2086 # a space-separated path list that must split
report=$(rustfmt --check --edition 2024 $files 2>&1) && {
    echo "check-fmt: all formatted"
    exit 0
}

mine=""
others=""
for f in $(printf '%s\n' "$report" | sed -n 's/^Diff in \(.*\):[0-9]*:.*$/\1/p' | sort -u); do
    case " $ours " in
        *" $f "*) mine="$mine $f" ;;
        *) others="$others $f" ;;
    esac
done

if [ -z "$mine" ] && [ -z "$others" ]; then
    # rustfmt failed without naming any file: a parse error rather than a formatting
    # difference. Never ours to pass.
    echo "check-fmt: rustfmt failed and named no file:" >&2
    printf '%s\n' "$report" >&2
    exit 1
fi

if [ -n "$others" ]; then
    echo "check-fmt: note — rustfmt also wanted these, which this change does not touch." >&2
    echo "  It follows \`mod\` declarations, so one \`lib.rs\` pulls in its children:" >&2
    for f in $others; do echo "    ${f#"$repo_root"/}" >&2; done
fi

if [ -n "$mine" ]; then
    echo "check-fmt: these files this change touches are not formatted:" >&2
    for f in $mine; do echo "    ${f#"$repo_root"/}" >&2; done
    printf '%s\n' "$report" >&2
    exit 1
fi

echo "check-fmt: all $(printf '%s\n' "$files" | wc -l) file(s) this change touches are formatted"