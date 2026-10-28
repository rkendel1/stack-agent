//! `adaptive-v1`: the context assembler for change, verify and act work, with four context
//! profiles (Light, Standard, Deep, Recovery) chosen deterministically from runtime signals.
//!
//! **What every profile carries.** The request's first system message is the work's instructions:
//! the goal, the rules that authorize what may be changed and how, the capability list and the
//! reply contract ([`EscalationContext::render_instructions`]). It is the same bytes in every
//! profile and on every turn. Every profile also carries the work's state (its turn and execution
//! bounds, as the legacy request has them), the loop's own evidence for this request (a rejection
//! diagnostic first), every failed observation in full, what is ruled out, the prior decisions,
//! and every observation it does not carry in full, named with its execution ID and what it
//! establishes. Nothing is cut silently: an observation a profile does not resend is listed under
//! `Condensed` with the bytes withheld, and the loop checks every such entry before the request is
//! sent (`Run::check_condensation`). The observation itself stays recorded in full in the work
//! report, its durable record and the audit.
//!
//! **How the profiles differ.**
//!
//! | Profile | Earlier successful observations in full | Added structure (user message) |
//! |---|---|---|
//! | Light | the latest two, every test run and every failure | task state |
//! | Standard | all, except a read-only observation a later run of the same request superseded with a different result | task state: changed files, test runs, requirements met and outstanding, failures |
//! | Deep | as Standard | Standard, plus every change and the test run that followed it, conflicting evidence, the model's refused claims labelled as claims, what is unresolved and what verification requires |
//! | Recovery | as Standard | Standard, plus what durable state says: restored and re-observed executions, settled actions, last verified progress, attempts, outstanding work and its uncertainty |
//!
//! Standard, Deep and Recovery send the same observations, so moving between them never changes
//! the system prefix a provider cache can reuse; they differ only in the user message, which comes
//! after the cache breakpoint. Light leaves out more, and entering or leaving it changes that
//! prefix (see `docs/product/adaptive-context.md`).
//!
//! **How a profile is selected** ([`select`]): a pure function of the trajectory, the work's kind
//! and [`WorkSignals`]; no model is asked and nothing is learned. Recovery while a resumed work has
//! recorded nothing of its own yet; Deep on conflicting evidence, repeated failed attempts, a
//! refused completion, repeated failed executions, or late in the turn budget with requirements
//! outstanding; Light only when Chip's evaluation finds every requirement met, or for verify work
//! that has seen no failure; otherwise Standard. Each request records the profile, the rule that
//! chose it and the previous profile (`ModelEscalation.profile`).

use chip_core::{
    CondensedObservation, ContextProfile, DecisionSource, DeduplicatedEscalationContext,
    EscalationContext, EscalationContextPolicy, ExecutionStatus, Observation, ObservationClass,
    ObservationKind, ProfileSelection, WorkDecision, WorkSignals, WorkState, WorkTrajectory,
    classify_observations, omissions,
};
use chip_pax::{PAX_TEST_CAPABILITY, PaxExecutionResult};

use crate::software_work::{GoalKind, INSPECT_CAPABILITIES, observation_header};

/// The assembler's stable identifier, recorded with every request it builds.
pub const ADAPTIVE_ID: &str = "adaptive-v1";
/// `work --context-profile auto|light|standard|deep|recovery|replay`.
pub const CONTEXT_PROFILE_FLAG: &str = "--context-profile";
/// The same, for a shell or a harness: the flag beats it.
pub const CONTEXT_PROFILE_VARIABLE: &str = "CHIP_CONTEXT_PROFILE";

/// How many of the latest observations Light always carries in full.
const LIGHT_RECENT: usize = 2;
/// Bytes of a model's own claim shown under Deep's hypotheses before the rest is named, not sent.
const MAX_CLAIM_BYTES: usize = 400;
/// The longest first output line a condensed observation repeats as a fact.
const MAX_HEADER_FACT_BYTES: usize = 2048;

/// How `work` chooses the escalation context of change, verify and act work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMode {
    /// The assembler selects a profile per request from runtime signals (the default).
    Auto,
    /// The assembler, with every request under this one profile: for controlled comparison.
    Fixed(ContextProfile),
    /// The legacy request: `dedup-v1`, every observation resent in order except an earlier one a
    /// later identical observation makes redundant. For controlled comparison.
    Replay,
}

impl Default for ContextMode {
    fn default() -> Self {
        Self::Auto
    }
}

impl ContextMode {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            "auto" => Ok(Self::Auto),
            "replay" => Ok(Self::Replay),
            other => ContextProfile::parse(other)
                .map(Self::Fixed)
                .ok_or_else(|| {
                    format!(
                        "takes auto, light, standard, deep, recovery or replay (got `{}`)",
                        other.chars().take(32).collect::<String>()
                    )
                }),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Replay => "replay",
            Self::Fixed(p) => p.name(),
        }
    }

    /// The flag's value if given, else the variable's, else `auto`. Where it came from is returned
    /// with it, for the report.
    pub fn resolve(
        flag: Option<&str>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<(Self, &'static str), String> {
        if let Some(text) = flag {
            return Self::parse(text)
                .map(|m| (m, "flag"))
                .map_err(|e| format!("{CONTEXT_PROFILE_FLAG} {e}"));
        }
        match env(CONTEXT_PROFILE_VARIABLE).filter(|v| !v.trim().is_empty()) {
            Some(text) => Self::parse(&text)
                .map(|m| (m, "environment"))
                .map_err(|e| format!("{CONTEXT_PROFILE_VARIABLE} {e}")),
            None => Ok((Self::Auto, "default")),
        }
    }
}

/// The adaptive context policy for one work.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveContext {
    pub kind: GoalKind,
    /// `Auto` or `Fixed`; `Replay` is not this policy (see [`policy_for`]).
    pub mode: ContextMode,
}

/// The policy `mode` names for a work of `kind`: `None` for `Replay`, and for an inspection, which
/// keeps its own bounded policy (`inspect-bounded-v1`).
pub fn policy_for(kind: GoalKind, mode: ContextMode) -> Option<AdaptiveContext> {
    (mode != ContextMode::Replay && kind != GoalKind::Inspect)
        .then_some(AdaptiveContext { kind, mode })
}

