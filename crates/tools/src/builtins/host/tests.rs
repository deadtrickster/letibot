//! `host` on a VM seat: the command runs through the MACHINE's backend and not the guest's, the
//! call goes to the gate like any other, and a host seat is answered with the reason this is not
//! the door for it.

use super::*;
use crate::backend::{BackendError, ExecBackend, Output};
use crate::events::RecordingToolSink;
use crate::runtime::{Gate, GateCall, GateDecision, Registry, ToolRuntime};
use letibot_transcript::{ToolCall, ToolOutcome};

/// What a backend ran, and where it thought it was.
type Ran = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// One backend in a named role: the session's own (a VM's) or the machine's. The name is what a
/// test asserts on, so `host` running on the wrong side is visible in the record.
struct Machine {
    role: &'static str,
    ran: Ran,
}

impl ExecBackend for Machine {
    fn run(&self, cmd: &Command) -> Result<Output, BackendError> {
        self.ran
            .lock()
            .unwrap()
            .push(format!("{}/{}", self.role, cmd.argv.join(" ")));
        Ok(Output {
            stdout: format!("on the {}\n", self.role).into_bytes(),
            stderr: Vec::new(),
            exit: 0,
        })
    }

    fn read(&self, _path: &str) -> Result<Vec<u8>, BackendError> {
        Err(BackendError::Unsupported("a test backend reads nothing"))
    }

    fn write(&self, _path: &str, _bytes: &[u8]) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("a test backend writes nothing"))
    }

    fn processes_arc(&self) -> Option<std::sync::Arc<dyn crate::exec::ProcessHost>> {
        None
    }

    fn workdir(&self, cwd: &str) -> Result<String, BackendError> {
        Ok(cwd.to_string())
    }

    fn list(&self, _path: &str) -> Result<Vec<crate::backend::DirEntry>, BackendError> {
        Ok(Vec::new())
    }

    fn stat(&self, _path: &str) -> Option<crate::backend::DirEntry> {
        None
    }

    fn describe(&self) -> String {
        format!("a test backend in the {} role", self.role)
    }
}

/// The gate, recording every call it was asked about.
struct Asked {
    names: Ran,
    answer: GateDecision,
}

impl Gate for Asked {
    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
        self.names.lock().unwrap().push(call.name.to_string());
        self.answer.clone()
    }
}

/// A runtime whose session runs in a guest and whose machine is the `host` door - or one whose
/// session IS the machine, when `door` is false. Returns what ran and what was adjudicated.
fn runtime(door: bool, answer: GateDecision) -> (ToolRuntime, Ran, Ran) {
    let ran: Ran = Default::default();
    let asked: Ran = Default::default();
    let mut reg = Registry::new();
    reg.register(Box::new(Host)).expect("the host tool");
    let guest = Machine {
        role: "guest",
        ran: ran.clone(),
    };
    let machine = Machine {
        role: "machine",
        ran: ran.clone(),
    };
    let host_seat: Option<std::sync::Arc<dyn ExecBackend>> =
        door.then(|| std::sync::Arc::new(machine) as std::sync::Arc<dyn ExecBackend>);
    let rt = ToolRuntime::new(reg, Box::new(guest))
        .with_host_seat(host_seat)
        .with_gate(Box::new(Asked {
            names: asked.clone(),
            answer,
        }));
    (rt, ran, asked)
}

fn call(args: &str) -> ToolCall {
    ToolCall {
        id: "c1".into(),
        name: "host".into(),
        arguments: args.into(),
    }
}

/// The command runs on THIS machine, once, and never in the guest.
#[test]
fn a_host_call_runs_the_command_on_the_machine_and_not_in_the_guest() {
    let (rt, ran, asked) = runtime(true, GateDecision::Admit);
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke("t1", &call(r#"{"command":"id -un"}"#), &mut sink);
    assert_eq!(r.outcome, ToolOutcome::Ok, "{r:?}");
    assert!(r.payload.contains("on the machine"), "{}", r.payload);
    assert_eq!(
        *ran.lock().unwrap(),
        vec!["machine/sh -c id -un".to_string()],
        "the machine's backend ran it, and only once"
    );
    assert_eq!(
        *asked.lock().unwrap(),
        vec!["host".to_string()],
        "and the gate saw the call by name"
    );
}

/// A `host` call is ADJUDICATED, and a refusal runs nothing at all.
#[test]
fn a_host_call_goes_through_the_gate_and_a_refusal_runs_nothing() {
    let (rt, ran, _) = runtime(
        true,
        GateDecision::refuse_and_tell(
            ToolOutcome::NotRun {
                why: "the operator refused it".into(),
            },
            "the operator refused it".to_string(),
        ),
    );
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke("t1", &call(r#"{"command":"cat ~/.ssh/config"}"#), &mut sink);
    assert!(ran.lock().unwrap().is_empty(), "nothing was run");
    assert_ne!(r.outcome, ToolOutcome::Ok, "{r:?}");
}

/// On a host seat the door is not a door: `bash` already runs there, and the tool says so.
#[test]
fn a_host_seat_is_told_the_shell_is_already_its_own() {
    let (rt, ran, _) = runtime(false, GateDecision::Admit);
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke("t1", &call(r#"{"command":"id -un"}"#), &mut sink);
    assert!(ran.lock().unwrap().is_empty(), "nothing was run");
    assert!(r.payload.contains("needs no second one"), "{}", r.payload);
}

/// A call with no `command` is refused before anything runs, in the words of the field it wants.
#[test]
fn a_host_call_needs_a_command() {
    let (rt, ran, _) = runtime(true, GateDecision::Admit);
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke("t1", &call("{}"), &mut sink);
    assert!(ran.lock().unwrap().is_empty(), "nothing was run");
    assert!(r.payload.contains("`command`"), "{}", r.payload);
}

/// The schema rule the prompt depends on: one line of description, exec-class, `command` required.
#[test]
fn the_schema_names_one_command_and_requires_it() {
    let s = Host.schema();
    assert_eq!(s.name, "host");
    assert_eq!(s.access, crate::schema::Access::Exec);
    assert_eq!(
        s.description.lines().count(),
        1,
        "one line: {}",
        s.description
    );
    assert_eq!(s.parameters["required"][0], "command");
}
