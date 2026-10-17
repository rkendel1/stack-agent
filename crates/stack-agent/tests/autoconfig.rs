//! Automatic configuration: profiling, resolution under policy and hard constraints, and
//! evidence-informed scoring. Deterministic; no model, no network, nothing executed.

use std::path::{Path, PathBuf};

use chip_cli::autoconfig::catalog::{
    self, CapabilityRule, Configuration, ContextChoice, Isolation, Stage,
};
use chip_cli::autoconfig::evidence::{
    Features, Origin, OutcomeClass, OutcomeRecord, RECORD_SCHEMA, Snapshot,
};
use chip_cli::autoconfig::profile::{
    Attr, Basis, BuildSystem, ProfileInputs, ProjectProfile, Verification,
};
use chip_cli::autoconfig::resolve::{Decision, Outcome, Policy, decide};
use chip_cli::software_work::GoalKind;

const DAY: u64 = 20_000;

fn dir(tag: &str, files: &[&str]) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "stack-agent-autoconfig-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for f in files {
        let p = d.join(f);
        if f.ends_with('/') {
            std::fs::create_dir_all(&p).unwrap();
        } else {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "x").unwrap();
        }
    }
    d
}

fn inputs(root: &Path, pax: bool, git: bool) -> ProfileInputs {
    ProfileInputs {
        root: root.to_path_buf(),
        pax: if pax {
            Ok(PathBuf::from("/opt/pax"))
        } else {
            Err("not on PATH".into())
        },
        git_on_path: git,
        model: Ok(("openai-compatible".into(), "scripted".into())),
    }
}

fn cargo_project(tag: &str) -> ProjectProfile {
    ProjectProfile::collect(&inputs(
        &dir(tag, &["Cargo.toml", "src/lib.rs", "tests/", ".git/"]),
        true,
        true,
    ))
}

fn key(d: &Decision) -> Option<&str> {
    match &d.outcome {
        Outcome::Selected { configuration } => Some(configuration),
        _ => None,
    }
}

fn assessment<'a>(d: &'a Decision, k: &str) -> &'a chip_cli::autoconfig::resolve::Assessment {
    d.assessments.iter().find(|a| a.configuration == k).unwrap()
}

fn record(
    config: &str,
    kind: &str,
    build: Option<&str>,
    outcome: OutcomeClass,
    age_days: u64,
    origin: Origin,
    n: usize,
) -> OutcomeRecord {
    OutcomeRecord {
        schema: RECORD_SCHEMA.into(),
        record: "outcome".into(),
        id: String::new(),
        selection_id: None,
        configuration: Some(config.into()),
        origin,
        features: Features {
            task_kind: kind.into(),
            build_system: build.map(String::from),
        },
        recorded_at: (DAY - age_days) * 86_400 + n as u64,
        outcome,
        terminal_state: "completed".into(),
        verified: outcome == OutcomeClass::VerifiedSuccess,
        delivery: None,
        cancellation: None,
        capabilities_invoked: Default::default(),
        verification_refs: vec![],
        duration_ms: None,
        tokens: None,
        cost: None,
        limitations: vec![],
    }
    .seal()
}

fn many(config: &str, outcome: OutcomeClass, n: usize) -> Vec<OutcomeRecord> {
    (0..n)
        .map(|i| {
            record(
                config,
                "change",
                Some("cargo"),
                outcome,
                1,
                Origin::Production,
                i,
            )
        })
        .collect()
}

fn run(profile: &ProjectProfile, kind: GoalKind, records: Vec<OutcomeRecord>) -> Decision {
    decide(
        profile,
        kind,
        &catalog::catalog(),
        &Policy::work(),
        &Snapshot::new(records),
        DAY,
    )
}

const DEDUP: &str = "change.cargo-verified@1";
const FULL: &str = "change.cargo-verified-full-context@1";
const EXPERIMENTAL: &str = "change.cargo-verified-full-context-isolated@1";

// ---- profiling ------------------------------------------------------------------------------------

