import struct, sys, re, collections, glob
T={0:'B',1:'b',2:'H',3:'h',4:'I',5:'i',6:'f',7:'?',10:'Q',11:'q',12:'d'}
def names(path):
    f=open(path,'rb')
    u32=lambda: struct.unpack('<I',f.read(4))[0]
    u64=lambda: struct.unpack('<Q',f.read(8))[0]
    s  =lambda: f.read(u64()).decode('utf-8','replace')
    def val(t):
        if t==8: return s()
        if t==9:
            et=u32(); n=u64()
            if et==8: return [s() for _ in range(n)]
            fmt=T[et]; return list(struct.unpack('<%d%s'%(n,fmt),f.read(n*struct.calcsize(fmt))))
        fmt=T[t]; return struct.unpack('<'+fmt,f.read(struct.calcsize(fmt)))[0]
    assert f.read(4)==b'GGUF'
    u32(); nt=u64(); nkv=u64()
    for _ in range(nkv): s(); val(u32())
    out=[]
    for _ in range(nt):
        n=s(); nd=u32(); [u64() for _ in range(nd)]; u32(); u64(); out.append(n)
    return out
label, pattern = sys.argv[1], sys.argv[2]
per=collections.defaultdict(set)
for p in sorted(glob.glob(pattern)):
    for n in names(p):
        m=re.match(r'blk\.(\d+)\.(.+?)(\.weight|\.bias|_scale.*)?$', n)
        if m: per[int(m.group(1))].add(m.group(2))
full=[i for i,ps in sorted(per.items()) if not any(p.startswith('ssm_') for p in ps)]
lin =[i for i,ps in sorted(per.items()) if     any(p.startswith('ssm_') for p in ps)]
tot=len(per)
print(f"{label:22} blocks {tot:3}   FULL-ATTENTION {len(full):3} ({100*len(full)/tot:.0f}%)   LINEAR/SSM {len(lin):3} ({100*len(lin)/tot:.0f}%)")
print(f"{'':22} full-attention at {full}")
