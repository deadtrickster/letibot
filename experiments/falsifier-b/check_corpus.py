"""Trap guard: assert no task identifier leaks into the filler corpus.

If a task name appeared in the preceding conversation the deeper depths would
be priming the model for the task, which would bias the experiment towards
finding NO degradation.
"""
import re, sys, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import filler, tasks

IDENTS = ["merge_intervals", "parse_size", "longest_common_prefix",
          "LruCache", "parse_range_list"]

def main():
    bad = 0
    for path in filler.CORPUS:
        text = open(path, encoding="utf-8").read().lower()
        for ident in IDENTS:
            n = text.count(ident.lower())
            if n:
                print(f"LEAK: {ident} x{n} in {path}")
                bad += 1
    # also check the assembled deepest prefix, lead-ins included
    msgs, exact = filler.build(150000, verbose=False)
    blob = "\n".join(m["content"] for m in msgs).lower()
    for ident in IDENTS:
        n = blob.count(ident.lower())
        if n:
            print(f"LEAK in assembled 150k prefix: {ident} x{n}")
            bad += 1
    print(f"assembled 150k prefix: {len(msgs)} turns, {exact} tokens, "
          f"{len(blob)} chars")
    print("CLEAN" if bad == 0 else f"{bad} LEAKS")
    return 1 if bad else 0

if __name__ == "__main__":
    sys.exit(main())
