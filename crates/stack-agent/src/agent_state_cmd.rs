//! `stack-agent agent-state`: what an operator does with a task's durable agent state.
//!
//! * `list` lists the tasks a database holds, and says which were interrupted.
//! * `show` reads a task's history from FeltDB: its intents, results, and every recorded work
//!   event with its provenance.
//! * `workflows` lists the workflow candidates proposed from verified episodes and validates each
//!   against the project as it is now, without executing anything.
//! * `episodes` lists the recorded episodes (`chip.Episode`) and whether each could be recalled
//!   into a work of this project, so far as that is known before a work starts.
//! * `export` maps a task's record to an OTLP/JSON trace (`chip-otel`): printed, or with `--send`
//!   delivered where the `OTEL_*` environment says.
//! * `settle` records an operator's conclusion about an intent whose outcome was never observed,
//!   so a refused (`OutcomeUncertain`) task can resume.
//!
//! Neither executes anything or calls a model. A settlement is a person's conclusion and is
//! recorded as one: it makes up no receipt, and it is never an execution's observation.

use std::collections::BTreeMap;

use chip_agent_state::{AgentStore, History, Settlement, StateError};
use serde_json::{Value, json};

const USAGE: &str = "\
usage: stack-agent agent-state list [--state-db FILE] [--json]
       stack-agent agent-state show [--state-db FILE] --task-id ID [--json]
       stack-agent agent-state episodes [--state-db FILE] [--project-dir DIR] [--json]
       stack-agent agent-state workflows [--state-db FILE] [--project-dir DIR] [--goal TEXT [--kind K]] [--json]
       stack-agent agent-state export [--state-db FILE] --task-id ID [--provider P] [--model M] [--send] [--json]
       stack-agent agent-state reconcile --state-db FILE --task-id ID --intent KEY --decided-by ID \\
           [--project-dir DIR] [--json]
       stack-agent agent-state settle --state-db FILE --task-id ID --intent KEY \\
           --executed true|false --decided-by ID --evidence TEXT [--json]

Without --state-db, list, show and episodes read the database of the project in the current
directory, the one every `work` and `serve` uses by default.

episodes lists every recorded episode: its outcome, when it was recorded, the records it cites,
whether it could be recalled into a work of the project in --project-dir (default: the current
directory) with the reason for each of recall's checks (hard exclusion, evidence quality, relevance,
context compatibility), and the feedback later attempts recorded about it (observational: recall
never reads it). That says what the record, the records it cites, the project and its conditions allow;
a work's kind, its goal and its capabilities' contracts decide the rest when it starts. Goal terms
are digests and are only counted. It reads; it changes nothing.

workflows lists every workflow candidate (a procedure proposed from one or more verified episodes)
and validates it against the project in --project-dir as it is now: its supporting evidence, the
project, its preconditions, the capabilities and contracts a work of its kind is offered, its inputs,
its verification, anything unresolved, conflicting procedures, and, with --goal (and --kind,
default change), the task. Each check that fails names a stable code. Validation is advisory: it
executes nothing, grants no authority and promises no success. It reads; it changes nothing.

export prints the task's record as one OTLP/JSON trace request (GenAI semantic conventions; no
prompt, reply, goal, output, reason, path or credential is ever in it). --send delivers it instead,
to the collector and file the OTEL_EXPORTER_OTLP_* and STACK_AGENT_OTLP_FILE variables name
(loopback collectors only). The record does not hold the provider or model the work asked for;
--provider and --model add them.

reconcile asks the capability's destination what happened to the intent's original operation
(by its recorded action key and binding) and resolves the intent only on sufficient independent
evidence; it never performs the operation. Today only `project.write` can be asked, through the
project in --project-dir (default: the current directory), which it locks while it asks: if a work
holds the project, nothing is concluded. Anything it cannot establish leaves the intent open, with
the reason recorded (see `show`); `settle` remains the operator's way out.

settle records an operator's conclusion that the intent's effect did (true) or did not (false)
occur, who decided, and the evidence it rests on. It performs nothing and is not a receipt.
Exit codes: 0 done, 1 refused (nothing written), 2 usage, 3 unavailable.";

const EXIT_DONE: i32 = 0;
const EXIT_REFUSED: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_UNAVAILABLE: i32 = 3;

