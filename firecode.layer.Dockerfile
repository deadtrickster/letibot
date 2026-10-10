# letibot's guest layer, as an IMPORTED IMAGE — the shared cache for this project.
#
# WHY THIS EXISTS ALONGSIDE `firecode.layer`. That file is the RECIPE: what a fresh guest needs,
# which firecode applies before a VM is announced up. It is applied per copy, and every
# daemon-spawned child is a copy with its own path — so its own project, so its own layer. Measured
# 2026-10-10 by looking at what those layers weigh: 144 M where nothing ran, 1.8 G for a toolchain
# alone, 2.0 G for a toolchain plus the registry, 2.6 G where the llama build also ran. One layer per
# SESSION id, every one of them filled from scratch — which is why a reviewer VM paid the whole two
# minutes again. The operator's words: *"2 minutes is too much if layer was saved."*
#
# An imported image is the layer firecode shares: its own table says so — *"imported images: docker
# images added with `firecode layer add`, one per digest"*, read-only and mounted into every VM. So
# the expensive half is baked here once, on the host's cores, and every copy inherits it.
#
# THE RECIPE KEEPS ITS GUARDS, which is what makes the pairing work: `firecode.layer` still runs on a
# fresh copy, and every line of it finds its work already done — the toolchain search finds cargo,
# the clone's `-d .git` test passes, cmake re-links nothing, `cargo fetch` finds a warm registry. The
# apply goes from two minutes to seconds and stays correct if this image is ever absent.
#
# NOT baked here, deliberately: the crates.io registry. A probe measured a cold `cargo fetch` inside
# a 5.85 s check, so it is seconds, and baking it would mean shipping this repo's Cargo.lock into a
# shared cache that other projects also read.

FROM firecode/rootfs:latest

ENV DEBIAN_FRONTEND=noninteractive

# ── The dev packages the build scripts need ──────────────────────────────────────────────────────
# The guest has the RUNTIME libsqlite3.so.0 and no sqlite3.pc and no /usr/include/sqlite3.h. `cc` is
# NOT named: it is a virtual package on Ubuntu, and asking apt for it killed the layer line that
# named it (exit 100, two seconds in) — three lines behind it never ran.
RUN set -eux; apt-get update; apt-get install -y --no-install-recommends libsqlite3-dev pkg-config cmake ninja-build git; rm -rf /var/lib/apt/lists/*

# ── The Rust toolchain, system-wide, and reachable by uid 1000 ───────────────────────────────────
# `RUSTUP_HOME`/`CARGO_HOME` under /usr/local because the guest's work runs as uid 1000 and a
# toolchain in root's home is invisible to it — and the PROFILE FILE is the load-bearing half: a
# probe proved a stripped shell cannot run cargo without it even with the toolchain installed.
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
RUN set -eux; curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --default-toolchain stable; ln -sfn /usr/local/cargo/bin/cargo /usr/local/bin/cargo; ln -sfn /usr/local/cargo/bin/rustc /usr/local/bin/rustc; ln -sfn /usr/local/cargo/bin/rustup /usr/local/bin/rustup; cargo --version; rustc --version
RUN printf 'export RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo\n' > /etc/profile.d/10-letibot-rust.sh
# ── Ownership, so the recipe's chown finds nothing to do ─────────────────────────────────────
#
# MEASURED: any edit to `firecode.layer` re-runs EVERY one of its RUN lines — the stamp is per FILE —
# and the recipe's `chown -R 1000:1000 /usr/local/cargo /usr/local/rustup` costs about 24 of the 40
# seconds such a boot takes, against about 14 on an unchanged layer file. The image should own its own
# toolchain so that line is a no-op. The crates.io registry is deliberately NOT here: it needs this
# repo's Cargo.lock, and the recipe's own fetch measured seconds.
RUN set -eux; chown -R 1000:1000 /usr/local/cargo /usr/local/rustup; ls -ld /usr/local/cargo /usr/local/rustup

# ── The llama.cpp checkout crates/llama/build.rs insists on ──────────────────────────────────────
# The FORK, at the absolute path the build reads by default (DEFAULT_LLAMA_DIR), which is why nothing
# sets LETIBOT_LLAMA_DIR anywhere: the path IS the default. /home/dead/Projects does not exist in the
# guest, so the mkdir is load-bearing.
RUN set -eux; mkdir -p /home/dead/Projects; git clone --depth 1 https://github.com/deadtrickster/llama.cpp.git /home/dead/Projects/llama.cpp; test -f /home/dead/Projects/llama.cpp/include/llama.h

# ── The real library, built here rather than in a guest ──────────────────────────────────────────
# This is the step that dominates the two minutes in a guest (4 cores, no swap). CPU-only: a guest
# has no GPU to pass through, and a reviewer checks that a branch COMPILES AND PASSES. Tests,
# examples and the server are off because nothing under test runs them.
RUN set -eux; cd /home/dead/Projects/llama.cpp; cmake -B build-glm -G Ninja -DCMAKE_BUILD_TYPE=Release -DGGML_CUDA=OFF -DLLAMA_CURL=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_SERVER=OFF; cmake --build build-glm --target llama --parallel 8; ls -l /home/dead/Projects/llama.cpp/build-glm/bin/libllama.so
