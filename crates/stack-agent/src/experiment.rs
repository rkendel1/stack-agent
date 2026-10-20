//! `stack-agent experiment`: the explicit, opt-in entry point to `chip-experiments`.
//!
//! Nothing else in this binary constructs an experiment: `work`, `verify` and `serve` never load,
//! resolve or run a strategy from here, and no environment variable or configuration changes what
//! they do. This module builds the registry explicitly (the reference experiments plus this
//! binary's production change policy, registered as production so `observe` can run it), validates
//! a definition, runs it, and prints the comparison. Exit codes follow the shared contract.

use std::path::PathBuf;
use std::sync::Arc;

use chip_core::{DeduplicatedEscalationContext, EscalationContextPolicy, LocalWorkPolicy};
use chip_experiments::reference::{self};
use chip_experiments::{
    Conclusion, DefinitionError, EvidenceField, ExperimentDefinition, ExtensionPoint, Mode,
    ModelSpec, Provenance, Redactor, Registry, ResolvedExperiment, RunLimits, Runner, Strategy,
    Verdict, Versioned, report_json, summary,
};

use crate::software_work::CompleteWhenVerified;
use crate::verify::{
    EXIT_NOT_VERIFIED, EXIT_RUNTIME_FAILURE, EXIT_UNAVAILABLE, EXIT_USAGE, EXIT_VERIFIED,
};

const USAGE: &str = "\
usage: stack-agent experiment list [--json]
       stack-agent experiment validate (<id> | --definition FILE) [--json]
       stack-agent experiment run (<id> | --definition FILE) [--json] [--report FILE] [--work-dir DIR]

Runs a registered experiment: a baseline and a candidate strategy on the same controlled tasks,
judged by the same host-defined criteria over host-observed evidence. Opt-in only; `work`, `verify`
and `serve` never run a candidate.

Modes:
  observe   runs only the baseline, which must be this binary's registered production behavior,
            in a disposable directory; nothing is compared and no decision is changed
  replay    runs both sides against recorded capability results and recorded model replies;
            nothing executes, and replay does not reproduce external effects, timing or a live model
  isolated  runs each side in its own disposable directory, offering only capabilities whose
            effects their executor confines to it; a capability nothing confines (git, pax.test)
            is refused. The directory is not an operating-system sandbox.
  shadow and canary are reserved and refused.

No model API key or network is needed: experiments use no model or recorded replies.
--report FILE writes the versioned JSON report (chip.experiment-report.v1); --json prints it.
Ctrl-C stops the current run at its next boundary and starts no further run.

Exit codes: 0 the comparison was established (observe: the baseline was verified),
1 ran correctly but incomplete or not verified, 2 usage or invalid definition,
3 the isolation the mode requires is unavailable (nothing ran), 4 runtime failure (cleanup, report).";

fn usage() -> i32 {
    eprintln!("{USAGE}");
    EXIT_USAGE
}

/// This binary's production policy for change work, exactly what `work --kind change` decides
/// locally, with the escalation context `work` uses for it.
struct ProductionChange;

impl Strategy for ProductionChange {
    fn id(&self) -> &'static str {
        "stack-agent.change"
    }

    fn version(&self) -> u32 {
        1
    }

    fn description(&self) -> &'static str {
        "the production change policy: complete when the runtime's own predicate holds, otherwise ask the model"
    }

    fn extension_points(&self) -> Vec<ExtensionPoint> {
        vec![ExtensionPoint::Decision, ExtensionPoint::Context]
    }

    fn decisions(&self) -> Arc<dyn LocalWorkPolicy> {
        Arc::new(CompleteWhenVerified)
    }

    fn context(&self) -> Arc<dyn EscalationContextPolicy> {
        Arc::new(DeduplicatedEscalationContext)
    }
}

fn observe_change() -> ExperimentDefinition {
    ExperimentDefinition {
        id: "stack-agent.observe-change".into(),
        version: 1,
        description: "observe the production change policy on the explicit retries task, with recorded model replies".into(),
        mode: Mode::Observe,
        baseline: Versioned { id: "stack-agent.change".into(), version: 1 },
        candidate: None,
        tasks: vec![Versioned { id: "retries.explicit".into(), version: 1 }],
        capabilities: vec!["project.read".into(), "project.search".into(), "project.write".into()],
        limits: RunLimits { max_turns: 8, max_executions: 6, deadline_ms: 30_000 },
        criteria: vec![
            "observed.expected-writes".into(),
            "tree.expected-files".into(),
            "tree.no-unexpected-changes".into(),
        ],
        model: ModelSpec::Recorded,
        evidence: vec![
            EvidenceField::Outcome,
            EvidenceField::Criteria,
            EvidenceField::Decisions,
            EvidenceField::Executions,
            EvidenceField::ModelCalls,
            EvidenceField::Duration,
        ],
    }
}

