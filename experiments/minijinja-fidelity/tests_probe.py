import sys, os
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..","..","tests","fidelity"))
import oracle_hf
vals = [("none",None),("true",True),("int",3),("float",1.5),("str","s"),("empty_str",""),
        ("list",[1,2]),("empty_list",[]),("map",{"a":1}),("empty_map",{})]
tests = ["none","string","number","integer","float","boolean","iterable","sequence","mapping","defined","undefined","true","false"]
env = oracle_hf.build_env()
for vn, v in vals:
    for t in tests:
        try: r = env.from_string("{{ x is %s }}" % t).render(x=v)
        except Exception as ex: r = "ERR:%s" % type(ex).__name__
        print("%s\t%s\t%s" % (vn, t, r))
    for name, src in (("in","{{ 'a' in x }}"),("length","{{ x | length }}")):
        try: r = env.from_string(src).render(x=v)
        except Exception as ex: r = "ERR:%s" % type(ex).__name__
        print("%s\t%s\t%s" % (vn, name, r))