impl EscalationContextPolicy for AdaptiveContext {
    fn id(&self) -> &'static str {
        ADAPTIVE_ID
    }

    fn build(&self, state: &WorkState<'_>, trajectory: &WorkTrajectory<'_>) -> EscalationContext {
        self.assemble(state, trajectory, &WorkSignals::default()).0
    }

    fn assemble(
        &self,
        state: &WorkState<'_>,
        trajectory: &WorkTrajectory<'_>,
        signals: &WorkSignals,
    ) -> (EscalationContext, Option<ProfileSelection>) {
        let analysis = Analysis::of(trajectory);
        let (profile, reason) = match self.mode {
            ContextMode::Fixed(p) => (p, format!("fixed by {CONTEXT_PROFILE_FLAG} {}", p.name())),
            _ => select(self.kind, state, trajectory, signals, &analysis),
        };
        let (context, sections) = assemble(profile, &reason, state, trajectory, signals, &analysis);
        (
            context,
            Some(ProfileSelection {
                profile,
                reason,
                previous: signals.previous_profile,
                sections,
            }),
        )
    }
}

/// One recorded observation, as the assembler reads it. Derived from the observation and its
/// origin only.
#[derive(Debug, Clone)]
struct Entry {
    execution_id: String,
    capability: String,
    success: bool,
    /// The path a project observation names in its first line, if any.
    path: Option<String>,
    /// A successful write that changed a file's content.
    changed: bool,
    /// Something that may have changed the project (a cli2 task that ran).
    may_change: bool,
    pax: Option<PaxExecutionResult>,
}

/// What the trajectory establishes, computed once per request.
#[derive(Debug, Clone, Default)]
pub(crate) struct Analysis {
    entries: Vec<Entry>,
    /// Indices the deduplicating base leaves out (a later identical observation is sent).
    omitted: Vec<usize>,
    /// `(earlier, later)`: a read-only observation whose request was answered again later with a
    /// different result. The earlier one no longer describes the project.
    stale: Vec<(usize, usize)>,
    /// `(earlier, later)`: the same request answered differently with no change by this work in
    /// between, the later being the latest answer to it and no change following it.
    conflicts: Vec<(usize, usize)>,
}

fn status_name(status: ExecutionStatus) -> &'static str {
    match status {
        ExecutionStatus::Success => "success",
        ExecutionStatus::Failure => "failure",
        ExecutionStatus::Cancelled => "cancelled",
    }
}

impl Analysis {
    pub(crate) fn of(trajectory: &WorkTrajectory<'_>) -> Self {
        let observations = trajectory.observations;
        let origins = trajectory.origins;
        let n = observations.len().min(origins.len());
        let entries: Vec<Entry> = (0..n)
            .map(|i| {
                let o = &observations[i];
                let header = observation_header(o);
                let first = o.output.as_deref().and_then(|t| t.lines().next());
                Entry {
                    execution_id: o.execution_id.to_string(),
                    capability: origins[i].capability.to_string(),
                    success: o.kind == ObservationKind::ExecutionCompleted
                        && o.status == ExecutionStatus::Success,
                    path: header
                        .as_ref()
                        .and_then(|h| h.get("path").and_then(|p| p.as_str()).map(str::to_string)),
                    changed: chip_project::write_summary(o).is_some_and(|(_, changed)| changed),
                    may_change: chip_cli2::may_have_changed(o),
                    pax: (origins[i].capability.as_str() == PAX_TEST_CAPABILITY)
                        .then(|| {
                            first.and_then(|l| chip_pax::parse_execution_result(l.as_bytes()).ok())
                        })
                        .flatten(),
                }
            })
            .collect();
        let classes = classify_observations(&origins[..n], &observations[..n]);
        let omitted: Vec<usize> = omissions(&origins[..n], &observations[..n])
            .into_iter()
            .map(|(o, _)| o)
            .collect();
        let changes_between =
            |a: usize, b: usize| entries[a + 1..b].iter().any(|e| e.changed || e.may_change);
        let mut stale = Vec::new();
        let mut conflicts = Vec::new();
        for j in 0..n {
            if classes[j] != ObservationClass::ChangedReality {
                continue;
            }
            let Some(i) = (0..j)
                .rev()
                .find(|i| origins[*i].invocation == origins[j].invocation)
            else {
                continue;
            };
            if INSPECT_CAPABILITIES.contains(&entries[i].capability.as_str())
                && entries[i].success
                && entries[j].success
            {
                stale.push((i, j));
            }
            let latest = !(j + 1..n).any(|k| origins[k].invocation == origins[j].invocation);
            // A test run's meaning is its first line, the canonical PAX result; the rest (PAX's
            // exit, the project fingerprint, stderr diagnostics with timings) is never evaluated
            // and differs between two runs that established the same thing. Two runs with the
            // same result are not conflicting evidence, however their diagnostics differ.
            let test_run = entries[j].capability == PAX_TEST_CAPABILITY;
            let first_line = |k: usize| {
                observations[k]
                    .output
                    .as_deref()
                    .and_then(|t| t.lines().next())
            };
            let same_meaning = test_run
                && observations[i].kind == observations[j].kind
                && observations[i].status == observations[j].status
                && first_line(i) == first_line(j);
            if latest && !same_meaning && !changes_between(i, j) && !changes_between(j, n) {
                conflicts.push((i, j));
            }
        }
        Self {
            entries,
            omitted,
            stale,
            conflicts,
        }
    }

    fn last_change(&self) -> Option<usize> {
        self.entries.iter().rposition(|e| e.changed)
    }

    /// Test runs after the first change that did not pass, since the last one that did.
    fn failed_attempts(&self) -> usize {
        let Some(first_change) = self.entries.iter().position(|e| e.changed) else {
            return 0;
        };
        let runs: Vec<&PaxExecutionResult> = self.entries[first_change..]
            .iter()
            .filter_map(|e| e.pax.as_ref())
            .collect();
        runs.iter()
            .rev()
            .take_while(|r| !r.is_passing_evidence())
            .count()
    }

    /// Failed observations at the end of the trajectory, in a row.
    fn trailing_failures(&self) -> usize {
        self.entries.iter().rev().take_while(|e| !e.success).count()
    }

