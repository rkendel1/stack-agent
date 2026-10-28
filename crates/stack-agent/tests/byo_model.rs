//! A host that embeds this crate supplies the model call itself (its own gateway holds the key).

use std::sync::{Arc, Mutex};

use chip_cli::local_environment::LocalEnvironmentProvider;
use chip_cli::software_work::{GoalKind, Identity, WorkRuntime};
use chip_core::{Environments, WorkId, WorkLimits, WorkOutcome};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};

struct Gateway {
    seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ModelProvider for Gateway {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        self.seen.lock().unwrap().push(
            request
                .messages
                .iter()
                .map(|m| m.content.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        Ok(ModelResponse::new(
            "g",
            r#"{"decision":"block","reason":"gateway says stop"}"#.to_string(),
            Usage::new(1, 1),
        ))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hosts_model_call_drives_the_work_and_its_reply_is_validated_like_any_other() {
    let dir = std::env::temp_dir().join(format!("chip-byo-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
    let gateway = Arc::new(Gateway {
        seen: Mutex::new(Vec::new()),
    });
    let runtime = WorkRuntime::with_provider(
        gateway.clone(),
        Identity {
            provider: "host-gateway".into(),
            model: "gw-model".into(),
            endpoint: "host://gateway".into(),
            credential: "held by the host".into(),
        },
    );
    let provider = LocalEnvironmentProvider::prepare(&dir, false)
        .await
        .unwrap();
    let env = Environments::new(Arc::new(provider))
        .acquire(WorkId::new("byo"))
        .await
        .unwrap();
    let (work, _) = runtime
        .run(
            WorkId::new("byo"),
            "Look around.",
            GoalKind::Inspect,
            WorkLimits {
                max_turns: 2,
                max_executions: 2,
            },
            None,
            env.environment(),
        )
        .await;
    assert_eq!(gateway.seen.lock().unwrap().len(), 1);
    assert!(
        matches!(work.report.outcome, WorkOutcome::Blocked { .. }),
        "{:?}",
        work.report.outcome
    );
    assert_eq!(work.identity.as_ref().unwrap().provider, "host-gateway");
    assert!(!work.verified);
}
