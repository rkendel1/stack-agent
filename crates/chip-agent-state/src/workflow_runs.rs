//! Workflow executions: what one attempt did with the workflow candidate it was given (the
//! `chip.WorkflowExecution` collection).
//!
//! A candidate is a proposal ([`crate::WorkflowCandidate`]); an episode is what an attempt
//! established ([`crate::EpisodeRecord`]). An execution record is neither: it says which candidate an
//! attempt followed, how far, why it stopped following, whether the planner took over, and the
//! outcome the host verified. It names the evidence (observation, result and episode records)
//! instead of copying it, and holds no task text, file content or model output. One per attempt
//! (`<task>~a<attempt>~wf`), written once, after the attempt's episode.

use serde::{Deserialize, Serialize};

use super::episodes::{EpisodeOutcome, EpisodeWrite};
use super::{AgentStore, StateError, Verification, from_flow};

pub const WORKFLOW_EXECUTION_COLLECTION: &str = "chip.WorkflowExecution";
pub const WORKFLOW_EXECUTION_SCHEMA: &str = "chip.workflow_execution.v1";

/// One candidate step, as the attempt left it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutedStep {
    pub index: usize,
    pub capability: String,
    pub role: String,
    /// `completed`, `failed` (executed, with another status than the step expects), `deviated`
    /// (another action was taken in its place), `not_executed` (requested, but validation or
    /// authorization refused it, or nothing ran), or `not_reached`.
    pub status: String,
    /// Who asked for it: `local` (Chip proposed it from the candidate), `model` (the model
    /// supplied its parameters), `earlier_attempt` (restored, not performed again).
    pub decided_by: Option<String>,
    pub execution_id: Option<String>,
    pub observation_record: Option<String>,
}

/// Why the attempt stopped following the candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stopped {
    pub step: usize,
    /// `deviated`, `step_failed`, `verification_failed`, `not_executed`, `cancelled`, or
    /// `work_ended` (the work ended, by a limit or a block, before the procedure did).
    pub code: String,
    /// Chip's sentence.
    pub detail: String,
}

/// One `chip.WorkflowExecution` record. Written once, never changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowExecutionRecord {
    pub schema: String,
    /// `<task>~a<attempt>~wf`.
    pub id: String,
    /// The candidate followed, its record, and the episodes it was proposed from.
    pub candidate: String,
    pub candidate_record: String,
    pub procedure_key: String,
    pub support: Vec<String>,
    /// How it was selected: the candidates considered, those eligible, and the rule that chose.
    pub considered: usize,
    pub eligible: usize,
    pub selection_rule: String,
    pub task_id: String,
    pub attempt: u64,
    pub work_id: String,
    pub steps: Vec<ExecutedStep>,
    pub completed_steps: usize,
    /// `None` when every step completed.
    pub stopped: Option<Stopped>,
    /// The planner decided after the attempt stopped following the candidate.
    pub fallback: bool,
    /// The attempt's own outcome, as its episode records it, the host's verdict, and the records.
    pub outcome: EpisodeOutcome,
    pub verification: Option<Verification>,
    pub result_record: String,
    pub episode: String,
    pub created_at_ms: u64,
    pub recorded_by: String,
}

impl AgentStore {
    /// Writes `record` once. The same record again is [`EpisodeWrite::AlreadyRecorded`]; a
    /// different one under the same id is refused and the stored one is kept.
    pub fn record_workflow_execution(
        &self,
        record: &WorkflowExecutionRecord,
    ) -> Result<EpisodeWrite, StateError> {
        if record.schema != WORKFLOW_EXECUTION_SCHEMA {
            return Err(StateError::InvalidInput(format!(
                "workflow execution {} is not a {WORKFLOW_EXECUTION_SCHEMA} record",
                record.id
            )));
        }
        let same = |stored: WorkflowExecutionRecord| {
            if &stored == record {
                Ok(EpisodeWrite::AlreadyRecorded)
            } else {
                Err(StateError::Conflict {
                    operation: "workflow execution",
                    detail: format!(
                        "a different execution is already recorded as {}; it is kept, not replaced",
                        record.id
                    ),
                })
            }
        };
        if let Some(stored) = self.workflow_execution(&record.id)? {
            return same(stored);
        }
        let value = serde_json::to_value(record).expect("an execution record serializes");
        match self.create(WORKFLOW_EXECUTION_COLLECTION, &record.id, value) {
            Ok(()) => Ok(EpisodeWrite::Recorded),
            Err(e) => match self.workflow_execution(&record.id)? {
                Some(stored) => same(stored),
                None => Err(e),
            },
        }
    }

    pub fn workflow_execution(
        &self,
        id: &str,
    ) -> Result<Option<WorkflowExecutionRecord>, StateError> {
        let Some((_, value)) = self.read(WORKFLOW_EXECUTION_COLLECTION, id)? else {
            return Ok(None);
        };
        super::schema_of(
            WORKFLOW_EXECUTION_COLLECTION,
            id,
            &value,
            WORKFLOW_EXECUTION_SCHEMA,
        )?;
        serde_json::from_value(value)
            .map(Some)
            .map_err(|e| StateError::InvalidHistory {
                task_id: id.split('~').next().unwrap_or_default().to_string(),
                detail: format!("workflow execution {id} does not parse: {e}"),
            })
    }

    /// Every execution record, by id, and `(id, why)` for each that is not readable.
    pub fn workflow_executions(
        &self,
    ) -> Result<(Vec<WorkflowExecutionRecord>, Vec<(String, String)>), StateError> {
        let rows = self
            .db
            .list_collection(WORKFLOW_EXECUTION_COLLECTION)
            .map_err(|e| from_flow("list", e))?;
        let (mut found, mut unreadable) = (Vec::new(), Vec::new());
        for row in rows {
            let Some(id) = row
                .key
                .strip_prefix(&format!("{WORKFLOW_EXECUTION_COLLECTION}:"))
            else {
                continue;
            };
            match self.workflow_execution(id) {
                Ok(Some(r)) => found.push(r),
                Ok(None) => {}
                Err(e) => unreadable.push((id.to_string(), e.to_string())),
            }
        }
        found.sort_by(|a, b| a.id.cmp(&b.id));
        Ok((found, unreadable))
    }
}
