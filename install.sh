#!/bin/sh
# Installs letibot.
#
#   curl -fsSL https://raw.githubusercontent.com/deadtrickster/letibot/main/install.sh | sh
#
# # WHAT THIS PUTS ON YOUR MACHINE, said before it does it
#
#   $PREFIX/harnessd           the daemon — the session, the gate, the model connection
#   $PREFIX/letibot-tui        the head — the terminal interface you look at
#   $PREFIX/letibot-askpass    what `sudo` runs inside a session to ask for a password
#   $PREFIX/libllama.so.0      the tokenizer, which the daemon links
#   $PREFIX/libggml.so.0       llama.cpp's compute layer, which libllama links
#   $PREFIX/libggml-cpu.so.0   the CPU backend, which libggml links
#   $PREFIX/libggml-base.so.0  and its base
#
# `$PREFIX` is `LETIBOT_INSTALL_DIR`, defaulting to `~/.local/bin`.
#
# **FOUR libraries, and the fourth is easy to miss**: `libllama.so.0` needs
# `libggml.so.0`, and `libggml.so.0` needs `libggml-cpu.so.0`. A package without
# that last one starts, gets as far as libggml, and dies with
# `libggml-cpu.so.0: cannot open shared object file`.
#
# **The libraries go in the same directory as the binaries, and they are not
# clutter.** `harnessd` links `libllama.so.0` for the tokenizer, and its build
# script bakes `$ORIGIN` into the runpath — so the daemon finds them beside itself
# and runs on a machine that has no llama.cpp at all. Splitting them into
# `~/.local/lib` would need a second mechanism to find them, and this is the one
# that was measured to work.
#
# # WHAT IT DOES NOT INSTALL
#
# **A model server.** letibot is a harness: it talks to an OpenAI-compatible
# endpoint, a llama.cpp `llama-server` by default on `127.0.0.1:8080`, or a cloud
# provider. Both are yours to provide, and this script says so at the end rather
# than leaving you to find out from a connection error.
#
# # HOW IT GETS THE BINARIES
#
#   1. the release asset for this platform — no toolchain needed
#   2. a build from a checkout, if this script is being run from one
#   3. a clone of the default branch, built from source
#
# **(2) and (3) need MORE than a Rust toolchain.** `harnessd` links llama.cpp, so
# the source path needs `cargo`, a C compiler, and a **built llama.cpp checkout** —
# `LETIBOT_LLAMA_DIR` (holding `include/llama.h`) and `LETIBOT_LLAMA_LIB` (holding
# `libllama.so.0`). That is a real prerequisite and it is named here, checked
# before the build starts, and reported in this script's own words rather than as a
# `llama.h not found` from inside cargo. The prebuilt path needs none of it.
#
# Environment:
#   LETIBOT_INSTALL_DIR   where everything goes   (default: ~/.local/bin)
#   LETIBOT_VERSION       tag or branch           (default: the default branch)
#   LETIBOT_FROM_SOURCE   set to anything to skip the download and build instead
#   LETIBOT_LLAMA_DIR     a llama.cpp checkout    (source builds: required)
#   LETIBOT_LLAMA_LIB     its lib directory       (source builds: required)

set -eu

REPO="deadtrickster/letibot"

INSTALL_DIR="${LETIBOT_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${LETIBOT_VERSION:-}"

# The three binaries and the FOUR libraries, as `scripts/make-dist.sh` packages
# them. `scripts/check-dist-names.sh` holds this list, the workflow and the
# packaging script to each other, so a rename in one place fails in CI.
BINARIES="harnessd letibot-tui letibot-askpass"
LIBRARIES="libllama.so.0 libggml.so.0 libggml-cpu.so.0 libggml-base.so.0"