/// `--flag value` pairs and bare `--json`, each at most once, and nothing else.
fn parse(args: &[String], takes: &[&str]) -> Result<(BTreeMap<String, String>, bool), String> {
    let (mut values, mut json) = (BTreeMap::new(), false);
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--json" && !json {
            json = true;
            i += 1;
            continue;
        }
        if !takes.contains(&flag) || values.contains_key(flag) {
            return Err(format!("unexpected argument `{flag}`"));
        }
        let Some(value) = args.get(i + 1) else {
            return Err(format!("{flag} needs a value"));
        };
        values.insert(flag.to_string(), value.clone());
        i += 2;
    }
    Ok((values, json))
}

pub fn agent_state(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("list") => list(&args[1..]),
        Some("show") => show(&args[1..]),
        Some("episodes") => episodes(&args[1..]),
        Some("workflows") => workflows(&args[1..]),
        Some("export") => export(&args[1..]),
        Some("settle") => settle(&args[1..]),
        Some("reconcile") => reconcile(&args[1..]),
        _ => usage(None),
    }
}

fn usage(why: Option<String>) -> i32 {
    if let Some(why) = why {
        eprintln!("error: {why}");
    }
    eprintln!("{USAGE}");
    EXIT_USAGE
}

/// `--state-db` when given, else the current project's default database (which must exist: reading
/// never creates one).
fn database(given: Option<&String>) -> Result<String, i32> {
    if let Some(path) = given {
        return Ok(path.clone());
    }
    let root = std::env::current_dir().map_err(|e| {
        eprintln!("error: no current directory ({e})");
        EXIT_UNAVAILABLE
    })?;
    let path = crate::work_state::default_database(&root, &|name: &str| std::env::var(name).ok())
        .map_err(|why| {
        eprintln!("error: durable agent state: {why}");
        EXIT_UNAVAILABLE
    })?;
    if !path.exists() {
        eprintln!(
            "error: durable agent state: this project has no database yet (no work has run here); pass --state-db FILE to read another"
        );
        return Err(EXIT_UNAVAILABLE);
    }
    Ok(path.display().to_string())
}

fn list(args: &[String]) -> i32 {
    let (values, json) = match parse(args, &["--state-db"]) {
        Ok(parsed) => parsed,
        Err(why) => return usage(Some(why)),
    };
    let store = match database(values.get("--state-db")).and_then(|db| open(&db)) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let mut tasks = match store.tasks() {
        Ok(tasks) => tasks,
        Err(e) => {
            eprintln!("error: durable agent state: {e}");
            return EXIT_UNAVAILABLE;
        }
    };
    tasks.sort_by_key(|t| t.updated_at_ms);
    // Nothing runs while this command holds the database, so a task still `running` here was
    // interrupted: its process ended before it finished.
    let status = |t: &chip_agent_state::TaskValue| -> String {
        if t.status == "running" {
            "interrupted".into()
        } else {
            t.status.clone()
        }
    };
    if json {
        println!(
            "{}",
            json!({"tasks": tasks.iter().map(|t| json!({
                "task_id": t.task_id, "status": status(t), "fence": t.fence,
                "unresolved_intents": t.unresolved_intents, "updated_at_ms": t.updated_at_ms,
            })).collect::<Vec<_>>()})
        );
    } else {
        for t in &tasks {
            println!(
                "{} {} ({} unresolved intent(s))",
                t.task_id,
                status(t),
                t.unresolved_intents
            );
        }
    }
    EXIT_DONE
}

