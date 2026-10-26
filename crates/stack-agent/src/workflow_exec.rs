//! Controlled workflow execution: following an eligible workflow candidate through Chip's own loop
//! (docs/product/workflow-execution.md).
//!
//! `stack-agent work --workflows` may follow one workflow candidate
//! (docs/product/workflow-candidates.md). It is followed by a [`WorkflowPolicy`], an ordinary
//! [`LocalWorkPolicy`]: the loop asks it before the model on every turn, as it asks any local policy.
//! Nothing else executes anything.
//!
//! * A step whose inputs are all fixed (a test run, a read of a fixed path) is **proposed**
//!   locally. The loop then validates it against the capability's current declaration, authorizes
//!   it, assigns its execution id, executes it and observes it, exactly as it does a model's request.
//! * A step with a parameter (a write's content) is **asked of the model**: the escalation says which
//!   step comes next and what the model must supply. Nothing historical is copied into a request.
//! * After every turn, progress is recomputed from what actually happened ([`progress`]): the
//!   requests that were executed and the observations they produced. Another action in a step's
//!   place, a request that was refused, or a status other than the step expects, stops the policy
//!   following the candidate. From then on the work is the planner's (the model's, with the kind's
//!   own local policy), with every observation made so far.
//! * Completion is never the candidate's: the kind's own policy completes the work only when the
//!   host's verification holds on this work's own observations.
//!
//! Selection ([`select`]) uses the candidate validation of `agent-state workflows` as its only test
//! of eligibility, with the work's task.

use std::collections::BTreeMap;

use chip_agent_state::{
    Current, Declared, EpisodeRecord, ExecutedStep, InputSource, Stopped, Validation,
    WORKFLOW_EXECUTION_SCHEMA, WorkflowCandidate, WorkflowExecutionRecord,
};
use chip_core::{
    CapabilityId, CapabilityRequest, DecisionRecord, DecisionSource, ExecutionId, ExecutionStatus,
    InputValue, LocalWorkPolicy, Observation, WorkDecision, WorkOutcome, WorkView,
};

/// What selection found.
#[derive(Debug, Clone)]
pub struct Selection {
    pub considered: usize,
    /// Each eligible candidate's id.
    pub eligible: Vec<String>,
    /// Codes that made the others ineligible, counted.
    pub ineligible: BTreeMap<String, usize>,
    pub selected: Option<WorkflowCandidate>,
    /// How the selection was made, or why none was.
    pub rule: String,
}

impl Selection {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "considered": self.considered,
            "eligible": self.eligible,
            "ineligible": self.ineligible,
            "selected": self.selected.as_ref().map(|c| c.id.clone()),
            "rule": self.rule,
        })
    }
}

