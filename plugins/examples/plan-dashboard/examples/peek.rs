fn main() {
    let text = std::fs::read_to_string("status.example.json").unwrap();
    let input = serde_json::json!({"v":1,"state":null,"look":{"exit":0,"stdout":text,"stderr":""}});
    println!(
        "{}",
        serde_json::to_string_pretty(&plan_dashboard::view(&input)).unwrap()
    );
}
