//! Learning documents and stores: integrity, bounds, reference confinement, status recomputation,
//! promotion rules and scope isolation. Deterministic; nothing runs.

use std::path::PathBuf;

use chip_cli::autoconfig::evidence::{
    Features, Origin, OutcomeClass, OutcomeRecord, RECORD_SCHEMA,
};
use chip_cli::learning::document::{
    Actions, Applicability, BEHAVIOR_SCHEMA, Behavior, BehaviorKey, Lesson, LessonKind, RUN_SCHEMA,
    Reference, RunDocument, Scope, StartingContext, Status, file_name,
};
use chip_cli::learning::policy::{assess, evidence};
use chip_cli::learning::store::{Store, ensure_project_identity, read_project_identity};

const NOW: u64 = 20_000 * 86_400;
const CATALOG: &str = "catalog.v1";

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("stack-agent-learning-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn line() -> String {
    chip_cli::learning::runtime_line()
}

fn key(config: &str) -> BehaviorKey {
    BehaviorKey {
        task_kind: "change".into(),
        build_system: Some("cargo".into()),
        configuration: config.into(),
    }
}

fn record(run: &str, config: &str, outcome: OutcomeClass, scope: Scope) -> OutcomeRecord {
    OutcomeRecord {
        schema: RECORD_SCHEMA.into(),
        record: "outcome".into(),
        id: String::new(),
        selection_id: None,
        run: run.into(),
        configuration: (config != "baseline").then(|| config.to_string()),
        origin: Origin::Production,
        features: Features {
            task_kind: "change".into(),
            build_system: Some("cargo".into()),
        },
        recorded_at: NOW,
        outcome,
        terminal_state: "completed".into(),
        verified: outcome == OutcomeClass::VerifiedSuccess,
        delivery: None,
        cancellation: None,
        capabilities_invoked: Default::default(),
        verification_refs: if scope == Scope::Project {
            vec!["sha256:00".into()]
        } else {
            vec![]
        },
        duration_ms: None,
        tokens: None,
        cost: None,
        limitations: vec![],
    }
    .seal()
}

#[allow(clippy::too_many_arguments)]
fn doc(
    scope: Scope,
    run: &str,
    config: &str,
    outcome: OutcomeClass,
    age_days: u64,
    project: &str,
) -> RunDocument {
    let mut r = record(run, config, outcome, scope);
    r.recorded_at = NOW - age_days * 86_400;
    let r = r.seal();
    RunDocument {
        schema: RUN_SCHEMA.into(),
        id: String::new(),
        scope,
        run: run.into(),
        runtime_version: env!("CARGO_PKG_VERSION").into(),
        task_kind: "change".into(),
        project: (scope == Scope::Project).then(|| project.to_string()),
        project_token: (scope == Scope::Runtime).then(|| format!("sha256:{project}")),
        starting_context: StartingContext {
            build_system: Some("cargo".into()),
            has_tests: Some(true),
            git_repository: Some(true),
            verification: Some("paxcargo".into()),
            profile_version: "profile.v1".into(),
            profile_digest: (scope == Scope::Project).then(|| "sha256:ff".to_string()),
        },
        actions: Actions {
            capabilities_invoked: Default::default(),
            model_calls: 2,
            decisions: 2,
            failed_executions: 0,
        },
        outcome,
        terminal_state: "completed".into(),
        outcome_record: Some(r.clone()),
        lessons: vec![Lesson {
            kind: LessonKind::Fact,
            statement: "x".into(),
            evidence: vec![r.id.clone()],
        }],
        evidence: vec![r.id],
        applicability: Applicability {
            task_kind: "change".into(),
            build_system: Some("cargo".into()),
            configuration: config.into(),
            catalog_version: CATALOG.into(),
            runtime_line: line(),
            recorded_at: NOW - age_days * 86_400,
            valid_days: 180,
        },
        limitations: vec![],
        behaviors: vec![],
    }
    .seal()
}

fn behavior(scope: Scope, config: &str, docs: &[&RunDocument], recorded: Status) -> Behavior {
    let k = key(config);
    Behavior {
        schema: BEHAVIOR_SCHEMA.into(),
        id: Behavior::id_for(scope, &k),
        scope,
        key: k,
        proposal: "use it".into(),
        catalog_version: CATALOG.into(),
        runtime_line: line(),
        references: docs
            .iter()
            .map(|d| Reference {
                document: d.id.clone(),
                path: format!("runs/{}", file_name(&d.id).unwrap()),
            })
            .collect(),
        recorded_status: recorded,
        recorded_support: 99,
        recorded_contradiction: 0,
        updated_at: NOW,
        limitations: vec![],
    }
}

fn project_store(tag: &str) -> (PathBuf, Store) {
    let root = temp(tag);
    (root.clone(), Store::project(&root))
}

