#!/usr/bin/env python3
"""CPython mirror of src/bin/probe.rs -- same probes, same output shape, so the
two can be diffed line by line. Environment is the one oracle_hf.build_env()
builds (transformers 5.16.1's), minus the parts the probes do not need."""
import json, sys, os
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "tests", "fidelity"))
import oracle_hf

def show(name, fn):
    try:
        print("%s\tOK\t%s" % (name, json.dumps(fn(), ensure_ascii=False)))
    except Exception as e:
        print("%s\tERR\t%s" % (name, e))

def r(src, **ctx):
    return lambda: oracle_hf.build_env().from_string(src).render(**ctx)

show("break", r("{% for i in [1,2,3,4] %}{% if i == 3 %}{% break %}{% endif %}{{ i }}{% endfor %}"))
show("continue", r("{% for i in [1,2,3,4] %}{% if i == 3 %}{% continue %}{% endif %}{{ i }}{% endfor %}"))
msgs = [{"n": "A"}, {"n": "B", "r": "R2"}, {"n": "C"}]
show("set_scope", r(
    "{% for m in msgs %}{% if m.r is string %}{% set r = m.r %}{% endif %}{{ m.n }}{% if r is defined %}:{{ r }}{% else %}-{% endif %} {% endfor %}",
    msgs=msgs))
show("namespace_carry", r(
    "{% set ns = namespace(r=none) %}{% for m in msgs %}{% if m.r is string %}{% set ns.r = m.r %}{% endif %}{{ m.n }}:{{ ns.r }} {% endfor %}",
    msgs=msgs))
show("set_inner_block", r(
    "{% for i in [1,2] %}{% if true %}{% set x = i %}{% endif %}{{ x if x is defined else '-' }}{% endfor %}"))
import jinja2
def stock_tojson():
    e = jinja2.Environment(trim_blocks=True, lstrip_blocks=True)
    return e.from_string("{{ v | tojson }}").render(v="a<b>&c é")
show("tojson_stock", stock_tojson)
def stock_ensure_ascii():
    e = jinja2.Environment(trim_blocks=True, lstrip_blocks=True)
    return e.from_string("{{ v | tojson(ensure_ascii=False) }}").render(v="a<b>&c é")
show("tojson_ensure_ascii", stock_ensure_ascii)
show("tojson_custom", r("{{ v | tojson(ensure_ascii=False) }}|{{ v | tojson }}", v="a<b>&c é"))
show("raise_exception", r("{{ raise_exception('boom') }}"))