    fn any_failure(&self) -> bool {
        self.entries
            .iter()
            .any(|e| !e.success || e.pax.as_ref().is_some_and(|r| !r.is_passing_evidence()))
    }
}

/// Selects the profile of one request and says why. Pure: the same trajectory, kind and signals
/// always give the same profile and the same reason.
pub(crate) fn select(
    kind: GoalKind,
    state: &WorkState<'_>,
    trajectory: &WorkTrajectory<'_>,
    signals: &WorkSignals,
    analysis: &Analysis,
) -> (ContextProfile, String) {
    // Recovery: resumed from durable state, and nothing of this run's own recorded yet.
    let resumed =
        signals.restored_observations + signals.settled_actions > 0 || signals.restored_response;
    if resumed && trajectory.observations.len() <= signals.restored_observations {
        return (
            ContextProfile::Recovery,
            format!(
                "resumed from durable state ({} restored observation(s), {} settled action(s){}); this run has recorded no observation of its own yet",
                signals.restored_observations,
                signals.settled_actions,
                if signals.restored_response {
                    ", a recorded response"
                } else {
                    ""
                }
            ),
        );
    }
    if let Some((i, j)) = analysis.conflicts.last() {
        return (
            ContextProfile::Deep,
            format!(
                "conflicting evidence: executions {} and {} answered the same request differently with no change by this work between or since",
                analysis.entries[*i].execution_id, analysis.entries[*j].execution_id
            ),
        );
    }
    let failed = analysis.failed_attempts();
    if failed >= 2 {
        return (
            ContextProfile::Deep,
            format!(
                "repeated failed attempts: the last {failed} test runs after a change did not pass"
            ),
        );
    }
    if matches!(
        trajectory.decisions.last(),
        Some(d) if d.source == DecisionSource::Model && matches!(d.decision, WorkDecision::Complete { .. })
    ) {
        return (
            ContextProfile::Deep,
            "the model's last decision was a completion Chip refused: the observations do not support it".to_string(),
        );
    }
    let trailing = analysis.trailing_failures();
    if trailing >= 2 {
        return (
            ContextProfile::Deep,
            format!("the last {trailing} executions failed"),
        );
    }
    let outstanding = signals
        .requirements
        .saturating_sub(signals.requirements_met);
    if outstanding > 0
        && analysis.last_change().is_some()
        && 3 * (state.turn + 1) >= 2 * state.max_turns.max(1)
    {
        return (
            ContextProfile::Deep,
            format!(
                "late in the turn budget (turn {} of {}) with {outstanding} requirement(s) outstanding",
                state.turn + 1,
                state.max_turns
            ),
        );
    }
    if signals.requirements > 0 && outstanding == 0 && trailing == 0 {
        return (
            ContextProfile::Light,
            "Chip's evaluation finds every requirement met: the next decision is routine (completing)".to_string(),
        );
    }
    if kind == GoalKind::Verify && !analysis.any_failure() && signals.rejected_in_a_row == 0 {
        return (
            ContextProfile::Light,
            "verify work with no failure observed: the next decision is routine (running the tests)".to_string(),
        );
    }
    match signals.previous_profile {
        Some(previous) if previous != ContextProfile::Standard => (
            ContextProfile::Standard,
            format!("de-escalated from {previous}: the conditions that selected it no longer hold"),
        ),
        _ => (ContextProfile::Standard, "the default".to_string()),
    }
}

/// Bytes, shown with what is withheld; never cut silently.
fn clip_claim(text: &str) -> String {
    let text = text.trim();
    if text.len() <= MAX_CLAIM_BYTES {
        return text.to_string();
    }
    let mut end = MAX_CLAIM_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}… ({} more bytes in the recorded decision)",
        &text[..end],
        text.len() - end
    )
}

fn pax_line(entry: &Entry) -> String {
    match &entry.pax {
        Some(r) => {
            let counts = r.tests.map_or(String::new(), |t| {
                format!(
                    " ({} failed, {} passed, {} ignored)",
                    t.failed, t.passed, t.ignored
                )
            });
            format!(
                "execution {}: {} ({}){counts}",
                entry.execution_id,
                r.status.as_str(),
                r.reason
            )
        }
        None => format!("execution {}: no PAX result", entry.execution_id),
    }
}

/// The facts a condensed observation is carried as: what its own record says, and why it is not
/// resent. Each line says whether it is observed or derived.
fn facts(observation: &Observation, why: String) -> Vec<String> {
    let output = observation.output.as_deref().unwrap_or("");
    let mut facts = vec![format!(
        "observed: {}, status {}, receipt {}",
        observation.kind.as_str(),
        status_name(observation.status),
        observation.receipt_id.as_deref().unwrap_or("none")
    )];
    match output.lines().next() {
        Some(first) if first.len() <= MAX_HEADER_FACT_BYTES => {
            facts.push(format!("observed (first line of its output): {first}"))
        }
        Some(first) => facts.push(format!(
            "observed: the first line of its output is {} bytes and is not repeated here",
            first.len()
        )),
        None => facts.push("observed: no output".to_string()),
    }
    facts.push(format!(
        "observed: output of {} bytes in {} line(s)",
        output.len(),
        output.lines().count()
    ));
    facts.push(format!("derived: {why}"));
    facts
}

/// Which observations `profile` carries as facts instead of in full, with why. Only successful
/// observations, never one the base already omits, never the latest, never a test run.
fn condensations(profile: ContextProfile, analysis: &Analysis) -> Vec<(usize, String)> {
    let n = analysis.entries.len();
    let eligible = |i: usize| {
        let e = &analysis.entries[i];
        // A test run is never condensed, whether or not its result could be parsed.
        e.success
            && e.pax.is_none()
            && e.capability != PAX_TEST_CAPABILITY
            && i + 1 < n
            && !analysis.omitted.contains(&i)
    };
    let mut out: Vec<(usize, String)> = analysis
        .stale
        .iter()
        .filter(|(i, _)| eligible(*i))
        .map(|(i, j)| {
            (
                *i,
                format!(
                    "superseded by execution {}, which answered the same request later with a different result; this earlier answer is no longer current and a new request would not return it",
                    analysis.entries[*j].execution_id
                ),
            )
        })
        .collect();
    out.dedup_by_key(|(i, _)| *i);
    if profile == ContextProfile::Light {
        for i in 0..n.saturating_sub(LIGHT_RECENT) {
            if eligible(i) && !out.iter().any(|(k, _)| *k == i) {
                out.push((
                    i,
                    "not resent under the light profile: not one of the latest observations".into(),
                ));
            }
        }
    }
    out.sort_by_key(|(i, _)| *i);
    out
}

