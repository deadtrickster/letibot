fn py_escape(s: &str, ensure_ascii: bool, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x7f => out.push(c),
            c => {
                if ensure_ascii {
                    let cp = c as u32;
                    if cp > 0xFFFF {
                        let v = cp - 0x10000;
                        out.push_str(&format!(
                            "\\u{:04x}\\u{:04x}",
                            0xD800 + (v >> 10),
                            0xDC00 + (v & 0x3FF)
                        ));
                    } else {
                        out.push_str(&format!("\\u{:04x}", cp));
                    }
                } else {
                    out.push(c);
                }
            }
        }
    }
    out.push('"');
}

/// CPython `repr(float)`, which is what `json.dumps` emits for a float.
///
/// Rust's `{}` for f64 is also shortest-round-trip, but it NEVER switches to
/// exponent notation: `1e300` prints as a 301-digit integer. CPython switches at
/// decimal exponent 16 and at -5, and pads the exponent to two digits. The fuzz
/// corpus found this; no hand-written fixture had a float in it.
fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let sci = format!("{:e}", f); // shortest round-trip, normalised: "-1.5e2"
    let (mant, exps) = sci.split_once('e').unwrap();
    let exp: i32 = exps.parse().unwrap();
    let neg = mant.starts_with('-');
    let digits: String = mant.trim_start_matches('-').chars().filter(|c| *c != '.').collect();
    let sign = if neg { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let d = digits.as_str();
        if exp >= 0 {
            let ip = (exp + 1) as usize;
            if ip >= d.len() {
                format!("{sign}{}{}.0", d, "0".repeat(ip - d.len()))
            } else {
                format!("{sign}{}.{}", &d[..ip], &d[ip..])
            }
        } else {
            format!("{sign}0.{}{}", "0".repeat((-exp - 1) as usize), d)
        }
    } else {
        format!("{sign}{}e{}{:02}", mant.trim_start_matches('-'),
                if exp < 0 { "-" } else { "+" }, exp.abs())
    }
}

fn py_number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    py_float_repr(n.as_f64().unwrap())
}

fn py_dumps(v: &serde_json::Value, ensure_ascii: bool, sort_keys: bool, out: &mut String) {
    match v {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(true) => out.push_str("true"),
        serde_json::Value::Bool(false) => out.push_str("false"),
        serde_json::Value::Number(n) => out.push_str(&py_number(n)),
        serde_json::Value::String(s) => py_escape(s, ensure_ascii, out),
        serde_json::Value::Array(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                py_dumps(e, ensure_ascii, sort_keys, out);
            }
            out.push(']');
        }
        serde_json::Value::Object(m) => {
            out.push('{');
            let mut first = true;
            let mut emit = |k: &String, val: &serde_json::Value, out: &mut String| {
                if !first {
                    out.push_str(", ");
                }
                first = false;
                py_escape(k, ensure_ascii, out);
                out.push_str(": ");
                py_dumps(val, ensure_ascii, sort_keys, out);
            };
            if sort_keys {
                let sorted: BTreeMap<_, _> = m.iter().collect();
                for (k, val) in sorted {
                    emit(k, val, out);
                }
            } else {
                for (k, val) in m.iter() {
                    emit(k, val, out);
                }
            }
            out.push('}');
        }
    }
}

fn tojson(v: ViaDeserialize<serde_json::Value>, kwargs: Kwargs) -> Result<Value, Error> {
    let ensure_ascii: bool = kwargs.get::<Option<bool>>("ensure_ascii")?.unwrap_or(false);
    let sort_keys: bool = kwargs.get::<Option<bool>>("sort_keys")?.unwrap_or(false);
    let indent: Option<Value> = kwargs.get::<Option<Value>>("indent")?;
    if indent.is_some() && !indent.as_ref().unwrap().is_none() {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            "tojson(indent=...) is not modelled by this stand-in",
        ));
    }
    kwargs.assert_all_used()?;
    let mut out = String::new();
    py_dumps(&v.0, ensure_ascii, sort_keys, &mut out);
    // `safe`: transformers' tojson returns a plain str and autoescaping is off,
    // so marking it safe is a no-op here; returned as a normal string.
    Ok(Value::from(out))
}

fn build_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_trim_blocks(true);
    env.set_lstrip_blocks(true);
    // Jinja2's default Undefined: prints empty, is falsy, but errors on
    // iteration/attribute access. minijinja's Lenient is the closest match.
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Lenient);
    // minijinja implements Jinja the language; `transformers` runs Jinja on top of
    // the PYTHON OBJECT MODEL, and chat templates lean on that heavily --
    // `.strip()`, `.startswith()`, `.items()`, `.split()` are Python methods, not
    // Jinja. minijinja-contrib ships a compatibility shim for exactly this reason.
    env.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
    env.add_filter("tojson", tojson);
    install_python_tests(&mut env);
    env.add_function("raise_exception", |m: String| -> Result<Value, Error> {
        Err(Error::new(ErrorKind::InvalidOperation, m))
    });
    env.add_function("strftime_now", |_f: String| -> Result<Value, Error> {
        // Deliberately not implemented: neither target template calls it, and a
        // clock in a fidelity harness is a source of non-determinism.
        Err(Error::new(
            ErrorKind::InvalidOperation,
            "strftime_now not implemented in the experiment harness",
        ))
    });
    env
}

/// Three of Jinja2's `is` tests answer differently in minijinja, because
/// Jinja2's answers are Python's and minijinja's are Rust's. Found by the fuzz
/// corpus, all three reachable from the shipped templates:
///
///   `none is iterable`  Jinja2 False (iter(None) raises); minijinja True.
///     GLM's `visible_text` and Qwen's `render_content` both branch on this, so
///     a message with `content: null` renders differently on the two engines.
///   `x is sequence`     Jinja2 True for str and dict (both have __len__ and
///     __getitem__); minijinja False for both.
///   `x is number`       Jinja2 True for bool (a Python bool IS an int);
///     minijinja False.
///
/// Overriding a builtin test is supported and is the whole shim: the engine is
/// right about Jinja, and wrong about Python, and the templates are Python.
fn install_python_tests(env: &mut Environment<'static>) {
    use minijinja::value::ValueKind::*;
    env.add_test("iterable", |v: Value| {
        matches!(v.kind(), Seq | Map | String | Iterable)
    });
    env.add_test("sequence", |v: Value| {
        matches!(v.kind(), Seq | Map | String)
    });
    env.add_test("number", |v: Value| {
        matches!(v.kind(), Number | Bool)
    });
}

