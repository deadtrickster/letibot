//! `cargo run -p letibot-tools --features testing --example exec_transcript`
//!
//! Prints, verbatim, the three things `TODO.md` T21 and T24 ask to see: the
//! refusal a kill-by-pattern gets, the refusal a wait-by-pattern gets, and the
//! record a reap leaves behind. It exists because those are prose the model
//! reads, and prose that is only ever asserted on in a test drifts into
//! something nobody would want to receive.
use letibot_tools::testing::runner_harness;

fn main() {
    let mut h = runner_harness().expect("cgroup v2");
    let host = h.processes.clone().unwrap();
    host.protect_outliving(
        std::process::id(),
        "this is the model server serving this session (llama-server on :8080)",
    );
    let comm = std::fs::read_to_string(format!("/proc/{}/comm", std::process::id()))
        .unwrap()
        .trim()
        .to_string();

    println!("======== T21.1 — kill by pattern ========");
    println!(
        "{}",
        h.call(
            "bash",
            &serde_json::json!({"command": "pkill -f llama-server"}).to_string()
        )
        .render()
    );

    println!("\n======== T21.2 — wait by pattern ========");
    println!(
        "{}",
        h.call(
            "bash",
            &serde_json::json!({
                "command": format!("until pgrep -f {comm}; do sleep 2; done; echo up")
            })
            .to_string()
        )
        .render()
    );

    println!("\n======== the reaper's record, through job_list ========");
    let started = h.call(
        "bash",
        &serde_json::json!({"command": "sh -c 'sleep 300 & sleep 300'", "background": true})
            .to_string(),
    );
    let id = {
        let t = &started.payload;
        let a = t.find("`j").unwrap();
        let r = &t[a + 1..];
        r[..r.find('`').unwrap()].to_string()
    };
    println!(
        "{}",
        h.call("job_kill", &serde_json::json!({"job": id}).to_string())
            .render()
    );
    println!("---- and job_list afterwards ----");
    println!("{}", h.call("job_list", "{}").render());
}
