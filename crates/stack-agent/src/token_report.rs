//! `stack-agent token-report`: explains where a finished run's tokens went, from its work report
//! alone. No model is called and nothing is sent anywhere.
//!
//! It reads a `work --json` report (schema `stack-agent.work-report.v1`). A report written with the
//! per-call ledger has a `token_usage` section ([`crate::token_usage::SCHEMA`]) and is analysed from
//! it; an older report is analysed from `context.calls`, which every report since per-call context
//! measurement has carried. What an older report does not hold (timings, cache writes, what each
//! call decided, tool output sizes) is listed as unavailable: it is never reconstructed, and no
//! token count is ever estimated from bytes.
//!
//! Derived figures say what they are derived from. The one bound this report computes,
//! `carried_input_upper_bound`, is only computed when the recorded counts show every request
//! extended the one before it (no observation omitted or condensed, the observation count never
//! falling); it is an upper bound on input that repeated earlier context, not a saving.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::token_usage::{InputSemantics, SCHEMA as TOKEN_USAGE_SCHEMA};

pub const REPORT_SCHEMA: &str = "stack-agent.token-report.v1";

const USAGE: &str = "\
usage: stack-agent token-report REPORT.json [--json] [--label KEY=VALUE]...

Explains where a finished run's tokens went, from its `work --json` report. No model call.
  --json             print the analysis as JSON (schema stack-agent.token-report.v1)
  --label KEY=VALUE  attach a label (course, obstacle, ...) to the analysis; repeatable
Reports written before per-call token accounting are analysed from context.calls, and what they
do not record is listed as unavailable rather than reconstructed.
";

/// One call, as far as the report records it.
#[derive(Debug, Clone, Default)]
struct Row {
    call: usize,
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    total_input: Option<u64>,
    request_bytes: Option<u64>,
    messages: Option<u64>,
    observations: Option<u64>,
    omitted: Option<u64>,
    condensed: Option<u64>,
    elapsed_ms: Option<f64>,
    resent_bytes: Option<u64>,
    decision: Option<String>,
    succeeded: Option<bool>,
    /// The context profile the request was assembled under (`adaptive-v1` reports only).
    profile: Option<String>,
}

fn u(v: &Value) -> Option<u64> {
    v.as_u64()
}

fn rows_from_context(report: &Value, semantics: InputSemantics) -> Option<Vec<Row>> {
    let calls = report.pointer("/context/calls")?.as_array()?;
    Some(
        calls
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let usage = &c["reported_usage"];
                let input = u(&c["reported_input_tokens"]).or_else(|| u(&usage["input_tokens"]));
                let raw = chip_inference::Usage {
                    prompt_tokens: input,
                    completion_tokens: u(&c["reported_output_tokens"])
                        .or_else(|| u(&usage["output_tokens"])),
                    cache_read_tokens: u(&usage["cache_read_tokens"]),
                    cache_write_tokens: u(&usage["cache_write_tokens"]),
                    ..Default::default()
                };
                let a = crate::token_usage::account(&raw, semantics);
                Row {
                    call: u(&c["call"]).map_or(i + 1, |n| n as usize),
                    input,
                    output: raw.completion_tokens,
                    cache_read: raw.cache_read_tokens,
                    cache_write: raw.cache_write_tokens,
                    total_input: a.total_input,
                    request_bytes: u(&c["request_bytes"]),
                    messages: u(&c["messages"]),
                    observations: u(&c["observations"]).or_else(|| u(&c["capability_results"])),
                    omitted: u(&c["omitted_observations"]),
                    condensed: u(&c["condensed_observations"]),
                    succeeded: c["succeeded"].as_bool(),
                    ..Row::default()
                }
            })
            .collect(),
    )
}

