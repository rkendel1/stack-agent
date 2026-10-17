//! The built-in configuration catalog: versioned, compiled-in combinations of existing parts. A
//! configuration chooses among things the binary already has: the goal kind's production decision
//! policy, one of Chip's escalation-context policies, and which of the environment's existing
//! capabilities to offer. It never adds a capability, an executor or a verifier.
//!
//! These are implementation details: users never name, select, install or manage them.

use std::collections::BTreeSet;

use chip_project::{GIT_CAPABILITIES, PROJECT_LIST, PROJECT_READ, PROJECT_SEARCH, PROJECT_WRITE};
use serde::Serialize;

use super::profile::BuildSystem;
use crate::software_work::GoalKind;

pub const CATALOG_VERSION: &str = "catalog.v1";

/// The capabilities the local environment can declare. A configuration may name no other.
pub fn known_capabilities() -> Vec<&'static str> {
    let mut all = vec![
        PROJECT_LIST,
        PROJECT_SEARCH,
        PROJECT_READ,
        PROJECT_WRITE,
        chip_pax::PAX_TEST_CAPABILITY,
        chip_pax::PROJECT_OBSERVE_CAPABILITY,
    ];
    all.extend(GIT_CAPABILITIES.iter().copied());
    all
}

/// What a configuration says about one capability. "Unavailable" is not here: that is a fact about
/// the environment, decided at selection time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityRule {
    /// The configuration is ineligible without it.
    Required,
    /// Offered when the environment can provide it.
    Preferred,
    /// Never offered, even when available.
    Prohibited,
}

/// Something a configuration needs that is not a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Requirement {
    /// A model is selected (every configuration here asks one when it cannot decide locally).
    Model,
    /// PAX can establish test results for this project (Cargo).
    CargoTestVerification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// May be selected by `work`.
    Production,
    /// May only be run by `stack-agent experiment`; `work` never selects it.
    Experimental,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    /// Runs in the local environment: the user's one mutable project directory.
    SharedProjectDirectory,
    /// Needs an environment that isolates its effects from the user's files.
    Isolated,
}

/// Which of Chip's existing escalation-context policies the work uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextChoice {
    /// `DeduplicatedEscalationContext`: what `work` uses for change and verify.
    Deduplicated,
    /// `FullEscalationContext`: chip-core's default, everything the loop knows.
    Full,
    /// The inspection-progress context `work` uses for inspect.
    InspectProgress,
}

#[derive(Debug, Clone, Serialize)]
pub struct Configuration {
    pub id: &'static str,
    pub version: u32,
    pub description: &'static str,
    pub stage: Stage,
    pub kinds: Vec<GoalKind>,
    /// `None`: any build system.
    pub build_systems: Option<Vec<BuildSystem>>,
    pub capabilities: Vec<(&'static str, CapabilityRule)>,
    pub requirements: Vec<Requirement>,
    pub context: ContextChoice,
    pub max_turns: usize,
    pub max_executions: usize,
    pub isolation: Isolation,
    /// The documented default order among eligible configurations when evidence cannot
    /// differentiate them: lower first.
    pub default_rank: u32,
}

impl Configuration {
    pub fn key(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }

