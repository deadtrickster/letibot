#!/usr/bin/env bash
# Copy a project's in-tree notes (.letibot/notes/*.md, on disk and committed) into the notes
# checkout under the project's key, and commit them there. The project tree is not changed.
#
#   scripts/migrate-notes.sh PROJECT [--repo DIR] [--remote URL] [--push]
#
#   --repo DIR    the notes checkout (default: $XDG_DATA_HOME/letibot/notes or
#                 ~/.local/share/letibot/notes, as the daemon's default)
#   --remote URL  clone it from URL when DIR does not exist yet, else add it as origin
#   --push        push the checkout to origin when done (otherwise: letibot notes push)
set -euo pipefail

die() {
  echo "migrate-notes: $*" >&2
  exit 1
}

project=""
repo="${XDG_DATA_HOME:-$HOME/.local/share}/letibot/notes"
remote=""
push=0
while [ $# -gt 0 ]; do
  case "$1" in
    --repo)
      [ $# -ge 2 ] || die "--repo needs a directory"
      repo="$2"
      shift 2
      ;;
    --remote)
      [ $# -ge 2 ] || die "--remote needs a URL"
      remote="$2"
      shift 2
      ;;
    --push)
      push=1
      shift
      ;;
    -h | --help)
      sed -n '2,10p' "$0"
      exit 0
      ;;
    -*) die "unknown flag: $1" ;;
    *)
      [ -z "$project" ] || die "one project at a time"
      project="$1"
      shift
      ;;
  esac
done
[ -n "$project" ] || die "usage: migrate-notes.sh PROJECT [--repo DIR] [--remote URL] [--push]"
project="$(cd -P -- "$project" && pwd)" || die "no such directory"

sha12() {
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$1" | sha256sum | cut -c1-12
  else
    printf '%s' "$1" | shasum -a 256 | cut -c1-12
  fi
}

# The same key as `letibot_tools::notes_repo::project_key`: the normalized origin URL when there
# is one, else the main checkout's real path.
normalize_remote() {
  local s="$1" host rest
  case "$s" in
    *://*) s="${s#*://}" ;;
    *:*)
      host="${s%%:*}"
      rest="${s#*:}"
      case "$host" in */*) ;; *) s="$host/$rest" ;; esac
      ;;
  esac
  case "$s" in
    *@*)
      host="${s%%@*}"
      case "$host" in */*) ;; *) s="${s#*@}" ;; esac
      ;;
  esac
  while [ "${s%/}" != "$s" ]; do s="${s%/}"; done
  s="${s%.git}"
  printf '%s' "$s" | tr '[:upper:]' '[:lower:]'
}

safe_name() {
  printf '%s' "$1" | LC_ALL=C sed 's/[^A-Za-z0-9._-]/_/g; s/^\.*//'
}

project_key() {
  local url norm common checkout
  url="$(git -C "$project" remote get-url origin 2>/dev/null || true)"
  if [ -n "$url" ]; then
    norm="$(normalize_remote "$url")"
    if [ -n "$norm" ]; then
      printf '%s-%s' "$(safe_name "${norm##*/}")" "$(sha12 "$norm")"
      return
    fi
  fi
  common="$(git -C "$project" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
  if [ -n "$common" ]; then
    case "$common" in
      */.git) checkout="${common%/.git}" ;;
      *) checkout="$common" ;;
    esac
  else
    checkout="$project"
  fi
  checkout="$(cd -P -- "$checkout" && pwd)"
  printf '%s-%s' "$(safe_name "${checkout##*/}")" "$(sha12 "$checkout")"
}

# A box with no git identity still commits, as letibot - the daemon's rule.
if [ -z "$(git config user.email 2>/dev/null || true)" ]; then
  export GIT_AUTHOR_NAME=letibot GIT_AUTHOR_EMAIL=letibot@localhost
  export GIT_COMMITTER_NAME=letibot GIT_COMMITTER_EMAIL=letibot@localhost
fi

if [ ! -d "$repo/.git" ]; then
  if [ -n "$remote" ]; then
    git clone -q "$remote" "$repo"
  else
    mkdir -p "$repo"
    git -C "$repo" init -q -b main
  fi
fi
if [ -n "$remote" ] && ! git -C "$repo" remote get-url origin >/dev/null 2>&1; then
  git -C "$repo" remote add origin "$remote"
fi
# A clone of an empty remote has no branch yet; the first commit must land on main.
if ! git -C "$repo" rev-parse -q --verify HEAD >/dev/null; then
  git -C "$repo" symbolic-ref HEAD refs/heads/main
fi

key="$(project_key)"
dest="$repo/$key"
mkdir -p "$dest"
src="$project/.letibot/notes"

copied=0
conflicts=()
place() {
  local name="$1" from="$2"
  if [ -e "$dest/$name" ]; then
    if cmp -s "$from" "$dest/$name"; then
      return
    fi
    conflicts+=("$name")
    return
  fi
  cp "$from" "$dest/$name"
  copied=$((copied + 1))
}

if [ -d "$src" ]; then
  for f in "$src"/*.md; do
    [ -e "$f" ] || continue
    place "${f##*/}" "$f"
  done
fi
# Committed and gone from disk: the text at HEAD, or, once a commit removed the directory, the
# text just before that commit. Either way the committed text is still a note.
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
rev=""
if git -C "$project" rev-parse -q --verify HEAD >/dev/null 2>&1; then
  if [ -n "$(git -C "$project" ls-tree --name-only HEAD -- .letibot/notes)" ]; then
    rev=HEAD
  else
    last="$(git -C "$project" rev-list -1 HEAD -- .letibot/notes)"
    if [ -n "$last" ] && git -C "$project" rev-parse -q --verify "$last^" >/dev/null; then
      rev="$last^"
    fi
  fi
fi
if [ -n "$rev" ]; then
  while IFS= read -r path; do
    case "$path" in */*/*/*) continue ;; *.md) ;; *) continue ;; esac
    name="${path##*/}"
    [ -e "$src/$name" ] && continue
    git -C "$project" show "$rev:$path" >"$tmp"
    place "$name" "$tmp"
  done < <(git -C "$project" ls-tree -r --name-only "$rev" -- .letibot/notes)
fi

git -C "$repo" add -A -- "$key"
if git -C "$repo" diff --cached --quiet; then
  echo "nothing new for $key in $repo"
else
  git -C "$repo" commit -q -m "migrate notes from ${project##*/}"
  echo "committed $copied note(s) into $dest"
fi
if [ "${#conflicts[@]}" -gt 0 ]; then
  echo "not copied, a different note of that name is already there:" >&2
  printf '  %s\n' "${conflicts[@]}" >&2
fi

if [ "$push" = 1 ]; then
  git -C "$repo" remote get-url origin >/dev/null 2>&1 || die "no origin to push to; pass --remote"
  git -C "$repo" fetch -q origin
  if git -C "$repo" rev-parse -q --verify origin/main >/dev/null; then
    git -C "$repo" rebase -q origin/main || {
      git -C "$repo" rebase --abort
      die "rebase onto origin/main failed, nothing pushed"
    }
  fi
  git -C "$repo" push -q origin HEAD:refs/heads/main
  echo "pushed $repo to origin main"
fi
[ "${#conflicts[@]}" -eq 0 ]
