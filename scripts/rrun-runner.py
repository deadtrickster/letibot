#!/usr/bin/env python3
"""The host side of a remote run: run each step in order, write status.json after every change.

Usage: runner.py RUN_DIR STEPS_JSON
RUN_DIR holds the checkout (tree/), the log of each step (step-N.log) and status.json.
"""
import json
import os
import re
import subprocess
import sys
import time

run_dir = sys.argv[1]
# The steps: a file's path (rrun ships them as steps.json), or the JSON itself.
steps = json.load(open(sys.argv[2])) if os.path.isfile(sys.argv[2]) else json.loads(sys.argv[2])
status_path = os.path.join(run_dir, "status.json")
status = json.load(open(status_path))


def load():
    try:
        return os.getloadavg()[0]
    except OSError:
        return None


def save():
    status["updated"] = int(time.time())
    status["load"] = load()
    tmp = status_path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(status, f)
    os.replace(tmp, status_path)


FAILURE = re.compile(
    r"(panicked at [^\n]{0,160}|^error: \d+ targets? failed:?$|^    `-p [^`]+`|^error(\[E\d+\])?: [^\n]{0,160})",
    re.M,
)

status["state"] = "running"
status["steps"] = [{"cmd": c, "state": "waiting"} for c in steps]
save()
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
