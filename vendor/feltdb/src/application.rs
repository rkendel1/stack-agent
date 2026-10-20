use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;
fn default_state_schema_version() -> u64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ApplicationMetadata {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldDefinition {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub nullable: bool,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub enum_values: Vec<String>,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub computed: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectionDefinition {
    pub name: String,
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexDefinition {
    pub name: String,
    pub collection: String,
    pub fields: Vec<String>,
    #[serde(default)]
    pub unique: bool,
    #[serde(default)]
    pub sparse: bool,
    #[serde(default = "default_index_version")]
    pub version: u32,
}
fn default_index_version() -> u32 {
    1
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReferenceDefinition {
    pub name: String,
    pub from_collection: String,
    pub from_field: String,
    pub to_collection: String,
    pub to_field: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueryDefinition {
    pub name: String,
    pub collection: String,
    #[serde(default)]
    pub filter: Value,
    #[serde(default)]
    pub fields: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionOperation {
    Create,
    Update,
    Delete,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionDefinition {
    pub name: String,
    pub operation: ActionOperation,
    pub collection: String,
    #[serde(default)]
    pub input_fields: Vec<String>,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    #[serde(default)]
    pub audit: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowStepDefinition {
    pub name: String,
    #[serde(default)]
    pub capability: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowDefinition {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<WorkflowStepDefinition>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriggerDefinition {
    pub name: String,
    pub event: String,
    pub workflow: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleDefinition {
    pub name: String,
    pub expression: String,
    pub workflow: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentDefinition {
    pub name: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityBinding {
    pub name: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default = "public_capability_visibility")]
    pub visibility: String,
}
fn public_capability_visibility() -> String {
    "public".into()
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretReference {
    pub binding: String,
    pub secret_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectionBinding {
    pub name: String,
    pub connection_id: String,
    #[serde(default)]
    pub configuration: Value,
    #[serde(default)]
    pub secrets: Vec<SecretReference>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyDefinition {
    pub name: String,
    pub resource: String,
    #[serde(default)]
    pub read: Option<String>,
    #[serde(default)]
    pub write: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataBinding {
    pub name: String,
    #[serde(default)]
    pub collection: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub fields: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormDefinition {
    pub name: String,
    pub collection: String,
    #[serde(default)]
    pub fields: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardDefinition {
    pub name: String,
    #[serde(default)]
    pub bindings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteDefinition {
    pub path: String,
    pub view: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct UiApplicationModel {
    #[serde(default)]
    pub routes: Vec<RouteDefinition>,
    #[serde(default)]
    pub bindings: Vec<DataBinding>,
    #[serde(default)]
    pub forms: Vec<FormDefinition>,
    #[serde(default)]
    pub dashboards: Vec<DashboardDefinition>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentDefinition {
    pub name: String,
    #[serde(default)]
    pub configuration: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactReference {
    pub name: String,
    pub uri: String,
    pub media_type: String,
    /// Immutable source bytes used by trusted control-plane services. The URI
    /// remains the content identity; values are base64 and never interpreted by Rust.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DeploymentIntent {
    #[serde(default)]
    pub environments: Vec<String>,
    #[serde(default)]
    pub placement: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeRequirement {
    #[serde(default = "runtime_contract_version")]
    pub contract: u32,
    #[serde(default = "minimum_engine_version")]
    pub minimum_engine: String,
    #[serde(default)]
    pub maximum_engine_exclusive: Option<String>,
}
impl Default for RuntimeRequirement {
    fn default() -> Self {
        Self {
            contract: runtime_contract_version(),
            minimum_engine: minimum_engine_version(),
            maximum_engine_exclusive: Some("2.0.0".into()),
        }
    }
}
fn runtime_contract_version() -> u32 {
    1
}
fn minimum_engine_version() -> String {
    "0.2.0".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApplicationManifest {
    pub application_id: String,
    pub tenant_id: String,
    pub schema: u32,
    #[serde(default = "default_state_schema_version")]
    pub state_schema_version: u64,
    pub metadata: ApplicationMetadata,
    #[serde(default)]
    pub collections: Vec<CollectionDefinition>,
    #[serde(default)]
    pub indexes: Vec<IndexDefinition>,
    #[serde(default)]
    pub references: Vec<ReferenceDefinition>,
    #[serde(default)]
    pub queries: Vec<QueryDefinition>,
    #[serde(default)]
    pub actions: Vec<ActionDefinition>,
    #[serde(default)]
    pub workflows: Vec<WorkflowDefinition>,
    #[serde(default)]
    pub triggers: Vec<TriggerDefinition>,
    #[serde(default)]
    pub schedules: Vec<ScheduleDefinition>,
    #[serde(default)]
    pub agents: Vec<AgentDefinition>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityBinding>,
    /// Resolved, provider-neutral module semantic contracts. Secret values are forbidden.
    #[serde(default)]
    pub modules: Vec<serde_json::Value>,
    #[serde(default)]
    pub connections: Vec<ConnectionBinding>,
    #[serde(default)]
    pub policies: Vec<PolicyDefinition>,
    #[serde(default)]
    pub ui: UiApplicationModel,
    #[serde(default)]
    pub environments: Vec<EnvironmentDefinition>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactReference>,
    #[serde(default)]
    pub deployment: DeploymentIntent,
    #[serde(default)]
    pub runtime: RuntimeRequirement,
}

impl ApplicationManifest {
    pub fn empty(
        tenant_id: impl Into<String>,
        application_id: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            application_id: application_id.into(),
            tenant_id: tenant_id.into(),
            schema: MANIFEST_SCHEMA_VERSION,
            state_schema_version: 1,
            metadata: ApplicationMetadata {
                name: name.into(),
                ..Default::default()
            },
            collections: vec![],
            indexes: vec![],
            references: vec![],
            queries: vec![],
            actions: vec![],
            workflows: vec![],
            triggers: vec![],
            schedules: vec![],
            agents: vec![],
            capabilities: vec![],
            modules: vec![],
            connections: vec![],
            policies: vec![],
            ui: UiApplicationModel::default(),
            environments: vec![
                EnvironmentDefinition {
                    name: "development".into(),
                    configuration: BTreeMap::new(),
                },
                EnvironmentDefinition {
                    name: "preview".into(),
                    configuration: BTreeMap::new(),
                },
                EnvironmentDefinition {
                    name: "staging".into(),
                    configuration: BTreeMap::new(),
                },
                EnvironmentDefinition {
                    name: "production".into(),
                    configuration: BTreeMap::new(),
                },
            ],
            artifacts: vec![],
            deployment: DeploymentIntent {
                environments: vec![
                    "development".into(),
                    "preview".into(),
                    "staging".into(),
                    "production".into(),
                ],
                placement: None,
            },
            runtime: RuntimeRequirement::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Compatibility {
    Compatible,
    RequiresMigration,
    Breaking,
    Invalid,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationIssue {
    pub category: String,
    pub path: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationReport {
    pub valid: bool,
    pub compatibility: Compatibility,
    pub issues: Vec<ValidationIssue>,
}

fn is_valid_policy_subject(subject: &str) -> bool {
    subject.split('|').map(str::trim).all(|term| {
        matches!(term, "authenticated" | "owner" | "member")
            || term
                .strip_prefix("self(")
                .and_then(|value| value.strip_suffix(')'))
                .is_some_and(|field| {
                    let mut characters = field.chars();
                    characters
                        .next()
                        .is_some_and(|value| value == '_' || value.is_ascii_alphabetic())
                        && characters.all(|value| value == '_' || value.is_ascii_alphanumeric())
                })
    })
}

fn unique(
    issues: &mut Vec<ValidationIssue>,
    category: &str,
    values: impl IntoIterator<Item = String>,
) {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() {
            issues.push(ValidationIssue {
                category: "structural".into(),
                path: category.into(),
                message: "name cannot be empty".into(),
            });
        } else if !seen.insert(value.clone()) {
            issues.push(ValidationIssue {
                category: "structural".into(),
                path: format!("{category}.{value}"),
                message: format!("duplicate {category} name"),
            });
        }
    }
}
fn contains_plaintext_secret(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "password" | "secret" | "token" | "api_key" | "apikey" | "private_key"
            ) && value.as_str().is_some_and(|v| !v.is_empty())
                || contains_plaintext_secret(value)
        }),
        Value::Array(values) => values.iter().any(contains_plaintext_secret),
        _ => false,
    }
}

pub fn validate_manifest(
    manifest: &ApplicationManifest,
    expected_tenant: &str,
    expected_application: &str,
    previous: Option<&ApplicationManifest>,
) -> ValidationReport {
    let mut issues = vec![];
    if manifest.schema != MANIFEST_SCHEMA_VERSION {
        issues.push(ValidationIssue {
            category: "compatibility".into(),
            path: "schema".into(),
            message: format!("unsupported manifest schema {}", manifest.schema),
        });
    }
    if manifest.tenant_id != expected_tenant {
        issues.push(ValidationIssue {
            category: "security".into(),
            path: "tenant_id".into(),
            message: "cross-tenant manifest rejected".into(),
        });
    }
    if manifest.application_id != expected_application {
        issues.push(ValidationIssue {
            category: "security".into(),
            path: "application_id".into(),
            message: "cross-application manifest rejected".into(),
        });
    }
    unique(
        &mut issues,
        "collections",
        manifest.collections.iter().map(|v| v.name.clone()),
    );
    unique(
        &mut issues,
        "indexes",
        manifest.indexes.iter().map(|v| v.name.clone()),
    );
    unique(
        &mut issues,
        "workflows",
        manifest.workflows.iter().map(|v| v.name.clone()),
    );
    unique(
        &mut issues,
        "actions",
        manifest.actions.iter().map(|v| v.name.clone()),
    );
    unique(
        &mut issues,
        "capabilities",
        manifest.capabilities.iter().map(|v| v.name.clone()),
    );
    unique(
        &mut issues,
        "connections",
        manifest.connections.iter().map(|v| v.name.clone()),
    );
    unique(
        &mut issues,
        "routes",
        manifest.ui.routes.iter().map(|v| v.path.clone()),
    );
    unique(
        &mut issues,
        "artifacts",
        manifest.artifacts.iter().map(|v| v.name.clone()),
    );
    unique(
        &mut issues,
        "environments",
        manifest.environments.iter().map(|v| v.name.clone()),
    );
    let collections: BTreeMap<_, _> = manifest
        .collections
        .iter()
        .map(|v| (v.name.as_str(), v))
        .collect();
    for collection in &manifest.collections {
        unique(
            &mut issues,
            &format!("collections.{}.fields", collection.name),
            collection.fields.iter().map(|v| v.name.clone()),
        );
        for field in &collection.fields {
            if !crate::state_contract::valid_manifest_field_type(&field.field_type) {
                issues.push(issue(
                    "state_schema",
                    format!("collections.{}.fields.{}", collection.name, field.name),
                    "unsupported field type",
                ));
            }
        }
    }
    let field_exists = |collection: &str, field: &str| {
        collections
            .get(collection)
            .is_some_and(|value| value.fields.iter().any(|candidate| candidate.name == field))
    };
    for index in &manifest.indexes {
        if !collections.contains_key(index.collection.as_str()) {
            issues.push(issue(
                "semantic",
                format!("indexes.{}", index.name),
                "collection does not exist",
            ));
        }
        for field in &index.fields {
            if !field_exists(&index.collection, field) {
                issues.push(issue(
                    "semantic",
                    format!("indexes.{}.fields.{field}", index.name),
                    "field does not exist",
                ));
            }
        }
    }
    for reference in &manifest.references {
        if !field_exists(&reference.from_collection, &reference.from_field)
            || !field_exists(&reference.to_collection, &reference.to_field)
        {
            issues.push(issue(
                "semantic",
                format!("references.{}", reference.name),
                "referenced collection or field does not exist",
            ));
        }
    }
    let capabilities: BTreeSet<_> = manifest
        .capabilities
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    for capability in &manifest.capabilities {
        if !matches!(
            capability.visibility.as_str(),
            "public" | "agent" | "workflow" | "internal"
        ) {
            issues.push(issue(
                "semantic",
                format!("capabilities.{}.visibility", capability.name),
                "visibility must be public, agent, workflow, or internal",
            ));
        }
    }
    let workflows: BTreeSet<_> = manifest.workflows.iter().map(|v| v.name.as_str()).collect();
    let queries: BTreeSet<_> = manifest.queries.iter().map(|v| v.name.as_str()).collect();
    let bindings: BTreeSet<_> = manifest
        .ui
        .bindings
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    let connections: BTreeSet<_> = manifest
        .connections
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    for workflow in &manifest.workflows {
        for step in &workflow.steps {
            if let Some(capability) = &step.capability {
                if !capabilities.contains(capability.as_str()) {
                    issues.push(issue(
                        "semantic",
                        format!("workflows.{}.steps.{}", workflow.name, step.name),
                        "capability does not exist",
                    ));
                }
            }
        }
    }
    for agent in &manifest.agents {
        for capability in &agent.capabilities {
            if !capabilities.contains(capability.as_str()) {
                issues.push(issue(
                    "security",
                    format!("agents.{}.capabilities", agent.name),
                    "agent capability is not application-bound",
                ));
            }
        }
    }
    for trigger in &manifest.triggers {
        if !workflows.contains(trigger.workflow.as_str()) {
            issues.push(issue(
                "semantic",
                format!("triggers.{}", trigger.name),
                "workflow does not exist",
            ));
        }
    }
    for schedule in &manifest.schedules {
        if !workflows.contains(schedule.workflow.as_str()) {
            issues.push(issue(
                "semantic",
                format!("schedules.{}", schedule.name),
                "workflow does not exist",
            ));
        }
    }
    for query in &manifest.queries {
        if !collections.contains_key(query.collection.as_str()) {
            issues.push(issue(
                "semantic",
                format!("queries.{}", query.name),
                "collection does not exist",
            ));
        }
    }
    for action in &manifest.actions {
        if !collections.contains_key(action.collection.as_str()) {
            issues.push(issue(
                "semantic",
                format!("actions.{}", action.name),
                "collection does not exist",
            ));
        }
        if action
            .input_fields
            .iter()
            .any(|field| !field_exists(&action.collection, field))
        {
            issues.push(issue(
                "semantic",
                format!("actions.{}.input_fields", action.name),
                "action field does not exist",
            ));
        }
        if action
            .required_capabilities
            .iter()
            .any(|capability| !capabilities.contains(capability.as_str()))
        {
            issues.push(issue(
                "security",
                format!("actions.{}.required_capabilities", action.name),
                "action capability is not application-bound",
            ));
        }
    }
    for binding in &manifest.ui.bindings {
        if binding
            .collection
            .as_deref()
            .is_some_and(|v| !collections.contains_key(v))
            || binding
                .query
                .as_deref()
                .is_some_and(|v| !queries.contains(v))
        {
            issues.push(issue(
                "semantic",
                format!("ui.bindings.{}", binding.name),
                "state or query binding does not exist",
            ));
        }
    }
    for form in &manifest.ui.forms {
        if !collections.contains_key(form.collection.as_str())
            || form
                .fields
                .iter()
                .any(|field| !field_exists(&form.collection, field))
        {
            issues.push(issue(
                "semantic",
                format!("ui.forms.{}", form.name),
                "form collection or field does not exist",
            ));
        }
    }
    for dashboard in &manifest.ui.dashboards {
        if dashboard
            .bindings
            .iter()
            .any(|v| !bindings.contains(v.as_str()))
        {
            issues.push(issue(
                "semantic",
                format!("ui.dashboards.{}", dashboard.name),
                "dashboard binding does not exist",
            ));
        }
    }
    for artifact in &manifest.artifacts {
        if !artifact.uri.starts_with("artifact://sha256:") {
            issues.push(issue(
                "structural",
                format!("artifacts.{}", artifact.name),
                "artifact URI must be content-addressed",
            ));
        }
    }
    if manifest.deployment.environments.iter().any(|v| {
        !manifest
            .environments
            .iter()
            .any(|candidate| &candidate.name == v)
    }) {
        issues.push(issue(
            "semantic",
            "deployment.environments",
            "deployment references an unknown environment",
        ));
    }
    for connection in &manifest.connections {
        if contains_plaintext_secret(&connection.configuration) {
            issues.push(issue(
                "security",
                format!("connections.{}.configuration", connection.name),
                "plaintext secrets are forbidden; use secret references",
            ));
        }
        for secret in &connection.secrets {
            if secret.secret_id.trim().is_empty() {
                issues.push(issue(
                    "security",
                    format!("connections.{}.secrets", connection.name),
                    "secret reference is empty",
                ));
            }
        }
    }
    for (index, module) in manifest.modules.iter().enumerate() {
        if contains_plaintext_secret(module) || module.get("value").is_some() {
            issues.push(issue(
                "security",
                format!("modules.{index}"),
                "module contracts may declare secret requirements but never secret values",
            ));
        }
    }
    for environment in &manifest.environments {
        if environment.configuration.iter().any(|(key, value)| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "password" | "secret" | "token" | "api_key" | "apikey" | "private_key"
            ) && !value.is_empty()
        }) {
            issues.push(issue(
                "security",
                format!("environments.{}.configuration", environment.name),
                "plaintext environment secrets are forbidden; use connection secret references",
            ));
        }
    }
    for policy in &manifest.policies {
        if let Some(name) = policy.resource.strip_prefix("connection:") {
            if !connections.contains(name) {
                issues.push(issue(
                    "semantic",
                    format!("policies.{}", policy.name),
                    "connection resource does not exist",
                ));
            }
        }
        if let Some(ref read_subject) = policy.read {
            if !is_valid_policy_subject(read_subject) {
                issues.push(issue(
                    "semantic",
                    format!("policies.{}.read", policy.name),
                    &format!("invalid policy subject '{}'", read_subject),
                ));
            }
        }
        if let Some(ref write_subject) = policy.write {
            if !is_valid_policy_subject(write_subject) {
                issues.push(issue(
                    "semantic",
                    format!("policies.{}.write", policy.name),
                    &format!("invalid policy subject '{}'", write_subject),
                ));
            }
        }
    }
    let state_schema = crate::state_contract::schema_from_manifest(manifest, "validation");
    let schema_validation = crate::state_contract::validate_schema(&state_schema);
    for schema_issue in schema_validation.issues {
        issues.push(issue(
            "state_schema",
            format!("state_schema.{}", schema_issue.path),
            &format!("{}: {}", schema_issue.code, schema_issue.message),
        ));
    }
    if let Some(previous) = previous {
        let before = crate::state_contract::schema_from_manifest(previous, "previous");
        let schema_changed = before.collections != state_schema.collections;
        if schema_changed && manifest.state_schema_version <= previous.state_schema_version {
            issues.push(issue(
                "compatibility",
                "state_schema_version",
                "state_schema_version must increase when state schema changes",
            ));
        }
        if manifest.state_schema_version < previous.state_schema_version {
            issues.push(issue(
                "compatibility",
                "state_schema_version",
                "state_schema_version cannot decrease",
            ));
        }
    }
    let compatibility = if !issues.is_empty() {
        Compatibility::Invalid
    } else if let Some(previous) = previous {
        compatibility(previous, manifest)
    } else {
        Compatibility::Compatible
    };
    ValidationReport {
        valid: issues.is_empty(),
        compatibility,
        issues,
    }
}
fn issue(category: &str, path: impl Into<String>, message: &str) -> ValidationIssue {
    ValidationIssue {
        category: category.into(),
        path: path.into(),
        message: message.into(),
    }
}
fn compatibility(before: &ApplicationManifest, after: &ApplicationManifest) -> Compatibility {
    for old in &before.collections {
        match after.collections.iter().find(|v| v.name == old.name) {
            None => return Compatibility::Breaking,
            Some(new) => {
                for field in &old.fields {
                    match new.fields.iter().find(|v| v.name == field.name) {
                        None => return Compatibility::Breaking,
                        Some(value) if value.field_type != field.field_type => {
                            return Compatibility::RequiresMigration
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Compatibility::Compatible
}

fn sorted(mut manifest: ApplicationManifest) -> ApplicationManifest {
    macro_rules! sort_name {
        ($field:expr) => {
            $field.sort_by(|a, b| a.name.cmp(&b.name));
        };
    }
    sort_name!(manifest.collections);
    for collection in &mut manifest.collections {
        sort_name!(collection.fields);
    }
    sort_name!(manifest.indexes);
    sort_name!(manifest.references);
    sort_name!(manifest.queries);
    sort_name!(manifest.actions);
    sort_name!(manifest.workflows);
    for workflow in &mut manifest.workflows {
        sort_name!(workflow.steps);
    }
    sort_name!(manifest.triggers);
    sort_name!(manifest.schedules);
    sort_name!(manifest.agents);
    for agent in &mut manifest.agents {
        agent.capabilities.sort();
    }
    sort_name!(manifest.capabilities);
    manifest.modules.sort_by(|a, b| {
        a.get("id")
            .and_then(Value::as_str)
            .cmp(&b.get("id").and_then(Value::as_str))
    });
    sort_name!(manifest.connections);
    sort_name!(manifest.policies);
    sort_name!(manifest.ui.bindings);
    sort_name!(manifest.ui.forms);
    sort_name!(manifest.ui.dashboards);
    manifest.ui.routes.sort_by(|a, b| a.path.cmp(&b.path));
    sort_name!(manifest.environments);
    sort_name!(manifest.artifacts);
    manifest.deployment.environments.sort();
    manifest
}
pub fn canonical_bytes(manifest: &ApplicationManifest) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&sorted(manifest.clone())).map_err(|e| e.to_string())
}
pub fn manifest_hash(manifest: &ApplicationManifest) -> Result<String, String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(canonical_bytes(manifest)?)
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RevisionStatus {
    Committed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationDraft {
    pub draft_id: String,
    pub application_id: String,
    pub tenant_id: String,
    pub base_revision_id: Option<String>,
    pub manifest: ApplicationManifest,
    pub version: u64,
    pub updated_at: u64,
    pub updated_by: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationRevision {
    pub revision_id: String,
    pub application_id: String,
    pub tenant_id: String,
    pub revision_number: u64,
    pub parent_revision_id: Option<String>,
    pub manifest_hash: String,
    pub manifest: ApplicationManifest,
    pub created_by: String,
    pub created_at: u64,
    pub status: RevisionStatus,
}
pub fn draft_as_revision(draft: &ApplicationDraft) -> Result<ApplicationRevision, String> {
    let hash = manifest_hash(&draft.manifest)?;
    Ok(ApplicationRevision {
        revision_id: format!(
            "appdraft://{}/{}/{}@{}",
            draft.tenant_id, draft.application_id, draft.draft_id, draft.version
        ),
        application_id: draft.application_id.clone(),
        tenant_id: draft.tenant_id.clone(),
        revision_number: 0,
        parent_revision_id: draft.base_revision_id.clone(),
        manifest_hash: hash,
        manifest: draft.manifest.clone(),
        created_by: draft.updated_by.clone(),
        created_at: draft.updated_at,
        status: RevisionStatus::Committed,
    })
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionPromotion {
    pub application_id: String,
    pub from_revision: Option<String>,
    pub to_revision: String,
    pub environment: String,
    pub promoted_by: String,
    pub promoted_at: u64,
    pub reason: String,
    #[serde(default)]
    pub diff_hash: String,
    #[serde(default)]
    pub correlation_id: String,
    #[serde(default)]
    pub rollback: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionAuditEvent {
    pub event: String,
    pub tenant_id: String,
    pub application_id: String,
    pub subject: String,
    pub revision_id: Option<String>,
    pub parent_revision_id: Option<String>,
    pub manifest_hash: Option<String>,
    pub authorization_decision: bool,
    pub timestamp: u64,
    pub correlation_id: String,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub from_revision: Option<String>,
    #[serde(default)]
    pub to_revision: Option<String>,
    #[serde(default)]
    pub diff_hash: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RevisionRecovery {
    pub recovery_id: String,
    pub application_id: String,
    pub environment: String,
    pub source_revision: String,
    pub target_revision: String,
    pub approved_by: String,
    pub reason: String,
    pub authorization_level: String,
    pub recovered_at: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChangeSafety {
    Safe,
    Additive,
    Modified,
    RequiresMigration,
    Breaking,
    SecuritySensitive,
    RuntimeSensitive,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestChange {
    pub section: String,
    pub name: String,
    pub kind: ChangeKind,
    pub safety: ChangeSafety,
    #[serde(default)]
    pub detail: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestDiff {
    pub from_revision: Option<String>,
    pub to_revision: String,
    pub changes: Vec<ManifestChange>,
    pub compatibility: Compatibility,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DiffClassification {
    Unchanged,
    Additive,
    Modified,
    MigrationRequired,
    Breaking,
    SecuritySensitive,
    RuntimeSensitive,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationOperation {
    pub section: String,
    pub resource: String,
    pub operation: String,
    pub destructive: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationPlan {
    pub from_revision: Option<String>,
    pub to_revision: String,
    pub operations: Vec<MigrationOperation>,
    pub destructive_operations: Vec<MigrationOperation>,
    pub reversible: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityChange {
    pub section: String,
    pub resource: String,
    pub description: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationRevisionDiff {
    pub application_id: String,
    pub from_revision: Option<String>,
    pub to_revision: String,
    pub changes: Vec<ManifestChange>,
    pub classification: DiffClassification,
    pub migrations: Vec<MigrationOperation>,
    pub security_changes: Vec<SecurityChange>,
    pub compatibility: Compatibility,
    pub migration_plan: MigrationPlan,
    pub diff_hash: String,
    #[serde(default)]
    pub state_schema_compatibility: Option<crate::state_contract::SchemaCompatibilityReport>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PreviewDataMode {
    Empty,
    Snapshot,
    Cloned,
    SharedReadOnly,
}
impl Default for PreviewDataMode {
    fn default() -> Self {
        Self::Empty
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewPolicy {
    #[serde(default)]
    pub state: PreviewDataMode,
    #[serde(default)]
    pub artifacts: PreviewDataMode,
    #[serde(default)]
    pub connections: PreviewDataMode,
    #[serde(default = "default_true")]
    pub workflows_enabled: bool,
}
fn default_true() -> bool {
    true
}
impl Default for PreviewPolicy {
    fn default() -> Self {
        Self {
            state: PreviewDataMode::Empty,
            artifacts: PreviewDataMode::SharedReadOnly,
            connections: PreviewDataMode::Empty,
            workflows_enabled: true,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationPreview {
    pub preview_id: String,
    pub application_id: String,
    pub tenant_id: String,
    pub revision_id: String,
    pub environment: String,
    pub based_on_revision: Option<String>,
    pub policy: PreviewPolicy,
    pub created_by: String,
    pub created_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RevisionRecords {
    drafts: Vec<ApplicationDraft>,
    revisions: Vec<ApplicationRevision>,
    promotions: Vec<RevisionPromotion>,
    #[serde(default)]
    previews: Vec<ApplicationPreview>,
    environment_pointers: BTreeMap<String, BTreeMap<String, String>>,
    audit: Vec<RevisionAuditEvent>,
    #[serde(default)]
    untrusted_revisions: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    recoveries: Vec<RevisionRecovery>,
}
#[derive(Clone)]
pub struct ApplicationStore {
    path: PathBuf,
    records: Arc<RwLock<RevisionRecords>>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn opaque_id(prefix: &str, seed: &str) -> String {
    let digest = Sha256::digest(format!("{}:{seed}", now()).as_bytes());
    format!(
        "{prefix}_{}",
        digest[..12]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

impl ApplicationStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let records = if path.exists() {
            serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
        } else {
            RevisionRecords::default()
        };
        Ok(Self {
            path,
            records: Arc::new(RwLock::new(records)),
        })
    }

    /// Return every tenant scope asserted by canonical application definition
    /// records for one stable application identity. Callers must reject zero
    /// or multiple distinct tenants; ordering is never ownership evidence.
    pub fn tenant_ownership_candidates(&self, application_id: &str) -> Vec<String> {
        let Ok(records) = self.records.read() else {
            return Vec::new();
        };
        records
            .revisions
            .iter()
            .map(|value| (&value.application_id, &value.tenant_id))
            .filter(|(candidate, _)| candidate.as_str() == application_id)
            .map(|(_, tenant)| tenant.clone())
            .collect()
    }
    fn persist(&self, r: &RevisionRecords) -> Result<(), String> {
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

    /// Remove revision material for one application after its tenancy record
    /// has passed the caller's guarded destruction policy. The append-only
    /// state and global audit logs remain separate.
    pub fn purge_application(&self, application_id: &str) -> Result<(), String> {
        let mut r = self
            .records
            .write()
            .map_err(|_| "application store lock poisoned")?;
        r.drafts
            .retain(|value| value.application_id != application_id);
        r.revisions
            .retain(|value| value.application_id != application_id);
        r.promotions
            .retain(|value| value.application_id != application_id);
        r.previews
            .retain(|value| value.application_id != application_id);
        r.environment_pointers.remove(application_id);
        r.audit
            .retain(|value| value.application_id != application_id);
        r.untrusted_revisions.remove(application_id);
        r.recoveries
            .retain(|value| value.application_id != application_id);
        self.persist(&r)
    }
    fn event(
        r: &mut RevisionRecords,
        event: &str,
        tenant: &str,
        app: &str,
        subject: &str,
        revision: Option<&ApplicationRevision>,
        parent: Option<String>,
    ) {
        r.audit.push(RevisionAuditEvent {
            event: event.into(),
            tenant_id: tenant.into(),
            application_id: app.into(),
            subject: subject.into(),
            revision_id: revision.map(|v| v.revision_id.clone()),
            parent_revision_id: revision
                .and_then(|v| v.parent_revision_id.clone())
                .or(parent),
            manifest_hash: revision.map(|v| v.manifest_hash.clone()),
            authorization_decision: true,
            timestamp: now(),
            correlation_id: opaque_id("corr", subject),
            environment: None,
            from_revision: None,
            to_revision: None,
            diff_hash: None,
        });
    }
    pub fn create_draft(
        &self,
        tenant: &str,
        app: &str,
        name: &str,
        actor: &str,
        base: Option<&str>,
    ) -> Result<ApplicationDraft, String> {
        let mut r = self
            .records
            .write()
            .map_err(|_| "application store lock poisoned")?;
        let manifest = match base {
            Some(id) => r
                .revisions
                .iter()
                .find(|v| v.revision_id == id && v.application_id == app && v.tenant_id == tenant)
                .ok_or("base revision not found")?
                .manifest
                .clone(),
            None => ApplicationManifest::empty(tenant, app, name),
        };
        let draft = ApplicationDraft {
            draft_id: opaque_id("draft", &format!("{app}:{actor}:{}", r.drafts.len())),
            application_id: app.into(),
            tenant_id: tenant.into(),
            base_revision_id: base.map(str::to_string),
            manifest,
            version: 1,
            updated_at: now(),
            updated_by: actor.into(),
        };
        r.drafts.push(draft.clone());
        Self::event(
            &mut r,
            "draft.created",
            tenant,
            app,
            actor,
            None,
            base.map(str::to_string),
        );
        self.persist(&r)?;
        Ok(draft)
    }
    pub fn get_draft(&self, tenant: &str, app: &str, id: &str) -> Option<ApplicationDraft> {
        self.records
            .read()
            .ok()?
            .drafts
            .iter()
            .find(|v| v.draft_id == id && v.application_id == app && v.tenant_id == tenant)
            .cloned()
    }
    pub fn update_draft(
        &self,
        tenant: &str,
        app: &str,
        id: &str,
        version: u64,
        manifest: ApplicationManifest,
        actor: &str,
    ) -> Result<ApplicationDraft, String> {
        let report = validate_manifest(&manifest, tenant, app, None);
        if !report.valid {
            return Err(format!(
                "invalid manifest: {}",
                serde_json::to_string(&report).unwrap_or_default()
            ));
        }
        let mut r = self
            .records
            .write()
            .map_err(|_| "application store lock poisoned")?;
        let index = r
            .drafts
            .iter()
            .position(|v| v.draft_id == id && v.application_id == app && v.tenant_id == tenant)
            .ok_or("draft not found")?;
        if r.drafts[index].version != version {
            return Err(format!("version_conflict:{}", r.drafts[index].version));
        }
        r.drafts[index].manifest = manifest;
        r.drafts[index].version += 1;
        r.drafts[index].updated_at = now();
        r.drafts[index].updated_by = actor.into();
        let result = r.drafts[index].clone();
        Self::event(
            &mut r,
            "draft.updated",
            tenant,
            app,
            actor,
            None,
            result.base_revision_id.clone(),
        );
        self.persist(&r)?;
        Ok(result)
    }
    pub fn validate_draft(
        &self,
        tenant: &str,
        app: &str,
        id: &str,
        actor: &str,
    ) -> Result<ValidationReport, String> {
        let mut r = self
            .records
            .write()
            .map_err(|_| "application store lock poisoned")?;
        let draft = r
            .drafts
            .iter()
            .find(|v| v.draft_id == id && v.application_id == app && v.tenant_id == tenant)
            .ok_or("draft not found")?
            .clone();
        let previous = draft
            .base_revision_id
            .as_ref()
            .and_then(|base| r.revisions.iter().find(|v| &v.revision_id == base))
            .map(|v| &v.manifest);
        let report = validate_manifest(&draft.manifest, tenant, app, previous);
        Self::event(
            &mut r,
            if report.valid {
                "draft.validated"
            } else {
                "revision.rejected"
            },
            tenant,
            app,
            actor,
            None,
            draft.base_revision_id,
        );
        self.persist(&r)?;
        Ok(report)
    }
    pub fn commit(
        &self,
        tenant: &str,
        app: &str,
        id: &str,
        actor: &str,
    ) -> Result<ApplicationRevision, String> {
        let mut r = self
            .records
            .write()
            .map_err(|_| "application store lock poisoned")?;
        let draft = r
            .drafts
            .iter()
            .find(|v| v.draft_id == id && v.application_id == app && v.tenant_id == tenant)
            .ok_or("draft not found")?
            .clone();
        let previous = draft
            .base_revision_id
            .as_ref()
            .and_then(|base| r.revisions.iter().find(|v| &v.revision_id == base))
            .map(|v| &v.manifest);
        let report = validate_manifest(&draft.manifest, tenant, app, previous);
        if !report.valid {
            return Err(format!(
                "invalid manifest: {}",
                serde_json::to_string(&report).unwrap_or_default()
            ));
        }
        let hash = manifest_hash(&draft.manifest)?;
        let number = r
            .revisions
            .iter()
            .filter(|v| v.application_id == app)
            .map(|v| v.revision_number)
            .max()
            .unwrap_or(0)
            + 1;
        let identity = Sha256::digest(
            format!(
                "{tenant}\0{app}\0{}\0{hash}",
                draft.base_revision_id.as_deref().unwrap_or("")
            )
            .as_bytes(),
        );
        let revision = ApplicationRevision {
            revision_id: format!("apprev://{tenant}/{app}/sha256:{identity:x}"),
            application_id: app.into(),
            tenant_id: tenant.into(),
            revision_number: number,
            parent_revision_id: draft.base_revision_id.clone(),
            manifest_hash: hash,
            manifest: sorted(draft.manifest),
            created_by: actor.into(),
            created_at: now(),
            status: RevisionStatus::Committed,
        };
        r.revisions.push(revision.clone());
        r.drafts.retain(|v| v.draft_id != id);
        Self::event(
            &mut r,
            "revision.created",
            tenant,
            app,
            actor,
            Some(&revision),
            None,
        );
        self.persist(&r)?;
        Ok(revision)
    }
    pub fn revisions(&self, tenant: &str, app: &str) -> Vec<ApplicationRevision> {
        let mut values = self
            .records
            .read()
            .map(|r| {
                r.revisions
                    .iter()
                    .filter(|v| v.tenant_id == tenant && v.application_id == app)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        values.sort_by_key(|v| std::cmp::Reverse(v.revision_number));
        values
    }
    pub fn revision(&self, tenant: &str, app: &str, id: &str) -> Option<ApplicationRevision> {
        self.records
            .read()
            .ok()?
            .revisions
            .iter()
            .find(|v| {
                v.tenant_id == tenant
                    && v.application_id == app
                    && (v.revision_id == id || v.revision_number.to_string() == id)
            })
            .cloned()
    }
    pub fn promote(
        &self,
        tenant: &str,
        app: &str,
        id: &str,
        environment: &str,
        actor: &str,
        reason: &str,
    ) -> Result<RevisionPromotion, String> {
        let expected = self.pointers(app).get(environment).cloned();
        self.move_environment_pointer(
            tenant,
            app,
            id,
            environment,
            expected.as_deref(),
            actor,
            reason,
            true,
            false,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn move_environment_pointer(
        &self,
        tenant: &str,
        app: &str,
        id: &str,
        environment: &str,
        expected_current_revision: Option<&str>,
        actor: &str,
        reason: &str,
        approve_destructive_changes: bool,
        rollback: bool,
    ) -> Result<RevisionPromotion, String> {
        let mut r = self
            .records
            .write()
            .map_err(|_| "application store lock poisoned")?;
        let revision = r
            .revisions
            .iter()
            .find(|v| {
                v.tenant_id == tenant
                    && v.application_id == app
                    && (v.revision_id == id || v.revision_number.to_string() == id)
            })
            .ok_or("revision not found")?
            .clone();
        if r.untrusted_revisions
            .get(app)
            .is_some_and(|values| values.contains_key(&revision.revision_id))
        {
            return Err("revision is permanently untrusted".into());
        }
        if manifest_hash(&revision.manifest)? != revision.manifest_hash {
            return Err("revision integrity check failed".into());
        }
        if !revision
            .manifest
            .environments
            .iter()
            .any(|v| v.name == environment)
        {
            return Err("environment is not defined by the revision".into());
        }
        let from = r
            .environment_pointers
            .get(app)
            .and_then(|v| v.get(environment))
            .cloned();
        if from.as_deref() != expected_current_revision {
            return Err(format!(
                "expected_revision_mismatch:{}",
                from.unwrap_or_default()
            ));
        }
        let before = from
            .as_deref()
            .and_then(|previous| r.revisions.iter().find(|v| v.revision_id == previous));
        let diff = application_revision_diff(before, &revision)?;
        if !approve_destructive_changes
            && diff.migration_plan.operations.iter().any(|v| v.destructive)
        {
            return Err("destructive_changes_require_approval".into());
        }
        r.environment_pointers
            .entry(app.into())
            .or_default()
            .insert(environment.into(), revision.revision_id.clone());
        let promotion = RevisionPromotion {
            application_id: app.into(),
            from_revision: from.clone(),
            to_revision: revision.revision_id.clone(),
            environment: environment.into(),
            promoted_by: actor.into(),
            promoted_at: now(),
            reason: reason.into(),
            diff_hash: diff.diff_hash,
            correlation_id: opaque_id("corr", &format!("{app}:{environment}:{actor}")),
            rollback,
        };
        r.promotions.push(promotion.clone());
        Self::event(
            &mut r,
            if rollback {
                "revision.rollback"
            } else {
                "revision.promoted"
            },
            tenant,
            app,
            actor,
            Some(&revision),
            None,
        );
        if let Some(event) = r.audit.last_mut() {
            event.event = if rollback {
                "application.revision.rolled_back".into()
            } else {
                "application.revision.promoted".into()
            };
            event.environment = Some(environment.into());
            event.from_revision = from;
            event.to_revision = Some(revision.revision_id.clone());
            event.diff_hash = Some(promotion.diff_hash.clone());
            event.correlation_id = promotion.correlation_id.clone();
        }
        self.persist(&r)?;
        Ok(promotion)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn recover_environment_pointer(
        &self,
        tenant: &str,
        app: &str,
        environment: &str,
        expected_current_revision: &str,
        target_revision: &str,
        actor: &str,
        reason: &str,
        authorization_level: &str,
        recovery_id: &str,
    ) -> Result<RevisionRecovery, String> {
        let mut records = self
            .records
            .write()
            .map_err(|_| "application store lock poisoned")?;
        if let Some(existing) = records
            .recoveries
            .iter()
            .find(|value| value.recovery_id == recovery_id)
        {
            if existing.application_id == app
                && existing.environment == environment
                && existing.source_revision == expected_current_revision
                && existing.target_revision == target_revision
            {
                return Ok(existing.clone());
            }
            return Err("recovery_id_conflict".into());
        }
        let current = records
            .environment_pointers
            .get(app)
            .and_then(|values| values.get(environment))
            .cloned()
            .unwrap_or_default();
        if current != expected_current_revision {
            return Err(format!("expected_revision_mismatch:{current}"));
        }
        let target = records
            .revisions
            .iter()
            .find(|value| {
                value.tenant_id == tenant
                    && value.application_id == app
                    && (value.revision_id == target_revision
                        || value.revision_number.to_string() == target_revision)
            })
            .ok_or("target revision not found")?
            .clone();
        if manifest_hash(&target.manifest)? != target.manifest_hash {
            return Err("target revision integrity check failed".into());
        }
        if records
            .untrusted_revisions
            .get(app)
            .is_some_and(|values| values.contains_key(&target.revision_id))
        {
            return Err("target revision is permanently untrusted".into());
        }
        let recovery = RevisionRecovery {
            recovery_id: recovery_id.into(),
            application_id: app.into(),
            environment: environment.into(),
            source_revision: expected_current_revision.into(),
            target_revision: target.revision_id.clone(),
            approved_by: actor.into(),
            reason: reason.into(),
            authorization_level: authorization_level.into(),
            recovered_at: now(),
        };
        records
            .untrusted_revisions
            .entry(app.into())
            .or_default()
            .insert(expected_current_revision.into(), recovery_id.into());
        records
            .environment_pointers
            .entry(app.into())
            .or_default()
            .insert(environment.into(), target.revision_id.clone());
        records.recoveries.push(recovery.clone());
        Self::event(
            &mut records,
            "application.revision.recovered",
            tenant,
            app,
            actor,
            Some(&target),
            Some(expected_current_revision.into()),
        );
        if let Some(event) = records.audit.last_mut() {
            event.environment = Some(environment.into());
            event.from_revision = Some(expected_current_revision.into());
            event.to_revision = Some(target.revision_id);
            event.correlation_id = recovery_id.into();
        }
        self.persist(&records)?;
        Ok(recovery)
    }

    pub fn history(&self, tenant: &str, app: &str, environment: &str) -> Vec<RevisionPromotion> {
        self.records
            .read()
            .map(|r| {
                r.promotions
                    .iter()
                    .filter(|v| {
                        v.application_id == app
                            && v.environment == environment
                            && r.revisions.iter().any(|rev| {
                                rev.tenant_id == tenant && rev.revision_id == v.to_revision
                            })
                    })
                    .cloned()
                    .rev()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn create_preview(
        &self,
        tenant: &str,
        app: &str,
        id: &str,
        actor: &str,
        policy: PreviewPolicy,
    ) -> Result<ApplicationPreview, String> {
        let revision = self.revision(tenant, app, id).ok_or("revision not found")?;
        if manifest_hash(&revision.manifest)? != revision.manifest_hash {
            return Err("revision integrity check failed".into());
        }
        let mut r = self
            .records
            .write()
            .map_err(|_| "application store lock poisoned")?;
        let preview = ApplicationPreview {
            preview_id: opaque_id(
                "preview",
                &format!("{app}:{}:{actor}", revision.revision_id),
            ),
            application_id: app.into(),
            tenant_id: tenant.into(),
            revision_id: revision.revision_id,
            environment: format!("preview/{actor}"),
            based_on_revision: r
                .environment_pointers
                .get(app)
                .and_then(|v| v.get("production"))
                .cloned(),
            policy,
            created_by: actor.into(),
            created_at: now(),
        };
        r.previews.push(preview.clone());
        self.persist(&r)?;
        Ok(preview)
    }
    pub fn preview(&self, tenant: &str, app: &str, id: &str) -> Option<ApplicationPreview> {
        self.records
            .read()
            .ok()?
            .previews
            .iter()
            .find(|v| v.tenant_id == tenant && v.application_id == app && v.preview_id == id)
            .cloned()
    }
    pub fn pointers(&self, app: &str) -> BTreeMap<String, String> {
        self.records
            .read()
            .ok()
            .and_then(|r| r.environment_pointers.get(app).cloned())
            .unwrap_or_default()
    }
    pub fn diff(&self, tenant: &str, app: &str, id: &str) -> Result<ManifestDiff, String> {
        let target = self.revision(tenant, app, id).ok_or("revision not found")?;
        let before = target
            .parent_revision_id
            .as_deref()
            .and_then(|parent| self.revision(tenant, app, parent));
        Ok(diff_manifests(before.as_ref(), &target))
    }
    pub fn revision_diff(
        &self,
        tenant: &str,
        app: &str,
        from: Option<&str>,
        to: &str,
    ) -> Result<ApplicationRevisionDiff, String> {
        let after = self.revision(tenant, app, to).ok_or("revision not found")?;
        let before = match from {
            Some(id) => Some(
                self.revision(tenant, app, id)
                    .ok_or("from revision not found")?,
            ),
            None => after
                .parent_revision_id
                .as_deref()
                .and_then(|id| self.revision(tenant, app, id)),
        };
        application_revision_diff(before.as_ref(), &after)
    }
    pub fn audit(&self, tenant: &str, app: &str) -> Vec<RevisionAuditEvent> {
        self.records
            .read()
            .map(|r| {
                r.audit
                    .iter()
                    .filter(|v| v.tenant_id == tenant && v.application_id == app)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn export_revision(&self, tenant: &str, app: &str, id: &str) -> Result<Value, String> {
        let revision = self.revision(tenant, app, id).ok_or("revision not found")?;
        let manifest = canonical_bytes(&revision.manifest)?;
        let checksum = format!("sha256:{:x}", Sha256::digest(&manifest));
        Ok(
            serde_json::json!({"format":"feltdb-app/v1","revision":revision,"files":{"manifest.json":String::from_utf8(manifest).map_err(|e|e.to_string())?},"checksums":{"manifest.json":checksum}}),
        )
    }
    pub fn import_draft(
        &self,
        tenant: &str,
        app: &str,
        name: &str,
        actor: &str,
        bundle: &Value,
    ) -> Result<ApplicationDraft, String> {
        if bundle.get("format").and_then(Value::as_str) != Some("feltdb-app/v1") {
            return Err("unsupported application bundle".into());
        }
        let text = bundle
            .pointer("/files/manifest.json")
            .and_then(Value::as_str)
            .ok_or("bundle manifest is missing")?;
        let expected = bundle
            .pointer("/checksums/manifest.json")
            .and_then(Value::as_str)
            .ok_or("bundle checksum is missing")?;
        let actual = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
        if actual != expected {
            return Err("bundle checksum mismatch".into());
        }
        let mut manifest: ApplicationManifest =
            serde_json::from_str(text).map_err(|e| e.to_string())?;
        manifest.tenant_id = tenant.into();
        manifest.application_id = app.into();
        let report = validate_manifest(&manifest, tenant, app, None);
        if !report.valid {
            return Err(format!(
                "invalid manifest: {}",
                serde_json::to_string(&report).unwrap_or_default()
            ));
        }
        let mut draft = self.create_draft(tenant, app, name, actor, None)?;
        draft = self.update_draft(tenant, app, &draft.draft_id, draft.version, manifest, actor)?;
        Ok(draft)
    }
}

fn named_map<T: Serialize>(values: &[T]) -> BTreeMap<String, Value> {
    values
        .iter()
        .filter_map(|v| {
            let value = serde_json::to_value(v).ok()?;
            Some((value.get("name")?.as_str()?.to_string(), value))
        })
        .collect()
}
fn section_changes<T: Serialize>(
    changes: &mut Vec<ManifestChange>,
    section: &str,
    before: &[T],
    after: &[T],
    default: ChangeSafety,
) {
    let left = named_map(before);
    let right = named_map(after);
    for (name, value) in &right {
        match left.get(name) {
            None => changes.push(ManifestChange {
                section: section.into(),
                name: name.clone(),
                kind: ChangeKind::Added,
                safety: default.clone(),
                detail: None,
            }),
            Some(old) if old != value => changes.push(ManifestChange {
                section: section.into(),
                name: name.clone(),
                kind: ChangeKind::Modified,
                safety: default.clone(),
                detail: None,
            }),
            _ => {}
        }
    }
    for name in left.keys().filter(|name| !right.contains_key(*name)) {
        changes.push(ManifestChange {
            section: section.into(),
            name: name.clone(),
            kind: ChangeKind::Removed,
            safety: ChangeSafety::Breaking,
            detail: Some("resource removal requires explicit destructive-change approval".into()),
        });
    }
}
pub fn diff_manifests(
    before: Option<&ApplicationRevision>,
    after: &ApplicationRevision,
) -> ManifestDiff {
    let empty = ApplicationManifest::empty(
        &after.tenant_id,
        &after.application_id,
        &after.manifest.metadata.name,
    );
    let left = before.map(|v| &v.manifest).unwrap_or(&empty);
    let mut changes = vec![];
    section_changes(
        &mut changes,
        "collections",
        &left.collections,
        &after.manifest.collections,
        ChangeSafety::RequiresMigration,
    );
    section_changes(
        &mut changes,
        "indexes",
        &left.indexes,
        &after.manifest.indexes,
        ChangeSafety::Safe,
    );
    section_changes(
        &mut changes,
        "workflows",
        &left.workflows,
        &after.manifest.workflows,
        ChangeSafety::Safe,
    );
    section_changes(
        &mut changes,
        "actions",
        &left.actions,
        &after.manifest.actions,
        ChangeSafety::SecuritySensitive,
    );
    section_changes(
        &mut changes,
        "agents",
        &left.agents,
        &after.manifest.agents,
        ChangeSafety::SecuritySensitive,
    );
    section_changes(
        &mut changes,
        "capabilities",
        &left.capabilities,
        &after.manifest.capabilities,
        ChangeSafety::SecuritySensitive,
    );
    section_changes(
        &mut changes,
        "connections",
        &left.connections,
        &after.manifest.connections,
        ChangeSafety::SecuritySensitive,
    );
    section_changes(
        &mut changes,
        "policies",
        &left.policies,
        &after.manifest.policies,
        ChangeSafety::SecuritySensitive,
    );
    let left_routes: BTreeMap<_, _> = left
        .ui
        .routes
        .iter()
        .map(|v| (v.path.clone(), v.view.clone()))
        .collect();
    let right_routes: BTreeMap<_, _> = after
        .manifest
        .ui
        .routes
        .iter()
        .map(|v| (v.path.clone(), v.view.clone()))
        .collect();
    for (path, view) in &right_routes {
        changes.push(ManifestChange {
            section: "routes".into(),
            name: path.clone(),
            kind: if left_routes.contains_key(path) {
                if left_routes.get(path) == Some(view) {
                    continue;
                } else {
                    ChangeKind::Modified
                }
            } else {
                ChangeKind::Added
            },
            safety: ChangeSafety::Safe,
            detail: None,
        });
    }
    for path in left_routes
        .keys()
        .filter(|path| !right_routes.contains_key(*path))
    {
        changes.push(ManifestChange {
            section: "routes".into(),
            name: path.clone(),
            kind: ChangeKind::Removed,
            safety: ChangeSafety::Breaking,
            detail: Some("route removal may break existing navigation".into()),
        });
    }
    section_changes(
        &mut changes,
        "forms",
        &left.ui.forms,
        &after.manifest.ui.forms,
        ChangeSafety::Safe,
    );
    section_changes(
        &mut changes,
        "dashboards",
        &left.ui.dashboards,
        &after.manifest.ui.dashboards,
        ChangeSafety::Safe,
    );
    ManifestDiff {
        from_revision: before.map(|v| v.revision_id.clone()),
        to_revision: after.revision_id.clone(),
        changes,
        compatibility: compatibility(left, &after.manifest),
    }
}

pub fn application_revision_diff(
    before: Option<&ApplicationRevision>,
    after: &ApplicationRevision,
) -> Result<ApplicationRevisionDiff, String> {
    if manifest_hash(&after.manifest)? != after.manifest_hash {
        return Err("revision integrity check failed".into());
    }
    if let Some(value) = before {
        if value.application_id != after.application_id || value.tenant_id != after.tenant_id {
            return Err("cross-application revision comparison is forbidden".into());
        }
        if manifest_hash(&value.manifest)? != value.manifest_hash {
            return Err("from revision integrity check failed".into());
        }
    }
    let legacy = diff_manifests(before, after);
    let state_schema_compatibility = before.map(|before| {
        crate::state_contract::compare_schemas(
            &crate::state_contract::schema_from_revision(before),
            &crate::state_contract::schema_from_revision(after),
        )
    });
    let migrations = legacy
        .changes
        .iter()
        .filter(|change| {
            matches!(
                change.safety,
                ChangeSafety::RequiresMigration | ChangeSafety::Breaking
            )
        })
        .map(|change| MigrationOperation {
            section: change.section.clone(),
            resource: change.name.clone(),
            operation: match change.kind {
                ChangeKind::Added => "add",
                ChangeKind::Modified => "alter",
                ChangeKind::Removed => "remove",
            }
            .into(),
            destructive: matches!(change.kind, ChangeKind::Removed)
                || matches!(change.safety, ChangeSafety::Breaking)
                || (change.section == "collections"
                    && matches!(legacy.compatibility, Compatibility::Breaking)),
        })
        .collect::<Vec<_>>();
    let security_changes = legacy
        .changes
        .iter()
        .filter(|change| matches!(change.safety, ChangeSafety::SecuritySensitive))
        .map(|change| SecurityChange {
            section: change.section.clone(),
            resource: change.name.clone(),
            description: format!("{:?} security-sensitive resource", change.kind),
        })
        .collect::<Vec<_>>();
    let schema_breaking = state_schema_compatibility.as_ref().is_some_and(|report| {
        matches!(
            report.classification,
            crate::state_contract::SchemaCompatibility::Destructive
                | crate::state_contract::SchemaCompatibility::Incompatible
        )
    });
    let classification = if legacy.changes.is_empty() {
        DiffClassification::Unchanged
    } else if schema_breaking
        || matches!(legacy.compatibility, Compatibility::Breaking)
        || legacy
            .changes
            .iter()
            .any(|v| matches!(v.safety, ChangeSafety::Breaking))
    {
        DiffClassification::Breaking
    } else if !security_changes.is_empty() {
        DiffClassification::SecuritySensitive
    } else if !migrations.is_empty() {
        DiffClassification::MigrationRequired
    } else if legacy
        .changes
        .iter()
        .any(|v| matches!(v.safety, ChangeSafety::RuntimeSensitive))
    {
        DiffClassification::RuntimeSensitive
    } else if legacy
        .changes
        .iter()
        .all(|v| matches!(v.kind, ChangeKind::Added))
    {
        DiffClassification::Additive
    } else {
        DiffClassification::Modified
    };
    let destructive_operations = migrations
        .iter()
        .filter(|v| v.destructive)
        .cloned()
        .collect::<Vec<_>>();
    let migration_plan = MigrationPlan {
        from_revision: legacy.from_revision.clone(),
        to_revision: legacy.to_revision.clone(),
        operations: migrations.clone(),
        destructive_operations,
        reversible: !migrations.iter().any(|v| v.destructive),
    };
    let hash_input = serde_json::json!({"application_id":after.application_id,"from_revision":legacy.from_revision,
        "to_revision":legacy.to_revision,"changes":legacy.changes,"classification":classification,
        "migrations":migrations,"security_changes":security_changes,"compatibility":legacy.compatibility,"migration_plan":migration_plan,"state_schema_compatibility":state_schema_compatibility});
    let diff_hash = format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&hash_input).map_err(|e| e.to_string())?)
    );
    Ok(ApplicationRevisionDiff {
        application_id: after.application_id.clone(),
        from_revision: legacy.from_revision,
        to_revision: legacy.to_revision,
        changes: legacy.changes,
        classification,
        migrations,
        security_changes,
        compatibility: legacy.compatibility,
        migration_plan,
        diff_hash,
        state_schema_compatibility,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store(name: &str) -> ApplicationStore {
        ApplicationStore::load(
            std::env::temp_dir().join(format!("feltdb-app-{name}-{}.json", now())),
        )
        .unwrap()
    }
    #[test]
    fn canonical_hash_ignores_named_section_order() {
        let mut a = ApplicationManifest::empty("t", "a", "App");
        a.collections = vec![
            CollectionDefinition {
                name: "z".into(),
                fields: vec![],
            },
            CollectionDefinition {
                name: "a".into(),
                fields: vec![],
            },
        ];
        let mut b = a.clone();
        b.collections.reverse();
        assert_eq!(manifest_hash(&a).unwrap(), manifest_hash(&b).unwrap());
    }
    #[test]
    fn revision_is_immutable_and_rollback_moves_pointer() {
        let s = store("lifecycle");
        let d = s.create_draft("t", "a", "App", "owner", None).unwrap();
        let r1 = s.commit("t", "a", &d.draft_id, "owner").unwrap();
        s.promote("t", "a", &r1.revision_id, "production", "owner", "initial")
            .unwrap();
        let d = s
            .create_draft("t", "a", "App", "owner", Some(&r1.revision_id))
            .unwrap();
        let r2 = s.commit("t", "a", &d.draft_id, "owner").unwrap();
        s.promote("t", "a", &r2.revision_id, "production", "owner", "upgrade")
            .unwrap();
        s.promote("t", "a", &r1.revision_id, "production", "owner", "rollback")
            .unwrap();
        assert_eq!(s.pointers("a")["production"], r1.revision_id);
        assert_eq!(s.revisions("t", "a").len(), 2);
    }
    #[test]
    fn stale_update_and_plaintext_secret_are_rejected() {
        let s = store("conflict");
        let d = s.create_draft("t", "a", "App", "owner", None).unwrap();
        let mut manifest = d.manifest.clone();
        manifest.connections.push(ConnectionBinding {
            name: "db".into(),
            connection_id: "c".into(),
            configuration: serde_json::json!({"password":"bad"}),
            secrets: vec![],
        });
        assert!(s
            .update_draft("t", "a", &d.draft_id, d.version, manifest, "owner")
            .is_err());
        let updated = s
            .update_draft(
                "t",
                "a",
                &d.draft_id,
                d.version,
                d.manifest.clone(),
                "owner",
            )
            .unwrap();
        assert!(s
            .update_draft("t", "a", &d.draft_id, d.version, updated.manifest, "owner")
            .unwrap_err()
            .starts_with("version_conflict:"));
    }
    #[test]
    fn export_import_preserves_hash() {
        let s = store("portable");
        let d = s.create_draft("t", "a", "App", "owner", None).unwrap();
        let r = s.commit("t", "a", &d.draft_id, "owner").unwrap();
        let bundle = s.export_revision("t", "a", &r.revision_id).unwrap();
        let imported = s
            .import_draft("t", "b", "Imported", "owner", &bundle)
            .unwrap();
        assert_eq!(
            manifest_hash(&imported.manifest).unwrap(),
            manifest_hash(&ApplicationManifest {
                tenant_id: "t".into(),
                application_id: "b".into(),
                ..r.manifest
            })
            .unwrap()
        );
    }
    #[test]
    fn promotion_is_compare_and_swap_and_history_is_durable() {
        let s = store("promotion-cas");
        let d = s.create_draft("t", "a", "App", "owner", None).unwrap();
        let r1 = s.commit("t", "a", &d.draft_id, "owner").unwrap();
        s.move_environment_pointer(
            "t",
            "a",
            &r1.revision_id,
            "production",
            None,
            "owner",
            "initial",
            true,
            false,
        )
        .unwrap();
        let d = s
            .create_draft("t", "a", "App", "owner", Some(&r1.revision_id))
            .unwrap();
        let r2 = s.commit("t", "a", &d.draft_id, "owner").unwrap();
        let d = s
            .create_draft("t", "a", "App", "owner", Some(&r1.revision_id))
            .unwrap();
        let r3 = s.commit("t", "a", &d.draft_id, "owner").unwrap();
        s.move_environment_pointer(
            "t",
            "a",
            &r2.revision_id,
            "production",
            Some(&r1.revision_id),
            "owner",
            "A",
            true,
            false,
        )
        .unwrap();
        let error = s
            .move_environment_pointer(
                "t",
                "a",
                &r3.revision_id,
                "production",
                Some(&r1.revision_id),
                "owner",
                "B",
                true,
                false,
            )
            .unwrap_err();
        assert!(error.starts_with("expected_revision_mismatch:"));
        assert_eq!(s.pointers("a")["production"], r2.revision_id);
        assert_eq!(s.history("t", "a", "production").len(), 2);
    }

    #[test]
    fn recovery_is_atomic_idempotent_durable_and_permanently_untrusts_source() {
        let s = store("revision-recovery");
        let d = s.create_draft("t", "a", "App", "owner", None).unwrap();
        let corrupt = s.commit("t", "a", &d.draft_id, "owner").unwrap();
        s.move_environment_pointer(
            "t",
            "a",
            &corrupt.revision_id,
            "staging",
            None,
            "owner",
            "initial",
            true,
            false,
        )
        .unwrap();
        let d = s
            .create_draft("t", "a", "App", "owner", Some(&corrupt.revision_id))
            .unwrap();
        let clean = s.commit("t", "a", &d.draft_id, "owner").unwrap();

        // Historical source corruption must not prevent recovery away from it.
        {
            let mut records = s.records.write().unwrap();
            records
                .revisions
                .iter_mut()
                .find(|value| value.revision_id == corrupt.revision_id)
                .unwrap()
                .manifest
                .metadata
                .name = "corrupted without updating its integrity hash".into();
            s.persist(&records).unwrap();
        }
        let recovered = s
            .recover_environment_pointer(
                "t",
                "a",
                "staging",
                &corrupt.revision_id,
                &clean.revision_id,
                "sherpa",
                "Replace corrupt historical revision",
                "ELEVATED",
                "recovery-1",
            )
            .unwrap();
        assert_eq!(recovered.target_revision, clean.revision_id);
        assert_eq!(
            s.recover_environment_pointer(
                "t",
                "a",
                "staging",
                &corrupt.revision_id,
                &clean.revision_id,
                "sherpa",
                "Replace corrupt historical revision",
                "ELEVATED",
                "recovery-1"
            )
            .unwrap(),
            recovered
        );
        assert!(s
            .move_environment_pointer(
                "t",
                "a",
                &corrupt.revision_id,
                "staging",
                Some(&clean.revision_id),
                "owner",
                "rollback",
                true,
                true
            )
            .unwrap_err()
            .contains("untrusted"));

        let reloaded = ApplicationStore::load(s.path.clone()).unwrap();
        assert_eq!(reloaded.pointers("a")["staging"], clean.revision_id);
        let records = reloaded.records.read().unwrap();
        assert_eq!(records.recoveries.len(), 1);
        assert!(records.untrusted_revisions["a"].contains_key(&corrupt.revision_id));
    }
    #[test]
    fn preview_remains_bound_when_production_moves() {
        let s = store("preview-bound");
        let d = s.create_draft("t", "a", "App", "owner", None).unwrap();
        let r1 = s.commit("t", "a", &d.draft_id, "owner").unwrap();
        s.move_environment_pointer(
            "t",
            "a",
            &r1.revision_id,
            "production",
            None,
            "owner",
            "initial",
            true,
            false,
        )
        .unwrap();
        let preview = s
            .create_preview(
                "t",
                "a",
                &r1.revision_id,
                "editor",
                PreviewPolicy::default(),
            )
            .unwrap();
        let d = s
            .create_draft("t", "a", "App", "owner", Some(&r1.revision_id))
            .unwrap();
        let r2 = s.commit("t", "a", &d.draft_id, "owner").unwrap();
        s.move_environment_pointer(
            "t",
            "a",
            &r2.revision_id,
            "production",
            Some(&r1.revision_id),
            "owner",
            "next",
            true,
            false,
        )
        .unwrap();
        assert_eq!(
            s.preview("t", "a", &preview.preview_id)
                .unwrap()
                .revision_id,
            r1.revision_id
        );
        assert_eq!(s.pointers("a")["production"], r2.revision_id);
    }
    #[test]
    fn destructive_change_requires_acknowledgement_and_audit_binds_diff() {
        let s = store("destructive-gate");
        let mut d = s.create_draft("t", "a", "App", "owner", None).unwrap();
        d.manifest.collections.push(CollectionDefinition {
            name: "records".into(),
            fields: vec![],
        });
        d = s
            .update_draft("t", "a", &d.draft_id, d.version, d.manifest, "owner")
            .unwrap();
        let r1 = s.commit("t", "a", &d.draft_id, "owner").unwrap();
        s.move_environment_pointer(
            "t",
            "a",
            &r1.revision_id,
            "production",
            None,
            "owner",
            "initial",
            true,
            false,
        )
        .unwrap();
        let mut d = s
            .create_draft("t", "a", "App", "owner", Some(&r1.revision_id))
            .unwrap();
        d.manifest.collections.clear();
        d.manifest.state_schema_version += 1;
        d = s
            .update_draft("t", "a", &d.draft_id, d.version, d.manifest, "owner")
            .unwrap();
        let r2 = s.commit("t", "a", &d.draft_id, "owner").unwrap();
        assert_eq!(
            s.move_environment_pointer(
                "t",
                "a",
                &r2.revision_id,
                "production",
                Some(&r1.revision_id),
                "owner",
                "remove",
                false,
                false
            )
            .unwrap_err(),
            "destructive_changes_require_approval"
        );
        let promotion = s
            .move_environment_pointer(
                "t",
                "a",
                &r2.revision_id,
                "production",
                Some(&r1.revision_id),
                "owner",
                "approved",
                true,
                false,
            )
            .unwrap();
        let audit = s.audit("t", "a");
        let event = audit.last().unwrap();
        assert_eq!(event.event, "application.revision.promoted");
        assert_eq!(
            event.diff_hash.as_deref(),
            Some(promotion.diff_hash.as_str())
        );
    }

    #[test]
    fn policy_with_valid_authenticated_subject() {
        let mut manifest = ApplicationManifest::empty("tenant", "app", "test");
        manifest.policies.push(PolicyDefinition {
            name: "TodoPolicy".into(),
            resource: "Todo".into(),
            read: Some("authenticated".into()),
            write: Some("authenticated".into()),
            capabilities: vec![],
        });

        let report = validate_manifest(&manifest, "tenant", "app", None);
        assert!(
            report.valid,
            "Policy with authenticated subject should be valid: {:?}",
            report.issues
        );
    }

    #[test]
    fn policy_with_valid_owner_subject() {
        let mut manifest = ApplicationManifest::empty("tenant", "app", "test");
        manifest.policies.push(PolicyDefinition {
            name: "ProjectPolicy".into(),
            resource: "Project".into(),
            read: Some("authenticated".into()),
            write: Some("owner".into()),
            capabilities: vec![],
        });

        let report = validate_manifest(&manifest, "tenant", "app", None);
        assert!(
            report.valid,
            "Policy with owner subject should be valid: {:?}",
            report.issues
        );
    }

    #[test]
    fn policy_with_identity_and_membership_composition_is_valid() {
        let mut manifest = ApplicationManifest::empty("tenant", "app", "test");
        manifest.policies.push(PolicyDefinition {
            name: "InvitationPolicy".into(),
            resource: "Invitation".into(),
            read: Some("self(user) | member".into()),
            write: Some("member".into()),
            capabilities: vec![],
        });

        let report = validate_manifest(&manifest, "tenant", "app", None);
        assert!(
            report.valid,
            "composed identity policy should be valid: {:?}",
            report.issues
        );
    }

    #[test]
    fn policy_with_invalid_subject_rejected() {
        let mut manifest = ApplicationManifest::empty("tenant", "app", "test");
        manifest.policies.push(PolicyDefinition {
            name: "BadPolicy".into(),
            resource: "BadCollection".into(),
            read: Some("unknown_subject".into()),
            write: None,
            capabilities: vec![],
        });

        let report = validate_manifest(&manifest, "tenant", "app", None);
        assert!(
            !report.valid,
            "Policy with unknown subject should be invalid"
        );
        assert!(
            report.issues.iter().any(|i| i.path.contains("BadPolicy")
                && i.path.contains("read")
                && i.message.contains("invalid policy subject")),
            "Should have error about invalid policy subject"
        );
    }

    #[test]
    fn policy_without_subjects_is_valid() {
        let mut manifest = ApplicationManifest::empty("tenant", "app", "test");
        manifest.policies.push(PolicyDefinition {
            name: "LegacyPolicy".into(),
            resource: "SomeCollection".into(),
            read: None,
            write: None,
            capabilities: vec!["some:capability".into()],
        });

        let report = validate_manifest(&manifest, "tenant", "app", None);
        assert!(
            report.valid,
            "Policy without subjects should be valid (backward compatible)"
        );
    }
}
