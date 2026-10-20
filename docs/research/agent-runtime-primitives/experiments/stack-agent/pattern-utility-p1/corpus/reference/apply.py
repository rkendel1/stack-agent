#!/usr/bin/env python3
"""Applies the reference solution of one task to a repository checkout. Used only to validate that
a task's acceptance procedure passes on a correct solution; no model ever sees these."""
import os, sys

def edit(root, rel, old, new, count=1):
    p = os.path.join(root, rel)
    s = open(p).read()
    assert old in s, (rel, old[:60])
    open(p, "w").write(s.replace(old, new, count))

def write(root, rel, text):
    p = os.path.join(root, rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, "w").write(text)

def t1(root):
    rel = "crates/chip-graph/src/store.rs"
    edit(root, rel, "    Invalid(String),\n}", "    Invalid(String),\n    /// A snapshot file exists but cannot be read.\n    Unreadable(String),\n}")
    edit(root, rel, '            StoreError::Invalid(m) => write!(f, "invalid snapshot: {m}"),', '            StoreError::Invalid(m) => write!(f, "invalid snapshot: {m}"),\n            StoreError::Unreadable(m) => write!(f, "unreadable snapshot: {m}"),')

def t2(root):
    write(root, "crates/chip-experiments/src/complete_after.rs", '''//! A local policy that completes the work from a given turn on.

use chip_core::{LocalWorkPolicy, WorkDecision, WorkView};

/// Completes the work from turn `.0` on, and decides nothing before.
#[derive(Debug, Clone, Copy)]
pub struct CompleteAfter(pub usize);

impl LocalWorkPolicy for CompleteAfter {
    fn propose(&self, view: &WorkView<'_>) -> Option<WorkDecision> {
        (view.turn >= self.0).then(|| WorkDecision::Complete {
            summary: format!("completed after {} turn(s)", self.0),
        })
    }
}
''')
    edit(root, "crates/chip-experiments/src/lib.rs", "pub mod criteria;\n", "pub mod complete_after;\npub mod criteria;\n")
    edit(root, "crates/chip-experiments/src/lib.rs", "pub use criteria::{", "pub use complete_after::CompleteAfter;\npub use criteria::{")

def t3(root):
    write(root, "crates/chip-experiments/src/echo.rs", '''//! A capability that echoes its text back.

use std::collections::BTreeMap;

use chip_core::{
    CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId, CapabilityInput,
    CapabilityProvider, ExecutionError, ExecutionRequest, ExecutionResult, Executor, InputValue,
};

const MAX_TEXT_BYTES: usize = 64;

pub struct EchoBackend;

#[async_trait::async_trait]
impl CapabilityProvider for EchoBackend {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut d = CapabilityDescriptor::new(CapabilityId::new("echo.say")?, "Echo", "Echoes its text.")
            .without_evidence_reuse();
        d.inputs.push(CapabilityInput {
            name: "text".to_string(),
            description: "the text to echo, at most 64 bytes".to_string(),
            required: true,
        });
        Ok(vec![d])
    }

    async fn availability(&self, id: &CapabilityId) -> CapabilityAvailability {
        if id.as_str() == "echo.say" {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable("capability is not provided".into())
        }
    }

    async fn validate_inputs(
        &self,
        _id: &CapabilityId,
        inputs: &BTreeMap<String, InputValue>,
    ) -> Result<(), CapabilityError> {
        match inputs.get("text") {
            Some(InputValue::Text(t)) if t.len() <= MAX_TEXT_BYTES => Ok(()),
            Some(InputValue::Text(t)) => Err(CapabilityError::InvalidInput(format!(
                "text is {} bytes; the limit is {MAX_TEXT_BYTES}",
                t.len()
            ))),
            _ => Err(CapabilityError::InvalidInput("text must be text".into())),
        }
    }
}

#[async_trait::async_trait]
impl Executor for EchoBackend {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        match request.inputs.get("text") {
            Some(InputValue::Text(t)) => Ok(ExecutionResult::success(request.id, t.clone())),
            _ => Err(ExecutionError::InvalidRequest("text must be text".into())),
        }
    }
}
''')
    edit(root, "crates/chip-experiments/src/lib.rs", "pub mod criteria;\n", "pub mod criteria;\npub mod echo;\n")
    edit(root, "crates/chip-experiments/src/lib.rs", "pub use criteria::{", "pub use criteria::{")
    edit(root, "crates/chip-experiments/src/lib.rs", "pub use definition::{", "pub use echo::EchoBackend;\npub use definition::{")

def t4(root):
    write(root, "crates/chip-experiments/src/limited.rs", '''//! Execute a capability request, refusing one that carries too many inputs.

use chip_core::{Agent, CapabilityError, CapabilityRequest, ExecutionReport};

/// As `Agent::execute_capability`, but a request that carries more than `max_inputs` inputs is
/// refused before anything executes.
pub async fn execute_capability_limited(
    agent: &Agent,
    request: &CapabilityRequest,
    max_inputs: usize,
) -> Result<ExecutionReport, CapabilityError> {
    let execution = agent.validate_capability_request(request).await?;
    if execution.inputs.len() > max_inputs {
        return Err(CapabilityError::InvalidInput(format!(
            "request carries {} inputs; the limit is {max_inputs}",
            execution.inputs.len()
        )));
    }
    Ok(agent.execute(execution).await)
}
''')
    edit(root, "crates/chip-experiments/src/lib.rs", "pub mod definition;\n", "pub mod definition;\npub mod limited;\n")
    edit(root, "crates/chip-experiments/src/lib.rs", "pub use plugin::{", "pub use limited::execute_capability_limited;\npub use plugin::{")