fn episodes(args: &[String]) -> i32 {
    let (values, json) = match parse(args, &["--state-db", "--project-dir"]) {
        Ok(parsed) => parsed,
        Err(why) => return usage(Some(why)),
    };
    let store = match database(values.get("--state-db")).and_then(|db| open(&db)) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let root = match values.get("--project-dir") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(e) => {
                eprintln!("error: no current directory ({e})");
                return EXIT_UNAVAILABLE;
            }
        },
    };
    let (mut episodes, unreadable) = match store.episodes() {
        Ok(found) => found,
        Err(e) => {
            eprintln!("error: durable agent state: {e}");
            return EXIT_UNAVAILABLE;
        }
    };
    episodes.sort_by(|a, b| a.created_at_ms.cmp(&b.created_at_ms).then(a.id.cmp(&b.id)));
    // What a work of this project would be compared with; nothing is started to find it.
    let project = crate::work_state::project_scope(&root);
    let conditions = crate::work_state::episode_conditions(
        &root,
        &crate::work_state::project_profile(&root, "", ""),
    );
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    // What later attempts did after each episode was presented: observational, never used by recall.
    let feedback = match store.feedback() {
        Ok((found, _)) => found,
        Err(e) => {
            eprintln!("error: durable agent state: {e}");
            return EXIT_UNAVAILABLE;
        }
    };
    let here = chip_agent_state::EpisodeContext {
        project: project.clone().unwrap_or_default(),
        conditions,
        ..Default::default()
    };
    let asking = chip_agent_state::Asking {
        context: &here,
        task_id: None,
        work_known: false,
    };
    let rows: Vec<Value> = episodes
        .iter()
        .map(|e| {
            let this_project = project.as_deref() == Some(e.scope.project.as_str());
            // The same assessment recall makes, for a work of this project not yet started.
            let eligibility = chip_agent_state::assess(e, &asking, now_ms, &|e| store.backing(e));
            let why_not = eligibility.excluded.as_ref().map(|x| x.describe());
            json!({
                "episode": e.id,
                "schema": e.schema,
                // A digest of the project's path, never the path.
                "project": e.scope.project,
                "scope": e.scope.level,
                "this_project": this_project,
                "task_id": e.task_id,
                "attempt": e.attempt,
                "kind": e.kind,
                "outcome": e.outcome.as_str(),
                "outcome_basis": e.outcome_basis,
                "recorded_at_ms": e.created_at_ms,
                "recorded_by": e.recorded_by,
                "evidence": {
                    "result": e.result_record,
                    "observations": e.observation_records,
                    "failures": e.failure_evidence,
                },
                "procedure": e.procedure.iter().map(|p| json!({
                    "capability": p.capability, "target": p.target, "status": p.status,
                })).collect::<Vec<_>>(),
                "conditions": e.conditions,
                // Digests, not words: only how many there are.
                "goal_terms": e.intent_terms.len(),
                "related": e.related,
                "recalled": e.recalled,
                "recallable_here": why_not.is_none(),
                "not_recallable_because": why_not,
                "eligibility": eligibility.to_json(),
                "feedback": {
                    "observational_only": true,
                    "counts": feedback.iter().filter(|f| f.episode == e.id).fold(
                        std::collections::BTreeMap::<&str, usize>::new(),
                        |mut m, f| {
                            *m.entry(f.classification.as_str()).or_default() += 1;
                            m
                        },
                    ),
                    "records": feedback.iter().filter(|f| f.episode == e.id).map(|f| json!({
                        "id": f.id, "attempt_episode": f.attempt_episode,
                        "classification": f.classification.as_str(),
                        "attempted": f.attempted, "verification_after": f.verification_after,
                        "outcome": f.outcome.as_str(),
                    })).collect::<Vec<_>>(),
                },
                "decided_when_a_work_starts": ["kind", "goal similarity", "capability contracts", "recall limit"],
            })
        })
        .collect();
    if json {
        println!(
            "{}",
            json!({
                "episodes": rows,
                "unreadable": unreadable.iter().map(|(id, why)| json!({"id": id, "error": why})).collect::<Vec<_>>(),
            })
        );
    } else {
        for r in &rows {
            let status = match r["not_recallable_because"].as_str() {
                None => "recallable here".to_string(),
                Some(why) => format!("not recallable: {why}"),
            };
            println!(
                "{} {} {} recorded_at_ms={} ({status})",
                r["episode"].as_str().unwrap_or_default(),
                r["kind"].as_str().unwrap_or_default(),
                r["outcome"].as_str().unwrap_or_default(),
                r["recorded_at_ms"],
            );
        }
        for (id, why) in &unreadable {
            println!("{id} unreadable: {why}");
        }
    }
    EXIT_DONE
}