/// The selection-evidence record of one experiment run, from the experiment's host verdict. Its
/// origin is always `experiment`, which the production scorer does not count.
pub fn outcome_record(
    run: &chip_experiments::RunRecord,
    configuration: String,
    task_kind: &str,
    recorded_at: u64,
) -> crate::autoconfig::evidence::OutcomeRecord {
    use chip_experiments::Verdict;
    let outcome = match &run.verdict {
        Verdict::Verified => crate::autoconfig::evidence::OutcomeClass::VerifiedSuccess,
        Verdict::CompletedUnverified { .. } => {
            crate::autoconfig::evidence::OutcomeClass::VerifiedFailure
        }
        Verdict::Incomplete {
            state: "blocked", ..
        } => crate::autoconfig::evidence::OutcomeClass::Blocked,
        Verdict::Incomplete { .. } => crate::autoconfig::evidence::OutcomeClass::Unverified,
        Verdict::RuntimeFailure { .. } => crate::autoconfig::evidence::OutcomeClass::RuntimeFailure,
        Verdict::Cancelled { .. } => crate::autoconfig::evidence::OutcomeClass::Cancelled,
        Verdict::EvidenceInvalid { .. } | Verdict::NotRun { .. } => {
            crate::autoconfig::evidence::OutcomeClass::InsufficientEvidence
        }
    };
    crate::autoconfig::evidence::OutcomeRecord {
        schema: crate::autoconfig::evidence::RECORD_SCHEMA.into(),
        record: "outcome".into(),
        id: String::new(),
        selection_id: None,
        run: run.work_id.clone(),
        configuration: Some(configuration),
        origin: crate::autoconfig::evidence::Origin::Experiment,
        features: crate::autoconfig::evidence::Features {
            task_kind: task_kind.into(),
            build_system: None,
        },
        recorded_at,
        outcome,
        terminal_state: run.terminal_state.unwrap_or("not_run").into(),
        verified: outcome == crate::autoconfig::evidence::OutcomeClass::VerifiedSuccess,
        delivery: run.delivery.map(String::from),
        cancellation: match &run.verdict {
            Verdict::Cancelled { cause, .. } => Some(cause.to_string()),
            _ => None,
        },
        capabilities_invoked: run.executions_by_capability.clone(),
        verification_refs: run
            .evidence
            .iter()
            .map(|e| e.output_sha256.clone())
            .collect(),
        duration_ms: run.duration.map(|d| d.as_millis() as u64),
        tokens: run.tokens.map(|(p, c)| p + c),
        cost: None,
        limitations: vec![format!(
            "experiment run ({}): informs people, not production selection",
            run.task
        )],
    }
    .seal()
}

/// A catalog configuration as an experiment strategy: the goal kind's production decision policy
/// with the configuration's context policy. Only its behavior is compared; its capability and
/// environment rules are the experiment definition's.
struct ConfigurationStrategy {
    id: &'static str,
    description: &'static str,
    context: crate::autoconfig::catalog::ContextChoice,
    sources: Vec<(&'static str, u32)>,
}

impl Strategy for ConfigurationStrategy {
    fn id(&self) -> &'static str {
        self.id
    }