/// Merges the ledger's per-call facts into the context rows (same order), or builds rows from the
/// ledger alone.
fn merge_ledger(rows: Option<Vec<Row>>, ledger: &[Value]) -> Vec<Row> {
    let mut rows = rows.unwrap_or_default();
    if rows.len() != ledger.len() {
        rows = ledger
            .iter()
            .enumerate()
            .map(|(i, _)| Row {
                call: i + 1,
                ..Row::default()
            })
            .collect();
    }
    for (row, c) in rows.iter_mut().zip(ledger) {
        let usage = &c["usage"];
        let reported = &usage["reported"];
        row.input = u(&reported["input_tokens"]).or(row.input);
        row.output = u(&reported["output_tokens"]).or(row.output);
        row.cache_read = u(&reported["cache_read_tokens"]).or(row.cache_read);
        row.cache_write = u(&reported["cache_write_tokens"]).or(row.cache_write);
        row.total_input = u(&usage["total_input_tokens"]).or(row.total_input);
        row.request_bytes = u(&c["request"]["content_bytes"]).or(row.request_bytes);
        row.messages = u(&c["request"]["messages"]).or(row.messages);
        row.elapsed_ms = c["elapsed_ms"].as_f64();
        row.resent_bytes = u(&c["request"]["resent_from_previous_call"]["bytes"]);
        row.decision = c["decision"].as_str().map(str::to_owned);
        row.profile = c["context"]["profile"].as_str().map(str::to_owned);
        row.succeeded = c["outcome"].as_str().map(|o| o == "response");
    }
    rows
}

