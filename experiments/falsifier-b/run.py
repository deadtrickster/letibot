"""Falsifier B runner.

Question: does model quality degrade with conversation depth, well before the
context window?

Design notes that matter for reading the numbers:
- One worker per depth, each pinned to its own server slot, so every depth
  keeps its own warm prompt cache. Concurrency is therefore 4 (one in flight
  per depth) and reported timings must be read as such -- they are NOT
  per-request throughput on an idle machine.
- All four depths run over the same wall-clock window, so any drift in server
  state hits every depth equally instead of aliasing onto depth. Within a
  depth, samples are rep-major with the task order rotated each rep, so tasks
  are interleaved and no task sits at a fixed position.
- The filler prefix is byte-identical for every sample at a given depth and the
  task is appended last, with cache_prompt=true, so a depth pays prefill once.
- Temperature is left at the server default (1.0, the production setting), so
  the spread across samples is real and is reported.
"""
import json, os, pathlib, sys, threading, time, urllib.error, urllib.request

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import filler, tasks, score

BASE = "http://127.0.0.1:8080"
MODEL = "qwen-3.8-flash-next"
SLOT_FOR_DEPTH = {0: 0, 20000: 1, 60000: 2, 150000: 4}
MAX_TOKENS = 16000
REPS = 8
DEPTHS = [0, 20000, 60000, 150000]

HERE = pathlib.Path(__file__).resolve().parent
RAW = HERE / "raw"
SCRATCH = pathlib.Path(os.environ.get(
    "FB_SCRATCH",
    "/tmp/claude-1000/-home-dead/1f0655c6-ec72-48bd-b02d-f0da75a80565/scratchpad/fb-crates"))


def chat(messages, id_slot, timeout=3600):
    body = {
        "model": MODEL,
        "messages": messages,
        "max_tokens": MAX_TOKENS,
        "cache_prompt": True,
        "id_slot": id_slot,
        "stream": False,
    }
    req = urllib.request.Request(BASE + "/v1/chat/completions",
                                 data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=timeout))


def one_depth(depth, prefix, exact, out, lock, counter):
    """Run every (task, rep) at one depth, sequentially, on one pinned slot."""
    id_slot = SLOT_FOR_DEPTH[depth]
    for rep in range(REPS):
        # rotate task order each rep: interleaves tasks, varies position
        order = [tasks.TASK_IDS[(i + rep) % len(tasks.TASK_IDS)]
                 for i in range(len(tasks.TASK_IDS))]
        for pos, tid in enumerate(order):
            with lock:
                counter[0] += 1
                n = counter[0]
            task = tasks.BY_ID[tid]
            msgs = prefix + [{"role": "user", "content": task["prompt"]}]
            rec = {"idx": n, "depth_target": depth, "depth_tokens": exact,
                   "id_slot": id_slot, "rep": rep, "position": pos,
                   "task": tid, "ts": time.time()}
            t0 = time.time()
            err, resp = None, None
            for _ in range(3):                     # transport retries only
                try:
                    resp = chat(msgs, id_slot)
                    break
                except (urllib.error.URLError, TimeoutError, OSError) as e:
                    err = f"{type(e).__name__}: {e}"
                    time.sleep(5)
            rec["wall_s"] = round(time.time() - t0, 3)

            if resp is None:
                rec.update({"transport_error": err, "outcome": "transport_error"})
                with lock:
                    out.write(json.dumps(rec) + "\n")
                    print(f"[{n:>3}] d={depth:>6} rep{rep} {tid:22s} "
                          f"TRANSPORT ERROR {err}", flush=True)
                continue

            ch = resp["choices"][0]
            content = ch["message"].get("content") or ""
            reasoning = ch["message"].get("reasoning_content") or ""
            fin = ch.get("finish_reason")
            tm = resp.get("timings", {})
            rec.update({
                "finish_reason": fin,
                "content_chars": len(content),
                "reasoning_chars": len(reasoning),
                "usage": resp.get("usage", {}),
                "cache_n": tm.get("cache_n"),
                "prompt_n": tm.get("prompt_n"),
                "prompt_ms": tm.get("prompt_ms"),
                "predicted_n": tm.get("predicted_n"),
                "predicted_ms": tm.get("predicted_ms"),
                "predicted_per_second": tm.get("predicted_per_second"),
            })

            # a truncated response is a failure of the RUN, not of ability
            if fin == "length":
                rec["outcome"] = "truncated"
                rec.update({"has_code": None, "compiles": None, "tests_pass": None})
            else:
                sc = score.score(tid, content, SCRATCH / f"d{depth}_s{n}")
                rec["outcome"] = "scored"
                rec.update({k: sc[k] for k in ("has_code", "compiles", "tests_pass")})
                rec["build_err"] = sc["build_err"][-1200:]
                rec["test_err"] = sc["test_err"][-1200:]

            rec["content"] = content
            flag = ("TRUNC" if fin == "length"
                    else ("PASS" if rec.get("tests_pass")
                          else ("compiles" if rec.get("compiles")
                                else ("nocode" if not rec.get("has_code") else "FAILC"))))
            with lock:
                out.write(json.dumps(rec) + "\n")
                print(f"[{n:>3}] d={depth:>6} rep{rep} {tid:22s} "
                      f"cache_n={rec['cache_n']:>7} prompt_n={rec['prompt_n']:>6} "
                      f"pred={rec['predicted_n']:>5} {rec['wall_s']:>7.1f}s  {flag}",
                      flush=True)


def main():
    RAW.mkdir(exist_ok=True)
    SCRATCH.mkdir(parents=True, exist_ok=True)
    props = json.load(urllib.request.urlopen(BASE + "/props", timeout=60))

    print("building filler prefixes", flush=True)
    prefixes = {}
    for d in DEPTHS:
        msgs, exact = filler.build(d)
        prefixes[d] = (msgs, exact)

    manifest = {
        "started": time.time(),
        "model": MODEL,
        "n_ctx_per_slot": props["default_generation_settings"]["n_ctx"],
        "total_slots": props["total_slots"],
        "server_defaults": props["default_generation_settings"]["params"],
        "concurrency": len(DEPTHS),
        "concurrency_note": ("one worker per depth, each pinned to its own slot; "
                             "timings are NOT idle-machine per-request numbers"),
        "slot_for_depth": {str(k): v for k, v in SLOT_FOR_DEPTH.items()},
        "max_tokens": MAX_TOKENS,
        "reps": REPS,
        "tasks": tasks.TASK_IDS,
        "corpus": filler.CORPUS,
        "depths": {str(d): {"target": d, "actual_tokens": prefixes[d][1],
                            "turns": len(prefixes[d][0])} for d in DEPTHS},
    }
    (RAW / "manifest.json").write_text(json.dumps(manifest, indent=2))

    out = open(RAW / "samples.jsonl", "a", buffering=1)
    lock = threading.Lock()
    counter = [0]
    t_run = time.time()

    threads = [threading.Thread(target=one_depth,
                                args=(d, prefixes[d][0], prefixes[d][1],
                                      out, lock, counter),
                                name=f"d{d}")
               for d in DEPTHS]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    out.close()
    print(f"done in {(time.time()-t_run)/60:.1f} min", flush=True)


if __name__ == "__main__":
    main()
