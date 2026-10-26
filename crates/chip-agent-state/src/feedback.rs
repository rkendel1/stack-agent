//! Feedback: what happened, in one later attempt, after a recalled episode was presented to it
//! (the `chip.EpisodeFeedback` collection).
//!
//! An episode is never changed. What a later attempt did with it is a separate, write-once record
//! naming both: the recalled episode and the attempt, one record per recalled episode per attempt
//! (`<task>~a<attempt>~r<rank>`). It is derived from that attempt's own episode, so from the
//! host's observations and verdict and nothing the model said:
//!
//! * **attempted**: whether the attempt's *first* write was the write the recalled episode's
//!   verified procedure ended with. `None` when that cannot be observed: the episode names no
//!   write, or the attempt wrote nothing.
//! * **verification_after**: the status of the first test run after that write, if any.
//! * **classification**:
//!   * `helpful`: the recalled write was performed first, succeeded, the next test run passed and
//!     the host verified the work. The procedure held here; that recall *caused* it is not
//!     established (the model may have done the same without it).
//!   * `misleading`: the recalled write was performed first and the next test run failed: here,
//!     the procedure did not pass verification.
//!   * `not_helpful`: the host verified the work, which did not start with the recalled write.
//!   * `inconclusive`: everything else, including a failed or unverified work that never
//!     performed the recalled write (a failure is not evidence the episode misled), and anything
//!     the record cannot show.
//!
//! Feedback is observational. Nothing reads it to decide recall, authorization or verification;
//! it promotes, demotes, rewrites and deletes nothing.

use serde::{Deserialize, Serialize};

use super::episodes::{EpisodeOutcome, EpisodeRecord, EpisodeWrite, final_write};
use super::{AgentStore, StateError, from_flow};

pub const FEEDBACK_COLLECTION: &str = "chip.EpisodeFeedback";
pub const FEEDBACK_SCHEMA: &str = "chip.episode_feedback.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackClass {
    Helpful,
    NotHelpful,
    Misleading,
    Inconclusive,
}

impl FeedbackClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Helpful => "helpful",
            Self::NotHelpful => "not_helpful",
            Self::Misleading => "misleading",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// One `chip.EpisodeFeedback` record. Written once, never changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackRecord {
    pub schema: String,
    /// `<task>~a<attempt>~r<rank>`.
    pub id: String,
    /// The recalled episode, and where it is.
    pub episode: String,
    pub episode_record: String,
    /// Its position among the episodes the attempt was told of (0 first).
    pub rank: usize,
    /// The attempt it was presented to, and that attempt's own episode.
    pub task_id: String,
    pub attempt: u64,
    pub work_id: String,
    pub attempt_episode: String,
    /// The write the recalled verified procedure ended with, if it names one.
    pub recalled_final_write: Option<String>,
    pub attempted: Option<bool>,
    /// The status and record of the attempt's first write, when `attempted` is known.
    pub attempt_status: Option<String>,
    pub attempt_evidence: Option<String>,
    /// The first test run after that write: its status and record.
    pub verification_after: Option<String>,
    pub verification_evidence: Option<String>,
    /// Writes after the first failing test run that followed the attempted write.
    pub corrective_writes: usize,
    /// The attempt's own outcome and the record it rests on.
    pub outcome: EpisodeOutcome,
    pub result_record: String,
    pub classification: FeedbackClass,
    /// Chip's sentence for the classification. Never the model's.
    pub basis: String,
    pub created_at_ms: u64,
    pub recorded_by: String,
}

