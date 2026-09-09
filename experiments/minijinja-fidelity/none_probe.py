import sys, os, json
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..","..","tests","fidelity"))
import oracle_hf
cases = [
    ("render_none","[{{ n }}]"),("concat_tilde","[{{ 'x' ~ n }}]"),("plus_str","[{{ n | string }}]"),
    ("in_list","[{{ [1, n] }}]"),("join","[{{ [1, n] | join('-') }}]"),("is_none","[{{ n is none }}]"),
    ("truthy","[{% if n %}T{% else %}F{% endif %}]"),("default","[{{ n | default('D') }}]"),
    ("trim","[{{ (n | string) | trim }}]"),("is_string","[{{ n is string }}]"),
    ("is_iterable","[{{ n is iterable }}]"),("undef_render","[{{ missing }}]"),
    ("undef_is_none","[{{ missing is none }}]"),("undef_is_defined","[{{ missing is defined }}]"),
    ("nested_attr_undef","[{{ n.foo }}]"),
]
for name, src in cases:
    try:
        print("%s\tOK\t%s" % (name, json.dumps(oracle_hf.build_env().from_string(src).render(n=None), ensure_ascii=False)))
    except Exception as ex:
        print("%s\tERR\t%s" % (name, ex))
