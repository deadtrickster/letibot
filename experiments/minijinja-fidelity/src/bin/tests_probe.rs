//! Every Jinja `is` test the two target templates use, applied to every value
//! kind they can meet. Printed as one line per (test, value) so the CPython
//! mirror can be diffed against it mechanically.
use minijinja::{Environment, Value, context};

fn main() {
    let mut e = Environment::new();
    e.set_trim_blocks(true); e.set_lstrip_blocks(true);
    e.set_undefined_behavior(minijinja::UndefinedBehavior::Lenient);
    e.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
    let vals: Vec<(&str, Value)> = vec![
        ("none", Value::from(())),
        ("true", Value::from(true)),
        ("int", Value::from(3)),
        ("float", Value::from(1.5)),
        ("str", Value::from("s")),
        ("empty_str", Value::from("")),
        ("list", Value::from_serialize(&vec![1, 2])),
        ("empty_list", Value::from_serialize(&Vec::<i32>::new())),
        ("map", Value::from_serialize(&serde_json::json!({"a": 1}))),
        ("empty_map", Value::from_serialize(&serde_json::json!({}))),
    ];
    let tests = ["none", "string", "number", "integer", "float", "boolean",
                 "iterable", "sequence", "mapping", "defined", "undefined", "true", "false"];
    for (vn, v) in &vals {
        for t in tests {
            let name = format!("{vn}|{t}");
            let src = format!("{{{{ x is {t} }}}}");
            e.add_template_owned(name.clone(), src).unwrap();
            let r = match e.get_template(&name).unwrap().render(context! { x => v.clone() }) {
                Ok(s) => s,
                Err(err) => format!("ERR:{}", err.kind()),
            };
            println!("{vn}\t{t}\t{r}");
        }
        // `in` on the value, as the Qwen template does with `'image' in item`
        let name = format!("{vn}|in");
        e.add_template_owned(name.clone(), "{{ 'a' in x }}".to_string()).unwrap();
        let r = match e.get_template(&name).unwrap().render(context! { x => v.clone() }) {
            Ok(s) => s, Err(err) => format!("ERR:{}", err.kind()),
        };
        println!("{vn}\tin\t{r}");
        // length
        let name = format!("{vn}|length");
        e.add_template_owned(name.clone(), "{{ x | length }}".to_string()).unwrap();
        let r = match e.get_template(&name).unwrap().render(context! { x => v.clone() }) {
            Ok(s) => s, Err(err) => format!("ERR:{}", err.kind()),
        };
        println!("{vn}\tlength\t{r}");
    }
}
