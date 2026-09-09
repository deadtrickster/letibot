// Micro-probes for the three Jinja-semantics questions T1 asks, run against
// minijinja. Each probe prints a line; the CPython side prints the same line
// shape so the two can be diffed mechanically.
use minijinja::{Environment, context, Error, ErrorKind, Value};

fn env() -> Environment<'static> {
    let mut e = Environment::new();
    e.set_trim_blocks(true);
    e.set_lstrip_blocks(true);
    e
}

fn show(name: &str, r: Result<String, Error>) {
    match r {
        Ok(s) => println!("{name}\tOK\t{:?}", s),
        Err(e) => println!("{name}\tERR\t{}", e),
    }
}

fn render(src: &str, ctx: Value) -> Result<String, Error> {
    let mut e = env();
    e.add_template("t", src)?;
    e.get_template("t")?.render(ctx)
}

fn main() {
    // --- 1. loop controls: {% break %} / {% continue %}
    show("break", render(
        "{% for i in [1,2,3,4] %}{% if i == 3 %}{% break %}{% endif %}{{ i }}{% endfor %}",
        context! {},
    ));
    show("continue", render(
        "{% for i in [1,2,3,4] %}{% if i == 3 %}{% continue %}{% endif %}{{ i }}{% endfor %}",
        context! {},
    ));

    // --- 2. bare {% set %} scoping inside {% for %} -- the minja bug.
    // Correct (CPython Jinja2): "A- B:R2 C-" -- the set does NOT survive the
    // iteration, so message C sees `r` undefined again.
    let scoping = r#"{% for m in msgs %}{% if m.r is string %}{% set r = m.r %}{% endif %}{{ m.n }}{% if r is defined %}:{{ r }}{% else %}-{% endif %} {% endfor %}"#;
    show("set_scope", render(scoping, context! {
        msgs => vec![
            context!{ n => "A" },
            context!{ n => "B", r => "R2" },
            context!{ n => "C" },
        ],
    }));

    // Same shape but with namespace() -- must carry ACROSS iterations.
    let ns = r#"{% set ns = namespace(r=none) %}{% for m in msgs %}{% if m.r is string %}{% set ns.r = m.r %}{% endif %}{{ m.n }}:{{ ns.r }} {% endfor %}"#;
    show("namespace_carry", render(ns, context! {
        msgs => vec![
            context!{ n => "A" },
            context!{ n => "B", r => "R2" },
            context!{ n => "C" },
        ],
    }));

    // A bare set in the loop body, read later in the SAME iteration but after
    // an inner block -- checks block-level scoping too.
    show("set_inner_block", render(
        "{% for i in [1,2] %}{% if true %}{% set x = i %}{% endif %}{{ x if x is defined else '-' }}{% endfor %}",
        context! {},
    ));

    // --- 3. tojson
    show("tojson_stock", render(r#"{{ v | tojson }}"#, context! { v => "a<b>&c \u{00e9}" }));
    show("tojson_ensure_ascii", render(r#"{{ v | tojson(ensure_ascii=False) }}"#, context! { v => "a<b>&c \u{00e9}" }));

    // custom tojson replacing the builtin
    let mut e = env();
    e.add_filter("tojson", |v: Value, kwargs: minijinja::value::Kwargs| -> Result<String, Error> {
        let ensure_ascii: bool = kwargs.get::<Option<bool>>("ensure_ascii")?.unwrap_or(true);
        kwargs.assert_all_used()?;
        let jv: serde_json::Value = serde_json::to_value(&v)
            .map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))?;
        let s = serde_json::to_string(&jv)
            .map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))?;
        Ok(if ensure_ascii { s.chars().map(|c| if (c as u32) < 128 { c.to_string() } else { format!("\\u{:04x}", c as u32) }).collect() } else { s })
    });
    e.add_template("ct", r#"{{ v | tojson(ensure_ascii=False) }}|{{ v | tojson }}"#).unwrap();
    show("tojson_custom", e.get_template("ct").unwrap().render(context! { v => "a<b>&c \u{00e9}" }));

    // --- misc: raise_exception / strftime_now globals, `is string` test on undefined
    let mut e2 = env();
    e2.add_function("raise_exception", |m: String| -> Result<Value, Error> {
        Err(Error::new(ErrorKind::InvalidOperation, m))
    });
    e2.add_template("r", "{{ raise_exception('boom') }}").unwrap();
    show("raise_exception", e2.get_template("r").unwrap().render(context! {}));
}
