#!/bin/sh
# Format-check the Rust files a change touches, and nothing else.
#
#     sh scripts/check-fmt.sh <base-rev>      e.g. origin/main, or a push's `before`
#
# The tree is not rustfmt-clean, so `cargo fmt --all -- --check` would fail for reasons
# unrelated to the change. An unresolvable base fails rather than passes.
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

# Shell scripts are checked whole, not by touched file: every tracked one is shfmt-clean and the
# check is cheap. One pinned version through mise, so every host formats alike; a missing one fails,
# because a skipped check reads as a pass.
SHFMT_VERSION=3.14.1
if command -v mise >/dev/null 2>&1; then
  shfmt() { mise exec "shfmt@$SHFMT_VERSION" -- shfmt "$@"; }
elif ! command -v shfmt >/dev/null 2>&1; then
  echo "check-fmt: neither mise nor shfmt found; install shfmt $SHFMT_VERSION" >&2
  exit 1
fi
if ! shfmt -i 2 -ci -d scripts/*.sh scripts/letibot scripts/leticode scripts/rrun scripts/etalon-snapshot; then
  echo "check-fmt: shell scripts are not shfmt-clean; run: shfmt -i 2 -ci -w scripts/*.sh scripts/letibot scripts/leticode scripts/rrun scripts/etalon-snapshot" >&2
  exit 1
fi
echo "check-fmt: shell scripts formatted"

# ACMR, never Deleted: rustfmt cannot read a file that is gone.
# Committed changes plus the working tree (`git status --porcelain`), so a local run before a
# commit sees new files too. A path listed twice does no harm.
files=$({
  git diff --name-only --diff-filter=ACMR "$base" HEAD -- '*.rs'
  git status --porcelain -- '*.rs' | sed 's/^...//'
} | sort -u)

if [ -z "$files" ]; then
  echo "check-fmt: this change touches no Rust files"
  exit 0
fi

echo "check-fmt: checking $(printf '%s\n' "$files" | wc -l) Rust file(s) this change touches"

# Absolute, because rustfmt reports absolute paths and `git diff` relative ones. A mixed
# comparison would match nothing and pass everything.
repo_root=$(git rev-parse --show-toplevel)
ours=""
for f in $files; do
  ours="$ours $repo_root/$f"
done

# `--edition 2024` is the workspace's: rustfmt parses by the edition it is told.
# rustfmt follows `mod` declarations and `--skip-children` is not on stable, so its output is
# judged instead of its exit code: only diffs in touched files are fatal, others are a note.
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
  # Only diffs in touched files are printed; the note above names the others.
  printf '%s\n' "$report" | awk -v ours="$ours" '
        /^Diff in / {
            file = $0
            sub(/^Diff in /, "", file)
            sub(/:[0-9]+:$/, "", file)
            show = (" " ours " " ~ (" " file " "))
        }
        show { print }
    ' >&2
  exit 1
fi

echo "check-fmt: all $(printf '%s\n' "$files" | wc -l) file(s) this change touches are formatted"
