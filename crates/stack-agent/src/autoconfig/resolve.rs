//! Selection: policy, then eligibility, then compatibility, then evidence, then a deterministic
//! score, then a stable choice.
//!
//! Hard constraints (policy, stage, isolation, applicability, required capabilities and
//! requirements) decide eligibility. Evidence only ranks configurations that are already eligible;
//! no score can make an ineligible configuration eligible. The same profile, catalog, policy,
//! evidence snapshot and day give the same decision.

use std::collections::BTreeMap;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::catalog::{
    CATALOG_VERSION, CapabilityRule, Configuration, ContextChoice, Isolation, Requirement, Stage,
};
use super::evidence::{EvidenceSource, Features, Origin, OutcomeClass, RECORD_SCHEMA};
use super::profile::{Attr, BuildSystem, ProjectProfile, Verification};
use crate::software_work::GoalKind;

pub const SCORER_VERSION: &str = "score.v1";

/// Evidence-backed scores need at least this much (freshness- and relevance-weighted) evidence.
pub const MIN_EVIDENCE: f64 = 3.0;
/// Evidence older than this is stale and not used.
pub const MAX_AGE_DAYS: u64 = 365;
/// Weight halves every this many days.
pub const HALF_LIFE_DAYS: f64 = 90.0;
/// The score a configuration without enough evidence is ranked at: neither success nor failure.
pub const NEUTRAL: f64 = 0.5;

/// What the command may use, independent of the project.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Stages this command may select. `work` allows production only.
    pub stages: Vec<Stage>,
    /// What the environment provides: the local environment is the user's shared directory.
    pub isolation: Isolation,
    /// Capabilities the caller forbids for this task, whatever a configuration prefers.
    pub prohibited: Vec<String>,
    /// Capability sources (`id@version`) this command can run a configuration with. `work`
    /// provides none.
    pub sources: Vec<String>,
}

impl Policy {
    /// `stack-agent work` in the local environment.
    pub fn work() -> Self {
        Self {
            stages: vec![Stage::Production],
            isolation: Isolation::SharedProjectDirectory,
            prohibited: Vec::new(),
            sources: Vec::new(),
        }
    }
}

/// Which capabilities the environment can provide now, from the profile.
pub fn available(capability: &str, profile: &ProjectProfile) -> Result<(), String> {
    let pax = profile.pax_tool.is(&true);
    match capability {
        "pax.test" if !pax => Err("PAX is not present".into()),
        "project.observe" if !pax => Err("PAX is not present".into()),
        "project.observe" if !profile.primary_build_system.is(&BuildSystem::Cargo) => {
            Err("PAX observation covers Cargo projects only".into())
        }
        c if c.starts_with("project.git.") && !profile.git_repository.is(&true) => {
            Err("the project is not a Git repository".into())
        }
        c if c.starts_with("project.git.") && !profile.git_tool.is(&true) => {
            Err("git is not on PATH".into())
        }
        _ => Ok(()),
    }
}