/// Assembles the request for `profile`: the deduplicating base context, the profile's condensation,
/// and the runtime-derived sections. Returns the context and the names of the sections added.
pub(crate) fn assemble(
    profile: ContextProfile,
    reason: &str,
    state: &WorkState<'_>,
    trajectory: &WorkTrajectory<'_>,
    signals: &WorkSignals,
    analysis: &Analysis,
) -> (EscalationContext, Vec<String>) {
    // The state line is the base's, unchanged. The work's id is not added to it: it differs on
    // every run, so it would make two works' first requests differ where nothing about the work
    // does (`--no-recall` promises the request of a project without episodes, byte for byte), and
    // the execution ids in the observations already carry it. A resumed work names it under
    // `recovery`.
    let mut context = DeduplicatedEscalationContext.build(state, trajectory);
    let condensed = condensations(profile, analysis);
    context.condensed = condensed
        .iter()
        .map(|(i, why)| {
            let o = &trajectory.observations[*i];
            CondensedObservation {
                execution_id: o.execution_id.clone(),
                withheld_bytes: o.render().len(),
                facts: facts(o, why.clone()),
            }
        })
        .collect();
    context.relevant_observations.retain(|o| {
        !context
            .condensed
            .iter()
            .any(|c| c.execution_id == o.execution_id)
    });

    let mut sections = vec!["profile".to_string()];
    let mut evidence = vec![format!(
        "context profile: {} ({reason}). Observations not resent in full are listed under Condensed with what they establish; their full text stays in the work's record. Requesting the same capability again returns its current answer, which is not the withheld text when the project has changed since.",
        profile.name()
    )];
    evidence.push(task_state(profile, analysis, signals));
    sections.push("task_state".into());
    if profile == ContextProfile::Deep {
        evidence.push(attempts(analysis));
        sections.push("attempts".into());
        if !analysis.conflicts.is_empty() {
            evidence.push(conflicts(analysis));
            sections.push("conflicts".into());
        }
        if let Some(h) = hypotheses(trajectory) {
            evidence.push(h);
            sections.push("hypotheses".into());
        }
        evidence.push(unresolved(signals));
        sections.push("unresolved".into());
    }
    if profile == ContextProfile::Recovery {
        evidence.push(recovery(trajectory, signals, analysis));
        sections.push("recovery".into());
        evidence.push(attempts(analysis));
        sections.push("attempts".into());
        evidence.push(unresolved(signals));
        sections.push("unresolved".into());
    }
    // The loop's own evidence (a rejection diagnostic, recalled episodes) stays first.
    context.relevant_evidence.extend(evidence);
    (context, sections)
}

/// Where the work stands, as Chip established it from the observations: verified facts first, then
/// what is open. Light carries the same facts without the per-file detail.
fn task_state(profile: ContextProfile, analysis: &Analysis, signals: &WorkSignals) -> String {
    let mut out = String::from("task state (Chip's record, from observations):");
    let mut changed: Vec<(&str, &str)> = Vec::new();
    for e in analysis.entries.iter().filter(|e| e.changed) {
        let path = e.path.as_deref().unwrap_or("?");
        match changed.iter_mut().find(|(p, _)| *p == path) {
            Some(slot) => slot.1 = &e.execution_id,
            None => changed.push((path, &e.execution_id)),
        }
    }
    if changed.is_empty() {
        out.push_str("\n  verified: this work has changed no file");
    } else if profile == ContextProfile::Light {
        out.push_str(&format!(
            "\n  verified: this work changed {} file(s)",
            changed.len()
        ));
    } else {
        out.push_str("\n  verified: files this work changed (latest change):");
        for (path, id) in &changed {
            out.push_str(&format!(" {path} (execution {id});"));
        }
    }
    let first_change = analysis.entries.iter().position(|e| e.changed);
    let last_change = analysis.last_change();
    let tests: Vec<(usize, &Entry)> = analysis
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.pax.is_some() || e.capability == PAX_TEST_CAPABILITY)
        .collect();
    match tests.last() {
        None => out.push_str("\n  verified: no test run yet"),
        Some((i, e)) => {
            let when = match last_change {
                None => "no change has been made",
                Some(c) if *i > c => "after the last change",
                Some(_) => "before the last change, so it does not describe the project now",
            };
            out.push_str(&format!(
                "\n  verified: last test run {} ({when})",
                pax_line(e)
            ));
            let before =
                first_change.and_then(|first| tests.iter().rev().find(|(k, _)| *k < first));
            if let (true, Some((_, before))) = (profile != ContextProfile::Light, before) {
                out.push_str(&format!(
                    "\n  verified: last test run before any change {}",
                    pax_line(before)
                ));
            }
        }
    }
    if signals.requirements > 0 {
        out.push_str(&format!(
            "\n  verified: {} of {} requirement(s) met by Chip's evaluation",
            signals.requirements_met, signals.requirements
        ));
    }
    let failures: Vec<&str> = analysis
        .entries
        .iter()
        .filter(|e| !e.success)
        .map(|e| e.execution_id.as_str())
        .collect();
    if !failures.is_empty() {
        out.push_str(&format!(
            "\n  verified: failed executions (sent in full): {}",
            failures.join(", ")
        ));
    }
    for item in &signals.outstanding {
        out.push_str(&format!("\n  open: {item}"));
    }
    out
}

/// Every change, and the test run that followed it before the next change (Deep, Recovery).
fn attempts(analysis: &Analysis) -> String {
    let mut out = String::from("attempts (each change, and the test run that followed it):");
    let entries = &analysis.entries;
    let changes: Vec<usize> = (0..entries.len()).filter(|i| entries[*i].changed).collect();
    if changes.is_empty() {
        out.push_str("\n  none: no file has been changed");
        return out;
    }
    for (k, c) in changes.iter().enumerate() {
        let until = changes.get(k + 1).copied().unwrap_or(entries.len());
        let test = entries[c + 1..until].iter().find(|e| e.pax.is_some());
        out.push_str(&format!(
            "\n  execution {} changed {}; {}",
            entries[*c].execution_id,
            entries[*c].path.as_deref().unwrap_or("?"),
            match test {
                Some(t) => format!("then test run {}", pax_line(t)),
                None if until < entries.len() => "no test run before the next change".to_string(),
                None => "no test run since".to_string(),
            }
        ));
    }
    out
}