fn workflows(args: &[String]) -> i32 {
    let (values, json) = match parse(args, &["--state-db", "--project-dir", "--goal", "--kind"]) {
        Ok(parsed) => parsed,
        Err(why) => return usage(Some(why)),
    };
    let task_kind = match values.get("--kind").map(String::as_str) {
        None => crate::software_work::GoalKind::Change,
        Some(k) => match crate::software_work::GoalKind::parse(k) {
            Some(kind) => kind,
            None => return usage(Some(format!("--kind {k} is not change, verify or inspect"))),
        },
    };
    if values.contains_key("--kind") && !values.contains_key("--goal") {
        return usage(Some("--kind describes the task given with --goal".into()));
    }
    let store = match database(values.get("--state-db")).and_then(|db| open(&db)) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let root = match values.get("--project-dir") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(e) => {
                eprintln!("error: no current directory ({e})");
                return EXIT_UNAVAILABLE;
            }
        },
    };
    let read = store.workflows().and_then(|(candidates, unreadable)| {
        let (episodes, _) = store.episodes()?;
        let (feedback, _) = store.feedback()?;
        Ok((candidates, unreadable, episodes, feedback))
    });
    let (candidates, unreadable, episodes, feedback) = match read {
        Ok(found) => found,
        Err(e) => {
            eprintln!("error: durable agent state: {e}");
            return EXIT_UNAVAILABLE;
        }
    };
    // The project as it is now: its identity, its conditions, and the capabilities each kind of
    // work would be offered here. Nothing is started and the project is not locked.
    let project = crate::work_state::project_scope(&root).unwrap_or_default();
    let conditions = crate::work_state::episode_conditions(
        &root,
        &crate::work_state::project_profile(&root, "", ""),
    );
    let environment = crate::local_environment::LocalEnvironment::new(
        crate::local_environment::opaque_id(&root),
        &root,
        chip_pax::PaxExecutor::new(&root),
        chip_core::EnvironmentDescription::default(),
    );
    let kinds: std::collections::BTreeSet<String> =
        candidates.iter().map(|c| c.kind.clone()).collect();
    let declared = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok()?;
                Some(rt.block_on(async {
                    let mut by_kind = BTreeMap::new();
                    for kind in &kinds {
                        if let Some(goal_kind) = crate::software_work::GoalKind::parse(kind) {
                            let offered = crate::software_work::work_capabilities(
                                goal_kind,
                                &environment,
                                None,
                            );
                            by_kind.insert(
                                kind.clone(),
                                crate::work_state::declared_capabilities(&*offered).await,
                            );
                        }
                    }
                    by_kind
                }))
            })
            .join()
            .ok()
            .flatten()
    });
    let Some(declared) = declared else {
        eprintln!("error: the current capability declarations could not be read");
        return EXIT_UNAVAILABLE;
    };
    let goal_terms = values
        .get("--goal")
        .map(|g| chip_agent_state::intent_terms(g));
    let by_id: BTreeMap<&str, &chip_agent_state::EpisodeRecord> =
        episodes.iter().map(|e| (e.id.as_str(), e)).collect();
    let empty = BTreeMap::new();
    let rows: Vec<Value> = candidates
        .iter()
        .map(|c| {
            let current = chip_agent_state::Current {
                project: &project,
                conditions: &conditions,
                capabilities: declared.get(&c.kind).unwrap_or(&empty),
                task: goal_terms
                    .as_ref()
                    .map(|t| (task_kind.name(), t.as_slice())),
                episode: &|id| by_id.get(id).map(|e| (*e).clone()),
                backing: &|e| store.backing(e),
            };
            let validation = chip_agent_state::validate(c, &candidates, &current);
            // What later attempts recorded after a supporting episode was presented: shown, never
            // used to decide anything.
            let later: Vec<&chip_agent_state::FeedbackRecord> = feedback
                .iter()
                .filter(|f| c.support.iter().any(|s| s.episode == f.episode))
                .collect();
            json!({
                "candidate": c.id,
                "schema": c.schema,
                "procedure_key": c.procedure_key,
                // A digest of the project's path, never the path.
                "project": c.scope.project,
                "kind": c.kind,
                // Digests, not words: only how many there are.
                "goal_terms": c.goal_terms.len(),
                "supporting_observations": c.support.len(),
                "strength": c.strength,
                "support": c.support.iter().map(|s| json!({
                    "episode": s.episode, "outcome": s.outcome.as_str(), "result": s.result_record,
                })).collect::<Vec<_>>(),
                "steps": c.steps.iter().map(|s| json!({
                    "capability": s.capability, "role": s.role, "contract": s.contract,
                    "inputs": s.inputs.iter().map(|i| match &i.source {
                        chip_agent_state::InputSource::Fixed { value } => json!({"name": i.name, "source": "fixed", "value": value}),
                        chip_agent_state::InputSource::Parameter => json!({"name": i.name, "source": "parameter"}),
                    }).collect::<Vec<_>>(),
                    "expected_status": s.expected_status,
                    "evidence": s.evidence,
                })).collect::<Vec<_>>(),
                "required_capabilities": c.required_capabilities,
                "preconditions": c.preconditions,
                "verification": c.verification.as_ref().map(|v| json!({
                    "kind": v.kind, "step": v.step, "host_verdict": v.host_verdict,
                })),
                "unresolved": c.unresolved.iter().map(|u| json!({"code": u.code, "detail": u.detail})).collect::<Vec<_>>(),
                "known_incompatibilities": {
                    "observational_only": true,
                    "later_misleading_feedback": later.iter().filter(|f| f.classification == chip_agent_state::FeedbackClass::Misleading).count(),
                    "later_helpful_feedback": later.iter().filter(|f| f.classification == chip_agent_state::FeedbackClass::Helpful).count(),
                },
                "created_at_ms": c.created_at_ms,
                "extracted_by": c.extracted_by,
                "validation": validation.to_json(),
            })
        })
        .collect();
    if json {
        println!(
            "{}",
            json!({
                "workflows": rows,
                "unreadable": unreadable.iter().map(|(id, why)| json!({"id": id, "error": why})).collect::<Vec<_>>(),
            })
        );
    } else {
        for r in &rows {
            let v = &r["validation"];
            let status = if v["eligible_for_consideration"] == true {
                "eligible for consideration".to_string()
            } else {
                format!(
                    "not eligible: {}",
                    v["codes"]
                        .as_array()
                        .map(|c| c
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", "))
                        .unwrap_or_default()
                )
            };
            println!(
                "{} {} {} step(s), {} supporting episode(s) ({status})",
                r["candidate"].as_str().unwrap_or_default(),
                r["kind"].as_str().unwrap_or_default(),
                r["steps"].as_array().map_or(0, Vec::len),
                r["supporting_observations"],
            );
        }
        for (id, why) in &unreadable {
            println!("{id} unreadable: {why}");
        }
    }
    EXIT_DONE
}

