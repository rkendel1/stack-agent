//! Project-scoped and runtime-scoped learning for `stack-agent work`.
//!
//! Before a run: both stores are located, applicable behaviors are retrieved (bounded) and
//! re-validated against the run documents they reference. With `--auto`, the outcome records of
//! qualifying behaviors become evidence for the existing resolver, which can only rank eligible
//! configurations. Without `--auto`, they are reported and select nothing.
//!
//! After a run: one run document per scope is written from host-observed evidence only (the
//! autoconfig outcome record and the work's own counters), and behaviors are created or updated
//! when the outcome bears on success. No model is asked anything for learning, and a storage
//! failure is reported as such without touching the task's outcome.
//!
//! See `docs/product/learning.md`.

pub mod document;
pub mod policy;
pub mod store;

use std::path::Path;

use document::{
    Actions, Applicability, BEHAVIOR_SCHEMA, Behavior, BehaviorKey, Lesson, LessonKind,
    MAX_REFERENCES, RUN_SCHEMA, Reference, RunDocument, Scope, StartingContext, Status, sha,
};
use policy::Assessed;
use store::{ProjectIdentity, Store};

use crate::autoconfig::catalog::CATALOG_VERSION;
use crate::autoconfig::evidence::{OutcomeClass, OutcomeRecord};
use crate::autoconfig::profile::{Attr, ProjectProfile};
use crate::software_work::{GoalKind, SoftwareWork};

/// How long a run document supports conclusions.
pub const VALID_DAYS: u64 = policy::STALE_DAYS;

pub fn runtime_line() -> String {
    let v = env!("CARGO_PKG_VERSION");
    let mut parts = v.split('.');
    format!(
        "{}.{}",
        parts.next().unwrap_or("0"),
        parts.next().unwrap_or("0")
    )
}

pub struct ScopeState {
    pub store: Result<Store, String>,
    pub retrieved: Vec<Assessed>,
    pub diagnostics: Vec<String>,
}

pub struct Learning {
    pub project: ScopeState,
    pub runtime: ScopeState,
    pub project_identity: Option<ProjectIdentity>,
    task_kind: String,
    build_system: Option<String>,
}

fn retrieve(
    store: &Store,
    expected_project: Option<&str>,
    task_kind: &str,
    build_system: Option<&str>,
    now: u64,
) -> (Vec<Assessed>, Vec<String>) {
    let (behaviors, mut diagnostics) = store.load_behaviors();
    let line = runtime_line();
    let assessed: Vec<Assessed> = behaviors
        .into_iter()
        .filter(|b| policy::applies(b, task_kind, build_system))
        .map(|b| {
            let mut a = policy::assess(store, b, now, CATALOG_VERSION, &line);
            // A project document must belong to this project.
            if let Some(expected) = expected_project {
                let foreign = a
                    .documents
                    .iter()
                    .filter(|d| d.project.as_deref() != Some(expected))
                    .count();
                if foreign > 0 {
                    a.documents
                        .retain(|d| d.project.as_deref() == Some(expected));
                    a.problems.push(format!(
                        "{foreign} referenced document(s) belong to another project"
                    ));
                    let behavior = a.behavior.clone();
                    let kept: Vec<String> = a.documents.iter().map(|d| d.id.clone()).collect();
                    let mut b2 = behavior;
                    b2.references.retain(|r| kept.contains(&r.document));
                    let problems = a.problems.clone();
                    a = policy::assess(store, b2, now, CATALOG_VERSION, &line);
                    a.problems.extend(problems);
                    if a.status == Status::Validated {
                        a.status = Status::Candidate;
                    }
                }
            }
            a
        })
        .collect();
    for a in &assessed {
        for p in &a.problems {
            diagnostics.push(format!("behavior {}: {p}", short(&a.behavior.id)));
        }
    }
    (policy::order(assessed), diagnostics)
}

fn short(id: &str) -> &str {
    let hex = id.strip_prefix("sha256:").unwrap_or(id);
    &hex[..hex.len().min(12)]
}

