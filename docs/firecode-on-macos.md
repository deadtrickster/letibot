# letibot + firecode on a Mac, from nothing

Standalone: everything needed to go from a bare Apple-silicon Mac to letibot sessions and
subagents whose tools run in [firecode](https://github.com/deadtrickster/firecode) microVMs.
Each step says how to check it worked before the next one depends on it.

**What you get.** A session or a subagent placed `where: firecode` runs its tools in a VM on a
*copy* of the project: root inside, no permission prompts, no access to your Mac's files, keys
or processes. When it ends, its work lands beside the project as `<project>-<timestamp>`; your
tree is never written to. A placed subagent can be held to an acceptance check that the harness
runs, not the subagent. Containers (docker, compose) run inside the VM.

**Requirements.** Apple silicon (M1 or later), macOS 14 or later, Homebrew, the Xcode Command
Line Tools (`xcode-select --install`), and about 20 GB free.

---

## 1. Install firecode

```sh
git clone https://github.com/deadtrickster/firecode ~/Projects/firecode
cd ~/Projects/firecode

brew install bash coreutils gnu-sed findutils gawk grep e2fsprogs \
  socat flock rsync tmux zstd python@3.13

# on PATH - firecode finds its own checkout through the link
ln -sf ~/Projects/firecode/bin/firecode /opt/homebrew/bin/firecode

firecode setup      # builds the VMM, fetches the arm64 kernel and its modules (~5 min)
firecode prepare    # builds the first guest image, inside a VM (~15 min)
firecode doctor
```

`setup` compiles `firecode-vz` (a small Swift program over Apple's Virtualization.framework) and
signs it locally with the virtualization entitlement; no Apple account is involved. `prepare`
needs no docker and no root. Nothing in the whole install asks for `sudo`.

**Check:** `firecode doctor` ends with `0 failure(s)`. Warnings about macFUSE and opencode are
optional features. Then:

```sh
mkdir -p /tmp/fc-try && cd /tmp/fc-try && git init -q && echo hi > README
firecode up && firecode in 'uname -m; docker run --rm hello-world | head -2' && firecode down
```

expects `aarch64`, then `Hello from Docker!`, then `work delivered:` or `nothing to deliver`.

**The first run reads your Claude credentials from the Keychain**, and macOS may ask whether
`security` may read "Claude Code-credentials". Choose *Always Allow*, or it asks every time the
credentials change.

## 2. Install letibot

The release archive is the short path; see the README's install section. From source on a Mac,
llama.cpp must be built with `@loader_path`:

```sh
git clone https://github.com/ggml-org/llama.cpp ~/Projects/llama.cpp
cmake -S ~/Projects/llama.cpp -B ~/Projects/llama.cpp/build -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON -DCMAKE_INSTALL_RPATH=@loader_path
cmake --build ~/Projects/llama.cpp/build -j "$(sysctl -n hw.ncpu)"

git clone https://github.com/deadtrickster/letibot ~/Projects/letibot/letibot
cd ~/Projects/letibot/letibot
LETIBOT_LLAMA_DIR=~/Projects/llama.cpp LETIBOT_LLAMA_LIB=~/Projects/llama.cpp/build/bin \
    cargo build --release
```

Cloud-only (no local model) skips llama.cpp: `cargo build --release --no-default-features --bins`.

**Check:** `./target/release/harnessd --help | grep -A3 -- --where` lists `host|firecode`.

## 3. Connect them

letibot finds firecode on `PATH`, or wherever `$FIRECODE_BIN` names. Set `FIRECODE_BIN` only if
firecode is not on the daemon's PATH, and restart the daemon after changing either: it is read
once, at start.

**Check:** with firecode missing, `task(where: firecode)` is refused at the call with a sentence
naming `PATH` and `$FIRECODE_BIN`. If you see that, the daemon cannot find it.

## 4. Use it

**A whole session in a VM:**

```sh
cd ~/Projects/stroppy-io
letibot --vm                                    # the session and all its subagents, in one VM
letibot --vm --vm-arg --mem --vm-arg 16384      # anything after --vm-arg goes to `firecode up`
```

With `--vm` the session lives in a VM, and everything is allowed there: no permission
prompts, for it or for any subagent. On a Mac the workspace folder is **mounted read-write**:
every repository in it can be read and changed, and agents create worktrees beside a repository
(`stroppy-fix-x`) and commit to them exactly as they would on the host. All of it lands in
`~/Projects/stroppy-io` as it is made, so nothing is copied and nothing comes back. Nothing
outside that folder is reachable: not your home directory, your keys or other projects.

Subagents, and theirs in turn, run in the **same VM** unless one is explicitly given a VM of
its own. A second `letibot --vm` in the same folder **joins** the VM that is already up instead
of booting another. The VM is brought down by the session that started it.

The trade-off is the point: anything in that VM can change anything in the folder, every
repository included, as an agent on the host could. `--vm-copy` gives the older placement, a
copy of one repository with the work delivered as a sibling directory at the end.

**A subagent in a VM**, from inside a session. The model calls `task` (or `task_start`) with
`where: firecode`, and can hold the child to a check:

```json
{"prompt": "make the e2e suite pass", "where": "firecode",
 "verify": "cd flow && go test -tags tilt ./e2e/postgres_postgres/ -timeout 20m"}
```

`verify` is a shell command that firecode runs in the child's project **after the child has
finished, before its VM stops**. Its exit status is the verdict, and the child cannot fake it.
The parent's answer carries both the delivered path and the verdict:

```
the subagent's VM finished normally. Everything it wrote is in /…/project-20261011-194613-30065 …

ACCEPTANCE CHECK FAILED: `cd flow && go test …` exited 1 (log: /…/project-…/.firecode-verify.log).
The work is NOT done, whatever the subagent said.
```

`verify` without `where: firecode` is refused, because nothing would run it.

**Where the work goes depends on what the child works in.**

- **A worktree** (a `task_start` child, or any session whose workspace is a git worktree) is
  **mounted, not copied**: the worktree read-write, the main checkout read-only, the repository's
  `.git` read-write. The child edits the real worktree and commits to its branch as an agent on
  the host would - `git log <branch>` shows its work while it runs - and nothing is copied in or
  out. Its answer says so: *"It worked in the worktree … itself - mounted, not copied"*.
- **Anything else** is copied, and the work comes back as a sibling directory: diff it and take
  what you want. Nothing is applied for you.

### Just a shell, or just a test run

Not every use needs an agent. From inside a project:

```sh
firecode sh                                   # a contained Linux shell; exit and it is gone
firecode sh -c 'GOARCH=386 go test ./...'     # one command, its exit status
```

The project is mounted read-only with a throwaway layer over it, so tests can write scratch files
and nothing reaches your tree, and nothing is kept afterwards - no copy, no result directory, no
per-project disk. x86 binaries (`GOARCH=386`, amd64 containers) run under qemu-user in the arm64
guest.

## 5. Containers in a VM

docker, compose and buildx are in the guest image, started on first use. A compose stack works
the way it does on a workstation, including service-name resolution and `host.docker.internal`
(which inside a VM means the VM itself). Large stacks need room:

```sh
firecode up --layer-size 48G --fresh-root --mem 16384 --vcpu 8
firecode in 'docker compose up -d --wait && make e2e'
```

or `--vm-arg --layer-size --vm-arg 48G` from letibot. Images, volumes and the build cache share the
VM's workspace layer (16 GB by default); PeerDB's dev stack needed 3.3 GB of images plus 8.9 GB
of build cache. `docker builder prune -af` gives most of the cache back.

Generated files a project ignores (protobuf output, `.env`) are **not** copied into the VM,
because firecode copies what git would. Generate them inside, the way the project's own tooling does.

## 6. Upgrading

Pulling new code changes nothing that is running.

1. **firecode:** `git pull`. Re-run `firecode setup` if the pull touched `macos/`, `scripts/setup-macos.sh`
   or `guest/initramfs/`, and `firecode prepare --force` if it touched `guest/build-rootfs.sh` or
   `guest/systemd/` (both are baked into the image). The other guest scripts travel with every VM
   and take effect at the next start.
2. **letibot:** `git pull`, then **rebuild the release binaries** (the `cargo build --release` line
   above), then restart the daemon. A daemon keeps running the binary it started with.
3. **Reseat.** When a rebuild changes a tool's schema or the system prompt, a resumed session is
   told it is speaking an older prompt and offered `/reseat`. If no offer appears after an
   upgrade you expected to change tools, the daemon is almost certainly still on an old binary:
   compare `stat -f %Sm target/release/harnessd` with the commit you pulled.

## 7. When something is wrong

| symptom | cause | fix |
|---|---|---|
| `task(where: firecode)` refused, naming PATH | the daemon cannot see `firecode` | step 3, then restart the daemon |
| `2 VMs are running - name one with --project` | a command was not told which VM | pass `--project DIR` (letibot always does) |
| `no space left on device` inside a VM | the workspace layer is full | `docker builder prune -af`, or `--layer-size 48G --fresh-root` |
| a project's tests cannot reach `host.docker.internal` | an old guest image | `cd ~/Projects/firecode && git pull`, then start a new VM |
| `docker`: `operation not supported` / bridge errors | a guest without the modules drive | `firecode setup` (builds `images/modules-<ver>.ext4`) |
| no `/reseat` offer after an upgrade | the daemon runs an old binary | step 6.2 |
| a VM you forgot is still up | `up` VMs stay up until told | `firecode list`, then `firecode down --project DIR` |
| `firecode doctor` fails on a tool | a Homebrew package is missing | the line names the `brew install` |

`firecode doctor` and `firecode list` are the first two things to run for anything else. A
VM's console log is under `~/Projects/firecode/runs/<run>/console.log`.
