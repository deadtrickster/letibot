import struct, sys
T = {0:'B',1:'b',2:'H',3:'h',4:'I',5:'i',6:'f',7:'?',10:'Q',11:'q',12:'d'}
def rd(f):
    def u32(): return struct.unpack('<I', f.read(4))[0]
    def u64(): return struct.unpack('<Q', f.read(8))[0]
    def s():   return f.read(u64()).decode('utf-8', 'replace')
    def val(t):
        if t == 8: return s()
        if t == 9:
            et = u32(); n = u64()
            if et == 8: return [s() for _ in range(n)]
            fmt = T[et]; sz = struct.calcsize(fmt)
            return list(struct.unpack('<%d%s' % (n, fmt), f.read(n*sz)))
        fmt = T[t]; return struct.unpack('<'+fmt, f.read(struct.calcsize(fmt)))[0]
    assert f.read(4) == b'GGUF'
    u32(); ntensor = u64(); nkv = u64()
    kv = {}
    for _ in range(nkv):
        k = s(); kv[k] = val(u32())
    return ntensor, kv
for path in sys.argv[1:]:
    with open(path,'rb') as f: nt, kv = rd(f)
    print("="*70); print(path.split('/')[-1], f"({nt} tensors in this shard)")
    for k in sorted(kv):
        v = kv[k]
        if isinstance(v, list):
            if len(v) > 12: v = f"[{len(v)} values] {v[:8]}..{v[-2:]}"
        elif isinstance(v, str) and len(v) > 90: v = v[:90]+"..."
        if any(w in k for w in ('token_embd','tokenizer.ggml.tokens','merges','token_type','scores')): continue
        print(f"  {k:52} {v}")