fn median(values: &mut [u64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let n = values.len();
    Some(if n % 2 == 1 {
        values[n / 2] as f64
    } else {
        (values[n / 2 - 1] + values[n / 2]) as f64 / 2.0
    })
}

fn sum_known(rows: &[Row], f: impl Fn(&Row) -> Option<u64>) -> Value {
    let known: Vec<u64> = rows.iter().filter_map(&f).collect();
    json!({
        "sum": (!known.is_empty()).then(|| known.iter().sum::<u64>()),
        "calls_known": known.len(),
        "calls_unknown": rows.len() - known.len(),
    })
}

/// Did every request extend the one before it, as far as the counts show?
fn append_only(rows: &[Row]) -> Option<bool> {
    let mut previous: Option<u64> = None;
    for r in rows {
        let (Some(obs), Some(omitted), Some(condensed)) = (r.observations, r.omitted, r.condensed)
        else {
            return None;
        };
        if omitted > 0 || condensed > 0 || previous.is_some_and(|p| obs < p) {
            return Some(false);
        }
        previous = Some(obs);
    }
    Some(true)
}

/// Analyses one work report. `labels` are attached as given.
pub fn analyze(report: &Value, labels: &BTreeMap<String, String>) -> Value {
    let provider = report["provider"].as_str().unwrap_or("unknown");
    let semantics = InputSemantics::of_provider(provider);
    let ledger = report
        .get("token_usage")
        .filter(|t| t["schema"] == TOKEN_USAGE_SCHEMA)
        .and_then(|t| t["calls"].as_array());
    let context_rows = rows_from_context(report, semantics);
    let source = match (&ledger, &context_rows) {
        (Some(_), _) => "token_usage",
        (None, Some(_)) => "context.calls",
        (None, None) => "usage totals only",
    };
    let rows = match ledger {
        Some(l) => merge_ledger(context_rows, l),
        None => context_rows.unwrap_or_default(),
    };

    let mut warnings: Vec<String> = Vec::new();
    let mut unavailable: Vec<&str> = Vec::new();
    if report["schema"] != "stack-agent.work-report.v1" {
        warnings.push(format!(
            "the input's schema is {}, not stack-agent.work-report.v1; fields are read where present",
            report["schema"]
        ));
    }
    if ledger.is_none() {
        unavailable.extend([
            "per-call timing (elapsed, gaps between calls)",
            "content resent between consecutive calls (measured)",
            "the decision each call produced",
            "tool output sizes and repeated or overlapping outputs",
            "local serialized request size",
        ]);
        warnings.push(
            "this report predates per-call token accounting (no token_usage section); it is analysed from context.calls, and what that does not record is listed under unavailable".into(),
        );
    }
    if provider == chip_inference::PROVIDER_ANTHROPIC
        && !rows.is_empty()
        && rows.iter().all(|r| r.cache_write.is_none())
    {
        warnings.push(
            "every call's cache_write_tokens is null: builds before per-call token accounting did not read Anthropic's cache_creation_input_tokens, so cache writes are unknown and total input (input + cache read + cache write) is not derived".into(),
        );
        unavailable.push("total input tokens including cache writes");
    }
    if rows.is_empty() {
        warnings.push("the report has no per-call records; only its usage totals are shown".into());
    }
    let unreported = rows.iter().filter(|r| r.input.is_none()).count();
    if unreported > 0 {
        warnings.push(format!(
            "{unreported} of {} calls have no reported input tokens (unknown, not 0)",
            rows.len()
        ));
    }
    // The report's own totals, cross-checked against the per-call sums.
    let usage = &report["usage"];
    let row_input: u64 = rows.iter().filter_map(|r| r.input).sum();
    if let Some(total) = u(&usage["input_tokens"])
        && !rows.is_empty()
        && total != row_input
    {
        warnings.push(format!(
            "usage.input_tokens ({total}) differs from the sum over calls ({row_input})"
        ));
    }

    let mut inputs: Vec<u64> = rows.iter().filter_map(|r| r.input).collect();
    let mean =
        (!inputs.is_empty()).then(|| inputs.iter().sum::<u64>() as f64 / inputs.len() as f64);
    let med = median(&mut inputs);
    let mut largest: Vec<&Row> = rows.iter().filter(|r| r.input.is_some()).collect();
    largest.sort_by(|a, b| b.input.cmp(&a.input).then(a.call.cmp(&b.call)));
    let mut cumulative = 0u64;
    let series: Vec<Value> = rows
        .iter()
        .map(|r| {
            if let Some(i) = r.input {
                cumulative += i;
            }
            json!({
                "call": r.call,
                "input_tokens": r.input,
                "cumulative_input_tokens": r.input.map(|_| cumulative),
                "output_tokens": r.output,
                "cache_read_tokens": r.cache_read,
                "cache_write_tokens": r.cache_write,
                "request_bytes": r.request_bytes,
                "messages": r.messages,
                "observations": r.observations,
                "elapsed_ms": r.elapsed_ms,
                "resent_bytes": r.resent_bytes,
                "decision": r.decision,
                "context_profile": r.profile,
            })
        })
        .collect();
    // Calls and provider-reported tokens per context profile, summed as the totals are. Which
    // profile a call used is the runtime's record; nothing here says what another profile would
    // have cost.
    let mut by_profile: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    for r in &rows {
        if let Some(p) = &r.profile {
            by_profile.entry(p.clone()).or_default().push(r.clone());
        }
    }
    let context_profiles = json!({
        "setting": report.pointer("/tuning/context_profile").cloned(),
        "by_profile": by_profile.iter().map(|(p, rs)| (p.clone(), json!({
            "calls": rs.len(),
            "input_tokens": sum_known(rs, |r| r.input),
            "cache_read_tokens": sum_known(rs, |r| r.cache_read),
            "cache_write_tokens": sum_known(rs, |r| r.cache_write),
            "total_input_tokens": sum_known(rs, |r| r.total_input),
            "output_tokens": sum_known(rs, |r| r.output),
        }))).collect::<serde_json::Map<_, _>>(),
        "changes": report.pointer("/token_usage/context_profiles/changes").cloned(),
        "note": "per-profile sums are what those calls reported; a comparison between profiles needs a controlled run, not this split",
    });

    // Growth between consecutive calls, from the recorded counts.
    let steps: Vec<(usize, i64, i64, i64, usize)> = rows
        .windows(2)
        .enumerate()
        .filter_map(|(k, w)| {
            let (a, b) = (&w[0], &w[1]);
            Some((
                b.call,
                b.input? as i64 - a.input? as i64,
                b.request_bytes.unwrap_or(0) as i64 - a.request_bytes.unwrap_or(0) as i64,
                b.observations.unwrap_or(0) as i64 - a.observations.unwrap_or(0) as i64,
                rows.len() - (k + 1),
            ))
        })
        .collect();
    let mut largest_steps = steps.clone();
    largest_steps.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let append = append_only(&rows);
    // Each call after the first repeats at most the smaller of its own and its predecessor's input.
    let carried_upper: Option<u64> = (append == Some(true) && rows.len() > 1)
        .then(|| {
            rows.windows(2)
                .map(|w| Some(w[0].input?.min(w[1].input?)))
                .sum::<Option<u64>>()
        })
        .flatten();
    let total_input_reported = (!inputs.is_empty()).then(|| inputs.iter().sum::<u64>());
    let cache_read_sum: Option<u64> = {
        let known: Vec<u64> = rows.iter().filter_map(|r| r.cache_read).collect();
        (!known.is_empty()).then(|| known.iter().sum())
    };
    let first = rows.first();
    let last = rows.last();
    let bytes_known: Vec<(u64, u64)> = rows
        .iter()
        .filter_map(|r| Some((r.request_bytes?, r.input?)))
        .collect();
    let bytes_per_token = (!bytes_known.is_empty()).then(|| {
        let (b, t) = bytes_known
            .iter()
            .fold((0u64, 0u64), |(b, t), (x, y)| (b + x, t + y));
        if t == 0 {
            Value::Null
        } else {
            json!(((b as f64 / t as f64) * 100.0).round() / 100.0)
        }
    });

    let token_usage = report.get("token_usage");
    let observations = token_usage.and_then(|t| t.get("observations")).cloned();
    let measurement = &report["measurement"];
    let rejected_calls: Option<(usize, u64)> = ledger.map(|_| {
        rows.iter()
            .filter(|r| r.decision.as_deref() == Some("rejected"))
            .fold((0, 0), |(n, t), r| (n + 1, t + r.input.unwrap_or(0)))
    });
    // Calls after which the next call carried no more observations: the reply executed nothing
    // (rejected, or a decision without an execution). A recorded fact about the counts, not a
    // reading of the replies.
    let idle: Vec<&Row> = rows
        .windows(2)
        .filter(|w| {
            matches!((w[0].observations, w[1].observations), (Some(a), Some(b)) if b <= a)
                && w[0].omitted == Some(0)
                && w[1].omitted == Some(0)
        })
        .map(|w| &w[0])
        .collect();
    let idle_input: Option<u64> = idle.iter().map(|r| r.input).sum();
    let malformed =
        u(&report["malformed_replies"]).unwrap_or(0) + u(&report["invalid_decisions"]).unwrap_or(0);

    // Ranked opportunities. Impact is an upper bound in input tokens where the recorded counts
    // support one, and `null` where they do not; it is never a measured saving.
    let mut opportunities: Vec<Value> = Vec::new();
    let prompt_cache = report
        .pointer("/generation/prompt_cache/value")
        .and_then(Value::as_bool);
    if provider == chip_inference::PROVIDER_ANTHROPIC && rows.len() > 1 {
        let nothing_cached = cache_read_sum.is_none_or(|n| n == 0);
        if nothing_cached {
            opportunities.push(json!({
                "id": "provider-prompt-cache",
                "summary": "No call read from the provider's prompt cache although each request repeats its predecessor's context; mark the stable prefix cacheable (Anthropic cache_control) so repeated input can be served as cache reads",
                "impact_input_tokens_upper_bound": carried_upper,
                "impact_basis": if carried_upper.is_some() { "sum over calls of min(input of this call, input of the previous call); the counts show every request extended the previous one" } else { "not computed: the counts do not show append-only requests" },
                "confidence": if append == Some(true) { "medium" } else { "low" },
                "risk": "low",
                "evidence": {
                    "cache_read_tokens": cache_read_sum,
                    "prompt_cache_setting": prompt_cache,
                    "append_only_requests": append,
                },
                "note": "cache reads are still billed (at a reduced rate) and the first write of each prefix costs more than ordinary input; only a controlled run's reported cache_read and cache_write tokens can say what it saves",
            }));
        }
    }
    if malformed > 0 {
        opportunities.push(json!({
            "id": "rejected-replies",
            "summary": "Replies that were not a valid decision each cost a full-context call; fewer rejected replies means fewer resent contexts",
            "impact_input_tokens_upper_bound": rejected_calls.map(|(_, t)| t).or(idle_input.filter(|_| !idle.is_empty())),
            "impact_basis": if rejected_calls.is_some() { "input tokens of the calls whose decision was rejected" } else if !idle.is_empty() { "input tokens of the calls after which the next request carried no new observation (this report does not attribute decisions to calls)" } else { "not computed: this report does not attribute decisions to calls" },
            "confidence": if rejected_calls.is_some() { "high" } else { "medium" },
            "risk": "medium",
            "evidence": {
                "malformed_replies": report["malformed_replies"],
                "invalid_decisions": report["invalid_decisions"],
                "rejected_calls": rejected_calls.map(|(n, _)| n),
                "calls_followed_by_no_new_observation": idle.iter().map(|r| r.call).collect::<Vec<_>>(),
                "input_tokens_of_those_calls": idle_input.filter(|_| !idle.is_empty()),
            },
            "note": "a rejected reply is also recovery evidence; any change must keep the strict parser and validation",
        }));
    }
    let big: Vec<&(usize, i64, i64, i64, usize)> = largest_steps
        .iter()
        .filter(|s| {
            s.1 > 0
                && last
                    .and_then(|l| l.input)
                    .is_some_and(|l| s.1 as u64 * 20 >= l)
        })
        .take(3)
        .collect();
    if !big.is_empty() {
        let carried: u64 = big.iter().map(|s| s.1 as u64 * s.4 as u64).sum();
        opportunities.push(json!({
            "id": "large-observations",
            "summary": "A few steps added most of the context, and every later call resent it; targeted reads or bounded tool output would shrink every later request",
            "impact_input_tokens_upper_bound": carried,
            "impact_basis": "for the largest growth steps (each at least 5% of the last call's input): growth in input tokens x the number of later calls",
            "confidence": "low",
            "risk": "high",
            "evidence": big.iter().map(|s| json!({"call": s.0, "input_token_growth": s.1, "request_byte_growth": s.2, "observations_added": s.3, "later_calls": s.4})).collect::<Vec<_>>(),
            "note": "what part of an output the model needed is not known; dropping or cutting observations risks correctness and recovery, so this needs its own evidence",
        }));
    }
    let repeated = &report["context"]["observations"];
    if u(&repeated["repeated_identical"]).unwrap_or(0) > 0 {
        opportunities.push(json!({
            "id": "repeated-identical-observations",
            "summary": "The same request returned the same result more than once",
            "impact_input_tokens_upper_bound": Value::Null,
            "impact_basis": "bytes only (retained_repeat_bytes_sent); not converted to tokens",
            "confidence": "medium",
            "risk": "low",
            "evidence": repeated,
        }));
    }
    opportunities.sort_by(|a, b| {
        let impact = |v: &Value| v["impact_input_tokens_upper_bound"].as_u64();
        let conf = |v: &Value| match v["confidence"].as_str() {
            Some("high") => 0,
            Some("medium") => 1,
            _ => 2,
        };
        impact(b)
            .is_some()
            .cmp(&impact(a).is_some())
            .then(impact(b).cmp(&impact(a)))
            .then(conf(a).cmp(&conf(b)))
    });
    for (i, o) in opportunities.iter_mut().enumerate() {
        o["rank"] = json!(i + 1);
    }

    json!({
        "schema": REPORT_SCHEMA,
        "source": source,
        "labels": labels,
        "run": {
            "provider": provider,
            "model": report["model"],
            "terminal_state": report["terminal_state"],
            "goal_satisfied": report["goal_satisfied"],
            "verified": report["verified"],
            "build": report["build"],
            "work_labels": token_usage.map(|t| t["run"]["labels"].clone()),
            "generation_prompt_cache": report.pointer("/generation/prompt_cache").cloned(),
        },
        "accounting": {
            "input_semantics": semantics.name(),
            "rules": "input_tokens is the provider's own input field: for Anthropic it excludes cache reads and writes, for OpenAI-compatible providers it includes cached tokens; totals are sums over the calls that reported a field, with how many did not",
        },
        "totals": {
            "calls": rows.len(),
            "executions": measurement.get("executions").cloned().unwrap_or_else(|| report["capability_requests"].clone()),
            "input_tokens": sum_known(&rows, |r| r.input),
            "output_tokens": sum_known(&rows, |r| r.output),
            "cache_read_tokens": sum_known(&rows, |r| r.cache_read),
            "cache_write_tokens": sum_known(&rows, |r| r.cache_write),
            "total_input_tokens": sum_known(&rows, |r| r.total_input),
            "report_usage": usage,
        },
        "per_call": {
            "mean_input_tokens": mean.map(|m| (m * 10.0).round() / 10.0),
            "median_input_tokens": med,
            "largest_calls": largest.iter().take(5).map(|r| json!({
                "call": r.call, "input_tokens": r.input, "request_bytes": r.request_bytes, "messages": r.messages,
            })).collect::<Vec<_>>(),
            "series": series,
        },
        "context_growth": {
            "first_call": first.map(|r| json!({"input_tokens": r.input, "request_bytes": r.request_bytes, "messages": r.messages})),
            "last_call": last.map(|r| json!({"input_tokens": r.input, "request_bytes": r.request_bytes, "messages": r.messages})),
            "mean_input_growth_per_call": match (first.and_then(|r| r.input), last.and_then(|r| r.input)) {
                (Some(a), Some(b)) if rows.len() > 1 => json!(((b as f64 - a as f64) / (rows.len() - 1) as f64 * 10.0).round() / 10.0),
                _ => Value::Null,
            },
            "append_only_requests": append,
            "carried_input_upper_bound": carried_upper,
            "carried_input_share_upper_bound": match (carried_upper, total_input_reported) {
                (Some(c), Some(t)) if t > 0 => json!(((c as f64 / t as f64) * 1000.0).round() / 1000.0),
                _ => Value::Null,
            },
            "largest_growth_steps": largest_steps.iter().take(5).map(|s| json!({
                "call": s.0, "input_token_growth": s.1, "request_byte_growth": s.2, "observations_added": s.3, "later_calls": s.4,
            })).collect::<Vec<_>>(),
            "resent_bytes_measured": token_usage.map(|t| t["context"].clone()),
        },
        "context_profiles": context_profiles,
        "observations": observations,
        "repetition": report["context"]["observations"],
        "executions_by_capability": report["executions_by_capability"],
        "time": {
            "model_latency_ms": measurement["model_latency_ms"],
            "compute_latency_ms": measurement["compute_latency_ms"],
            "total_latency_ms": measurement["total_latency_ms"],
            "ledger": token_usage.map(|t| t["time"].clone()),
        },
        "estimated_vs_reported": {
            "provider_reported_input_tokens": total_input_reported,
            "request_bytes_per_reported_input_token": bytes_per_token,
            "usage_estimate": report.get("usage_estimate").cloned(),
            "note": "bytes per token is an observed ratio for this run, shown for orientation; nothing in this report is estimated from it",
        },
        "failures": {
            "provider_failures": report["context"]["provider_failures"],
            "model_failures": report["model_failures"],
            "malformed_replies": report["malformed_replies"],
            "invalid_decisions": report["invalid_decisions"],
            "recoveries": report["recoveries"],
            "rejected_calls": rejected_calls.map(|(n, t)| json!({"calls": n, "input_tokens": t})),
        },
        "data_quality": {
            "warnings": warnings,
            "unavailable": unavailable,
            "work_report_warnings": token_usage.map(|t| t["data_quality"].clone()),
        },
        "opportunities": opportunities,
    })
}

fn human(a: &Value) -> String {
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };
    let n = |v: &Value| v.as_u64().map_or("unknown".to_string(), |n| n.to_string());
    line(format!(
        "Token report ({}; source: {})",
        a["run"]["model"].as_str().unwrap_or("unknown model"),
        a["source"].as_str().unwrap_or("")
    ));
    let t = &a["totals"];
    line(format!(
        "  calls {}  executions {}",
        t["calls"],
        n(&t["executions"])
    ));
    for (name, key) in [
        ("input", "input_tokens"),
        ("output", "output_tokens"),
        ("cache read", "cache_read_tokens"),
        ("cache write", "cache_write_tokens"),
    ] {
        line(format!(
            "  {name:<12} {} ({} of {} calls reported)",
            n(&t[key]["sum"]),
            t[key]["calls_known"],
            t["calls"]
        ));
    }
    let p = &a["per_call"];
    line(format!(
        "  input per call: mean {}  median {}",
        p["mean_input_tokens"], p["median_input_tokens"]
    ));
    let g = &a["context_growth"];
    line(format!(
        "  growth: first call {} -> last call {} input tokens; append-only requests: {}; carried input (upper bound): {}",
        n(&g["first_call"]["input_tokens"]),
        n(&g["last_call"]["input_tokens"]),
        g["append_only_requests"],
        n(&g["carried_input_upper_bound"])
    ));
    if let Some(profiles) = a["context_profiles"]["by_profile"].as_object()
        && !profiles.is_empty()
    {
        let parts: Vec<String> = profiles
            .iter()
            .map(|(p, v)| {
                format!(
                    "{p} {} call(s), input {}",
                    v["calls"],
                    n(&v["input_tokens"]["sum"])
                )
            })
            .collect();
        line(format!("  context profiles: {}", parts.join("; ")));
    }
    if let Some(ops) = a["opportunities"].as_array() {
        line("Opportunities (ranked; impact is an upper bound, not a saving):".into());
        for o in ops {
            line(format!(
                "  {}. {}  impact {}  confidence {}  risk {}",
                o["rank"],
                o["id"].as_str().unwrap_or(""),
                n(&o["impact_input_tokens_upper_bound"]),
                o["confidence"].as_str().unwrap_or(""),
                o["risk"].as_str().unwrap_or("")
            ));
        }
    }
    if let Some(ws) = a["data_quality"]["warnings"].as_array() {
        for w in ws {
            line(format!("  warning: {}", w.as_str().unwrap_or("")));
        }
    }
    if let Some(us) = a["data_quality"]["unavailable"].as_array() {
        for u in us {
            line(format!("  unavailable: {}", u.as_str().unwrap_or("")));
        }
    }
    out
}