fn runtime_store(tag: &str) -> Store {
    let base = temp(tag);
    Store::runtime(&|n: &str| {
        (n == "STACK_AGENT_DATA_DIR").then(|| base.to_string_lossy().into_owned())
    })
    .unwrap()
    .0
}

const FULL: &str = "change.cargo-verified-full-context@1";

// ---- document integrity --------------------------------------------------------------------------

#[test]
fn documents_round_trip_deterministically_and_tampering_is_refused() {
    let d = doc(
        Scope::Project,
        "r1",
        FULL,
        OutcomeClass::VerifiedSuccess,
        0,
        "proj_a",
    );
    let again = doc(
        Scope::Project,
        "r1",
        FULL,
        OutcomeClass::VerifiedSuccess,
        0,
        "proj_a",
    );
    assert_eq!(d.to_text(), again.to_text(), "deterministic");
    assert_eq!(
        RunDocument::from_text(&d.to_text(), Scope::Project).unwrap(),
        d
    );
    // A forged success: the outcome changed, the id not.
    let mut v: serde_json::Value = serde_json::from_str(&d.to_text()).unwrap();
    v["outcome"] = "verified_success".into();
    v["terminal_state"] = "blocked".into();
    assert!(
        RunDocument::from_text(&v.to_string(), Scope::Project)
            .unwrap_err()
            .0
            .contains("altered or forged")
    );
    // An outcome record that disagrees with the document.
    let mut forged = d.clone();
    forged.outcome = OutcomeClass::VerifiedFailure;
    let forged = forged.seal();
    assert!(
        RunDocument::from_text(&forged.to_text(), Scope::Project)
            .unwrap_err()
            .0
            .contains("outcome record")
    );
    // Unsupported schema, wrong scope, oversize.
    let mut v: serde_json::Value = serde_json::from_str(&d.to_text()).unwrap();
    v["schema"] = "stack-agent.learning.run.v0".into();
    assert!(
        RunDocument::from_text(&v.to_string(), Scope::Project)
            .unwrap_err()
            .0
            .contains("unsupported schema")
    );
    assert!(RunDocument::from_text(&d.to_text(), Scope::Runtime).is_err());
    assert!(
        RunDocument::from_text(&"x".repeat(17 * 1024), Scope::Project)
            .unwrap_err()
            .0
            .contains("larger")
    );
}

#[test]
fn runtime_documents_carry_no_project_specific_identity_or_evidence() {
    let mut d = doc(
        Scope::Runtime,
        "r1",
        FULL,
        OutcomeClass::VerifiedSuccess,
        0,
        "a",
    );
    assert!(d.validate(Scope::Runtime).is_ok());
    d.project = Some("proj_0123456789abcdef".into());
    assert!(d.clone().seal().validate(Scope::Runtime).is_err());
    let mut d = doc(
        Scope::Runtime,
        "r1",
        FULL,
        OutcomeClass::VerifiedSuccess,
        0,
        "a",
    );
    let mut r = d.outcome_record.clone().unwrap();
    r.verification_refs = vec!["sha256:00".into()];
    d.outcome_record = Some(r.seal());
    assert!(
        d.seal()
            .validate(Scope::Runtime)
            .unwrap_err()
            .0
            .contains("project-specific evidence")
    );
}

#[test]
fn references_cannot_leave_the_learning_root() {
    let (_, store) = project_store("escape");
    let d = doc(
        Scope::Project,
        "r1",
        FULL,
        OutcomeClass::VerifiedSuccess,
        0,
        "proj_a",
    );
    let mut b = behavior(Scope::Project, FULL, &[&d], Status::Validated);
    for bad in [
        "../../etc/passwd",
        "runs/../../secret.json",
        "/etc/passwd",
        "runs/x.json",
    ] {
        b.references[0].path = bad.into();
        assert!(
            Behavior::from_text(&b.to_text(), Scope::Project).is_err(),
            "{bad}"
        );
    }
    assert!(store.load_run("../../etc/passwd").is_err());
    assert!(store.load_run("sha256:../../x").is_err());
    assert!(file_name("sha256:ABCDEF").is_none());
}

#[cfg(unix)]
#[test]
fn a_symlinked_runs_directory_is_never_followed() {
    let (root, store) = project_store("symlink");
    let outside = temp("symlink-outside");
    let d = doc(
        Scope::Project,
        "r1",
        FULL,
        OutcomeClass::VerifiedSuccess,
        0,
        "proj_a",
    );
    std::fs::write(outside.join(file_name(&d.id).unwrap()), d.to_text()).unwrap();
    std::fs::create_dir_all(root.join(".stack-agent/learning")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(".stack-agent/learning/runs")).unwrap();
    assert!(store.load_run(&d.id).is_err());
    assert!(
        store.write_run(&d).is_err(),
        "nothing is written through a link"
    );
}

