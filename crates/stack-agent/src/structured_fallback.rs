//! An explicit, bounded ladder for a server that refuses the structured-output request.
//!
//! Off unless the operator asks (`--structured-output-fallback on`). A server that cannot enforce
//! the requested `response_format` often answers the request itself with an HTTP error
//! (llama-cpp-python answers `json_schema` with a 500; some hosted APIs with a 400). With the ladder
//! on, such a refusal moves the work down one rung (the configured mode, then `json-object`, then
//! `none`) **for the rest of the work**, and the same request is sent again on the next rung.
//!
//! What this is not: a retry loop, a model or provider switch, or a repair. Only the *request
//! format* changes, the model and endpoint are the ones the operator chose, a rung is never
//! re-climbed, and at most `rungs - 1` steps can happen per work. A reply is validated by Chip
//! exactly as on any rung; what a weaker rung cannot enforce, Chip still checks. Every step is
//! reported (stderr, and `structured_output_fallback` in the report) with the status and reason.
//!
//! Only an HTTP answer that rejects the request counts (400, 404, 415, 422, 500, 501). A timeout,
//! a transport failure, an authentication or rate-limit answer, or a reply that arrived and was bad
//! never steps down: those are not statements about the response format.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse};
use serde_json::{Value, json};

use crate::work_generation::StructuredOutput;

/// HTTP statuses that say "this request was not accepted", the only evidence a step needs.
const REFUSAL_STATUSES: [u16; 6] = [400, 404, 415, 422, 500, 501];

pub struct Rung {
    pub mode: StructuredOutput,
    pub provider: Arc<dyn ModelProvider>,
}

#[derive(Debug, Clone)]
struct Step {
    from: StructuredOutput,
    to: StructuredOutput,
    status: u16,
    reason: String,
}

pub struct StructuredFallback {
    rungs: Vec<Rung>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    current: usize,
    steps: Vec<Step>,
}

impl StructuredFallback {
    pub fn new(rungs: Vec<Rung>) -> Self {
        assert!(!rungs.is_empty(), "a ladder has at least one rung");
        Self {
            rungs,
            state: Mutex::default(),
        }
    }

    /// The HTTP status of a refusal of the request, if `error` is one.
    pub(crate) fn refusal(error: &FxError) -> Option<(u16, String)> {
        let FxError::Inference(failure) = error else {
            return None;
        };
        let status = failure.status.filter(|s| REFUSAL_STATUSES.contains(s))?;
        let detail = failure.detail.clone().unwrap_or_default();
        Some((status, detail.chars().take(200).collect()))
    }

    /// For the work report.
    pub fn report(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        json!({
            "enabled": true,
            "rungs": self.rungs.iter().map(|r| r.mode.name()).collect::<Vec<_>>(),
            "used": self.rungs[state.current].mode.name(),
            "steps": state.steps.iter().map(|s| json!({
                "from": s.from.name(), "to": s.to.name(), "status": s.status, "reason": s.reason,
            })).collect::<Vec<_>>(),
        })
    }
}

#[async_trait]
impl ModelProvider for StructuredFallback {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        loop {
            let current = self.state.lock().unwrap_or_else(|e| e.into_inner()).current;
            let rung = &self.rungs[current];
            let error = match rung.provider.complete(request.clone()).await {
                Ok(response) => return Ok(response),
                Err(error) => error,
            };
            let Some((status, reason)) = Self::refusal(&error) else {
                return Err(error);
            };
            if current + 1 >= self.rungs.len() {
                return Err(error);
            }
            let next = self.rungs[current + 1].mode;
            eprintln!(
                "note: the endpoint refused the `{}` structured-output request (HTTP {status}); \
                 continuing with `{}` for the rest of this work",
                rung.mode.name(),
                next.name()
            );
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.current = current + 1;
            state.steps.push(Step {
                from: rung.mode,
                to: next,
                status,
                reason,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chip_inference::{Delivery, InferenceFailure, Usage};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scripted {
        calls: AtomicUsize,
        answer: Result<(), FxError>,
    }

    #[async_trait]
    impl ModelProvider for Scripted {
        async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.answer {
                Ok(()) => Ok(ModelResponse::new("m", "{}".to_string(), Usage::new(1, 1))),
                Err(e) => Err(e.clone()),
            }
        }
    }

    fn http(status: u16) -> FxError {
        FxError::Inference(Box::new(InferenceFailure {
            kind: "provider_error".into(),
            delivery: Delivery::ConfirmedResponse,
            status: Some(status),
            retry_after_ms: None,
            detail: Some("grammar unsupported".into()),
            partial_output: None,
            remote_cancellation: None,
        }))
    }

    fn rung(mode: StructuredOutput, answer: Result<(), FxError>) -> (Rung, Arc<Scripted>) {
        let p = Arc::new(Scripted {
            calls: AtomicUsize::new(0),
            answer,
        });
        (
            Rung {
                mode,
                provider: p.clone(),
            },
            p,
        )
    }

    fn request() -> ModelRequest {
        ModelRequest::new("m", vec![])
    }

    #[tokio::test]
    async fn a_refused_request_steps_down_once_and_stays_down() {
        let (a, pa) = rung(StructuredOutput::Schema, Err(http(500)));
        let (b, pb) = rung(StructuredOutput::JsonObject, Ok(()));
        let (c, pc) = rung(StructuredOutput::None, Ok(()));
        let ladder = StructuredFallback::new(vec![a, b, c]);
        assert!(ladder.complete(request()).await.is_ok());
        assert!(ladder.complete(request()).await.is_ok());
        // The refusing rung was tried exactly once; the next call went straight to the rung below.
        assert_eq!(pa.calls.load(Ordering::SeqCst), 1);
        assert_eq!(pb.calls.load(Ordering::SeqCst), 2);
        assert_eq!(pc.calls.load(Ordering::SeqCst), 0);
        let report = ladder.report();
        assert_eq!(report["used"], "json-object");
        assert_eq!(report["steps"].as_array().unwrap().len(), 1);
        assert_eq!(report["steps"][0]["status"], 500);
    }

    #[tokio::test]
    async fn the_ladder_is_bounded_and_ends_in_the_last_rungs_error() {
        let (a, _) = rung(StructuredOutput::Schema, Err(http(400)));
        let (b, _) = rung(StructuredOutput::JsonObject, Err(http(422)));
        let (c, pc) = rung(StructuredOutput::None, Err(http(400)));
        let ladder = StructuredFallback::new(vec![a, b, c]);
        assert!(ladder.complete(request()).await.is_err());
        assert_eq!(pc.calls.load(Ordering::SeqCst), 1);
        assert_eq!(ladder.report()["steps"].as_array().unwrap().len(), 2);
        // Another call does not climb back or step further.
        assert!(ladder.complete(request()).await.is_err());
        assert_eq!(ladder.report()["steps"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn only_a_rejected_request_steps_down() {
        for (error, why) in [
            (http(401), "authentication"),
            (http(429), "rate limit"),
            (http(503), "unavailable"),
            (FxError::Timeout("slow".into()), "timeout"),
            (FxError::Provider("reset".into()), "transport"),
        ] {
            let (a, _) = rung(StructuredOutput::Schema, Err(error));
            let (b, pb) = rung(StructuredOutput::None, Ok(()));
            let ladder = StructuredFallback::new(vec![a, b]);
            assert!(ladder.complete(request()).await.is_err(), "{why}");
            assert_eq!(pb.calls.load(Ordering::SeqCst), 0, "{why}");
            assert!(ladder.report()["steps"].as_array().unwrap().is_empty());
        }
    }
}
