//! `inspect-bounded-v1`: what an inspection's model request carries, chosen by Chip.
//!
//! The policy starts from the same context `inspect-progress-v1` sends (the goal, the limits, the
//! prior decisions, what is ruled out, the repetition-free observations and the inspection-progress
//! summary) and adds a manifest-first status read from the recorded observations. When a context
//! budget is configured and that context would not fit, it condenses recorded observations into
//! facts, largest saving first, until it fits. If it still does not fit, it returns what it has and
//! the loop's existing budget check refuses to send it.
//!
//! Authority: none is added. The policy is a pure function of the recorded trajectory. Every fact
//! is read out of one recorded observation's own output, is labelled as observed or derived, and
//! carries that observation's number and execution ID. The observation stays recorded in full with
//! its receipt; the work report, the audit and grounding see all of it. A failed observation is
//! never condensed, the most recent observation is never condensed, and the loop (not this module)
//! checks every condensation before a request is sent. A model's words are not an input here.

use std::collections::BTreeSet;

use chip_core::{
    CondensedObservation, DeduplicatedEscalationContext, EscalationContext,
    EscalationContextPolicy, ExecutionStatus, Observation, ObservationKind, WorkState,
    WorkTrajectory,
};
use chip_project::{PROJECT_LIST, PROJECT_READ, PROJECT_SEARCH};

use crate::software_work::{inspection_progress, observation_header};

/// The policy's stable identifier.
pub const INSPECT_BOUNDED_ID: &str = "inspect-bounded-v1";

/// File names that declare what a project is and what it depends on, in any ecosystem.
const MANIFEST_NAMES: [&str; 8] = [
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "Gemfile",
    "composer.json",
];
/// Directories that hold build output or vendored code, not project structure.
const NOT_STRUCTURE: [&str; 2] = ["target", "node_modules"];
/// How many names a single line of status or facts lists before saying "N more".
const SHOWN: usize = 12;
const MAX_HEADINGS: usize = 24;
const MAX_EXCERPT_CHARS: usize = 160;

/// The bounded inspection context policy.
#[derive(Debug, Clone, Copy, Default)]
pub struct InspectBoundedContext;

/// For configuration that names the policy.
pub fn inspect_bounded_context_policy() -> std::sync::Arc<dyn EscalationContextPolicy> {
    std::sync::Arc::new(InspectBoundedContext)
}

impl EscalationContextPolicy for InspectBoundedContext {
    fn id(&self) -> &'static str {
        INSPECT_BOUNDED_ID
    }

    fn build(&self, state: &WorkState<'_>, trajectory: &WorkTrajectory<'_>) -> EscalationContext {
        build(state, trajectory, false)
    }
}

pub const INSPECT_COVERED_ID: &str = "inspect-bounded-members-v1";

/// [`InspectBoundedContext`] for an inspection submitted under a coverage contract: the same
/// context, and in addition the workspace members the root manifest declares and the manifests of
/// those not yet read, so the model can see what the contract still requires. Used only when the
/// submitter declared the contract; an inspection without one is not pushed toward exhaustive
/// reading, because what it needs is not known.
#[derive(Debug, Clone, Copy, Default)]
pub struct InspectCoveredContext;

impl EscalationContextPolicy for InspectCoveredContext {
    fn id(&self) -> &'static str {
        INSPECT_COVERED_ID
    }

    fn build(&self, state: &WorkState<'_>, trajectory: &WorkTrajectory<'_>) -> EscalationContext {
        build(state, trajectory, true)
    }
}

fn build(
    state: &WorkState<'_>,
    trajectory: &WorkTrajectory<'_>,
    declared_members: bool,
) -> EscalationContext {
    let mut context = DeduplicatedEscalationContext.build(state, trajectory);
    // The loop's own evidence for this escalation (a rejected decision, the goal's progress) comes
    // first and is kept as it is; the inspection progress follows it.
    let at = context.relevant_evidence.len();
    let progress = inspection_progress(trajectory);
    context.relevant_evidence.push(progress.clone());
    context
        .relevant_evidence
        .push(manifest_status(trajectory, declared_members));
    if declared_members {
        condense_parsed_manifests(&mut context, trajectory);
    }
    if let Some(budget) = state.context_budget_bytes {
        condense_to_fit(&mut context, trajectory, budget);
    }
    if declared_members {
        // The progress summary numbers every observation; a condensed one is already numbered, with
        // its path, in the condensed list, so its entry is not repeated there.
        let numbers = condensed_numbers(&context, trajectory);
        context.relevant_evidence[at] = compact_progress(&progress, &numbers);
        context.prior_decisions = compact_decisions(&context.prior_decisions);
        context.ruled_out = compact_ruled_out(&context.ruled_out);
    }
    context
}

