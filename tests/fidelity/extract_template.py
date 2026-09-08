#!/usr/bin/env python3
"""Extract `tokenizer.chat_template` from a GGUF, so `template_sha` means something.

    tests/fidelity/extract_template.py ~/models/glm-5.3-flash/GLM-5.3-Flash-...-00001-of-00006.gguf \
        > crates/dialect-glm/template/glm-5.3-flash.jinja

Deliberately dependency-free — `gguf-py` needs numpy, and this only has to walk the
metadata block, which is length-prefixed and sits at the head of the first shard.
Also prints the token table with `--tokens`, which is how a dialect finds out that
`<arg_key>` is one token and `[gMASK]<sop>` is two.
"""

import json
import struct
import sys

SCALARS = {0: "<B", 1: "<b", 2: "<H", 3: "<h", 4: "<I", 5: "<i", 6: "<f", 7: "<?",
           10: "<Q", 11: "<q", 12: "<d"}
TYPE_NAMES = {1: "NORMAL", 2: "UNKNOWN", 3: "CONTROL", 4: "USER_DEFINED", 5: "UNUSED", 6: "BYTE"}


def read_metadata(path, want_tokens=False):
    kv = {}
    with open(path, "rb") as f:
        if f.read(4) != b"GGUF":
            sys.exit(f"{path} is not a GGUF file")
        struct.unpack("<I", f.read(4))  # format version
        _tensors, n_kv = struct.unpack("<QQ", f.read(16))

        def rstr():
            (n,) = struct.unpack("<Q", f.read(8))
            return f.read(n).decode("utf-8", "replace")

        for _ in range(n_kv):
            key = rstr()
            (t,) = struct.unpack("<I", f.read(4))
            if t == 8:
                kv[key] = rstr()
            elif t == 9:
                (et,) = struct.unpack("<I", f.read(4))
                (n,) = struct.unpack("<Q", f.read(8))
                keep = want_tokens and key in (
                    "tokenizer.ggml.tokens", "tokenizer.ggml.token_type")
                if et == 8:
                    if keep:
                        kv[key] = [rstr() for _ in range(n)]
                    else:
                        for _ in range(n):
                            (ln,) = struct.unpack("<Q", f.read(8))
                            f.seek(ln, 1)
                else:
                    fmt = SCALARS[et]
                    size = struct.calcsize(fmt)
                    if keep:
                        kv[key] = list(struct.unpack("<" + fmt[1] * n, f.read(n * size)))
                    else:
                        f.seek(n * size, 1)
            else:
                f.seek(struct.calcsize(SCALARS[t]), 1)
    return kv


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags = {a for a in sys.argv[1:] if a.startswith("--")}
    if not args:
        sys.exit(__doc__)
    kv = read_metadata(args[0], want_tokens="--tokens" in flags)

    if "--tokens" in flags:
        toks = kv.get("tokenizer.ggml.tokens", [])
        types = kv.get("tokenizer.ggml.token_type", [])
        for i, t in enumerate(toks):
            if types[i] in (3, 4):
                print(i, repr(t), TYPE_NAMES.get(types[i], types[i]))
        return
    if "--meta" in flags:
        print(json.dumps({k: v for k, v in kv.items() if isinstance(v, (str, int, float))
                          and k != "tokenizer.chat_template"}, indent=2)[:4000])
        return

    tmpl = kv.get("tokenizer.chat_template")
    if tmpl is None:
        sys.exit("no tokenizer.chat_template in this GGUF")
    sys.stdout.write(tmpl)


if __name__ == "__main__":
    main()