/// A step's identity for comparing procedures: its capability, role and fixed inputs.
fn material(c: &WorkflowCandidate) -> Vec<(String, String, Vec<(String, Option<String>)>)> {
    c.steps
        .iter()
        .map(|s| {
            (
                s.capability.clone(),
                s.role.clone(),
                s.inputs
                    .iter()
                    .map(|i| {
                        (
                            i.name.clone(),
                            match &i.source {
                                InputSource::Fixed { value } => Some(value.clone()),
                                InputSource::Parameter => None,
                            },
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}

/// Chooses the candidate a work of `kind` with goal-term digests `terms` may follow: each
/// candidate is validated (the same `chip_agent_state::validate` the inspection uses, with the
/// task), and only an eligible one can be chosen. Several eligible candidates are chosen between
/// only when they describe the same procedure (then the lowest id); materially different
/// procedures are never chosen between, and the work is planned as without workflows.
#[allow(clippy::too_many_arguments)]
pub fn select(
    candidates: &[WorkflowCandidate],
    episodes: &[EpisodeRecord],
    backing: &dyn Fn(&EpisodeRecord) -> Result<(), String>,
    project: &str,
    conditions: &BTreeMap<String, String>,
    capabilities: &BTreeMap<String, Declared>,
    kind: &str,
    terms: &[String],
) -> Selection {
    let mut eligible: Vec<&WorkflowCandidate> = Vec::new();
    let mut ineligible = BTreeMap::new();
    for c in candidates {
        let validation: Validation = chip_agent_state::validate(
            c,
            candidates,
            &Current {
                project,
                conditions,
                capabilities,
                task: Some((kind, terms)),
                episode: &|id| episodes.iter().find(|e| e.id == id).cloned(),
                backing,
            },
        );
        if validation.eligible {
            eligible.push(c);
        } else {
            for code in validation.codes() {
                *ineligible.entry(code.to_string()).or_default() += 1;
            }
        }
    }
    eligible.sort_by(|a, b| a.id.cmp(&b.id));
    let (selected, rule) = match eligible.as_slice() {
        [] => (None, "no candidate is eligible for this task".to_string()),
        [one] => (
            Some((*one).clone()),
            "the one eligible candidate".to_string(),
        ),
        [first, rest @ ..] if rest.iter().all(|c| material(c) == material(first)) => (
            Some((*first).clone()),
            format!(
                "{} eligible candidates describe the same procedure; the lowest id",
                eligible.len()
            ),
        ),
        _ => (
            None,
            format!(
                "{} eligible candidates describe different procedures; none is chosen between",
                eligible.len()
            ),
        ),
    };
    Selection {
        considered: candidates.len(),
        eligible: eligible.iter().map(|c| c.id.clone()).collect(),
        ineligible,
        selected,
        rule,
    }
}

/// One candidate step, as the policy follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedStep {
    pub capability: String,
    pub role: String,
    pub fixed: BTreeMap<String, String>,
    pub parameters: Vec<String>,
    pub expected: ExecutionStatus,
}

pub fn planned(candidate: &WorkflowCandidate) -> Vec<PlannedStep> {
    candidate
        .steps
        .iter()
        .map(|s| PlannedStep {
            capability: s.capability.clone(),
            role: s.role.clone(),
            fixed: s
                .inputs
                .iter()
                .filter_map(|i| match &i.source {
                    InputSource::Fixed { value } => Some((i.name.clone(), value.clone())),
                    InputSource::Parameter => None,
                })
                .collect(),
            parameters: s
                .inputs
                .iter()
                .filter(|i| i.source == InputSource::Parameter)
                .map(|i| i.name.clone())
                .collect(),
            expected: match s.expected_status.as_str() {
                "success" => ExecutionStatus::Success,
                "cancelled" => ExecutionStatus::Cancelled,
                _ => ExecutionStatus::Failure,
            },
        })
        .collect()
}

/// How an earlier attempt's restored request is labelled.
pub const RESTORED: &str = "restored";

/// One executed request, in order: who asked, what, and what was observed (`None`: nothing ran).
#[derive(Debug, Clone)]
pub struct Performed {
    pub by: &'static str,
    pub request: CapabilityRequest,
    pub status: Option<ExecutionStatus>,
    /// Its observation record, where one was written.
    pub record: Option<String>,
}

/// How far the attempt followed the candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// Steps completed, in order, with who asked for them, the execution and its record.
    pub completed: Vec<(usize, &'static str, String, Option<String>)>,
    /// The step at which following stopped, and why.
    pub stopped: Option<(usize, &'static str, String)>,
    /// The execution at the step where following stopped, and its record, if it was executed.
    pub stopped_execution: Option<(String, Option<String>)>,
    /// Requests performed after following stopped (the planner's).
    pub after_stop: usize,
}

impl Progress {
    pub fn following(&self, steps: &[PlannedStep]) -> bool {
        self.stopped.is_none() && self.completed.len() < steps.len()
    }
}

fn matches(step: &PlannedStep, request: &CapabilityRequest) -> bool {
    request.capability_id.as_str() == step.capability
        && step.fixed.iter().all(|(name, value)| {
            matches!(request.inputs.get(name), Some(InputValue::Text(v)) if v == value)
        })
        && request
            .inputs
            .keys()
            .all(|name| step.fixed.contains_key(name) || step.parameters.contains(name))
}

/// The requests the attempt performed, in order: those an earlier attempt performed (restored),
/// then each request this attempt decided, with the status its execution was observed with.
pub fn performed(
    prior: &[Performed],
    decisions: &[DecisionRecord],
    observations: &[Observation],
    record_of: &dyn Fn(&str) -> Option<String>,
) -> Vec<Performed> {
    let mut out = prior.to_vec();
    for d in decisions {
        if let WorkDecision::RequestCapability(request) = &d.decision {
            out.push(Performed {
                by: match d.source {
                    DecisionSource::Local => "local",
                    DecisionSource::Model => "model",
                },
                request: request.clone(),
                status: observations
                    .iter()
                    .find(|o| o.execution_id == request.execution_id)
                    .map(|o| o.status),
                record: record_of(&request.execution_id.0),
            });
        }
    }
    out
}

/// What the performed requests did to the candidate's steps. Pure: the same history gives the
/// same progress.
pub fn progress(steps: &[PlannedStep], performed: &[Performed]) -> Progress {
    let mut p = Progress {
        completed: Vec::new(),
        stopped: None,
        stopped_execution: None,
        after_stop: 0,
    };
    for done in performed {
        if p.stopped.is_some() {
            p.after_stop += 1;
            continue;
        }
        let k = p.completed.len();
        let Some(step) = steps.get(k) else {
            // The procedure is complete; whatever follows is the planner's.
            break;
        };
        let id = done.request.execution_id.to_string();
        if !matches(step, &done.request) {
            p.stopped = Some((
                k,
                "deviated",
                format!(
                    "{} was performed where the step is {}",
                    done.request.capability_id, step.capability
                ),
            ));
        } else {
            match done.status {
                None => {
                    p.stopped = Some((
                        k,
                        "not_executed",
                        "the step was requested, and validation or authorization refused it (or nothing ran)".into(),
                    ))
                }
                Some(status) if status == step.expected => {
                    p.completed.push((k, done.by, id, done.record.clone()));
                }
                Some(status) => {
                    p.stopped_execution = Some((id, done.record.clone()));
                    p.stopped = Some((
                        k,
                        if step.role == "verify" {
                            "verification_failed"
                        } else {
                            "step_failed"
                        },
                        format!(
                            "{} was observed {}, where the step expects {}",
                            step.capability,
                            status_name(status),
                            status_name(step.expected)
                        ),
                    ));
                }
            }
        }
    }
    p
}

/// Why following stopped, from the progress and the work's outcome: a departure the progress
/// found, or, when the work ended before the procedure did with nothing departing from it, the
/// ending itself (`cancelled`, or `work_ended`). `None` when every step completed.
pub fn stop_of(
    p: &Progress,
    steps: &[PlannedStep],
    outcome: &WorkOutcome,
) -> Option<(usize, String, String)> {
    if let Some((k, code, detail)) = &p.stopped {
        return Some((*k, code.to_string(), detail.clone()));
    }
    (p.completed.len() < steps.len()).then(|| {
        (
            p.completed.len(),
            match outcome {
                WorkOutcome::Cancelled { .. } => "cancelled",
                _ => "work_ended",
            }
            .to_string(),
            format!("the work ended {}", outcome.terminal_state().name()),
        )
    })
}

fn status_name(s: ExecutionStatus) -> &'static str {
    match s {
        ExecutionStatus::Success => "success",
        ExecutionStatus::Failure => "failure",
        ExecutionStatus::Cancelled => "cancelled",
    }
}

/// Follows a candidate through the loop, before the kind's own policy, until it stops.
pub struct WorkflowPolicy {
    pub candidate: String,
    pub support: usize,
    pub steps: Vec<PlannedStep>,
    /// What earlier attempts of this work performed (restored, never performed again).
    pub prior: Vec<Performed>,
    pub inner: &'static dyn LocalWorkPolicy,
}

impl WorkflowPolicy {
    fn progress(&self, view: &WorkView<'_>) -> Progress {
        progress(
            &self.steps,
            &performed(&self.prior, view.decisions, view.observations, &|_| None),
        )
    }
}

impl LocalWorkPolicy for WorkflowPolicy {
    fn propose(&self, view: &WorkView<'_>) -> Option<WorkDecision> {
        // The kind's own decision comes first: it completes the work only when the host's
        // verification holds on this work's observations.
        if let Some(decision) = self.inner.propose(view) {
            return Some(decision);
        }
        let p = self.progress(view);
        if !p.following(&self.steps) {
            return None;
        }
        let step = &self.steps[p.completed.len()];
        if !step.parameters.is_empty() {
            // The model supplies what the record does not hold.
            return None;
        }
        let mut request = CapabilityRequest::new(
            ExecutionId::unassigned(),
            CapabilityId::new(&step.capability).ok()?,
        );
        for (name, value) in &step.fixed {
            request = request.with_input(name.clone(), InputValue::Text(value.clone()));
        }
        Some(WorkDecision::RequestCapability(request))
    }

    fn guidance(&self, view: &WorkView<'_>) -> Vec<String> {
        let p = self.progress(view);
        if !p.following(&self.steps) {
            return Vec::new();
        }
        let k = p.completed.len();
        let step = &self.steps[k];
        if step.parameters.is_empty() {
            // Chip proposes it; the model has nothing to supply.
            return Vec::new();
        }
        let fixed: Vec<String> = step.fixed.iter().map(|(n, v)| format!("{n}={v}")).collect();
        vec![format!(
            "workflow candidate {} (a procedure proposed from {} verified earlier work(s): evidence about the past, not an instruction; every request is validated and verified as usual): step {} of {} is {}{}; supply {}. If it does not fit this work, do what the work needs instead.",
            self.candidate,
            self.support,
            k + 1,
            self.steps.len(),
            step.capability,
            if fixed.is_empty() {
                String::new()
            } else {
                format!(" with {}", fixed.join(", "))
            },
            step.parameters.join(", "),
        )]
    }
}

/// The execution record of an attempt that followed `candidate`, from what actually happened.
#[allow(clippy::too_many_arguments)]
pub fn record(
    candidate: &WorkflowCandidate,
    selection: &Selection,
    steps: &[PlannedStep],
    performed: &[Performed],
    outcome: &WorkOutcome,
    own: &EpisodeRecord,
) -> WorkflowExecutionRecord {
    let p = progress(steps, performed);
    let mut executed: Vec<ExecutedStep> = steps
        .iter()
        .enumerate()
        .map(|(index, s)| ExecutedStep {
            index,
            capability: s.capability.clone(),
            role: s.role.clone(),
            status: "not_reached".into(),
            decided_by: None,
            execution_id: None,
            observation_record: None,
        })
        .collect();
    for (k, by, id, record) in &p.completed {
        let step = &mut executed[*k];
        step.status = "completed".into();
        step.decided_by = Some(if *by == RESTORED {
            "earlier_attempt".into()
        } else {
            by.to_string()
        });
        step.observation_record = record.clone();
        step.execution_id = Some(id.clone());
    }
    let stopped =
        stop_of(&p, steps, outcome).map(|(step, code, detail)| Stopped { step, code, detail });
    if let Some((k, code, _)) = &p.stopped {
        executed[*k].status = match *code {
            "deviated" => "deviated",
            "not_executed" => "not_executed",
            _ => "failed",
        }
        .into();
        if let Some((id, record)) = &p.stopped_execution {
            executed[*k].execution_id = Some(id.clone());
            executed[*k].observation_record = record.clone();
        }
    }
    WorkflowExecutionRecord {
        schema: WORKFLOW_EXECUTION_SCHEMA.into(),
        id: format!("{}~a{}~wf", own.task_id, own.attempt),
        candidate: candidate.id.clone(),
        candidate_record: format!("{}:{}", chip_agent_state::WORKFLOW_COLLECTION, candidate.id),
        procedure_key: candidate.procedure_key.clone(),
        support: candidate
            .support
            .iter()
            .map(|s| s.episode.clone())
            .collect(),
        considered: selection.considered,
        eligible: selection.eligible.len(),
        selection_rule: selection.rule.clone(),
        task_id: own.task_id.clone(),
        attempt: own.attempt,
        work_id: own.work_id.clone(),
        completed_steps: p.completed.len(),
        steps: executed,
        fallback: p.stopped.is_some() && p.after_stop > 0,
        stopped,
        outcome: own.outcome,
        verification: own.verification.clone(),
        result_record: own.result_record.clone(),
        episode: own.id.clone(),
        created_at_ms: own.created_at_ms,
        recorded_by: own.recorded_by.clone(),
    }
}

// ---- the work's side ---------------------------------------------------------------------------

/// [`select`] over this database's candidates and episodes, for a work of `kind` with `goal` in the
/// project at `root`, offered `capabilities`. Reads only.
pub async fn select_for(
    store: &chip_agent_state::AgentStore,
    root: &std::path::Path,
    kind: crate::software_work::GoalKind,
    goal: &str,
    profile: &crate::autoconfig::profile::ProjectProfile,
    capabilities: &dyn chip_core::CapabilityProvider,
) -> Selection {
    let read = store.workflows().and_then(|(candidates, _)| {
        let (episodes, _) = store.episodes()?;
        Ok((candidates, episodes))
    });
    let (candidates, episodes) = match read {
        Ok(found) => found,
        Err(why) => {
            return Selection {
                considered: 0,
                eligible: Vec::new(),
                ineligible: BTreeMap::new(),
                selected: None,
                rule: format!(
                    "the candidates could not be read ({why}); the work is planned as without workflows"
                ),
            };
        }
    };
    let project = crate::work_state::project_scope(root).unwrap_or_default();
    let conditions = crate::work_state::episode_conditions(root, profile);
    let declared = crate::work_state::declared_capabilities(capabilities).await;
    select(
        &candidates,
        &episodes,
        &|e| store.backing(e),
        &project,
        &conditions,
        &declared,
        kind.name(),
        &chip_agent_state::intent_terms(goal),
    )
}

/// The policy that follows `candidate` for the begun `work`, wrapping the kind's own `inner`
/// policy, and starting from what earlier attempts of the work already performed.
pub fn policy_for(
    candidate: &WorkflowCandidate,
    work: &chip_agent_state::DurableWork,
    inner: &'static dyn LocalWorkPolicy,
) -> WorkflowPolicy {
    let records = work.restored_records();
    let prior = work
        .restored()
        .into_iter()
        .enumerate()
        .map(|(i, r)| Performed {
            by: RESTORED,
            request: r.request,
            status: Some(r.result.status),
            record: records.get(i).map(|id| format!("chip.Observation:{id}")),
        })
        .collect();
    WorkflowPolicy {
        candidate: candidate.id.clone(),
        support: candidate.support.len(),
        steps: planned(candidate),
        prior,
        inner,
    }
}

/// Builds and writes the attempt's execution record.
pub fn record_execution(
    store: &chip_agent_state::AgentStore,
    work: &chip_agent_state::DurableWork,
    selection: &Selection,
    policy: &WorkflowPolicy,
    report: &chip_core::WorkReport,
    own: &EpisodeRecord,
) -> Result<WorkflowExecutionRecord, chip_agent_state::StateError> {
    let candidate = selection
        .selected
        .as_ref()
        .expect("a followed workflow was selected");
    let (task, attempt) = (work.task_id().to_string(), work.attempt());
    let done = performed(
        &policy.prior,
        &report.decisions,
        &report.observations,
        &|id| {
            Some(format!(
                "chip.Observation:{task}~{}",
                chip_agent_state::intent_key(attempt, id)
            ))
        },
    );
    let record = record(
        candidate,
        selection,
        &policy.steps,
        &done,
        &report.outcome,
        own,
    );
    store.record_workflow_execution(&record)?;
    Ok(record)
}

/// The `workflow` section of a work's report.
pub fn report_json(
    enabled: bool,
    followed: Option<&(Selection, WorkflowPolicy)>,
    unselected: Option<&Selection>,
    report: &chip_core::WorkReport,
    recorded: Option<&Result<WorkflowExecutionRecord, chip_agent_state::StateError>>,
) -> serde_json::Value {
    if !enabled {
        return serde_json::json!({"enabled": false});
    }
    let Some((selection, policy)) = followed else {
        return serde_json::json!({
            "enabled": true,
            "selection": unselected.map(Selection::to_json),
            "followed": false,
        });
    };
    let p = progress(
        &policy.steps,
        &performed(
            &policy.prior,
            &report.decisions,
            &report.observations,
            &|_| None,
        ),
    );
    serde_json::json!({
        "enabled": true,
        "selection": selection.to_json(),
        "followed": true,
        "steps": policy.steps.len(),
        "completed_steps": p.completed.len(),
        "local_steps": p.completed.iter().filter(|(_, by, _, _)| *by == "local").count(),
        "stopped": stop_of(&p, &policy.steps, &report.outcome).map(|(step, code, detail)| serde_json::json!({
            "step": step, "code": code, "detail": detail,
        })),
        "fallback": p.stopped.is_some() && p.after_stop > 0,
        "record": match recorded {
            Some(Ok(r)) => serde_json::json!(format!("{}:{}", chip_agent_state::WORKFLOW_EXECUTION_COLLECTION, r.id)),
            Some(Err(why)) => serde_json::json!({"error": why.to_string()}),
            None => serde_json::Value::Null,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chip_agent_state::{EpisodeOutcome, EpisodeScope, Support, WorkflowInput, WorkflowStep};
    use chip_core::{Observation, ObservationKind, WorkGoal};

    fn step(capability: &str, role: &str, path: Option<&str>, content: bool) -> WorkflowStep {
        let mut inputs = Vec::new();
        if content {
            inputs.push(WorkflowInput {
                name: "content".into(),
                source: InputSource::Parameter,
            });
        }
        if let Some(p) = path {
            inputs.push(WorkflowInput {
                name: "path".into(),
                source: InputSource::Fixed { value: p.into() },
            });
        }
        WorkflowStep {
            capability: capability.into(),
            contract: Some("c1.v1".into()),
            role: role.into(),
            inputs,
            expected_status: "success".into(),
            evidence: vec![],
        }
    }

    fn candidate(id: &str, path: &str) -> WorkflowCandidate {
        WorkflowCandidate {
            schema: "chip.workflow_candidate.v1".into(),
            id: id.into(),
            procedure_key: format!("wfp1.{path}"),
            scope: EpisodeScope {
                level: "project".into(),
                project: "path:p".into(),
            },
            kind: "change".into(),
            goal_signature: "g1.x".into(),
            goal_terms: vec!["t1".into(), "t2".into()],
            preconditions: BTreeMap::new(),
            required_capabilities: [
                ("project.write".to_string(), Some("c1.v1".to_string())),
                ("pax.test".to_string(), Some("c1.v1".to_string())),
            ]
            .into_iter()
            .collect(),
            steps: vec![
                step("project.write", "change", Some(path), true),
                step("pax.test", "verify", None, false),
            ],
            verification: Some(chip_agent_state::VerificationRequirement {
                kind: "test_run_after_last_change".into(),
                step: Some(1),
                host_verdict: "verified".into(),
            }),
            support: vec![Support {
                episode: "t~a2".into(),
                episode_record: "chip.Episode:t~a2".into(),
                result_record: "chip.WorkResult:t~a2".into(),
                outcome: EpisodeOutcome::VerifiedSuccess,
            }],
            strength: "single_observation".into(),
            unresolved: vec![],
            created_at_ms: 1,
            extracted_by: "test".into(),
        }
    }

    fn declared() -> BTreeMap<String, Declared> {
        [
            (
                "project.write",
                vec![("path".to_string(), true), ("content".to_string(), true)],
            ),
            ("pax.test", vec![]),
        ]
        .into_iter()
        .map(|(c, inputs)| {
            (
                c.to_string(),
                Declared {
                    contract: "c1.v1".into(),
                    inputs,
                },
            )
        })
        .collect()
    }

    fn episode() -> EpisodeRecord {
        serde_json::from_value(serde_json::json!({
            "schema": "chip.episode.v1", "id": "t~a2",
            "scope": {"level": "project", "project": "path:p"},
            "task_id": "t", "attempt": 2, "work_id": "t", "kind": "change",
            "intent_terms": ["t1", "t2"], "conditions": {}, "procedure": [],
            "result_record": "chip.WorkResult:t~a2", "observation_records": [],
            "turns": 1, "executions": 0, "model_calls": 0, "outcome": "verified_success",
            "outcome_basis": "b",
            "verification": {"accepted": true, "verified": true, "goal_satisfied": true,
                "grounded": false, "evidence_invalidated": null, "exit_status": 0},
            "failure_evidence": [], "related": [], "recalled": [], "created_at_ms": 1,
            "recorded_by": "t"
        }))
        .unwrap()
    }

    fn pick(candidates: &[WorkflowCandidate], terms: &[&str]) -> Selection {
        let terms: Vec<String> = terms.iter().map(|t| t.to_string()).collect();
        select(
            candidates,
            &[episode()],
            &|_| Ok(()),
            "path:p",
            &BTreeMap::new(),
            &declared(),
            "change",
            &terms,
        )
    }

    #[test]
    fn selection_takes_the_one_eligible_candidate_and_never_chooses_between_different_procedures() {
        let one = pick(&[candidate("wfp1.a.1", "src/lib.rs")], &["t1", "t2"]);
        assert_eq!(one.selected.unwrap().id, "wfp1.a.1");
        // Unrelated task: ineligible, with the validation's own code.
        let other = pick(&[candidate("wfp1.a.1", "src/lib.rs")], &["zz"]);
        assert!(other.selected.is_none());
        assert_eq!(other.ineligible["task_mismatch"], 1);
        // Two candidates, same procedure, different goal signatures: the lowest id.
        let mut a = candidate("wfp1.b.2", "src/lib.rs");
        a.goal_signature = "g1.a".into();
        a.procedure_key = "wfp1.b".into();
        let mut b = candidate("wfp1.a.9", "src/lib.rs");
        b.goal_signature = "g1.b".into();
        b.procedure_key = "wfp1.a".into();
        let tie = pick(&[a.clone(), b.clone()], &["t1", "t2"]);
        assert_eq!(tie.selected.unwrap().id, "wfp1.a.9");
        assert_eq!(
            pick(&[b.clone(), a.clone()], &["t1", "t2"]).eligible,
            tie.eligible
        );
        // Two eligible candidates with materially different procedures: none is chosen.
        let mut c = candidate("wfp1.c.3", "src/two.rs");
        c.goal_signature = "g1.c".into();
        c.procedure_key = "wfp1.c".into();
        let different = pick(&[a, c], &["t1", "t2"]);
        assert!(different.selected.is_none(), "{}", different.rule);
        assert_eq!(different.eligible.len(), 2);
        // Conflicting candidates (same task, different procedure) are ineligible by validation.
        let conflict = pick(
            &[
                candidate("wfp1.x.1", "src/lib.rs"),
                candidate("wfp1.y.1", "src/two.rs"),
            ]
            .map(|mut c| {
                c.procedure_key = c.id.clone();
                c
            }),
            &["t1", "t2"],
        );
        assert!(conflict.selected.is_none());
        assert_eq!(conflict.ineligible["conflicting_procedure"], 2);
        // Superseded and stale-contract candidates are not selected either.
        let mut old = candidate("wfp1.a.1", "src/lib.rs");
        let mut newer = candidate("wfp1.a.2", "src/lib.rs");
        newer.support.push(newer.support[0].clone());
        old.procedure_key = "k".into();
        newer.procedure_key = "k".into();
        let s = pick(&[old, newer], &["t1", "t2"]);
        assert_eq!(s.selected.unwrap().id, "wfp1.a.2");
        assert_eq!(s.ineligible["superseded"], 1);
        let mut stale = candidate("wfp1.a.1", "src/lib.rs");
        stale
            .required_capabilities
            .insert("pax.test".into(), Some("c1.old".into()));
        assert_eq!(
            pick(&[stale], &["t1", "t2"]).ineligible["contract_changed"],
            1
        );
    }

    fn request(capability: &str, path: Option<&str>, id: &str) -> CapabilityRequest {
        let mut r =
            CapabilityRequest::new(ExecutionId::new(id), CapabilityId::new(capability).unwrap());
        if let Some(p) = path {
            r = r
                .with_input("path", InputValue::Text(p.into()))
                .with_input("content", InputValue::Text("x".into()));
        }
        r
    }

    fn did(by: &'static str, r: CapabilityRequest, status: Option<ExecutionStatus>) -> Performed {
        Performed {
            by,
            request: r,
            status,
            record: None,
        }
    }

    #[test]
    fn progress_follows_what_happened_and_stops_at_the_first_departure() {
        use ExecutionStatus::*;
        let steps = planned(&candidate("c", "src/lib.rs"));
        let write = |p: &str| request("project.write", Some(p), "e1");
        let test = || request("pax.test", None, "e2");
        // Both steps, as expected.
        let p = progress(
            &steps,
            &[
                did("model", write("src/lib.rs"), Some(Success)),
                did("local", test(), Some(Success)),
            ],
        );
        assert_eq!(p.completed.len(), 2);
        assert!(p.stopped.is_none() && !p.following(&steps));
        // Another file written in the step's place: deviated; what follows is the planner's.
        let p = progress(
            &steps,
            &[
                did("model", write("src/two.rs"), Some(Success)),
                did("model", write("src/lib.rs"), Some(Success)),
            ],
        );
        assert_eq!(p.stopped.as_ref().unwrap().1, "deviated");
        assert_eq!(p.after_stop, 1);
        // The write failed; the test failed; the request was refused.
        let failed = progress(&steps, &[did("model", write("src/lib.rs"), Some(Failure))]);
        assert_eq!(failed.stopped.unwrap().1, "step_failed");
        let unverified = progress(
            &steps,
            &[
                did("model", write("src/lib.rs"), Some(Success)),
                did("local", test(), Some(Failure)),
            ],
        );
        assert_eq!(
            unverified.stopped.as_ref().unwrap().1,
            "verification_failed"
        );
        assert_eq!(unverified.stopped_execution.as_ref().unwrap().0, "e2");
        let refused = progress(&steps, &[did("model", write("src/lib.rs"), None)]);
        assert_eq!(refused.stopped.unwrap().1, "not_executed");
        // An extra input the step does not carry is not the step.
        let mut extra = write("src/lib.rs");
        extra
            .inputs
            .insert("mode".into(), InputValue::Text("x".into()));
        assert_eq!(
            progress(&steps, &[did("model", extra, Some(Success))])
                .stopped
                .unwrap()
                .1,
            "deviated"
        );
        // An earlier attempt's restored step counts, and is not performed again.
        let p = progress(&steps, &[did(RESTORED, write("src/lib.rs"), Some(Success))]);
        assert_eq!(p.completed[0].1, RESTORED);
        assert!(p.following(&steps));
    }

    fn view<'a>(
        goal: &'a WorkGoal,
        decisions: &'a [DecisionRecord],
        observations: &'a [Observation],
    ) -> WorkView<'a> {
        WorkView {
            goal,
            turn: decisions.len(),
            executions: observations.len(),
            observations,
            decisions,
        }
    }

    fn observed(id: &str, status: ExecutionStatus) -> Observation {
        Observation {
            execution_id: ExecutionId::new(id),
            kind: ObservationKind::ExecutionCompleted,
            status,
            output: Some("ok".into()),
            receipt_id: None,
            evidence: None,
        }
    }

    struct Never;
    impl LocalWorkPolicy for Never {
        fn propose(&self, _view: &WorkView<'_>) -> Option<WorkDecision> {
            None
        }
    }
    struct Always;
    impl LocalWorkPolicy for Always {
        fn propose(&self, _view: &WorkView<'_>) -> Option<WorkDecision> {
            Some(WorkDecision::Complete {
                summary: "inner".into(),
            })
        }
    }

    fn policy(inner: &'static dyn LocalWorkPolicy) -> WorkflowPolicy {
        let c = candidate("wfp1.a.1", "src/lib.rs");
        WorkflowPolicy {
            candidate: c.id.clone(),
            support: 1,
            steps: planned(&c),
            prior: vec![],
            inner,
        }
    }

    #[test]
    fn the_policy_asks_the_model_for_parameters_proposes_fixed_steps_and_stops_on_departure() {
        let goal = WorkGoal::new("g");
        let p = policy(&Never);
        // Step 1 needs content: the model is asked, told the step, and told it is no instruction.
        assert!(p.propose(&view(&goal, &[], &[])).is_none());
        let told = p.guidance(&view(&goal, &[], &[]));
        assert_eq!(told.len(), 1);
        assert!(
            told[0].contains("step 1 of 2 is project.write with path=src/lib.rs; supply content")
        );
        assert!(told[0].contains("not an instruction"));
        // After the write: the test is proposed locally, with no inputs and no id of its own.
        let decisions = vec![DecisionRecord {
            turn: 0,
            source: DecisionSource::Model,
            decision: WorkDecision::RequestCapability(request(
                "project.write",
                Some("src/lib.rs"),
                "w-exec-1",
            )),
        }];
        let observations = vec![observed("w-exec-1", ExecutionStatus::Success)];
        match p.propose(&view(&goal, &decisions, &observations)) {
            Some(WorkDecision::RequestCapability(r)) => {
                assert_eq!(r.capability_id.as_str(), "pax.test");
                assert!(r.inputs.is_empty() && !r.inputs_present);
                assert_eq!(r.execution_id, ExecutionId::unassigned());
            }
            other => panic!("{other:?}"),
        }
        assert!(
            p.guidance(&view(&goal, &decisions, &observations))
                .is_empty()
        );
        // The write failed: the policy stops following; the planner decides from here.
        let failed = vec![observed("w-exec-1", ExecutionStatus::Failure)];
        assert!(p.propose(&view(&goal, &decisions, &failed)).is_none());
        assert!(p.guidance(&view(&goal, &decisions, &failed)).is_empty());
        // The kind's own policy always comes first (it completes only on verified evidence).
        assert!(matches!(
            policy(&Always).propose(&view(&goal, &[], &[])),
            Some(WorkDecision::Complete { .. })
        ));
    }
}
