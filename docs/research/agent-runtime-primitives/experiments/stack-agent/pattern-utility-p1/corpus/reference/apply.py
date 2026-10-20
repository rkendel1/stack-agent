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
    edit(root, "crates/chip-core/src/work.rs", "/// Deterministic policy for tests and demos: the decision for iteration `n` is entry `n`;",
         "/// Completes the work from turn `.0` on, and decides nothing before.\n#[derive(Debug, Clone, Copy)]\npub struct CompleteAfter(pub usize);\n\nimpl LocalWorkPolicy for CompleteAfter {\n    fn propose(&self, view: &WorkView<'_>) -> Option<WorkDecision> {\n        (view.turn >= self.0).then(|| WorkDecision::Complete {\n            summary: format!(\"completed after {} turn(s)\", self.0),\n        })\n    }\n}\n\n/// Deterministic policy for tests and demos: the decision for iteration `n` is entry `n`;")
    edit(root, "crates/chip-core/src/lib.rs", "    EscalationContext, EscalationContextPolicy,", "    CompleteAfter, EscalationContext, EscalationContextPolicy,")

def t3(root):
    write(root, "crates/chip-core/src/echo.rs", '''//! A capability that echoes its text back.

use std::collections::BTreeMap;

use crate::{
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
    edit(root, "crates/chip-core/src/lib.rs", "mod decision;\n", "mod decision;\nmod echo;\n")
    edit(root, "crates/chip-core/src/lib.rs", "pub use capability_set::{", "pub use echo::EchoBackend;\npub use capability_set::{")

def t4(root):
    edit(root, "crates/chip-core/src/lib.rs", "    /// Optionally injects a source of capability descriptors.",
         "    /// As [`execute_capability`](Self::execute_capability), but a request that carries more than\n    /// `max_inputs` inputs is refused before anything executes.\n    pub async fn execute_capability_limited(\n        &self,\n        request: &CapabilityRequest,\n        max_inputs: usize,\n    ) -> Result<ExecutionReport, CapabilityError> {\n        let execution = self.validate_capability_request(request).await?;\n        if execution.inputs.len() > max_inputs {\n            return Err(CapabilityError::InvalidInput(format!(\n                \"request carries {} inputs; the limit is {max_inputs}\",\n                execution.inputs.len()\n            )));\n        }\n        Ok(self.execute(execution).await)\n    }\n\n    /// Optionally injects a source of capability descriptors.")

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

if __name__ == "__main__":
    {"t1": t1, "t2": t2, "t3": t3, "t4": t4, "t5": t5, "t6": t6}[sys.argv[1]](sys.argv[2])