fn open(path: &str) -> Result<AgentStore, i32> {
    AgentStore::open(path).map_err(|e| {
        eprintln!("error: durable agent state: {e}");
        EXIT_UNAVAILABLE
    })
}

fn settlement_json(s: &Settlement) -> Value {
    json!({
        "intent": s.intent_key, "capability": s.capability, "binding": s.binding,
        "executed": s.executed, "decided_by": s.decided_by, "evidence": s.evidence,
        "kind": "operator_settlement",
    })
}

fn history_json(history: &History, settlements: &[Settlement]) -> Value {
    json!({
        "task": history.to_json(),
        "lease_expires_at_ms": history.task.lease.as_ref().map(|l| l.expires_at),
        "intents": history.intents.iter().map(|i| json!({
            "key": i.value.key, "action": without_inputs(&i.value.action),
            "inputs_recorded": i.value.action.contains(" inputs="), "state": i.value.state,
            "result": i.value.result, "conclusion": i.value.conclusion,
        })).collect::<Vec<_>>(),
        "settlements": settlements.iter().map(settlement_json).collect::<Vec<_>>(),
        "last_checkpoint": history.checkpoints.last().map(|c| json!({
            "version": c.version, "actor": c.actor, "status": c.status, "summary": c.summary,
        })),
        "results": history.results.iter().map(|r| json!({
            "attempt": r.attempt, "work_id": r.work_id, "outcome": r.outcome,
            "requested_status": r.requested_status, "verification": r.verification,
            "persistence_failures": r.persistence_failures,
        })).collect::<Vec<_>>(),
        // The response text stays in the database; what is shown is where it is and what it cost.
        "model_responses": history.model_responses.iter().map(|r| json!({
            "attempt": r.attempt, "turn": r.turn, "model": r.model, "response_id": r.response_id,
            "output_bytes": r.output.len(), "output_sha256": r.output_sha256, "usage": r.usage,
            "observed": r.observed, "intents_before": r.intents_before,
        })).collect::<Vec<_>>(),
        // Each open intent: whether the destination could be asked what happened, and every
        // query already made, with what the destination said and why that was not enough.
        "unresolved": history.unresolved_intents().iter().map(|i| {
            let queries: Vec<&chip_agent_state::ReconciliationRecord> = history
                .reconciliations
                .iter()
                .filter(|r| r.intent_key == i.value.key)
                .collect();
            let askable = chip_agent_state::reconcilability(&i.value);
            let tag = chip_agent_state::ActionTag::parse(&i.value.action);
            let capability = tag.as_ref().map(|t| t.capability.clone());
            let supported = capability.as_deref().is_some_and(reconciliation_supported);
            let last = queries.last();
            json!({
                "intent": i.value.key,
                "capability": capability,
                // The key and binding are digests; no input of the request is shown.
                "action_key": tag.as_ref().map(|t| t.key.clone()),
                "binding": tag.as_ref().map(|t| t.binding.clone()),
                "reconcilable": askable.is_ok(),
                "not_reconcilable_because": askable.as_ref().err(),
                "capability_has_a_reconciler": supported,
                "queries": queries.len(),
                "last_finding": last.map(|q| q.finding.clone()),
                "destination_reported": last.map(|q| q.destination.clone()),
                "key_barred": last.map(|q| q.destination == "not_applied_barred"),
                "why_not_resolved": last.and_then(|q| q.reason.clone()),
                // A query only reads and bars; it never performs the operation.
                "another_query_is_safe": askable.is_ok() && supported,
                "manual_settlement_required": !(askable.is_ok() && supported)
                    || queries.last().is_some_and(|q| q.finding == "indeterminate"),
            })
        }).collect::<Vec<_>>(),
        "reconciliations": history.reconciliations.iter().map(|r| json!({
            "intent": r.intent_key, "query_no": r.query_no, "capability": r.capability,
            "action_key": r.action_key, "binding": r.binding, "finding": r.finding,
            "destination": r.destination, "destination_detail": r.destination_detail,
            "business_effect": r.business_effect, "business_detail": r.business_detail,
            "receipt_id": r.receipt_id, "reason": r.reason, "decided_by": r.decided_by,
            "kind": "destination_evidence",
        })).collect::<Vec<_>>(),
        "model_calls_in_flight": history.model_calls_in_flight().iter().map(|c| json!({
            "attempt": c.attempt, "turn": c.turn,
            "outcome": "unknown: the attempt stopped before the call returned; whether the provider received, answered or charged it is not known, and the resumed attempt asks again",
        })).collect::<Vec<_>>(),
        "events": history.events.iter().map(|e| json!({
            "attempt": e.attempt, "index": e.index, "recorded_at_ms": e.recorded_at_ms,
            "provenance": e.provenance.as_str(), "event": e.event,
        })).collect::<Vec<_>>(),
    })
}

