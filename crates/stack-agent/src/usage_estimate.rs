//! Token counts for a server that does not report them, labelled as estimates.
//!
//! Some OpenAI-compatible servers (llama-cpp-python's streaming API among them) send no `usage`, and
//! the report then carries `null`. With `--estimate-usage`, a call whose reply carried no usage at all
//! is counted by a fixed rule, one token per four bytes of the text sent and of the text received, and
//! the totals are reported as `usage_estimate` with the method named. The estimate never replaces or
//! fills in the provider's own field: `usage` stays `null` for such a call, and a call that did report
//! usage is never estimated. It is a rough size, not a tokenizer's count.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse};
use serde_json::{Value, json};

pub const METHOD: &str = "bytes/4 (one token per four bytes of text; not a tokenizer)";

#[derive(Debug, Default)]
pub struct UsageEstimates {
    input: AtomicU64,
    output: AtomicU64,
    calls: AtomicU64,
}

impl UsageEstimates {
    pub fn totals(&self) -> (u64, u64) {
        (
            self.input.load(Ordering::SeqCst),
            self.output.load(Ordering::SeqCst),
        )
    }

    pub fn report(&self) -> Value {
        let (input, output) = self.totals();
        json!({
            "estimated": true,
            "method": METHOD,
            "calls_estimated": self.calls.load(Ordering::SeqCst),
            "input_tokens": input,
            "output_tokens": output,
        })
    }
}

pub struct Estimating {
    pub inner: Arc<dyn ModelProvider>,
    pub estimates: Arc<UsageEstimates>,
}

fn tokens(bytes: usize) -> u64 {
    (bytes as u64).div_ceil(4)
}

#[async_trait]
impl ModelProvider for Estimating {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        let sent: usize = request.messages.iter().map(|m| m.content.len()).sum();
        let response = self.inner.complete(request).await?;
        let usage = &response.usage;
        if usage.prompt_tokens.is_none() && usage.completion_tokens.is_none() {
            self.estimates
                .input
                .fetch_add(tokens(sent), Ordering::SeqCst);
            self.estimates
                .output
                .fetch_add(tokens(response.output.len()), Ordering::SeqCst);
            self.estimates.calls.fetch_add(1, Ordering::SeqCst);
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chip_inference::{Message, MessageRole, Usage};

    struct Fixed(Usage);

    #[async_trait]
    impl ModelProvider for Fixed {
        async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
            Ok(ModelResponse::new("m", "x".repeat(40), self.0.clone()))
        }
    }

    fn request() -> ModelRequest {
        ModelRequest::new("m", vec![Message::new(MessageRole::User, "y".repeat(100))])
    }

    #[tokio::test]
    async fn only_a_call_without_any_reported_usage_is_estimated_and_its_usage_stays_absent() {
        let est = Arc::new(UsageEstimates::default());
        let none = Estimating {
            inner: Arc::new(Fixed(Usage::default())),
            estimates: est.clone(),
        };
        let r = none.complete(request()).await.unwrap();
        assert_eq!(
            r.usage.prompt_tokens, None,
            "the provider's field is never filled in"
        );
        assert_eq!(est.totals(), (25, 10));
        let reported = Estimating {
            inner: Arc::new(Fixed(Usage::new(3, 2))),
            estimates: est.clone(),
        };
        reported.complete(request()).await.unwrap();
        assert_eq!(est.totals(), (25, 10), "a reported call is not estimated");
        assert_eq!(est.report()["calls_estimated"], 1);
    }
}
