use minijinja::{Environment, Value, context};
fn main() {
    let mut e = Environment::new();
    e.set_trim_blocks(true); e.set_lstrip_blocks(true);
    e.set_undefined_behavior(minijinja::UndefinedBehavior::Lenient);
    e.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
    let cases = [
        ("render_none", "[{{ n }}]"),
        ("concat_tilde", "[{{ 'x' ~ n }}]"),
        ("plus_str", "[{{ n | string }}]"),
        ("in_list", "[{{ [1, n] }}]"),
        ("join", "[{{ [1, n] | join('-') }}]"),
        ("is_none", "[{{ n is none }}]"),
        ("truthy", "[{% if n %}T{% else %}F{% endif %}]"),
        ("default", "[{{ n | default('D') }}]"),
        ("trim", "[{{ (n | string) | trim }}]"),
        ("is_string", "[{{ n is string }}]"),
        ("is_iterable", "[{{ n is iterable }}]"),
        ("undef_render", "[{{ missing }}]"),
        ("undef_is_none", "[{{ missing is none }}]"),
        ("undef_is_defined", "[{{ missing is defined }}]"),
        ("nested_attr_undef", "[{{ n.foo }}]"),
    ];
    for (name, src) in cases {
        e.add_template_owned(name.to_string(), src.to_string()).unwrap();
        match e.get_template(name).unwrap().render(context!{ n => Value::from(()) }) {
            Ok(s) => println!("{name}\tOK\t{s:?}"),
            Err(err) => println!("{name}\tERR\t{err}"),
        }
    }
}