/// An intent's `action` without the request's inputs, which can hold anything a model wrote (file
/// content, paths, amounts). The capability, key and binding identify the request without them.
fn without_inputs(action: &str) -> &str {
    action
        .split_once(" inputs=")
        .map_or(action, |(head, _)| head)
}

/// The capabilities whose destination can be asked what happened (see
/// [`crate::project_reconciler`]).
fn reconciliation_supported(capability: &str) -> bool {
    capability == chip_project::PROJECT_WRITE && cfg!(unix)
}

fn reconcile(args: &[String]) -> i32 {
    let (values, json) = match parse(
        args,
        &[
            "--state-db",
            "--task-id",
            "--intent",
            "--decided-by",
            "--project-dir",
        ],
    ) {
        Ok(parsed) => parsed,
        Err(why) => return usage(Some(why)),
    };
    let get = |k: &str| values.get(k).map(String::as_str);
    let (Some(db), Some(task), Some(intent), Some(decided_by)) = (
        get("--state-db"),
        get("--task-id"),
        get("--intent"),
        get("--decided-by"),
    ) else {
        return usage(Some(
            "--state-db, --task-id, --intent and --decided-by are all required".into(),
        ));
    };
    let root = match get("--project-dir") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(e) => {
                eprintln!("error: no current directory ({e})");
                return EXIT_UNAVAILABLE;
            }
        },
    };
    let store = match open(db) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let reconciler = crate::project_reconciler::ProjectWriteReconciler::new(root);
    // The command may be called from inside the product's runtime; ask on a thread of its own.
    let done = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map(|rt| {
                        rt.block_on(chip_agent_state::reconcile(
                            &store,
                            task,
                            intent,
                            decided_by,
                            Some(&reconciler),
                        ))
                    })
            })
            .join()
    });
    let done = match done {
        Ok(Ok(done)) => done,
        _ => {
            eprintln!("error: reconciliation could not be run");
            return EXIT_UNAVAILABLE;
        }
    };
    match done {
        Ok(r) => {
            let reason = match &r.finding {
                chip_agent_state::Finding::Indeterminate { reason } => Some(reason.clone()),
                _ => None,
            };
            if json {
                println!(
                    "{}",
                    json!({
                        "command": "agent-state reconcile", "intent": intent,
                        "finding": r.finding.name(), "resolved": r.resolved,
                        "record": r.record_id, "reason": reason,
                    })
                );
            } else if r.resolved {
                println!(
                    "Intent {intent} of task {task}: {}. Resolved from the destination's own record; the operation was not performed.",
                    r.finding.name()
                );
            } else {
                println!(
                    "Intent {intent} of task {task} stays unresolved: {}. Nothing was performed; settle it with `agent-state settle` if you can establish what happened.",
                    reason.as_deref().unwrap_or("indeterminate")
                );
            }
            if r.resolved { EXIT_DONE } else { EXIT_REFUSED }
        }
        Err(e @ StateError::ReconciliationRefused { .. }) => {
            eprintln!("error: {e}");
            EXIT_REFUSED
        }
        Err(e) => {
            eprintln!("error: durable agent state: {e}");
            EXIT_UNAVAILABLE
        }
    }
}

