//! The remote backend carries the idempotency key to the worker and the worker's declaration back,
//! and adds nothing: no key it was not given, no declaration the worker did not make.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chip_core::{
    CapabilityDescriptor, CapabilityId, CapabilityProvider, ExecutionId, ExecutionRequest,
    ExecutionResult, Executor, Idempotency,
};
use chip_remote_env::{
    CommandOutput, CommandRunner, RemoteCapabilityBackend, Request, Response, RunnerError,
    WORKER_ENV, WorkerCommand,
};

/// A fake environment: records each request it is sent and answers it.
struct Fake {
    declared: Idempotency,
    seen: Mutex<Vec<Request>>,
}

#[async_trait::async_trait]
impl CommandRunner for Fake {
    async fn run(
        &self,
        _argv: Vec<String>,
        env: BTreeMap<String, String>,
    ) -> Result<CommandOutput, RunnerError> {
        let request = Request::decode(&env[WORKER_ENV]).unwrap();
        self.seen.lock().unwrap().push(request.clone());
        let response = match request {
            Request::Capabilities => Response::Capabilities(vec![
                CapabilityDescriptor::new(CapabilityId::new("world.notify").unwrap(), "N", "d")
                    .with_idempotency(self.declared),
            ]),
            Request::Execute { execution_id, .. } => Response::Executed(Ok(
                ExecutionResult::success(ExecutionId::new(execution_id), "sent"),
            )),
            other => panic!("unexpected {other:?}"),
        };
        Ok(CommandOutput {
            exit_code: Some(0),
            stdout: response.encode(),
            stderr: String::new(),
        })
    }
}

#[tokio::test]
async fn the_key_reaches_the_worker_only_when_there_is_one() {
    for declared in [Idempotency::Deduplicated, Idempotency::NotCarried] {
        let fake = Arc::new(Fake {
            declared,
            seen: Mutex::new(Vec::new()),
        });
        let backend = RemoteCapabilityBackend::connect(
            fake.clone(),
            WorkerCommand {
                program: "stack-agent".into(),
                root: ".".into(),
            },
        )
        .await
        .unwrap();
        // The worker's declaration, unchanged.
        assert_eq!(
            backend.capabilities().await.unwrap()[0].idempotency,
            declared
        );

        let request = ExecutionRequest::new(ExecutionId::new("e-1"), "world.notify");
        backend
            .execute(request.clone().with_idempotency_key("ak1.k"))
            .await
            .unwrap();
        backend.execute(request).await.unwrap();
        let keys: Vec<Option<String>> = fake
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|r| match r {
                Request::Execute {
                    idempotency_key, ..
                } => Some(idempotency_key.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(keys, vec![Some("ak1.k".to_string()), None], "{declared:?}");
    }
}