def t5(root):
    write(root, "crates/chip-agent-state/tests/keys.rs", '''use chip_agent_state::{action_key, intent_key};

#[test]
fn an_intent_key_is_deterministic_and_names_the_attempt() {
    assert_eq!(intent_key(3, "task-exec-1"), intent_key(3, "task-exec-1"));
    assert_ne!(intent_key(3, "task-exec-1"), intent_key(4, "task-exec-1"));
    assert_ne!(intent_key(3, "task-exec-1"), intent_key(3, "task-exec-2"));
}

#[test]
fn an_action_key_is_the_same_for_every_attempt_and_differs_by_action_and_task() {
    assert_eq!(action_key("t", "t-exec-1"), action_key("t", "t-exec-1"));
    assert_ne!(action_key("t", "t-exec-1"), action_key("t", "t-exec-2"));
    assert_ne!(action_key("t", "t-exec-1"), action_key("u", "t-exec-1"));
}

#[test]
fn the_two_keys_are_different_things() {
    assert_ne!(intent_key(1, "t-exec-1"), action_key("t", "t-exec-1"));
}
''')

def t6(root):
    write(root, "crates/chip-graph/src/bounded_log.rs", '''//! A fixed-capacity log that drops its oldest item to make room.

use std::collections::VecDeque;

pub struct BoundedLog<T> {
    items: VecDeque<T>,
    capacity: usize,
}

impl<T> BoundedLog<T> {
    pub fn new(capacity: usize) -> Option<Self> {
        (capacity > 0).then(|| BoundedLog {
            items: VecDeque::with_capacity(capacity),
            capacity,
        })
    }

    /// Appends `item`, and returns the oldest item if it had to be dropped to make room.
    pub fn push(&mut self, item: T) -> Option<T> {
        let dropped = (self.items.len() == self.capacity)
            .then(|| self.items.pop_front())
            .flatten();
        self.items.push_back(item);
        dropped
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter()
    }
}
''')
    edit(root, "crates/chip-graph/src/lib.rs", "mod analyze;\n", "mod analyze;\nmod bounded_log;\n")
    edit(root, "crates/chip-graph/src/lib.rs", "pub use analyze::{", "pub use bounded_log::BoundedLog;\npub use analyze::{")

def t3_common(root):
    """The most frequent CapabilityProvider shape (two methods): the text limit is checked while
    executing, not when the request is validated. Compiles, echoes, and fails the acceptance."""
    t3(root)
    p = os.path.join(root, "crates/chip-experiments/src/echo.rs")
    s = open(p).read()
    a = s.index("    async fn validate_inputs(")
    b = s.index("#[async_trait::async_trait]\nimpl Executor")
    s = s[:a].rstrip() + "\n}\n\n" + s[b:]
    s = s.replace("Some(InputValue::Text(t)) => Ok(ExecutionResult::success(request.id, t.clone())),",
                  "Some(InputValue::Text(t)) if t.len() > MAX_TEXT_BYTES => Err(ExecutionError::InvalidRequest(\"text is too long\".into())),\n            Some(InputValue::Text(t)) => Ok(ExecutionResult::success(request.id, t.clone())),")
    s = s.replace("    CapabilityProvider, ExecutionError, ExecutionRequest, ExecutionResult, Executor, InputValue,\n};", "    CapabilityProvider, ExecutionError, ExecutionRequest, ExecutionResult, Executor, InputValue,\n};\n#[allow(unused_imports)]\nuse std::collections::BTreeMap as _Unused;")
    open(p, "w").write(s)

def t4_effect_first(root):
    """Executes first and checks afterwards: the effect precedes the validation."""
    t4(root)
    p = os.path.join(root, "crates/chip-experiments/src/limited.rs")
    s = open(p).read()
    a = s.index("    let execution = agent.validate_capability_request")
    b = s.index("}\n", s.rindex("Ok(agent.execute(execution).await)")) 
    body = '''    let early = chip_core::ExecutionRequest::for_capability(request.execution_id.clone(), &request.capability_id)
        .with_inputs(request.inputs.clone());
    let report = agent.execute(early).await;
    let execution = agent.validate_capability_request(request).await?;
    if execution.inputs.len() > max_inputs {
        return Err(CapabilityError::InvalidInput(format!(
            "request carries {} inputs; the limit is {max_inputs}",
            execution.inputs.len()
        )));
    }
    Ok(report)
'''
    s = s[:a] + body + s[b:]
    open(p, "w").write(s)

if __name__ == "__main__":
    {"t1": t1, "t2": t2, "t3": t3, "t4": t4, "t5": t5, "t6": t6, "t3_common": t3_common, "t4_effect_first": t4_effect_first}[sys.argv[1]](sys.argv[2])
