//! OpenTelemetry export of a finished work (`chip-otel`), when the environment asks for it.
//!
//! The export is built from the work's durable record after the host's verdict is recorded, so it
//! says what FeltDB holds and nothing the record does not. It is a report, like learning: a
//! refused configuration or a failed delivery is said, and never changes the outcome or the exit
//! status. Export is configured with the standard `OTEL_*` variables (and `STACK_AGENT_OTLP_FILE`);
//! see `chip_otel::export` for what is accepted and refused, and docs/product/telemetry.md.

use chip_agent_state::{AgentStore, History};
use chip_otel::export::{Config, config_from_env, export};
use serde_json::{Value, json};

/// What the environment asks for: `Ok(None)` when export is off.
pub fn configured(env: &dyn Fn(&str) -> Option<String>) -> Result<Option<Config>, String> {
    config_from_env(env).map(|c| c.enabled().then_some(c))
}

/// The export context: the service, and the provider and model the work asked for.
pub fn context(config: &Config, provider: &str, model: &str) -> chip_otel::Context {
    chip_otel::Context {
        service_name: config
            .service_name
            .clone()
            .unwrap_or_else(|| "stack-agent".into()),
        service_version: env!("CARGO_PKG_VERSION").into(),
        provider: Some(provider.into()),
        request_model: Some(model.into()),
    }
}

/// Exports one task's record to every configured sink; the report's `telemetry` value.
pub fn export_history(config: &Config, history: &History, context: &chip_otel::Context) -> Value {
    let request = chip_otel::trace(history, context);
    let results = export(config, &request);
    let sinks: Vec<Value> = config
        .sinks
        .iter()
        .zip(&results)
        .map(|(sink, result)| {
            json!({
                "sink": sink.describe(),
                "exported": result.is_ok(),
                "error": result.as_ref().err(),
            })
        })
        .collect();
    json!({
        "configured": true,
        "exported": results.iter().all(Result::is_ok),
        "trace_id": chip_otel::trace_id(&history.task.task_id),
        "sinks": sinks,
    })
}

/// The `telemetry` report of a work: off, refused, not possible without a record, or exported.
/// `history` is the record as the work finished it, or `None` to read it from `store`.
pub fn report(
    env: &dyn Fn(&str) -> Option<String>,
    store: Option<&AgentStore>,
    task_id: &str,
    history: Option<&History>,
    provider: &str,
    model: &str,
) -> Value {
    let config = match configured(env) {
        Ok(None) => return json!({"configured": false}),
        Ok(Some(config)) => config,
        Err(why) => return json!({"configured": true, "exported": false, "error": why}),
    };
    let read;
    let history = match (history, store) {
        (Some(h), _) => h,
        (None, Some(store)) => match store.history(task_id) {
            Ok(Some(h)) => {
                read = h;
                &read
            }
            Ok(None) => return unrecorded("FeltDB holds no record of this work"),
            Err(why) => return unrecorded(&format!("its record cannot be read: {why}")),
        },
        (None, None) => {
            return unrecorded(
                "this work was not persisted in durable agent state, which export is built from",
            );
        }
    };
    export_history(&config, history, &context(&config, provider, model))
}

fn unrecorded(why: &str) -> Value {
    json!({"configured": true, "exported": false, "error": format!("not exported: {why}")})
}

/// One stderr line when export was asked for and did not happen.
pub fn report_failure(telemetry: &Value) {
    if telemetry["configured"] != true || telemetry["exported"] == true {
        return;
    }
    if let Some(why) = telemetry["error"].as_str() {
        eprintln!("note: telemetry export: {why}; the work's outcome is unchanged");
    }
    for sink in telemetry["sinks"].as_array().into_iter().flatten() {
        if sink["exported"] != true {
            eprintln!(
                "note: telemetry export to {} failed: {}; the work's outcome is unchanged",
                sink["sink"].as_str().unwrap_or_default(),
                sink["error"].as_str().unwrap_or_default()
            );
        }
    }
}