/// The `token-report` command. Exit 0 on success, 2 on a usage error, 3 when the report cannot be
/// read or is not JSON.
pub fn run(args: &[String]) -> i32 {
    let mut path: Option<&str> = None;
    let mut as_json = false;
    let mut labels = BTreeMap::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => as_json = true,
            "--help" | "-h" => {
                print!("{USAGE}");
                return 0;
            }
            "--label" => {
                i += 1;
                let Some(text) = args.get(i) else {
                    eprint!("{USAGE}");
                    return 2;
                };
                match crate::token_usage::parse_labels(text) {
                    Ok(more) => {
                        for (k, v) in more {
                            if labels.insert(k.clone(), v).is_some() {
                                eprintln!("error: run label `{k}` is given twice");
                                return 2;
                            }
                        }
                    }
                    Err(why) => {
                        eprintln!("error: {why}");
                        return 2;
                    }
                }
            }
            other if !other.starts_with('-') && path.is_none() => path = Some(other),
            _ => {
                eprint!("{USAGE}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(path) = path else {
        eprint!("{USAGE}");
        return 2;
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return 3;
        }
    };
    // A `work --json` run prints one report per line; the last line is the report.
    let report: Value = match serde_json::from_str(&text).or_else(|_| {
        serde_json::from_str(
            text.lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(""),
        )
    }) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {path} is not a JSON work report: {e}");
            return 3;
        }
    };
    let analysis = analyze(&report, &labels);
    if as_json {
        println!("{analysis}");
    } else {
        print!("{}", human(&analysis));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(n: u64, input: Option<u64>, bytes: u64, obs: u64) -> Value {
        json!({
            "call": n, "turn": n, "request_bytes": bytes, "messages": obs + 2,
            "observations": obs, "capability_results": obs,
            "omitted_observations": 0, "condensed_observations": 0,
            "reported_input_tokens": input, "reported_output_tokens": input.map(|_| 10),
            "reported_usage": input.map(|i| json!({"input_tokens": i, "output_tokens": 10, "cache_read_tokens": 0, "cache_write_tokens": null})),
            "succeeded": input.is_some(),
        })
    }

    fn historical(calls: Vec<Value>) -> Value {
        json!({
            "schema": "stack-agent.work-report.v1",
            "provider": "anthropic", "model": "m",
            "context": {"calls": calls, "observations": {"repeated_identical": 0}},
            "usage": {"input_tokens": 600, "output_tokens": 30},
            "malformed_replies": 1,
        })
    }

    #[test]
    fn a_historical_report_is_analysed_and_what_it_lacks_is_listed_not_invented() {
        let r = historical(vec![
            call(1, Some(100), 400, 0),
            call(2, Some(200), 800, 1),
            call(3, Some(300), 1200, 2),
        ]);
        let a = analyze(&r, &BTreeMap::new());
        assert_eq!(a["source"], "context.calls");
        assert_eq!(a["totals"]["input_tokens"]["sum"], 600);
        assert_eq!(a["totals"]["cache_write_tokens"]["sum"], Value::Null);
        assert_eq!(a["totals"]["total_input_tokens"]["calls_known"], 0);
        assert_eq!(a["per_call"]["median_input_tokens"], 200.0);
        assert_eq!(a["per_call"]["series"][2]["cumulative_input_tokens"], 600);
        assert_eq!(a["context_growth"]["append_only_requests"], true);
        // min(100,200) + min(200,300)
        assert_eq!(a["context_growth"]["carried_input_upper_bound"], 300);
        assert_eq!(a["per_call"]["series"][0]["elapsed_ms"], Value::Null);
        let unavailable = a["data_quality"]["unavailable"].to_string();
        assert!(unavailable.contains("per-call timing"), "{unavailable}");
        assert!(unavailable.contains("cache writes"), "{unavailable}");
        assert_eq!(a["opportunities"][0]["id"], "provider-prompt-cache");
        assert_eq!(
            a["opportunities"][0]["impact_input_tokens_upper_bound"],
            300
        );
        // Decisions are not attributed in a historical report and every call added an
        // observation, so the rejected-reply impact is not computed.
        let rejected = a["opportunities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["id"] == "rejected-replies")
            .unwrap();
        assert_eq!(rejected["impact_input_tokens_upper_bound"], Value::Null);
        // A call after which nothing new was observed is named, with its input.
        let r = historical(vec![
            call(1, Some(100), 400, 0),
            call(2, Some(150), 500, 0),
            call(3, Some(300), 1200, 1),
        ]);
        let a = analyze(&r, &BTreeMap::new());
        let rejected = a["opportunities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["id"] == "rejected-replies")
            .unwrap();
        assert_eq!(
            rejected["evidence"]["calls_followed_by_no_new_observation"],
            json!([1])
        );
        assert_eq!(rejected["impact_input_tokens_upper_bound"], 100);
    }

    #[test]
    fn incomplete_records_stay_incomplete() {
        let mut r = historical(vec![
            call(1, Some(100), 400, 0),
            call(2, None, 800, 1),
            call(3, Some(300), 1200, 2),
        ]);
        r["context"]["calls"][1]["omitted_observations"] = json!(1);
        let a = analyze(&r, &BTreeMap::new());
        assert_eq!(a["totals"]["input_tokens"]["sum"], 400);
        assert_eq!(a["totals"]["input_tokens"]["calls_unknown"], 1);
        assert_eq!(a["context_growth"]["append_only_requests"], false);
        assert_eq!(
            a["context_growth"]["carried_input_upper_bound"],
            Value::Null
        );
        let w = a["data_quality"]["warnings"].to_string();
        assert!(
            w.contains("1 of 3 calls have no reported input tokens"),
            "{w}"
        );
        assert!(w.contains("differs from the sum"), "{w}");
        // No per-call data at all.
        let bare = analyze(
            &json!({"schema": "stack-agent.work-report.v1", "provider": "anthropic", "usage": {"input_tokens": 5}}),
            &BTreeMap::new(),
        );
        assert_eq!(bare["source"], "usage totals only");
        assert_eq!(bare["totals"]["calls"], 0);
        assert_eq!(bare["opportunities"], json!([]));
    }

    #[test]
    fn openai_reports_never_suggest_anthropic_caching_and_cached_input_is_not_added() {
        let mut r = historical(vec![call(1, Some(100), 400, 0), call(2, Some(200), 800, 1)]);
        r["provider"] = json!("openai-compatible");
        r["context"]["calls"][1]["reported_usage"]["cache_read_tokens"] = json!(80);
        r["usage"]["input_tokens"] = json!(300);
        let a = analyze(&r, &BTreeMap::new());
        assert_eq!(a["totals"]["total_input_tokens"]["sum"], 300);
        assert_eq!(a["totals"]["cache_read_tokens"]["sum"], 80);
        assert!(
            !a["opportunities"]
                .to_string()
                .contains("provider-prompt-cache")
        );
    }
}