say() { printf '%s\n' "$*"; }
# Everything that is not the binary path goes to stderr, so the functions below can
# be used in a command substitution without their chatter becoming the answer.
warn() { printf '%s\n' "$*" >&2; }
die() { printf 'letibot: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1; }

# A checkout to build from, or nothing: the directory holding this script, if it
# holds letibot's own Cargo.toml. The `name = "letibot"`-shaped test is what keeps
# a stray `Cargo.toml` in the current directory from being mistaken for the
# project — when this is piped into `sh`, `$0` is `sh` and the directory is the
# caller's.
local_checkout() {
    dir=$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd) || return 1
    if [ -f "$dir/Cargo.toml" ] && grep -q '^\[workspace\]' "$dir/Cargo.toml" 2>/dev/null &&
        grep -q '"crates/harnessd"' "$dir/Cargo.toml" 2>/dev/null; then
        printf '%s' "$dir"
    else
        return 1
    fi
}

# The prebuilt release asset for this platform, extracted, or nothing.
#
# Returning non-zero is a normal outcome, not an error: a platform with no
# published asset falls through to the source build.
try_prebuilt() {
    tmp="$1"
    case "$(uname -s)/$(uname -m)" in
        Linux/x86_64)                 triple=x86_64-unknown-linux-gnu ;;
        Linux/aarch64 | Linux/arm64)  triple=aarch64-unknown-linux-gnu ;;
        *) return 1 ;;
    esac
    name="letibot-$triple.tar.gz"
    if [ -n "$VERSION" ]; then
        url="https://github.com/$REPO/releases/download/$VERSION/$name"
    else
        url="https://github.com/$REPO/releases/latest/download/$name"
    fi
    need curl || return 1
    need tar || return 1
    # `-q` FIRST, and it is not cosmetic: curl reads ~/.curlrc, so a single
    # `insecure` line there turns certificate verification off for every curl the
    # user runs — measured (in rano, whose installer this is ported from): exit 0
    # with the curlrc, exit 60 without it. Downloading binaries that are then
    # executed is not a place to inherit somebody's debugging shortcuts.
    # `--proto`/`--proto-redir` keep both hops on HTTPS.
    curl -q -fsSL --proto '=https' --proto-redir '=https' -o "$tmp/$name" "$url" 2>/dev/null || return 1
    mkdir -p "$tmp/unpacked" || return 1
    tar -xzf "$tmp/$name" -C "$tmp/unpacked" 2>/dev/null || return 1
    [ -x "$tmp/unpacked/harnessd" ] || return 1
    printf '%s' "$tmp/unpacked"
}

# Build a checkout and print the directory holding what was built, or fail naming
# what is missing.
build_from_source() {
    src="$1"
    need cargo || die "no cargo found: install Rust from https://rustup.rs and re-run"
    # The C shim and the tree-sitter grammars are C.
    if ! need cc && ! need gcc && ! need clang; then
        die "no C compiler found (cc, gcc or clang): the tokenizer shim and the tree-sitter grammars are compiled at build time"
    fi
    # **The prerequisite nobody expects, checked here so it is not a cargo error.**
    llama_dir="${LETIBOT_LLAMA_DIR:-}"
    llama_lib="${LETIBOT_LLAMA_LIB:-}"
    if [ -z "$llama_dir" ] || [ -z "$llama_lib" ]; then
        die "harnessd links llama.cpp, so a source build needs a built llama.cpp.
  Set both, pointing at YOUR checkout and its built libraries:
    LETIBOT_LLAMA_DIR=/path/to/llama.cpp          (holding include/llama.h)
    LETIBOT_LLAMA_LIB=/path/to/llama.cpp/build/bin (holding libllama.so.0)
  Or use the prebuilt release, which carries the libraries it needs:
    unset LETIBOT_FROM_SOURCE and re-run without a checkout in \$0's directory."
    fi
    [ -f "$llama_dir/include/llama.h" ] ||
        die "no include/llama.h under LETIBOT_LLAMA_DIR=$llama_dir"
    [ -f "$llama_lib/libllama.so.0" ] ||
        die "no libllama.so.0 under LETIBOT_LLAMA_LIB=$llama_lib"

    warn "Building letibot from $src — this takes a few minutes..."
    cargo build --release --manifest-path "$src/Cargo.toml" \
        --bin harnessd --bin letibot-tui --bin letibot-askpass >&2
    out="$src/target/release"
    for want in $BINARIES; do
        [ -x "$out/$want" ] || die "the build produced no $out/$want"
    done
    printf '%s' "$out"
}