impl Learning {
    /// Locates both stores and retrieves applicable behaviors. Never fails: an unavailable store is
    /// a reported state.
    pub fn prepare(
        project_root: &Path,
        profile: &ProjectProfile,
        kind: GoalKind,
        now: u64,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let task_kind = kind.name().to_string();
        let build_system = profile
            .primary_build_system
            .known()
            .map(|b| b.name().to_string());
        let (project_identity, identity_problem) = match store::read_project_identity(project_root)
        {
            Ok(i) => (i, None),
            Err(e) => (None, Some(e)),
        };
        let project = if !project_root.is_dir() {
            ScopeState {
                store: Err("no project root".into()),
                retrieved: vec![],
                diagnostics: vec![],
            }
        } else if let Some(problem) = identity_problem {
            ScopeState {
                store: Err(format!(
                    "project learning is unusable: {problem}; nothing is read from it or written to it"
                )),
                retrieved: vec![],
                diagnostics: vec![],
            }
        } else {
            let store = Store::project(project_root);
            let (retrieved, diagnostics) = match &project_identity {
                Some(identity) => retrieve(
                    &store,
                    Some(&identity.id),
                    &task_kind,
                    build_system.as_deref(),
                    now,
                ),
                None => (vec![], vec![]),
            };
            ScopeState {
                store: Ok(store),
                retrieved,
                diagnostics,
            }
        };
        let runtime = match Store::runtime(env) {
            Ok((store, _)) => {
                let (retrieved, diagnostics) =
                    retrieve(&store, None, &task_kind, build_system.as_deref(), now);
                ScopeState {
                    store: Ok(store),
                    retrieved,
                    diagnostics,
                }
            }
            Err(e) => ScopeState {
                store: Err(e),
                retrieved: vec![],
                diagnostics: vec![],
            },
        };
        Learning {
            project,
            runtime,
            project_identity,
            task_kind,
            build_system,
        }
    }

    /// The evidence learning hands to `--auto`: project records first; a runtime record of a run the
    /// project already contributed is dropped (it is the same outcome).
    pub fn evidence(&self) -> Vec<OutcomeRecord> {
        let mut out = policy::evidence(Scope::Project, &self.project.retrieved);
        let runs: std::collections::BTreeSet<String> = out.iter().map(|r| r.run.clone()).collect();
        out.extend(
            policy::evidence(Scope::Runtime, &self.runtime.retrieved)
                .into_iter()
                .filter(|r| !runs.contains(&r.run)),
        );
        out
    }