    fn version(&self) -> u32 {
        1
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn extension_points(&self) -> Vec<ExtensionPoint> {
        vec![ExtensionPoint::Decision, ExtensionPoint::Context]
    }

    fn decisions(&self) -> Arc<dyn LocalWorkPolicy> {
        Arc::new(CompleteWhenVerified)
    }

    fn context(&self) -> Arc<dyn EscalationContextPolicy> {
        crate::autoconfig::resolve::context_policy(self.context)
    }

    /// The configuration's capability sources, so an experiment runs it exactly as the catalog
    /// defines it.
    fn sources(&self) -> Vec<Versioned> {
        self.sources
            .iter()
            .map(|(id, version)| Versioned {
                id: (*id).to_string(),
                version: *version,
            })
            .collect()
    }
}

/// The experiment strategy id of a catalog configuration (`config.<id>`).
pub const CONFIG_PREFIX: &str = "config.";

fn configuration_strategies() -> Vec<Arc<dyn Strategy>> {
    crate::autoconfig::catalog::catalog()
        .into_iter()
        .filter(|c| c.kinds.contains(&crate::software_work::GoalKind::Change) && c.version == 1)
        .map(|c| {
            Arc::new(ConfigurationStrategy {
                id: Box::leak(format!("{CONFIG_PREFIX}{}", c.id).into_boxed_str()),
                description: c.description,
                context: c.context,
                sources: c.sources.clone(),
            }) as Arc<dyn Strategy>
        })
        .collect()
}

fn configuration_experiment() -> ExperimentDefinition {
    ExperimentDefinition {
        id: "autoconfig.change-context".into(),
        version: 1,
        description: "the two production change configurations (deduplicated against full escalation context) on the same task, with recorded model replies".into(),
        mode: Mode::Isolated,
        baseline: Versioned { id: "config.change.cargo-verified".into(), version: 1 },
        candidate: Some(Versioned { id: "config.change.cargo-verified-full-context".into(), version: 1 }),
        ..observe_change()
    }
}

/// Everything `stack-agent experiment` can resolve, constructed explicitly.
pub fn registry() -> Registry {
    let mut registry = reference::registry();
    for strategy in configuration_strategies() {
        registry = registry.with_strategy(strategy);
    }
    registry
        .with_production_strategy(Arc::new(ProductionChange))
        .with_experiment(observe_change())
        .with_experiment(configuration_experiment())
}

/// Credential values present in this process, to be scrubbed from every output. They are read only
/// to be removed; nothing here uses them.
fn secrets() -> Vec<String> {
    [
        "CHIP_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        crate::provider_selection::PROXY_PASSWORD_VARIABLE,
    ]
    .iter()
    .filter_map(|name| std::env::var(name).ok())
    .collect()
}

struct Args {
    target: Option<String>,
    definition: Option<PathBuf>,
    json: bool,
    report: Option<PathBuf>,
    work_dir: Option<PathBuf>,
}

fn parse(args: &[String], run: bool) -> Result<Args, i32> {
    let mut out = Args {
        target: None,
        definition: None,
        json: false,
        report: None,
        work_dir: None,
    };
    let mut i = 0;
    while i < args.len() {
        let value = |i: usize| args.get(i + 1).filter(|v| !v.starts_with("--")).cloned();
        match args[i].as_str() {
            "--json" => out.json = true,
            "--definition" => {
                out.definition = Some(value(i).ok_or_else(usage)?.into());
                i += 1;
            }
            "--report" if run => {
                out.report = Some(value(i).ok_or_else(usage)?.into());
                i += 1;
            }
            "--work-dir" if run => {
                out.work_dir = Some(value(i).ok_or_else(usage)?.into());
                i += 1;
            }
            flag if flag.starts_with("--") => {
                eprintln!("error: unexpected argument `{flag}`");
                return Err(usage());
            }
            id if out.target.is_none() => out.target = Some(id.to_string()),
            extra => {
                eprintln!("error: unexpected argument `{extra}`");
                return Err(usage());
            }
        }
        i += 1;
    }
    if out.target.is_some() == out.definition.is_some() {
        eprintln!("error: name exactly one registered experiment id or one --definition file");
        return Err(usage());
    }
    Ok(out)
}

/// The definition the arguments name, resolved; or the exit code and a printed reason.
fn resolve(
    registry: &Registry,
    args: &Args,
    redactor: &Redactor,
) -> Result<ResolvedExperiment, i32> {
    let definition = match (&args.target, &args.definition) {
        (Some(id), _) => match registry.experiment(id) {
            Some(d) => d.clone(),
            None => {
                let known: Vec<&str> = registry.experiments().map(|d| d.id.as_str()).collect();
                return Err(refuse(
                    args.json,
                    &DefinitionError::Malformed(format!(
                        "no experiment `{id}` is registered (registered: {})",
                        known.join(", ")
                    )),
                    "unknown_experiment",
                    redactor,
                ));
            }
        },
        (None, Some(path)) => {
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: cannot read the definition ({})", e.kind());
                    return Err(EXIT_USAGE);
                }
            };
            match ExperimentDefinition::from_json(&text) {
                Ok(d) => d,
                Err(e) => return Err(refuse(args.json, &e, e.code(), redactor)),
            }
        }
        (None, None) => unreachable!("checked by parse"),
    };
    registry
        .resolve(&definition)
        .map_err(|e| refuse(args.json, &e, e.code(), redactor))
}

fn refuse(json: bool, error: &DefinitionError, code: &str, redactor: &Redactor) -> i32 {
    let message = redactor.scrub(&error.to_string());
    if json {
        println!(
            "{}",
            serde_json::json!({"valid": false, "error": {"code": code, "message": message}})
        );
    } else {
        eprintln!("error: experiment definition refused: {message}; nothing was run");
    }
    match error {
        DefinitionError::IsolationUnavailable(_) => EXIT_UNAVAILABLE,
        _ => EXIT_USAGE,
    }
}