/// Runs of the same decision by the same source, written once: `turns 3-9 (model): request
/// project.read (7 times)`. Under a coverage contract a model reads dozens of manifests in a row,
/// and one line per turn repeats what the numbered observations already say.
fn compact_decisions(decisions: &[String]) -> Vec<String> {
    // "turn N (source): label"
    let parse = |d: &str| -> Option<(usize, String, String)> {
        let rest = d.strip_prefix("turn ")?;
        let (n, rest) = rest.split_once(' ')?;
        let (source, label) = rest.split_once("): ")?;
        Some((
            n.parse().ok()?,
            source.trim_start_matches('(').to_string(),
            label.to_string(),
        ))
    };
    let mut out: Vec<String> = Vec::new();
    let mut run: Option<(usize, usize, String, String)> = None; // first, last, source, label
    let flush = |run: &mut Option<(usize, usize, String, String)>, out: &mut Vec<String>| {
        if let Some((first, last, source, label)) = run.take() {
            out.push(if first == last {
                format!("turn {first} ({source}): {label}")
            } else {
                format!(
                    "turns {first}-{last} ({source}): {label} ({} times)",
                    last - first + 1
                )
            });
        }
    };
    for decision in decisions {
        match (parse(decision), run.as_mut()) {
            (Some((n, source, label)), Some((_, last, rs, rl)))
                if n == *last + 1 && source == *rs && label == *rl =>
            {
                *last = n;
            }
            (Some((n, source, label)), _) => {
                flush(&mut run, &mut out);
                run = Some((n, n, source, label));
            }
            (None, _) => {
                flush(&mut run, &mut out);
                out.push(decision.clone());
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Operations that ran and whose result did not by itself satisfy the goal are one count: every
/// observation is numbered in the context already, and under a coverage contract every read is
/// such an operation until the last. A failure or any other reason to rule something out is kept
/// as written.
fn compact_ruled_out(ruled_out: &[String]) -> Vec<String> {
    const EXECUTED: &str = ": executed, but its observation did not satisfy the goal";
    let (executed, other): (Vec<&String>, Vec<&String>) =
        ruled_out.iter().partition(|r| r.ends_with(EXECUTED));
    let mut out: Vec<String> = other.into_iter().cloned().collect();
    if !executed.is_empty() {
        out.insert(
            0,
            format!(
                "{} operation(s) executed without by themselves satisfying the goal (their observations are numbered above)",
                executed.len()
            ),
        );
    }
    out
}

/// The 1-based numbers of the observations this context carries only as condensed facts.
fn condensed_numbers(
    context: &EscalationContext,
    trajectory: &WorkTrajectory<'_>,
) -> BTreeSet<usize> {
    context
        .condensed
        .iter()
        .filter_map(|c| {
            trajectory
                .observations
                .iter()
                .position(|o| o.execution_id == c.execution_id)
                .map(|i| i + 1)
        })
        .collect()
}

/// `progress` without the "Operation references" entries of observations in `numbers`.
fn compact_progress(progress: &str, numbers: &BTreeSet<usize>) -> String {
    const LABEL: &str = "- Operation references: ";
    progress
        .lines()
        .map(|line| {
            let Some(list) = line.strip_prefix(LABEL) else {
                return line.to_string();
            };
            let entries: Vec<&str> = list.trim_end_matches('.').split("; ").collect();
            let kept: Vec<&str> = entries
                .iter()
                .copied()
                .filter(|e| {
                    e.strip_prefix("Observation ")
                        .and_then(|r| r.split(':').next())
                        .and_then(|n| n.parse::<usize>().ok())
                        .is_none_or(|n| !numbers.contains(&n))
                })
                .collect();
            let dropped = entries.len() - kept.len();
            if dropped == 0 {
                return line.to_string();
            }
            format!(
                "{LABEL}{}{}{dropped} condensed observation(s) are numbered in the Condensed list.",
                kept.join("; "),
                if kept.is_empty() { "" } else { "; " }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Under a coverage contract, a manifest the runtime has parsed (the root, or a declared member) is
/// carried as the one line [`Inventory`] derives from it, not as its text, once a later observation
/// exists: the observation just made is always sent in full, so the model sees each manifest whole
/// once. The observation, its receipt and its parsed facts stay with the runtime, which is what
/// coverage and the answer checks read; only what is re-sent to the model changes.
fn condense_parsed_manifests(context: &mut EscalationContext, trajectory: &WorkTrajectory<'_>) {
    let observations = trajectory.observations;
    let count = observations.len().min(trajectory.origins.len());
    let inventory = crate::inspect_coverage::Inventory::derive(observations);
    let mut chosen: Vec<(usize, CondensedObservation)> = existing(context, trajectory);
    for (i, observation) in observations
        .iter()
        .enumerate()
        .take(count.saturating_sub(1))
    {
        let line = if inventory.root_read == Some(i + 1) {
            inventory.root_line()
        } else {
            inventory
                .members
                .iter()
                .find(|m| m.state == crate::inspect_coverage::MemberState::Read(i + 1))
                .and_then(|m| m.compact_line())
        };
        let still_sent = context
            .relevant_observations
            .iter()
            .any(|o| o.execution_id == observation.execution_id);
        let unique = observations
            .iter()
            .filter(|x| x.execution_id == observation.execution_id)
            .count()
            == 1;
        let (Some(line), true, true, true) = (line, succeeded(observation), still_sent, unique)
        else {
            continue;
        };
        context
            .relevant_observations
            .retain(|o| o.execution_id != observation.execution_id);
        chosen.push((
            i,
            CondensedObservation {
                execution_id: observation.execution_id.clone(),
                withheld_bytes: observation.render().len(),
                facts: vec![line],
            },
        ));
    }
    context.condensed = ordered(&chosen);
}

/// The condensations already chosen for `context`, with their positions in the trajectory.
fn existing(
    context: &EscalationContext,
    trajectory: &WorkTrajectory<'_>,
) -> Vec<(usize, CondensedObservation)> {
    context
        .condensed
        .iter()
        .filter_map(|c| {
            trajectory
                .observations
                .iter()
                .position(|o| o.execution_id == c.execution_id)
                .map(|i| (i, c.clone()))
        })
        .collect()
}

fn succeeded(observation: &Observation) -> bool {
    observation.kind == ObservationKind::ExecutionCompleted
        && observation.status == ExecutionStatus::Success
}

// ---- selection --------------------------------------------------------------------------------

/// Condenses recorded observations until the context fits `budget`, or none can be. Deterministic:
/// candidates are ordered by redundancy (an observation that a later identical one repeats goes first),
/// then by bytes saved (largest first), then by position in the trajectory.
fn condense_to_fit(
    context: &mut EscalationContext,
    trajectory: &WorkTrajectory<'_>,
    budget: usize,
) {
    if context.metrics().bytes <= budget {
        return;
    }
    let observations = trajectory.observations;
    let count = observations.len().min(trajectory.origins.len());
    let sent: Vec<_> = context
        .relevant_observations
        .iter()
        .map(|o| o.execution_id.clone())
        .collect();
    let unique = |o: &Observation| {
        observations
            .iter()
            .filter(|x| x.execution_id == o.execution_id)
            .count()
            == 1
    };

    let mut candidates: Vec<(bool, usize, usize, CondensedObservation)> = Vec::new();
    // The latest observation is what the model is being asked about: it is always sent in full.
    for i in 0..count.saturating_sub(1) {
        let observation = &observations[i];
        if !succeeded(observation)
            || !sent.contains(&observation.execution_id)
            || !unique(observation)
        {
            continue;
        }
        let facts = match trajectory.origins[i].capability.as_str() {
            PROJECT_READ => read_facts(i + 1, observation),
            PROJECT_SEARCH => search_facts(i + 1, observation),
            _ => None,
        };
        let Some(facts) = facts else { continue };
        let entry = CondensedObservation {
            execution_id: observation.execution_id.clone(),
            withheld_bytes: observation.render().len(),
            facts,
        };
        let cost: usize = entry.facts.iter().map(|f| f.len() + 8).sum::<usize>() + 120;
        if entry.withheld_bytes > cost {
            // A later observation of the same invocation with the same result already carries
            // everything this one says, so condensing it loses nothing: it goes first.
            let redundant = (i + 1..count).any(|j| {
                trajectory.origins[j].invocation == trajectory.origins[i].invocation
                    && observations[j].kind == observation.kind
                    && observations[j].status == observation.status
                    && observations[j].output == observation.output
            });
            candidates.push((redundant, entry.withheld_bytes - cost, i, entry));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));

    let mut chosen: Vec<(usize, CondensedObservation)> = existing(context, trajectory);
    for (_, _, index, entry) in candidates {
        context
            .relevant_observations
            .retain(|o| o.execution_id != entry.execution_id);
        chosen.push((index, entry));
        context.condensed = ordered(&chosen);
        if context.metrics().bytes <= budget {
            return;
        }
    }
    // Not enough could be left out. What is returned is the smallest context this policy can
    // justify; the loop's budget check rejects it before anything is sent.
}

fn ordered(chosen: &[(usize, CondensedObservation)]) -> Vec<CondensedObservation> {
    let mut sorted: Vec<&(usize, CondensedObservation)> = chosen.iter().collect();
    sorted.sort_by_key(|(index, _)| *index);
    sorted.into_iter().map(|(_, c)| c.clone()).collect()
}

// ---- facts ------------------------------------------------------------------------------------

/// The text after the observation's header line and marker (`--- content ---` or similar).
pub(crate) fn body(observation: &Observation) -> &str {
    let output = observation.output.as_deref().unwrap_or("");
    output
        .split_once("\n--- ")
        .and_then(|(_, rest)| rest.split_once(" ---\n"))
        .map_or("", |(_, text)| text)
}

fn listed(items: &[String], limit: usize) -> String {
    let mut shown: Vec<String> = items.iter().take(limit).cloned().collect();
    if items.len() > limit {
        shown.push(format!("{} more", items.len() - limit));
    }
    shown.join(", ")
}

/// What a successful `project.read` establishes. Header fields are copied (observed); everything
/// parsed out of the content is marked as derived.
fn read_facts(number: usize, observation: &Observation) -> Option<Vec<String>> {
    let header = observation_header(observation)?;
    let path = header["path"].as_str()?;
    let (bytes, file_bytes) = (header["bytes"].as_u64()?, header["file_bytes"].as_u64()?);
    let complete = header["complete"].as_bool()?;
    let mut facts = vec![format!(
        "observed (observation {number}): project.read of `{path}` returned {bytes} of {file_bytes} bytes; complete: {complete}"
    )];
    let text = body(observation);
    let name = path.rsplit('/').next().unwrap_or(path);
    if !complete {
        facts.push(format!(
            "observation {number} is a partial read; nothing is derived from it"
        ));
    } else if name == "Cargo.toml" {
        facts.extend(cargo_facts(number, path, text));
    } else if name.ends_with(".md") {
        facts.extend(markdown_facts(number, text));
    } else {
        facts.push(format!(
            "derived (observation {number}): {} lines",
            text.lines().count()
        ));
    }
    Some(facts)
}

/// What a Cargo manifest literally declares: its package name and description, its workspace
/// members, its binary targets and the names in its dependency tables, marking `path` and
/// `workspace` declarations. A relationship appears only because a manifest line declares it.
fn cargo_facts(number: usize, path: &str, text: &str) -> Vec<String> {
    let Ok(manifest) = text.parse::<toml::Table>() else {
        return vec![format!(
            "observation {number}: `{path}` did not parse as TOML; no manifest facts are derived"
        )];
    };
    let mut facts = Vec::new();
    if let Some(package) = manifest.get("package").and_then(toml::Value::as_table) {
        if let Some(name) = package.get("name").and_then(toml::Value::as_str) {
            facts.push(format!(
                "derived (observation {number}): package name `{name}`"
            ));
        }
        if let Some(text) = package.get("description").and_then(toml::Value::as_str) {
            facts.push(format!(
                "derived (observation {number}): package description \"{}\"",
                excerpt(text)
            ));
        }
    }
    let workspace = manifest.get("workspace").and_then(toml::Value::as_table);
    if let Some(members) = workspace
        .and_then(|w| w.get("members"))
        .and_then(toml::Value::as_array)
    {
        let members: Vec<String> = members
            .iter()
            .filter_map(|m| m.as_str().map(str::to_string))
            .collect();
        facts.push(format!(
            "derived (observation {number}): [workspace] members ({}): {}",
            members.len(),
            listed(&members, 40)
        ));
    }
    if let Some(bins) = manifest.get("bin").and_then(toml::Value::as_array) {
        let names: Vec<String> = bins
            .iter()
            .filter_map(|b| b.get("name").and_then(toml::Value::as_str))
            .map(str::to_string)
            .collect();
        if !names.is_empty() {
            facts.push(format!(
                "derived (observation {number}): [[bin]] targets: {}",
                names.join(", ")
            ));
        }
    }
    let tables = [
        ("dependencies", manifest.get("dependencies")),
        ("dev-dependencies", manifest.get("dev-dependencies")),
        ("build-dependencies", manifest.get("build-dependencies")),
        (
            "workspace.dependencies",
            workspace.and_then(|w| w.get("dependencies")),
        ),
    ];
    for (label, table) in tables {
        let Some(table) = table.and_then(toml::Value::as_table) else {
            continue;
        };
        let rows: Vec<String> = table
            .iter()
            .map(|(name, spec)| match spec.as_table() {
                Some(t) if t.get("path").and_then(toml::Value::as_str).is_some() => {
                    format!("{name} (path {})", t["path"].as_str().unwrap_or_default())
                }
                Some(t) if t.get("workspace").and_then(toml::Value::as_bool) == Some(true) => {
                    format!("{name} (workspace)")
                }
                _ => name.clone(),
            })
            .collect();
        facts.push(format!(
            "derived (observation {number}): [{label}] declares {} entries: {}",
            rows.len(),
            listed(&rows, 60)
        ));
    }
    facts
}

/// A markdown document's headings and its first line of prose, quoted. Fenced code is skipped.
fn markdown_facts(number: usize, text: &str) -> Vec<String> {
    let (mut headings, mut first, mut fenced) = (Vec::<String>::new(), None, false);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
        } else if fenced || trimmed.is_empty() {
        } else if trimmed.starts_with('#') {
            headings.push(trimmed.to_string());
        } else if first.is_none() {
            first = Some(trimmed.to_string());
        }
    }
    let mut facts = Vec::new();
    if let Some(first) = first {
        facts.push(format!(
            "observed (observation {number}): first line of prose, quoted: \"{}\"",
            excerpt(&first)
        ));
    }
    facts.push(format!(
        "derived (observation {number}): {} headings: {}",
        headings.len(),
        if headings.is_empty() {
            "none".to_string()
        } else {
            listed(&headings, MAX_HEADINGS)
        }
    ));
    facts
}

fn excerpt(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_EXCERPT_CHARS).collect();
    if text.chars().count() > MAX_EXCERPT_CHARS {
        out.push('…');
    }
    out
}

/// What a successful `project.search` located: the match count and the files and lines, never the
/// matched text, which is the part of the observation this stands in for.
fn search_facts(number: usize, observation: &Observation) -> Option<Vec<String>> {
    let header = observation_header(observation)?;
    let matches = header["matches"].as_u64()?;
    let truncated = header["truncated"].as_bool().unwrap_or(false);
    let mut files: std::collections::BTreeMap<String, usize> = Default::default();
    for row in body(observation).lines() {
        if let Some((path, rest)) = row.split_once(':') {
            if rest.split_once(':').is_some() {
                *files.entry(path.to_string()).or_default() += 1;
            }
        }
    }
    let rows: Vec<String> = files.iter().map(|(p, n)| format!("{p} ({n})")).collect();
    Some(vec![
        format!(
            "observed (observation {number}): project.search returned {matches} matches; truncated: {truncated}"
        ),
        format!(
            "derived (observation {number}): matching files: {}",
            if rows.is_empty() {
                "none".to_string()
            } else {
                listed(&rows, SHOWN)
            }
        ),
    ])
}

// ---- manifest-first status --------------------------------------------------------------------

/// Which manifests and documents the recorded observations show, which have been read and which
/// are the next to read. Read from observations only: a model's claim to have read something
/// counts for nothing.
fn manifest_status(trajectory: &WorkTrajectory<'_>, declared_members: bool) -> String {
    let (mut read, mut failed, mut listed_dirs) = (
        std::collections::BTreeMap::<String, usize>::new(),
        BTreeSet::<String>::new(),
        BTreeSet::<String>::new(),
    );
    // (directory listed, the rows of that listing)
    let mut rows: Vec<(String, Vec<(bool, String)>)> = Vec::new();
    let count = trajectory.observations.len().min(trajectory.origins.len());
    for i in 0..count {
        let (observation, origin) = (&trajectory.observations[i], &trajectory.origins[i]);
        let Some(header) = observation_header(observation) else {
            continue;
        };
        let path = header["path"].as_str().unwrap_or(".").to_string();
        match origin.capability.as_str() {
            PROJECT_READ if succeeded(observation) => {
                read.entry(path).or_insert(i + 1);
            }
            PROJECT_READ => {
                failed.insert(path);
            }
            PROJECT_LIST if succeeded(observation) => {
                listed_dirs.insert(path.clone());
                let entries = body(observation)
                    .lines()
                    .filter_map(|row| {
                        let mut parts = row.split(' ');
                        let kind = parts.next()?;
                        let name = parts.next()?.to_string();
                        matches!(kind, "dir" | "file").then_some((kind == "dir", name))
                    })
                    .collect();
                rows.push((path, entries));
            }
            _ => {}
        }
    }
    let basename = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let is_manifest = |p: &str| MANIFEST_NAMES.contains(&basename(p).as_str());
    let is_document = |p: &str| {
        let upper = basename(p).to_ascii_uppercase();
        upper.starts_with("README") || upper.starts_with("ARCHITECTURE")
    };

    if read.is_empty() && rows.is_empty() {
        return "Manifest-first status (derived from recorded observations): no project metadata has been observed yet. Prefer project.list of \".\", then the manifests and README it shows, before searching for implementation symbols.".to_string();
    }

    let unseen = |p: &String| !read.contains_key(p) && !failed.contains(p);
    // Manifests and documents a listing shows that have not been read.
    let unread_seen: Vec<String> = rows
        .iter()
        .flat_map(|(_, entries)| entries.iter().filter(|(dir, _)| !dir))
        .map(|(_, name)| name.clone())
        .filter(|p| (is_manifest(p) || is_document(p)) && unseen(p))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    // The manifest file names this project has shown, to look for in the directories it lists.
    let names: BTreeSet<String> = read
        .keys()
        .filter(|p| is_manifest(p))
        .map(|p| basename(p))
        .chain(
            rows.iter()
                .flat_map(|(_, entries)| entries.iter())
                .filter(|(dir, name)| !dir && is_manifest(name))
                .map(|(_, name)| basename(name)),
        )
        .collect();
    let known_files: BTreeSet<&String> = rows
        .iter()
        .flat_map(|(_, entries)| entries.iter())
        .filter(|(dir, _)| !dir)
        .map(|(_, name)| name)
        .collect();
    let candidates: Vec<String> = rows
        .iter()
        .filter(|(dir, _)| dir != ".")
        .flat_map(|(_, entries)| entries.iter().filter(|(dir, _)| *dir))
        .map(|(_, name)| name)
        .filter(|dir| {
            let leaf = basename(dir);
            !leaf.starts_with('.') && !NOT_STRUCTURE.contains(&leaf.as_str())
        })
        .flat_map(|dir| names.iter().map(move |n| format!("{dir}/{n}")))
        // A directory already listed shows whether the file is there.
        .filter(|p| {
            let parent = p.rsplit_once('/').map_or("", |(d, _)| d);
            !listed_dirs.contains(parent) || known_files.contains(p)
        })
        .filter(unseen)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let numbered: Vec<String> = read
        .iter()
        .filter(|(p, _)| is_manifest(p) || is_document(p))
        .map(|(p, n)| format!("{p} (observation {n})"))
        .collect();
    let mut status = String::from(
        "Manifest-first status, derived from recorded observations (not from any statement by the model):",
    );
    status.push_str(&format!(
        "\n- Manifests and documents read: {}.",
        if numbered.is_empty() {
            "none".into()
        } else {
            listed(&numbered, SHOWN)
        }
    ));
    status.push_str(&format!(
        "\n- Directories already listed (listing them again adds nothing): {}.",
        if listed_dirs.is_empty() {
            "none".into()
        } else {
            listed(&listed_dirs.iter().cloned().collect::<Vec<_>>(), SHOWN)
        }
    ));
    if !unread_seen.is_empty() {
        status.push_str(&format!(
            "\n- Manifests and documents shown by a listing and not yet read: {}.",
            listed(&unread_seen, SHOWN)
        ));
    }
    // A workspace manifest that was read declares its members; those, not a guess from a listing,
    // are what is still to read. Parsed from the recorded observation only.
    let inventory = crate::inspect_coverage::Inventory::derive(trajectory.observations);
    let declared = declared_members && inventory.is_workspace && !inventory.members.is_empty();
    // Without a coverage contract the declared members play no part in the context at all.
    let outstanding: Vec<String> = if declared {
        inventory
            .outstanding()
            .iter()
            .map(|m| m.manifest())
            .collect()
    } else {
        Vec::new()
    };
    if declared {
        status.push_str(&format!(
            "\n- Workspace members declared by Cargo.toml (observation {}): {}; their manifests read: {}.",
            inventory.root_read.unwrap_or_default(),
            inventory.members.len(),
            inventory.read().len()
        ));
        if !outstanding.is_empty() {
            status.push_str(&format!(
                "\n- Declared member manifests not yet read: {}.",
                listed(&outstanding, SHOWN)
            ));
            status.push_str(
                "\n- Coverage is required and a plausible summary does not satisfy it: the runtime accepts an answer only after every declared member manifest was read, so read each outstanding one with project.read first. Manifests already read appear in the Condensed list as `#observation path [package version] -> internal dependencies | dev: ... | external counts`.",
            );
        }
        if outstanding.is_empty() && inventory.status().is_satisfied() {
            status.push_str(&format!(
                "\n- Every declared member manifest is recorded as read. The next decision is the answer: complete, with a summary of at most {} bytes (hard limit {}) that states the member count, the responsibilities and dependencies the manifests establish, and what they do not show; do not list every crate.",
                crate::inspect_coverage::SUMMARY_TARGET_BYTES,
                chip_core::MAX_TEXT_BYTES
            ));
        }
        let gaps = inventory.all_unresolved();
        if !gaps.is_empty() {
            status.push_str(&format!(
                "\n- Declarations that could not be turned into a manifest to read: {}.",
                listed(
                    &gaps
                        .iter()
                        .map(|u| format!("{} ({})", u.candidate, u.reason))
                        .collect::<Vec<_>>(),
                    4
                )
            ));
        }
    }
    let candidates = if declared { Vec::new() } else { candidates };
    if !candidates.is_empty() {
        status.push_str(&format!(
            "\n- Manifests that would sit in the listed directories, not yet read (whether each exists is not yet observed): {}.",
            listed(&candidates, SHOWN)
        ));
    }
    let unread_seen_nothing =
        unread_seen.is_empty() && candidates.is_empty() && outstanding.is_empty();
    status.push_str(if unread_seen_nothing {
        "\n- No unread manifest is known. If the observations establish what was asked, answer now and cite observation numbers; otherwise say what is missing instead of searching for implementation."
    } else {
        "\n- Next: project.read the first unread path above, using the exact project-relative path (no trailing slash), rather than searching for implementation symbols. Do not read a path listed as read."
    });
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_decisions_are_one_line_and_nothing_else_is_merged() {
        let d = |n: usize, source: &str, label: &str| format!("turn {n} ({source}): {label}");
        let input = vec![
            d(1, "model", "request project.list"),
            d(2, "model", "request project.read"),
            d(3, "model", "request project.read"),
            d(4, "model", "request project.read"),
            d(5, "local", "request project.read"),
            d(6, "model", "request project.read"),
            "not a turn line".to_string(),
        ];
        assert_eq!(
            compact_decisions(&input),
            [
                "turn 1 (model): request project.list",
                "turns 2-4 (model): request project.read (3 times)",
                "turn 5 (local): request project.read",
                "turn 6 (model): request project.read",
                "not a turn line",
            ]
        );
        assert!(compact_decisions(&[]).is_empty());
    }

    #[test]
    fn executed_without_satisfying_is_a_count_and_every_other_reason_is_kept() {
        let kept = "capability project.read: its execution failed".to_string();
        let input = vec![
            "capability project.read{path=a}: executed, but its observation did not satisfy the goal".to_string(),
            kept.clone(),
            "capability project.read{path=b}: executed, but its observation did not satisfy the goal".to_string(),
        ];
        let out = compact_ruled_out(&input);
        assert_eq!(out.len(), 2);
        assert!(
            out[0].starts_with("2 operation(s) executed without by themselves satisfying the goal")
        );
        assert_eq!(out[1], kept);
        assert!(compact_ruled_out(&[]).is_empty());
    }

    #[test]
    fn the_progress_summary_drops_only_the_entries_of_condensed_observations() {
        let progress = "Inspection progress:\n- Operation references: Observation 1: project.list succeeded for .; Observation 2: project.read succeeded for Cargo.toml; Observation 3: project.read succeeded for a/Cargo.toml.\n- Source files read: Cargo.toml.";
        let out = compact_progress(progress, &[2usize].into_iter().collect());
        assert!(
            out.contains("Observation 1: project.list succeeded for ."),
            "{out}"
        );
        assert!(
            out.contains("Observation 3: project.read succeeded for a/Cargo.toml"),
            "{out}"
        );
        assert!(!out.contains("Observation 2:"), "{out}");
        assert!(
            out.contains("1 condensed observation(s) are numbered"),
            "{out}"
        );
        assert!(out.contains("- Source files read: Cargo.toml."));
        assert_eq!(compact_progress(progress, &Default::default()), progress);
    }

    #[test]
    fn cargo_facts_state_only_what_the_manifest_declares() {
        let text = r#"
[package]
name = "demo"
description = "A demo"

[[bin]]
name = "demo-bin"

[dependencies]
core = { path = "../core" }
serde = "1"
shared = { workspace = true }

[dev-dependencies]
tempfile = "3"
"#;
        let facts = cargo_facts(3, "crates/demo/Cargo.toml", text).join("\n");
        assert!(facts.contains("package name `demo`"), "{facts}");
        assert!(facts.contains("[[bin]] targets: demo-bin"), "{facts}");
        assert!(
            facts.contains(
                "[dependencies] declares 3 entries: core (path ../core), serde, shared (workspace)"
            ),
            "{facts}"
        );
        assert!(
            facts.contains("[dev-dependencies] declares 1 entries: tempfile"),
            "{facts}"
        );
        assert!(!facts.contains("build-dependencies"), "{facts}");
        assert!(
            facts.lines().all(|l| l.contains("observation 3")),
            "{facts}"
        );
    }

    #[test]
    fn a_manifest_that_does_not_parse_yields_no_derived_facts() {
        let facts = cargo_facts(1, "Cargo.toml", "[package\nname = ");
        assert_eq!(facts.len(), 1);
        assert!(facts[0].contains("did not parse"), "{facts:?}");
    }

    #[test]
    fn markdown_facts_skip_fenced_code_and_quote_the_first_prose_line() {
        let text = "# Title\n\nFirst prose.\n\n```\n# not a heading\n```\n## Part\n";
        let facts = markdown_facts(2, text).join("\n");
        assert!(facts.contains("\"First prose.\""), "{facts}");
        assert!(facts.contains("2 headings: # Title, ## Part"), "{facts}");
        assert!(!facts.contains("not a heading"), "{facts}");
    }
}
