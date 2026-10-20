use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chip_core::{
    Agent, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CapabilityRequest, ExecutionError, ExecutionId,
    ExecutionRequest, ExecutionResult, Executor, InputValue,
};
use chip_experiments::execute_capability_limited;
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse};

struct NoModel;

#[async_trait::async_trait]
impl ModelProvider for NoModel {
    async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
        Err(FxError::Provider("no model".into()))
    }
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut d = CapabilityDescriptor::new(CapabilityId::new("x.op")?, "Op", "an op");
        for n in ["a", "b", "c"] {
            d.inputs.push(CapabilityInput {
                name: n.into(),
                description: n.into(),
                required: false,
            });
        }
        Ok(vec![d])
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

struct Count(AtomicUsize);

#[async_trait::async_trait]
impl Executor for Count {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ExecutionResult::success(r.id, "ran"))
    }
}

fn rig() -> (Agent, Arc<Count>) {
    let count = Arc::new(Count(AtomicUsize::new(0)));
    let agent = Agent::new(Arc::new(NoModel))
        .with_capabilities(Arc::new(Caps))
        .with_executor(count.clone());
    (agent, count)
}

fn request(inputs: &[&str]) -> CapabilityRequest {
    let mut r = CapabilityRequest::new(ExecutionId::new("e"), CapabilityId::new("x.op").unwrap());
    for name in inputs {
        r = r.with_input(*name, InputValue::Integer(1));
    }
    r
}

#[tokio::test]
async fn more_inputs_than_the_limit_is_refused_and_nothing_runs() {
    let (agent, count) = rig();
    let err = execute_capability_limited(&agent, &request(&["a", "b", "c"]), 2).await
        .unwrap_err();
    assert!(matches!(err, CapabilityError::InvalidInput(_)), "{err:?}");
    assert_eq!(count.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn up_to_the_limit_it_behaves_as_execute_capability() {
    let (agent, count) = rig();
    let report = execute_capability_limited(&agent, &request(&["a", "b"]), 2).await.unwrap();
    assert_eq!(report.result.unwrap().output, "ran");
    assert_eq!(count.0.load(Ordering::SeqCst), 1);
    let report = execute_capability_limited(&agent, &request(&[]), 0).await.unwrap();
    assert!(report.result.is_ok());
    assert_eq!(count.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_limit_of_zero_refuses_any_input() {
    let (agent, count) = rig();
    assert!(execute_capability_limited(&agent, &request(&["a"]), 0).await.is_err());
    assert_eq!(count.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn ordinary_validation_still_applies_first() {
    let (agent, count) = rig();
    let err = execute_capability_limited(&agent, &request(&["not_declared"]), 5).await
        .unwrap_err();
    assert!(matches!(err, CapabilityError::InvalidInput(_)), "{err:?}");
    assert_eq!(count.0.load(Ordering::SeqCst), 0);
}