fn conflicts(analysis: &Analysis) -> String {
    let mut out = String::from(
        "conflicting evidence (the same request answered differently with no change by this work between; the later answer is the latest):",
    );
    for (i, j) in &analysis.conflicts {
        let (a, b) = (&analysis.entries[*i], &analysis.entries[*j]);
        out.push_str(&format!(
            "\n  {}: execution {} then execution {}",
            a.capability, a.execution_id, b.execution_id
        ));
        if let (Some(_), Some(_)) = (&a.pax, &b.pax) {
            out.push_str(&format!(" ({} / {})", pax_line(a), pax_line(b)));
        }
    }
    out
}

/// The model's own completion claims that Chip refused: hypotheses, labelled, never evidence.
fn hypotheses(trajectory: &WorkTrajectory<'_>) -> Option<String> {
    let claims: Vec<String> = trajectory
        .decisions
        .iter()
        .filter(|d| d.source == DecisionSource::Model)
        .filter_map(|d| match &d.decision {
            WorkDecision::Complete { summary } => {
                Some(format!("turn {}: {}", d.turn + 1, clip_claim(summary)))
            }
            _ => None,
        })
        .collect();
    (!claims.is_empty()).then(|| {
        format!(
            "hypotheses (the model's own refused completion claims; not evidence):\n  {}",
            claims.join("\n  ")
        )
    })
}

fn unresolved(signals: &WorkSignals) -> String {
    if signals.outstanding.is_empty() {
        return "unresolved: nothing Chip evaluates is outstanding".to_string();
    }
    format!(
        "unresolved (what verification still requires, as Chip will evaluate it):\n  {}",
        signals.outstanding.join("\n  ")
    )
}