fn show(args: &[String]) -> i32 {
    let (values, json) = match parse(args, &["--state-db", "--task-id"]) {
        Ok(parsed) => parsed,
        Err(why) => return usage(Some(why)),
    };
    let Some(task) = values.get("--task-id") else {
        return usage(Some("--task-id is required".into()));
    };
    let db = match database(values.get("--state-db")) {
        Ok(db) => db,
        Err(code) => return code,
    };
    let store = match open(&db) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let read = store
        .history(task)
        .and_then(|h| Ok((h, store.settlements(task)?)));
    match read {
        Ok((Some(history), settlements)) => {
            if json {
                println!("{}", history_json(&history, &settlements));
            } else {
                let t = &history.task;
                println!(
                    "Task {}: {} (owner {}, fence {}, checkpoint {}).",
                    t.task_id, t.status, t.owner, t.fence, t.checkpoint_version
                );
                for i in &history.intents {
                    println!(
                        "  intent {}: {} ({})",
                        i.value.key,
                        i.value.state,
                        without_inputs(&i.value.action)
                    );
                }
                for e in &history.events {
                    println!(
                        "  event a{}#{} {} [{}]",
                        e.attempt,
                        e.index,
                        e.event["kind"].as_str().unwrap_or("?"),
                        e.provenance.as_str()
                    );
                }
                for r in &history.model_responses {
                    println!(
                        "  model response a{} turn {}: {} bytes from {}",
                        r.attempt,
                        r.turn,
                        r.output.len(),
                        r.model
                    );
                }
                for c in history.model_calls_in_flight() {
                    println!(
                        "  model call a{} turn {}: in flight when the attempt stopped; outcome unknown (it may have been charged), not replayed",
                        c.attempt, c.turn
                    );
                }
                for r in &history.results {
                    if let Some(v) = &r.verification {
                        println!(
                            "  result a{}: {} (verified {}, accepted {}, exit {})",
                            r.attempt, r.outcome, v.verified, v.accepted, v.exit_status
                        );
                    }
                }
                for i in history.unresolved_intents() {
                    if let Err(why) = chip_agent_state::reconcilability(&i.value) {
                        println!("  intent {}: cannot be reconciled: {why}", i.value.key);
                    }
                }
                for r in &history.reconciliations {
                    println!(
                        "  reconciliation {} #{}: {} (destination {}: {}; business effect {}{}){}",
                        r.intent_key,
                        r.query_no,
                        r.finding,
                        r.destination,
                        r.destination_detail,
                        r.business_effect,
                        if r.business_detail.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", r.business_detail)
                        },
                        r.reason
                            .as_deref()
                            .map(|w| format!(" - insufficient: {w}"))
                            .unwrap_or_default()
                    );
                }
                for s in &settlements {
                    println!(
                        "  settled {}: {} by {} (an operator's conclusion): {}",
                        s.intent_key,
                        if s.executed {
                            "executed"
                        } else {
                            "not executed"
                        },
                        s.decided_by.as_deref().unwrap_or("an unrecorded operator"),
                        s.evidence
                    );
                }
            }
            EXIT_DONE
        }
        Ok((None, _)) => {
            eprintln!("error: there is no task {task} in {db}");
            EXIT_REFUSED
        }
        Err(e) => {
            eprintln!("error: durable agent state: {e}");
            EXIT_UNAVAILABLE
        }
    }
}

