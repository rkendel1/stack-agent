//! The replay recordings are what isolated execution actually observes.
//!
//! Each committed recording is regenerated here by executing its requests through the real
//! `ProjectExecutor` over a fresh copy of the task's project, and must be identical. To rewrite the
//! committed files after a deliberate fixture change:
//! `cargo test -p chip-experiments --test recordings -- --ignored`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chip_core::{ExecutionId, ExecutionRequest, ExecutionStatus, Executor, InputValue};
use chip_experiments::plugin::{RECORDING_SCHEMA, recorded_inputs};
use chip_experiments::reference::{explicit_task, worded_task};
use chip_experiments::{RecordedExecution, Recording, TaskFixture};
use chip_project::ProjectExecutor;

fn requests(task: &TaskFixture) -> Vec<(&'static str, Vec<(&'static str, String)>)> {
    let (path, content) = task.expected_files[0].clone();
    let path: &'static str = Box::leak(path.into_boxed_str());
    vec![
        ("project.read", vec![("path", "docs/notes.md".to_string())]),
        ("project.read", vec![("path", path.to_string())]),
        ("project.search", vec![("query", "retries".to_string())]),
        (
            "project.write",
            vec![("path", path.to_string()), ("content", content)],
        ),
    ]
}

async fn record(task: &TaskFixture) -> Recording {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "chip-experiments-record-{}-{}",
        std::process::id(),
        task.id
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, content) in &task.files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
    }
    let root = dir.canonicalize().unwrap();
    let executor = ProjectExecutor::new(&root);
    let mut entries = Vec::new();
    for (i, (capability, inputs)) in requests(task).into_iter().enumerate() {
        let inputs: BTreeMap<String, InputValue> = inputs
            .into_iter()
            .map(|(k, v)| (k.to_string(), InputValue::Text(v)))
            .collect();
        let result = executor
            .execute(
                ExecutionRequest::new(ExecutionId::new(format!("r{i}")), capability)
                    .with_inputs(inputs.clone()),
            )
            .await
            .unwrap();
        assert!(
            !result.output.contains(root.to_string_lossy().as_ref()),
            "a recording must not name a host path"
        );
        entries.push(RecordedExecution {
            capability: capability.to_string(),
            inputs: recorded_inputs(&inputs),
            succeeded: result.status == ExecutionStatus::Success,
            output: result.output,
        });
    }
    std::fs::remove_dir_all(&dir).unwrap();
    Recording {
        schema: RECORDING_SCHEMA.into(),
        entries,
    }
}

fn file(task: &TaskFixture) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("fixtures/{}.recording.json", task.id))
}

#[tokio::test]
async fn each_committed_recording_is_what_execution_observes() {
    for task in [explicit_task(), worded_task()] {
        let fresh = record(&task).await;
        assert_eq!(
            task.recording.as_ref(),
            Some(&fresh),
            "{} differs from execution; regenerate with --ignored",
            task.id
        );
        assert!(fresh.entries.iter().all(|e| e.succeeded), "{}", task.id);
    }
}

#[tokio::test]
#[ignore = "rewrites the committed recordings"]
async fn regenerate_recordings() {
    for task in [explicit_task(), worded_task()] {
        let fresh = record(&task).await;
        std::fs::write(
            file(&task),
            serde_json::to_string_pretty(&fresh).unwrap() + "\n",
        )
        .unwrap();
    }
}