    pub fn rule(&self, capability: &str) -> Option<CapabilityRule> {
        self.capabilities
            .iter()
            .find(|(c, _)| *c == capability)
            .map(|(_, r)| *r)
    }
}

fn git(rule: CapabilityRule) -> Vec<(&'static str, CapabilityRule)> {
    GIT_CAPABILITIES.iter().map(|c| (*c, rule)).collect()
}

fn change(
    id: &'static str,
    description: &'static str,
    context: ContextChoice,
    stage: Stage,
    isolation: Isolation,
    rank: u32,
) -> Configuration {
    let mut capabilities = vec![
        (PROJECT_READ, CapabilityRule::Required),
        (PROJECT_WRITE, CapabilityRule::Required),
        (chip_pax::PAX_TEST_CAPABILITY, CapabilityRule::Required),
        (PROJECT_LIST, CapabilityRule::Preferred),
        (PROJECT_SEARCH, CapabilityRule::Preferred),
        (
            chip_pax::PROJECT_OBSERVE_CAPABILITY,
            CapabilityRule::Preferred,
        ),
    ];
    capabilities.extend(git(CapabilityRule::Preferred));
    Configuration {
        id,
        version: 1,
        description,
        stage,
        kinds: vec![GoalKind::Change],
        build_systems: Some(vec![BuildSystem::Cargo]),
        capabilities,
        requirements: vec![Requirement::Model, Requirement::CargoTestVerification],
        context,
        max_turns: crate::software_work::DEFAULT_MAX_TURNS,
        max_executions: crate::software_work::DEFAULT_MAX_EXECUTIONS,
        isolation,
        default_rank: rank,
    }
}

/// The catalog, in default-rank order.
pub fn catalog() -> Vec<Configuration> {
    let mut verify_caps = vec![
        (PROJECT_READ, CapabilityRule::Required),
        (chip_pax::PAX_TEST_CAPABILITY, CapabilityRule::Required),
        (PROJECT_LIST, CapabilityRule::Preferred),
        (PROJECT_SEARCH, CapabilityRule::Preferred),
        (
            chip_pax::PROJECT_OBSERVE_CAPABILITY,
            CapabilityRule::Preferred,
        ),
        (PROJECT_WRITE, CapabilityRule::Prohibited),
    ];
    verify_caps.extend(git(CapabilityRule::Preferred));
    let mut inspect_caps = vec![
        (PROJECT_READ, CapabilityRule::Required),
        (PROJECT_LIST, CapabilityRule::Preferred),
        (PROJECT_SEARCH, CapabilityRule::Preferred),
        (
            chip_pax::PROJECT_OBSERVE_CAPABILITY,
            CapabilityRule::Preferred,
        ),
        (PROJECT_WRITE, CapabilityRule::Prohibited),
        (chip_pax::PAX_TEST_CAPABILITY, CapabilityRule::Prohibited),
    ];
    inspect_caps.extend(git(CapabilityRule::Preferred));
    vec![
        change(
            "change.cargo-verified",
            "change a Cargo project and verify it with pax.test; the production change policy and context",
            ContextChoice::Deduplicated,
            Stage::Production,
            Isolation::SharedProjectDirectory,
            0,
        ),
        change(
            "change.cargo-verified-full-context",
            "as change.cargo-verified, but every escalation carries the full trajectory (FullEscalationContext)",
            ContextChoice::Full,
            Stage::Production,
            Isolation::SharedProjectDirectory,
            1,
        ),
        Configuration {
            id: "verify.cargo",
            version: 1,
            description: "establish whether a Cargo project's tests pass; writing is never offered",
            stage: Stage::Production,
            kinds: vec![GoalKind::Verify],
            build_systems: Some(vec![BuildSystem::Cargo]),
            capabilities: verify_caps,
            requirements: vec![Requirement::Model, Requirement::CargoTestVerification],
            context: ContextChoice::Deduplicated,
            max_turns: crate::software_work::DEFAULT_MAX_TURNS,
            max_executions: crate::software_work::DEFAULT_MAX_EXECUTIONS,
            isolation: Isolation::SharedProjectDirectory,
            default_rank: 0,
        },
        Configuration {
            id: "inspect.read-only",
            version: 1,
            description: "answer a question about any project from observations; writing and running tests are never offered",
            stage: Stage::Production,
            kinds: vec![GoalKind::Inspect],
            build_systems: None,
            capabilities: inspect_caps,
            requirements: vec![Requirement::Model],
            context: ContextChoice::InspectProgress,
            max_turns: crate::software_work::DEFAULT_MAX_TURNS,
            max_executions: crate::software_work::DEFAULT_MAX_EXECUTIONS,
            isolation: Isolation::SharedProjectDirectory,
            default_rank: 0,
        },
        change(
            "change.cargo-verified-full-context-isolated",
            "an experimental full-context change that must run isolated; only `stack-agent experiment` may run it",
            ContextChoice::Full,
            Stage::Experimental,
            Isolation::Isolated,
            2,
        ),
    ]
}

/// Why a catalog is not accepted. The built-in catalog is checked by a test; a host composing its
/// own (tests do) gets the same refusals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogError(pub String);

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid configuration catalog: {}", self.0)
    }
}

pub fn validate(catalog: &[Configuration]) -> Result<(), CatalogError> {
    let err = |s: String| Err(CatalogError(s));
    let known = known_capabilities();
    let mut keys = BTreeSet::new();
    for c in catalog {
        if c.id.is_empty() || c.version == 0 {
            return err(format!("{:?} has no id or version", c.id));
        }
        if !keys.insert(c.key()) {
            return err(format!("{} is defined twice", c.key()));
        }
        if c.kinds.is_empty() {
            return err(format!("{} applies to no goal kind", c.key()));
        }
        let mut seen = BTreeSet::new();
        for (cap, _) in &c.capabilities {
            if !known.contains(cap) {
                return err(format!("{} names unknown capability {cap}", c.key()));
            }
            if !seen.insert(*cap) {
                return err(format!(
                    "{} states {cap} twice (a capability has exactly one rule)",
                    c.key()
                ));
            }
        }
        if !c
            .capabilities
            .iter()
            .any(|(_, r)| *r == CapabilityRule::Required)
        {
            return err(format!("{} requires no capability", c.key()));
        }
        if !(1..=64).contains(&c.max_turns) || !(1..=64).contains(&c.max_executions) {
            return err(format!("{} has limits outside 1 to 64", c.key()));
        }
        // Compatibility: the inspection-progress context belongs to inspection only, and the
        // read-only kinds never offer writing.
        if c.context == ContextChoice::InspectProgress
            && c.kinds.iter().any(|k| *k != GoalKind::Inspect)
        {
            return err(format!(
                "{}: the inspection-progress context is only compatible with inspect",
                c.key()
            ));
        }
        if c.kinds
            .iter()
            .any(|k| matches!(k, GoalKind::Verify | GoalKind::Inspect))
            && c.rule(PROJECT_WRITE) != Some(CapabilityRule::Prohibited)
        {
            return err(format!(
                "{}: verify and inspect must prohibit {PROJECT_WRITE}",
                c.key()
            ));
        }
        if c.kinds.contains(&GoalKind::Change)
            && c.rule(chip_pax::PAX_TEST_CAPABILITY) != Some(CapabilityRule::Required)
        {
            return err(format!(
                "{}: change must require {} (nothing else can verify a change)",
                c.key(),
                chip_pax::PAX_TEST_CAPABILITY
            ));
        }
        if c.requirements.contains(&Requirement::CargoTestVerification)
            && c.build_systems
                .as_ref()
                .is_none_or(|b| b != &vec![BuildSystem::Cargo])
        {
            return err(format!(
                "{}: Cargo test verification applies only to Cargo projects",
                c.key()
            ));
        }
    }
    Ok(())
}

/// The configuration with this exact key, if the catalog has it.
pub fn find<'a>(catalog: &'a [Configuration], key: &str) -> Option<&'a Configuration> {
    catalog.iter().find(|c| c.key() == key)
}
