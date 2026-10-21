//! What `stack-agent work` shows while a work runs: one short line per model call and per
//! operation, on stderr, so a run that takes minutes is not a silent terminal.
//!
//! Every line is derived from a [`WorkEvent`] the loop already records; nothing here decides,
//! retries or changes anything. A line names the capability, the project-relative path and, for a
//! refused or failed operation, its error code, or the test counts PAX reported. It never carries
//! file content, a model reply, a goal or a credential. The finished report, not these lines, is
//! the verdict.

use std::collections::HashMap;
use std::sync::Mutex;

use chip_core::{ExecutionEvent, WorkEvent};

/// The longest piece of an execution's own failure text shown on one line.
const MAX_DETAIL_CHARS: usize = 160;

/// Turns events into progress lines. It remembers which capability each execution id asked for,
/// because the result event names only the id.
#[derive(Debug, Default)]
pub struct Progress {
    capabilities: Mutex<HashMap<String, String>>,
}

impl Progress {
    /// The line for `event`, if it is one a person watching the run needs.
    pub fn line(&self, event: &WorkEvent) -> Option<String> {
        let mut capabilities = self.capabilities.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            // Events count turns from 0; the report counts them from 1.
            WorkEvent::ModelEscalation { turn, .. } => {
                Some(format!("turn {}: asking the model", turn + 1))
            }
            WorkEvent::Execution(ExecutionEvent::ExecutionRequested { id, intent }) => {
                capabilities.insert(id.0.clone(), intent.clone());
                None
            }
            WorkEvent::Execution(ExecutionEvent::ExecutionCompleted { id, output }) => {
                let capability = capabilities.get(&id.0).cloned().unwrap_or_default();
                Some(format!("  ok      {}", describe(&capability, output)))
            }
            WorkEvent::Execution(ExecutionEvent::ExecutionFailed { id, reason }) => {
                let capability = capabilities.get(&id.0).cloned().unwrap_or_default();
                Some(format!("  failed  {}", describe(&capability, reason)))
            }
            _ => None,
        }
    }
}

/// `capability path (detail)`, from the first line of an execution's output when it is the
/// canonical JSON object Chip's executors write; otherwise a bounded first line of it.
pub fn describe(capability: &str, output: &str) -> String {
    let first = output.lines().next().unwrap_or_default();
    let Ok(head) = serde_json::from_str::<serde_json::Value>(first) else {
        let detail: String = first.chars().take(MAX_DETAIL_CHARS).collect();
        return if detail.is_empty() {
            capability.to_string()
        } else {
            format!("{capability} ({detail})")
        };
    };
    let mut text = capability.to_string();
    if let Some(path) = head["path"].as_str() {
        text.push(' ');
        text.push_str(path);
    }
    let detail = match (head["error"].as_str(), head["status"].as_str()) {
        (Some(error), _) => Some(match head["found"].as_str() {
            Some(found) => format!("{error}: {found}"),
            None => error.to_string(),
        }),
        // PAX's own result: its status and the counts it gave.
        (None, Some(status)) => Some(match head["tests"].as_object() {
            Some(t) => format!(
                "tests {status}: {} passed, {} failed",
                t.get("passed").and_then(|v| v.as_u64()).unwrap_or(0),
                t.get("failed").and_then(|v| v.as_u64()).unwrap_or(0)
            ),
            None => format!("tests {status}"),
        }),
        (None, None) => None,
    };
    if let Some(detail) = detail {
        text.push_str(&format!(" ({detail})"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::Progress;
    use chip_core::{ContextMetrics, ExecutionEvent, ExecutionId, WorkEvent, WorkId};

    fn requested(id: &str, capability: &str) -> WorkEvent {
        WorkEvent::Execution(ExecutionEvent::ExecutionRequested {
            id: ExecutionId(id.into()),
            intent: capability.into(),
        })
    }

    #[test]
    fn each_model_call_and_operation_is_one_line_without_content() {
        let p = Progress::default();
        let escalation = WorkEvent::ModelEscalation {
            work_id: WorkId::new("w"),
            turn: 2,
            reason: "r".into(),
            context_policy: "p".into(),
            context: ContextMetrics {
                bytes: 0,
                chars: 0,
                observations: 0,
                decisions: 0,
                evidence_items: 0,
                ruled_out: 0,
                messages: 0,
                system_messages: 0,
                user_messages: 0,
                assistant_messages: 0,
                omitted_observations: 0,
                condensed_observations: 0,
            },
        };
        assert_eq!(
            p.line(&escalation).as_deref(),
            Some("turn 3: asking the model")
        );

        assert_eq!(p.line(&requested("e1", "project.read")), None);
        let read = WorkEvent::Execution(ExecutionEvent::ExecutionCompleted {
            id: ExecutionId("e1".into()),
            output: "{\"capability\":\"project.read\",\"path\":\"src/lib.rs\",\"sha256\":\"ab\"}\n--- content ---\nsecret body\n".into(),
        });
        let line = p.line(&read).unwrap();
        assert_eq!(line, "  ok      project.read src/lib.rs");
        assert!(!line.contains("secret body"));

        p.line(&requested("e2", "project.write"));
        let stale = WorkEvent::Execution(ExecutionEvent::ExecutionFailed {
            id: ExecutionId("e2".into()),
            reason: "{\"capability\":\"project.write\",\"error\":\"stale_base\",\"found\":\"different_content\",\"path\":\"src/lib.rs\"}".into(),
        });
        assert_eq!(
            p.line(&stale).as_deref(),
            Some("  failed  project.write src/lib.rs (stale_base: different_content)")
        );

        p.line(&requested("e3", "pax.test"));
        let tests = WorkEvent::Execution(ExecutionEvent::ExecutionFailed {
            id: ExecutionId("e3".into()),
            reason: "{\"schema\":\"pax.execution-result.v1\",\"status\":\"failed\",\"tests\":{\"passed\":0,\"failed\":1}}".into(),
        });
        assert_eq!(
            p.line(&tests).as_deref(),
            Some("  failed  pax.test (tests failed: 0 passed, 1 failed)")
        );

        // A failure that is not a canonical line is shown bounded, on one line.
        p.line(&requested("e4", "project.read"));
        let long = WorkEvent::Execution(ExecutionEvent::ExecutionFailed {
            id: ExecutionId("e4".into()),
            reason: format!("{}\nsecond line", "x".repeat(500)),
        });
        let line = p.line(&long).unwrap();
        assert!(line.len() < 200 && !line.contains("second line"), "{line}");
    }
}