// ---- status is recomputed, never trusted ----------------------------------------------------------------

fn setup(store: &Store, docs: &[&RunDocument]) {
    for d in docs {
        store.write_run(d).unwrap();
    }
}

#[test]
fn project_promotion_rules() {
    let (_, s) = project_store("promote");
    let a = doc(
        Scope::Project,
        "r1",
        FULL,
        OutcomeClass::VerifiedSuccess,
        1,
        "proj_a",
    );
    let b = doc(
        Scope::Project,
        "r2",
        FULL,
        OutcomeClass::VerifiedSuccess,
        2,
        "proj_a",
    );
    let f = doc(
        Scope::Project,
        "r3",
        FULL,
        OutcomeClass::VerifiedFailure,
        1,
        "proj_a",
    );
    let u = doc(
        Scope::Project,
        "r4",
        FULL,
        OutcomeClass::Unverified,
        1,
        "proj_a",
    );
    let old = doc(
        Scope::Project,
        "r5",
        FULL,
        OutcomeClass::VerifiedSuccess,
        400,
        "proj_a",
    );
    let old2 = doc(
        Scope::Project,
        "r6",
        FULL,
        OutcomeClass::VerifiedSuccess,
        300,
        "proj_a",
    );
    setup(&s, &[&a, &b, &f, &u, &old, &old2]);
    let st = |docs: &[&RunDocument], recorded: Status| {
        assess(
            &s,
            behavior(Scope::Project, FULL, docs, recorded),
            NOW,
            CATALOG,
            &line(),
        )
        .status
    };
    assert_eq!(
        st(&[&a], Status::Validated),
        Status::Candidate,
        "one success is a candidate, whatever the file says"
    );
    assert_eq!(st(&[&a, &b], Status::Candidate), Status::Validated);
    assert_eq!(
        st(&[&a, &b, &f], Status::Validated),
        Status::Contested,
        "conflict is not overwritten"
    );
    assert_eq!(
        st(
            &[
                &f,
                &u,
                &doc(
                    Scope::Project,
                    "r7",
                    FULL,
                    OutcomeClass::VerifiedFailure,
                    1,
                    "proj_a"
                )
            ],
            Status::Candidate
        ),
        Status::Candidate,
        "r7 is missing, so 2 contradictions"
    );
    assert_eq!(st(&[&old, &old2], Status::Validated), Status::Stale);
    // A missing reference keeps a behavior from being validated.
    let missing = doc(
        Scope::Project,
        "r8",
        FULL,
        OutcomeClass::VerifiedSuccess,
        1,
        "proj_a",
    );
    let a2 = assess(
        &s,
        behavior(Scope::Project, FULL, &[&a, &b, &missing], Status::Validated),
        NOW,
        CATALOG,
        &line(),
    );
    assert_eq!(a2.status, Status::Candidate);
    assert_eq!(a2.problems.len(), 1);
    // The same document referenced twice counts once.
    let dup = assess(
        &s,
        behavior(Scope::Project, FULL, &[&a, &a], Status::Validated),
        NOW,
        CATALOG,
        &line(),
    );
    assert_eq!(dup.support, 1);
    assert_eq!(dup.status, Status::Candidate);
    // Another catalog version makes it stale.
    let mut other = behavior(Scope::Project, FULL, &[&a, &b], Status::Validated);
    other.catalog_version = "catalog.v0".into();
    assert_eq!(
        assess(&s, other, NOW, CATALOG, &line()).status,
        Status::Stale
    );
}

#[test]
fn rejection_needs_three_contradictions_and_no_support() {
    let (_, s) = project_store("reject");
    let fs: Vec<RunDocument> = (0..3)
        .map(|i| {
            doc(
                Scope::Project,
                &format!("f{i}"),
                FULL,
                OutcomeClass::VerifiedFailure,
                1,
                "proj_a",
            )
        })
        .collect();
    setup(&s, &fs.iter().collect::<Vec<_>>());
    let a = assess(
        &s,
        behavior(
            Scope::Project,
            FULL,
            &fs.iter().collect::<Vec<_>>(),
            Status::Validated,
        ),
        NOW,
        CATALOG,
        &line(),
    );
    assert_eq!(a.status, Status::Rejected);
    // Its failures still count as evidence for --auto ranking; candidates' do not.
    assert_eq!(evidence(Scope::Project, &[a]).len(), 3);
}

