//! **Experimental, read-only** selection of the pattern observations relevant to a task.
//!
//! Given a task representation (files, graph symbol ids and explicit families the *caller* names), a
//! pattern report, the graph it refers to and the report's staleness, it returns which patterns bear
//! on the task, why, with a bounded number of supporting examples and counterexamples. It is not a
//! policy engine and it decides nothing: it reads the report and the graph and changes neither.
//!
//! What it deliberately does not do:
//!
//! * It does not read the task's prose. A goal is not parsed, and no name is compared with another
//!   for similarity: a pattern is selected only through a *fact* (a file the task names holds an
//!   observation; a symbol the task names is the subject of one; a graph `Implements` edge names the
//!   family; the task names the family; a test observation mentions a task symbol).
//! * It does not rank by frequency. A pattern's score is the strength of the signal that selected it.
//! * It does not use a stale report. A report that does not match the source yields `Stale` and
//!   nothing else.
//! * It does not invent relevance. A task that names nothing, or nothing known, yields
//!   `InsufficientEvidence`; a task whose references match no pattern yields `NoApplicablePattern`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::model::{ArchitectureGraph, GraphEdgeKind};
use crate::patterns::{
    Classification, Observation, Pattern, PatternCategory, PatternReport, Staleness,
};

pub const SELECTION_SCHEMA: &str = "chip.pattern-selection.v0";

/// What the caller says the task touches. Nothing here is inferred from the goal's wording.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternTask {
    pub id: String,
    /// Repository-relative paths of files the task reads or changes.
    #[serde(default)]
    pub files: Vec<String>,
    /// Graph symbol node ids (`symbol:<file>:<qualified name>`).
    #[serde(default)]
    pub symbols: Vec<String>,
    /// Implementation families (a trait name, `error_type`, `test_shape`, `call_order`) named outright.
    #[serde(default)]
    pub families: Vec<String>,
    /// Restrict to these categories; empty means all.
    #[serde(default)]
    pub categories: Vec<PatternCategory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectLimits {
    pub max_patterns: usize,
    pub max_examples: usize,
    /// The rendered block, in bytes.
    pub max_rendered_bytes: usize,
}

