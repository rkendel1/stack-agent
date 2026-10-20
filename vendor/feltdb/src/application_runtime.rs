use crate::{
    application::*,
    state_contract::{schema_from_revision, StateSchema},
    FeltDb,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

pub const APPLICATION_RUNTIME_CONTRACT_VERSION: u32 = 1;
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReadinessStatus {
    Ready,
    ReadyWithWarnings,
    Blocked,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessCheck {
    pub name: String,
    pub passed: bool,
    pub blocking: bool,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeReadiness {
    pub revision_id: String,
    pub environment: String,
    pub checks: Vec<ReadinessCheck>,
    pub status: ReadinessStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateContract {
    pub schema: StateSchema,
    pub collections: Vec<CollectionDefinition>,
    pub indexes: Vec<IndexDefinition>,
    pub references: Vec<ReferenceDefinition>,
    pub mutation_preconditions: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueryContract {
    pub definitions: Vec<QueryDefinition>,
    pub live_bindings: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionContract {
    pub definitions: Vec<ActionDefinition>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowContract {
    pub definitions: Vec<WorkflowDefinition>,
    pub revision_bound: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriggerContract {
    pub definitions: Vec<TriggerDefinition>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleContract {
    pub definitions: Vec<ScheduleDefinition>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentContract {
    pub definitions: Vec<AgentDefinition>,
    pub immutable_capability_snapshot: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityContract {
    pub bindings: Vec<CapabilityBinding>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedConnection {
    pub name: String,
    pub connection_id: String,
    pub secret_bindings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectionContract {
    pub bindings: Vec<ResolvedConnection>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyContract {
    pub definitions: Vec<PolicyDefinition>,
    pub required_layers: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiRuntimeContract {
    pub routes: Vec<RouteDefinition>,
    pub forms: Vec<FormDefinition>,
    pub dashboards: Vec<DashboardDefinition>,
    pub bindings: Vec<DataBinding>,
    pub actions: Vec<String>,
    pub navigation: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactContract {
    pub references: Vec<ArtifactReference>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentContract {
    pub name: String,
    pub configuration: BTreeMap<String, String>,
    pub state_namespace: String,
    pub isolated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeCompatibility {
    pub manifest_version: u32,
    pub runtime_contract_version: u32,
    pub engine_version: String,
    pub minimum_engine: String,
    pub maximum_engine_exclusive: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApplicationRuntimeContract {
    pub application_id: String,
    pub tenant_id: String,
    pub revision_id: String,
    pub revision_number: u64,
    pub metadata: ApplicationMetadata,
    pub state: StateContract,
    pub queries: QueryContract,
    pub actions: ActionContract,
    pub workflows: WorkflowContract,
    pub triggers: TriggerContract,
    pub schedules: ScheduleContract,
    pub agents: AgentContract,
    pub capabilities: CapabilityContract,
    pub connections: ConnectionContract,
    pub policies: PolicyContract,
    pub ui: UiRuntimeContract,
    pub artifacts: ArtifactContract,
    pub environment: EnvironmentContract,
    pub compatibility: RuntimeCompatibility,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeInventory {
    pub capabilities: BTreeSet<String>,
    pub connections: BTreeSet<String>,
    pub enforce_availability: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeIssue {
    pub path: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeValidation {
    pub valid: bool,
    pub contract_hash: Option<String>,
    pub issues: Vec<RuntimeIssue>,
}

fn semver_tuple(value: &str) -> Option<(u64, u64, u64)> {
    let mut p = value.split('.').map(|v| v.parse::<u64>().ok());
    Some((
        p.next()??,
        p.next().unwrap_or(Some(0))?,
        p.next().unwrap_or(Some(0))?,
    ))
}
fn compatible(requirement: &RuntimeRequirement) -> bool {
    if requirement.contract != APPLICATION_RUNTIME_CONTRACT_VERSION {
        return false;
    }
    let Some(engine) = semver_tuple(ENGINE_VERSION) else {
        return false;
    };
    if semver_tuple(&requirement.minimum_engine).is_some_and(|minimum| engine < minimum) {
        return false;
    }
    if requirement
        .maximum_engine_exclusive
        .as_deref()
        .and_then(semver_tuple)
        .is_some_and(|maximum| engine >= maximum)
    {
        return false;
    }
    true
}

pub fn resolve_runtime(
    revision: &ApplicationRevision,
    environment: &str,
    inventory: &RuntimeInventory,
) -> Result<ApplicationRuntimeContract, RuntimeValidation> {
    let mut issues = vec![];
    match manifest_hash(&revision.manifest) {
        Ok(hash) if hash == revision.manifest_hash => {}
        Ok(_) => issues.push(RuntimeIssue {
            path: "manifest_hash".into(),
            message: "revision integrity check failed".into(),
        }),
        Err(error) => issues.push(RuntimeIssue {
            path: "manifest".into(),
            message: error,
        }),
    }
    let validation = validate_manifest(
        &revision.manifest,
        &revision.tenant_id,
        &revision.application_id,
        None,
    );
    for issue in validation.issues {
        issues.push(RuntimeIssue {
            path: issue.path,
            message: issue.message,
        });
    }
    if !compatible(&revision.manifest.runtime) {
        issues.push(RuntimeIssue{path:"runtime".into(),message:format!("runtime requires contract {} and engine >= {} < {:?}; current contract is {} and engine is {}",revision.manifest.runtime.contract,revision.manifest.runtime.minimum_engine,revision.manifest.runtime.maximum_engine_exclusive,APPLICATION_RUNTIME_CONTRACT_VERSION,ENGINE_VERSION)});
    }
    let environment_definition_name = if environment.starts_with("preview/") {
        "preview"
    } else {
        environment
    };
    let environment_definition = revision
        .manifest
        .environments
        .iter()
        .find(|v| v.name == environment_definition_name)
        .cloned();
    if environment_definition.is_none() {
        issues.push(RuntimeIssue {
            path: "environment".into(),
            message: "environment is not declared by the revision".into(),
        });
    }
    if inventory.enforce_availability {
        for capability in &revision.manifest.capabilities {
            if capability.provider.is_some() && !inventory.capabilities.contains(&capability.name) {
                issues.push(RuntimeIssue {
                    path: format!("capabilities.{}", capability.name),
                    message: "required capability provider is unavailable".into(),
                });
            }
        }
        for connection in &revision.manifest.connections {
            if !inventory.connections.contains(&connection.connection_id) {
                issues.push(RuntimeIssue {
                    path: format!("connections.{}", connection.name),
                    message: "connection is unavailable or unauthorized".into(),
                });
            }
        }
    }
    if !issues.is_empty() {
        return Err(RuntimeValidation {
            valid: false,
            contract_hash: None,
            issues,
        });
    }
    let environment_definition = environment_definition.expect("checked above");
    let isolated = environment == "preview"
        || environment.starts_with("preview/")
        || environment == "development";
    let revision_key = revision
        .manifest_hash
        .strip_prefix("sha256:")
        .unwrap_or(&revision.manifest_hash)
        .chars()
        .take(12)
        .collect::<String>();
    let state_namespace = if isolated {
        format!(
            "{}:{}:{}:{revision_key}",
            revision.tenant_id, revision.application_id, environment
        )
    } else {
        format!(
            "{}:{}:{}",
            revision.tenant_id, revision.application_id, environment
        )
    };
    let mut actions = revision
        .manifest
        .actions
        .iter()
        .map(|v| v.name.clone())
        .collect::<Vec<_>>();
    actions.sort();
    let mut navigation = revision
        .manifest
        .ui
        .routes
        .iter()
        .map(|v| v.path.clone())
        .collect::<Vec<_>>();
    navigation.sort();
    Ok(ApplicationRuntimeContract {
        application_id: revision.application_id.clone(),
        tenant_id: revision.tenant_id.clone(),
        revision_id: revision.revision_id.clone(),
        revision_number: revision.revision_number,
        metadata: revision.manifest.metadata.clone(),
        state: StateContract {
            schema: schema_from_revision(revision),
            collections: revision.manifest.collections.clone(),
            indexes: revision.manifest.indexes.clone(),
            references: revision.manifest.references.clone(),
            mutation_preconditions: true,
        },
        queries: QueryContract {
            definitions: revision.manifest.queries.clone(),
            live_bindings: true,
        },
        actions: ActionContract {
            definitions: revision.manifest.actions.clone(),
        },
        workflows: WorkflowContract {
            definitions: revision.manifest.workflows.clone(),
            revision_bound: true,
        },
        triggers: TriggerContract {
            definitions: revision.manifest.triggers.clone(),
        },
        schedules: ScheduleContract {
            definitions: revision.manifest.schedules.clone(),
        },
        agents: AgentContract {
            definitions: revision.manifest.agents.clone(),
            immutable_capability_snapshot: true,
        },
        capabilities: CapabilityContract {
            bindings: revision.manifest.capabilities.clone(),
        },
        connections: ConnectionContract {
            bindings: revision
                .manifest
                .connections
                .iter()
                .map(|v| ResolvedConnection {
                    name: v.name.clone(),
                    connection_id: v.connection_id.clone(),
                    secret_bindings: v.secrets.iter().map(|s| s.binding.clone()).collect(),
                })
                .collect(),
        },
        policies: PolicyContract {
            definitions: revision.manifest.policies.clone(),
            required_layers: vec![
                "tenant".into(),
                "application".into(),
                format!("environment:{environment}"),
                "principal".into(),
                "workload_snapshot".into(),
            ],
        },
        ui: UiRuntimeContract {
            routes: revision.manifest.ui.routes.clone(),
            forms: revision.manifest.ui.forms.clone(),
            dashboards: revision.manifest.ui.dashboards.clone(),
            bindings: revision.manifest.ui.bindings.clone(),
            actions,
            navigation,
        },
        artifacts: ArtifactContract {
            references: revision.manifest.artifacts.clone(),
        },
        environment: EnvironmentContract {
            name: environment.into(),
            configuration: environment_definition.configuration,
            state_namespace,
            isolated,
        },
        compatibility: RuntimeCompatibility {
            manifest_version: revision.manifest.schema,
            runtime_contract_version: APPLICATION_RUNTIME_CONTRACT_VERSION,
            engine_version: ENGINE_VERSION.into(),
            minimum_engine: revision.manifest.runtime.minimum_engine.clone(),
            maximum_engine_exclusive: revision.manifest.runtime.maximum_engine_exclusive.clone(),
        },
    })
}

pub fn runtime_readiness(
    revision: &ApplicationRevision,
    environment: &str,
    inventory: &RuntimeInventory,
) -> RuntimeReadiness {
    let report = validate_runtime(revision, environment, inventory);
    let names = [
        "manifest valid",
        "runtime compatible",
        "schema valid",
        "references valid",
        "capabilities available",
        "connections available",
        "policies valid",
        "required artifacts available",
        "environment valid",
        "migration valid",
    ];
    let mut checks = names
        .into_iter()
        .map(|name| ReadinessCheck {
            name: name.into(),
            passed: true,
            blocking: true,
            message: "passed".into(),
        })
        .collect::<Vec<_>>();
    for error in &report.issues {
        let index = if error.path.contains("capabilit") {
            4
        } else if error.path.contains("connection") {
            5
        } else if error.path.contains("environment") {
            8
        } else if error.path.contains("runtime") {
            1
        } else if error.path.contains("polic") {
            6
        } else if error.path.contains("artifact") {
            7
        } else if error.path.contains("reference") {
            3
        } else {
            0
        };
        checks[index].passed = false;
        checks[index].message = error.message.clone();
    }
    let status = if checks.iter().any(|v| !v.passed && v.blocking) {
        ReadinessStatus::Blocked
    } else if checks.iter().any(|v| !v.passed) {
        ReadinessStatus::ReadyWithWarnings
    } else {
        ReadinessStatus::Ready
    };
    RuntimeReadiness {
        revision_id: revision.revision_id.clone(),
        environment: environment.into(),
        checks,
        status,
    }
}
pub fn canonical_runtime_bytes(contract: &ApplicationRuntimeContract) -> Result<Vec<u8>, String> {
    serde_json::to_vec(contract).map_err(|e| e.to_string())
}
pub fn runtime_hash(contract: &ApplicationRuntimeContract) -> Result<String, String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(canonical_runtime_bytes(contract)?)
    ))
}
pub fn validate_runtime(
    revision: &ApplicationRevision,
    environment: &str,
    inventory: &RuntimeInventory,
) -> RuntimeValidation {
    match resolve_runtime(revision, environment, inventory) {
        Ok(contract) => RuntimeValidation {
            valid: true,
            contract_hash: runtime_hash(&contract).ok(),
            issues: vec![],
        },
        Err(report) => report,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStatus {
    Running,
    Stopped,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInstance {
    pub runtime_id: String,
    pub tenant_id: String,
    pub application_id: String,
    pub revision_id: String,
    pub environment: String,
    pub contract_hash: String,
    pub state_namespace: String,
    pub status: RuntimeStatus,
    pub started_by: String,
    pub started_at: u64,
    pub stopped_at: Option<u64>,
}
#[derive(Debug, Default, Serialize, Deserialize)]
struct RuntimeRecords {
    instances: Vec<RuntimeInstance>,
}
#[derive(Clone)]
pub struct RuntimeStore {
    path: PathBuf,
    records: Arc<RwLock<RuntimeRecords>>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
impl RuntimeStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let records = if path.exists() {
            serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
        } else {
            RuntimeRecords::default()
        };
        Ok(Self {
            path,
            records: Arc::new(RwLock::new(records)),
        })
    }
    fn persist(&self, r: &RuntimeRecords) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(r).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(tmp, &self.path).map_err(|e| e.to_string())
    }
    pub fn start(
        &self,
        contract: &ApplicationRuntimeContract,
        actor: &str,
    ) -> Result<RuntimeInstance, String> {
        let mut r = self
            .records
            .write()
            .map_err(|_| "runtime store lock poisoned")?;
        let hash = runtime_hash(contract)?;
        if let Some(current) = r.instances.iter_mut().find(|v| {
            v.application_id == contract.application_id
                && v.revision_id == contract.revision_id
                && v.environment == contract.environment.name
        }) {
            current.status = RuntimeStatus::Running;
            current.stopped_at = None;
            let result = current.clone();
            self.persist(&r)?;
            return Ok(result);
        }
        let identity = Sha256::digest(
            format!(
                "{}:{}:{}:{hash}",
                contract.application_id, contract.revision_id, contract.environment.name
            )
            .as_bytes(),
        );
        let instance = RuntimeInstance {
            runtime_id: format!("runtime_{:x}", identity),
            tenant_id: contract.tenant_id.clone(),
            application_id: contract.application_id.clone(),
            revision_id: contract.revision_id.clone(),
            environment: contract.environment.name.clone(),
            contract_hash: hash,
            state_namespace: contract.environment.state_namespace.clone(),
            status: RuntimeStatus::Running,
            started_by: actor.into(),
            started_at: now(),
            stopped_at: None,
        };
        r.instances.push(instance.clone());
        self.persist(&r)?;
        Ok(instance)
    }
    pub fn stop(
        &self,
        tenant: &str,
        app: &str,
        revision: &str,
        environment: &str,
    ) -> Result<RuntimeInstance, String> {
        let mut r = self
            .records
            .write()
            .map_err(|_| "runtime store lock poisoned")?;
        let instance = r
            .instances
            .iter_mut()
            .find(|v| {
                v.tenant_id == tenant
                    && v.application_id == app
                    && v.revision_id == revision
                    && v.environment == environment
            })
            .ok_or("runtime is not running")?;
        instance.status = RuntimeStatus::Stopped;
        instance.stopped_at = Some(now());
        let result = instance.clone();
        self.persist(&r)?;
        Ok(result)
    }
    pub fn active(
        &self,
        tenant: &str,
        app: &str,
        revision: &str,
        environment: &str,
    ) -> Option<RuntimeInstance> {
        self.records
            .read()
            .ok()?
            .instances
            .iter()
            .find(|v| {
                v.tenant_id == tenant
                    && v.application_id == app
                    && v.revision_id == revision
                    && v.environment == environment
                    && v.status == RuntimeStatus::Running
            })
            .cloned()
    }
    pub fn list(&self, tenant: &str, app: &str) -> Vec<RuntimeInstance> {
        self.records
            .read()
            .map(|r| {
                r.instances
                    .iter()
                    .filter(|v| v.tenant_id == tenant && v.application_id == app)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryExecution {
    pub query: String,
    pub revision_id: String,
    pub state_version: u64,
    pub subscription_id: String,
    pub records: Vec<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionExecution {
    pub action: String,
    pub revision_id: String,
    pub state_version: u64,
    pub record_id: String,
    pub value: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeExecutionRequest {
    pub execution_id: String,
    pub application_id: String,
    pub revision_id: String,
    pub workflow_id: String,
    pub trigger: String,
    pub subject: String,
    pub input: Value,
    pub state: String,
    pub checkpoint: Option<Value>,
    pub result: Option<Value>,
}
pub fn trigger_requests(
    contract: &ApplicationRuntimeContract,
    event: &str,
    subject: &str,
    input: Value,
) -> Vec<RuntimeExecutionRequest> {
    contract
        .triggers
        .definitions
        .iter()
        .filter(|trigger| trigger.event == event)
        .map(|trigger| {
            let identity = Sha256::digest(
                format!(
                    "{}:{event}:{subject}:{}",
                    contract.revision_id,
                    serde_json::to_string(&input).unwrap_or_default()
                )
                .as_bytes(),
            );
            RuntimeExecutionRequest {
                execution_id: format!("execution_{identity:x}"),
                application_id: contract.application_id.clone(),
                revision_id: contract.revision_id.clone(),
                workflow_id: trigger.workflow.clone(),
                trigger: trigger.name.clone(),
                subject: subject.into(),
                input: input.clone(),
                state: "requested".into(),
                checkpoint: None,
                result: None,
            }
        })
        .collect()
}
fn runtime_collection(contract: &ApplicationRuntimeContract, collection: &str) -> String {
    format!("{}:{collection}", contract.environment.state_namespace)
}
fn matches_filter(value: &Value, filter: &Value) -> bool {
    filter.as_object().is_none_or(|expected| {
        expected
            .iter()
            .all(|(key, wanted)| value.get(key) == Some(wanted))
    })
}
fn project(value: &Value, fields: &[String]) -> Value {
    if fields.is_empty() {
        return value.clone();
    }
    let mut out = Map::new();
    for field in fields {
        if let Some(value) = value.get(field) {
            out.insert(field.clone(), value.clone());
        }
    }
    Value::Object(out)
}
pub fn execute_query(
    db: &FeltDb,
    contract: &ApplicationRuntimeContract,
    name: &str,
) -> Result<QueryExecution, String> {
    let query = contract
        .queries
        .definitions
        .iter()
        .find(|v| v.name == name)
        .ok_or("query not found")?;
    let collection = runtime_collection(contract, &query.collection);
    let rows = db
        .export_snapshot()
        .map_err(|e| e.to_string())?
        .rows
        .into_iter()
        .filter(|row| {
            row.capability == collection
                && !row.deleted
                && matches_filter(&row.value, &query.filter)
        })
        .map(|row| {
            let mut value = project(&row.value, &query.fields);
            if let Some(object) = value.as_object_mut() {
                object.insert("_id".into(), Value::String(row.key));
            }
            value
        })
        .collect::<Vec<_>>();
    let version = db.sequence().map_err(|e| e.to_string())?;
    Ok(QueryExecution {
        query: name.into(),
        revision_id: contract.revision_id.clone(),
        state_version: version,
        subscription_id: format!("query://{}/{}/{version}", contract.revision_id, name),
        records: rows,
    })
}
fn validate_input(
    contract: &ApplicationRuntimeContract,
    action: &ActionDefinition,
    input: &Value,
) -> Result<(), String> {
    let collection = contract
        .state
        .collections
        .iter()
        .find(|v| v.name == action.collection)
        .ok_or("action collection not found")?;
    let object = input.as_object().ok_or("action input must be an object")?;
    for field in &collection.fields {
        if field.required
            && action.operation == ActionOperation::Create
            && !object.contains_key(&field.name)
        {
            return Err(format!("required field is missing: {}", field.name));
        }
    }
    if object
        .keys()
        .any(|key| !collection.fields.iter().any(|field| &field.name == key) && key != "id")
    {
        return Err("action input contains an unknown field".into());
    }
    Ok(())
}
pub fn execute_action(
    db: &FeltDb,
    contract: &ApplicationRuntimeContract,
    name: &str,
    input: Value,
) -> Result<ActionExecution, String> {
    let action = contract
        .actions
        .definitions
        .iter()
        .find(|v| v.name == name)
        .ok_or("action not found")?;
    validate_input(contract, action, &input)?;
    let capability = db.capability(&runtime_collection(contract, &action.collection));
    let id = input.get("id").and_then(Value::as_str).map(str::to_string);
    let (record_id, value) = match action.operation {
        ActionOperation::Create => {
            let mut value = input;
            if let Some(object) = value.as_object_mut() {
                object.remove("id");
            }
            let key = if let Some(id) = id {
                capability
                    .insert_with_key(&id, value.clone())
                    .map_err(|e| e.to_string())?;
                id
            } else {
                capability
                    .insert(value.clone())
                    .map_err(|e| e.to_string())?
            };
            (key, Some(value))
        }
        ActionOperation::Update => {
            let id = id.ok_or("update action requires id")?;
            let mut value = input;
            if let Some(object) = value.as_object_mut() {
                object.remove("id");
            }
            capability
                .update(&id, value.clone())
                .map_err(|e| e.to_string())?;
            (id, Some(value))
        }
        ActionOperation::Delete => {
            let id = id.ok_or("delete action requires id")?;
            capability.delete(&id).map_err(|e| e.to_string())?;
            (id, None)
        }
    };
    Ok(ActionExecution {
        action: name.into(),
        revision_id: contract.revision_id.clone(),
        state_version: db.sequence().map_err(|e| e.to_string())?,
        record_id,
        value,
    })
}

/// Copies the current materialized application state into an isolated namespace.
/// This is intentionally a point-in-time copy: later production writes cannot flow
/// into the preview and preview writes cannot flow back to production.
pub fn snapshot_state_namespace(db: &FeltDb, from: &str, to: &str) -> Result<usize, String> {
    if from == to {
        return Err("source and destination namespaces must differ".into());
    }
    let prefix = format!("{from}:");
    let rows = db
        .export_snapshot()
        .map_err(|e| e.to_string())?
        .rows
        .into_iter()
        .filter(|row| !row.deleted && row.capability.starts_with(&prefix))
        .collect::<Vec<_>>();
    for row in &rows {
        let suffix = row
            .capability
            .strip_prefix(&prefix)
            .ok_or("invalid source namespace")?;
        db.capability(&format!("{to}:{suffix}"))
            .insert_with_key(&row.key, row.value.clone())
            .map_err(|e| e.to_string())?;
    }
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn revision() -> ApplicationRevision {
        let mut manifest = ApplicationManifest::empty("tenant", "app", "App");
        manifest.collections.push(CollectionDefinition {
            name: "incidents".into(),
            fields: vec![
                FieldDefinition {
                    name: "title".into(),
                    field_type: "string".into(),
                    required: true,
                    nullable: false,
                    default: None,
                    enum_values: vec![],
                    reference: None,
                    computed: None,
                },
                FieldDefinition {
                    name: "status".into(),
                    field_type: "string".into(),
                    required: false,
                    nullable: false,
                    default: None,
                    enum_values: vec![],
                    reference: None,
                    computed: None,
                },
            ],
        });
        manifest.queries.push(QueryDefinition {
            name: "OpenIncidents".into(),
            collection: "incidents".into(),
            filter: serde_json::json!({"status":"open"}),
            fields: vec![],
        });
        manifest.actions.push(ActionDefinition {
            name: "CreateIncident".into(),
            operation: ActionOperation::Create,
            collection: "incidents".into(),
            input_fields: vec!["title".into(), "status".into()],
            required_capabilities: vec![],
            audit: true,
        });
        let hash = manifest_hash(&manifest).unwrap();
        ApplicationRevision {
            revision_id: "apprev://tenant/app/test".into(),
            application_id: "app".into(),
            tenant_id: "tenant".into(),
            revision_number: 1,
            parent_revision_id: None,
            manifest_hash: hash,
            manifest,
            created_by: "owner".into(),
            created_at: 1,
            status: RevisionStatus::Committed,
        }
    }
    #[test]
    fn integrity_and_runtime_hash_are_deterministic() {
        let revision = revision();
        let a = resolve_runtime(&revision, "preview", &RuntimeInventory::default()).unwrap();
        let b = resolve_runtime(&revision, "preview", &RuntimeInventory::default()).unwrap();
        assert_eq!(runtime_hash(&a).unwrap(), runtime_hash(&b).unwrap());
        let mut corrupt = revision;
        corrupt.manifest.metadata.name = "Changed".into();
        assert!(!validate_runtime(&corrupt, "preview", &RuntimeInventory::default()).valid);
    }
    #[test]
    fn preview_state_is_isolated_and_actions_feed_queries() {
        let revision = revision();
        let preview = resolve_runtime(&revision, "preview", &RuntimeInventory::default()).unwrap();
        let production =
            resolve_runtime(&revision, "production", &RuntimeInventory::default()).unwrap();
        assert_ne!(
            preview.environment.state_namespace,
            production.environment.state_namespace
        );
        let db = FeltDb::open(std::env::temp_dir().join(format!("runtime-{}.log", now()))).unwrap();
        execute_action(
            &db,
            &preview,
            "CreateIncident",
            serde_json::json!({"title":"Power outage","status":"open"}),
        )
        .unwrap();
        assert_eq!(
            execute_query(&db, &preview, "OpenIncidents")
                .unwrap()
                .records
                .len(),
            1
        );
        assert!(execute_query(&db, &production, "OpenIncidents")
            .unwrap()
            .records
            .is_empty());
    }
    #[test]
    fn preview_snapshot_is_a_point_in_time_copy() {
        let revision = revision();
        let production = resolve_runtime(&revision, "production", &Default::default()).unwrap();
        let preview = resolve_runtime(&revision, "preview/editor", &Default::default()).unwrap();
        let db = FeltDb::open(std::env::temp_dir().join(format!("runtime-snapshot-{}.log", now())))
            .unwrap();
        execute_action(
            &db,
            &production,
            "CreateIncident",
            serde_json::json!({"id":"one","title":"First","status":"open"}),
        )
        .unwrap();
        assert_eq!(
            snapshot_state_namespace(
                &db,
                &production.environment.state_namespace,
                &preview.environment.state_namespace
            )
            .unwrap(),
            1
        );
        execute_action(
            &db,
            &production,
            "CreateIncident",
            serde_json::json!({"id":"two","title":"Second","status":"open"}),
        )
        .unwrap();
        assert_eq!(
            execute_query(&db, &preview, "OpenIncidents")
                .unwrap()
                .records
                .len(),
            1
        );
        assert_eq!(
            execute_query(&db, &production, "OpenIncidents")
                .unwrap()
                .records
                .len(),
            2
        );
    }

    #[test]
    fn unavailable_connections_fail_closed_and_trigger_keeps_revision() {
        let mut revision = revision();
        revision.manifest.connections.push(ConnectionBinding {
            name: "dispatch".into(),
            connection_id: "connection_missing".into(),
            configuration: Value::Object(Default::default()),
            secrets: vec![SecretReference {
                binding: "token".into(),
                secret_id: "secret_1".into(),
            }],
        });
        revision.manifest.workflows.push(WorkflowDefinition {
            name: "Dispatch".into(),
            steps: vec![],
        });
        revision.manifest.triggers.push(TriggerDefinition {
            name: "incident-created".into(),
            event: "Incident.created".into(),
            workflow: "Dispatch".into(),
        });
        revision.manifest_hash = manifest_hash(&revision.manifest).unwrap();
        let inventory = RuntimeInventory {
            enforce_availability: true,
            ..Default::default()
        };
        assert!(!validate_runtime(&revision, "production", &inventory).valid);
        let contract =
            resolve_runtime(&revision, "production", &RuntimeInventory::default()).unwrap();
        let requests = trigger_requests(
            &contract,
            "Incident.created",
            "agent_1",
            serde_json::json!({"id":"i1"}),
        );
        assert_eq!(requests[0].revision_id, revision.revision_id);
        assert_eq!(requests[0].workflow_id, "Dispatch");
        assert!(!serde_json::to_string(&contract)
            .unwrap()
            .contains("secret_1"));
    }
}