#[test]
fn a_cargo_project_is_profiled_with_its_evidence() {
    let p = cargo_project("cargo");
    match &p.primary_build_system {
        Attr::Known {
            value,
            basis,
            evidence,
        } => {
            assert_eq!(*value, BuildSystem::Cargo);
            assert!(
                matches!(basis, Basis::Inferred { .. }),
                "a classification is inferred, not observed"
            );
            assert_eq!(evidence, &["file:Cargo.toml"]);
        }
        other => panic!("{other:?}"),
    }
    assert!(p.verification.is(&Verification::PaxCargo));
    assert!(p.has_tests.is(&true));
    assert!(p.git_repository.is(&true));
    assert!(matches!(p.network, Attr::Unknown { .. }), "never probed");
    for a in [&p.git_repository, &p.git_tool, &p.pax_tool] {
        assert!(matches!(
            a,
            Attr::Known {
                basis: Basis::Observed,
                ..
            }
        ));
    }
    // Every cited source is an observation.
    let sources: Vec<&str> = p.observations.iter().map(|o| o.source.as_str()).collect();
    if let Attr::Known { evidence, .. } = &p.verification {
        for e in evidence {
            assert!(sources.contains(&e.as_str()), "{e} is not an observation");
        }
    }
}

#[test]
fn other_build_systems_unknown_and_conflicting_signals_are_explicit() {
    let npm = ProjectProfile::collect(&inputs(
        &dir("npm", &["package.json", "package-lock.json"]),
        true,
        true,
    ));
    assert!(npm.primary_build_system.is(&BuildSystem::Npm));
    assert!(npm.verification.is(&Verification::PaxUninterpreted));

    let both = ProjectProfile::collect(&inputs(
        &dir("both", &["Cargo.toml", "package.json"]),
        true,
        true,
    ));
    match &both.primary_build_system {
        Attr::Ambiguous { candidates, .. } => {
            assert_eq!(candidates, &[BuildSystem::Cargo, BuildSystem::Npm])
        }
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(both.verification, Attr::Unknown { .. }),
        "ambiguity is not resolved by guessing"
    );

    let empty = ProjectProfile::collect(&inputs(&dir("empty", &["notes.txt"]), true, false));
    assert!(matches!(empty.primary_build_system, Attr::Unknown { .. }));
    assert!(matches!(empty.languages, Attr::Unknown { .. }));
    assert!(empty.has_tests.is(&false));
    assert!(empty.git_repository.is(&false));

    let no_pax = ProjectProfile::collect(&inputs(&dir("nopax", &["Cargo.toml"]), false, true));
    assert!(no_pax.verification.is(&Verification::None));
}

#[test]
fn profiling_is_deterministic_and_reads_no_content() {
    let root = dir(
        "stable",
        &[
            "Cargo.toml",
            "src/lib.rs",
            "tests/it.rs",
            "b_test.py",
            ".git/",
        ],
    );
    std::fs::write(root.join("src/lib.rs"), "sk-SENTINEL-content").unwrap();
    let a = ProjectProfile::collect(&inputs(&root, true, true));
    let b = ProjectProfile::collect(&inputs(&root, true, true));
    assert_eq!(a, b);
    assert_eq!(a.digest(), b.digest());
    let mut sorted = a.observations.clone();
    sorted.sort();
    assert_eq!(a.observations, sorted);
    assert!(
        !a.to_json().to_string().contains("sk-SENTINEL"),
        "no file content is recorded"
    );
}

// ---- resolution and policy --------------------------------------------------------------------------

#[test]
fn the_built_in_catalog_is_valid() {
    catalog::validate(&catalog::catalog()).unwrap();
}

#[test]
fn a_cargo_change_selects_the_production_default_without_evidence() {
    let d = run(&cargo_project("default"), GoalKind::Change, vec![]);
    assert_eq!(key(&d), Some(DEDUP));
    assert_eq!(d.confidence, "evidence_limited");
    assert_eq!(assessment(&d, FULL).rank, Some(2));
    let exp = assessment(&d, EXPERIMENTAL);
    assert!(!exp.eligible);
    assert!(
        exp.exclusions.iter().any(|e| e.contains("experimental")),
        "{:?}",
        exp.exclusions
    );
    assert!(
        exp.exclusions.iter().any(|e| e.contains("isolated")),
        "{:?}",
        exp.exclusions
    );
    assert!(d.offered().unwrap().contains(&"pax.test".to_string()));
    assert!(
        d.offered()
            .unwrap()
            .contains(&"project.git.status".to_string())
    );
}

#[test]
fn a_required_capability_unavailable_blocks_and_an_optional_one_does_not() {
    let no_pax =
        ProjectProfile::collect(&inputs(&dir("rq", &["Cargo.toml", ".git/"]), false, true));
    let d = run(&no_pax, GoalKind::Change, vec![]);
    match &d.outcome {
        Outcome::Blocked { reasons } => assert!(
            reasons
                .iter()
                .any(|r| r.contains("requires pax.test") && r.contains("PAX is not present")),
            "{reasons:?}"
        ),
        other => panic!("{other:?}"),
    }
    let no_git = ProjectProfile::collect(&inputs(&dir("opt", &["Cargo.toml"]), true, false));
    let d = run(&no_git, GoalKind::Change, vec![]);
    assert_eq!(key(&d), Some(DEDUP));
    let a = assessment(&d, DEDUP);
    assert!(
        a.preferred_unavailable
            .iter()
            .any(|p| p.starts_with("project.git.status")),
        "{:?}",
        a.preferred_unavailable
    );
    assert!(!a.offered.iter().any(|o| o.starts_with("project.git.")));
}

