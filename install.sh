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
# **`letibot` is last and it is the one the closing line names**, so an archive
# without it installs cleanly and then answers `command not found` to the command
# this script told the user to type. MEASURED by the operator on the real one-liner:
# seven files landed, both binaries printed their versions, and the launcher was not
# among them — while `scripts/letibot` had been tracked for hours.
#
# It ships in the archive with everything else (see `make-dist.sh`, which decides
# that) so the script and the binaries come from ONE ref.
BINARIES="harnessd letibot-tui letibot-askpass letibot"
LIBRARIES="libllama.so.0 libggml.so.0 libggml-cpu.so.0 libggml-base.so.0"
# **The same four on macOS, in Mach-O's spelling.** MEASURED 2026-10-07 with `otool -L` on
# a CPU-only llama.cpp build: libllama needs libggml, libggml-base and libggml-cpu, all
# `@rpath/` with an `@loader_path` rpath, which is `$ORIGIN` in dyld's words. A Metal or
# BLAS build adds libggml-metal and libggml-blas; the release builds CPU-only for the
# same reason Linux's does — the daemon tokenises and runs no model.
LIBRARIES_DARWIN="libllama.0.dylib libggml.0.dylib libggml-cpu.0.dylib libggml-base.0.dylib"
HOST_OS=$(uname -s)
if [ "$HOST_OS" = Darwin ]; then
    LIBRARIES=$LIBRARIES_DARWIN
fi

# **THE LIBRARIES THE BINARIES NEED *FROM THE HOST*, and why this list is three.**
#
# The archive carries the four llama libraries. Everything else they link has to
# come from the machine they land on, and MEASURED against the released asset, that
# set is:
#
#   libc.so.6  libm.so.6  libgcc_s.so.1  ld-linux-*.so.*    every glibc Linux
#   libstdc++.so.6    the C++ runtime    libllama, libggml, libggml-base, libggml-cpu
#   libgomp.so.1      OpenMP             libggml-base, libggml-cpu
#   libsqlite3.so.0   SQLite             harnessd
#
# The first line is on any glibc system by definition. **The last three are not**:
# they arrive with a toolchain, or with software that happens to need them, and a
# minimal container has none of them. Without this check the install reports success
# and then `harnessd` dies at exec with `cannot open shared object file` — after
# every file has been copied, and with nothing naming the library.
#
# **NAMED, NOT BUNDLED, and that is deliberate.** A shipped libstdc++ is a
# compatibility claim nobody has measured: it has to match the host's libc, and one
# older than the host's fails worse and more mysteriously than a missing one.
# libgomp and libsqlite3 are the same argument. So this names what is missing and
# how to get it — a line a person can paste.
HOST_LIBS="libstdc++.so.6 libgomp.so.1 libsqlite3.so.0"