/// Whether a requirement holds: `Some(true)`, `Some(false)`, or `None` when the profile cannot say.
fn requirement_holds(requirement: Requirement, profile: &ProjectProfile) -> (Option<bool>, String) {
    match requirement {
        Requirement::Model => match &profile.model {
            Attr::Known { .. } => (Some(true), "a model is selected".into()),
            _ => (Some(false), "no model is selected".into()),
        },
        Requirement::CargoTestVerification => match &profile.verification {
            Attr::Known { value: Verification::PaxCargo, .. } => (Some(true), "PAX can verify Cargo tests".into()),
            Attr::Known { value: Verification::PaxUninterpreted, .. } => (
                Some(false),
                "PAX has no authoritative test semantics for this project's build system, so no test result could verify the outcome".into(),
            ),
            Attr::Known { value: Verification::None, .. } => (Some(false), "PAX is not present, so nothing can run the tests".into()),
            Attr::Ambiguous { .. } | Attr::Unknown { .. } => (None, "whether PAX can verify the tests is undecidable from the profile".into()),
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceScore {
    /// Weighted counts of the outcomes that bear on success.
    pub successes: f64,
    pub failures: f64,
    pub unverified: f64,
    /// Weighted evidence behind the score (successes + failures + unverified).
    pub weight: f64,
    /// `(successes + 1) / (successes + failures + unverified/2 + 2)`; `None` below [`MIN_EVIDENCE`].
    pub score: Option<f64>,
    /// The score used for ranking: `score`, or [`NEUTRAL`].
    pub effective: f64,
    pub used: Vec<String>,
    /// Records that matched the configuration but were not counted, and why.
    pub ignored: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Assessment {
    pub configuration: String,
    pub stage: Stage,
    pub eligible: bool,
    /// Hard-constraint failures. Any one makes the configuration ineligible.
    pub exclusions: Vec<String>,
    /// Exclusions the profile could not decide (unknown or ambiguous facts).
    pub undecidable: Vec<String>,
    pub requirements_met: Vec<String>,
    pub compatibility: Vec<String>,
    /// Capabilities that will be offered.
    pub offered: Vec<String>,
    pub preferred_unavailable: Vec<String>,
    /// Available capabilities that will be withheld because they are prohibited.
    pub withheld: Vec<String>,
    pub evidence: Option<EvidenceScore>,
    pub rank: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Outcome {
    Selected {
        configuration: String,
    },
    /// No configuration is eligible and running would be unsafe or pointless: nothing runs.
    Blocked {
        reasons: Vec<String>,
    },
    /// Selection could not decide (undecidable facts or an internal refusal): the existing,
    /// unconfigured behavior runs, and the reason is reported.
    Fallback {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub schema: &'static str,
    pub record: &'static str,
    pub id: String,
    pub catalog_version: &'static str,
    pub scorer_version: &'static str,
    pub profile_version: &'static str,
    pub profile_digest: String,
    pub task_kind: &'static str,
    pub features: Features,
    /// The day (days since the Unix epoch) evidence freshness was computed for.
    pub day: u64,
    pub outcome: Outcome,
    /// `high`: the selected configuration has enough relevant evidence; `evidence_limited`
    /// otherwise (the documented default order decided).
    pub confidence: &'static str,
    pub rationale: String,
    pub assessments: Vec<Assessment>,
    pub evidence_used: Vec<String>,
}

impl Decision {
    pub fn selected<'a>(&self, catalog: &'a [Configuration]) -> Option<&'a Configuration> {
        match &self.outcome {
            Outcome::Selected { configuration } => super::catalog::find(catalog, configuration),
            _ => None,
        }
    }

    pub fn offered(&self) -> Option<&[String]> {
        let Outcome::Selected { configuration } = &self.outcome else {
            return None;
        };
        self.assessments
            .iter()
            .find(|a| &a.configuration == configuration)
            .map(|a| a.offered.as_slice())
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("a decision serializes")
    }
}

pub fn features(kind: GoalKind, profile: &ProjectProfile) -> Features {
    Features {
        task_kind: kind.name().into(),
        build_system: profile
            .primary_build_system
            .known()
            .map(|b| b.name().to_string()),
    }
}

fn score(
    configuration: &Configuration,
    here: &Features,
    day: u64,
    evidence: &dyn EvidenceSource,
) -> EvidenceScore {
    let key = configuration.key();
    let mut s = EvidenceScore {
        successes: 0.0,
        failures: 0.0,
        unverified: 0.0,
        weight: 0.0,
        score: None,
        effective: NEUTRAL,
        used: Vec::new(),
        ignored: BTreeMap::new(),
    };
    let mut ignore = |why: &str| *s.ignored.entry(why.to_string()).or_default() += 1;
    let mut counted: Vec<(&str, f64, OutcomeClass)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for r in evidence.outcomes() {
        if r.schema != RECORD_SCHEMA || r.configuration.as_deref() != Some(key.as_str()) {
            continue;
        }
        // A record supplied twice is one outcome, counted once.
        if !seen.insert(r.id.as_str()) {
            ignore("duplicate record");
            continue;
        }
        if r.origin == Origin::Experiment {
            ignore("experiment origin (informs people, not production selection)");
            continue;
        }
        if r.features.task_kind != here.task_kind {
            ignore("different task kind");
            continue;
        }
        let relevance = match (&r.features.build_system, &here.build_system) {
            (Some(a), Some(b)) if a == b => 1.0,
            (Some(_), Some(_)) => {
                ignore("different build system");
                continue;
            }
            _ => 0.5,
        };
        let age = day.saturating_sub(r.recorded_at / 86_400);
        if age > MAX_AGE_DAYS {
            ignore("stale (older than 365 days)");
            continue;
        }
        if r.recorded_at / 86_400 > day {
            ignore("recorded in the future");
            continue;
        }
        let w = relevance * 0.5f64.powf(age as f64 / HALF_LIFE_DAYS);
        match r.outcome {
            OutcomeClass::Blocked => ignore("blocked (says nothing about success)"),
            OutcomeClass::Cancelled => ignore("cancelled (says nothing about success)"),
            OutcomeClass::RuntimeFailure => ignore("runtime failure (environmental; not counted)"),
            OutcomeClass::InsufficientEvidence => {
                ignore("insufficient evidence (the host could not judge it)")
            }
            class => counted.push((r.id.as_str(), w, class)),
        }
    }
    // Order-independent: sorted by id before summing, so the same snapshot in any order scores the same.
    counted.sort_by(|a, b| a.0.cmp(b.0));
    for (id, w, class) in counted {
        match class {
            OutcomeClass::VerifiedSuccess => s.successes += w,
            OutcomeClass::VerifiedFailure => s.failures += w,
            OutcomeClass::Unverified => s.unverified += w,
            _ => unreachable!("filtered above"),
        }
        s.used.push(id.to_string());
    }
    s.weight = s.successes + s.failures + s.unverified;
    if s.weight >= MIN_EVIDENCE {
        let v = (s.successes + 1.0) / (s.successes + s.failures + s.unverified / 2.0 + 2.0);
        s.score = Some(v);
        s.effective = v;
    }
    s
}

/// The pipeline. `day` is days since the Unix epoch (the caller's clock, recorded in the decision).
pub fn decide(
    profile: &ProjectProfile,
    kind: GoalKind,
    catalog: &[Configuration],
    policy: &Policy,
    evidence: &dyn EvidenceSource,
    day: u64,
) -> Decision {
    let here = features(kind, profile);
    if let Err(e) = super::catalog::validate(catalog) {
        return finish(
            profile,
            kind,
            here,
            day,
            Outcome::Fallback {
                reason: e.to_string(),
            },
            "evidence_limited",
            "the catalog is invalid; the existing behavior runs".into(),
            Vec::new(),
        );
    }
    let mut assessments = Vec::new();
    for c in catalog {
        let mut a = Assessment {
            configuration: c.key(),
            stage: c.stage,
            eligible: false,
            exclusions: Vec::new(),
            undecidable: Vec::new(),
            requirements_met: Vec::new(),
            compatibility: Vec::new(),
            offered: Vec::new(),
            preferred_unavailable: Vec::new(),
            withheld: Vec::new(),
            evidence: None,
            rank: None,
        };
        // 1-2. Hard constraints: applicability, policy, isolation, capabilities, requirements.
        if !c.kinds.contains(&kind) {
            a.exclusions
                .push(format!("does not apply to {} work", kind.name()));
        }
        if !policy.stages.contains(&c.stage) {
            a.exclusions.push(
                format!(
                    "{:?} configurations may not be selected by this command",
                    c.stage
                )
                .to_lowercase(),
            );
        }
        if c.isolation == Isolation::Isolated && policy.isolation != Isolation::Isolated {
            a.exclusions.push(
                "requires an isolated environment; this one is the user's shared project directory"
                    .into(),
            );
        }
        for (id, version) in &c.sources {
            let key = format!("{id}@{version}");
            if !policy.sources.contains(&key) {
                a.exclusions.push(format!(
                    "runs with capability source {key}, which this command does not provide"
                ));
            }
        }
        if let Some(systems) = &c.build_systems {
            match &profile.primary_build_system {
                Attr::Known { value, .. } if systems.contains(value) => a
                    .compatibility
                    .push(format!("build system {} is supported", value.name())),
                Attr::Known { value, .. } => a.exclusions.push(format!(
                    "applies to {} projects; this one is {}",
                    systems
                        .iter()
                        .map(|s| s.name())
                        .collect::<Vec<_>>()
                        .join("/"),
                    value.name()
                )),
                Attr::Ambiguous { candidates, .. } => a.undecidable.push(format!(
                    "the build system is ambiguous ({})",
                    candidates
                        .iter()
                        .map(|s| s.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                Attr::Unknown { reason } => a
                    .undecidable
                    .push(format!("the build system is unknown ({reason})")),
            }
        }
        for (cap, rule) in &c.capabilities {
            let prohibited_by_policy = policy.prohibited.iter().any(|p| p == cap);
            match (rule, available(cap, profile)) {
                (CapabilityRule::Required, _) if prohibited_by_policy => a
                    .exclusions
                    .push(format!("requires {cap}, which this task prohibits")),
                (CapabilityRule::Required, Ok(())) => a.offered.push(cap.to_string()),
                (CapabilityRule::Required, Err(why)) => a
                    .exclusions
                    .push(format!("requires {cap}, which is unavailable: {why}")),
                (CapabilityRule::Preferred, Ok(())) if prohibited_by_policy => {
                    a.withheld.push(cap.to_string())
                }
                (CapabilityRule::Preferred, Ok(())) => a.offered.push(cap.to_string()),
                (CapabilityRule::Preferred, Err(why)) => {
                    a.preferred_unavailable.push(format!("{cap}: {why}"))
                }
                (CapabilityRule::Prohibited, Ok(())) => a.withheld.push(cap.to_string()),
                (CapabilityRule::Prohibited, Err(_)) => {}
            }
        }
        for r in &c.requirements {
            match requirement_holds(*r, profile) {
                (Some(true), why) => a.requirements_met.push(why),
                (Some(false), why) => a.exclusions.push(why),
                (None, why) => a.undecidable.push(why),
            }
        }
        a.offered.sort();
        a.withheld.sort();
        a.eligible = a.exclusions.is_empty() && a.undecidable.is_empty();
        // 3-4. Evidence, for eligible configurations only.
        if a.eligible {
            a.evidence = Some(score(c, &here, day, evidence));
        }
        assessments.push((c, a));
    }

    // 5-6. Rank eligible configurations: effective score, then default rank, then key.
    let mut eligible: Vec<usize> = (0..assessments.len())
        .filter(|i| assessments[*i].1.eligible)
        .collect();
    eligible.sort_by(|x, y| {
        let (cx, ax) = &assessments[*x];
        let (cy, ay) = &assessments[*y];
        let ex = ax.evidence.as_ref().map_or(NEUTRAL, |e| e.effective);
        let ey = ay.evidence.as_ref().map_or(NEUTRAL, |e| e.effective);
        ey.total_cmp(&ex)
            .then(cx.default_rank.cmp(&cy.default_rank))
            .then(cx.key().cmp(&cy.key()))
    });
    for (rank, i) in eligible.iter().enumerate() {
        assessments[*i].1.rank = Some(rank + 1);
    }
    let evidence_used: Vec<String> = {
        let mut ids: Vec<String> = assessments
            .iter()
            .filter_map(|(_, a)| a.evidence.as_ref())
            .flat_map(|e| e.used.iter().cloned())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    };
    let applicable: Vec<&Assessment> = assessments
        .iter()
        .map(|(_, a)| a)
        .filter(|a| {
            !a.exclusions.iter().any(|e| e.starts_with("does not apply"))
                && policy.stages.contains(&a.stage)
        })
        .collect();

    let (outcome, confidence, rationale) = match eligible.first() {
        Some(i) => {
            let (c, a) = &assessments[*i];
            let e = a
                .evidence
                .as_ref()
                .expect("eligible configurations are scored");
            let runner_up = eligible.get(1).map(|j| &assessments[*j]);
            let (confidence, why) = match (e.score, runner_up) {
                (Some(v), Some((rc, ra)))
                    if ra.evidence.as_ref().is_some_and(|r| r.score.is_some()) =>
                {
                    (
                        "high",
                        format!(
                            "ranked first on verified outcomes ({v:.3} against {:.3} for {}) from {:.1} weighted records",
                            ra.evidence.as_ref().unwrap().effective,
                            rc.key(),
                            e.weight
                        ),
                    )
                }
                (Some(v), Some((rc, _))) => (
                    "high",
                    format!(
                        "ranked first on verified outcomes ({v:.3}, from {:.1} weighted records; {} has too little evidence and is ranked neutral)",
                        e.weight,
                        rc.key()
                    ),
                ),
                (Some(v), None) => (
                    "high",
                    format!("the only eligible configuration; verified-outcome score {v:.3}"),
                ),
                (None, Some(_))
                    if eligible[1..].iter().any(|j| {
                        assessments[*j]
                            .1
                            .evidence
                            .as_ref()
                            .is_some_and(|r| r.score.is_some())
                    }) =>
                {
                    (
                        "evidence_limited",
                        format!(
                            "ranked neutral for lack of evidence ({:.1} weighted records), above alternatives whose verified outcomes score below neutral",
                            e.weight
                        ),
                    )
                }
                (None, Some(_)) => (
                    "evidence_limited",
                    format!(
                        "evidence does not differentiate the eligible configurations ({:.1} weighted records; {MIN_EVIDENCE} needed), so the documented default order decided",
                        e.weight
                    ),
                ),
                (None, None) => (
                    "evidence_limited",
                    "the only eligible configuration; there is too little evidence to score it"
                        .into(),
                ),
            };
            (
                Outcome::Selected {
                    configuration: c.key(),
                },
                confidence,
                why,
            )
        }
        None if applicable
            .iter()
            .any(|a| a.exclusions.is_empty() && !a.undecidable.is_empty()) =>
        {
            let reasons: Vec<String> = applicable
                .iter()
                .flat_map(|a| {
                    a.undecidable
                        .iter()
                        .map(move |u| format!("{}: {u}", a.configuration))
                })
                .collect();
            (
                Outcome::Fallback {
                    reason: format!(
                        "eligibility could not be decided ({}); the existing, unconfigured behavior runs",
                        reasons.join("; ")
                    ),
                },
                "evidence_limited",
                "undecidable facts: the baseline runs unchanged".into(),
            )
        }
        None => {
            let mut reasons: Vec<String> = applicable
                .iter()
                .flat_map(|a| {
                    a.exclusions
                        .iter()
                        .map(move |e| format!("{}: {e}", a.configuration))
                })
                .collect();
            if reasons.is_empty() {
                reasons.push(format!(
                    "no {} configuration may be selected by this command",
                    kind.name()
                ));
            }
            (
                Outcome::Blocked { reasons },
                "high",
                "no configuration is eligible; nothing runs".into(),
            )
        }
    };
    finish(
        profile,
        kind,
        here,
        day,
        outcome,
        confidence,
        rationale,
        assessments.into_iter().map(|(_, a)| a).collect::<Vec<_>>(),
    )
    .with_evidence(evidence_used)
}

#[allow(clippy::too_many_arguments)]
fn finish(
    profile: &ProjectProfile,
    kind: GoalKind,
    features: Features,
    day: u64,
    outcome: Outcome,
    confidence: &'static str,
    rationale: String,
    assessments: Vec<Assessment>,
) -> Decision {
    Decision {
        schema: RECORD_SCHEMA,
        record: "selection",
        id: String::new(),
        catalog_version: CATALOG_VERSION,
        scorer_version: SCORER_VERSION,
        profile_version: profile.version,
        profile_digest: profile.digest(),
        task_kind: kind.name(),
        features,
        day,
        outcome,
        confidence,
        rationale,
        assessments,
        evidence_used: Vec::new(),
    }
    .sealed()
}

impl Decision {
    fn with_evidence(mut self, used: Vec<String>) -> Self {
        self.evidence_used = used;
        self.sealed()
    }

    /// The id is the digest of everything else, so the same inputs give the same id.
    fn sealed(mut self) -> Self {
        self.id = String::new();
        let text = serde_json::to_string(&self).expect("a decision serializes");
        self.id = format!(
            "sha256:{}",
            Sha256::digest(text.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        self
    }
}

/// The context policy a configuration names, as the object `work` passes to Chip.
pub fn context_policy(
    choice: ContextChoice,
) -> std::sync::Arc<dyn chip_core::EscalationContextPolicy> {
    match choice {
        ContextChoice::Deduplicated => {
            std::sync::Arc::new(chip_core::DeduplicatedEscalationContext)
        }
        ContextChoice::Full => std::sync::Arc::new(chip_core::FullEscalationContext),
        ContextChoice::InspectProgress => crate::software_work::inspect_context_policy(),
    }
}