fn export(args: &[String]) -> i32 {
    let mut args = args.to_vec();
    let send = match args.iter().position(|a| a == "--send") {
        Some(at) => {
            args.remove(at);
            true
        }
        None => false,
    };
    let (values, json) = match parse(&args, &["--state-db", "--task-id", "--provider", "--model"]) {
        Ok(parsed) => parsed,
        Err(why) => return usage(Some(why)),
    };
    let Some(task) = values.get("--task-id") else {
        return usage(Some("--task-id is required".into()));
    };
    let db = match database(values.get("--state-db")) {
        Ok(db) => db,
        Err(code) => return code,
    };
    let store = match open(&db) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let history = match store.history(task) {
        Ok(Some(history)) => history,
        Ok(None) => {
            eprintln!("error: there is no task {task} in {db}");
            return EXIT_REFUSED;
        }
        Err(e) => {
            eprintln!("error: durable agent state: {e}");
            return EXIT_UNAVAILABLE;
        }
    };
    let env = |name: &str| std::env::var(name).ok();
    let config = if send {
        match crate::telemetry::configured(&env) {
            Ok(Some(config)) => config,
            Ok(None) => {
                eprintln!(
                    "error: --send needs an export destination: OTEL_EXPORTER_OTLP_ENDPOINT (a loopback collector) or STACK_AGENT_OTLP_FILE"
                );
                return EXIT_USAGE;
            }
            Err(why) => {
                eprintln!("error: telemetry export: {why}");
                return EXIT_REFUSED;
            }
        }
    } else {
        // Printing reads the service name the same way sending does, and nothing else.
        chip_otel::export::Config {
            sinks: Vec::new(),
            service_name: env("OTEL_SERVICE_NAME"),
        }
    };
    let mut context = crate::telemetry::context(&config, "", "");
    context.provider = values.get("--provider").cloned();
    context.request_model = values.get("--model").cloned();
    if !send {
        println!("{}", chip_otel::trace(&history, &context));
        return EXIT_DONE;
    }
    let report = crate::telemetry::export_history(&config, &history, &context);
    if json {
        println!("{report}");
    } else {
        for sink in report["sinks"].as_array().into_iter().flatten() {
            match sink["error"].as_str() {
                None => println!(
                    "exported trace {} to {}",
                    report["trace_id"].as_str().unwrap_or_default(),
                    sink["sink"].as_str().unwrap_or_default()
                ),
                Some(why) => eprintln!(
                    "error: export to {} failed: {why}",
                    sink["sink"].as_str().unwrap_or_default()
                ),
            }
        }
    }
    if report["exported"] == true {
        EXIT_DONE
    } else {
        EXIT_UNAVAILABLE
    }
}

fn settle(args: &[String]) -> i32 {
    let (values, json) = match parse(
        args,
        &[
            "--state-db",
            "--task-id",
            "--intent",
            "--executed",
            "--decided-by",
            "--evidence",
        ],
    ) {
        Ok(parsed) => parsed,
        Err(why) => return usage(Some(why)),
    };
    let get = |k: &str| values.get(k).map(String::as_str);
    let (Some(db), Some(task), Some(intent), Some(executed), Some(decided_by), Some(evidence)) = (
        get("--state-db"),
        get("--task-id"),
        get("--intent"),
        get("--executed"),
        get("--decided-by"),
        get("--evidence"),
    ) else {
        return usage(Some(
            "--state-db, --task-id, --intent, --executed, --decided-by and --evidence are all required: a settlement names who decided and on what evidence".into(),
        ));
    };
    let executed = match executed {
        "true" => true,
        "false" => false,
        other => {
            return usage(Some(format!(
                "--executed takes true or false, not {other:?}"
            )));
        }
    };
    let store = match open(db) {
        Ok(store) => store,
        Err(code) => return code,
    };
    match chip_agent_state::settle(&store, task, intent, executed, decided_by, evidence) {
        Ok(settled) => {
            if json {
                println!(
                    "{}",
                    json!({
                        "command": "agent-state settle",
                        "settled": settlement_json(&settled.settlement),
                        "record": settled.record_id,
                        "state": history_json(&settled.history, std::slice::from_ref(&settled.settlement)),
                    })
                );
            } else {
                println!(
                    "Settled intent {intent} of task {task}: {} by {decided_by}. This is your conclusion, not an observation; the task can be resumed now.",
                    if executed {
                        "the effect occurred, so it will not be performed again"
                    } else {
                        "the effect did not occur, so the work may perform it if it still needs to"
                    },
                );
            }
            EXIT_DONE
        }
        Err(e @ StateError::SettlementRefused { .. }) => {
            eprintln!("error: {e}");
            EXIT_REFUSED
        }
        Err(e) => {
            eprintln!("error: durable agent state: {e}");
            EXIT_UNAVAILABLE
        }
    }
}