fn list(args: &[String]) -> i32 {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return usage(),
    };
    let registry = registry();
    if json {
        let experiments: Vec<_> = registry
            .experiments()
            .map(|d| serde_json::json!({"id": d.id, "version": d.version, "mode": d.mode.name(), "description": d.description}))
            .collect();
        let strategies: Vec<_> = registry
            .strategies()
            .map(|(s, production)| serde_json::json!({"id": s.id(), "version": s.version(), "production": production, "description": s.description()}))
            .collect();
        println!(
            "{}",
            serde_json::json!({"experiments": experiments, "strategies": strategies})
        );
    } else {
        println!("Experiments:");
        for d in registry.experiments() {
            println!(
                "  {}@{} ({}): {}",
                d.id,
                d.version,
                d.mode.name(),
                d.description
            );
        }
        println!("Strategies:");
        for (s, production) in registry.strategies() {
            println!(
                "  {}@{}{}: {}",
                s.id(),
                s.version(),
                if production { " (production)" } else { "" },
                s.description()
            );
        }
    }
    EXIT_VERIFIED
}

fn validate(args: &[String]) -> i32 {
    let Ok(parsed) = parse(args, false) else {
        return EXIT_USAGE;
    };
    let redactor = Redactor::new(secrets());
    let registry = registry();
    match resolve(&registry, &parsed, &redactor) {
        Ok(resolved) => {
            let d = &resolved.definition;
            if parsed.json {
                println!(
                    "{}",
                    serde_json::json!({"valid": true, "id": d.id, "version": d.version, "mode": d.mode.name(), "definition_digest": d.digest()})
                );
            } else {
                println!(
                    "valid: {}@{} ({} mode), {}",
                    d.id,
                    d.version,
                    d.mode.name(),
                    d.digest()
                );
            }
            EXIT_VERIFIED
        }
        Err(code) => code,
    }
}

async fn run(args: &[String]) -> i32 {
    let Ok(parsed) = parse(args, true) else {
        return EXIT_USAGE;
    };
    let redactor = Redactor::new(secrets());
    let registry = registry();
    let resolved = match resolve(&registry, &parsed, &redactor) {
        Ok(r) => r,
        Err(code) => return code,
    };
    let work_dir = parsed
        .work_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("stack-agent-experiments"));
    let runner = Runner::new(work_dir);
    let handle = runner.cancel_handle();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            handle.cancel();
            eprintln!(
                "stack-agent: interrupt received; the current run stops at its next boundary and no further run starts"
            );
        }
    });
    let result = runner.run(&resolved).await;
    let provenance = Provenance {
        host: format!("stack-agent {}", env!("CARGO_PKG_VERSION")),
        source_revision: None,
    };
    let mut report = report_json(&resolved, &result, &provenance, &redactor);
    // Runs of catalog configurations become selection-evidence records, marked `experiment`: the
    // production selection never counts them (docs/product/automatic-configuration.md).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let evidence: Vec<_> = result
        .runs
        .iter()
        .filter_map(|run| {
            let id = run.strategy.strip_prefix(CONFIG_PREFIX)?;
            Some(outcome_record(
                run,
                format!("{id}@{}", run.strategy_version),
                "change",
                now,
            ))
        })
        .collect();
    if !evidence.is_empty() {
        report["selection_evidence"] = serde_json::to_value(&evidence).expect("records serialize");
    }
    let mut code = match (&result.comparison.conclusion, resolved.definition.mode) {
        (Conclusion::Incomplete(_), _) => EXIT_NOT_VERIFIED,
        (Conclusion::NotApplicable(_), _) => {
            if result.runs.iter().all(|r| r.verdict == Verdict::Verified) {
                EXIT_VERIFIED
            } else {
                EXIT_NOT_VERIFIED
            }
        }
        _ => EXIT_VERIFIED,
    };
    if !result.cleanup_failures.is_empty() {
        code = EXIT_RUNTIME_FAILURE;
    }
    if let Some(path) = &parsed.report {
        let text = serde_json::to_string_pretty(&report).expect("a report serializes") + "\n";
        if let Err(e) = std::fs::write(path, text) {
            eprintln!("error: cannot write the report ({})", e.kind());
            code = EXIT_RUNTIME_FAILURE;
        }
    }
    if parsed.json {
        println!("{report}");
    } else {
        print!("{}", summary(&resolved, &result, &redactor));
    }
    code
}

pub async fn experiment(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("list") => list(&args[1..]),
        Some("validate") => validate(&args[1..]),
        Some("run") => run(&args[1..]).await,
        Some("--help" | "-h") => {
            println!("{USAGE}");
            EXIT_VERIFIED
        }
        _ => usage(),
    }
}
