//! Where a work's durable state lives, what a work is called, and how the host's verdict is put
//! into FeltDB.
//!
//! Every `work` and every `serve` work is persisted through FeltDB (`chip-agent-state`) unless
//! `--no-state` says otherwise: its intents, observations and result, as before, and now every
//! event of its trajectory (`chip.WorkEvent`, with its provenance) and the host's verification
//! verdict (`chip.work_result.v2`).
//!
//! * The database is `--state-db FILE` when given. Otherwise it is the project's own database under
//!   the runtime data directory, `<data>/agent-state/<project token>.feltdb`, where the token is a
//!   hash of the project's canonical path (so the path is not in the file name).
//! * Each work has its own id, `work_<16 hex>` from the operating system's random source, which is
//!   also its FeltDB task id unless `--task-id` names an existing task to resume. Before, every CLI
//!   work was called `work`, so its execution ids repeated from run to run.
//! * A database that was asked for (`--state-db`) and cannot be used stops the work before it
//!   starts. The default database is a record, like learning: if it cannot be used, the work runs
//!   and says it was not persisted.

use std::path::{Path, PathBuf};

use chip_agent_state::{AgentStore, TaskValue, Verification};
use sha2::{Digest, Sha256};

use crate::software_work::SoftwareWork;

/// A new work id: `work_` and 64 random bits as hex. Never derived from the goal, the time alone
/// or a counter, so two processes (or two runs of one) never share an id.
pub fn new_work_id() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 8];
    let random = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if random.is_err() {
        // No random source (not Unix): the clock, the process and a counter, hashed.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let mut hash = Sha256::new();
        hash.update(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
                .to_le_bytes(),
        );
        hash.update(std::process::id().to_le_bytes());
        hash.update(
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                .to_le_bytes(),
        );
        bytes.copy_from_slice(&hash.finalize()[..8]);
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("work_{hex}")
}

/// The project's default database: `<data>/agent-state/<token>.feltdb`. `Err` says why there is
/// none (no runtime data directory).
pub fn default_database(
    project_root: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<PathBuf, String> {
    let (_, base) = crate::learning::store::Store::runtime(env)?;
    let canonical = std::fs::canonicalize(project_root)
        .map_err(|e| format!("the project root cannot be resolved ({})", e.kind()))?;
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let token: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    Ok(base.join("agent-state").join(format!("{token}.feltdb")))
}

/// Opens the default database, creating its private directory first.
pub fn open_default(path: &Path) -> Result<AgentStore, String> {
    if let Some(dir) = path.parent() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
            .create(dir)
            .map_err(|e| format!("the state directory cannot be created ({})", e.kind()))?;
    }
    AgentStore::open(path).map_err(|e| e.to_string())
}

/// A goal as a FeltDB objective (at most the contract's 1024 bytes, cut at a character).
pub fn objective(goal: &str) -> String {
    let goal = goal.trim();
    let max = chip_agent_state::feltdb::agent_transition_contract::MAX_TEXT_BYTES;
    if goal.len() <= max {
        return goal.to_string();
    }
    let mut end = max - "…".len();
    while !goal.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &goal[..end])
}

/// Tasks of this database that are still `running` but are not `current`: their process ended
/// before the work finished. A running work holds the project lock, so in this project no other
/// process is running one now.
pub fn interrupted(store: &AgentStore, current: &str) -> Vec<TaskValue> {
    let mut tasks: Vec<TaskValue> = store
        .tasks()
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.status == "running" && t.task_id != current)
        .collect();
    tasks.sort_by_key(|t| std::cmp::Reverse(t.updated_at_ms));
    tasks
}

pub fn interrupted_json(tasks: &[TaskValue]) -> serde_json::Value {
    serde_json::Value::Array(
        tasks
            .iter()
            .map(|t| {
                serde_json::json!({
                    "task_id": t.task_id,
                    "unresolved_intents": t.unresolved_intents,
                    "updated_at_ms": t.updated_at_ms,
                })
            })
            .collect(),
    )
}

/// One stderr line per interrupted work, so a restart never starts blind (audit HA-16).
pub fn report_interrupted(tasks: &[TaskValue]) {
    for t in tasks {
        eprintln!(
            "note: work {} was interrupted before it finished ({} execution(s) with no recorded outcome); its effects may be in the project (`git status`). Inspect it with `stack-agent agent-state show --task-id {}`.",
            t.task_id, t.unresolved_intents, t.task_id
        );
    }
}

/// The host's verdict on a finished work, as FeltDB records it.
pub fn verification(work: &SoftwareWork) -> Verification {
    let exit_status = work.exit_status();
    Verification {
        accepted: exit_status == crate::verify::EXIT_VERIFIED,
        verified: work.verified,
        goal_satisfied: work.goal_satisfied,
        grounded: work.grounded,
        evidence_invalidated: work.evidence_invalidated.clone(),
        exit_status,
    }
}