#[test]
fn prohibited_capabilities_are_withheld_even_when_available() {
    let d = run(&cargo_project("inspect"), GoalKind::Inspect, vec![]);
    assert_eq!(key(&d), Some("inspect.read-only@1"));
    let a = assessment(&d, "inspect.read-only@1");
    assert_eq!(a.withheld, ["pax.test", "project.write"]);
    assert!(!a.offered.contains(&"project.write".to_string()));
    let d = run(&cargo_project("verify"), GoalKind::Verify, vec![]);
    assert_eq!(assessment(&d, "verify.cargo@1").withheld, ["project.write"]);
}

#[test]
fn policy_and_isolation_are_hard_constraints() {
    let p = cargo_project("policy");
    let mut policy = Policy::work();
    policy.prohibited = vec!["project.write".into()];
    let d = decide(
        &p,
        GoalKind::Change,
        &catalog::catalog(),
        &policy,
        &Snapshot::default(),
        DAY,
    );
    assert!(
        matches!(d.outcome, Outcome::Blocked { .. }),
        "a task that prohibits writing cannot be a change"
    );
    // Only an environment that isolates, and a command that allows experiments, admits the
    // experimental configuration.
    let lab = Policy {
        stages: vec![Stage::Production, Stage::Experimental],
        isolation: Isolation::Isolated,
        prohibited: vec![],
    };
    let d = decide(
        &p,
        GoalKind::Change,
        &catalog::catalog(),
        &lab,
        &Snapshot::default(),
        DAY,
    );
    assert!(assessment(&d, EXPERIMENTAL).eligible);
}

#[test]
fn incompatible_or_invalid_definitions_are_refused() {
    let base = catalog::catalog();
    let mut dup = base.clone();
    dup.push(base[0].clone());
    assert!(
        catalog::validate(&dup)
            .unwrap_err()
            .0
            .contains("defined twice")
    );
    let mut bad: Vec<Configuration> = base.clone();
    bad[0]
        .capabilities
        .push(("pax.nonsense", CapabilityRule::Preferred));
    assert!(
        catalog::validate(&bad)
            .unwrap_err()
            .0
            .contains("unknown capability")
    );
    let mut bad = base.clone();
    bad[0].context = ContextChoice::InspectProgress;
    assert!(
        catalog::validate(&bad)
            .unwrap_err()
            .0
            .contains("only compatible with inspect")
    );
    let mut bad = base.clone();
    let inspect = bad
        .iter_mut()
        .find(|c| c.id == "inspect.read-only")
        .unwrap();
    inspect.capabilities.retain(|(c, _)| *c != "project.write");
    assert!(
        catalog::validate(&bad)
            .unwrap_err()
            .0
            .contains("must prohibit")
    );
    let mut bad = base.clone();
    bad[0].capabilities.retain(|(c, _)| *c != "pax.test");
    assert!(
        catalog::validate(&bad)
            .unwrap_err()
            .0
            .contains("must require pax.test")
    );
    // An invalid catalog never selects anything: the existing behavior runs, with the reason.
    let d = decide(
        &cargo_project("invalid"),
        GoalKind::Change,
        &dup,
        &Policy::work(),
        &Snapshot::default(),
        DAY,
    );
    assert!(matches!(&d.outcome, Outcome::Fallback { reason } if reason.contains("invalid")));
    assert!(catalog::find(&base, "change.unknown@1").is_none());
}