    /// Writes the run's documents and updates behaviors. `outcome` is `None` for a run that never
    /// started (an `--auto` refusal).
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        project_root: &Path,
        profile: &ProjectProfile,
        configuration: Option<&str>,
        work: Option<&SoftwareWork>,
        outcome: Option<&OutcomeRecord>,
        run: &str,
        now: u64,
        refusal: Option<&[String]>,
    ) -> Recorded {
        let configuration = configuration.unwrap_or("baseline").to_string();
        let class = outcome.map_or(OutcomeClass::Blocked, |o| o.outcome);
        let key = BehaviorKey {
            task_kind: self.task_kind.clone(),
            build_system: self.build_system.clone(),
            configuration: configuration.clone(),
        };
        let updates = policy::bears_on_success(class);

        // Project scope.
        let project = match &self.project.store {
            Err(e) => ScopeWrite::skipped(Scope::Project, e.clone()),
            Ok(store) => match store::ensure_project_identity(project_root, now) {
                Err(e) => ScopeWrite::skipped(
                    Scope::Project,
                    format!("project learning not persisted: {e}"),
                ),
                Ok(identity) => {
                    self.project_identity = Some(identity.clone());
                    let doc = self.document(
                        Scope::Project,
                        profile,
                        &configuration,
                        work,
                        outcome,
                        run,
                        now,
                        Some(identity.id.clone()),
                        None,
                        refusal,
                        updates.then(|| Behavior::id_for(Scope::Project, &key)),
                    );
                    write_scope(store, doc, updates.then(|| key.clone()), now)
                }
            },
        };
        // Runtime scope: generic facts only, and a salted token instead of the project's identity.
        let runtime = match &self.runtime.store {
            Err(e) => ScopeWrite::skipped(
                Scope::Runtime,
                format!("runtime learning not persisted: {e}"),
            ),
            Ok(store) => match store::runtime_salt(store) {
                Err(e) => ScopeWrite::skipped(
                    Scope::Runtime,
                    format!("runtime learning not persisted: {e}"),
                ),
                Ok(salt) => {
                    let token = self
                        .project_identity
                        .as_ref()
                        .map(|i| sha(&format!("{salt}|{}", i.id)));
                    let doc = self.document(
                        Scope::Runtime,
                        profile,
                        &configuration,
                        work,
                        outcome,
                        run,
                        now,
                        None,
                        token,
                        refusal,
                        updates.then(|| Behavior::id_for(Scope::Runtime, &key)),
                    );
                    write_scope(store, doc, updates.then(|| key.clone()), now)
                }
            },
        };
        Recorded { project, runtime }
    }

    #[allow(clippy::too_many_arguments)]
    fn document(
        &self,
        scope: Scope,
        profile: &ProjectProfile,
        configuration: &str,
        work: Option<&SoftwareWork>,
        outcome: Option<&OutcomeRecord>,
        run: &str,
        now: u64,
        project: Option<String>,
        project_token: Option<String>,
        refusal: Option<&[String]>,
        behavior: Option<String>,
    ) -> RunDocument {
        let class = outcome.map_or(OutcomeClass::Blocked, |o| o.outcome);
        let record = outcome.map(|o| match scope {
            Scope::Project => o.clone(),
            Scope::Runtime => {
                let mut r = o.clone();
                r.verification_refs.clear();
                r.selection_id = None;
                r.seal()
            }
        });
        let attr_bool = |a: &Attr<bool>| a.known().copied();
        let lessons = lessons(
            scope,
            work,
            record.as_ref(),
            class,
            configuration,
            &self.task_kind,
            refusal,
        );
        let mut evidence: Vec<String> = record.iter().map(|r| r.id.clone()).collect();
        if scope == Scope::Project {
            evidence.extend(
                record
                    .iter()
                    .flat_map(|r| r.verification_refs.iter().cloned()),
            );
        }
        RunDocument {
            schema: RUN_SCHEMA.into(),
            id: String::new(),
            scope,
            run: run.to_string(),
            runtime_version: env!("CARGO_PKG_VERSION").into(),
            task_kind: self.task_kind.clone(),
            project,
            project_token,
            starting_context: StartingContext {
                build_system: self.build_system.clone(),
                has_tests: attr_bool(&profile.has_tests),
                git_repository: attr_bool(&profile.git_repository),
                verification: profile
                    .verification
                    .known()
                    .map(|v| format!("{v:?}").to_lowercase()),
                profile_version: profile.version.into(),
                profile_digest: (scope == Scope::Project).then(|| profile.digest()),
            },
            actions: Actions {
                capabilities_invoked: work
                    .map(|w| w.utility.executions_by_capability.clone())
                    .unwrap_or_default(),
                model_calls: work.map_or(0, |w| w.utility.model_calls),
                decisions: work.map_or(0, |w| w.report.decisions.len()),
                failed_executions: work.map_or(0, |w| w.utility.failed_observations),
            },
            outcome: class,
            terminal_state: work.map_or("not_run".into(), |w| {
                w.report.outcome.terminal_state().name().into()
            }),
            outcome_record: record,
            lessons,
            evidence,
            applicability: Applicability {
                task_kind: self.task_kind.clone(),
                build_system: self.build_system.clone(),
                configuration: configuration.into(),
                catalog_version: CATALOG_VERSION.into(),
                runtime_line: runtime_line(),
                recorded_at: now,
                valid_days: VALID_DAYS,
            },
            limitations: vec![
                "derived from host-observed evidence only; no model wrote any of it".into(),
                "one run is a historical record, not a validated behavior".into(),
            ],
            behaviors: behavior.into_iter().collect(),
        }
        .seal()
    }
}