/// What durable state says about a resumed work.
fn recovery(trajectory: &WorkTrajectory<'_>, signals: &WorkSignals, analysis: &Analysis) -> String {
    let restored: Vec<&str> = analysis
        .entries
        .iter()
        .take(signals.restored_observations)
        .map(|e| e.execution_id.as_str())
        .collect();
    let mut out = format!(
        "recovery (from durable state): work {} was interrupted and resumed. {} observation(s) restored from earlier attempts ({}), {} of them performed again for a fresh observation; {} action(s) settled by an operator (under ruled out){}.",
        if signals.work_id.is_empty() {
            "?"
        } else {
            &signals.work_id
        },
        signals.restored_observations,
        if restored.is_empty() {
            "none".to_string()
        } else {
            restored.join(", ")
        },
        signals.reobserved,
        signals.settled_actions,
        if signals.restored_response {
            "; a response recorded before the interruption was offered"
        } else {
            ""
        }
    );
    let last_change = analysis.last_change();
    let verified = analysis.entries.iter().enumerate().rev().find(|(i, e)| {
        e.pax.as_ref().is_some_and(|r| r.is_passing_evidence())
            && last_change.is_none_or(|c| *i > c)
    });
    out.push_str(&match verified {
        Some((_, e)) => format!(
            "\n  last verified progress: test run {} after the last change",
            pax_line(e)
        ),
        None => "\n  last verified progress: none; no passing test run after the last change"
            .to_string(),
    });
    out.push_str(&format!(
        "\n  uncertainty: an execution of an earlier attempt with no recorded outcome is either re-observed above or settled under ruled out; nothing else about it is assumed ({} ruled-out note(s))",
        trajectory.ruled_out.len()
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chip_core::{CapabilityId, DecisionRecord, ExecutionId, ObservationOrigin};

    struct T {
        observations: Vec<Observation>,
        origins: Vec<ObservationOrigin>,
        decisions: Vec<DecisionRecord>,
        ruled_out: Vec<String>,
    }

    impl T {
        fn new() -> Self {
            Self {
                observations: vec![],
                origins: vec![],
                decisions: vec![],
                ruled_out: vec![],
            }
        }
        fn push(&mut self, capability: &str, inputs: &str, status: ExecutionStatus, output: &str) {
            let n = self.observations.len() + 1;
            self.observations.push(Observation {
                execution_id: ExecutionId::new(format!("w-exec-{n}")),
                kind: ObservationKind::ExecutionCompleted,
                status,
                output: Some(output.to_string()),
                receipt_id: Some(format!("r{n}")),
                evidence: None,
            });
            self.origins.push(ObservationOrigin {
                capability: CapabilityId::new(capability).unwrap(),
                invocation: format!("{capability}|{inputs}"),
                reusable: false,
                provider_response_id: None,
            });
        }
        fn read(&mut self, path: &str, body: &str) {
            self.push(
                "project.read",
                path,
                ExecutionStatus::Success,
                &format!(
                    "{{\"capability\":\"project.read\",\"path\":\"{path}\",\"bytes\":{}}}\n{body}",
                    body.len()
                ),
            );
        }
        fn write(&mut self, path: &str) {
            self.push(
                "project.write",
                &format!("{path}{}", self.observations.len()),
                ExecutionStatus::Success,
                &format!("{{\"capability\":\"project.write\",\"path\":\"{path}\",\"operation\":\"replaced\",\"changed\":true}}"),
            );
        }
        fn test(&mut self, passed: bool) {
            let (status, failed) = if passed { ("passed", 0) } else { ("failed", 1) };
            self.push(
                "pax.test",
                "",
                ExecutionStatus::Success,
                &format!("{{\"schema\":\"pax.execution-result.v1\",\"operation\":\"test\",\"status\":\"{status}\",\"reason\":\"{}\",\"tool\":\"cargo\",\"exit_code\":{},\"tests\":{{\"passed\":5,\"failed\":{failed},\"ignored\":0,\"measured\":0}}}}", if passed {"tests-passed"} else {"tests-failed"}, if passed {0} else {101}),
            );
        }
        /// A test run as PAX renders it: the canonical result, then diagnostics that vary.
        fn test_run(&mut self, passed: bool, stderr: &str) {
            let (status, failed) = if passed { ("passed", 0) } else { ("failed", 1) };
            self.push(
                "pax.test",
                "",
                ExecutionStatus::Success,
                &format!("{{\"schema\":\"pax.execution-result.v1\",\"operation\":\"test\",\"status\":\"{status}\",\"reason\":\"{}\",\"tool\":\"cargo\",\"exit_code\":{},\"tests\":{{\"passed\":5,\"failed\":{failed},\"ignored\":0,\"measured\":0}}}}\npax_process_exit: 0\n--- stderr (diagnostics only; never evaluated) ---\n{stderr}", if passed {"tests-passed"} else {"tests-failed"}, if passed {0} else {101}),
            );
        }
        fn fail(&mut self, capability: &str, inputs: &str) {
            self.push(
                capability,
                inputs,
                ExecutionStatus::Failure,
                "{\"error\":\"not_found\"}",
            );
        }
        fn trajectory(&self) -> WorkTrajectory<'_> {
            WorkTrajectory {
                observations: &self.observations,
                origins: &self.origins,
                decisions: &self.decisions,
                ruled_out: &self.ruled_out,
                evidence: &[],
                question: "Q",
            }
        }
    }

    fn state(turn: usize) -> WorkState<'static> {
        WorkState {
            goal: "G",
            turn,
            max_turns: 12,
            executions: 0,
            max_executions: 8,
            context_budget_bytes: None,
        }
    }

    fn signals() -> WorkSignals {
        WorkSignals {
            work_id: "work_0123".into(),
            requirements: 1,
            outstanding: vec!["a project file was changed and PAX passed after it".into()],
            ..WorkSignals::default()
        }
    }

    fn run(
        mode: ContextMode,
        kind: GoalKind,
        t: &T,
        turn: usize,
        s: &WorkSignals,
    ) -> (EscalationContext, ProfileSelection) {
        let (c, p) = AdaptiveContext { kind, mode }.assemble(&state(turn), &t.trajectory(), s);
        (c, p.expect("adaptive records a profile"))
    }

    fn sent(c: &EscalationContext) -> Vec<String> {
        c.relevant_observations
            .iter()
            .map(|o| o.execution_id.to_string())
            .collect()
    }

    #[test]
    fn standard_is_the_default_and_keeps_every_current_observation() {
        let mut t = T::new();
        t.read("src/a.rs", "fn a() {}");
        t.read("src/b.rs", "fn b() {}");
        let (c, p) = run(ContextMode::Auto, GoalKind::Change, &t, 0, &signals());
        assert_eq!(p.profile, ContextProfile::Standard);
        assert_eq!(p.reason, "the default");
        assert_eq!(sent(&c), ["w-exec-1", "w-exec-2"]);
        assert!(c.condensed.is_empty());
        // No per-run identity in the state line: the base's state line, unchanged.
        assert_eq!(
            c.current_state,
            DeduplicatedEscalationContext
                .build(&state(0), &t.trajectory())
                .current_state
        );
        assert!(c.render_turn().contains("open: a project file was changed"));
    }

    #[test]
    fn standard_condenses_only_a_read_a_later_read_superseded_with_provenance() {
        let mut t = T::new();
        t.read("src/a.rs", "old body");
        t.write("src/a.rs");
        t.read("src/a.rs", "new body");
        t.read("src/b.rs", "b");
        let (c, p) = run(ContextMode::Auto, GoalKind::Change, &t, 1, &signals());
        assert_eq!(p.profile, ContextProfile::Standard);
        assert_eq!(sent(&c), ["w-exec-2", "w-exec-3", "w-exec-4"]);
        assert_eq!(c.condensed.len(), 1);
        let item = &c.condensed[0];
        assert_eq!(item.execution_id.to_string(), "w-exec-1");
        assert_eq!(item.withheld_bytes, t.observations[0].render().len());
        let facts = item.facts.join("\n");
        assert!(facts.contains("receipt r1"), "{facts}");
        assert!(facts.contains("\"path\":\"src/a.rs\""), "{facts}");
        assert!(
            facts.contains("superseded by execution w-exec-3"),
            "{facts}"
        );
        assert!(!facts.contains("old body"));
    }

    #[test]
    fn light_leaves_out_old_successful_output_but_keeps_failures_tests_and_the_latest() {
        let mut t = T::new();
        t.read("src/big.rs", &"x".repeat(5000));
        t.fail("project.read", "src/missing.rs");
        t.test(true);
        t.read("src/c.rs", "c");
        t.read("src/d.rs", "d");
        let s = WorkSignals {
            requirements: 1,
            requirements_met: 1,
            outstanding: vec![],
            ..signals()
        };
        let (c, p) = run(ContextMode::Auto, GoalKind::Act, &t, 3, &s);
        assert_eq!(p.profile, ContextProfile::Light);
        // The large old read is not resent; its identity and header are.
        assert_eq!(sent(&c), ["w-exec-2", "w-exec-3", "w-exec-4", "w-exec-5"]);
        assert_eq!(c.condensed[0].execution_id.to_string(), "w-exec-1");
        assert!(!c.render_turn().contains(&"x".repeat(100)));
        assert!(
            c.render_turn()
                .contains("failed executions (sent in full): w-exec-2")
        );
    }

    #[test]
    fn every_profile_keeps_instructions_identity_rejections_ruled_out_and_failures() {
        let mut t = T::new();
        t.read("src/a.rs", "a");
        t.write("src/a.rs");
        t.fail("project.read", "nope");
        t.read("src/a.rs", "a2");
        t.ruled_out
            .push("capability project.read|nope: its execution failed".into());
        let evidence =
            vec!["turn 3: your reply was rejected and nothing in it was done".to_string()];
        for profile in ContextProfile::ALL {
            let traj = WorkTrajectory {
                evidence: &evidence,
                ..t.trajectory()
            };
            let (c, p) = AdaptiveContext {
                kind: GoalKind::Change,
                mode: ContextMode::Fixed(profile),
            }
            .assemble(&state(2), &traj, &signals());
            let p = p.unwrap();
            assert_eq!(p.profile, profile);
            assert_eq!(c.render_instructions(), "Goal: G\nQuestion: Q", "{profile}");
            assert!(!c.current_state.contains("work_0123"), "{profile}");
            assert_eq!(
                c.relevant_evidence[0], evidence[0],
                "{profile}: rejection first"
            );
            assert_eq!(c.ruled_out, t.ruled_out, "{profile}");
            assert!(
                sent(&c).contains(&"w-exec-3".to_string()),
                "{profile}: failure in full"
            );
            assert!(
                c.render_turn()
                    .contains("Next: reply with one JSON decision object.")
            );
        }
    }

    #[test]
    fn deep_on_repeated_failed_attempts_with_history_and_no_blind_replay() {
        let mut t = T::new();
        t.read("src/a.rs", "v0");
        t.write("src/a.rs");
        t.test(false);
        t.read("src/a.rs", "v1");
        t.write("src/a.rs");
        t.test(false);
        let (c, p) = run(ContextMode::Auto, GoalKind::Change, &t, 5, &signals());
        assert_eq!(p.profile, ContextProfile::Deep);
        assert!(
            p.reason
                .contains("last 2 test runs after a change did not pass"),
            "{}",
            p.reason
        );
        let turn = c.render_turn();
        assert!(
            turn.contains(
                "execution w-exec-2 changed src/a.rs; then test run execution w-exec-3: failed"
            ),
            "{turn}"
        );
        assert!(
            turn.contains(
                "execution w-exec-5 changed src/a.rs; then test run execution w-exec-6: failed"
            ),
            "{turn}"
        );
        assert!(
            turn.contains("unresolved (what verification still requires"),
            "{turn}"
        );
        // The stale read is not replayed: Deep sends what Standard sends.
        assert_eq!(c.condensed.len(), 1);
        assert!(!sent(&c).contains(&"w-exec-1".to_string()));
        assert_eq!(
            p.sections,
            ["profile", "task_state", "attempts", "unresolved"]
        );
    }

    #[test]
    fn deep_on_conflicting_evidence_and_on_a_refused_completion() {
        let mut t = T::new();
        t.write("src/a.rs");
        t.test(true);
        t.test(false);
        let (_, p) = run(ContextMode::Auto, GoalKind::Change, &t, 3, &signals());
        assert_eq!(p.profile, ContextProfile::Deep);
        assert!(
            p.reason
                .starts_with("conflicting evidence: executions w-exec-2 and w-exec-3"),
            "{}",
            p.reason
        );

        let mut t = T::new();
        t.read("src/a.rs", "a");
        t.decisions.push(DecisionRecord {
            turn: 1,
            source: DecisionSource::Model,
            decision: WorkDecision::Complete {
                summary: "done, trust me".into(),
            },
        });
        let (c, p) = run(ContextMode::Auto, GoalKind::Change, &t, 2, &signals());
        assert_eq!(p.profile, ContextProfile::Deep);
        let turn = c.render_turn();
        assert!(
            turn.contains("hypotheses (the model's own refused completion claims; not evidence)")
        );
        assert!(turn.contains("turn 2: done, trust me"));
        // A claim is never stated as verified.
        assert!(!turn.contains("verified: done"));
    }

    #[test]
    fn two_test_runs_with_the_same_result_are_not_conflicting_evidence_whatever_their_diagnostics() {
        // The same result, with different timings in the diagnostics (what cargo prints on every
        // run): not a conflict, so not Deep for that reason.
        let mut t = T::new();
        t.write("src/a.rs");
        t.test_run(true, "Finished `test` profile in 0.52s");
        t.test_run(true, "Finished `test` profile in 0.61s");
        let (c, p) = run(ContextMode::Auto, GoalKind::Change, &t, 3, &signals());
        assert!(!p.reason.starts_with("conflicting evidence"), "{}", p.reason);
        assert!(!c.render_turn().contains("conflicting evidence"));
        // A different result with no change between is a conflict, whatever the diagnostics say.
        let mut t = T::new();
        t.write("src/a.rs");
        t.test_run(true, "Finished `test` profile in 0.52s");
        t.test_run(false, "Finished `test` profile in 0.52s");
        let (_, p) = run(ContextMode::Auto, GoalKind::Change, &t, 3, &signals());
        assert_eq!(p.profile, ContextProfile::Deep);
        assert!(p.reason.starts_with("conflicting evidence"), "{}", p.reason);
    }

    #[test]
    fn light_never_condenses_a_test_run_even_one_whose_result_did_not_parse() {
        let mut t = T::new();
        t.push(
            "pax.test",
            "",
            ExecutionStatus::Success,
            "not a pax result\nsomething else",
        );
        t.read("src/c.rs", "c");
        t.read("src/d.rs", "d");
        let (c, _) = run(
            ContextMode::Fixed(ContextProfile::Light),
            GoalKind::Change,
            &t,
            3,
            &signals(),
        );
        assert!(sent(&c).contains(&"w-exec-1".to_string()), "{:?}", sent(&c));
        assert!(c.condensed.is_empty());
    }

    #[test]
    fn a_superseded_read_is_never_said_to_be_retrievable_by_asking_again() {
        let mut t = T::new();
        t.read("src/a.rs", "old body");
        t.write("src/a.rs");
        t.read("src/a.rs", "new body");
        let (c, _) = run(ContextMode::Auto, GoalKind::Change, &t, 2, &signals());
        let turn = c.render_turn();
        assert!(!turn.contains("request the capability again to see one in full"));
        assert!(turn.contains("a new request would not return it"), "{turn}");
        assert!(turn.contains("full text stays in the work's record"), "{turn}");
    }

    #[test]
    fn reads_of_different_ranges_or_paths_are_never_superseded_by_each_other() {
        // Different inputs are different requests: a read of other lines, or of another path,
        // supersedes nothing, whatever it returned and whatever changed between.
        let mut t = T::new();
        t.push(
            "project.read",
            "src/a.rs|lines=1-50",
            ExecutionStatus::Success,
            "{\"capability\":\"project.read\",\"path\":\"src/a.rs\"}\nfirst half",
        );
        t.write("src/a.rs");
        t.push(
            "project.read",
            "src/a.rs|lines=51-100",
            ExecutionStatus::Success,
            "{\"capability\":\"project.read\",\"path\":\"src/a.rs\"}\nsecond half",
        );
        t.read("src/a.rs", "whole file");
        let (c, p) = run(ContextMode::Auto, GoalKind::Change, &t, 3, &signals());
        assert_eq!(p.profile, ContextProfile::Standard);
        assert!(c.condensed.is_empty(), "{:?}", c.condensed);
        assert_eq!(sent(&c).len(), 4);
        // The same request answered again only after a failed attempt to read it: the failure is
        // never what supersedes, and the earlier successful answer stays.
        let mut t = T::new();
        t.read("src/a.rs", "body");
        t.push(
            "project.read",
            "src/a.rs",
            ExecutionStatus::Failure,
            "{\"error\":\"unreadable\"}",
        );
        t.read("src/b.rs", "b");
        let (c, _) = run(ContextMode::Auto, GoalKind::Change, &t, 3, &signals());
        assert!(c.condensed.is_empty(), "{:?}", c.condensed);
    }

    #[test]
    fn escalation_and_de_escalation_are_deterministic_and_recorded() {
        let mut t = T::new();
        t.write("src/a.rs");
        t.test(false);
        t.write("src/a.rs");
        t.test(false);
        let s = signals();
        let (a, pa) = run(ContextMode::Auto, GoalKind::Change, &t, 4, &s);
        let (b, pb) = run(ContextMode::Auto, GoalKind::Change, &t, 4, &s);
        assert_eq!((a, pa.clone()), (b, pb));
        assert_eq!(pa.profile, ContextProfile::Deep);
        // A passing run after a further change: the failures no longer hold, back to Standard.
        t.write("src/a.rs");
        t.test(true);
        let s = WorkSignals {
            previous_profile: Some(ContextProfile::Deep),
            ..signals()
        };
        let (_, p) = run(ContextMode::Auto, GoalKind::Change, &t, 5, &s);
        assert_eq!(p.profile, ContextProfile::Standard);
        assert_eq!(p.previous, Some(ContextProfile::Deep));
        assert!(
            p.reason.starts_with("de-escalated from deep"),
            "{}",
            p.reason
        );
    }

    #[test]
    fn recovery_reconstructs_a_resumed_work_from_durable_evidence_then_steps_down() {
        let mut t = T::new();
        t.read("src/a.rs", "a");
        t.write("src/a.rs");
        t.test(false);
        t.ruled_out.push("capability project.write (inputs not recorded): an operator (ops) concluded that its effect occurred, but its outcome was never observed".into());
        let s = WorkSignals {
            restored_observations: 3,
            reobserved: 1,
            settled_actions: 1,
            ..signals()
        };
        let (c, p) = run(ContextMode::Auto, GoalKind::Change, &t, 0, &s);
        assert_eq!(p.profile, ContextProfile::Recovery);
        let turn = c.render_turn();
        assert!(
            turn.contains("work work_0123 was interrupted and resumed"),
            "{turn}"
        );
        assert!(turn.contains("3 observation(s) restored from earlier attempts (w-exec-1, w-exec-2, w-exec-3), 1 of them performed again"), "{turn}");
        assert!(turn.contains("last verified progress: none"), "{turn}");
        assert!(
            turn.contains(
                "execution w-exec-2 changed src/a.rs; then test run execution w-exec-3: failed"
            ),
            "{turn}"
        );
        assert!(turn.contains("open: a project file was changed"), "{turn}");
        assert!(turn.contains("an operator (ops) concluded"), "{turn}");
        // Every restored observation is still sent.
        assert_eq!(sent(&c).len(), 3);
        // Once this run records an observation of its own, recovery is over.
        t.read("src/b.rs", "b");
        let s = WorkSignals {
            previous_profile: Some(ContextProfile::Recovery),
            ..s
        };
        let (_, p) = run(ContextMode::Auto, GoalKind::Change, &t, 1, &s);
        assert_ne!(p.profile, ContextProfile::Recovery);
    }

    #[test]
    fn missing_evidence_is_never_reported_as_verified() {
        let t = T::new();
        let (c, _) = run(ContextMode::Auto, GoalKind::Change, &t, 0, &signals());
        let turn = c.render_turn();
        assert!(turn.contains("verified: this work has changed no file"));
        assert!(turn.contains("verified: no test run yet"));
        assert!(!turn.contains("verified: last test run"));
        assert!(turn.contains("verified: 0 of 1 requirement(s) met"));
        assert!(turn.contains("open: "));
    }

    #[test]
    fn a_test_run_before_the_last_change_is_said_not_to_describe_the_project() {
        let mut t = T::new();
        t.write("src/a.rs");
        t.test(true);
        t.write("src/a.rs");
        let (c, _) = run(ContextMode::Auto, GoalKind::Change, &t, 3, &signals());
        assert!(
            c.render_turn()
                .contains("before the last change, so it does not describe the project now")
        );
    }

    #[test]
    fn verify_work_with_no_failure_is_light_and_a_failure_ends_it() {
        let t = T::new();
        let (_, p) = run(ContextMode::Auto, GoalKind::Verify, &t, 0, &signals());
        assert_eq!(p.profile, ContextProfile::Light);
        let mut t = T::new();
        t.test(false);
        let (_, p) = run(ContextMode::Auto, GoalKind::Verify, &t, 1, &signals());
        assert_eq!(p.profile, ContextProfile::Standard);
    }

    #[test]
    fn modes_parse_and_replay_and_inspect_keep_their_policies() {
        assert_eq!(ContextMode::parse("auto"), Ok(ContextMode::Auto));
        assert_eq!(ContextMode::parse("replay"), Ok(ContextMode::Replay));
        assert_eq!(
            ContextMode::parse(" deep "),
            Ok(ContextMode::Fixed(ContextProfile::Deep))
        );
        assert!(ContextMode::parse("full").is_err());
        assert!(policy_for(GoalKind::Change, ContextMode::Replay).is_none());
        assert!(policy_for(GoalKind::Inspect, ContextMode::Auto).is_none());
        assert!(policy_for(GoalKind::Act, ContextMode::Auto).is_some());
        let env = |_: &str| Some("light".to_string());
        assert_eq!(
            ContextMode::resolve(None, &env),
            Ok((ContextMode::Fixed(ContextProfile::Light), "environment"))
        );
        assert_eq!(
            ContextMode::resolve(Some("replay"), &env),
            Ok((ContextMode::Replay, "flag"))
        );
        assert!(ContextMode::resolve(None, &|_| Some("bogus".into())).is_err());
    }
}
