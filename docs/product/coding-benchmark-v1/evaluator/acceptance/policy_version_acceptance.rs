//! Benchmark acceptance test (evaluator-held; not given to the agent).
//!
//! `Policy::intersect`: "a contract satisfies the result exactly when it
//! satisfies both. The result is never less restrictive than either input."
//! `allowed_runtime_versions` (docs/policy.md): the version the runtime
//! reports must contain one of the listed prefixes. Only cases whose answer
//! follows unambiguously from those two statements are asserted.

use std::collections::BTreeMap;

use compute_core::{
    IsolationRequirement, NetworkPolicy, PlatformIdentity, ProviderIdentity, ResourceLimits,
    RuntimeKind, WORKLOAD_SPEC_VERSION, WorkloadBundle, WorkloadSpec,
};
use compute_policy::*;

fn contract() -> ExecutionContract {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("main.py"), "print('hello')\n").unwrap();
    let workload = WorkloadSpec {
        version: WORKLOAD_SPEC_VERSION.into(),
        runtime: RuntimeKind::Python,
        runtime_version: None,
        architecture: None,
        entrypoint: "main.py".into(),
        args: vec![],
        env: BTreeMap::new(),
        inputs: vec![],
        outputs: vec![],
        resources: ResourceLimits::default(),
        network: NetworkPolicy::None,
        isolation: IsolationRequirement::default(),
        dependencies: None,
    };
    let bundle = WorkloadBundle::create_from(workload, root.path()).unwrap();
    ExecutionContract::from_bundle(&bundle, None).unwrap()
}

fn provider(runtime_version: &str) -> ProviderFacts {
    ProviderFacts {
        identity: ProviderIdentity::Local { id: "local".into() },
        distribution_id: None,
        platform: Some(PlatformIdentity {
            os: "linux".into(),
            architecture: "x86_64".into(),
            runtime_abi: None,
        }),
        runtime_version: Some(runtime_version.into()),
    }
}

fn policy(json: serde_json::Value) -> Policy {
    Policy::from_json(json.to_string().as_bytes()).unwrap()
}

fn admitted(policy: &Policy, runtime_version: &str) -> bool {
    admit(
        policy,
        &contract(),
        &provider(runtime_version),
        &CapabilityStatus::Compatible,
    )
    .admitted
}

fn compose(sources: &[serde_json::Value]) -> Policy {
    let kinds = [
        PolicySourceKind::Server,
        PolicySourceKind::Provider,
        PolicySourceKind::Explicit,
    ];
    let sources = sources
        .iter()
        .zip(kinds)
        .map(|(source, kind)| (kind, policy(source.clone())))
        .collect::<Vec<_>>();
    EffectivePolicy::compose(&sources).policy
}

fn versions(python: &[&str]) -> serde_json::Value {
    serde_json::json!({"version": 1, "allowed_runtime_versions": {"python": python}})
}

/// Admitted by the composition exactly when admitted by every source.
fn assert_exact(sources: &[serde_json::Value], probes: &[&str]) {
    let composed = compose(sources);
    for probe in probes {
        let each = sources.iter().all(|source| admitted(&policy(source.clone()), probe));
        assert_eq!(
            admitted(&composed, probe),
            each,
            "{probe} under {sources:?}: every source says {each}"
        );
    }
    // Order never matters.
    let mut reversed = sources.to_vec();
    reversed.reverse();
    let other = compose(&reversed);
    for probe in probes {
        assert_eq!(admitted(&composed, probe), admitted(&other, probe), "{probe}");
    }
}

const PROBES: &[&str] = &["3.13.1", "3.13.0", "3.12.4", "3.11.9", "2.7.18", "3.14.0"];

#[test]
fn a_broad_server_restriction_and_a_narrow_caller_pin_compose() {
    assert_exact(&[versions(&["3"]), versions(&["3.13"])], PROBES);
    assert!(admitted(&compose(&[versions(&["3"]), versions(&["3.13"])]), "3.13.1"));
}

#[test]
fn a_narrow_server_restriction_and_a_broad_caller_pin_compose() {
    assert_exact(&[versions(&["3.12"]), versions(&["3"])], PROBES);
    assert!(admitted(&compose(&[versions(&["3.12"]), versions(&["3"])]), "3.12.4"));
}

#[test]
fn several_allowed_versions_on_each_side_compose() {
    assert_exact(&[versions(&["3.12", "3.13"]), versions(&["3"])], PROBES);
    assert_exact(&[versions(&["3.11", "3.13"]), versions(&["3.13", "2"])], PROBES);
    assert_exact(
        &[versions(&["3"]), versions(&["3.12", "3.13"]), versions(&["3.13"])],
        PROBES,
    );
}

#[test]
fn identical_and_disjoint_restrictions_keep_their_meaning() {
    assert_exact(&[versions(&["3.13"]), versions(&["3.13"])], PROBES);
    // Nothing both allow: nothing is admitted.
    let disjoint = compose(&[versions(&["3.12"]), versions(&["3.13"])]);
    for probe in PROBES {
        assert!(!admitted(&disjoint, probe), "{probe}");
    }
    // One source alone still decides.
    assert_exact(&[versions(&["3.13"]), serde_json::json!({"version": 1})], PROBES);
}

#[test]
fn other_runtimes_are_untouched_by_the_composition() {
    let composed = compose(&[
        serde_json::json!({"version": 1, "allowed_runtime_versions": {"python": ["3"], "node": ["22"]}}),
        versions(&["3.13"]),
    ]);
    let versions = composed.allowed_runtime_versions.as_ref().unwrap();
    assert_eq!(
        versions.get(&compute_core::RuntimeKind::Node),
        Some(&vec!["22".to_owned()])
    );
}
