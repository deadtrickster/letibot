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

# **The contents of the archive are the other half of the contract**, and they are
# read from `make-dist.sh` so the two cannot drift: it names what it puts in, and
# this asserts the installer expects exactly that.
made=$(grep -oE '\b(harnessd|letibot-tui|letibot-askpass|libllama\.so\.0|libggml\.so\.0|libggml-base\.so\.0)\b' \
    "$repo/scripts/make-dist.sh" | sort -u)
for want in harnessd letibot-tui letibot-askpass libllama.so.0 libggml.so.0 libggml-base.so.0; do
    case " $(printf '%s ' $made) " in
        *" $want "*) ;;
        *) echo "check-dist-names: make-dist.sh never names $want" >&2; exit 1 ;;
    esac
done

echo "check-dist-names: $(printf '%s ' $built | wc -w) triple(s) built, all asked for by install.sh; $(printf '%s ' $made | wc -w) file(s) in the package"

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