impl Default for SelectLimits {
    fn default() -> Self {
        SelectLimits {
            max_patterns: 5,
            max_examples: 3,
            max_rendered_bytes: 4096,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionStatus {
    Selected,
    /// The task's references are valid and analyzed, and no pattern bears on them.
    NoApplicablePattern,
    /// The task names nothing the report can speak to.
    InsufficientEvidence,
    /// The report does not match the source: nothing is offered.
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    /// `file`, `subject`, `implements`, `family` or `verification`.
    pub signal: String,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Example {
    pub observation: String,
    pub name: String,
    pub file: String,
    pub line: usize,
    pub signature: Vec<String>,
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shape {
    pub pattern_id: String,
    pub signature: Vec<String>,
    pub support: usize,
    pub classification: Classification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedPattern {
    pub pattern_id: String,
    pub category: PatternCategory,
    pub family: String,
    pub signature: Vec<String>,
    pub classification: Classification,
    /// Always `false`.
    pub enforced: bool,
    pub score: u32,
    pub reasons: Vec<Reason>,
    /// The observations of this pattern that are in the task's own files or symbols.
    pub in_task: Vec<Example>,
    pub supporting: Vec<Example>,
    pub counterexamples: Vec<Example>,
    pub support_count: usize,
    pub counterexample_count: usize,
    /// Every shape the family has in the report, so a reader sees the most frequent one beside this.
    pub family_shapes: Vec<Shape>,
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub schema: String,
    pub task: String,
    pub status: SelectionStatus,
    pub explanation: String,
    pub report_id: String,
    pub graph_snapshot: String,
    pub coverage_complete: bool,
    /// What limits how far this selection can be trusted.
    pub warnings: Vec<String>,
    /// References the task made that the graph or report does not know.
    pub unknown_references: Vec<String>,
    /// Files a stale report is stale about.
    pub stale_files: Vec<String>,
    pub patterns: Vec<SelectedPattern>,
    pub omitted_patterns: usize,
}

fn example(o: &Observation) -> Example {
    Example {
        observation: o.id.clone(),
        name: o.name.clone(),
        file: o.file.clone(),
        line: o.line,
        signature: o.signature.clone(),
        flags: o.flags.clone(),
    }
}

/// The last `::`-separated segment of a symbol id's qualified name.
fn symbol_tail(id: &str) -> Option<&str> {
    let rest = id.strip_prefix("symbol:")?;
    let (_, qualified) = rest.rsplit_once(':')?;
    // `file:qualified` splits at the last colon of the id; a `::` inside the name is kept whole.
    qualified.rsplit("::").next()
}

fn blank(task: &PatternTask, report: &PatternReport, graph: &ArchitectureGraph) -> Selection {
    Selection {
        schema: SELECTION_SCHEMA.to_string(),
        task: task.id.clone(),
        status: SelectionStatus::InsufficientEvidence,
        explanation: String::new(),
        report_id: report.report_id.clone(),
        graph_snapshot: graph.snapshot_id.to_string(),
        coverage_complete: report.coverage.complete,
        warnings: Vec::new(),
        unknown_references: Vec::new(),
        stale_files: Vec::new(),
        patterns: Vec::new(),
        omitted_patterns: 0,
    }
}

/// Selects the patterns relevant to `task`. Pure: the same task, report, graph and staleness give the
/// same selection, whatever order the task lists its references in.
pub fn select_patterns(
    task: &PatternTask,
    report: &PatternReport,
    staleness: &Staleness,
    graph: &ArchitectureGraph,
    limits: &SelectLimits,
) -> Selection {
    let mut out = blank(task, report, graph);

    if !staleness.is_current() {
        out.status = SelectionStatus::Stale;
        out.stale_files = staleness
            .changed
            .iter()
            .chain(&staleness.removed)
            .chain(&staleness.added)
            .cloned()
            .collect();
        out.explanation = "the pattern report does not match the source on disk, so none of it is offered as current; rebuild it with `stack-agent init`".to_string();
        return out;
    }
    if report.graph_snapshot != graph.snapshot_id {
        out.status = SelectionStatus::Stale;
        out.explanation =
            "the pattern report was derived from a different graph snapshot than the graph given"
                .to_string();
        return out;
    }

    // What the task names, and what the report and graph know of it.
    let nodes: BTreeSet<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
    let files: BTreeSet<&str> = task.files.iter().map(String::as_str).collect();
    let symbols: BTreeSet<&str> = task.symbols.iter().map(String::as_str).collect();
    let families: BTreeSet<&str> = task.families.iter().map(String::as_str).collect();
    let mut usable_references = 0usize;

    for f in &files {
        if report.sources.contains_key(*f) {
            usable_references += 1;
        } else if let Some(s) = report.coverage.skipped.iter().find(|s| s.path == *f) {
            out.warnings
                .push(format!("{f} was not analyzed: {}", s.reason));
        } else if report.coverage.parse_failures.iter().any(|p| p == f) {
            out.warnings
                .push(format!("{f} did not parse; nothing in it was observed"));
        } else {
            out.unknown_references.push(format!("file {f}"));
        }
    }
    for s in &symbols {
        if nodes.contains(*s) {
            usable_references += 1;
        } else {
            out.unknown_references.push(format!("symbol {s}"));
        }
    }
    usable_references += families.len();
    if !report.coverage.complete {
        out.warnings.push(
            "analysis coverage is partial; absence of a pattern here is not evidence of absence"
                .to_string(),
        );
    }
    if usable_references == 0 {
        out.status = SelectionStatus::InsufficientEvidence;
        out.explanation = if files.is_empty() && symbols.is_empty() && families.is_empty() {
            "the task names no file, symbol or family, and its wording is not interpreted"
                .to_string()
        } else {
            "nothing the task names is known to the graph or was analyzed".to_string()
        };
        return out;
    }

    // Families implied by the graph: the traits that symbols in the task's files, or the task's own
    // symbols, are recorded as implementing.
    let in_task_symbol = |id: &str| -> bool {
        symbols.contains(id)
            || id
                .strip_prefix("symbol:")
                .and_then(|r| r.rsplit_once(':'))
                .is_some_and(|(file, _)| files.contains(file))
    };
    let mut implied: BTreeMap<String, String> = BTreeMap::new();
    for e in graph
        .edges
        .iter()
        .filter(|e| e.kind == GraphEdgeKind::Implements)
    {
        if in_task_symbol(&e.from) {
            if let Some(name) = symbol_tail(&e.to) {
                implied
                    .entry(name.to_string())
                    .or_insert_with(|| format!("{} implements {}", e.from, e.to));
            }
        }
    }

    let by_id: BTreeMap<&str, &Observation> = report
        .observations
        .iter()
        .map(|o| (o.id.as_str(), o))
        .collect();
    let verification_hit = |o: &Observation| -> bool {
        o.category == PatternCategory::TestShape
            && (o.mentions.iter().any(|m| symbols.contains(m.as_str()))
                || files.contains(o.file.as_str()))
    };

    struct Hit<'a> {
        score: u32,
        reasons: Vec<Reason>,
        in_task: Vec<&'a Observation>,
    }
    let mut hits: BTreeMap<&str, Hit<'_>> = BTreeMap::new();
    let wanted = |c: PatternCategory| task.categories.is_empty() || task.categories.contains(&c);

    for p in report.patterns.iter().filter(|p| wanted(p.category)) {
        let mut hit = Hit {
            score: 0,
            reasons: Vec::new(),
            in_task: Vec::new(),
        };
        let note = |score: u32, signal: &str, evidence: String, hit: &mut Hit<'_>| {
            hit.score = hit.score.max(score);
            if !hit
                .reasons
                .iter()
                .any(|r| r.signal == signal && r.evidence == evidence)
            {
                hit.reasons.push(Reason {
                    signal: signal.to_string(),
                    evidence,
                });
            }
        };
        for id in &p.support {
            let Some(o) = by_id.get(id.as_str()) else {
                continue;
            };
            if files.contains(o.file.as_str()) {
                note(
                    100,
                    "file",
                    format!("{} holds observation {} ({})", o.file, o.id, o.name),
                    &mut hit,
                );
                hit.in_task.push(o);
            } else if symbols.contains(o.subject.as_str()) {
                note(
                    100,
                    "subject",
                    format!(
                        "{} is the subject of observation {} ({})",
                        o.subject, o.id, o.name
                    ),
                    &mut hit,
                );
                hit.in_task.push(o);
            } else if verification_hit(o) {
                note(
                    40,
                    "verification",
                    format!("test {} mentions a symbol the task names", o.name),
                    &mut hit,
                );
                hit.in_task.push(o);
            }
        }
        if families.contains(p.family.as_str()) {
            note(
                60,
                "family",
                format!("the task names family {}", p.family),
                &mut hit,
            );
        }
        if let Some(why) = implied.get(&p.family) {
            note(50, "implements", why.clone(), &mut hit);
        }
        if hit.score > 0 {
            hits.insert(p.id.as_str(), hit);
        }
    }

    // Shapes per family, for the side-by-side view.
    let mut shapes: BTreeMap<(PatternCategory, &str), Vec<&Pattern>> = BTreeMap::new();
    for p in &report.patterns {
        shapes
            .entry((p.category, p.family.as_str()))
            .or_default()
            .push(p);
    }

    let mut ranked: Vec<(&Pattern, Hit<'_>)> = hits
        .into_iter()
        .filter_map(|(id, h)| report.patterns.iter().find(|p| p.id == id).map(|p| (p, h)))
        .collect();
    ranked.sort_by(|a, b| b.1.score.cmp(&a.1.score).then_with(|| a.0.id.cmp(&b.0.id)));
    out.omitted_patterns = ranked.len().saturating_sub(limits.max_patterns);
    ranked.truncate(limits.max_patterns);

    for (p, mut h) in ranked {
        h.in_task.sort_by(|a, b| a.id.cmp(&b.id));
        h.in_task.dedup_by(|a, b| a.id == b.id);
        h.reasons.truncate(4);
        let in_task_ids: BTreeSet<&str> = h.in_task.iter().map(|o| o.id.as_str()).collect();
        let pick = |ids: &[String], prefer_task: bool| -> Vec<Example> {
            let mut obs: Vec<&Observation> = ids
                .iter()
                .filter_map(|i| by_id.get(i.as_str()).copied())
                .collect();
            obs.sort_by(|a, b| {
                let ka = prefer_task
                    && (in_task_ids.contains(a.id.as_str()) || files.contains(a.file.as_str()));
                let kb = prefer_task
                    && (in_task_ids.contains(b.id.as_str()) || files.contains(b.file.as_str()));
                kb.cmp(&ka).then_with(|| a.id.cmp(&b.id))
            });
            obs.into_iter()
                .take(limits.max_examples)
                .map(example)
                .collect()
        };
        let family_shapes = shapes[&(p.category, p.family.as_str())]
            .iter()
            .take(8)
            .map(|q| Shape {
                pattern_id: q.id.clone(),
                signature: q.signature.clone(),
                support: q.confidence.support,
                classification: q.classification,
            })
            .collect();
        out.patterns.push(SelectedPattern {
            pattern_id: p.id.clone(),
            category: p.category,
            family: p.family.clone(),
            signature: p.signature.clone(),
            classification: p.classification,
            enforced: false,
            score: h.score,
            reasons: h.reasons,
            in_task: h
                .in_task
                .iter()
                .take(limits.max_examples)
                .map(|o| example(o))
                .collect(),
            supporting: pick(&p.support, true),
            counterexamples: pick(&p.counterexamples, true),
            support_count: p.confidence.support,
            counterexample_count: p.confidence.counterexamples,
            family_shapes,
            caveats: p.caveats.clone(),
        });
    }

    if out.patterns.is_empty() {
        out.status = SelectionStatus::NoApplicablePattern;
        out.explanation = "the files, symbols and families the task names are known and analyzed, and no observed pattern bears on them; this is not a reason to create one".to_string();
    } else {
        out.status = SelectionStatus::Selected;
        out.explanation = format!(
            "{} pattern(s) selected through facts about the files, symbols and families the task names; frequency did not rank them",
            out.patterns.len()
        );
    }
    out
}

fn sig(s: &[String]) -> String {
    if s.is_empty() {
        "(no methods)".to_string()
    } else {
        s.join(", ")
    }
}

/// The selection as a block of text a person or a model can read, within `limits.max_rendered_bytes`.
/// It always says that nothing in it is a requirement.
///
/// A budget of 2000 bytes or less (a goal channel's ceiling) is rendered compactly: one short entry
/// per pattern, the family's shapes said once, so that what survives a small budget is every
/// selected pattern in brief and never just the first few in full.
pub fn render_selection(sel: &Selection, limits: &SelectLimits) -> String {
    let compact = limits.max_rendered_bytes <= 2000;
    let mut t = String::new();
    t.push_str(if compact {
        "Observed patterns in this repository (context, NOT requirements; frequency is not correctness; a task may depart from them):\n"
    } else {
        "Observed implementation patterns for this task (read-only context). They describe how this repository's code is written. They are NOT requirements, and frequency is not correctness: following one is optional, and a task may legitimately depart from all of them.\n"
    });
    match sel.status {
        SelectionStatus::Stale => {
            t.push_str(&format!(
                "Pattern evidence is out of date ({} file(s) differ) and is not offered.\n",
                sel.stale_files.len()
            ));
            return t;
        }
        SelectionStatus::InsufficientEvidence => {
            t.push_str(&format!("Insufficient evidence: {}.\n", sel.explanation));
            return t;
        }
        SelectionStatus::NoApplicablePattern => {
            t.push_str("No applicable observed pattern was found for the files and symbols named. That is not an instruction to create one.\n");
            if !compact {
                for w in &sel.warnings {
                    t.push_str(&format!("Limit: {w}.\n"));
                }
            }
            return t;
        }
        SelectionStatus::Selected => {}
    }
    let kind = |c: Classification| {
        if c == Classification::CandidateConvention {
            "candidate convention"
        } else {
            "observed"
        }
    };
    let mut shown_family: BTreeSet<(String, String)> = BTreeSet::new();
    let mut omitted = 0usize;
    for p in &sel.patterns {
        let block = if compact {
            let mut b = format!(
                "- {} `{}` [{}] shape: {}; seen {}x",
                format!("{:?}", p.category).to_lowercase(),
                p.family,
                kind(p.classification),
                sig(&p.signature),
                p.support_count,
            );
            let key = (format!("{:?}", p.category), p.family.clone());
            if p.family_shapes.len() > 1 && shown_family.insert(key) {
                let shapes: Vec<String> = p
                    .family_shapes
                    .iter()
                    .take(4)
                    .map(|s| format!("[{}] x{}", sig(&s.signature), s.support))
                    .collect();
                b.push_str(&format!("; shapes in this family: {}", shapes.join(", ")));
            }
            if let Some(e) = p.supporting.first() {
                b.push_str(&format!("; e.g. {} ({}:{})", e.name, e.file, e.line));
            }
            if let Some(e) = p.counterexamples.first() {
                b.push_str(&format!(
                    "; different: {} [{}] ({}:{})",
                    e.name,
                    sig(&e.signature),
                    e.file,
                    e.line
                ));
            }
            if let Some(r) = p.reasons.first() {
                b.push_str(&format!("; selected by {}", r.signal));
            }
            b.push_str(".\n");
            b
        } else {
            let mut block = format!(
                "\n- {} `{}` [{}; shape: {}] seen {} time(s); {} observation(s) of other shapes in the same family.\n",
                format!("{:?}", p.category).to_lowercase(),
                p.family,
                kind(p.classification),
                sig(&p.signature),
                p.support_count,
                p.counterexample_count,
            );
            for r in &p.reasons {
                block.push_str(&format!("  why: {} ({})\n", r.signal, r.evidence));
            }
            if p.family_shapes.len() > 1 {
                let shapes: Vec<String> = p
                    .family_shapes
                    .iter()
                    .map(|s| format!("[{}] x{}", sig(&s.signature), s.support))
                    .collect();
                block.push_str(&format!("  shapes in this family: {}\n", shapes.join("; ")));
            }
            for e in &p.supporting {
                block.push_str(&format!("  example: {} ({}:{})\n", e.name, e.file, e.line));
            }
            for e in &p.counterexamples {
                let legacy = if e.flags.iter().any(|f| f == "legacy") {
                    ", deprecated"
                } else {
                    ""
                };
                block.push_str(&format!(
                    "  different shape: {} [{}] ({}:{}{})\n",
                    e.name,
                    sig(&e.signature),
                    e.file,
                    e.line,
                    legacy
                ));
            }
            if let Some(c) = p.caveats.first() {
                block.push_str(&format!("  caveat: {c}\n"));
            }
            block
        };
        if t.len() + block.len() > limits.max_rendered_bytes {
            omitted += 1;
            continue;
        }
        t.push_str(&block);
    }
    if omitted > 0 {
        let note = format!("({omitted} more pattern(s) omitted to stay within the size limit)\n");
        if t.len() + note.len() <= limits.max_rendered_bytes {
            t.push_str(&note);
        }
    }
    if !compact {
        for w in &sel.warnings {
            let line = format!("Limit: {w}.\n");
            if t.len() + line.len() <= limits.max_rendered_bytes {
                t.push_str(&line);
            }
        }
    }
    t
}
