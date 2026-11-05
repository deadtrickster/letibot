#!/usr/bin/env python3
"""The host side of a remote run: run each step in order, write status.json after every change.

Usage: runner.py RUN_DIR STEPS_JSON
       runner.py --settle RUN_DIR
RUN_DIR holds the checkout (tree/), the log of each step (step-N.log) and status.json.
--settle is the unit's ExecStopPost: a run that ended without writing its end is marked "died".
CARGO_TARGET_DIR is the dir the steps build in and the one the lock is taken on;
RRUN_CLEAN_TARGET=1 removes that dir when the run ends.
"""
import fcntl
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time

TERMINAL = ("passed", "failed", "stopped", "died")


def settle(run_dir):
    """Mark a run whose runner ended without a terminal state, so no reader polls it forever.

    systemd runs this after the unit stops however it stopped (SIGKILL, OOM, a crash, a full disk),
    and hands over why in SERVICE_RESULT, EXIT_CODE and EXIT_STATUS.
    """
    path = os.path.join(run_dir, "status.json")
    try:
        status = json.load(open(path))
    except (OSError, ValueError):
        return 0
    if status.get("state") in TERMINAL:
        return 0
    why = "the runner ended without recording an end: service {} ({} {})".format(
        os.environ.get("SERVICE_RESULT", "?"),
        os.environ.get("EXIT_CODE", "?"),
        os.environ.get("EXIT_STATUS", "?"),
    )
    try:
        st = os.statvfs(run_dir)
        why += ", {} MiB free on the run's disk".format(st.f_bavail * st.f_frsize >> 20)
    except OSError:
        pass
    status["state"] = "died"
    status["error"] = why
    status["ended"] = int(time.time())
    for step in status.get("steps", []):
        if step.get("state") in ("running", "waiting"):
            step["state"] = "died"
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(status, f)
    os.replace(tmp, path)
    return 0


if sys.argv[1] == "--settle":
    sys.exit(settle(sys.argv[2]))

run_dir = sys.argv[1]
# The steps: a file's path (rrun ships them as steps.json), or the JSON itself.
steps = json.load(open(sys.argv[2])) if os.path.isfile(sys.argv[2]) else json.loads(sys.argv[2])
status_path = os.path.join(run_dir, "status.json")
status = json.load(open(status_path))

# **What this run builds in.** `rrun --target` sets CARGO_TARGET_DIR, so two runs handed two dirs
# build at once, and the lock below is about this one path and nothing else.
target = os.environ.get("CARGO_TARGET_DIR") or os.path.join(run_dir, "tree", "target")
# The dir is this run's own, so a run told to clean gives it back when it ends.
clean_target = os.environ.get("RRUN_CLEAN_TARGET") == "1"
# What decides whether the dir is this run's to remove at all: a run stopped while it is still
# queued has built nothing in it, and the run ahead of it is still building there.
holding = False


def remove_run_dirs():
    """The checkout, and the target dir when the run was told to give it back."""
    shutil.rmtree(os.path.join(run_dir, "tree"), ignore_errors=True)
    if clean_target and holding:
        shutil.rmtree(target, ignore_errors=True)


def load():
    try:
        return os.getloadavg()[0]
    except OSError:
        return None


def freshen_sources():
    """Give every tracked file in the checkout one fresh mtime.

    A run that waited for the lock holds a checkout cloned before the build it queued behind, and
    cargo calls a source that is not NEWER than its artifact fresh, so it links the other branch.
    """
    tree = os.path.join(run_dir, "tree")
    try:
        listed = subprocess.run(["git", "ls-files", "-z"], cwd=tree, capture_output=True, check=True)
    except (OSError, subprocess.CalledProcessError):
        return
    for name in listed.stdout.split(b"\0"):
        if name:
            try:
                os.utime(os.path.join(tree, os.fsdecode(name)))
            except OSError:
                pass


def save():
    status["updated"] = int(time.time())
    status["load"] = load()
    tmp = status_path + ".tmp"
    # A full disk must not kill the run between steps: the write is retried at the next save, and
    # the unit's stop hook settles a status that never reached its end.
    try:
        with open(tmp, "w") as f:
            json.dump(status, f)
        os.replace(tmp, status_path)
    except OSError as e:
        print(f"runner: status not written: {e}", file=sys.stderr, flush=True)


# A stopped run says so. `systemctl --user stop rrun-NAME` sends SIGTERM, and without this the
# status kept saying "running" forever, so the Runs tab showed gates nobody was running
# (MEASURED 2026-10-27: five such rows, two of them stopped by hand that morning).
def stopped(signum, _frame):
    status["state"] = "stopped"
    status["ended"] = int(time.time())
    for st in status.get("steps", []):
        if st.get("state") in ("running", "waiting"):
            st["state"] = "stopped"
    save()
    remove_run_dirs()
    sys.exit(128 + signum)


signal.signal(signal.SIGTERM, stopped)
signal.signal(signal.SIGINT, stopped)

FAILURE = re.compile(
    r"(panicked at [^\n]{0,160}|^error: \d+ targets? failed:?$|^    `-p [^`]+`|^error(\[E\d+\])?: [^\n]{0,160})",
    re.M,
)

# One run at a time per target directory. Two checkouts of the same workspace get the same crate
# hashes, so a second run in the shared target overwrites the rlibs the first is still linking
# against (it once failed a doctest with an import the other branch did not have). The lock is
# held until this process exits; a run waiting for it reads "queued".
os.makedirs(target, exist_ok=True)
# Beside the dir, not in it: --clean-target deletes the dir at the end, and a lock inside it would
# let the next run mint a fresh inode and build alongside one that is still queued on the old one.
lock = open(target.rstrip("/") + ".rrun.lock", "w")
try:
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    holding = True
except BlockingIOError:
    status["state"] = "queued"
    save()
    fcntl.flock(lock, fcntl.LOCK_EX)
    holding = True
    status["started"] = int(time.time())

status["state"] = "running"
status["steps"] = [{"cmd": c, "state": "waiting"} for c in steps]
save()
# Under the lock and before any step, so this run's sources are newer than the artifacts it shares.
freshen_sources()
failed = False
for i, cmd in enumerate(steps):
    st = status["steps"][i]
    st.update(state="running", started=int(time.time()))
    status["step"] = i
    save()
    log = os.path.join(run_dir, f"step-{i}.log")
    with open(log, "w") as out:
        p = subprocess.run(
            ["bash", "-lc", cmd],
            cwd=os.path.join(run_dir, "tree"),
            stdout=out,
            stderr=subprocess.STDOUT,
        )
    text = open(log, errors="replace").read()
    st.update(
        state="passed" if p.returncode == 0 else "failed",
        ended=int(time.time()),
        exit=p.returncode,
        failures=list(dict.fromkeys(m.group(0).strip() for m in FAILURE.finditer(text)))[:40]
        if p.returncode
        else [],
        tail=text[-1500:] if p.returncode else "",
    )
    save()
    if p.returncode != 0:
        failed = True
        break
status["state"] = "failed" if failed else "passed"
status["ended"] = int(time.time())
save()
# The target records build-script triggers as absolute paths into this tree, and a later run in
# another tree would see them unchanged and skip the rebuild. Gone, they rerun; and the dir goes
# with the tree when this run was told to clean it.
remove_run_dirs()
sys.exit(1 if failed else 0)