main() {
    say "letibot installs into: $INSTALL_DIR"
    say "  $BINARIES"
    say "  $LIBRARIES  (the tokenizer and llama.cpp's compute layer)"
    say ""

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT INT TERM

    # Where the built (or downloaded) files are.
    from=""
    # Where the four libraries are, when the source came from a build rather than
    # from an asset — a build leaves them in the llama.cpp tree, not beside the
    # binaries.
    libs_from=""

    if src=$(local_checkout); then
        from=$(build_from_source "$src")
        libs_from="${LETIBOT_LLAMA_LIB:-}"
    else
        if [ -z "${LETIBOT_FROM_SOURCE:-}" ]; then
            from=$(try_prebuilt "$tmp") || from=""
        fi
        if [ -z "$from" ]; then
            need git || die "no git found: needed to fetch the source (or run this from a checkout)"
            warn "Fetching $REPO..."
            if [ -n "$VERSION" ]; then
                git clone --depth 1 --branch "$VERSION" "https://github.com/$REPO.git" "$tmp/letibot" >&2
            else
                git clone --depth 1 "https://github.com/$REPO.git" "$tmp/letibot" >&2
            fi
            from=$(build_from_source "$tmp/letibot")
            libs_from="${LETIBOT_LLAMA_LIB:-}"
        fi
    fi

    mkdir -p "$INSTALL_DIR" || die "cannot create $INSTALL_DIR"
    for want in $BINARIES; do
        [ -f "$from/$want" ] || die "$from/$want is missing"
        # cp+chmod rather than install(1): `install` is in GNU and BSD but not in
        # POSIX, and the difference is not worth a portability question here.
        cp "$from/$want" "$INSTALL_DIR/$want" || die "cannot write $INSTALL_DIR/$want"
        chmod 755 "$INSTALL_DIR/$want"
    done
    for so in $LIBRARIES; do
        src_so="$from/$so"
        [ -f "$src_so" ] || src_so="${libs_from:-$from}/$so"
        [ -f "$src_so" ] || die "cannot find $so to install: harnessd links it and will not start without it"
        cp -L "$src_so" "$INSTALL_DIR/$so" || die "cannot write $INSTALL_DIR/$so"
        chmod 644 "$INSTALL_DIR/$so"
    done

    # **Say the version rather than assuming.** A binary that cannot run is a
    # failure this script would otherwise report as success — and this is also the
    # first real proof that the libraries landed, because a daemon whose
    # `libllama.so.0` is missing dies at exec with `cannot open shared object file`
    # before it can print anything.
    got=$("$INSTALL_DIR/harnessd" --version) ||
        die "installed $INSTALL_DIR/harnessd, but it does not run (its libraries are $LIBRARIES, beside it)"
    tui=$("$INSTALL_DIR/letibot-tui" --version) || die "installed letibot-tui, but it does not run"
    say ""
    say "$got"
    say "$tui"
    say "  ->  $INSTALL_DIR"

    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *)
            say ""
            say "That directory is not on your PATH. Add it with:"
            say "  export PATH=\"$INSTALL_DIR:\$PATH\""
            ;;
    esac

    say ""
    say "Next: you need a model server. letibot talks to an OpenAI-compatible"
    say "endpoint — a llama.cpp llama-server by default, on 127.0.0.1:8080:"
    say "  llama-server -m MODEL.gguf --port 8080"
    say "or a cloud provider instead (letibot --provider deepseek|glm|grok)."
    say "Then run:  letibot"
}

main "$@"