/// Deterministic lessons from what the host observed. Facts are observations; hypotheses are
/// labelled as such and never count as evidence of anything.
fn lessons(
    scope: Scope,
    work: Option<&SoftwareWork>,
    record: Option<&OutcomeRecord>,
    class: OutcomeClass,
    configuration: &str,
    task_kind: &str,
    refusal: Option<&[String]>,
) -> Vec<Lesson> {
    let mut out = Vec::new();
    let rid: Vec<String> = record.iter().map(|r| r.id.clone()).collect();
    let fact = |statement: String, evidence: Vec<String>| Lesson {
        kind: LessonKind::Fact,
        statement,
        evidence,
    };
    out.push(fact(
        format!(
            "{task_kind} work with {configuration} ended {} (host-classified)",
            class.name()
        ),
        rid.clone(),
    ));
    if let Some(reasons) = refusal {
        out.push(fact(
            format!(
                "automatic configuration refused the work: {} reason(s) recorded in the selection",
                reasons.len()
            ),
            vec![],
        ));
    }
    let Some(w) = work else { return out };
    if scope == Scope::Project {
        let last = w.report.observations.iter().rev().find_map(|o| {
            let first = o.output.as_deref()?.lines().next()?;
            chip_pax::parse_execution_result(first.as_bytes()).ok()
        });
        if let Some(r) = last {
            out.push(fact(
                format!(
                    "the last test run reported {} ({})",
                    r.status.as_str(),
                    r.reason
                ),
                record
                    .map(|r| r.verification_refs.clone())
                    .unwrap_or_default(),
            ));
        }
        if w.evidence_invalidated.is_some() {
            out.push(fact(
                "passing test evidence was refused (tampering or a change after the run)".into(),
                rid.clone(),
            ));
        }
        if w.utility.failed_observations > 0 {
            out.push(fact(
                format!(
                    "{} capability execution(s) failed",
                    w.utility.failed_observations
                ),
                rid.clone(),
            ));
        }
    }
    if let chip_core::WorkOutcome::LimitReached { limit } = &w.report.outcome {
        out.push(fact(
            format!("the work reached its {} limit", limit.name()),
            rid.clone(),
        ));
        out.push(Lesson {
            kind: LessonKind::Hypothesis,
            statement: format!("this kind of task may need a larger {} limit", limit.name()),
            evidence: rid,
        });
    }
    out
}

/// What one scope's write did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScopeWrite {
    pub scope: Scope,
    pub persisted: bool,
    pub document: Option<String>,
    pub path: Option<String>,
    pub error: Option<String>,
    pub behaviors: Vec<serde_json::Value>,
    pub behavior_errors: Vec<String>,
}

impl ScopeWrite {
    fn skipped(scope: Scope, why: String) -> Self {
        ScopeWrite {
            scope,
            persisted: false,
            document: None,
            path: None,
            error: Some(why),
            behaviors: vec![],
            behavior_errors: vec![],
        }
    }
}

fn write_scope(store: &Store, doc: RunDocument, key: Option<BehaviorKey>, now: u64) -> ScopeWrite {
    let scope = store.scope;
    let path = match store.write_run(&doc) {
        Ok(p) => p,
        Err(e) => {
            return ScopeWrite::skipped(
                scope,
                format!("{} learning not persisted: {e}", scope.name()),
            );
        }
    };
    let mut w = ScopeWrite {
        scope,
        persisted: true,
        document: Some(doc.id.clone()),
        path: Some(format!("{}/{path}", store.shown)),
        error: None,
        behaviors: vec![],
        behavior_errors: vec![],
    };
    let Some(key) = key else { return w };
    let id = Behavior::id_for(scope, &key);
    // Re-read right before writing and merge, so a concurrent writer's reference is kept.
    let existing = match store.load_behavior(&id) {
        Ok(b) => b,
        Err(e) => {
            w.behavior_errors.push(format!(
                "behavior {} is invalid ({e}); it was left as it is, not overwritten",
                short(&id)
            ));
            return w;
        }
    };
    let mut b = existing.unwrap_or_else(|| Behavior {
        schema: BEHAVIOR_SCHEMA.into(),
        id: id.clone(),
        scope,
        proposal: match key.configuration.as_str() {
            "baseline" => format!(
                "for {} work on {} projects, the unconfigured behavior",
                key.task_kind,
                key.build_system.as_deref().unwrap_or("unrecognized")
            ),
            c => format!(
                "for {} work on {} projects, use configuration {c}",
                key.task_kind,
                key.build_system.as_deref().unwrap_or("unrecognized")
            ),
        },
        key: key.clone(),
        catalog_version: CATALOG_VERSION.into(),
        runtime_line: runtime_line(),
        references: vec![],
        recorded_status: Status::Candidate,
        recorded_support: 0,
        recorded_contradiction: 0,
        updated_at: now,
        limitations: vec![
            "advisory: it can only inform ranking among eligible configurations under --auto"
                .into(),
        ],
    });
    if !b.references.iter().any(|r| r.document == doc.id) {
        b.references.push(Reference {
            document: doc.id.clone(),
            path,
        });
    }
    if b.references.len() > MAX_REFERENCES {
        let excess = b.references.len() - MAX_REFERENCES;
        b.references.drain(..excess);
    }
    b.catalog_version = CATALOG_VERSION.into();
    b.runtime_line = runtime_line();
    let a = policy::assess(store, b.clone(), now, CATALOG_VERSION, &runtime_line());
    b.recorded_status = a.status;
    b.recorded_support = a.support;
    b.recorded_contradiction = a.contradiction;
    b.updated_at = now;
    match store.write_behavior(&b) {
        Ok(()) => w.behaviors.push(serde_json::json!({"id": b.id, "status": a.status.name(), "support": a.support, "contradiction": a.contradiction})),
        Err(e) => w.behavior_errors.push(format!("behavior {} not persisted: {e}", short(&id))),
    }
    w
}