#[test]
fn no_eligible_configuration_blocks_and_undecidable_facts_fall_back() {
    let npm = ProjectProfile::collect(&inputs(&dir("npm-change", &["package.json"]), true, true));
    let d = run(&npm, GoalKind::Change, vec![]);
    match &d.outcome {
        Outcome::Blocked { reasons } => {
            assert!(
                reasons
                    .iter()
                    .any(|r| r.contains("applies to cargo projects; this one is npm")),
                "{reasons:?}"
            );
            assert!(
                reasons
                    .iter()
                    .any(|r| r.contains("no authoritative test semantics")),
                "{reasons:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    // Inspect applies to any project.
    assert_eq!(
        key(&run(&npm, GoalKind::Inspect, vec![])),
        Some("inspect.read-only@1")
    );
    let both = ProjectProfile::collect(&inputs(
        &dir("both-change", &["Cargo.toml", "package.json"]),
        true,
        true,
    ));
    let d = run(&both, GoalKind::Change, vec![]);
    assert!(
        matches!(&d.outcome, Outcome::Fallback { reason } if reason.contains("ambiguous")),
        "{:?}",
        d.outcome
    );
}

#[test]
fn ties_are_broken_by_default_rank_then_key() {
    let p = cargo_project("ties");
    let mut c = catalog::catalog();
    c[1].default_rank = 0; // the same rank as change.cargo-verified
    let d = decide(
        &p,
        GoalKind::Change,
        &c,
        &Policy::work(),
        &Snapshot::default(),
        DAY,
    );
    // Equal scores (neutral) and equal ranks: the key decides, ascending by byte
    // ("change.cargo-verified-full-context@1" < "change.cargo-verified@1", since '-' < '@').
    assert_eq!(key(&d), Some(FULL));
    assert_eq!(assessment(&d, DEDUP).rank, Some(2));
    // And with the catalog's own ranks, rank decides before the key.
    let d = run(&p, GoalKind::Change, vec![]);
    assert_eq!(key(&d), Some(DEDUP));
}

// ---- scoring and learning -------------------------------------------------------------------------------

#[test]
fn verified_success_outranks_verified_failure() {
    let mut records = many(DEDUP, OutcomeClass::VerifiedFailure, 4);
    records.extend(many(FULL, OutcomeClass::VerifiedSuccess, 4));
    let d = run(&cargo_project("learn"), GoalKind::Change, records);
    assert_eq!(key(&d), Some(FULL));
    assert_eq!(d.confidence, "high");
    assert_eq!(d.evidence_used.len(), 8);
    let full = assessment(&d, FULL).evidence.as_ref().unwrap();
    assert!(full.score.unwrap() > 0.7);
}

#[test]
fn missing_verification_counts_against_and_non_informative_outcomes_do_not_count() {
    let d = run(
        &cargo_project("unverified"),
        GoalKind::Change,
        many(DEDUP, OutcomeClass::Unverified, 4),
    );
    assert_eq!(
        key(&d),
        Some(FULL),
        "unverified completions are not success"
    );
    assert_eq!(d.confidence, "evidence_limited");

    let mut records = many(DEDUP, OutcomeClass::Cancelled, 5);
    records.extend(many(DEDUP, OutcomeClass::Blocked, 5));
    records.extend(many(DEDUP, OutcomeClass::RuntimeFailure, 5));
    let d = run(&cargo_project("noninformative"), GoalKind::Change, records);
    assert_eq!(key(&d), Some(DEDUP));
    let e = assessment(&d, DEDUP).evidence.as_ref().unwrap();
    assert_eq!(e.weight, 0.0);
    assert_eq!(e.ignored.values().sum::<usize>(), 15);
    assert!(d.evidence_used.is_empty());
}

#[test]
fn small_stale_and_irrelevant_evidence_does_not_decide() {
    let p = cargo_project("weak");
    // Two successes: below the minimum, so neutral; the default decides.
    let d = run(
        &p,
        GoalKind::Change,
        many(FULL, OutcomeClass::VerifiedSuccess, 2),
    );
    assert_eq!(key(&d), Some(DEDUP));
    // Stale successes are ignored.
    let stale: Vec<_> = (0..6)
        .map(|i| {
            record(
                FULL,
                "change",
                Some("cargo"),
                OutcomeClass::VerifiedSuccess,
                400,
                Origin::Production,
                i,
            )
        })
        .collect();
    let d = run(&p, GoalKind::Change, stale);
    assert_eq!(key(&d), Some(DEDUP));
    assert!(
        assessment(&d, FULL)
            .evidence
            .as_ref()
            .unwrap()
            .ignored
            .keys()
            .any(|k| k.starts_with("stale"))
    );
    // Another build system or another task kind is not comparable.
    let elsewhere: Vec<_> = (0..6)
        .map(|i| {
            record(
                FULL,
                "change",
                Some("npm"),
                OutcomeClass::VerifiedSuccess,
                1,
                Origin::Production,
                i,
            )
        })
        .chain((0..6).map(|i| {
            record(
                FULL,
                "inspect",
                Some("cargo"),
                OutcomeClass::VerifiedSuccess,
                1,
                Origin::Production,
                10 + i,
            )
        }))
        .collect();
    assert_eq!(key(&run(&p, GoalKind::Change, elsewhere)), Some(DEDUP));
    // Older evidence weighs less: 4 successes 180 days old weigh 1.0 in all.
    let aged: Vec<_> = (0..4)
        .map(|i| {
            record(
                FULL,
                "change",
                Some("cargo"),
                OutcomeClass::VerifiedSuccess,
                180,
                Origin::Production,
                i,
            )
        })
        .collect();
    let d = run(&p, GoalKind::Change, aged);
    let e = assessment(&d, FULL).evidence.as_ref().unwrap();
    assert!((e.weight - 1.0).abs() < 1e-9, "{}", e.weight);
    assert_eq!(key(&d), Some(DEDUP), "too little weighted evidence");
}

#[test]
fn experiment_evidence_never_changes_the_production_selection() {
    let records: Vec<_> = (0..10)
        .map(|i| {
            record(
                FULL,
                "change",
                Some("cargo"),
                OutcomeClass::VerifiedSuccess,
                1,
                Origin::Experiment,
                i,
            )
        })
        .collect();
    let d = run(&cargo_project("experiment"), GoalKind::Change, records);
    assert_eq!(key(&d), Some(DEDUP));
    assert!(
        assessment(&d, FULL)
            .evidence
            .as_ref()
            .unwrap()
            .ignored
            .keys()
            .any(|k| k.starts_with("experiment"))
    );
}

#[test]
fn a_high_score_cannot_override_a_hard_constraint() {
    let mut records: Vec<_> = (0..10)
        .map(|i| {
            record(
                EXPERIMENTAL,
                "change",
                Some("cargo"),
                OutcomeClass::VerifiedSuccess,
                1,
                Origin::Production,
                i,
            )
        })
        .collect();
    records.extend(many(DEDUP, OutcomeClass::VerifiedFailure, 6));
    let d = run(
        &cargo_project("override"),
        GoalKind::Change,
        records.clone(),
    );
    assert_ne!(key(&d), Some(EXPERIMENTAL));
    assert!(
        assessment(&d, EXPERIMENTAL).evidence.is_none(),
        "an ineligible configuration is not even scored"
    );
    let npm = ProjectProfile::collect(&inputs(&dir("npm-override", &["package.json"]), true, true));
    let npm_records: Vec<_> = (0..10)
        .map(|i| {
            record(
                DEDUP,
                "change",
                Some("npm"),
                OutcomeClass::VerifiedSuccess,
                1,
                Origin::Production,
                i,
            )
        })
        .collect();
    assert!(matches!(
        run(&npm, GoalKind::Change, npm_records).outcome,
        Outcome::Blocked { .. }
    ));
}

#[test]
fn identical_inputs_and_snapshots_give_identical_decisions_in_any_order() {
    let p = cargo_project("repeat");
    let mut records = many(DEDUP, OutcomeClass::VerifiedFailure, 3);
    records.extend(many(FULL, OutcomeClass::VerifiedSuccess, 5));
    let a = run(&p, GoalKind::Change, records.clone()).to_json();
    let b = run(&p, GoalKind::Change, records.clone()).to_json();
    records.reverse();
    let c = run(&p, GoalKind::Change, records).to_json();
    assert_eq!(a, b);
    assert_eq!(a, c, "the snapshot's order does not matter");
    assert_eq!(a["id"], c["id"]);
}

// ---- evidence snapshots -------------------------------------------------------------------------------

#[test]
fn a_snapshot_is_exact_or_refused() {
    let r = record(
        DEDUP,
        "change",
        Some("cargo"),
        OutcomeClass::VerifiedSuccess,
        1,
        Origin::Production,
        0,
    );
    let text = serde_json::to_string(&vec![r.clone()]).unwrap();
    assert_eq!(Snapshot::from_json(&text).unwrap().records_len(), 1);
    let jsonl = format!(
        "{}\n{}\n",
        serde_json::to_string(&r).unwrap(),
        serde_json::to_string(&r).unwrap()
    );
    assert_eq!(Snapshot::from_json(&jsonl).unwrap().records_len(), 2);
    let mut tampered = serde_json::to_value(&r).unwrap();
    tampered["outcome"] = "verified_success".into();
    tampered["verified"] = false.into();
    assert!(
        Snapshot::from_json(&tampered.to_string())
            .unwrap_err()
            .contains("does not match")
    );
    let mut selection = serde_json::to_value(&r).unwrap();
    selection["record"] = "selection".into();
    assert!(
        Snapshot::from_json(&selection.to_string())
            .unwrap_err()
            .contains("only outcome records")
    );
    assert!(Snapshot::from_json("{\"schema\":\"other\"}").is_err());
}
