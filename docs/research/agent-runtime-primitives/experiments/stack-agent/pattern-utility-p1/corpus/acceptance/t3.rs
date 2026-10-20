use std::sync::Arc;

use chip_core::{
    Agent, CapabilityError, CapabilityId, CapabilityProvider, CapabilityRequest, EchoBackend,
    ExecutionId, InputValue,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse};

struct NoModel;

#[async_trait::async_trait]
impl ModelProvider for NoModel {
    async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
        Err(FxError::Provider("no model".into()))
    }
}

fn agent() -> Agent {
    Agent::new(Arc::new(NoModel))
        .with_capabilities(Arc::new(EchoBackend))
        .with_executor(Arc::new(EchoBackend))
}

fn say(text: &str) -> CapabilityRequest {
    CapabilityRequest::new(ExecutionId::new("e"), CapabilityId::new("echo.say").unwrap())
        .with_input("text", InputValue::Text(text.into()))
}

#[tokio::test]
async fn it_declares_one_capability_with_one_required_input() {
    let declared = EchoBackend.capabilities().await.unwrap();
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].id.as_str(), "echo.say");
    assert_eq!(declared[0].inputs.len(), 1);
    assert_eq!(declared[0].inputs[0].name, "text");
    assert!(declared[0].inputs[0].required);
    assert!(!declared[0].reuse_evidence);
}

#[tokio::test]
async fn it_echoes_the_text() {
    let report = agent().execute_capability(&say("hello")).await.unwrap();
    assert_eq!(report.result.unwrap().output, "hello");
}

#[tokio::test]
async fn a_text_of_64_bytes_passes_validation_and_65_does_not() {
    let a = agent();
    assert!(a.validate_capability_request(&say(&"x".repeat(64))).await.is_ok());
    let err = a.validate_capability_request(&say(&"x".repeat(65))).await.unwrap_err();
    assert!(matches!(err, CapabilityError::InvalidInput(_)), "{err:?}");
}

#[tokio::test]
async fn an_over_long_text_never_reaches_the_executor() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting(AtomicUsize);
    #[async_trait::async_trait]
    impl chip_core::Executor for Counting {
        async fn execute(
            &self,
            r: chip_core::ExecutionRequest,
        ) -> Result<chip_core::ExecutionResult, chip_core::ExecutionError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            EchoBackend.execute(r).await
        }
    }
    let counting = Arc::new(Counting(AtomicUsize::new(0)));
    let a = Agent::new(Arc::new(NoModel))
        .with_capabilities(Arc::new(EchoBackend))
        .with_executor(counting.clone());
    assert!(a.execute_capability(&say(&"y".repeat(65))).await.is_err());
    assert_eq!(counting.0.load(Ordering::SeqCst), 0);
    assert!(a.execute_capability(&say("ok")).await.is_ok());
    assert_eq!(counting.0.load(Ordering::SeqCst), 1);
}