/// The feedback the attempt recorded as `this` gives about `recalled`, presented to it at `rank`.
/// A pure function of the two records.
pub fn feedback_of(this: &EpisodeRecord, recalled: &EpisodeRecord, rank: usize) -> FeedbackRecord {
    use FeedbackClass::*;
    let target = final_write(recalled).map(str::to_string);
    // The attempt's steps with their observation records (the episode builds both in step order
    // under the same bound, so they align).
    let steps: Vec<(&str, Option<&str>, &str, Option<&String>)> = this
        .procedure
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                s.capability.as_str(),
                s.target.as_deref(),
                s.status.as_str(),
                this.observation_records.get(i),
            )
        })
        .collect();
    let first_write = steps.iter().position(|(c, ..)| *c == "project.write");
    let attempted = match (&target, first_write) {
        (Some(target), Some(i)) => Some(steps[i].1 == Some(target.as_str())),
        _ => None,
    };
    let (mut attempt_status, mut attempt_evidence) = (None, None);
    let (mut verification_after, mut verification_evidence) = (None, None);
    let mut corrective_writes = 0;
    if let (Some(true), Some(i)) = (attempted, first_write) {
        attempt_status = Some(steps[i].2.to_string());
        attempt_evidence = steps[i].3.cloned();
        if let Some(j) = steps[i + 1..]
            .iter()
            .position(|(c, ..)| *c == "pax.test")
            .map(|k| i + 1 + k)
        {
            verification_after = Some(steps[j].2.to_string());
            verification_evidence = steps[j].3.cloned();
            if steps[j].2 != "success" {
                corrective_writes = steps[j + 1..]
                    .iter()
                    .filter(|(c, ..)| *c == "project.write")
                    .count();
            }
        }
    }
    let verified = this.outcome == EpisodeOutcome::VerifiedSuccess;
    let (classification, basis) = match (attempted, attempt_status.as_deref(), verification_after.as_deref()) {
        (None, ..) if target.is_none() => (
            Inconclusive,
            "the recalled episode names no verified write to compare with".to_string(),
        ),
        (None, ..) => (
            Inconclusive,
            "this attempt wrote nothing, so whether it used the recalled procedure cannot be observed".to_string(),
        ),
        (Some(true), Some("success"), Some("success")) if verified => (
            Helpful,
            "this attempt performed the recalled write first, the next test run passed and the host verified the work: the procedure held here (that recall caused it is not established)".to_string(),
        ),
        (Some(true), Some("success"), Some(status)) if status != "success" => (
            Misleading,
            "this attempt performed the recalled write first and the next test run failed: the procedure did not pass verification here".to_string(),
        ),
        (Some(true), ..) => (
            Inconclusive,
            "this attempt performed the recalled write first, but no test run after it, or the work's own outcome, establishes either way".to_string(),
        ),
        (Some(false), ..) if verified => (
            NotHelpful,
            "the host verified this attempt, which did not start with the recalled write".to_string(),
        ),
        (Some(false), ..) => (
            Inconclusive,
            "this attempt did not start with the recalled write and was not verified: its failure is not evidence about the episode".to_string(),
        ),
    };
    FeedbackRecord {
        schema: FEEDBACK_SCHEMA.into(),
        id: format!("{}~a{}~r{rank}", this.task_id, this.attempt),
        episode: recalled.id.clone(),
        episode_record: format!("{}:{}", super::EPISODE_COLLECTION, recalled.id),
        rank,
        task_id: this.task_id.clone(),
        attempt: this.attempt,
        work_id: this.work_id.clone(),
        attempt_episode: this.id.clone(),
        recalled_final_write: target,
        attempted,
        attempt_status,
        attempt_evidence,
        verification_after,
        verification_evidence,
        corrective_writes,
        outcome: this.outcome,
        result_record: this.result_record.clone(),
        classification,
        basis,
        created_at_ms: this.created_at_ms,
        recorded_by: this.recorded_by.clone(),
    }
}

impl AgentStore {
    /// Writes `feedback` once. The same record again is [`EpisodeWrite::AlreadyRecorded`]; a
    /// different one under the same id is refused and the stored one is kept.
    pub fn record_feedback(&self, feedback: &FeedbackRecord) -> Result<EpisodeWrite, StateError> {
        if feedback.schema != FEEDBACK_SCHEMA {
            return Err(StateError::InvalidInput(format!(
                "feedback {} is not a {FEEDBACK_SCHEMA} record",
                feedback.id
            )));
        }
        let same = |stored: FeedbackRecord| {
            if &stored == feedback {
                Ok(EpisodeWrite::AlreadyRecorded)
            } else {
                Err(StateError::Conflict {
                    operation: "episode feedback",
                    detail: format!(
                        "different feedback is already recorded as {}; it is kept, not replaced",
                        feedback.id
                    ),
                })
            }
        };
        if let Some(stored) = self.feedback_record(&feedback.id)? {
            return same(stored);
        }
        let value = serde_json::to_value(feedback).expect("feedback serializes");
        match self.create(FEEDBACK_COLLECTION, &feedback.id, value) {
            Ok(()) => Ok(EpisodeWrite::Recorded),
            Err(e) => match self.feedback_record(&feedback.id)? {
                Some(stored) => same(stored),
                None => Err(e),
            },
        }
    }

    pub fn feedback_record(&self, id: &str) -> Result<Option<FeedbackRecord>, StateError> {
        let Some((_, value)) = self.read(FEEDBACK_COLLECTION, id)? else {
            return Ok(None);
        };
        super::schema_of(FEEDBACK_COLLECTION, id, &value, FEEDBACK_SCHEMA)?;
        serde_json::from_value(value)
            .map(Some)
            .map_err(|e| StateError::InvalidHistory {
                task_id: id.split('~').next().unwrap_or_default().to_string(),
                detail: format!("feedback {id} does not parse: {e}"),
            })
    }

    /// Every feedback record, and `(id, why)` for each that is not readable.
    pub fn feedback(&self) -> Result<(Vec<FeedbackRecord>, Vec<(String, String)>), StateError> {
        let rows = self
            .db
            .list_collection(FEEDBACK_COLLECTION)
            .map_err(|e| from_flow("list", e))?;
        let (mut found, mut unreadable) = (Vec::new(), Vec::new());
        for row in rows {
            let Some(id) = row.key.strip_prefix(&format!("{FEEDBACK_COLLECTION}:")) else {
                continue;
            };
            match self.feedback_record(id) {
                Ok(Some(f)) => found.push(f),
                Ok(None) => {}
                Err(e) => unreadable.push((id.to_string(), e.to_string())),
            }
        }
        found.sort_by(|a, b| a.id.cmp(&b.id));
        Ok((found, unreadable))
    }
}
