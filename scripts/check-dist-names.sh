#!/bin/sh
# Every triple the release workflow builds must be one install.sh asks for.
#
# The failure this prevents is silent and expensive-ish: publishing
# `letibot-aarch64-unknown-linux-musl.tar.gz` when the installer downloads
# `letibot-aarch64-unknown-linux-gnu.tar.gz` means the asset exists, the download
# 404s, and every install falls back to a source build. Nothing reports it — the
# workflow is green and the release looks right.
#
# The reverse is reported but not fatal: a triple the installer knows and we do not
# ship degrades to the source build, which is what install.sh is designed to do.
# That is a gap worth seeing, not an error.
#
# `rano`'s script is the template and this is a port of it. What is different here
# is the CONTENTS check: `rano` ships one binary and this ships three plus three
# libraries, so the names it asks for are read out of `make-dist.sh` rather than
# restated — a package missing `letibot-askpass` is a session that cannot answer a
# password prompt, and nothing else would notice.

set -eu

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

# What install.sh derives from `uname`. Read from its `triple=<name>` assignments
# rather than by pattern-matching what a triple looks like: the first version of
# this script (in rano) required `-unknown-` in the middle, so it silently ignored
# both `*-apple-darwin` targets and reported the real workflow as building assets
# the installer never asks for. The assignment is the contract; the shape is not.
known=$(grep -oE 'triple=[A-Za-z0-9_.-]+' "$repo/install.sh" | sed 's/^triple=//' | sort -u)
if [ -z "$known" ]; then
    echo "check-dist-names: found no triples in install.sh — did the layout change?" >&2
    exit 1
fi

# What the workflows build. The matrix lists them literally, as `triple: <name>`,
# so this does not have to understand YAML.
built=$(grep -h 'triple:' "$repo"/.github/workflows/*.yml 2>/dev/null |
    sed 's/.*triple:[[:space:]]*//' |
    grep -v '^$' |
    grep -v '^[$]' |
    sort -u)
if [ -z "$built" ]; then
    echo "check-dist-names: no triples found in the workflows" >&2
    exit 1
fi

bad=""
for t in $built; do
    case " $(printf '%s ' $known) " in
        *" $t "*) ;;
        *) bad="$bad $t" ;;
    esac
done
if [ -n "$bad" ]; then
    echo "check-dist-names: the workflow builds triples install.sh never asks for:$bad" >&2
    echo "  Every such asset is published and downloaded by nobody." >&2
    exit 1
fi

# A triple install.sh knows and we do not ship: reported, not fatal.
gap=""
for t in $known; do
    case " $(printf '%s ' $built) " in
        *" $t "*) ;;
        *) gap="$gap $t" ;;
    esac
done
if [ -n "$gap" ]; then
    echo "check-dist-names: install.sh knows triples we do not publish:$gap" >&2
    echo "  Those installs fall back to a source build, which is by design." >&2
fi

# **THE INSTALLER AND THE ARCHIVE MUST NAME EXACTLY THE SAME FILES.**
#
# This is the contract that nothing was holding, and the skew it leaves is silent on
# our side and total on the user's. The install script is served from `main`:
#
#     curl -fsSL https://raw.githubusercontent.com/.../main/install.sh | sh
#
# while the archive comes from `releases/latest`:
#
#     https://github.com/$REPO/releases/latest/download/$name
#
# **Two different refs.** MEASURED 2026-10-01: `releases/latest` was `v0.1.1`, SIX
# commits behind `main`. It happened to work because the only thing that changed in
# those six commits was `install.sh` itself — the moment it names a file the
# published archive does not hold, every person running the one-liner gets a script
# that wants a binary `releases/latest` has never contained, instantly and with no
# cache in between. Not a regression CI would find: the release that breaks is one
# already published and never rebuilt.
#
# So the two lists are read from their own files and compared as SETS, both ways:
#
#   install.sh    BINARIES + LIBRARIES   what the installer copies
#   make-dist.sh  ARCHIVE                what the packager builds
#
# Read from the files rather than restated here, because a checker that carries its
# own copy of the list is a third place to drift — which is what this replaces.
install_files=""
for var in BINARIES LIBRARIES; do
    v=$(sed -n "s/^$var=\"\(.*\)\"$/\1/p" "$repo/install.sh" | head -1)
    [ -n "$v" ] || { echo "check-dist-names: install.sh has no $var line." >&2; exit 1; }
    install_files="$install_files $v"
done
packaged=$(sed -n 's/^ARCHIVE_BINARIES="\(.*\)"$/\1/p;s/^ARCHIVE_LIBRARIES="\(.*\)"$/\1/p' \
    "$repo/scripts/make-dist.sh")
