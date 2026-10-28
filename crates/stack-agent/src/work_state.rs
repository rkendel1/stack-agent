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

/// A goal as a FeltDB task objective, within the agent-transition contract's text limit
/// (`MAX_TEXT_BYTES`, 1,024 bytes).
///
/// A goal within the limit is the objective, unchanged (so every task recorded before this rule
/// reads exactly as it did). A longer goal is represented by its beginning, cut at a character and
/// marked, and a stable reference to the whole goal: its SHA-256 digest and its length in bytes.
/// The digest makes the objective name one goal exactly (two goals that share their first kilobyte
/// still have different objectives, so `begin` refuses to continue one task as the other), and it
/// is reproducible from the goal alone, so a continuation (`--task-id`) or a service recovery that
/// is given the same goal computes the same objective. A digest cannot reconstruct a goal: the
/// whole goal, byte for byte, is in the task's admission record (`chip.Admission`, up to
/// `MAX_ADMISSION_GOAL_BYTES`), written before the task is created, under the same task id.
pub fn objective(goal: &str) -> String {
    let goal = goal.trim();
    let max = chip_agent_state::feltdb::agent_transition_contract::MAX_TEXT_BYTES;
    if goal.len() <= max {
        return goal.to_string();
    }
    let digest: String = Sha256::digest(goal.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let reference = format!(
        "… [goal sha256:{digest}, {} bytes; the whole goal is in chip.Admission]",
        goal.len()
    );
    let mut end = max - reference.len();
    while !goal.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{reference}", &goal[..end])
}

/// The objective earlier releases recorded for a goal over the limit: its first 1,021 bytes and
/// `…`, with no reference to the rest. Only ever compared with, never written.
pub fn legacy_objective(goal: &str) -> String {
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

/// The objective to name task `task_id` by when it is begun with `goal`: [`objective`], except for
/// a task an earlier release recorded with the [`legacy_objective`] of this same goal, which keeps
/// the objective it was created with so it can still be continued. Nothing is rewritten.
pub fn objective_for(store: &AgentStore, task_id: &str, goal: &str) -> String {
    let current = objective(goal);
    let existing = store
        .tasks()
        .ok()
        .and_then(|tasks| tasks.into_iter().find(|t| t.task_id == task_id));
    match existing {
        Some(task) if task.objective != current && task.objective == legacy_objective(goal) => {
            task.objective
        }
        _ => current,
    }
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

/// The project's episode scope: `path:` and the first 16 hex characters of the `sha256` of its
/// canonical path, the token its default database is named after. Never the path itself. A
/// project that is moved starts with no episodes.
pub fn project_scope(project_root: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(project_root).ok()?;
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let token: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("path:{token}"))
}

/// The digest of what a capability declares: its id, version, inputs, effect, idempotency and
/// evidence reuse. A capability whose contract changes makes the episodes that executed it stale.
pub fn contract_digest(descriptor: &chip_core::CapabilityDescriptor) -> String {
    let declared = format!(
        "{}\0{:?}\0{:?}\0{:?}\0{:?}\0{:?}\0{}",
        descriptor.id,
        descriptor.version,
        descriptor.inputs,
        descriptor.max_input_bytes,
        descriptor.effect,
        descriptor.idempotency,
        descriptor.reuse_evidence
    );
    let digest = Sha256::digest(declared.as_bytes());
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("c1.{hex}")
}

/// The conditions a work in `project_root` runs under, as episodes record and recall compares
/// them: build systems, whether tests are present, the PAX binary the work would use (a digest;
/// nothing is started to find it) and the runtime line.
pub fn episode_conditions(
    project_root: &Path,
    profile: &crate::autoconfig::profile::ProjectProfile,
) -> std::collections::BTreeMap<String, String> {
    use crate::autoconfig::profile::Attr;
    let tests = match &profile.has_tests {
        Attr::Known { value, .. } => value.to_string(),
        _ => "unknown".to_string(),
    };
    let build_systems: Vec<&str> = profile.build_systems.iter().map(|b| b.name()).collect();
    // The verifier is identified by the digest of the PAX binary the work would use. Nothing is
    // started to find it: a work that never needs PAX starts no PAX process, and the environment
    // learns PAX's version only once a test runs, too late to compare against.
    let verifier = chip_pax::PaxExecutor::new(project_root)
        .locate()
        .ok()
        .and_then(|path| std::fs::read(path).ok())
        .map_or_else(
            || "unavailable".to_string(),
            |bytes| {
                let digest = Sha256::digest(&bytes);
                let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
                format!("sha256:{hex}")
            },
        );
    [
        ("build_systems", build_systems.join(",")),
        ("tests", tests),
        ("verifier", verifier),
        ("test_definitions", test_definitions(project_root)),
        ("runtime_line", crate::learning::runtime_line()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// The most entries [`test_definitions`] visits, and the most bytes it reads, before it gives up
/// and says `unknown`.
const MAX_TEST_SCAN_ENTRIES: usize = 20_000;
const MAX_TEST_SCAN_BYTES: u64 = 64 * 1024 * 1024;

/// A digest of the project's test definitions: every file the runtime's test-file rule
/// ([`crate::software_work::is_test_path`], the rule that refuses tampered test evidence) names,
/// by path and content. `none` when the project has no such file; `unknown` when the project is
/// too large to read within the bounds. Version control, build output, dependencies and hidden
/// directories are skipped, and links are not followed. Nothing is started.
///
/// A verified episode passed the tests the project had then. When these change, it no longer
/// describes this project's verification.
pub fn test_definitions(project_root: &Path) -> String {
    let mut found: Vec<(String, [u8; 32])> = Vec::new();
    let (mut entries, mut bytes) = (0usize, 0u64);
    let mut stack = vec![(project_root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            entries += 1;
            if entries > MAX_TEST_SCAN_ENTRIES {
                return "unknown".into();
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                if !name.starts_with('.') && !matches!(name.as_str(), "target" | "node_modules") {
                    stack.push((entry.path(), path));
                }
            } else if meta.is_file() && crate::software_work::is_test_path(&path) {
                bytes += meta.len();
                if bytes > MAX_TEST_SCAN_BYTES {
                    return "unknown".into();
                }
                let Ok(content) = std::fs::read(entry.path()) else {
                    return "unknown".into();
                };
                found.push((path, Sha256::digest(&content).into()));
            }
        }
    }
    if found.is_empty() {
        return "none".into();
    }
    found.sort();
    let mut hasher = Sha256::new();
    for (path, digest) in &found {
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(digest);
    }
    let hex: String = hasher.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// What each capability `capabilities` offers declares now, as workflow validation compares it:
/// its contract digest and its inputs.
pub async fn declared_capabilities(
    capabilities: &dyn chip_core::CapabilityProvider,
) -> std::collections::BTreeMap<String, chip_agent_state::Declared> {
    capabilities
        .capabilities()
        .await
        .unwrap_or_default()
        .iter()
        .map(|d| {
            (
                d.id.to_string(),
                chip_agent_state::Declared {
                    contract: contract_digest(d),
                    inputs: d
                        .inputs
                        .iter()
                        .map(|i| (i.name.clone(), i.required))
                        .collect(),
                },
            )
        })
        .collect()
}

/// What episodes need to know about a work that FeltDB's history does not hold: its project, kind,
/// goal terms (as digests), the conditions it runs under and the contracts of the capabilities it
/// is offered. `None` when the project has no scope (its root cannot be resolved): then nothing is
/// recalled or recorded.
pub async fn episode_context(
    project_root: &Path,
    kind_name: &str,
    goal: &str,
    profile: &crate::autoconfig::profile::ProjectProfile,
    capabilities: &dyn chip_core::CapabilityProvider,
) -> Option<chip_agent_state::EpisodeContext> {
    let project = project_scope(project_root)?;
    let contracts = capabilities
        .capabilities()
        .await
        .unwrap_or_default()
        .iter()
        .map(|d| (d.id.to_string(), contract_digest(d)))
        .collect();
    let conditions = episode_conditions(project_root, profile);
    Some(chip_agent_state::EpisodeContext {
        project,
        kind: kind_name.to_string(),
        intent_terms: chip_agent_state::intent_terms(goal),
        conditions,
        contracts,
        recorded_by: format!("stack-agent {}", env!("CARGO_PKG_VERSION")),
    })
}

/// The project profile episodes describe a work's conditions with: file names and `PATH` lookups
/// only, nothing started (the same profile `work` collects for learning).
pub fn project_profile(
    project_root: &Path,
    provider: &str,
    model: &str,
) -> crate::autoconfig::profile::ProjectProfile {
    use crate::autoconfig::profile::{ProfileInputs, ProjectProfile, on_path};
    ProjectProfile::collect(&ProfileInputs {
        root: project_root.to_path_buf(),
        pax: chip_pax::PaxExecutor::new(project_root)
            .locate()
            .map_err(|e| e.to_string()),
        git_on_path: on_path("git"),
        model: Ok((provider.to_string(), model.to_string())),
    })
}

type Written<T> = Result<(T, chip_agent_state::EpisodeWrite), chip_agent_state::StateError>;

/// What [`Episodes::record`] wrote: the work's episode, and feedback about each episode that was
/// recalled for it (none when the episode could not be written: feedback is derived from it).
pub struct Recorded {
    episode: Written<chip_agent_state::EpisodeRecord>,
    feedback: Vec<Written<chip_agent_state::FeedbackRecord>>,
    /// Workflow candidates extracted after the episode (none when it could not be written).
    workflows: Option<Result<chip_agent_state::Extracted, chip_agent_state::StateError>>,
}

/// Episodic memory for one durable work, the same for `work` and `serve`: recall once the task is
/// begun and before the work runs, and an episode once `finish_verified` has fixed the outcome.
/// Neither step changes the work's outcome or exit status, and neither runs without durable state.
pub struct Episodes {
    context: Option<chip_agent_state::EpisodeContext>,
    recall: Option<chip_agent_state::Recall>,
    /// Why recall was not attempted (`--no-recall`, no project scope).
    off: Option<String>,
}

impl Recorded {
    /// The work's own episode, when it was written (or was already there).
    pub fn episode(&self) -> Option<&chip_agent_state::EpisodeRecord> {
        self.episode.as_ref().ok().map(|(e, _)| e)
    }
}

impl Episodes {
    /// Nothing recalled and nothing to record, with the reason (a work with no project identity).
    pub fn unavailable(why: &str) -> Self {
        Self {
            context: None,
            recall: None,
            off: Some(why.to_string()),
        }
    }

    /// Describes the begun `work` and, unless `recall` is false, recalls for it: what is recalled
    /// is then in `work.recalled()`, which the spec takes (`WorkSpec::with_recalled`).
    pub async fn prepare(
        work: &chip_agent_state::DurableWork,
        project_root: &Path,
        kind_name: &str,
        goal: &str,
        profile: &crate::autoconfig::profile::ProjectProfile,
        capabilities: &dyn chip_core::CapabilityProvider,
        recall: bool,
    ) -> Self {
        let context = episode_context(project_root, kind_name, goal, profile, capabilities).await;
        let (recall, off) = match (&context, recall) {
            (_, false) => (None, Some("--no-recall".to_string())),
            (Some(context), true) => (Some(work.recall(context)), None),
            (None, true) => (None, Some("the project has no episode scope".to_string())),
        };
        Self {
            context,
            recall,
            off,
        }
    }

    /// Records the work's episode from what `finish_verified` committed. A failure is said on
    /// stderr and in the report, and changes nothing else.
    pub fn record(
        &self,
        durable: &chip_agent_state::DurableWork,
        finished: &chip_agent_state::Finished,
    ) -> Option<Recorded> {
        let context = self.context.as_ref()?;
        let episode = durable.record_episode(finished, context);
        let feedback = match &episode {
            Ok((own, _)) => durable.record_feedback(own),
            Err(why) => {
                eprintln!("note: episode not recorded: {why}");
                Vec::new()
            }
        };
        for why in feedback.iter().filter_map(|f| f.as_ref().err()) {
            eprintln!("note: episode feedback not recorded: {why}");
        }
        // Workflow candidates, proposed from the project's verified episodes. Like the episode and
        // its feedback, a failure here is reported and changes nothing else.
        let workflows = episode
            .is_ok()
            .then(|| durable.extract_workflows(&context.project, &context.recorded_by));
        if let Some(Err(why)) = &workflows {
            eprintln!("note: workflow candidates not extracted: {why}");
        }
        Some(Recorded {
            episode,
            feedback,
            workflows,
        })
    }

    /// The `episodes` section of a work's report: what was recalled for it, and the episode its
    /// outcome was recorded as.
    pub fn json(&self, recorded: Option<&Recorded>) -> serde_json::Value {
        serde_json::json!({
            "recall": match (&self.off, &self.recall) {
                (Some(why), _) => serde_json::json!({"enabled": false, "reason": why}),
                (None, Some(recall)) => recall.to_json(),
                (None, None) => serde_json::Value::Null,
            },
            "feedback": recorded.map_or_else(Vec::new, |r| r.feedback.iter().map(|f| match f {
                Ok((f, written)) => serde_json::json!({
                    "episode": f.episode,
                    "classification": f.classification.as_str(),
                    "attempted": f.attempted,
                    "verification_after": f.verification_after,
                    "corrective_writes": f.corrective_writes,
                    "basis": f.basis,
                    "record": format!("{}:{}", chip_agent_state::FEEDBACK_COLLECTION, f.id),
                    "already_recorded": *written == chip_agent_state::EpisodeWrite::AlreadyRecorded,
                }),
                Err(why) => serde_json::json!({"error": why.to_string()}),
            }).collect::<Vec<_>>()),
            "workflows": match recorded.and_then(|r| r.workflows.as_ref()) {
                Some(Ok(done)) => serde_json::json!({
                    "written": done.written,
                    "already_recorded": done.already_recorded.len(),
                    "rejected": done.rejected.iter().fold(
                        std::collections::BTreeMap::<&str, usize>::new(),
                        |mut m, r| {
                            *m.entry(r.code).or_default() += 1;
                            m
                        },
                    ),
                }),
                Some(Err(why)) => serde_json::json!({"error": why.to_string()}),
                None => serde_json::Value::Null,
            },
            "recorded": match recorded.map(|r| &r.episode) {
                Some(Ok((episode, written))) => serde_json::json!({
                    "episode": episode.id,
                    "outcome": episode.outcome.as_str(),
                    "basis": episode.outcome_basis,
                    "already_recorded": *written == chip_agent_state::EpisodeWrite::AlreadyRecorded,
                }),
                Some(Err(why)) => serde_json::json!({"error": why.to_string()}),
                None => serde_json::Value::Null,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chip_agent_state::feltdb::agent_transition_contract::MAX_TEXT_BYTES;
    use chip_agent_state::{AdmissionOrigin, Begun, DurableConfig, StateError};

    /// A goal the size of the T4 obstacle's (1,829 bytes), multibyte characters included.
    fn long_goal(tail: &str) -> String {
        let mut goal =
            "Fix the resolver so multi-version providers resolve to the highest served version — "
                .repeat(21);
        goal.push_str(tail);
        assert!(
            goal.len() > MAX_TEXT_BYTES && goal.len() <= 2000,
            "{}",
            goal.len()
        );
        goal
    }

    fn store() -> (tempfile::TempDir, AgentStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("state.feltdb")).unwrap();
        (dir, store)
    }

    fn start(store: &AgentStore, task: &str, goal: &str) -> Result<Begun, StateError> {
        let config = DurableConfig::default();
        let objective = objective_for(store, task, goal);
        chip_agent_state::ensure_admitted(
            store,
            task,
            "change",
            goal.trim(),
            &objective,
            AdmissionOrigin::Cli,
            &config,
        )?;
        std::thread::sleep(chip_agent_state::ADMISSION_HANDOFF);
        chip_agent_state::begin(store, task, &objective, &config)
    }

    #[test]
    fn a_goal_within_the_limit_is_its_own_objective() {
        assert_eq!(objective("  fix the test \n"), "fix the test");
        let exact = "x".repeat(MAX_TEXT_BYTES);
        assert_eq!(objective(&exact), exact);
        assert_eq!(legacy_objective(&exact), exact);
    }

    #[test]
    fn a_longer_goal_is_bounded_with_a_stable_digest_and_a_reference_to_the_whole_goal() {
        let goal = long_goal("A");
        let o = objective(&goal);
        assert!(o.len() <= MAX_TEXT_BYTES, "{}", o.len());
        assert_eq!(o, objective(&goal), "stable");
        let digest: String = Sha256::digest(goal.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(o.ends_with(&format!(
            "… [goal sha256:{digest}, {} bytes; the whole goal is in chip.Admission]",
            goal.len()
        )));
        assert!(goal.starts_with(o.split('…').next().unwrap()));
        // Two goals sharing their first kilobyte have different objectives; the old rule gave them
        // the same one.
        let other = long_goal("B");
        assert_ne!(objective(&goal), objective(&other));
        assert_eq!(legacy_objective(&goal), legacy_objective(&other));
    }

    #[test]
    fn an_oversized_goal_is_admitted_begun_and_continued_without_a_contract_failure() {
        let (_dir, store) = store();
        let goal = long_goal("A");
        // What strict runs used to send: the whole goal, refused by FeltDB's contract.
        let refused =
            chip_agent_state::begin(&store, "work_full", goal.trim(), &DurableConfig::default());
        assert!(refused.is_err());
        assert!(matches!(
            start(&store, "work_a", &goal),
            Ok(Begun::Ready(_))
        ));
        // The whole goal is recoverable from the admission record, byte for byte.
        match chip_agent_state::admission(&store, "work_a") {
            chip_agent_state::Admission::Valid(record) => assert_eq!(record.goal, goal.trim()),
            other => panic!("{other:?}"),
        }
        // Another goal that shares the first kilobyte cannot continue this task.
        let other = long_goal("B");
        let config = DurableConfig::default();
        assert!(matches!(
            chip_agent_state::begin(
                &store,
                "work_a",
                &objective_for(&store, "work_a", &other),
                &config
            ),
            Err(StateError::ObjectiveMismatch { .. })
        ));
    }

    #[test]
    fn a_task_recorded_with_the_old_cut_objective_keeps_it() {
        let (_dir, store) = store();
        let goal = long_goal("A");
        let config = DurableConfig::default();
        chip_agent_state::admit(
            &store,
            "work_old",
            "change",
            goal.trim(),
            &legacy_objective(&goal),
            AdmissionOrigin::Cli,
            &config,
        )
        .unwrap();
        std::thread::sleep(chip_agent_state::ADMISSION_HANDOFF);
        assert_eq!(
            objective_for(&store, "work_old", &goal),
            legacy_objective(&goal)
        );
        assert!(matches!(
            start(&store, "work_old", &goal),
            Ok(Begun::Ready(_))
        ));
        // A new task gets the digest form.
        assert_eq!(objective_for(&store, "work_new", &goal), objective(&goal));
    }
}