say() { printf '%s\n' "$*"; }
# Everything that is not the binary path goes to stderr, so the functions below can
# be used in a command substitution without their chatter becoming the answer.
warn() { printf '%s\n' "$*" >&2; }
die() { printf 'letibot: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1; }

# Is this a glibc system at all?
#
# The shipped binaries name `libc.so.6` and `ld-linux-<arch>.so.2`. On musl the
# loader is `ld-musl-<arch>.so.1` instead — so no glibc loader means the prebuilt
# assets cannot run HERE whatever else is installed, which is a DIFFERENT sentence
# from "you are missing libgomp" and deserves its own.
have_glibc() {
    for d in /lib /lib64 /usr/lib /usr/lib64; do
        [ -d "$d" ] || continue
        for f in "$d"/ld-linux*.so.* "$d"/*/ld-linux*.so.*; do
            [ -e "$f" ] && return 0
        done
    done
    return 1
}

# Can THIS machine's loader find $1? 0 yes, 1 no.
#
# Deliberately no third "could not tell" that refuses: a check which fails when it
# cannot check would refuse boxes that are perfectly fine, which is worse than the
# mystery it replaces. The run at the end is the backstop, and it now names the
# library the loader actually wanted.
host_has_lib() {
    if need ldconfig; then
        # Matched in the shell rather than piped through `grep -q`, which is the
        # shape this tree already uses for this exact question (`socket_is_listening`
        # in `scripts/letibot`). `grep -q` exits on the first match and SIGPIPEs
        # whoever is writing, and `pipefail` then reports the writer's 141 as the
        # answer — measured there as a live daemon reported dead about one run in
        # ten. install.sh does not set pipefail today; the pattern costs nothing and
        # does not depend on that staying true.
        case "$(ldconfig -p 2>/dev/null)" in
            *"$1"*) return 0 ;;
            *) return 1 ;;
        esac
    fi
    # No ldconfig. Unusual on glibc, normal on musl — and musl is answered above.
    for d in /lib /lib64 /usr/lib /usr/lib64; do
        [ -d "$d" ] || continue
        [ -f "$d/$1" ] && return 0
        for m in "$d"/*/; do
            [ -f "$m$1" ] && return 0
        done
    done
    return 1
}

# **Refuse before anything is downloaded or copied.**
#
# This is the sentence install.sh already had one level down — "a daemon whose
# `libllama.so.0` is missing dies at exec with `cannot open shared object file`" —
# applied to the libraries that are NOT in the archive. The value is turning an
# exit-127 mystery into a line a person can paste.
require_host_runtime() {
    # **macOS needs nothing from this list.** Everything the binaries and the four
    # libraries link outside the archive is part of the OS — libc++, libsqlite3, libiconv,
    # libSystem, and the Accelerate/Metal/Foundation frameworks (`otool -L`, measured on
    # the same build as LIBRARIES_DARWIN) — so there is no glibc to find and no package
    # to name. What a Mac can lack is the Command Line Tools, and only the launcher feels
    # it: it writes its daemon record with python3, which on macOS arrives with them.
    if [ "$HOST_OS" = Darwin ]; then
        if ! xcode-select -p >/dev/null 2>&1; then
            warn "note: the Command Line Tools are not installed, and the \`letibot\` launcher
  uses python3, which on macOS comes with them. Install them with:
    xcode-select --install"
        fi
        return 0
    fi
    if ! have_glibc; then
        die "this machine has no glibc dynamic loader, and the published binaries are
  built for glibc — they cannot run here whatever is installed. (A musl system
  such as Alpine is the usual case.) Build from source instead:
      LETIBOT_FROM_SOURCE=1 sh install.sh
  which needs git, Rust, a C compiler and a built llama.cpp checkout.
  Nothing has been downloaded or copied."
    fi
    missing=""
    for lib in $HOST_LIBS; do
        host_has_lib "$lib" || missing="$missing $lib"
    done
    [ -n "$missing" ] || return 0
    die "this machine is missing libraries the binaries need at run time:$missing

  The install would otherwise succeed, and then harnessd would die at exec with
  'cannot open shared object file' — naming none of them. Install them:

    Debian/Ubuntu   apt-get install libstdc++6 libgomp1 libsqlite3-0
    Fedora/RHEL     dnf install libstdc++ libgomp sqlite-libs

  They are not bundled on purpose: a libstdc++ has to match the host's libc, and
  one older than the host's fails worse and more mysteriously than a missing one.
  Nothing has been downloaded or copied."
}

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
        # Apple silicon. An Intel Mac has no asset and builds from source.
        Darwin/arm64)                 triple=aarch64-apple-darwin ;;
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
    # The first of LIBRARIES is libllama in this platform's spelling.
    libllama=${LIBRARIES%% *}
    if [ -z "$llama_dir" ] || [ -z "$llama_lib" ]; then
        die "harnessd links llama.cpp, so a source build needs a built llama.cpp.
  Set both, pointing at YOUR checkout and its built libraries:
    LETIBOT_LLAMA_DIR=/path/to/llama.cpp          (holding include/llama.h)
    LETIBOT_LLAMA_LIB=/path/to/llama.cpp/build/bin (holding $libllama)
  Or use the prebuilt release, which carries the libraries it needs:
    unset LETIBOT_FROM_SOURCE and re-run without a checkout in \$0's directory."
    fi
    [ -f "$llama_dir/include/llama.h" ] ||
        die "no include/llama.h under LETIBOT_LLAMA_DIR=$llama_dir"
    [ -f "$llama_lib/$libllama" ] ||
        die "no $libllama under LETIBOT_LLAMA_LIB=$llama_lib"

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
    # **Before the banner, before the download, before a single file is copied.**
    # A box that cannot run the binaries should learn that in the first second,
    # not after 16 MB and six files it will have to remove.
    require_host_runtime

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
    # **Where `letibot` comes from, and it is NOT `$from` in a source install.**
    #
    # `cargo build --release` also produces `target/release/letibot` — the multicall
    # binary — so `$from/letibot` in a source install is the Rust binary rather than
    # the shell launcher. MEASURED on a real source install: the binary landed under
    # the launcher's name and `letibot --sessions` answered *"no such role:
    # --sessions"*, because the multicall cannot be the launcher yet.
    #
    # So the checkout's `scripts/` is searched first, and what is found there must be
    # a SCRIPT. That check is the point: an ELF at this name is the multicall, and
    # installing it silently replaces the command a person types with one that does
    # not answer. A file that exists and is the wrong THING is worse than a file that
    # is missing, because only the second one is noticed.
    launcher_from=""

    if src=$(local_checkout); then
        from=$(build_from_source "$src")
        libs_from="${LETIBOT_LLAMA_LIB:-}"
        launcher_from="$src/scripts"
    else
        if [ -z "${LETIBOT_FROM_SOURCE:-}" ]; then
            from=$(try_prebuilt "$tmp") || from=""
        fi
        if [ -z "$from" ]; then
            # **Name `curl` FIRST, because it is the absence that actually explains
            # this.** MEASURED on a minimal box (no curl, no wget, no git, no tar),
            # where the message used to be "no git found: needed to fetch the source".
            # That is true and misleading: the asset path returns quietly when curl
            # is missing, so by the time control reaches here the user is being told
            # to install git — the tool they cannot fetch either — while the one tool
            # whose absence turned a one-step install into a source build is unnamed.
            #
            # `wget` is not a substitute and this says so, because it is the first
            # thing a person on a minimal image tries.
            need curl || die "no curl found, and there is no prebuilt install without it.
  curl downloads the release asset; wget is not a substitute.
    Debian/Ubuntu   apt-get install -y curl
    Alpine          apk add curl
    Fedora/RHEL     dnf install curl
  Or build from source, which needs git, Rust, a C compiler AND a built
  llama.cpp checkout — more, not less. See the README's Install section."
            need git || die "no git found: needed to fetch the source (or run this from a checkout)"
            warn "Fetching $REPO..."
            if [ -n "$VERSION" ]; then
                git clone --depth 1 --branch "$VERSION" "https://github.com/$REPO.git" "$tmp/letibot" >&2
            else
                git clone --depth 1 "https://github.com/$REPO.git" "$tmp/letibot" >&2
            fi
            from=$(build_from_source "$tmp/letibot")
            libs_from="${LETIBOT_LLAMA_LIB:-}"
            launcher_from="$tmp/letibot/scripts"
        fi
    fi

    mkdir -p "$INSTALL_DIR" || die "cannot create $INSTALL_DIR"
    # **`letibot` may be absent, and that is a RELEASE being old rather than an
    # archive being broken.** The launcher joined the archive after `v0.1.1` was
    # published, and `releases/latest` is whatever it is — so a user installing today
    # from that release gets everything except this file. Installing the rest and
    # saying so is the honest answer; refusing the whole install would leave them
    # with nothing and no way to get it.
    #
    # Every OTHER name is required, because those are the files `harnessd` needs.
    have_launcher=1
    for want in $BINARIES; do
        src_file="$from/$want"
        if [ "$want" = "letibot" ]; then
            # The checkout's copy wins in a source install; the archive is the only
            # source otherwise. Then the resolved file must be a SCRIPT — see
            # `launcher_from` above for why an ELF here is the failure to avoid.
            if [ -n "$launcher_from" ] && [ -f "$launcher_from/$want" ]; then
                src_file="$launcher_from/$want"
            fi
            if [ ! -f "$src_file" ]; then
                have_launcher=0
                continue
            fi
            if head -c 2 "$src_file" 2>/dev/null | grep -q '^#!'; then
                :
            else
                die "$src_file is not a script, and \`letibot\` must be one.
  That file is a compiled binary — most likely the multicall at
  target/release/letibot, which does not implement the launcher yet. Install the
  release asset, or run this from a checkout whose scripts/letibot is present."
            fi
        else
            [ -f "$src_file" ] || die "$src_file is missing"
        fi
        # cp+chmod rather than install(1): `install` is in GNU and BSD but not in
        # POSIX, and the difference is not worth a portability question here.
        cp "$src_file" "$INSTALL_DIR/$want" || die "cannot write $INSTALL_DIR/$want"
        chmod 755 "$INSTALL_DIR/$want"
    done
    for so in $LIBRARIES; do
        src_so="$from/$so"
        [ -f "$src_so" ] || src_so="${libs_from:-$from}/$so"
        [ -f "$src_so" ] || die "cannot find $so to install: harnessd links it and will not start without it"
        cp -L "$src_so" "$INSTALL_DIR/$so" || die "cannot write $INSTALL_DIR/$so"
        chmod 644 "$INSTALL_DIR/$so"
    done
    # **A Mac's own llama.cpp is usually a Metal build**, and its libllama then also names
    # libggml-metal and libggml-blas. The release asset is CPU-only and has none, but a
    # source install links whatever LETIBOT_LLAMA_LIB holds — so on macOS every further
    # `@rpath/` library the installed ones name is copied too, from the same place, and a
    # name that is nowhere is refused here rather than at the daemon's first exec.
    if [ "$HOST_OS" = Darwin ]; then
        for so in $LIBRARIES; do
            for dep in $(otool -L "$INSTALL_DIR/$so" | sed -n 's|^[[:space:]]*@rpath/\([^ ]*\) .*|\1|p'); do
                [ -f "$INSTALL_DIR/$dep" ] && continue
                src_so="$from/$dep"
                [ -f "$src_so" ] || src_so="${libs_from:-$from}/$dep"
                [ -f "$src_so" ] || die "cannot find $dep, which $so links: harnessd will not start without it"
                cp -L "$src_so" "$INSTALL_DIR/$dep" || die "cannot write $INSTALL_DIR/$dep"
                chmod 644 "$INSTALL_DIR/$dep"
                say "  + $dep  (linked by this llama.cpp build)"
            done
        done
    fi

    # **Say the version rather than assuming.** A binary that cannot run is a
    # failure this script would otherwise report as success — and this is also the
    # first real proof that the libraries landed, because a daemon whose
    # `libllama.so.0` is missing dies at exec with `cannot open shared object file`
    # before it can print anything.
    # `2>&1` and a message that reads the loader's own words: the old one blamed
    # "its libraries are $LIBRARIES, beside it", which are present — the failure it
    # was printing is almost always a HOST library the pre-flight above could not
    # see. The loader names it exactly, so this quotes the loader.
    got=$("$INSTALL_DIR/harnessd" --version 2>&1) || die "installed $INSTALL_DIR/harnessd, but it does not run.
  $got

  If that names a library, it is the loader's own word for what is missing. The
  four in $LIBRARIES are beside the binary; libstdc++, libgomp and libsqlite3 come
  from the host and are listed at the top of this script."
    tui=$("$INSTALL_DIR/letibot-tui" --version 2>&1) || die "installed letibot-tui, but it does not run.
  $tui"
    # **And the LAUNCHER, which now links llama too.**
    #
    # It did not, while `letibot` was only a dispatcher over `sessionlog` and the two
    # renderer crates. Wiring the `m1` role put `letibot-harnessd` in its dependency
    # graph, and `harnessd` links `libllama` — so the binary an install puts on PATH
    # needs the four libraries beside it like everything else. MEASURED, on the first
    # run of the wired role:
    #
    #   ./letibot-m1: error while loading shared libraries: libllama.so.0: cannot
    #   open shared object file
    #
    # Which is why its build script bakes `$ORIGIN` and why this check exists. It is
    # also the check that would have caught it: `--version` needs the loads to
    # resolve, and it needs no daemon, no model and no store.
    #
    # Skipped when the launcher is absent, because an older release does not carry it.
    if [ "$have_launcher" = 1 ]; then
        # **`--help`, NOT `--version`.** MEASURED against a real package: the
        # launcher is a shell script and has no `--version` — the multicall binary
        # does, and the two are different things under one name. The first version of
        # this check used `--version` and would have failed EVERY install with
        # `letibot: unknown flag: --version`, which is the same class of mistake as
        # the launcher being absent: a check asserting a flag nothing implements.
        #
        # `--help` is true of the launcher, needs no daemon and no model, exits 0, and
        # proves what matters — that it ran, and that `$ORIGIN` resolved the llama
        # libraries it now links.
        lch=$("$INSTALL_DIR/letibot" --help 2>&1) || die "installed $INSTALL_DIR/letibot, but it does not run.
  $lch

  It links the same libraries as harnessd, so this is the same answer: one of the
  four in $LIBRARIES is not beside it, or a host library is missing."
        case "$lch" in
            *letibot*) ;;
            *) die "installed $INSTALL_DIR/letibot, and it ran, but said something unexpected:
  $lch" ;;
        esac
    fi
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
    # **What is installed is a harness and a head, NOT a working chat.** Both need a
    # model behind them, and saying so here is the difference between a user who
    # knows what the next step is and one who types `letibot` and reads a connection
    # error. The operator measured the same gap from the other side on
    # `debian:stable-slim`: the install lands, and the first thing that stops you is
    # a model rather than a package.
    say "What you have now: a daemon, a head, and the libraries they link. They do"
    say "not include a MODEL — nothing will answer a turn until one is reachable."
    say ""
    say "Point them at one with a llama.cpp llama-server on 127.0.0.1:8080:"
    say "  llama-server -m MODEL.gguf --port 8080"
    say "or use a cloud provider instead (--provider deepseek|glm|grok)."
    say ""
    if [ "$have_launcher" = 1 ]; then
        say "Then run:  letibot"
    else
        # The closing line must name something that IS there. An archive built
        # before the launcher joined it installs everything else, and telling the
        # user to run a command this release does not carry is the exact defect
        # this branch exists to stop repeating.
        say "This release predates the \`letibot\` launcher, so it was not installed."
        say "Run the head directly for now, or install a release that has it:"
        say "  $INSTALL_DIR/letibot-tui"
    fi
}

main "$@"