[ -n "$packaged" ] || { echo "check-dist-names: make-dist.sh has no ARCHIVE_* declaration." >&2; exit 1; }

missing_from_archive=""   # install.sh wants it; the packager does not make it
missing_from_installer="" # the packager makes it; install.sh does not copy it
for f in $install_files; do
    case " $(printf '%s ' $packaged) " in
        *" $f "*) ;;
        *) missing_from_archive="$missing_from_archive $f" ;;
    esac
done
for f in $packaged; do
    case " $(printf '%s ' $install_files) " in
        *" $f "*) ;;
        *) missing_from_installer="$missing_from_installer $f" ;;
    esac
done
if [ -n "$missing_from_archive" ]; then
    echo "check-dist-names: install.sh copies files the packager does not build:$missing_from_archive" >&2
    echo "  Those installs fail at copy time, or worse: the one-liner is served from" >&2
    echo "  main while the asset comes from releases/latest, so a user gets the" >&2
    echo "  mismatch with no tag between them. Add them to ARCHIVE_* in make-dist.sh" >&2
    echo "  (and to the release's asset if the archive is already published)." >&2
    exit 1
fi
if [ -n "$missing_from_installer" ]; then
    echo "check-dist-names: the packager builds files install.sh never copies:$missing_from_installer" >&2
    echo "  They ship in the archive and are not installed, which is a size cost and," >&2
    echo "  for a binary, a file nothing runs." >&2
    exit 1
fi

# **The macOS list, held to the same rule.** install.sh's LIBRARIES_DARWIN is what a Mac
# copies and make-dist.sh's ARCHIVE_LIBRARIES_DARWIN is what an `*-apple-darwin` archive
# holds; the binaries are shared with the Linux lists above.
inst_darwin=$(sed -n 's/^LIBRARIES_DARWIN="\(.*\)"$/\1/p' "$repo/install.sh" | head -1)
pack_darwin=$(sed -n 's/^ARCHIVE_LIBRARIES_DARWIN="\(.*\)"$/\1/p' "$repo/scripts/make-dist.sh" | head -1)
if [ -z "$inst_darwin" ] || [ -z "$pack_darwin" ]; then
    echo "check-dist-names: install.sh's LIBRARIES_DARWIN or make-dist.sh's" >&2
    echo "  ARCHIVE_LIBRARIES_DARWIN is missing, so the macOS asset has no list to agree with." >&2
    exit 1
fi
if [ "$(printf '%s\n' $inst_darwin | sort)" != "$(printf '%s\n' $pack_darwin | sort)" ]; then
    echo "check-dist-names: the macOS library lists disagree:" >&2
    echo "  install.sh  LIBRARIES_DARWIN:         $inst_darwin" >&2
    echo "  make-dist   ARCHIVE_LIBRARIES_DARWIN: $pack_darwin" >&2
    exit 1
fi

echo "check-dist-names: $(printf '%s ' $built | wc -w) triple(s) built, all asked for by install.sh; install.sh and the archive agree on $(printf '%s ' $packaged | wc -w) file(s)"

# **The two workflows must build against the SAME llama.cpp.**
#
# They each pin it separately, because each is a separate job on a separate runner —
# and a pin that drifts means CI goes green against one library while the release
# publishes binaries linked against another. Nothing else would notice: both runs are
# green, the assets are built, and the difference is a tokenizer.
#
# Read as `LLAMA_REF:` values, so this does not have to understand YAML.
pins=$(grep -h '^[[:space:]]*LLAMA_REF:' "$repo"/.github/workflows/*.yml 2>/dev/null |
    sed 's/.*LLAMA_REF:[[:space:]]*//' |
    sed 's/[[:space:]]*$//' |
    grep -v '^$' |
    sort -u)
n=$(printf '%s\n' "$pins" | grep -c . || true)
if [ "$n" -eq 0 ]; then
    echo "check-dist-names: no LLAMA_REF pin found in the workflows." >&2
    echo "  The tree does not compile without a llama.cpp checkout, so every job" >&2
    echo "  must name one. Silence here means a job is building against whatever" >&2
    echo "  its clone happened to default to." >&2
    exit 1
fi
if [ "$n" -gt 1 ]; then
    echo "check-dist-names: the workflows pin different llama.cpp revisions:" >&2
    printf '  %s\n' $pins >&2
    echo "  CI would test a different library from the one the release ships." >&2
    exit 1
fi
echo "check-dist-names: both workflows pin llama.cpp $pins"

