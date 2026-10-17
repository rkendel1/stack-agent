//! Evidence-driven automatic configuration for `stack-agent work --auto`.
//!
//! The user describes the work; with `--auto`, stack-agent profiles the project
//! ([`profile::ProjectProfile`]), resolves a configuration from the built-in catalog
//! ([`catalog::catalog`]) under policy and hard constraints, ranks eligible configurations by
//! recorded verified outcomes ([`resolve::decide`]), runs the work through the same Chip loop,
//! FX boundary and verification as always (only narrowing the capabilities offered and choosing an
//! existing context policy), and returns selection and outcome records ([`evidence`]).
//!
//! Without `--auto`, none of this runs and `work` is unchanged. See
//! `docs/product/automatic-configuration.md`.

pub mod catalog;
pub mod evidence;
pub mod profile;
pub mod resolve;

use profile::{Attr, ProjectProfile};
use resolve::{Decision, Outcome};

/// What is said about persistence wherever records are shown.
pub const PERSISTENCE: &str = "not persisted: stack-agent stores no evidence; keep the records from --json output and pass them back with --selection-evidence";

/// The machine-readable section `work --auto --json` adds to its report.
pub fn report_json(
    profile: &ProjectProfile,
    decision: &Decision,
    outcome: Option<&evidence::OutcomeRecord>,
) -> serde_json::Value {
    serde_json::json!({
        "profile": profile.to_json(),
        "profile_digest": profile.digest(),
        "decision": decision.to_json(),
        "outcome_record": outcome,
        "persistence": PERSISTENCE,
    })
}

fn attr<T: std::fmt::Debug>(a: &Attr<T>, show: impl Fn(&T) -> String) -> String {
    match a {
        Attr::Known {
            value, evidence, ..
        } => format!("{} ({})", show(value), evidence.join(", ")),
        Attr::Ambiguous { candidates, .. } => format!(
            "ambiguous: {}",
            candidates.iter().map(&show).collect::<Vec<_>>().join(", ")
        ),
        Attr::Unknown { reason } => format!("unknown ({reason})"),
    }
}

/// The concise human section.
pub fn report_human(
    profile: &ProjectProfile,
    decision: &Decision,
    outcome: Option<&evidence::OutcomeRecord>,
) -> String {
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };
    line(format!(
        "Automatic configuration ({}, {}, {}):",
        decision.catalog_version, decision.scorer_version, decision.profile_version
    ));
    line(format!(
        "  Project: build system {}; tests {}; Git repository {}; verification {}",
        attr(&profile.primary_build_system, |b| b.name().to_string()),
        attr(&profile.has_tests, |t| if *t {
            "present"
        } else {
            "not found"
        }
        .to_string()),
        attr(&profile.git_repository, |g| if *g { "yes" } else { "no" }
            .to_string()),
        attr(&profile.verification, |v| format!("{v:?}")),
    ));
    match &decision.outcome {
        Outcome::Selected { configuration } => {
            line(format!(
                "  Selected: {configuration} ({}): {}",
                decision.confidence, decision.rationale
            ));
            if let Some(a) = decision
                .assessments
                .iter()
                .find(|a| &a.configuration == configuration)
            {
                line(format!("  Offered: {}", a.offered.join(", ")));
                if !a.withheld.is_empty() {
                    line(format!(
                        "  Withheld (prohibited): {}",
                        a.withheld.join(", ")
                    ));
                }
                if !a.preferred_unavailable.is_empty() {
                    line(format!(
                        "  Preferred but unavailable: {}",
                        a.preferred_unavailable.join("; ")
                    ));
                }
            }
        }
        Outcome::Blocked { reasons } => {
            line(
                "  Blocked: no configuration can do this work safely here; nothing was run.".into(),
            );
            for r in reasons {
                line(format!("    {r}"));
            }
        }
        Outcome::Fallback { reason } => {
            line(format!("  Fallback to the existing behavior: {reason}"))
        }
    }
    for a in decision.assessments.iter().filter(|a| !a.eligible) {
        let why: Vec<&str> = a
            .exclusions
            .iter()
            .chain(&a.undecidable)
            .map(String::as_str)
            .collect();
        line(format!(
            "  Not eligible: {}: {}",
            a.configuration,
            why.join("; ")
        ));
    }
    if !decision.evidence_used.is_empty() {
        line(format!(
            "  Evidence used: {} outcome record(s)",
            decision.evidence_used.len()
        ));
    }
    if let Some(o) = outcome {
        line(format!(
            "  Outcome: {} (record {}; {PERSISTENCE})",
            o.outcome.name(),
            o.id
        ));
    }
    out
}