#[test]
fn runtime_promotion_needs_distinct_projects() {
    let s = runtime_store("runtime-promote");
    let a1 = doc(
        Scope::Runtime,
        "r1",
        FULL,
        OutcomeClass::VerifiedSuccess,
        1,
        "aaaa",
    );
    let a2 = doc(
        Scope::Runtime,
        "r2",
        FULL,
        OutcomeClass::VerifiedSuccess,
        1,
        "aaaa",
    );
    let b1 = doc(
        Scope::Runtime,
        "r3",
        FULL,
        OutcomeClass::VerifiedSuccess,
        1,
        "bbbb",
    );
    setup(&s, &[&a1, &a2, &b1]);
    let one = assess(
        &s,
        behavior(Scope::Runtime, FULL, &[&a1, &a2], Status::Validated),
        NOW,
        CATALOG,
        &line(),
    );
    assert_eq!(
        one.status,
        Status::Candidate,
        "two successes in one project do not generalize"
    );
    assert!(
        evidence(Scope::Runtime, &[one]).is_empty(),
        "a runtime candidate informs nothing"
    );
    let two = assess(
        &s,
        behavior(Scope::Runtime, FULL, &[&a1, &b1], Status::Candidate),
        NOW,
        CATALOG,
        &line(),
    );
    assert_eq!(two.status, Status::Validated);
    assert_eq!(two.distinct_projects, 2);
    assert_eq!(evidence(Scope::Runtime, &[two]).len(), 2);
    // Another runtime line: stale until revalidated.
    let mut other = behavior(Scope::Runtime, FULL, &[&a1, &b1], Status::Validated);
    other.runtime_line = "0.0".into();
    assert_eq!(
        assess(&s, other, NOW, CATALOG, &line()).status,
        Status::Stale
    );
}

#[test]
fn baseline_runs_and_wrong_scope_documents_inform_nothing() {
    let (_, s) = project_store("baseline");
    let a = doc(
        Scope::Project,
        "r1",
        "baseline",
        OutcomeClass::VerifiedSuccess,
        1,
        "proj_a",
    );
    let b = doc(
        Scope::Project,
        "r2",
        "baseline",
        OutcomeClass::VerifiedSuccess,
        1,
        "proj_a",
    );
    setup(&s, &[&a, &b]);
    let v = assess(
        &s,
        behavior(Scope::Project, "baseline", &[&a, &b], Status::Candidate),
        NOW,
        CATALOG,
        &line(),
    );
    assert_eq!(v.status, Status::Validated);
    assert!(
        evidence(Scope::Project, &[v]).is_empty(),
        "the baseline is not a configuration --auto can rank"
    );
    // A runtime document placed in a project store is refused.
    let r = runtime_store("wrong-scope");
    let rd = doc(
        Scope::Runtime,
        "r9",
        FULL,
        OutcomeClass::VerifiedSuccess,
        1,
        "aaaa",
    );
    r.write_run(&rd).unwrap();
    assert!(s.write_run(&rd).is_err());
}

// ---- stores and identity -------------------------------------------------------------------------------

#[test]
fn stores_are_separate_and_identity_is_stable() {
    let (root, project) = project_store("identity");
    let runtime = runtime_store("identity-runtime");
    assert!(project.root.starts_with(&root));
    assert!(
        !runtime.root.starts_with(&root),
        "runtime data never lives in the project"
    );
    assert!(read_project_identity(&root).unwrap().is_none());
    let id = ensure_project_identity(&root, NOW).unwrap();
    assert_eq!(
        ensure_project_identity(&root, NOW + 1).unwrap(),
        id,
        "created once"
    );
    assert!(id.id.starts_with("proj_"));
    // Moving the directory keeps the identity.
    let moved = temp("identity-moved");
    std::fs::remove_dir_all(&moved).unwrap();
    std::fs::rename(&root, &moved).unwrap();
    assert_eq!(read_project_identity(&moved).unwrap().unwrap(), id);
    // An invalid identity file is reported and never overwritten.
    std::fs::write(
        moved.join(".stack-agent/project.json"),
        "{\"schema\":\"x\"}",
    )
    .unwrap();
    assert!(read_project_identity(&moved).is_err());
    assert!(ensure_project_identity(&moved, NOW).is_err());
}

#[test]
fn an_unavailable_runtime_location_is_explicit() {
    assert!(
        Store::runtime(&|_| None)
            .unwrap_err()
            .contains("no data directory")
    );
    assert!(
        Store::runtime(&|n| (n == "STACK_AGENT_DATA_DIR").then(|| "relative/dir".into()))
            .unwrap_err()
            .contains("absolute")
    );
    let file = temp("runtime-file").join("not-a-dir");
    std::fs::write(&file, "x").unwrap();
    let store = Store::runtime(&|n| {
        (n == "STACK_AGENT_DATA_DIR").then(|| file.to_string_lossy().into_owned())
    })
    .unwrap()
    .0;
    let d = doc(
        Scope::Runtime,
        "r1",
        FULL,
        OutcomeClass::VerifiedSuccess,
        0,
        "a",
    );
    assert!(
        store.write_run(&d).is_err(),
        "a write that cannot happen says so"
    );
}