pub struct Recorded {
    pub project: ScopeWrite,
    pub runtime: ScopeWrite,
}

fn retrieved_json(s: &ScopeState) -> serde_json::Value {
    serde_json::json!({
        "store": s.store.as_ref().ok().map(|st| st.shown.clone()),
        "unavailable": s.store.as_ref().err(),
        "behaviors": s.retrieved.iter().map(|a| serde_json::json!({
            "id": a.behavior.id,
            "status": a.status.name(),
            "proposal": a.behavior.proposal,
            "support": a.support,
            "contradiction": a.contradiction,
            "distinct_projects": a.distinct_projects,
            "references": a.behavior.references.iter().map(|r| r.path.clone()).collect::<Vec<_>>(),
            "problems": a.problems,
        })).collect::<Vec<_>>(),
        "diagnostics": s.diagnostics,
    })
}

impl Learning {
    pub fn report_json(
        &self,
        recorded: Option<&Recorded>,
        evidence_for_auto: Option<usize>,
    ) -> serde_json::Value {
        serde_json::json!({
            "project": {"retrieved": retrieved_json(&self.project), "written": recorded.map(|r| &r.project)},
            "runtime": {"retrieved": retrieved_json(&self.runtime), "written": recorded.map(|r| &r.runtime)},
            "evidence_for_auto": evidence_for_auto,
            "note": "learned behaviors are advisory: they can only rank eligible configurations under --auto, and never change capabilities, authorization or verification",
        })
    }

    pub fn report_human(
        &self,
        recorded: Option<&Recorded>,
        evidence_for_auto: Option<usize>,
    ) -> String {
        let mut out = String::from("Learning:\n");
        for (name, s, w) in [
            ("project", &self.project, recorded.map(|r| &r.project)),
            ("runtime", &self.runtime, recorded.map(|r| &r.runtime)),
        ] {
            match &s.store {
                Ok(st) => out.push_str(&format!(
                    "  {name} ({}): {} applicable behavior(s)",
                    st.shown,
                    s.retrieved.len()
                )),
                Err(e) => out.push_str(&format!("  {name}: unavailable ({e})")),
            }
            for a in &s.retrieved {
                out.push_str(&format!(
                    "; {} {} ({} for, {} against)",
                    short(&a.behavior.id),
                    a.status.name(),
                    a.support,
                    a.contradiction
                ));
            }
            out.push('\n');
            for d in &s.diagnostics {
                out.push_str(&format!("    diagnostic: {d}\n"));
            }
            if let Some(w) = w {
                match (&w.path, &w.error) {
                    (Some(p), _) => out.push_str(&format!("    wrote {p}\n")),
                    (None, Some(e)) => out.push_str(&format!("    NOT persisted: {e}\n")),
                    _ => {}
                }
                for e in &w.behavior_errors {
                    out.push_str(&format!("    {e}\n"));
                }
            }
        }
        if let Some(n) = evidence_for_auto {
            out.push_str(&format!(
                "  {n} learned outcome record(s) informed --auto ranking\n"
            ));
        }
        out
    }
}
