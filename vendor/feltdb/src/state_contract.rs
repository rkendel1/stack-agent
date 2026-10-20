//! Query Execution Contract
//!
//! This module defines the observable behavior of query execution in FeltDB.
//! The query execution contract has three key guarantees:
//!
//! 1. PLANNER-EXECUTOR ALIGNMENT: The access_method field in QueryPlan indicates
//!    whether an index will be used (value: "index_lookup") or a full scan (value: "full_table_scan").
//!    The executor MUST honor this decision. If plan says "index_lookup", the executor MUST
//!    use an index. Fallback to scan violates the contract.
//!
//! 2. ACTUAL MEASUREMENTS: All timing fields (planning_ms, index_lookup_ms, filtering_ms,
//!    serialization_ms, total_ms) and metrics (index_hits, index_misses) MUST be actual
//!    measurements from real execution using Instant::now(). Estimates or derived values
//!    violate the contract. The index_metrics struct MUST reflect what actually executed,
//!    not what was planned.
//!
//! 3. FALLBACK TRACKING: If a query cannot use an index (field not indexed, expression too complex),
//!    the planner sets access_method: "full_table_scan" and fallback_reason describes why.
//!    The executor then scans all rows. This is not a contract violation—it's the correct
//!    fallback path. The violation only occurs if the planner said "use index" but executor
//!    fell back to scan without updating these fields.

use crate::{
    AtomicMutation, AtomicPrecondition, FeltDb, FlowError, PreconditionFailure, RecordPrecondition,
    StoredRow,
    application::{ApplicationManifest, ApplicationRevision, FieldDefinition},
    policy_evaluation::{
        Actor, AuthorizationState, PolicyEvaluator, PolicySubject, RecordAuthorizationContext,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};

pub const STATE_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveType {
    String,
    Integer,
    Number,
    Boolean,
    Timestamp,
    Date,
    Time,
    Json,
    Uuid,
    Decimal,
    Money,
    Bigint,
    Email,
    Url,
    Phone,
    Binary,
    File,
    GeoPoint,
    Object,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldType {
    Primitive { primitive: PrimitiveType },
    Enum { values: Vec<String> },
    Reference { collection: String },
    Array { items: Box<FieldType> },
    Map { values: Box<FieldType> },
    Vector { dimensions: Option<usize> },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FieldConstraints {
    #[serde(default)]
    pub minimum: Option<f64>,
    #[serde(default)]
    pub maximum: Option<f64>,
    #[serde(default)]
    pub min_length: Option<usize>,
    #[serde(default)]
    pub max_length: Option<usize>,
    #[serde(default)]
    pub pattern: Option<String>,
}
impl Default for FieldConstraints {
    fn default() -> Self {
        Self {
            minimum: None,
            maximum: None,
            min_length: None,
            max_length: None,
            pattern: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FieldSchema {
    pub name: String,
    pub field_type: FieldType,
    #[serde(default)]
    pub nullable: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub constraints: FieldConstraints,
    #[serde(default)]
    pub computed: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchemaIndex {
    pub name: String,
    pub fields: Vec<String>,
    #[serde(default)]
    pub unique: bool,
    #[serde(default)]
    pub sparse: bool,
    #[serde(default = "one")]
    pub version: u32,
}
fn one() -> u32 {
    1
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CollectionSchema {
    pub name: String,
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub fields: Vec<FieldSchema>,
    #[serde(default)]
    pub indexes: Vec<SchemaIndex>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateSchema {
    pub contract_version: u32,
    pub schema_version: u64,
    pub application_id: String,
    pub revision_id: String,
    #[serde(default)]
    pub collections: Vec<CollectionSchema>,
}
fn parse_manifest_type(value: &str) -> FieldType {
    let value = value.trim();
    if let Some(inner) = value
        .strip_prefix("array<")
        .and_then(|v| v.strip_suffix('>'))
    {
        return FieldType::Array {
            items: Box::new(parse_manifest_type(inner)),
        };
    }
    if let Some(inner) = value.strip_prefix("map<").and_then(|v| v.strip_suffix('>')) {
        return FieldType::Map {
            values: Box::new(parse_manifest_type(inner)),
        };
    }
    if value == "vector" {
        return FieldType::Vector { dimensions: None };
    }
    if let Some(inner) = value
        .strip_prefix("vector<")
        .and_then(|v| v.strip_suffix('>'))
    {
        return FieldType::Vector {
            dimensions: inner.parse().ok(),
        };
    }
    FieldType::Primitive {
        primitive: match value {
            "int" | "integer" => PrimitiveType::Integer,
            "number" | "float" => PrimitiveType::Number,
            "bool" | "boolean" => PrimitiveType::Boolean,
            "timestamp" | "datetime" => PrimitiveType::Timestamp,
            "date" => PrimitiveType::Date,
            "time" => PrimitiveType::Time,
            "json" => PrimitiveType::Json,
            "uuid" => PrimitiveType::Uuid,
            "decimal" => PrimitiveType::Decimal,
            "money" => PrimitiveType::Money,
            "bigint" => PrimitiveType::Bigint,
            "email" => PrimitiveType::Email,
            "url" => PrimitiveType::Url,
            "phone" => PrimitiveType::Phone,
            "binary" | "bytes" => PrimitiveType::Binary,
            "file" => PrimitiveType::File,
            "geo" | "geopoint" => PrimitiveType::GeoPoint,
            "object" => PrimitiveType::Object,
            _ => PrimitiveType::String,
        },
    }
}
pub fn valid_manifest_field_type(value: &str) -> bool {
    let value = value.trim();
    if matches!(
        value,
        "text"
            | "string"
            | "int"
            | "integer"
            | "number"
            | "float"
            | "bool"
            | "boolean"
            | "timestamp"
            | "datetime"
            | "date"
            | "time"
            | "json"
            | "uuid"
            | "decimal"
            | "money"
            | "bigint"
            | "email"
            | "url"
            | "phone"
            | "binary"
            | "bytes"
            | "file"
            | "geo"
            | "geopoint"
            | "object"
            | "enum"
            | "reference"
            | "vector"
    ) {
        return true;
    }
    if value
        .strip_prefix("vector<")
        .and_then(|v| v.strip_suffix('>'))
        .is_some_and(|v| v.parse::<usize>().is_ok_and(|n| n > 0))
    {
        return true;
    }
    value
        .strip_prefix("array<")
        .and_then(|v| v.strip_suffix('>'))
        .or_else(|| value.strip_prefix("map<").and_then(|v| v.strip_suffix('>')))
        .is_some_and(valid_manifest_field_type)
}
fn manifest_field_type(field: &FieldDefinition) -> FieldType {
    if let Some(collection) = &field.reference {
        FieldType::Reference {
            collection: collection.clone(),
        }
    } else if !field.enum_values.is_empty() {
        FieldType::Enum {
            values: field.enum_values.clone(),
        }
    } else {
        parse_manifest_type(&field.field_type)
    }
}
pub fn schema_from_manifest(
    manifest: &ApplicationManifest,
    revision_id: impl Into<String>,
) -> StateSchema {
    StateSchema {
        contract_version: STATE_CONTRACT_VERSION,
        schema_version: manifest.state_schema_version,
        application_id: manifest.application_id.clone(),
        revision_id: revision_id.into(),
        collections: manifest
            .collections
            .iter()
            .map(|collection| CollectionSchema {
                name: collection.name.clone(),
                version: manifest.state_schema_version as u32,
                fields: collection
                    .fields
                    .iter()
                    .map(|field| FieldSchema {
                        name: field.name.clone(),
                        field_type: manifest_field_type(field),
                        nullable: field.nullable,
                        required: field.required,
                        default: field.default.clone(),
                        constraints: Default::default(),
                        computed: field.computed.clone(),
                    })
                    .collect(),
                indexes: manifest
                    .indexes
                    .iter()
                    .filter(|index| index.collection == collection.name)
                    .map(|index| SchemaIndex {
                        name: index.name.clone(),
                        fields: index.fields.clone(),
                        unique: index.unique,
                        sparse: index.sparse,
                        version: index.version,
                    })
                    .collect(),
            })
            .collect(),
    }
}
pub fn schema_from_revision(revision: &ApplicationRevision) -> StateSchema {
    schema_from_manifest(&revision.manifest, revision.revision_id.clone())
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchemaIssue {
    pub path: String,
    pub code: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaValidation {
    pub valid: bool,
    pub schema_hash: Option<String>,
    pub issues: Vec<SchemaIssue>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SchemaCompatibility {
    Safe,
    RequiresMigration,
    Destructive,
    Incompatible,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaChange {
    pub collection: String,
    pub field: Option<String>,
    pub classification: SchemaCompatibility,
    pub description: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaCompatibilityReport {
    pub from_version: u64,
    pub to_version: u64,
    pub classification: SchemaCompatibility,
    pub changes: Vec<SchemaChange>,
}

fn sorted_schema(mut schema: StateSchema) -> StateSchema {
    schema.collections.sort_by(|a, b| a.name.cmp(&b.name));
    for collection in &mut schema.collections {
        collection.fields.sort_by(|a, b| a.name.cmp(&b.name));
        collection.indexes.sort_by(|a, b| a.name.cmp(&b.name));
        for field in &mut collection.fields {
            if let FieldType::Enum { values } = &mut field.field_type {
                values.sort();
            }
        }
    }
    schema
}
pub fn canonical_schema_bytes(schema: &StateSchema) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&sorted_schema(schema.clone())).map_err(|e| e.to_string())
}
pub fn schema_hash(schema: &StateSchema) -> Result<String, String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(canonical_schema_bytes(schema)?)
    ))
}
pub fn validate_schema(schema: &StateSchema) -> SchemaValidation {
    let mut issues = vec![];
    if schema.contract_version != STATE_CONTRACT_VERSION {
        issues.push(SchemaIssue {
            path: "contract_version".into(),
            code: "UNSUPPORTED_CONTRACT".into(),
            message: "unsupported state contract version".into(),
        });
    }
    let mut collections = BTreeSet::new();
    for collection in &schema.collections {
        if collection.name.trim().is_empty() || !collections.insert(collection.name.clone()) {
            issues.push(SchemaIssue {
                path: format!("collections.{}", collection.name),
                code: "INVALID_COLLECTION".into(),
                message: "collection names must be non-empty and unique".into(),
            });
        }
        let mut fields = BTreeSet::new();
        for field in &collection.fields {
            if field.name.trim().is_empty() || !fields.insert(field.name.clone()) {
                issues.push(SchemaIssue {
                    path: format!("collections.{}.fields.{}", collection.name, field.name),
                    code: "INVALID_FIELD".into(),
                    message: "field names must be non-empty and unique".into(),
                });
            }
            if field.required && field.nullable {
                issues.push(SchemaIssue {
                    path: format!("collections.{}.fields.{}", collection.name, field.name),
                    code: "INVALID_NULLABILITY".into(),
                    message: "required fields cannot be nullable".into(),
                });
            }
            if field.computed.is_some() && field.default.is_some() {
                issues.push(SchemaIssue {
                    path: format!("collections.{}.fields.{}", collection.name, field.name),
                    code: "INVALID_COMPUTED_DEFAULT".into(),
                    message: "computed fields cannot declare defaults".into(),
                });
            }
            if let Some(expression) = &field.computed {
                let sources = expression
                    .strip_prefix("copy:")
                    .map(|v| vec![v])
                    .or_else(|| {
                        expression
                            .strip_prefix("concat:")
                            .map(|v| v.split(',').collect())
                    })
                    .unwrap_or_default();
                if sources.is_empty()
                    || sources.iter().any(|source| {
                        !collection
                            .fields
                            .iter()
                            .any(|candidate| candidate.name == source.trim())
                    })
                {
                    issues.push(SchemaIssue{path:format!("collections.{}.fields.{}.computed",collection.name,field.name),code:"INVALID_COMPUTED_FIELD".into(),message:"computed expressions must use copy:<field> or concat:<field>,<field> with known fields".into()});
                }
            }
            if field
                .default
                .as_ref()
                .is_some_and(|value| !valid_type(value, field))
            {
                issues.push(SchemaIssue {
                    path: format!(
                        "collections.{}.fields.{}.default",
                        collection.name, field.name
                    ),
                    code: "INVALID_DEFAULT".into(),
                    message: "default does not satisfy the field type".into(),
                });
            }
            if let FieldType::Enum { values } = &field.field_type {
                let unique = values.iter().collect::<BTreeSet<_>>();
                if values.is_empty() || unique.len() != values.len() {
                    issues.push(SchemaIssue {
                        path: format!("collections.{}.fields.{}", collection.name, field.name),
                        code: "INVALID_ENUM".into(),
                        message: "enum values must be non-empty and unique".into(),
                    });
                }
            }
        }
        for index in &collection.indexes {
            if index.fields.is_empty() || index.fields.iter().any(|f| !fields.contains(f)) {
                issues.push(SchemaIssue {
                    path: format!("collections.{}.indexes.{}", collection.name, index.name),
                    code: "INVALID_INDEX".into(),
                    message: "indexes must reference existing fields".into(),
                });
            }
        }
    }
    for collection in &schema.collections {
        for field in &collection.fields {
            if let FieldType::Reference { collection: target } = &field.field_type {
                if !collections.contains(target) {
                    issues.push(SchemaIssue {
                        path: format!("collections.{}.fields.{}", collection.name, field.name),
                        code: "INVALID_REFERENCE".into(),
                        message: format!("unknown reference target {target}"),
                    });
                }
            }
        }
    }
    let valid = issues.is_empty();
    SchemaValidation {
        valid,
        schema_hash: valid.then(|| schema_hash(schema).ok()).flatten(),
        issues,
    }
}
pub fn compare_schemas(before: &StateSchema, after: &StateSchema) -> SchemaCompatibilityReport {
    let mut changes = vec![];
    let old: BTreeMap<_, _> = before.collections.iter().map(|v| (&v.name, v)).collect();
    let new: BTreeMap<_, _> = after.collections.iter().map(|v| (&v.name, v)).collect();
    for (name, collection) in &new {
        if !old.contains_key(name) {
            changes.push(SchemaChange {
                collection: (*name).clone(),
                field: None,
                classification: SchemaCompatibility::Safe,
                description: "collection added".into(),
            });
            continue;
        }
        let previous = old[name];
        let old_fields: BTreeMap<_, _> = previous.fields.iter().map(|v| (&v.name, v)).collect();
        let new_fields: BTreeMap<_, _> = collection.fields.iter().map(|v| (&v.name, v)).collect();
        for (field, current) in &new_fields {
            match old_fields.get(field) {
                None => changes.push(SchemaChange {
                    collection: (*name).clone(),
                    field: Some((*field).clone()),
                    classification: if current.required && current.default.is_none() {
                        SchemaCompatibility::Destructive
                    } else {
                        SchemaCompatibility::Safe
                    },
                    description: "field added".into(),
                }),
                Some(prior) if prior.field_type != current.field_type => {
                    let classification = match (&prior.field_type, &current.field_type) {
                        (FieldType::Enum { values: a }, FieldType::Enum { values: b })
                            if a.iter().all(|v| b.contains(v)) =>
                        {
                            SchemaCompatibility::Safe
                        }
                        (FieldType::Enum { .. }, FieldType::Enum { .. }) => {
                            SchemaCompatibility::Destructive
                        }
                        _ => SchemaCompatibility::Incompatible,
                    };
                    changes.push(SchemaChange {
                        collection: (*name).clone(),
                        field: Some((*field).clone()),
                        classification,
                        description: "field type changed".into(),
                    });
                }
                Some(prior)
                    if prior.required != current.required || prior.nullable != current.nullable =>
                {
                    changes.push(SchemaChange {
                        collection: (*name).clone(),
                        field: Some((*field).clone()),
                        classification: if current.required {
                            SchemaCompatibility::Destructive
                        } else {
                            SchemaCompatibility::Safe
                        },
                        description: "field requirement changed".into(),
                    })
                }
                _ => {}
            }
        }
        for field in old_fields.keys().filter(|f| !new_fields.contains_key(*f)) {
            changes.push(SchemaChange {
                collection: (*name).clone(),
                field: Some((*field).clone()),
                classification: SchemaCompatibility::Destructive,
                description: "field removed".into(),
            });
        }
    }
    for name in old.keys().filter(|n| !new.contains_key(*n)) {
        changes.push(SchemaChange {
            collection: (*name).clone(),
            field: None,
            classification: SchemaCompatibility::Destructive,
            description: "collection removed".into(),
        });
    }
    let classification = changes
        .iter()
        .map(|v| v.classification.clone())
        .max()
        .unwrap_or(SchemaCompatibility::Safe);
    SchemaCompatibilityReport {
        from_version: before.schema_version,
        to_version: after.schema_version,
        classification,
        changes,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizationContext {
    pub subject: String,
    pub tenant_id: String,
    pub application_id: String,
    pub revision_id: String,
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
    #[serde(default)]
    pub readable_collections: BTreeSet<String>,
    #[serde(default)]
    pub writable_collections: BTreeSet<String>,
    #[serde(default)]
    pub field_projections: BTreeMap<String, BTreeSet<String>>,
}
impl AuthorizationContext {
    fn can(&self, cap: &str) -> bool {
        self.capabilities.contains("*") || self.capabilities.contains(cap)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadContext {
    pub application_id: String,
    pub revision_id: String,
    pub schema_version: u64,
    pub state_version: u64,
    pub causal_cursor: BTreeMap<String, u64>,
    pub state_namespace: String,
    pub authorization: AuthorizationContext,
    #[serde(skip, default)]
    snapshot_rows: Vec<StoredRow>,
    #[serde(skip, default)]
    live_query: bool,
}
pub fn begin_read(
    db: &FeltDb,
    schema: &StateSchema,
    state_namespace: &str,
    authorization: AuthorizationContext,
) -> Result<ReadContext, StateFailure> {
    begin_read_internal(db, schema, state_namespace, authorization, true)
}

/// Begin a query read without cloning the namespace up front. Query execution
/// uses authoritative collection/index access and loads a namespace snapshot
/// only when policy evaluation or reference expansion requires it.
pub fn begin_query_read(
    db: &FeltDb,
    schema: &StateSchema,
    state_namespace: &str,
    authorization: AuthorizationContext,
) -> Result<ReadContext, StateFailure> {
    build_read_context(db, schema, state_namespace, authorization, Vec::new(), true)
}

fn begin_read_internal(
    db: &FeltDb,
    schema: &StateSchema,
    state_namespace: &str,
    authorization: AuthorizationContext,
    capture_snapshot: bool,
) -> Result<ReadContext, StateFailure> {
    if authorization.application_id != schema.application_id
        || authorization.revision_id != schema.revision_id
    {
        return Err(StateFailure::new(
            "SCHEMA_MISMATCH",
            "authorization context does not match schema",
        ));
    }
    let snapshot_rows = if capture_snapshot {
        db.state_rows_for_namespace(state_namespace)
            .map_err(StateFailure::storage)?
    } else {
        Vec::new()
    };
    build_read_context(
        db,
        schema,
        state_namespace,
        authorization,
        snapshot_rows,
        !capture_snapshot,
    )
}

fn build_read_context(
    db: &FeltDb,
    schema: &StateSchema,
    state_namespace: &str,
    authorization: AuthorizationContext,
    snapshot_rows: Vec<StoredRow>,
    live_query: bool,
) -> Result<ReadContext, StateFailure> {
    Ok(ReadContext {
        application_id: schema.application_id.clone(),
        revision_id: schema.revision_id.clone(),
        schema_version: schema.schema_version,
        state_version: db.sequence().map_err(StateFailure::storage)?,
        causal_cursor: db
            .operation_versions()
            .map_err(StateFailure::storage)?
            .into_iter()
            .collect(),
        state_namespace: state_namespace.into(),
        authorization,
        snapshot_rows,
        live_query,
    })
}

fn namespace_capability(state_namespace: &str, collection: &str) -> String {
    format!("{state_namespace}:{collection}")
}

fn membership_collection_name(schema: &StateSchema) -> Option<String> {
    AuthorizationState::new(Arc::new(schema.clone()), "", Arc::new(Vec::new()))
        .find_membership_collection()
}

fn member_policy_snapshot_rows(
    db: &FeltDb,
    schema: &StateSchema,
    state_namespace: &str,
) -> Result<Vec<StoredRow>, StateFailure> {
    let Some(collection) = membership_collection_name(schema) else {
        return Ok(Vec::new());
    };
    db.list_collection(&namespace_capability(state_namespace, &collection))
        .map_err(StateFailure::storage)
}

fn load_reference_rows(
    db: &FeltDb,
    state_namespace: &str,
    values: &[Value],
    references: &[QueryReference],
) -> Result<BTreeMap<(String, String), Value>, StateFailure> {
    let mut rows = BTreeMap::new();
    let mut requested = BTreeMap::<String, BTreeSet<String>>::new();
    for reference in references {
        for id in values
            .iter()
            .filter_map(|value| value.get(&reference.field).and_then(Value::as_str))
        {
            requested
                .entry(reference.collection.clone())
                .or_default()
                .insert(id.to_string());
        }
    }
    for (collection, ids) in requested {
        let capability = namespace_capability(state_namespace, &collection);
        let loaded = db
            .get_collection_records(&capability, ids.iter().map(String::as_str))
            .map_err(StateFailure::storage)?;
        let present = loaded
            .into_iter()
            .map(|row| (row.key, row.value))
            .collect::<BTreeMap<_, _>>();
        for id in ids {
            rows.insert(
                (collection.clone(), id.clone()),
                present.get(&id).cloned().unwrap_or(Value::Null),
            );
        }
    }
    Ok(rows)
}

fn transaction_snapshot_rows(
    db: &FeltDb,
    schema: &StateSchema,
    request: &TransactionRequest,
    uniform_write_policy: Option<&PolicySubject>,
    collection_write_policies: Option<&BTreeMap<String, PolicySubject>>,
) -> Result<Vec<StoredRow>, StateFailure> {
    let namespace = request
        .state_namespace
        .as_deref()
        .unwrap_or(&request.application_id);
    let mut full_collections = BTreeSet::new();
    let mut records = BTreeSet::new();
    for condition in &request.preconditions {
        if let TransactionPrecondition::CollectionVersion { collection, .. } = condition {
            full_collections.insert(collection.clone());
        }
    }
    let membership_collection = membership_collection_name(schema);
    for operation in &request.operations {
        let substrate = is_substrate_collection(&operation.collection);
        records.insert((operation.collection.clone(), operation.id.clone()));
        let write_policy = collection_write_policies
            .and_then(|policies| policies.get(&operation.collection))
            .or(uniform_write_policy);
        if matches!(write_policy, Some(&PolicySubject::Member)) {
            if let Some(collection) = &membership_collection {
                full_collections.insert(collection.clone());
            }
        }
        if substrate || matches!(operation.kind, TransactionOperationKind::Delete) {
            continue;
        }
        let definition = collection(schema, &operation.collection)?;
        if definition.indexes.iter().any(|index| index.unique) {
            full_collections.insert(operation.collection.clone());
        }
    }

    let mut snapshot = Vec::new();
    let mut materialized = BTreeSet::new();
    for collection in &full_collections {
        snapshot.extend(
            db.list_collection(&namespace_capability(namespace, collection))
                .map_err(StateFailure::storage)?,
        );
        materialized.insert(collection.clone());
    }
    for (collection, key) in records {
        if materialized.contains(&collection) {
            continue;
        }
        if let Some(row) = db
            .get_collection_record(&namespace_capability(namespace, &collection), &key)
            .map_err(StateFailure::storage)?
        {
            snapshot.push(row);
        }
    }
    Ok(snapshot)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operator", rename_all = "camelCase")]
pub enum QueryFilter {
    Eq { field: String, value: Value },
    Neq { field: String, value: Value },
    Lt { field: String, value: Value },
    Lte { field: String, value: Value },
    Gt { field: String, value: Value },
    Gte { field: String, value: Value },
    In { field: String, values: Vec<Value> },
    Contains { field: String, value: Value },
    StartsWith { field: String, value: String },
    EndsWith { field: String, value: String },
    Exists { field: String, exists: bool },
    And { filters: Vec<QueryFilter> },
    Or { filters: Vec<QueryFilter> },
    Not { filter: Box<QueryFilter> },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Asc,
    Desc,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryOrder {
    pub field: String,
    pub direction: SortDirection,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateFunction {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateDefinition {
    pub name: String,
    pub function: AggregateFunction,
    #[serde(default)]
    pub field: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryReference {
    pub field: String,
    pub collection: String,
    #[serde(default)]
    pub projection: Vec<String>,
    pub as_name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalQuery {
    pub collection: String,
    #[serde(default)]
    pub filter: Option<QueryFilter>,
    #[serde(default)]
    pub order_by: Vec<QueryOrder>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub projection: Vec<String>,
    #[serde(default)]
    pub group_by: Vec<String>,
    #[serde(default)]
    pub aggregates: Vec<AggregateDefinition>,
    #[serde(default)]
    pub references: Vec<QueryReference>,
    /// A semantic clause is a recognized query concept that this surface does
    /// not compose. It is carried through deserialization so `execute_query`
    /// can refuse it with a structured failure instead of an unknown key
    /// silently disappearing; see `docs/reference/semantic-query.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<Value>,
}
/// Actual execution timing measurements for a query.
/// All fields are MEASURED using Instant::now(), not estimated or derived.
/// Each field captures time for its phase: planning, index lookup, filtering, serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTiming {
    /// Time spent in query planning (determining access method)
    #[serde(default)]
    pub planning_ms: Option<f64>,
    /// Time spent in index lookup phase (pre-filtering via indexed field)
    #[serde(default)]
    pub index_lookup_ms: Option<f64>,
    /// Time spent evaluating full query filter predicates
    #[serde(default)]
    pub filtering_ms: Option<f64>,
    /// Time spent serializing results to JSON
    #[serde(default)]
    pub serialization_ms: Option<f64>,
    /// Total end-to-end execution time
    #[serde(default)]
    pub total_ms: Option<f64>,
}

/// Actual index usage metrics from query execution.
/// These fields MUST reflect what the executor actually did, not what was planned.
/// If the planner selected an index but executor fell back to scan, these metrics MUST show
/// fallback_used=true and fallback_reason must explain why, else the contract is violated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMetrics {
    /// Which index was used, if any (e.g., "status_idx"). None if no index used.
    #[serde(default)]
    pub index_used: Option<String>,
    /// Number of rows returned by index pre-filter phase (actual count, not estimate)
    #[serde(default)]
    pub index_hits: usize,
    /// Number of rows filtered out by index pre-filter (actual count, not estimate)
    #[serde(default)]
    pub index_misses: usize,
    /// True if executor fell back to full scan (violates contract if planner said "index_lookup")
    #[serde(default)]
    pub fallback_used: bool,
    /// Reason for fallback (e.g., "index_not_found", "unsupported_expression")
    #[serde(default)]
    pub fallback_reason: Option<String>,
}

impl ExecutionTiming {
    pub fn new() -> Self {
        ExecutionTiming {
            planning_ms: None,
            index_lookup_ms: None,
            filtering_ms: None,
            serialization_ms: None,
            total_ms: None,
        }
    }
}

impl Default for ExecutionTiming {
    fn default() -> Self {
        Self::new()
    }
}

/// Query execution plan with CONTRACT guarantees.
/// The planner makes a decision (access_method: "index_lookup" or "full_table_scan").
/// The executor MUST honor this decision and update index_metrics and timing to prove it did.
/// Violation: planner says "index_lookup" but executor provides metrics showing full_table_scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryPlan {
    pub implementation: String,
    pub collection: String,
    pub index: Option<String>,
    pub scan_fallback: bool,
    /// ACCESS METHOD DECISION: "index_lookup" or "full_table_scan" (planner's choice)
    /// Executor MUST follow this decision and update index_metrics to prove it did.
    #[serde(default)]
    pub access_method: Option<String>,
    #[serde(default)]
    pub estimated_rows: Option<usize>,
    /// Reason for fallback to scan (only set if access_method="full_table_scan")
    #[serde(default)]
    pub fallback_reason: Option<String>,
    /// Number of rows examined by executor (actual, not estimate)
    #[serde(default)]
    pub actual_rows_scanned: Option<usize>,
    /// Number of rows returned after all filtering (actual, not estimate)
    #[serde(default)]
    pub actual_rows_returned: Option<usize>,
    /// Records that matched the complete scoped predicate before pagination.
    #[serde(default)]
    pub matching_records: Option<usize>,
    /// Candidates outside the requested equality scope that were examined.
    #[serde(default)]
    pub unrelated_records_examined: Option<usize>,
    /// Zero-based position within the scoped, deterministically ordered set.
    #[serde(default)]
    pub pagination_position: Option<usize>,
    /// Total execution time in milliseconds (actual measurement)
    #[serde(default)]
    pub execution_ms: Option<f64>,
    /// Detailed timing breakdown (planning, index_lookup, filtering, serialization)
    #[serde(default)]
    pub timing: Option<ExecutionTiming>,
    /// Proof that executor followed planner's access_method decision
    #[serde(default)]
    pub index_metrics: Option<IndexMetrics>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OperationType {
    Read,
    Query,
    Insert,
    Update,
    Delete,
    Transaction,
    Sync,
    Replication,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub query_id: String,
    pub query_hash: String,
    pub application_revision: String,
    pub schema_version: u64,
    pub state_version: u64,
    pub causal_cursor: BTreeMap<String, u64>,
    pub records: Vec<Value>,
    pub aggregates: Vec<Value>,
    pub next_cursor: Option<String>,
    pub plan: QueryPlan,
    #[serde(default)]
    pub operation_type: Option<OperationType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateFailure {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub expected: Option<Value>,
    #[serde(default)]
    pub actual: Option<Value>,
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub transaction: Option<String>,
    #[serde(default)]
    pub causal_position: Option<u64>,
    /// The structured predicate failure, when the refusal was a record
    /// precondition. Named `failure` because that is what the direct
    /// authority's refused transaction already calls it: one refusal, one
    /// shape, whichever endpoint answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Value>,
}
impl StateFailure {
    fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            expected: None,
            actual: None,
            resource: None,
            transaction: None,
            causal_position: None,
            failure: None,
        }
    }
    fn storage(error: impl std::fmt::Display) -> Self {
        Self::new("STORAGE_FAILURE", &error.to_string())
    }
}

fn collection<'a>(
    schema: &'a StateSchema,
    name: &str,
) -> Result<&'a CollectionSchema, StateFailure> {
    schema
        .collections
        .iter()
        .find(|v| v.name == name)
        .ok_or_else(|| StateFailure::new("UNKNOWN_COLLECTION", name))
}
fn filter_fields(filter: &QueryFilter, out: &mut BTreeSet<String>) {
    match filter {
        QueryFilter::Eq { field, .. }
        | QueryFilter::Neq { field, .. }
        | QueryFilter::Lt { field, .. }
        | QueryFilter::Lte { field, .. }
        | QueryFilter::Gt { field, .. }
        | QueryFilter::Gte { field, .. }
        | QueryFilter::In { field, .. }
        | QueryFilter::Contains { field, .. }
        | QueryFilter::StartsWith { field, .. }
        | QueryFilter::EndsWith { field, .. }
        | QueryFilter::Exists { field, .. } => {
            out.insert(field.clone());
        }
        QueryFilter::And { filters } | QueryFilter::Or { filters } => {
            for f in filters {
                filter_fields(f, out)
            }
        }
        QueryFilter::Not { filter } => filter_fields(filter, out),
    }
}
fn compare_value(left: &Value, right: &Value) -> Option<Ordering> {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64()?.partial_cmp(&b.as_f64()?),
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        _ => None,
    }
}
fn matches(value: &Value, filter: &QueryFilter) -> bool {
    let get = |field: &str| value.get(field);
    match filter {
        QueryFilter::Eq { field, value: v } => get(field) == Some(v),
        QueryFilter::Neq { field, value: v } => get(field) != Some(v),
        QueryFilter::Lt { field, value: v } => {
            get(field).and_then(|x| compare_value(x, v)) == Some(Ordering::Less)
        }
        QueryFilter::Lte { field, value: v } => matches!(
            get(field).and_then(|x| compare_value(x, v)),
            Some(Ordering::Less | Ordering::Equal)
        ),
        QueryFilter::Gt { field, value: v } => {
            get(field).and_then(|x| compare_value(x, v)) == Some(Ordering::Greater)
        }
        QueryFilter::Gte { field, value: v } => matches!(
            get(field).and_then(|x| compare_value(x, v)),
            Some(Ordering::Greater | Ordering::Equal)
        ),
        QueryFilter::In { field, values } => get(field).is_some_and(|x| values.contains(x)),
        QueryFilter::Contains { field, value: v } => get(field).is_some_and(|x| {
            x.as_str()
                .zip(v.as_str())
                .is_some_and(|(a, b)| a.contains(b))
                || x.as_array().is_some_and(|a| a.contains(v))
        }),
        QueryFilter::StartsWith { field, value: v } => get(field)
            .and_then(Value::as_str)
            .is_some_and(|x| x.starts_with(v)),
        QueryFilter::EndsWith { field, value: v } => get(field)
            .and_then(Value::as_str)
            .is_some_and(|x| x.ends_with(v)),
        QueryFilter::Exists { field, exists } => get(field).is_some() == *exists,
        QueryFilter::And { filters } => filters.iter().all(|f| matches(value, f)),
        QueryFilter::Or { filters } => filters.iter().any(|f| matches(value, f)),
        QueryFilter::Not { filter } => !matches(value, filter),
    }
}
fn projected(value: &Value, fields: &[String]) -> Value {
    if fields.is_empty() {
        return value.clone();
    }
    let mut out = Map::new();
    for field in fields {
        if let Some(v) = value.get(field) {
            out.insert(field.clone(), v.clone());
        }
    }
    Value::Object(out)
}
pub fn query_hash(query: &CanonicalQuery) -> Result<String, StateFailure> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(query).map_err(StateFailure::storage)?)
    ))
}

/// Hash only the immutable query shape. Pagination state is deliberately
/// excluded so every page remains bound to the same collection, scope,
/// ordering, projection, and limit.
fn pagination_query_hash(query: &CanonicalQuery) -> Result<String, StateFailure> {
    let mut shape = query.clone();
    shape.cursor = None;
    shape.offset = 0;
    query_hash(&shape)
}

fn encode_query_cursor(query_hash: &str, position: usize) -> String {
    format!("feltdb-query-v1.{position}.{}", &query_hash[7..])
}

fn decode_query_cursor(cursor: &str, expected_hash: &str) -> Result<usize, StateFailure> {
    let mut parts = cursor.split('.');
    let version = parts.next();
    let position = parts.next();
    let hash = parts.next();
    if version != Some("feltdb-query-v1")
        || parts.next().is_some()
        || hash != Some(&expected_hash[7..])
    {
        return Err(StateFailure::new(
            "INVALID_CURSOR",
            "cursor does not match the query scope, collection, ordering, or limit",
        ));
    }
    position
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| StateFailure::new("INVALID_CURSOR", "cursor is malformed"))
}

fn query_value(row: &crate::StoredRow) -> Value {
    let mut value = row.value.clone();
    if let Some(object) = value.as_object_mut() {
        object.insert("_id".into(), Value::String(row.key.clone()));
        object.insert(
            "_version".into(),
            Value::from(
                row.operation
                    .as_ref()
                    .map(|value| value.sequence)
                    .unwrap_or(0),
            ),
        );
    }
    value
}
fn extract_equality_predicate(filter: &QueryFilter) -> Option<(String, Value)> {
    match filter {
        QueryFilter::Eq { field, value } => Some((field.clone(), value.clone())),
        QueryFilter::And { filters } => filters.iter().find_map(extract_equality_predicate),
        _ => None,
    }
}
fn extract_indexed_equality(
    filter: &QueryFilter,
    definition: &CollectionSchema,
) -> Option<(String, Value)> {
    match filter {
        QueryFilter::Eq { field, value } if can_use_index(definition, field) => {
            Some((field.clone(), value.clone()))
        }
        QueryFilter::And { filters } => filters
            .iter()
            .find_map(|filter| extract_indexed_equality(filter, definition)),
        _ => None,
    }
}

fn contains_managed_scope(filter: &QueryFilter) -> bool {
    match filter {
        QueryFilter::Eq { field, .. } => matches!(
            field.as_str(),
            "tenant_id" | "tenantId" | "organization_id" | "organizationId" | "scope_prefix"
        ),
        QueryFilter::And { filters } => filters.iter().any(contains_managed_scope),
        _ => false,
    }
}

fn extract_range_predicate(filter: &QueryFilter) -> Option<(String, Option<Value>, Option<Value>)> {
    match filter {
        QueryFilter::Gte { field, value } => Some((field.clone(), Some(value.clone()), None)),
        QueryFilter::Lte { field, value } => Some((field.clone(), None, Some(value.clone()))),
        QueryFilter::Gt { field, value } => Some((field.clone(), Some(value.clone()), None)),
        QueryFilter::Lt { field, value } => Some((field.clone(), None, Some(value.clone()))),
        _ => None,
    }
}

fn can_use_index(definition: &CollectionSchema, field: &str) -> bool {
    definition
        .indexes
        .iter()
        .any(|idx| idx.fields.contains(&field.to_string()))
}

fn can_use_index_for_range(definition: &CollectionSchema, field: &str) -> bool {
    definition
        .indexes
        .iter()
        .any(|idx| idx.fields.contains(&field.to_string()))
}

pub fn explain_query(
    schema: &StateSchema,
    query: &CanonicalQuery,
) -> Result<QueryPlan, StateFailure> {
    let definition = collection(schema, &query.collection)?;

    let mut predicate_fields = BTreeSet::new();
    if let Some(filter) = &query.filter {
        filter_fields(filter, &mut predicate_fields)
    }

    let (used_index, index_name, _index_predicate_field_and_value) =
        if let Some(filter) = &query.filter {
            if let Some((field, _value)) = extract_equality_predicate(filter) {
                if can_use_index(definition, &field) {
                    let index_name = definition
                        .indexes
                        .iter()
                        .find(|idx| idx.fields.contains(&field))
                        .map(|idx| idx.name.clone());

                    if index_name.is_some() {
                        (true, index_name, Some((field, Value::Null)))
                    } else {
                        (false, None, None)
                    }
                } else {
                    (false, None, None)
                }
            } else {
                (false, None, None)
            }
        } else {
            (false, None, None)
        };

    let (execution_strategy, scan_fallback_flag, access_method) = if used_index {
        ("index_lookup".into(), false, Some("index".to_string()))
    } else {
        (
            "full_table_scan".into(),
            true,
            Some("sequential_scan".to_string()),
        )
    };

    Ok(QueryPlan {
        implementation: execution_strategy,
        collection: query.collection.clone(),
        index: index_name.clone(),
        scan_fallback: scan_fallback_flag,
        access_method,
        estimated_rows: if used_index { Some(1) } else { None },
        fallback_reason: if scan_fallback_flag {
            Some("no_applicable_index".to_string())
        } else {
            None
        },
        actual_rows_scanned: None,
        actual_rows_returned: None,
        matching_records: None,
        unrelated_records_examined: None,
        pagination_position: None,
        execution_ms: None,
        timing: None,
        index_metrics: None,
    })
}

pub fn execute_query(
    _db: &FeltDb,
    schema: &StateSchema,
    context: &ReadContext,
    query: &CanonicalQuery,
    read_policy: Option<PolicySubject>,
) -> Result<QueryResult, StateFailure> {
    let query_start = Instant::now();

    // Fail closed on semantic intent before any deterministic work. Every query
    // surface must either implement a recognized semantic clause per the
    // semantic-query contract or refuse it; this surface refuses, and it never
    // answers the deterministic part as though the clause had been absent.
    if let Some(clause) = &query.semantic {
        return Err(if clause.is_object() {
            StateFailure::new(
                "SEMANTIC_UNSUPPORTED",
                "canonical queries recognize the semantic clause but do not compose semantic decisions; use the bounded authority query",
            )
        } else {
            StateFailure::new("INVALID_QUERY", "semantic clause must be an object")
        });
    }
    if query.limit == Some(0) || query.limit.is_some_and(|limit| limit > 1000) {
        return Err(StateFailure::new(
            "INVALID_QUERY",
            "limit must be between 1 and 1000",
        ));
    }
    if context.schema_version != schema.schema_version || context.revision_id != schema.revision_id
    {
        return Err(StateFailure::new(
            "SCHEMA_MISMATCH",
            "read context does not match schema",
        ));
    }
    // Substrate collections carry no application schema: their records are
    // validated by the substrate that writes them, and they are read only
    // under `composition:read`.
    let substrate = is_substrate_collection(&query.collection);
    let substrate_definition;
    let definition = if substrate {
        if !query.references.is_empty() {
            return Err(StateFailure::new(
                "INVALID_QUERY",
                "substrate collections do not expand references",
            ));
        }
        substrate_definition = CollectionSchema {
            name: query.collection.clone(),
            version: 1,
            fields: vec![],
            indexes: vec![],
        };
        &substrate_definition
    } else {
        collection(schema, &query.collection)?
    };
    let readable = if substrate {
        context.authorization.can("composition:read")
    } else {
        context.authorization.can("state:read")
            && (context.authorization.readable_collections.is_empty()
                || context
                    .authorization
                    .readable_collections
                    .contains(&query.collection))
    };
    if !readable {
        return Err(StateFailure::new(
            "AUTHORIZATION_DENIED",
            "collection is not readable",
        ));
    }
    let available: BTreeSet<_> = definition
        .fields
        .iter()
        .map(|v| v.name.clone())
        .chain(query.references.iter().map(|v| v.as_name.clone()))
        .chain(["_id".into(), "_version".into()])
        .collect();
    let mut requested = query.projection.iter().cloned().collect::<BTreeSet<_>>();
    let mut predicate_fields = BTreeSet::new();
    if let Some(filter) = &query.filter {
        filter_fields(filter, &mut requested);
        filter_fields(filter, &mut predicate_fields)
    }
    for order in &query.order_by {
        requested.insert(order.field.clone());
    }
    for group in &query.group_by {
        requested.insert(group.clone());
    }
    for agg in &query.aggregates {
        if let Some(field) = &agg.field {
            requested.insert(field.clone());
        }
    }
    for reference in &query.references {
        requested.insert(reference.field.clone());
    }
    if !substrate && requested.iter().any(|f| !available.contains(f)) {
        return Err(StateFailure::new(
            "UNKNOWN_FIELD",
            "query references an unknown field",
        ));
    }
    if let Some(allowed) = context
        .authorization
        .field_projections
        .get(&query.collection)
    {
        if requested
            .iter()
            .any(|f| !allowed.contains(f) && f != "_id" && f != "_version")
        {
            return Err(StateFailure::new(
                "UNAUTHORIZED_PROJECTION",
                "query references a protected field",
            ));
        }
    }
    let effective_projection = context
        .authorization
        .field_projections
        .get(&query.collection)
        .filter(|_| query.projection.is_empty())
        .map(|allowed| allowed.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_else(|| query.projection.clone());
    let capability = format!("{}:{}", context.state_namespace, query.collection);
    for reference in &query.references {
        let target = collection(schema, &reference.collection)?;
        if !context.authorization.readable_collections.is_empty()
            && !context
                .authorization
                .readable_collections
                .contains(&reference.collection)
        {
            return Err(StateFailure::new(
                "AUTHORIZATION_DENIED",
                "referenced collection is not readable",
            ));
        }
        if reference
            .projection
            .iter()
            .any(|field| !target.fields.iter().any(|v| &v.name == field))
        {
            return Err(StateFailure::new(
                "UNKNOWN_FIELD",
                "reference projection contains an unknown field",
            ));
        }
        if let Some(allowed) = context
            .authorization
            .field_projections
            .get(&reference.collection)
        {
            if reference
                .projection
                .iter()
                .any(|field| !allowed.contains(field))
            {
                return Err(StateFailure::new(
                    "UNAUTHORIZED_PROJECTION",
                    "reference projection contains a protected field",
                ));
            }
        }
    }

    let policy_snapshot = if matches!(read_policy, Some(PolicySubject::Member)) {
        member_policy_snapshot_rows(_db, schema, &context.state_namespace)?
    } else {
        Vec::new()
    };

    // Detect unfiltered COUNT(*) for maintained cardinality optimization
    let is_unfiltered_count = query.filter.is_none()
        && query.group_by.is_empty()
        && query
            .aggregates
            .iter()
            .any(|agg| matches!(agg.function, AggregateFunction::Count));

    // If unfiltered COUNT(*), use maintained cardinality instead of scanning rows
    // NOTE: Skip this optimization if record-level authorization is in effect,
    // since maintained cardinality doesn't account for per-record filtering
    if is_unfiltered_count && read_policy.is_none() {
        let cardinality = _db.collection_cardinality(&capability).unwrap_or(0);

        let count_result = serde_json::json!(cardinality);
        let query_hash = query_hash(query)?;
        let execution_ms = query_start.elapsed().as_secs_f64() * 1000.0;

        return Ok(QueryResult {
            query_id: format!("query:{}:{}", context.state_version, &query_hash[7..23]),
            query_hash,
            application_revision: context.revision_id.clone(),
            schema_version: context.schema_version,
            state_version: context.state_version,
            causal_cursor: std::collections::BTreeMap::new(),
            records: vec![],
            aggregates: vec![count_result],
            next_cursor: None,
            plan: QueryPlan {
                implementation: "maintained_count".into(),
                collection: query.collection.clone(),
                index: None,
                scan_fallback: false,
                access_method: Some("maintained_count".to_string()),
                estimated_rows: Some(1),
                fallback_reason: None,
                actual_rows_scanned: Some(0),
                actual_rows_returned: Some(1),
                matching_records: Some(1),
                unrelated_records_examined: Some(0),
                pagination_position: Some(0),
                execution_ms: Some(execution_ms),
                timing: Some(ExecutionTiming {
                    planning_ms: None,
                    index_lookup_ms: None,
                    filtering_ms: None,
                    serialization_ms: None,
                    total_ms: Some(execution_ms),
                }),
                index_metrics: Some(IndexMetrics {
                    index_used: None,
                    index_hits: 0,
                    index_misses: 0,
                    fallback_used: false,
                    fallback_reason: None,
                }),
            },
            operation_type: Some(OperationType::Query),
        });
    }

    let (planned_index, index_name, index_predicate_field_and_value) =
        if let Some(filter) = &query.filter {
            if let Some((field, value)) = extract_indexed_equality(filter, definition) {
                let index_name = definition
                    .indexes
                    .iter()
                    .find(|idx| idx.fields.contains(&field))
                    .map(|idx| idx.name.clone());

                if index_name.is_some() {
                    (true, index_name, Some((field, value)))
                } else {
                    (false, None, None)
                }
            } else {
                (false, None, None)
            }
        } else {
            (false, None, None)
        };
    let used_index = planned_index && context.live_query;
    // Substrate collections declare no indexes and already live inside the
    // application's tenant-scoped namespace; their bounded queries are exempt.
    if context.live_query
        && !substrate
        && query.filter.as_ref().is_some_and(contains_managed_scope)
        && !used_index
    {
        return Err(StateFailure::new(
            "INDEX_UNAVAILABLE",
            "managed tenant scope requires a schema-declared equality index",
        ));
    }

    let planning_ms = query_start.elapsed().as_secs_f64() * 1000.0;
    let filtering_start = Instant::now();

    // Execute against authoritative rows. A schema-declared equality index is
    // derived for this exact namespaced collection and must answer the query;
    // the managed path never silently reinterprets an indexed scope as a scan.
    let execution_before = crate::query_execution_diagnostics::counters();
    let index_lookup_start = Instant::now();
    let selected_rows = if !context.live_query {
        context
            .snapshot_rows
            .iter()
            .filter(|row| {
                row.capability == capability
                    && !row.deleted
                    && query
                        .filter
                        .as_ref()
                        .is_none_or(|filter| matches(&query_value(row), filter))
            })
            .cloned()
            .collect()
    } else if let Some((field, search_value)) = &index_predicate_field_and_value {
        _db.create_equality_index(&capability, field)
            .map_err(StateFailure::storage)?;
        _db.query_collection_by_equality(&capability, &[(field.as_str(), search_value)], |row| {
            !row.deleted
                && query
                    .filter
                    .as_ref()
                    .is_none_or(|filter| matches(&query_value(row), filter))
        })
        .map_err(StateFailure::storage)?
        .ok_or_else(|| {
            StateFailure::new(
                "INDEX_UNAVAILABLE",
                "the schema-declared scoped equality index is unavailable",
            )
        })?
    } else {
        _db.query_collection(&capability, None, |row| {
            !row.deleted
                && query
                    .filter
                    .as_ref()
                    .is_none_or(|filter| matches(&query_value(row), filter))
        })
        .map_err(StateFailure::storage)?
    };
    let index_lookup_ms = index_lookup_start.elapsed().as_secs_f64() * 1000.0;
    let execution_after = crate::query_execution_diagnostics::counters();
    let candidates_examined = if used_index {
        // The equality executor clones only predicate-matching candidates. Its
        // process counters remain useful operationally but cannot be differenced
        // per request under concurrency, so the response reports the isolated
        // result of this execution.
        selected_rows.len()
    } else {
        execution_after
            .scan_records_visited
            .saturating_sub(execution_before.scan_records_visited) as usize
    };
    let _predicate_evaluated = execution_after
        .records_predicate_evaluated
        .saturating_sub(execution_before.records_predicate_evaluated)
        as usize;
    let mut values = selected_rows.iter().map(query_value).collect::<Vec<_>>();

    if let Some(policy) = &read_policy {
        let actor =
            if context.authorization.subject.is_empty() || context.authorization.subject == ":" {
                None
            } else {
                context
                    .authorization
                    .subject
                    .split(':')
                    .nth(1)
                    .map(|id| Actor::new(id))
            };

        let auth_state = matches!(policy, &PolicySubject::Member).then(|| {
            AuthorizationState::new(
                Arc::new(schema.clone()),
                context.state_namespace.clone(),
                Arc::new(policy_snapshot.clone()),
            )
        });

        values.retain(|record_value| {
            if let Some(record_id) = record_value.get("_id").and_then(Value::as_str) {
                let mut record_context = RecordAuthorizationContext {
                    actor: actor.clone(),
                    collection: query.collection.clone(),
                    record_id: record_id.to_string(),
                    record_value: record_value.clone(),
                    authorization_state: None,
                };
                if let Some(state) = &auth_state {
                    record_context.authorization_state = Some(state.clone());
                }

                PolicyEvaluator::evaluate_record(policy.clone(), &record_context).is_ok()
            } else {
                true
            }
        });
    }

    let reference_rows =
        load_reference_rows(_db, &context.state_namespace, &values, &query.references)?;
    for value in &mut values {
        for reference in &query.references {
            let id = value.get(&reference.field).and_then(Value::as_str);
            let related = id
                .and_then(|id| {
                    reference_rows
                        .get(&(reference.collection.clone(), id.to_string()))
                        .filter(|value| !value.is_null())
                })
                .map(|value| projected(value, &reference.projection))
                .unwrap_or(Value::Null);
            if let Some(object) = value.as_object_mut() {
                object.insert(reference.as_name.clone(), related);
            }
        }
    }
    values.sort_by(|a, b| {
        for order in &query.order_by {
            let cmp = a
                .get(&order.field)
                .zip(b.get(&order.field))
                .and_then(|(x, y)| compare_value(x, y))
                .unwrap_or(Ordering::Equal);
            if cmp != Ordering::Equal {
                return if matches!(order.direction, SortDirection::Desc) {
                    cmp.reverse()
                } else {
                    cmp
                };
            }
        }
        compare_value(
            a.get("_id").unwrap_or(&Value::Null),
            b.get("_id").unwrap_or(&Value::Null),
        )
        .unwrap_or(Ordering::Equal)
    });
    let pagination_hash = pagination_query_hash(query)?;
    let start = if let Some(cursor) = &query.cursor {
        decode_query_cursor(cursor, &pagination_hash)?
    } else {
        query.offset
    };
    let limit = query.limit.unwrap_or(100).min(1000);
    let end = (start + limit).min(values.len());
    let page = if start <= values.len() {
        values[start..end].to_vec()
    } else {
        vec![]
    };
    let filtering_ms = filtering_start.elapsed().as_secs_f64() * 1000.0;
    let aggregates = aggregate(&values, &query.group_by, &query.aggregates)?;
    let hash = pagination_hash;

    let (execution_strategy, scan_fallback_flag, access_method) = if used_index {
        ("index_lookup".into(), false, Some("index".to_string()))
    } else {
        (
            "full_table_scan".into(),
            true,
            Some("sequential_scan".to_string()),
        )
    };

    let rows_scanned = candidates_examined;

    let index_metrics = Some(IndexMetrics {
        index_used: index_name.clone(),
        index_hits: if used_index { candidates_examined } else { 0 },
        index_misses: 0,
        fallback_used: scan_fallback_flag,
        fallback_reason: if scan_fallback_flag {
            Some("no_applicable_index".to_string())
        } else {
            None
        },
    });

    let serialization_start = Instant::now();
    let serialized_records: Vec<Value> = page
        .iter()
        .map(|v| projected(v, &effective_projection))
        .collect();
    let serialization_ms = serialization_start.elapsed().as_secs_f64() * 1000.0;

    let total_ms = query_start.elapsed().as_secs_f64() * 1000.0;
    let returned_count = serialized_records.len();
    let next_cursor = (end < values.len()).then(|| encode_query_cursor(&hash, end));

    let timing = ExecutionTiming {
        planning_ms: Some(planning_ms),
        index_lookup_ms: used_index.then_some(index_lookup_ms),
        filtering_ms: Some(filtering_ms),
        serialization_ms: Some(serialization_ms),
        total_ms: Some(total_ms),
    };

    Ok(QueryResult {
        query_id: format!("query:{}:{}", context.state_version, &hash[7..23]),
        query_hash: hash,
        application_revision: context.revision_id.clone(),
        schema_version: context.schema_version,
        state_version: context.state_version,
        causal_cursor: context.causal_cursor.clone(),
        records: serialized_records,
        aggregates,
        next_cursor,
        plan: QueryPlan {
            implementation: execution_strategy,
            collection: query.collection.clone(),
            index: index_name,
            scan_fallback: scan_fallback_flag,
            access_method,
            estimated_rows: Some(values.len()),
            fallback_reason: if scan_fallback_flag {
                Some("no_applicable_index".to_string())
            } else {
                None
            },
            actual_rows_scanned: Some(rows_scanned),
            actual_rows_returned: Some(returned_count),
            matching_records: Some(values.len()),
            unrelated_records_examined: Some(0),
            pagination_position: Some(start),
            execution_ms: Some(total_ms),
            timing: Some(timing),
            index_metrics,
        },
        operation_type: Some(OperationType::Query),
    })
}
fn aggregate(
    values: &[Value],
    groups: &[String],
    defs: &[AggregateDefinition],
) -> Result<Vec<Value>, StateFailure> {
    if defs.is_empty() {
        return Ok(vec![]);
    }
    let mut buckets: BTreeMap<String, (Map<String, Value>, Vec<&Value>)> = BTreeMap::new();
    for value in values {
        let mut key = Map::new();
        for field in groups {
            key.insert(
                field.clone(),
                value.get(field).cloned().unwrap_or(Value::Null),
            );
        }
        let encoded = serde_json::to_string(&key).map_err(StateFailure::storage)?;
        buckets
            .entry(encoded)
            .or_insert((key, vec![]))
            .1
            .push(value);
    }
    let mut out = vec![];
    for (_, (group, rows)) in buckets {
        let mut result = group;
        for def in defs {
            let nums = def
                .field
                .as_ref()
                .map(|field| {
                    rows.iter()
                        .filter_map(|v| v.get(field).and_then(Value::as_f64))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let value = match def.function {
                AggregateFunction::Count => Value::from(rows.len() as u64),
                AggregateFunction::Sum => Value::from(nums.iter().sum::<f64>()),
                AggregateFunction::Avg => {
                    if nums.is_empty() {
                        Value::Null
                    } else {
                        Value::from(nums.iter().sum::<f64>() / nums.len() as f64)
                    }
                }
                AggregateFunction::Min => nums
                    .into_iter()
                    .reduce(f64::min)
                    .map(Value::from)
                    .unwrap_or(Value::Null),
                AggregateFunction::Max => nums
                    .into_iter()
                    .reduce(f64::max)
                    .map(Value::from)
                    .unwrap_or(Value::Null),
            };
            result.insert(def.name.clone(), value);
        }
        out.push(Value::Object(result));
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionOperationKind {
    Insert,
    Update,
    Delete,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionOperation {
    pub kind: TransactionOperationKind,
    pub collection: String,
    pub id: String,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub if_version: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransactionPrecondition {
    CollectionVersion {
        collection: String,
        version: u64,
    },
    StateVersion {
        version: u64,
    },
    ApplicationRevision {
        revision_id: String,
    },
    CausalCursor {
        cursor: BTreeMap<String, u64>,
    },
    /// What must be true of one record for the transaction to commit.
    ///
    /// Distinct from every variant above, and evaluated in a different place
    /// for a reason: those compare counters that can be read before the commit
    /// lock is taken, while this one compares the fields a caller reads back —
    /// the document's `__version`, its authority epoch, its lease — and must
    /// therefore be evaluated *inside* the same lock as the writes. Checking it
    /// against the pre-commit snapshot would be a client-side check wearing the
    /// authority's name: between the read and the write, another writer
    /// commits.
    ///
    /// It is also what makes `putIfAbsent` and `updateIfVersion` mean the same
    /// thing against a managed authority as against the file runtime. Mapping
    /// a caller's `expectedVersion` onto `if_version` would not: that fences
    /// the row's internal storage sequence, which is a different number that
    /// happens to share a name.
    Record {
        collection: String,
        id: String,
        #[serde(default)]
        require_absent: bool,
        #[serde(default)]
        expected_version: Option<u64>,
        #[serde(default)]
        expected_epoch: Option<u64>,
        #[serde(default)]
        expected_lease_id: Option<String>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionRequest {
    #[serde(default)]
    pub transaction_id: Option<String>,
    pub tenant_id: String,
    pub application_id: String,
    pub revision_id: String,
    pub schema_version: u64,
    #[serde(default)]
    pub state_namespace: Option<String>,
    #[serde(default)]
    /// Optimistic authority revision. A mismatched revision is rejected.
    #[serde(rename = "base_revision")]
    pub causal_parent: Option<u64>,
    pub authorization: AuthorizationContext,
    pub operations: Vec<TransactionOperation>,
    #[serde(default)]
    pub preconditions: Vec<TransactionPrecondition>,
    /// Read bases this authority issued for this state namespace
    /// (`capture_read_basis`), re-evaluated inside the commit lock.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fences: Vec<crate::ReadBasis>,
}

/// Collections the composition substrate keeps inside an application's state
/// namespace. They are not part of the application contract: their records
/// are validated by the substrate, and reading or writing them requires the
/// dedicated `composition:read` / `composition:write` capabilities rather
/// than `state:*`. Every other `_feltdb` name remains unwritable here.
pub const SUBSTRATE_COLLECTIONS: &[&str] = &[
    "_feltdb.CompositionContract",
    "_feltdb.Composition",
    "_feltdb.CompositionVersion",
    "_feltdb.CompositionTopology",
    "_feltdb.StateOwnership",
    "_feltdb.StateDependency",
    "_feltdb.StateChange",
    "_feltdb.StateAttribution",
    "_feltdb.StateObservation",
    "_feltdb.StateProposal",
    "_feltdb.StateImpact",
    "_feltdb.Intent",
    "_feltdb.IntentEvent",
    "_feltdb.ProposalEvent",
];

pub fn is_substrate_collection(name: &str) -> bool {
    SUBSTRATE_COLLECTIONS.contains(&name)
}

/// Capture a read basis over an application state namespace.
///
/// Every collection the read set names must be one the caller may read in
/// full: an application collection readable under `state:read` whose read
/// policy does not depend on the record, or a substrate collection under
/// `composition:read`. A token is a digest over every row it covers, so a
/// basis over rows the caller could not read would disclose them; such a
/// read set is refused, never digested.
pub fn capture_read_basis(
    db: &FeltDb,
    schema: &StateSchema,
    state_namespace: &str,
    reads: &crate::ReadSet,
    readable: &dyn Fn(&str) -> bool,
) -> Result<crate::ReadBasis, StateFailure> {
    let reads = reads
        .normalised()
        .map_err(|error| StateFailure::new("VALIDATION_FAILED", &error.to_string()))?;
    authorize_read_set(schema, &reads, readable)?;
    db.read_basis_in(
        &reads,
        &crate::ReadLayout::Namespace(state_namespace.to_string()),
    )
    .map_err(StateFailure::storage)
}

/// The refusal for a read basis over a collection the caller cannot read in full.
pub fn read_basis_denied(collection: &str) -> StateFailure {
    let mut failure = StateFailure::new(
        "AUTHORIZATION_DENIED",
        "a read basis may only cover collections the caller can read in full",
    );
    failure.resource = Some(collection.to_string());
    failure
}

fn authorize_read_set(
    schema: &StateSchema,
    reads: &crate::ReadSet,
    readable: &dyn Fn(&str) -> bool,
) -> Result<(), StateFailure> {
    let collections = reads
        .records
        .iter()
        .map(|record| record.collection.as_str())
        .chain(
            reads
                .predicates
                .iter()
                .map(|predicate| predicate.collection.as_str()),
        );
    for name in collections {
        if !is_substrate_collection(name) {
            collection(schema, name)?;
        }
        if !readable(name) {
            return Err(read_basis_denied(name));
        }
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionAudit {
    pub transaction_id: String,
    pub subject: String,
    pub tenant: String,
    pub application: String,
    pub revision: String,
    pub schema_version: u64,
    pub state_before: u64,
    pub state_after: u64,
    pub operations: usize,
    pub authorization_decisions: Vec<String>,
    pub causal_parent: Option<u64>,
    pub result: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionResult {
    pub transaction_id: String,
    pub base_revision: u64,
    pub commit_revision: u64,
    pub status: crate::TransactionStatus,
    pub state_before: u64,
    pub state_after: u64,
    pub causal_cursor: BTreeMap<String, u64>,
    pub duplicate: bool,
    pub audit: TransactionAudit,
}

fn valid_type(value: &Value, field: &FieldSchema) -> bool {
    if value.is_null() {
        return field.nullable;
    }
    match &field.field_type {
        FieldType::Primitive { primitive } => match primitive {
            PrimitiveType::String => value.is_string(),
            PrimitiveType::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
            PrimitiveType::Number => value.is_number(),
            PrimitiveType::Boolean => value.is_boolean(),
            PrimitiveType::Json => true,
            PrimitiveType::Timestamp => value.as_str().is_some_and(valid_timestamp),
            PrimitiveType::Date => value.as_str().is_some_and(valid_date),
            PrimitiveType::Time => value.as_str().is_some_and(valid_time),
            PrimitiveType::Uuid => value.as_str().is_some_and(valid_uuid),
            PrimitiveType::Decimal => value.as_str().is_some_and(valid_decimal),
            PrimitiveType::Bigint => value.as_str().is_some_and(valid_integer_string),
            PrimitiveType::Email => value.as_str().is_some_and(valid_email),
            PrimitiveType::Url => value.as_str().is_some_and(valid_url),
            PrimitiveType::Phone => value.as_str().is_some_and(valid_phone),
            PrimitiveType::Binary => value.as_str().is_some_and(valid_base64),
            PrimitiveType::Money => valid_money(value),
            PrimitiveType::File => valid_file(value),
            PrimitiveType::GeoPoint => valid_geo(value),
            PrimitiveType::Object => value.is_object(),
        },
        FieldType::Enum { values } => value
            .as_str()
            .is_some_and(|v| values.iter().any(|x| x == v)),
        FieldType::Reference { .. } => value.is_string(),
        FieldType::Array { items } => value.as_array().is_some_and(|values| {
            values.iter().all(|value| {
                valid_type(
                    value,
                    &FieldSchema {
                        name: "item".into(),
                        field_type: (**items).clone(),
                        nullable: false,
                        required: true,
                        default: None,
                        constraints: Default::default(),
                        computed: None,
                    },
                )
            })
        }),
        FieldType::Map { values } => value.as_object().is_some_and(|entries| {
            entries
                .values()
                .all(|value| valid_nested_type(value, values))
        }),
        FieldType::Vector { dimensions } => value.as_array().is_some_and(|values| {
            dimensions.is_none_or(|expected| values.len() == expected)
                && values.iter().all(Value::is_number)
        }),
    }
}

fn valid_nested_type(value: &Value, field_type: &FieldType) -> bool {
    valid_type(
        value,
        &FieldSchema {
            name: "value".into(),
            field_type: field_type.clone(),
            nullable: false,
            required: true,
            default: None,
            constraints: Default::default(),
            computed: None,
        },
    )
}
fn valid_integer_string(value: &str) -> bool {
    !value.is_empty()
        && value
            .strip_prefix('-')
            .unwrap_or(value)
            .chars()
            .all(|c| c.is_ascii_digit())
}
fn valid_decimal(value: &str) -> bool {
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    let mut parts = unsigned.split('.');
    parts
        .next()
        .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
        && parts
            .next()
            .is_none_or(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
        && parts.next().is_none()
}
fn valid_date(value: &str) -> bool {
    value.len() == 10
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value[..4].parse::<u16>().is_ok()
        && value[5..7]
            .parse::<u8>()
            .is_ok_and(|v| (1..=12).contains(&v))
        && value[8..]
            .parse::<u8>()
            .is_ok_and(|v| (1..=31).contains(&v))
}
fn valid_time(value: &str) -> bool {
    value.len() >= 5
        && value.as_bytes().get(2) == Some(&b':')
        && value[..2].parse::<u8>().is_ok_and(|v| v < 24)
        && value[3..5].parse::<u8>().is_ok_and(|v| v < 60)
        && (value.len() == 5
            || value.as_bytes().get(5) == Some(&b':')
                && value
                    .get(6..8)
                    .is_some_and(|v| v.parse::<u8>().is_ok_and(|s| s < 60)))
}
fn valid_timestamp(value: &str) -> bool {
    value.get(..10).is_some_and(valid_date)
        && value
            .as_bytes()
            .get(10)
            .is_some_and(|v| *v == b'T' || *v == b' ')
        && value.get(11..).is_some_and(valid_time)
}
fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.chars().enumerate().all(|(i, c)| {
            matches!(i, 8 | 13 | 18 | 23) && c == '-'
                || !matches!(i, 8 | 13 | 18 | 23) && c.is_ascii_hexdigit()
        })
}
fn valid_email(value: &str) -> bool {
    let mut parts = value.split('@');
    parts.next().is_some_and(|v| !v.is_empty())
        && parts
            .next()
            .is_some_and(|v| v.contains('.') && !v.starts_with('.') && !v.ends_with('.'))
        && parts.next().is_none()
}
fn valid_url(value: &str) -> bool {
    (value.starts_with("https://") || value.starts_with("http://"))
        && value.split("//").nth(1).is_some_and(|v| !v.is_empty())
}
fn valid_phone(value: &str) -> bool {
    let digits = value.chars().filter(|c| c.is_ascii_digit()).count();
    (7..=15).contains(&digits)
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || "+-(). ".contains(c))
}
fn valid_base64(value: &str) -> bool {
    !value.is_empty()
        && value.len() % 4 == 0
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
}
fn valid_money(value: &Value) -> bool {
    value.as_object().is_some_and(|v| {
        v.get("amount")
            .and_then(Value::as_str)
            .is_some_and(valid_decimal)
            && v.get("currency")
                .and_then(Value::as_str)
                .is_some_and(|c| c.len() == 3 && c.chars().all(|x| x.is_ascii_uppercase()))
    })
}
fn valid_file(value: &Value) -> bool {
    value.as_object().is_some_and(|v| {
        v.get("name").is_some_and(Value::is_string)
            && v.get("type").is_some_and(Value::is_string)
            && v.get("size").and_then(Value::as_u64).is_some()
            && (v.get("url").is_some_and(Value::is_string)
                || v.get("ref").is_some_and(Value::is_string))
    })
}
fn valid_geo(value: &Value) -> bool {
    value.as_object().is_some_and(|v| {
        v.get("latitude")
            .and_then(Value::as_f64)
            .is_some_and(|x| (-90.0..=90.0).contains(&x))
            && v.get("longitude")
                .and_then(Value::as_f64)
                .is_some_and(|x| (-180.0..=180.0).contains(&x))
    })
}
fn validate_record(collection: &CollectionSchema, value: &mut Value) -> Result<(), StateFailure> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| StateFailure::new("VALIDATION_FAILED", "record must be an object"))?;
    for field in &collection.fields {
        if let Some(expression) = &field.computed {
            let computed = if let Some(source) = expression.strip_prefix("copy:") {
                object.get(source.trim()).cloned().unwrap_or(Value::Null)
            } else if let Some(sources) = expression.strip_prefix("concat:") {
                Value::String(
                    sources
                        .split(',')
                        .filter_map(|source| object.get(source.trim()).and_then(Value::as_str))
                        .collect::<String>(),
                )
            } else {
                return Err(StateFailure::new(
                    "VALIDATION_FAILED",
                    "unsupported computed expression",
                ));
            };
            object.insert(field.name.clone(), computed);
        }
    }
    for field in &collection.fields {
        if !object.contains_key(&field.name) {
            if let Some(default) = &field.default {
                object.insert(field.name.clone(), default.clone());
            } else if field.required && field.computed.is_none() {
                return Err(StateFailure::new(
                    "VALIDATION_FAILED",
                    &format!("missing required field {}", field.name),
                ));
            }
        }
        if let Some(value) = object.get(&field.name) {
            if !valid_type(value, field) {
                return Err(StateFailure::new(
                    "VALIDATION_FAILED",
                    &format!("invalid value for {}", field.name),
                ));
            }
            if let Some(text) = value.as_str() {
                if field.constraints.min_length.is_some_and(|n| text.len() < n)
                    || field.constraints.max_length.is_some_and(|n| text.len() > n)
                {
                    return Err(StateFailure::new(
                        "VALIDATION_FAILED",
                        &format!("length constraint failed for {}", field.name),
                    ));
                }
            }
            if let Some(number) = value.as_f64() {
                if field.constraints.minimum.is_some_and(|n| number < n)
                    || field.constraints.maximum.is_some_and(|n| number > n)
                {
                    return Err(StateFailure::new(
                        "VALIDATION_FAILED",
                        &format!("numeric constraint failed for {}", field.name),
                    ));
                }
            }
        }
    }
    // `__version` and `id` are record identity, carried by every client
    // record whether or not the contract declares them; a declared `id` is
    // validated by its declaration above.
    if object.keys().any(|name| {
        name != "__version" && name != "id" && !collection.fields.iter().any(|f| &f.name == name)
    }) {
        return Err(StateFailure::new(
            "VALIDATION_FAILED",
            "record contains an unknown field",
        ));
    }
    Ok(())
}
pub fn execute_transaction(
    db: &FeltDb,
    schema: &StateSchema,
    request: &TransactionRequest,
    write_policy: Option<PolicySubject>,
) -> Result<TransactionResult, StateFailure> {
    execute_transaction_with_policies(db, schema, request, write_policy.as_ref(), None)
}
pub fn execute_transaction_with_collection_policies(
    db: &FeltDb,
    schema: &StateSchema,
    request: &TransactionRequest,
    write_policies: &BTreeMap<String, PolicySubject>,
) -> Result<TransactionResult, StateFailure> {
    execute_transaction_with_policies(db, schema, request, None, Some(write_policies))
}
fn execute_transaction_with_policies(
    db: &FeltDb,
    schema: &StateSchema,
    request: &TransactionRequest,
    uniform_write_policy: Option<&PolicySubject>,
    collection_write_policies: Option<&BTreeMap<String, PolicySubject>>,
) -> Result<TransactionResult, StateFailure> {
    if request.application_id != schema.application_id
        || request.revision_id != schema.revision_id
        || request.schema_version != schema.schema_version
    {
        return Err(StateFailure::new(
            "SCHEMA_MISMATCH",
            "transaction does not match state schema",
        ));
    }
    if request.authorization.subject.is_empty()
        || request.authorization.tenant_id != request.tenant_id
        || request.authorization.application_id != request.application_id
        || request.authorization.revision_id != request.revision_id
    {
        return Err(StateFailure::new(
            "AUTHORIZATION_DENIED",
            "transaction authorization context is invalid",
        ));
    }
    if !request.authorization.can("state:write") {
        return Err(StateFailure::new(
            "AUTHORIZATION_DENIED",
            "state:write is required",
        ));
    }
    let canonical = if request.fences.is_empty() {
        serde_json::to_vec(&(
            request.tenant_id.as_str(),
            request.application_id.as_str(),
            request.revision_id.as_str(),
            request.schema_version,
            request.state_namespace.as_deref(),
            request.causal_parent,
            &request.authorization.subject,
            &request.preconditions,
            &request.operations,
        ))
    } else {
        // Fences are part of what the transaction means, so they bind its
        // identity. Unfenced transactions hash exactly as they always have.
        serde_json::to_vec(&(
            request.tenant_id.as_str(),
            request.application_id.as_str(),
            request.revision_id.as_str(),
            request.schema_version,
            request.state_namespace.as_deref(),
            request.causal_parent,
            &request.authorization.subject,
            &request.preconditions,
            &request.operations,
            &request.fences,
        ))
    }
    .map_err(StateFailure::storage)?;
    let transaction_id = request
        .transaction_id
        .clone()
        .unwrap_or_else(|| format!("tx_{:x}", Sha256::digest(&canonical)));
    // Client transaction identities are application-local. The managed store
    // serves many applications from one durable log, so using the bare client
    // id as the substrate id lets one application collide with another.
    // Hash the authorization boundary into the private storage identity while
    // preserving the caller's id in every public result and audit record.
    let storage_transaction_id = format!(
        "txscope_{:x}_{}",
        Sha256::digest(
            format!(
                "{}\0{}\0{}",
                request.tenant_id,
                request.application_id,
                request
                    .state_namespace
                    .as_deref()
                    .unwrap_or(&request.application_id)
            )
            .as_bytes()
        ),
        transaction_id
    );
    let payload_hash = format!("sha256:{:x}", Sha256::digest(&canonical));
    if db
        .has_applied_transaction(&storage_transaction_id)
        .map_err(StateFailure::storage)?
    {
        if db
            .applied_transaction_payload_hash(&storage_transaction_id)
            .map_err(StateFailure::storage)?
            .as_deref()
            != Some(payload_hash.as_str())
        {
            return Err(StateFailure::new(
                "IDEMPOTENCY_CONFLICT",
                "transaction ID was already committed with a different canonical payload",
            ));
        }
        let state = db.sequence().map_err(StateFailure::storage)?;
        let cursor = db
            .operation_versions()
            .map_err(StateFailure::storage)?
            .into_iter()
            .collect();
        let audit = TransactionAudit {
            transaction_id: transaction_id.clone(),
            subject: request.authorization.subject.clone(),
            tenant: request.tenant_id.clone(),
            application: request.application_id.clone(),
            revision: request.revision_id.clone(),
            schema_version: request.schema_version,
            state_before: state,
            state_after: state,
            operations: request.operations.len(),
            authorization_decisions: vec!["state:write allowed".into()],
            causal_parent: request.causal_parent,
            result: "duplicate".into(),
        };
        return Ok(TransactionResult {
            transaction_id,
            base_revision: db.current_revision().map_err(StateFailure::storage)?,
            commit_revision: db.current_revision().map_err(StateFailure::storage)?,
            status: crate::TransactionStatus::Committed,
            state_before: state,
            state_after: state,
            causal_cursor: cursor,
            duplicate: true,
            audit,
        });
    }
    let namespace = request
        .state_namespace
        .as_deref()
        .unwrap_or(&request.application_id)
        .to_string();
    let snapshot = transaction_snapshot_rows(
        db,
        schema,
        request,
        uniform_write_policy,
        collection_write_policies,
    )?;
    // Record preconditions are collected here and evaluated by the store,
    // inside the same lock as the writes. Evaluating them against `snapshot`
    // alongside the counters below would reintroduce exactly the gap they
    // exist to close.
    let mut record_preconditions: Vec<RecordPrecondition> = Vec::new();
    for condition in &request.preconditions {
        let (resource, expected, actual, passed) = match condition {
            TransactionPrecondition::Record {
                collection,
                id,
                require_absent,
                expected_version,
                expected_epoch,
                expected_lease_id,
            } => {
                let precondition = RecordPrecondition {
                    capability: format!("{namespace}:{collection}"),
                    key: id.clone(),
                    require_absent: *require_absent,
                    expected_version: *expected_version,
                    expected_epoch: *expected_epoch,
                    expected_lease_id: expected_lease_id.clone(),
                };
                if precondition.is_empty() {
                    // Refused rather than ignored: a precondition that
                    // constrains nothing reads as protection and provides none.
                    let mut failure = StateFailure::new(
                        "VALIDATION_FAILED",
                        &format!("precondition on {collection}:{id} constrains nothing"),
                    );
                    failure.resource = Some(format!("{collection}:{id}"));
                    return Err(failure);
                }
                record_preconditions.push(precondition);
                continue;
            }
            TransactionPrecondition::CollectionVersion {
                collection,
                version,
            } => {
                let capability = format!(
                    "{}:{}",
                    request
                        .state_namespace
                        .as_deref()
                        .unwrap_or(&request.application_id),
                    collection
                );
                let actual = snapshot
                    .iter()
                    .filter(|row| row.capability == capability)
                    .filter_map(|row| row.operation.as_ref().map(|op| op.sequence))
                    .max()
                    .unwrap_or(0);
                (
                    collection.clone(),
                    Value::from(*version),
                    Value::from(actual),
                    actual == *version,
                )
            }
            TransactionPrecondition::StateVersion { version } => {
                let actual = db.sequence().map_err(StateFailure::storage)?;
                (
                    "state".into(),
                    Value::from(*version),
                    Value::from(actual),
                    actual == *version,
                )
            }
            TransactionPrecondition::ApplicationRevision { revision_id } => (
                ("application_revision").into(),
                Value::String(revision_id.clone()),
                Value::String(schema.revision_id.clone()),
                revision_id == &schema.revision_id,
            ),
            TransactionPrecondition::CausalCursor { cursor } => {
                let actual: BTreeMap<_, _> = db
                    .operation_versions()
                    .map_err(StateFailure::storage)?
                    .into_iter()
                    .collect();
                (
                    "causal_cursor".into(),
                    serde_json::to_value(cursor).map_err(StateFailure::storage)?,
                    serde_json::to_value(&actual).map_err(StateFailure::storage)?,
                    cursor == &actual,
                )
            }
        };
        if !passed {
            let mut failure =
                StateFailure::new("PRECONDITION_FAILED", "transaction precondition failed");
            failure.resource = Some(resource);
            failure.expected = Some(expected);
            failure.actual = Some(actual);
            failure.transaction = Some(transaction_id.clone());
            failure.causal_position = db.sequence().ok();
            return Err(failure);
        }
    }
    let mut mutations = vec![];
    let mut preconditions = vec![];
    let mut overlay: BTreeMap<(String, String), Option<Value>> = BTreeMap::new();
    for operation in &request.operations {
        if !request.authorization.writable_collections.is_empty()
            && !request
                .authorization
                .writable_collections
                .contains(&operation.collection)
        {
            return Err(StateFailure::new(
                "AUTHORIZATION_DENIED",
                "collection is not writable",
            ));
        }
        // Substrate collections are validated by the substrate that owns them,
        // and written only under the dedicated capability.
        let substrate = is_substrate_collection(&operation.collection);
        if substrate && !request.authorization.can("composition:write") {
            let mut failure = StateFailure::new(
                "AUTHORIZATION_DENIED",
                "composition:write is required for substrate collections",
            );
            failure.resource = Some(operation.collection.clone());
            return Err(failure);
        }
        let definition = if substrate {
            None
        } else {
            Some(collection(schema, &operation.collection)?)
        };
        let capability = format!(
            "{}:{}",
            request
                .state_namespace
                .as_deref()
                .unwrap_or(&request.application_id),
            operation.collection
        );
        let key = operation.id.clone();
        let persisted = snapshot
            .iter()
            .find(|row| row.capability == capability && row.key == key && !row.deleted);
        let existing_value = overlay
            .get(&(capability.clone(), key.clone()))
            .cloned()
            .unwrap_or_else(|| persisted.map(|row| Some(row.value.clone())).unwrap_or(None));

        // Per-operation record authorization
        let write_policy = collection_write_policies
            .and_then(|policies| policies.get(&operation.collection))
            .or(uniform_write_policy)
            .filter(|_| !substrate);
        if let Some(policy) = write_policy {
            let actor = if request.authorization.subject.is_empty()
                || request.authorization.subject == ":"
            {
                None
            } else {
                request
                    .authorization
                    .subject
                    .split(':')
                    .nth(1)
                    .map(|id| Actor::new(id))
            };

            // Select authorization record based on operation type
            let auth_record_value = match operation.kind {
                TransactionOperationKind::Insert => {
                    // For create: authorize against proposed record
                    operation.value.clone()
                }
                TransactionOperationKind::Update | TransactionOperationKind::Delete => {
                    // For update/delete: authorize against EXISTING record (before changes)
                    // This prevents authorization bypass via ownership/org changes
                    existing_value
                        .clone()
                        .ok_or_else(|| StateFailure::new("CONFLICT", "record does not exist"))?
                }
            };

            // Create authorization state for policies that need to verify relationships
            let auth_state = AuthorizationState::new(
                Arc::new(schema.clone()),
                request
                    .state_namespace
                    .as_deref()
                    .unwrap_or(&request.application_id)
                    .to_string(),
                Arc::new(snapshot.clone()),
            );

            let record_context = RecordAuthorizationContext {
                actor,
                collection: operation.collection.clone(),
                record_id: key.clone(),
                record_value: auth_record_value,
                authorization_state: Some(auth_state),
            };

            PolicyEvaluator::evaluate_record(policy.clone(), &record_context).map_err(|_| {
                StateFailure::new(
                    "AUTHORIZATION_DENIED",
                    &format!(
                        "record authorization failed for operation on {}:{}",
                        operation.collection, key
                    ),
                )
            })?;
        }

        let value = match operation.kind {
            TransactionOperationKind::Insert => {
                if existing_value.is_some() {
                    return Err(StateFailure::new("CONFLICT", "record already exists"));
                }
                let mut value = operation.value.clone();
                match definition {
                    Some(definition) => validate_record(definition, &mut value)?,
                    None if value.is_object() => {}
                    None => {
                        return Err(StateFailure::new(
                            "VALIDATION_FAILED",
                            "record must be an object",
                        ));
                    }
                }
                Some(value)
            }
            TransactionOperationKind::Update => {
                let current = existing_value
                    .ok_or_else(|| StateFailure::new("CONFLICT", "record does not exist"))?;
                let mut value = current;
                let patch = operation.value.as_object().ok_or_else(|| {
                    StateFailure::new("VALIDATION_FAILED", "update must be an object")
                })?;
                for (k, v) in patch {
                    value
                        .as_object_mut()
                        .expect("existing object")
                        .insert(k.clone(), v.clone());
                }
                if let Some(definition) = definition {
                    validate_record(definition, &mut value)?;
                }
                Some(value)
            }
            TransactionOperationKind::Delete => {
                if existing_value.is_none() {
                    return Err(StateFailure::new("CONFLICT", "record does not exist"));
                }
                None
            }
        };
        preconditions.push(AtomicPrecondition {
            capability: capability.clone(),
            key: key.clone(),
            expected_version: operation.if_version.or_else(|| {
                persisted
                    .and_then(|row| row.operation.as_ref())
                    .map(|op| op.sequence)
            }),
        });
        overlay.insert((capability.clone(), key.clone()), value.clone());
        mutations.push(AtomicMutation {
            capability,
            key,
            value,
        });
    }
    for definition in &schema.collections {
        let capability = format!(
            "{}:{}",
            request
                .state_namespace
                .as_deref()
                .unwrap_or(&request.application_id),
            definition.name
        );
        let mut referenced_records = BTreeMap::new();
        for ((cap, key), value) in overlay
            .iter()
            .filter(|((cap, _), value)| cap == &capability && value.is_some())
        {
            let value = value.as_ref().expect("filtered");
            for field in &definition.fields {
                if let FieldType::Reference { collection: target } = &field.field_type {
                    if let Some(reference) = value.get(&field.name).and_then(Value::as_str) {
                        let target_capability = format!(
                            "{}:{}",
                            request
                                .state_namespace
                                .as_deref()
                                .unwrap_or(&request.application_id),
                            target
                        );
                        let present = if let Some(value) =
                            overlay.get(&(target_capability.clone(), reference.into()))
                        {
                            value.is_some()
                        } else if snapshot.iter().any(|row| {
                            row.capability == target_capability
                                && row.key == reference
                                && !row.deleted
                        }) {
                            true
                        } else {
                            let cache_key = (target_capability.clone(), reference.to_string());
                            if let Some(present) = referenced_records.get(&cache_key) {
                                *present
                            } else {
                                let present = db
                                    .get_collection_record(&target_capability, reference)
                                    .map_err(StateFailure::storage)?
                                    .is_some();
                                referenced_records.insert(cache_key, present);
                                present
                            }
                        };
                        if !present {
                            let mut failure = StateFailure::new(
                                "VALIDATION_FAILED",
                                &format!(
                                    "reference {}.{} points to a missing record",
                                    definition.name, field.name
                                ),
                            );
                            failure.resource = Some(format!("{cap}:{key}"));
                            return Err(failure);
                        }
                    }
                }
            }
        }
        for index in definition.indexes.iter().filter(|index| index.unique) {
            let mut seen = BTreeSet::new();
            let existing = snapshot
                .iter()
                .filter(|row| row.capability == capability && !row.deleted)
                .filter(|row| !overlay.contains_key(&(capability.clone(), row.key.clone())))
                .map(|row| &row.value)
                .chain(
                    overlay
                        .iter()
                        .filter(|((cap, _), _)| cap == &capability)
                        .filter_map(|(_, value)| value.as_ref()),
                );
            for value in existing {
                let tuple = index
                    .fields
                    .iter()
                    .map(|field| value.get(field).cloned().unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                if index.sparse && tuple.iter().any(Value::is_null) {
                    continue;
                }
                let encoded = serde_json::to_string(&tuple).map_err(StateFailure::storage)?;
                if !seen.insert(encoded) {
                    return Err(StateFailure::new(
                        "CONFLICT",
                        &format!("unique index {} violated", index.name),
                    ));
                }
            }
        }
    }
    let commit = db
        .apply_atomic_transaction_fenced_in(
            &storage_transaction_id,
            Some(&payload_hash),
            request.causal_parent,
            &preconditions,
            &record_preconditions,
            &crate::ReadLayout::Namespace(namespace.clone()),
            &request.fences,
            &mutations,
            Some(serde_json::json!({"transaction_id":transaction_id.clone(),"subject":request.authorization.subject.clone(),"tenant":request.tenant_id.clone(),"application":request.application_id.clone(),"revision":request.revision_id.clone(),"schema_version":request.schema_version,"operations":request.operations.len(),"authorization_decisions":["state:write allowed"],"causal_parent":request.causal_parent,"result":"committed"})),
        )
        .map_err(|error| {
            // A record precondition that did not hold is a lost race, and it
            // says which predicate failed and what the authority holds. That
            // detail is the difference between "re-read and decide again" and
            // "this deployment is broken", so it is carried through as
            // structure rather than flattened into the message.
            if let FlowError::PreconditionFailed(refusal) = &error {
                let mut failure =
                    StateFailure::new("PRECONDITION_FAILED", &error.to_string());
                failure.transaction = Some(transaction_id.clone());
                failure.causal_position = db.sequence().ok();
                failure.resource = Some(refusal.key().to_string());
                failure.failure = serde_json::to_value(refusal.as_ref()).ok();
                if let PreconditionFailure::Version {
                    expected, actual, ..
                } = refusal.as_ref()
                {
                    failure.expected = Some(Value::from(*expected));
                    failure.actual = Some(Value::from(*actual));
                }
                return failure;
            }
            if let FlowError::RevisionConflict { expected, actual } = &error {
                let mut failure =
                    StateFailure::new("PRECONDITION_FAILED", &error.to_string());
                failure.transaction = Some(transaction_id.clone());
                failure.causal_position = db.sequence().ok();
                failure.resource = Some("authority_revision".into());
                failure.expected = Some(Value::from(*expected));
                failure.actual = Some(Value::from(*actual));
                return failure;
            }
            let text = error.to_string();
            if text.contains("INVALID_READ_SET:") {
                // A malformed or foreign basis is a caller error, never a lost race.
                let mut failure = StateFailure::new("VALIDATION_FAILED", &text);
                failure.transaction = Some(transaction_id.clone());
                return failure;
            }
            if text.contains("PRECONDITION_FAILED") {
                let mut failure = StateFailure::new("PRECONDITION_FAILED", &text);
                failure.transaction = Some(transaction_id.clone());
                failure.causal_position = db.sequence().ok();
                if let Some(condition) = preconditions.first() {
                    failure.resource = Some(format!("{}:{}", condition.capability, condition.key));
                    failure.expected = Some(
                        condition
                            .expected_version
                            .map(Value::from)
                            .unwrap_or(Value::Null),
                    );
                    failure.actual = Some(
                        snapshot
                            .iter()
                            .find(|row| {
                                row.capability == condition.capability && row.key == condition.key
                            })
                            .and_then(|row| row.operation.as_ref())
                            .map(|op| Value::from(op.sequence))
                            .unwrap_or(Value::Null),
                    );
                }
                failure
            } else {
                StateFailure::storage(error)
            }
        })?;
    let cursor = db
        .operation_versions()
        .map_err(StateFailure::storage)?
        .into_iter()
        .collect();
    let audit = TransactionAudit {
        transaction_id: transaction_id.clone(),
        subject: request.authorization.subject.clone(),
        tenant: request.tenant_id.clone(),
        application: request.application_id.clone(),
        revision: request.revision_id.clone(),
        schema_version: request.schema_version,
        state_before: commit.state_before,
        state_after: commit.state_after,
        operations: request.operations.len(),
        authorization_decisions: vec!["state:write allowed".into()],
        causal_parent: request.causal_parent,
        result: if commit.duplicate {
            "duplicate"
        } else {
            "committed"
        }
        .into(),
    };
    Ok(TransactionResult {
        transaction_id,
        base_revision: commit.base_revision,
        commit_revision: commit.commit_revision,
        status: commit.status,
        state_before: commit.state_before,
        state_after: commit.state_after,
        causal_cursor: cursor,
        duplicate: commit.duplicate,
        audit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extended_field_types_have_canonical_validation() {
        let valid = [
            (
                "uuid",
                serde_json::json!("550e8400-e29b-41d4-a716-446655440000"),
            ),
            ("decimal", serde_json::json!("-1234567890.0123456789")),
            ("bigint", serde_json::json!("9223372036854775808123")),
            ("date", serde_json::json!("2026-08-21")),
            ("time", serde_json::json!("13:45:30")),
            ("datetime", serde_json::json!("2026-08-21T13:45:30Z")),
            ("email", serde_json::json!("person@example.com")),
            ("url", serde_json::json!("https://example.com/path")),
            ("phone", serde_json::json!("+1 (212) 555-0100")),
            ("binary", serde_json::json!("SGVsbG8=")),
            (
                "money",
                serde_json::json!({"amount":"19.99","currency":"USD"}),
            ),
            (
                "file",
                serde_json::json!({"name":"report.pdf","type":"application/pdf","size":42,"ref":"sha256:abc"}),
            ),
            (
                "geo",
                serde_json::json!({"latitude":40.7,"longitude":-74.0}),
            ),
            ("object", serde_json::json!({"enabled":true})),
            (
                "array<uuid>",
                serde_json::json!(["550e8400-e29b-41d4-a716-446655440000"]),
            ),
            ("map<integer>", serde_json::json!({"one":1,"two":2})),
            ("vector<3>", serde_json::json!([0.1, 0.2, 0.3])),
        ];
        for (kind, value) in valid {
            assert!(
                valid_nested_type(&value, &parse_manifest_type(kind)),
                "{kind} rejected {value}"
            );
        }
        assert!(!valid_nested_type(
            &serde_json::json!("12.3x"),
            &parse_manifest_type("decimal")
        ));
        assert!(!valid_nested_type(
            &serde_json::json!([1, 2]),
            &parse_manifest_type("vector<3>")
        ));
        assert!(!valid_nested_type(
            &serde_json::json!({"latitude":100,"longitude":0}),
            &parse_manifest_type("geo")
        ));
    }
    fn schema() -> StateSchema {
        StateSchema {
            contract_version: 1,
            schema_version: 1,
            application_id: "app".into(),
            revision_id: "rev".into(),
            collections: vec![CollectionSchema {
                name: "incidents".into(),
                version: 1,
                fields: vec![
                    FieldSchema {
                        name: "title".into(),
                        field_type: FieldType::Primitive {
                            primitive: PrimitiveType::String,
                        },
                        nullable: false,
                        required: true,
                        default: None,
                        constraints: Default::default(),
                        computed: None,
                    },
                    FieldSchema {
                        name: "status".into(),
                        field_type: FieldType::Enum {
                            values: vec!["open".into(), "closed".into()],
                        },
                        nullable: false,
                        required: false,
                        default: Some(Value::String("open".into())),
                        constraints: Default::default(),
                        computed: None,
                    },
                ],
                indexes: vec![SchemaIndex {
                    name: "status".into(),
                    fields: vec!["status".into()],
                    unique: false,
                    sparse: false,
                    version: 1,
                }],
            }],
        }
    }
    fn scoped_schema() -> StateSchema {
        let mut schema = schema();
        schema.collections[0].fields[1].field_type = FieldType::Primitive {
            primitive: PrimitiveType::String,
        };
        schema
    }
    fn auth() -> AuthorizationContext {
        AuthorizationContext {
            subject: "user:1".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:read".into(), "state:write".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        }
    }
    fn schema_with_owner() -> StateSchema {
        StateSchema {
            contract_version: 1,
            schema_version: 1,
            application_id: "app".into(),
            revision_id: "rev".into(),
            collections: vec![CollectionSchema {
                name: "incidents".into(),
                version: 1,
                fields: vec![
                    FieldSchema {
                        name: "title".into(),
                        field_type: FieldType::Primitive {
                            primitive: PrimitiveType::String,
                        },
                        nullable: false,
                        required: true,
                        default: None,
                        constraints: Default::default(),
                        computed: None,
                    },
                    FieldSchema {
                        name: "owner_id".into(),
                        field_type: FieldType::Primitive {
                            primitive: PrimitiveType::String,
                        },
                        nullable: false,
                        required: false,
                        default: None,
                        constraints: Default::default(),
                        computed: None,
                    },
                ],
                indexes: vec![],
            }],
        }
    }

    fn db(name: &str) -> FeltDb {
        FeltDb::open(
            std::env::temp_dir().join(format!("state-contract-{name}-{}.log", crate::now_ms())),
        )
        .unwrap()
    }
    #[test]
    fn schema_hash_is_deterministic_and_invalid_schema_fails() {
        let a = schema();
        let mut b = a.clone();
        b.collections[0].fields.reverse();
        assert_eq!(schema_hash(&a).unwrap(), schema_hash(&b).unwrap());
        let mut invalid = a;
        invalid.collections[0].fields[0].required = true;
        invalid.collections[0].fields[0].nullable = true;
        assert!(!validate_schema(&invalid).valid);
    }
    #[test]
    fn compatibility_classifies_required_and_type_changes() {
        let a = schema();
        let mut b = a.clone();
        b.schema_version = 2;
        b.collections[0].fields.push(FieldSchema {
            name: "owner".into(),
            field_type: FieldType::Primitive {
                primitive: PrimitiveType::String,
            },
            nullable: false,
            required: true,
            default: None,
            constraints: Default::default(),
            computed: None,
        });
        assert_eq!(
            compare_schemas(&a, &b).classification,
            SchemaCompatibility::Destructive
        );
        b.collections[0].fields[0].field_type = FieldType::Primitive {
            primitive: PrimitiveType::Integer,
        };
        assert_eq!(
            compare_schemas(&a, &b).classification,
            SchemaCompatibility::Incompatible
        );
    }
    #[test]
    fn transaction_is_atomic_and_preconditions_are_structured() {
        let db = db("tx");
        let schema = schema();
        let request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: Some(0),
            authorization: auth(),
            preconditions: vec![],
            operations: vec![
                TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: "one".into(),
                    value: serde_json::json!({"title":"First"}),
                    if_version: None,
                },
                TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: "two".into(),
                    value: serde_json::json!({"title":"Second"}),
                    if_version: None,
                },
            ],
            fences: vec![],
        };
        let result = execute_transaction(&db, &schema, &request, None).unwrap();
        assert_eq!(result.state_after, 2);
        assert_eq!(db.export_snapshot().unwrap().rows.len(), 2);
        let mut stale = request;
        stale.transaction_id = Some("stale".into());
        stale.causal_parent = Some(0);
        assert_eq!(
            execute_transaction(&db, &schema, &stale, None)
                .unwrap_err()
                .code,
            "CONFLICT"
        );
    }
    /// A conditional write through the canonical contract has to mean the same
    /// thing it means everywhere else in FeltDB, or `putIfAbsent` and
    /// `updateIfVersion` become file-runtime privileges.
    ///
    /// The predicate under test is the document's `__version` -- the number a
    /// caller reads back -- not the row's internal storage sequence. Those are
    /// two different numbers that happen to share a name, and a fence on the
    /// wrong one is worse than no fence, because it reads as protection.
    #[test]
    fn a_record_precondition_fences_the_document_version() {
        let db = db("record-precondition");
        let schema = schema();
        let write = |id: &str,
                     transaction: &str,
                     title: &str,
                     preconditions: Vec<TransactionPrecondition>,
                     kind: TransactionOperationKind| {
            execute_transaction(
                &db,
                &schema,
                &TransactionRequest {
                    transaction_id: Some(transaction.into()),
                    tenant_id: "tenant".into(),
                    application_id: "app".into(),
                    revision_id: "rev".into(),
                    schema_version: 1,
                    state_namespace: None,
                    causal_parent: None,
                    authorization: auth(),
                    preconditions,
                    operations: vec![TransactionOperation {
                        kind,
                        collection: "incidents".into(),
                        id: id.into(),
                        value: serde_json::json!({ "title": title }),
                        if_version: None,
                    }],
                    fences: vec![],
                },
                None,
            )
        };
        let fence = |version: Option<u64>, absent: bool| {
            vec![TransactionPrecondition::Record {
                collection: "incidents".into(),
                id: "i1".into(),
                require_absent: absent,
                expected_version: version,
                expected_epoch: None,
                expected_lease_id: None,
            }]
        };
        let version = || {
            db.state_rows()
                .unwrap()
                .into_iter()
                .find(|row| row.capability == "app:incidents" && row.key == "i1" && !row.deleted)
                .and_then(|row| row.value.get("__version").and_then(Value::as_u64))
        };

        // A conditional create is a create at version 1.
        write(
            "i1",
            "create",
            "First",
            fence(None, true),
            TransactionOperationKind::Insert,
        )
        .expect("the record is absent, so the create commits");
        assert_eq!(version(), Some(1));

        // The same create again is a lost race. Whichever check catches it,
        // the answer is a conflict and nothing was written.
        let refused = write(
            "i1",
            "create-again",
            "Second",
            fence(None, true),
            TransactionOperationKind::Insert,
        )
        .expect_err("the record now exists");
        assert!(
            refused.code == "CONFLICT" || refused.code == "PRECONDITION_FAILED",
            "a lost creation race is a conflict, got {}",
            refused.code,
        );
        assert_eq!(version(), Some(1), "a refused transaction writes nothing");

        // A guard on a record the transaction does not write is evaluated
        // inside the commit lock, and says which predicate failed.
        let guarded = write(
            "i2",
            "guarded",
            "Other",
            fence(None, true),
            TransactionOperationKind::Insert,
        )
        .expect_err("i1 exists, so the guard fails and i2 is never written");
        assert_eq!(guarded.code, "PRECONDITION_FAILED");
        assert_eq!(
            guarded
                .failure
                .as_ref()
                .and_then(|value| value.get("predicate").and_then(Value::as_str)),
            Some("present"),
        );
        assert!(
            db.state_rows()
                .unwrap()
                .iter()
                .all(|row| row.key != "i2" || row.deleted),
            "a refused guard leaves the staged write unwritten",
        );

        // A stale fence conflicts and names what the authority actually holds.
        let stale = write(
            "i1",
            "stale",
            "Third",
            fence(Some(7), false),
            TransactionOperationKind::Update,
        )
        .expect_err("version 7 is not the version on record");
        assert_eq!(stale.code, "PRECONDITION_FAILED");
        assert_eq!(stale.expected, Some(Value::from(7)));
        assert_eq!(stale.actual, Some(Value::from(1)));
        assert_eq!(version(), Some(1));

        // The fenced write commits and the authority owns the next version.
        write(
            "i1",
            "fenced",
            "Fourth",
            fence(Some(1), false),
            TransactionOperationKind::Update,
        )
        .expect("version 1 is the version on record");
        assert_eq!(version(), Some(2));

        // Replaying a committed transaction id applies nothing, so it cannot
        // advance the version a second time.
        write(
            "i1",
            "fenced",
            "Fourth",
            fence(Some(1), false),
            TransactionOperationKind::Update,
        )
        .expect("a duplicate transaction id is not a failure");
        assert_eq!(version(), Some(2));
    }

    /// A precondition that constrains nothing is refused rather than accepted,
    /// because one that is silently ignored reads as protection and provides
    /// none.
    #[test]
    fn an_empty_record_precondition_is_refused() {
        let db = db("empty-precondition");
        let schema = schema();
        let failure = execute_transaction(
            &db,
            &schema,
            &TransactionRequest {
                transaction_id: Some("empty".into()),
                tenant_id: "tenant".into(),
                application_id: "app".into(),
                revision_id: "rev".into(),
                schema_version: 1,
                state_namespace: None,
                causal_parent: None,
                authorization: auth(),
                preconditions: vec![TransactionPrecondition::Record {
                    collection: "incidents".into(),
                    id: "i1".into(),
                    require_absent: false,
                    expected_version: None,
                    expected_epoch: None,
                    expected_lease_id: None,
                }],
                operations: vec![TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: "i1".into(),
                    value: serde_json::json!({ "title": "First" }),
                    if_version: None,
                }],
                fences: vec![],
            },
            None,
        )
        .expect_err("a precondition that constrains nothing is a caller error");
        assert_eq!(failure.code, "VALIDATION_FAILED");
        assert!(db.state_rows().unwrap().iter().all(|row| row.deleted));
    }

    #[test]
    fn validation_failure_rolls_back_every_operation() {
        let db = db("rollback");
        let schema = schema();
        let request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth(),
            preconditions: vec![],
            operations: vec![
                TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: "one".into(),
                    value: serde_json::json!({"title":"ok"}),
                    if_version: None,
                },
                TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: "two".into(),
                    value: serde_json::json!({"status":"open"}),
                    if_version: None,
                },
            ],
            fences: vec![],
        };
        assert_eq!(
            execute_transaction(&db, &schema, &request, None)
                .unwrap_err()
                .code,
            "VALIDATION_FAILED"
        );
        assert!(db.export_snapshot().unwrap().rows.is_empty());
    }
    #[test]
    fn query_filters_orders_projects_and_aggregates() {
        let db = db("query");
        let schema = schema();
        for (id, title, status) in [("1", "B", "open"), ("2", "A", "open"), ("3", "C", "closed")] {
            execute_transaction(
                &db,
                &schema,
                &TransactionRequest {
                    transaction_id: None,
                    tenant_id: "tenant".into(),
                    application_id: "app".into(),
                    revision_id: "rev".into(),
                    schema_version: 1,
                    state_namespace: None,
                    causal_parent: None,
                    authorization: auth(),
                    preconditions: vec![],
                    operations: vec![TransactionOperation {
                        kind: TransactionOperationKind::Insert,
                        collection: "incidents".into(),
                        id: id.into(),
                        value: serde_json::json!({"title":title,"status":status}),
                        if_version: None,
                    }],
                    fences: vec![],
                },
                None,
            )
            .unwrap();
        }
        let context = begin_read(&db, &schema, "app", auth()).unwrap();
        let result = execute_query(
            &db,
            &schema,
            &context,
            &CanonicalQuery {
                collection: "incidents".into(),
                filter: Some(QueryFilter::Eq {
                    field: "status".into(),
                    value: Value::String("open".into()),
                }),
                order_by: vec![QueryOrder {
                    field: "title".into(),
                    direction: SortDirection::Asc,
                }],
                limit: Some(1),
                offset: 0,
                cursor: None,
                projection: vec!["title".into()],
                group_by: vec!["status".into()],
                aggregates: vec![AggregateDefinition {
                    name: "total".into(),
                    function: AggregateFunction::Count,
                    field: None,
                }],
                references: vec![],
                semantic: None,
            },
            None,
        )
        .unwrap();
        assert_eq!(result.records, vec![serde_json::json!({"title":"A"})]);
        assert_eq!(result.aggregates[0]["total"], 2);
        assert_eq!(result.state_version, 3);
    }
    #[test]
    fn authorization_prevents_projection_before_read() {
        let db = db("auth");
        let schema = schema();
        let mut auth = auth();
        auth.field_projections
            .insert("incidents".into(), ["status".into()].into());
        let context = begin_read(&db, &schema, "app", auth).unwrap();
        let error = execute_query(
            &db,
            &schema,
            &context,
            &CanonicalQuery {
                collection: "incidents".into(),
                filter: None,
                order_by: vec![],
                limit: None,
                offset: 0,
                cursor: None,
                projection: vec!["title".into()],
                group_by: vec![],
                aggregates: vec![],
                references: vec![],
                semantic: None,
            },
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "UNAUTHORIZED_PROJECTION");
    }
    #[test]
    fn stale_record_precondition_is_structured_and_identity_is_idempotent() {
        let db = db("precondition");
        let schema = schema();
        let mut request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth(),
            preconditions: vec![],
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "one".into(),
                value: serde_json::json!({"title":"First"}),
                if_version: None,
            }],
            fences: vec![],
        };
        let first = execute_transaction(&db, &schema, &request, None).unwrap();
        let duplicate = execute_transaction(&db, &schema, &request, None).unwrap();
        assert_eq!(first.transaction_id, duplicate.transaction_id);
        assert!(duplicate.duplicate);
        request.transaction_id = Some("stale-update".into());
        request.operations = vec![TransactionOperation {
            kind: TransactionOperationKind::Update,
            collection: "incidents".into(),
            id: "one".into(),
            value: serde_json::json!({"title":"Changed"}),
            if_version: Some(99),
        }];
        let failure = execute_transaction(&db, &schema, &request, None).unwrap_err();
        assert_eq!(failure.code, "PRECONDITION_FAILED");
        assert_eq!(failure.expected, Some(Value::from(99)));
        assert_eq!(failure.actual, Some(Value::from(1)));
    }
    #[test]
    fn stale_authority_revision_is_a_structured_precondition_failure() {
        let db = db("authority-revision-precondition");
        let schema = schema();
        let mut request = TransactionRequest {
            transaction_id: Some("initial".into()),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth(),
            preconditions: vec![],
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "one".into(),
                value: serde_json::json!({"title":"First"}),
                if_version: None,
            }],
            fences: vec![],
        };
        execute_transaction(&db, &schema, &request, None).unwrap();
        request.transaction_id = Some("stale-parent".into());
        request.causal_parent = Some(0);
        request.operations[0].id = "two".into();

        let failure = execute_transaction(&db, &schema, &request, None).unwrap_err();
        assert_eq!(failure.code, "PRECONDITION_FAILED");
        assert_eq!(failure.resource.as_deref(), Some("authority_revision"));
        assert_eq!(failure.expected, Some(Value::from(0)));
        assert_eq!(failure.actual, Some(Value::from(1)));
    }
    #[test]
    fn explicit_transaction_id_is_bound_to_canonical_payload_across_restart() {
        let path = std::env::temp_dir().join(format!(
            "feltdb-content-id-{}-{}.log",
            std::process::id(),
            crate::now_ms()
        ));
        let schema = schema();
        let request = TransactionRequest {
            transaction_id: Some("stable-client-id".into()),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth(),
            preconditions: vec![],
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "content-bound".into(),
                value: serde_json::json!({"title":"Original"}),
                if_version: None,
            }],
            fences: vec![],
        };
        {
            let db = crate::open(&path).unwrap();
            assert!(
                !execute_transaction(&db, &schema, &request, None)
                    .unwrap()
                    .duplicate
            );
            assert!(
                execute_transaction(&db, &schema, &request, None)
                    .unwrap()
                    .duplicate
            );
        }
        let reopened = crate::open(&path).unwrap();
        assert!(
            execute_transaction(&reopened, &schema, &request, None)
                .unwrap()
                .duplicate
        );
        let mut changed = request.clone();
        changed.operations[0].value = serde_json::json!({"title":"Changed"});
        let failure = execute_transaction(&reopened, &schema, &changed, None).unwrap_err();
        assert_eq!(failure.code, "IDEMPOTENCY_CONFLICT");
        assert!(failure.message.contains("different canonical payload"));
        std::fs::remove_file(path).ok();
    }
    #[test]
    fn explicit_transaction_id_is_isolated_between_applications() {
        fn application(
            app: &str,
            namespace: &str,
            transaction: &str,
            record: &str,
        ) -> (StateSchema, TransactionRequest) {
            let mut schema = schema();
            schema.application_id = app.into();
            let mut authorization = auth();
            authorization.application_id = app.into();
            let request = TransactionRequest {
                transaction_id: Some(transaction.into()),
                tenant_id: "tenant".into(),
                application_id: app.into(),
                revision_id: "rev".into(),
                schema_version: 1,
                state_namespace: Some(namespace.into()),
                causal_parent: None,
                authorization,
                preconditions: vec![],
                operations: vec![TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: record.into(),
                    value: serde_json::json!({"title":format!("{app}-{record}")}),
                    if_version: None,
                }],
                fences: vec![],
            };
            (schema, request)
        }
        for (name, order) in [
            (
                "a-then-b",
                vec![("app-a", "shared-1", "one"), ("app-b", "shared-1", "one")],
            ),
            (
                "b-then-a",
                vec![("app-b", "shared-1", "one"), ("app-a", "shared-1", "one")],
            ),
            (
                "interleaved",
                vec![
                    ("app-a", "shared-1", "one"),
                    ("app-b", "shared-1", "one"),
                    ("app-a", "shared-2", "two"),
                    ("app-b", "shared-2", "two"),
                ],
            ),
        ] {
            let path = std::env::temp_dir().join(format!(
                "feltdb-app-isolation-{name}-{}-{}.log",
                std::process::id(),
                crate::now_ms()
            ));
            {
                let db = crate::open(&path).unwrap();
                for (app, transaction, record) in &order {
                    let (schema, request) =
                        application(app, &format!("{app}:production"), transaction, record);
                    assert!(
                        !execute_transaction(&db, &schema, &request, None)
                            .unwrap()
                            .duplicate
                    );
                }
            }
            let reopened = crate::open(&path).unwrap();
            for (app, transaction, record) in &order {
                let (schema, request) =
                    application(app, &format!("{app}:production"), transaction, record);
                assert!(
                    execute_transaction(&reopened, &schema, &request, None)
                        .unwrap()
                        .duplicate
                );
                let rows = reopened.state_rows().unwrap();
                assert_eq!(
                    rows.iter()
                        .filter(
                            |row| row.capability == format!("{app}:production:incidents")
                                && row.key == *record
                                && !row.deleted
                        )
                        .count(),
                    1
                );
            }
            std::fs::remove_file(path).ok();
        }
    }
    #[test]
    fn read_context_remains_a_stable_snapshot() {
        let db = db("snapshot-read");
        let schema = schema();
        let insert = |id: &str| TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth(),
            preconditions: vec![],
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: id.into(),
                value: serde_json::json!({"title":id}),
                if_version: None,
            }],
            fences: vec![],
        };
        execute_transaction(&db, &schema, &insert("one"), None).unwrap();
        let context = begin_read(&db, &schema, "app", auth()).unwrap();
        execute_transaction(&db, &schema, &insert("two"), None).unwrap();
        let result = execute_query(
            &db,
            &schema,
            &context,
            &CanonicalQuery {
                collection: "incidents".into(),
                filter: None,
                order_by: vec![],
                limit: None,
                offset: 0,
                cursor: None,
                projection: vec![],
                group_by: vec![],
                aggregates: vec![],
                references: vec![],
                semantic: None,
            },
            None,
        )
        .unwrap();
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.state_version, 1);
    }
    #[test]
    fn transaction_log_envelope_replays_all_rows() {
        let path =
            std::env::temp_dir().join(format!("state-contract-replay-{}.log", crate::now_ms()));
        let schema = schema();
        {
            let db = FeltDb::open(&path).unwrap();
            let request = TransactionRequest {
                transaction_id: Some("replay-tx".into()),
                tenant_id: "tenant".into(),
                application_id: "app".into(),
                revision_id: "rev".into(),
                schema_version: 1,
                state_namespace: None,
                causal_parent: None,
                authorization: auth(),
                preconditions: vec![],
                operations: vec![
                    TransactionOperation {
                        kind: TransactionOperationKind::Insert,
                        collection: "incidents".into(),
                        id: "one".into(),
                        value: serde_json::json!({"title":"One"}),
                        if_version: None,
                    },
                    TransactionOperation {
                        kind: TransactionOperationKind::Insert,
                        collection: "incidents".into(),
                        id: "two".into(),
                        value: serde_json::json!({"title":"Two"}),
                        if_version: None,
                    },
                ],
                fences: vec![],
            };
            execute_transaction(&db, &schema, &request, None).unwrap();
        }
        let reopened = FeltDb::open(path).unwrap();
        assert_eq!(reopened.export_snapshot().unwrap().rows.len(), 2);
        assert_eq!(reopened.current_revision().unwrap(), 1);
    }
    #[test]
    fn indexed_equality_query_uses_index_not_scan() {
        let db = db("regression-indexed-equality");
        let schema = schema();
        let request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth(),
            preconditions: vec![],
            operations: (1..=100)
                .map(|i| TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: format!("incident-{}", i),
                    value: serde_json::json!({
                        "title": format!("Incident {}", i),
                        "status": if i % 2 == 0 { "open" } else { "closed" }
                    }),
                    if_version: None,
                })
                .collect(),
            fences: vec![],
        };
        execute_transaction(&db, &schema, &request, None).unwrap();
        let context = begin_query_read(&db, &schema, "app", auth()).unwrap();
        let result = execute_query(
            &db,
            &schema,
            &context,
            &CanonicalQuery {
                collection: "incidents".into(),
                filter: Some(QueryFilter::Eq {
                    field: "status".into(),
                    value: Value::String("open".into()),
                }),
                order_by: vec![],
                limit: Some(100),
                offset: 0,
                cursor: None,
                projection: vec![],
                group_by: vec![],
                aggregates: vec![],
                references: vec![],
                semantic: None,
            },
            None,
        )
        .unwrap();
        assert_eq!(result.plan.access_method, Some("index".to_string()));
        assert_eq!(result.plan.index, Some("status".to_string()));
        assert!(!result.plan.scan_fallback);
        assert_eq!(result.plan.implementation, "index_lookup");
        assert!(result.plan.actual_rows_scanned.unwrap() <= 100);
        assert_eq!(
            result.plan.index_metrics.as_ref().unwrap().index_used,
            Some("status".to_string())
        );
        assert!(result.plan.index_metrics.as_ref().unwrap().index_hits > 0);
    }
    #[test]
    fn non_indexed_field_query_falls_back_to_scan() {
        let db = db("regression-non-indexed");
        let schema = schema();
        let request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth(),
            preconditions: vec![],
            operations: (1..=50)
                .map(|i| TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: format!("incident-{}", i),
                    value: serde_json::json!({
                        "title": format!("Incident {}", i),
                        "status": "open"
                    }),
                    if_version: None,
                })
                .collect(),
            fences: vec![],
        };
        execute_transaction(&db, &schema, &request, None).unwrap();
        let context = begin_read(&db, &schema, "app", auth()).unwrap();
        let result = execute_query(
            &db,
            &schema,
            &context,
            &CanonicalQuery {
                collection: "incidents".into(),
                filter: Some(QueryFilter::Eq {
                    field: "title".into(),
                    value: Value::String("Incident 25".into()),
                }),
                order_by: vec![],
                limit: Some(100),
                offset: 0,
                cursor: None,
                projection: vec![],
                group_by: vec![],
                aggregates: vec![],
                references: vec![],
                semantic: None,
            },
            None,
        )
        .unwrap();
        assert_eq!(
            result.plan.access_method,
            Some("sequential_scan".to_string())
        );
        assert!(result.plan.scan_fallback);
        assert_eq!(result.plan.implementation, "full_table_scan");
        assert_eq!(
            result.plan.index_metrics.as_ref().unwrap().fallback_used,
            true
        );
        assert_eq!(
            result.plan.index_metrics.as_ref().unwrap().fallback_reason,
            Some("no_applicable_index".to_string())
        );
    }
    #[test]
    fn timing_measurements_are_populated() {
        let db = db("regression-timing");
        let schema = schema();
        let request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth(),
            preconditions: vec![],
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "test-incident".into(),
                value: serde_json::json!({"title": "Test", "status": "open"}),
                if_version: None,
            }],
            fences: vec![],
        };
        execute_transaction(&db, &schema, &request, None).unwrap();
        let context = begin_query_read(&db, &schema, "app", auth()).unwrap();
        let result = execute_query(
            &db,
            &schema,
            &context,
            &CanonicalQuery {
                collection: "incidents".into(),
                filter: Some(QueryFilter::Eq {
                    field: "status".into(),
                    value: Value::String("open".into()),
                }),
                order_by: vec![],
                limit: Some(100),
                offset: 0,
                cursor: None,
                projection: vec![],
                group_by: vec![],
                aggregates: vec![],
                references: vec![],
                semantic: None,
            },
            None,
        )
        .unwrap();
        assert!(result.plan.timing.is_some());
        let timing = result.plan.timing.unwrap();
        assert!(timing.planning_ms.is_some());
        assert!(timing.filtering_ms.is_some());
        assert!(timing.serialization_ms.is_some());
        assert!(timing.total_ms.is_some());
        assert!(timing.total_ms.unwrap() > 0.0);
    }
    #[test]
    fn index_metrics_track_hits_and_misses() {
        let db = db("regression-metrics");
        let schema = schema();
        let request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth(),
            preconditions: vec![],
            operations: (1..=20)
                .map(|i| TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: format!("incident-{}", i),
                    value: serde_json::json!({
                        "title": format!("Incident {}", i),
                        "status": if i <= 10 { "open" } else { "closed" }
                    }),
                    if_version: None,
                })
                .collect(),
            fences: vec![],
        };
        execute_transaction(&db, &schema, &request, None).unwrap();
        let context = begin_query_read(&db, &schema, "app", auth()).unwrap();
        let result = execute_query(
            &db,
            &schema,
            &context,
            &CanonicalQuery {
                collection: "incidents".into(),
                filter: Some(QueryFilter::Eq {
                    field: "status".into(),
                    value: Value::String("open".into()),
                }),
                order_by: vec![],
                limit: Some(100),
                offset: 0,
                cursor: None,
                projection: vec![],
                group_by: vec![],
                aggregates: vec![],
                references: vec![],
                semantic: None,
            },
            None,
        )
        .unwrap();
        let metrics = result.plan.index_metrics.as_ref().unwrap();
        assert_eq!(metrics.index_used, Some("status".to_string()));
        assert_eq!(metrics.index_hits, 10);
        assert_eq!(metrics.index_misses, 0);
        assert!(!metrics.fallback_used);
    }

    #[test]
    fn scoped_cursor_is_complete_deterministic_and_bound_to_scope_and_collection() {
        let db = db("scoped-pagination");
        let schema = scoped_schema();
        let operations = (0..237)
            .map(|i| (format!("a-{i:03}"), "tenant-a"))
            .chain((0..1000).map(|i| (format!("b-{i:04}"), "tenant-b")))
            .map(|(id, scope)| TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: id.clone(),
                value: serde_json::json!({"title": id, "status": scope}),
                if_version: None,
            })
            .collect();
        execute_transaction(
            &db,
            &schema,
            &TransactionRequest {
                transaction_id: Some("seed-scopes".into()),
                tenant_id: "tenant".into(),
                application_id: "app".into(),
                revision_id: "rev".into(),
                schema_version: 1,
                state_namespace: None,
                causal_parent: None,
                authorization: auth(),
                preconditions: vec![],
                operations,
                fences: vec![],
            },
            None,
        )
        .unwrap();

        let context = begin_query_read(&db, &schema, "app", auth()).unwrap();
        let mut query = CanonicalQuery {
            collection: "incidents".into(),
            filter: Some(QueryFilter::Eq {
                field: "status".into(),
                value: Value::String("tenant-a".into()),
            }),
            order_by: vec![],
            limit: Some(25),
            offset: 0,
            cursor: None,
            projection: vec![],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };
        let mut ids = Vec::new();
        let first_cursor = loop {
            let page = execute_query(&db, &schema, &context, &query, None).unwrap();
            assert!(page.records.len() <= 25);
            assert!(
                page.records
                    .iter()
                    .all(|record| record["status"] == "tenant-a")
            );
            ids.extend(
                page.records
                    .iter()
                    .map(|record| record["_id"].as_str().unwrap().to_owned()),
            );
            let Some(cursor) = page.next_cursor else {
                break query.cursor.clone();
            };
            query.cursor = Some(cursor);
        };
        assert_eq!(ids.len(), 237);
        let unique = ids.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), 237);
        assert!(first_cursor.is_some());

        let cursor = encode_query_cursor(
            &pagination_query_hash(&CanonicalQuery {
                cursor: None,
                ..query.clone()
            })
            .unwrap(),
            25,
        );
        let mut wrong_scope = query.clone();
        wrong_scope.cursor = Some(cursor.clone());
        wrong_scope.filter = Some(QueryFilter::Eq {
            field: "status".into(),
            value: Value::String("tenant-b".into()),
        });
        assert_eq!(
            execute_query(&db, &schema, &context, &wrong_scope, None)
                .unwrap_err()
                .code,
            "INVALID_CURSOR"
        );
        let mut malformed = query;
        malformed.cursor = Some("not-a-cursor".into());
        assert_eq!(
            execute_query(&db, &schema, &context, &malformed, None)
                .unwrap_err()
                .code,
            "INVALID_CURSOR"
        );
    }

    /// Every query surface must either implement a recognized semantic clause
    /// per `docs/reference/semantic-query.md` or refuse it. This surface
    /// refuses: a semantic clause is `SEMANTIC_UNSUPPORTED` before any
    /// deterministic work, a malformed one is `INVALID_QUERY`, nothing is
    /// read, nothing is judged, and the same query without the clause still
    /// answers.
    #[test]
    fn semantic_clause_is_recognized_and_refused_never_ignored() {
        let db = db("semantic-refusal");
        let schema = schema();
        execute_transaction(
            &db,
            &schema,
            &TransactionRequest {
                transaction_id: Some("seed".into()),
                tenant_id: "tenant".into(),
                application_id: "app".into(),
                revision_id: "rev".into(),
                schema_version: 1,
                state_namespace: None,
                causal_parent: None,
                authorization: auth(),
                preconditions: vec![],
                operations: vec![TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: "i1".into(),
                    value: serde_json::json!({ "title": "First", "status": "open" }),
                    if_version: None,
                }],
                fences: vec![],
            },
            None,
        )
        .unwrap();
        let context = begin_query_read(&db, &schema, "app", auth()).unwrap();
        let plain = CanonicalQuery {
            collection: "incidents".into(),
            filter: Some(QueryFilter::Eq {
                field: "status".into(),
                value: Value::String("open".into()),
            }),
            order_by: vec![],
            limit: Some(10),
            offset: 0,
            cursor: None,
            projection: vec![],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };
        let state_before = db.state_rows().unwrap().len();

        // A semantic clause in the bounded query's own grammar, and one that
        // only expresses semantic intent, are both recognized and refused.
        for clause in [
            serde_json::json!({
                "decision": { "kind": "binary", "predicate": "needs a human" },
                "filter": { "is": true },
            }),
            serde_json::json!({ "decision": "needs_human" }),
            serde_json::json!({}),
        ] {
            let failure = execute_query(
                &db,
                &schema,
                &context,
                &CanonicalQuery {
                    semantic: Some(clause),
                    ..plain.clone()
                },
                None,
            )
            .expect_err("semantic intent is never silently dropped");
            assert_eq!(failure.code, "SEMANTIC_UNSUPPORTED");
        }

        // A semantic value that is not a clause is a validation error, not a
        // recognized-but-unsupported operation.
        for malformed in [
            serde_json::json!("yes"),
            serde_json::json!(true),
            serde_json::json!(1),
            serde_json::json!(["needs_human"]),
        ] {
            let failure = execute_query(
                &db,
                &schema,
                &context,
                &CanonicalQuery {
                    semantic: Some(malformed),
                    ..plain.clone()
                },
                None,
            )
            .expect_err("a malformed semantic value is invalid");
            assert_eq!(failure.code, "INVALID_QUERY");
        }

        // The clause is refused ahead of every other validation, so a query
        // that is also otherwise invalid still fails on semantic intent.
        let failure = execute_query(
            &db,
            &schema,
            &context,
            &CanonicalQuery {
                semantic: Some(serde_json::json!({ "decision": "needs_human" })),
                limit: Some(0),
                ..plain.clone()
            },
            None,
        )
        .unwrap_err();
        assert_eq!(failure.code, "SEMANTIC_UNSUPPORTED");

        // Refusal reads nothing, judges nothing, and writes nothing: no
        // judgment collection appears and the state is what it was.
        assert_eq!(db.state_rows().unwrap().len(), state_before);
        let judgments: Vec<Value> = db
            .capability(crate::semantic_decision::DEFAULT_DECISION_COLLECTION)
            .find(|_: &Value| true)
            .unwrap();
        assert!(judgments.is_empty());

        // The same query without the clause answers as before.
        let page = execute_query(&db, &schema, &context, &plain, None).unwrap();
        assert_eq!(page.records.len(), 1);
    }

    /// Over the wire, `semantic` is carried into the query rather than lost
    /// as an unknown key, `null` means absent, and an unrelated unknown key
    /// keeps its existing behavior of being ignored: only semantic intent is
    /// recognized, because only semantic intent changes what a query means.
    #[test]
    fn semantic_clause_survives_deserialization_and_changes_query_identity() {
        let plain: CanonicalQuery = serde_json::from_value(serde_json::json!({
            "collection": "incidents",
            "limit": 10,
        }))
        .unwrap();
        assert!(plain.semantic.is_none());
        let with_clause: CanonicalQuery = serde_json::from_value(serde_json::json!({
            "collection": "incidents",
            "limit": 10,
            "semantic": { "decision": "needs_human" },
        }))
        .unwrap();
        assert_eq!(
            with_clause.semantic,
            Some(serde_json::json!({ "decision": "needs_human" }))
        );
        assert_ne!(
            query_hash(&plain).unwrap(),
            query_hash(&with_clause).unwrap(),
            "a semantic clause is part of what the query is"
        );
        let malformed: CanonicalQuery = serde_json::from_value(serde_json::json!({
            "collection": "incidents",
            "semantic": "yes",
        }))
        .unwrap();
        assert_eq!(malformed.semantic, Some(Value::String("yes".into())));
        let null: CanonicalQuery = serde_json::from_value(serde_json::json!({
            "collection": "incidents",
            "semantic": null,
        }))
        .unwrap();
        assert!(null.semantic.is_none());
        let unrelated: CanonicalQuery = serde_json::from_value(serde_json::json!({
            "collection": "incidents",
            "limit": 10,
            "refill": true,
        }))
        .unwrap();
        assert_eq!(query_hash(&unrelated).unwrap(), query_hash(&plain).unwrap());
        // Serialization of a query without a clause is unchanged, so every
        // existing query hash and cursor binding is unchanged.
        assert!(!serde_json::to_string(&plain).unwrap().contains("semantic"));
    }

    #[test]
    fn scoped_index_work_is_independent_of_unrelated_growth() {
        fn examined(unrelated: usize) -> usize {
            let db = db(&format!("scope-growth-{unrelated}"));
            let schema = scoped_schema();
            let operations = (0..20)
                .map(|i| (format!("target-{i}"), "tenant-a"))
                .chain((0..unrelated).map(|i| (format!("other-{i}"), "tenant-b")))
                .map(|(id, status)| TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: id.clone(),
                    value: serde_json::json!({"title": id, "status": status}),
                    if_version: None,
                })
                .collect();
            execute_transaction(
                &db,
                &schema,
                &TransactionRequest {
                    transaction_id: None,
                    tenant_id: "tenant".into(),
                    application_id: "app".into(),
                    revision_id: "rev".into(),
                    schema_version: 1,
                    state_namespace: None,
                    causal_parent: None,
                    authorization: auth(),
                    preconditions: vec![],
                    operations,
                    fences: vec![],
                },
                None,
            )
            .unwrap();
            let context = begin_query_read(&db, &schema, "app", auth()).unwrap();
            let result = execute_query(
                &db,
                &schema,
                &context,
                &CanonicalQuery {
                    collection: "incidents".into(),
                    filter: Some(QueryFilter::Eq {
                        field: "status".into(),
                        value: Value::String("tenant-a".into()),
                    }),
                    order_by: vec![],
                    limit: Some(20),
                    offset: 0,
                    cursor: None,
                    projection: vec![],
                    group_by: vec![],
                    aggregates: vec![],
                    references: vec![],
                    semantic: None,
                },
                None,
            )
            .unwrap();
            assert_eq!(result.records.len(), 20);
            result.plan.actual_rows_scanned.unwrap()
        }
        assert_eq!(
            [examined(100), examined(1000), examined(10_000)],
            [20, 20, 20]
        );
    }

    #[test]
    fn managed_scope_without_schema_index_fails_closed() {
        let db = db("scope-missing-index");
        let mut schema = schema();
        schema.collections[0].fields[0].name = "tenant_id".into();
        execute_transaction(
            &db,
            &schema,
            &TransactionRequest {
                transaction_id: None,
                tenant_id: "tenant".into(),
                application_id: "app".into(),
                revision_id: "rev".into(),
                schema_version: 1,
                state_namespace: None,
                causal_parent: None,
                authorization: auth(),
                preconditions: vec![],
                operations: vec![TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: "a".into(),
                    value: serde_json::json!({"tenant_id":"tenant-a"}),
                    if_version: None,
                }],
                fences: vec![],
            },
            None,
        )
        .unwrap();
        let context = begin_query_read(&db, &schema, "app", auth()).unwrap();
        let failure = execute_query(
            &db,
            &schema,
            &context,
            &CanonicalQuery {
                collection: "incidents".into(),
                filter: Some(QueryFilter::Eq {
                    field: "tenant_id".into(),
                    value: Value::String("tenant-a".into()),
                }),
                order_by: vec![],
                limit: Some(20),
                offset: 0,
                cursor: None,
                projection: vec![],
                group_by: vec![],
                aggregates: vec![],
                references: vec![],
                semantic: None,
            },
            None,
        )
        .unwrap_err();
        assert_eq!(failure.code, "INDEX_UNAVAILABLE");
    }
    #[test]
    #[ignore]
    fn gate_a_46_collection_sherpa_workload_benchmark() {
        use std::time::Instant;
        fn sherpa_schema() -> StateSchema {
            let mut collections = vec![];
            for i in 1..=46 {
                collections.push(CollectionSchema {
                    name: format!("collection_{}", i),
                    version: 1,
                    fields: vec![
                        FieldSchema {
                            name: "id".into(),
                            field_type: FieldType::Primitive {
                                primitive: PrimitiveType::String,
                            },
                            nullable: false,
                            required: true,
                            default: None,
                            constraints: Default::default(),
                            computed: None,
                        },
                        FieldSchema {
                            name: "name".into(),
                            field_type: FieldType::Primitive {
                                primitive: PrimitiveType::String,
                            },
                            nullable: false,
                            required: true,
                            default: None,
                            constraints: Default::default(),
                            computed: None,
                        },
                        FieldSchema {
                            name: "status".into(),
                            field_type: FieldType::Enum {
                                values: vec!["active".into(), "inactive".into()],
                            },
                            nullable: false,
                            required: false,
                            default: Some(Value::String("active".into())),
                            constraints: Default::default(),
                            computed: None,
                        },
                    ],
                    indexes: vec![
                        SchemaIndex {
                            name: "id_idx".into(),
                            fields: vec!["id".into()],
                            unique: false,
                            sparse: false,
                            version: 1,
                        },
                        SchemaIndex {
                            name: "status_idx".into(),
                            fields: vec!["status".into()],
                            unique: false,
                            sparse: false,
                            version: 1,
                        },
                    ],
                });
            }
            StateSchema {
                contract_version: 1,
                schema_version: 1,
                application_id: "app".into(),
                revision_id: "rev".into(),
                collections,
            }
        }
        let db = db("gate-a-sherpa-bench");
        let schema = sherpa_schema();
        let auth_ctx = auth();
        println!("Gate A Benchmark: Loading 46 collections...");
        let load_start = Instant::now();
        for col_idx in 1..=46 {
            let collection_name = format!("collection_{}", col_idx);
            let records_per_collection = 500;
            let mut operations = vec![];
            for record_idx in 1..=records_per_collection {
                operations.push(TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: collection_name.clone(),
                    id: format!("rec-{}", record_idx),
                    value: serde_json::json!({
                        "id": format!("id-{}", record_idx),
                        "name": format!("Record {}", record_idx),
                        "status": if record_idx % 2 == 0 { "active" } else { "inactive" }
                    }),
                    if_version: None,
                });
            }
            let request = TransactionRequest {
                transaction_id: None,
                tenant_id: "tenant".into(),
                application_id: "app".into(),
                revision_id: "rev".into(),
                schema_version: 1,
                state_namespace: None,
                causal_parent: None,
                authorization: auth_ctx.clone(),
                preconditions: vec![],
                operations,
                fences: vec![],
            };
            execute_transaction(&db, &schema, &request, None).unwrap();
        }
        let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;
        println!("  Loaded ~23,000 records in {:.2}ms", load_ms);
        println!("Gate A Benchmark: Executing 46 indexed queries...");
        let query_start = Instant::now();
        let context = begin_read(&db, &schema, "app", auth_ctx).unwrap();
        let mut query_times = vec![];
        let mut index_used_count = 0;
        for col_idx in 1..=46 {
            let collection_name = format!("collection_{}", col_idx);
            let q_start = Instant::now();
            let result = execute_query(
                &db,
                &schema,
                &context,
                &CanonicalQuery {
                    collection: collection_name,
                    filter: Some(QueryFilter::Eq {
                        field: "status".into(),
                        value: Value::String("active".into()),
                    }),
                    order_by: vec![],
                    limit: Some(1000),
                    offset: 0,
                    cursor: None,
                    projection: vec![],
                    group_by: vec![],
                    aggregates: vec![],
                    references: vec![],
                    semantic: None,
                },
                None,
            )
            .unwrap();
            let q_ms = q_start.elapsed().as_secs_f64() * 1000.0;
            query_times.push(q_ms);
            assert_eq!(
                result.plan.access_method,
                Some("index".to_string()),
                "Query {} should use index",
                col_idx
            );
            if result
                .plan
                .index_metrics
                .as_ref()
                .unwrap()
                .index_used
                .is_some()
            {
                index_used_count += 1;
            }
        }
        let total_query_time_ms = query_start.elapsed().as_secs_f64() * 1000.0;
        query_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median_query_time = query_times[query_times.len() / 2];
        println!("\n=== Gate A Verification ===");
        println!("Indexes used: {}/46", index_used_count);
        println!(
            "Total query time: {:.2}ms (target: <10,000ms)",
            total_query_time_ms
        );
        println!(
            "Per-query median: {:.2}ms (target: <100ms)",
            median_query_time
        );
        assert_eq!(index_used_count, 46, "All queries must use indexes");
        assert!(
            total_query_time_ms < 10000.0,
            "Total time must be <10s (was {:.2}ms)",
            total_query_time_ms
        );
    }

    #[test]
    fn authenticated_policy_allows_query_with_capability() {
        let db = db("auth_read_allow");
        let schema = schema();

        // Actor with "state:read" capability
        let mut auth_context = auth();
        auth_context.subject = "user:alice".into();

        let context = begin_read(&db, &schema, "app", auth_context).unwrap();
        let query = CanonicalQuery {
            collection: "incidents".into(),
            filter: None,
            order_by: vec![],
            limit: Some(100),
            offset: 0,
            cursor: None,
            projection: vec![],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };

        let result = execute_query(&db, &schema, &context, &query, None);
        assert!(
            result.is_ok(),
            "Query should succeed with state:read capability"
        );
    }

    #[test]
    fn authenticated_policy_denies_query_without_capability() {
        let db = db("auth_read_deny");
        let schema = schema();

        // Actor without "state:read" capability (unauthenticated equivalent)
        let auth_context = AuthorizationContext {
            subject: "user:bob".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: BTreeSet::new(), // No capabilities
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let context = begin_read(&db, &schema, "app", auth_context).unwrap();
        let query = CanonicalQuery {
            collection: "incidents".into(),
            filter: None,
            order_by: vec![],
            limit: Some(100),
            offset: 0,
            cursor: None,
            projection: vec![],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };

        let result = execute_query(&db, &schema, &context, &query, None);
        assert!(
            result.is_err(),
            "Query should fail without state:read capability"
        );
        if let Err(e) = result {
            assert_eq!(e.code, "AUTHORIZATION_DENIED");
        }
    }

    #[test]
    fn write_policy_requires_state_write_capability() {
        let db = db("auth_write_deny");
        let schema = schema();

        // Transaction without state:write capability
        let auth_context = AuthorizationContext {
            subject: "user:charlie".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: BTreeSet::new(), // No capabilities
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth_context,
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "proj-1".into(),
                value: serde_json::json!({"title": "Test Project"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };

        let result = execute_transaction(&db, &schema, &request, None);
        assert!(
            result.is_err(),
            "Transaction should fail without state:write capability"
        );
        if let Err(e) = result {
            assert_eq!(e.code, "AUTHORIZATION_DENIED");
        }
    }

    // PR #2.1 Verification Tests: Prove runtime enforcement works end-to-end

    #[test]
    fn policy_enforcement_unauthenticated_read_denied() {
        // Test: Unauthenticated actor cannot read even with valid query
        let db = db("policy_unauth_read");
        let schema = schema();

        // Pre-populate data
        let insert_request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth(),
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "inc-1".into(),
                value: serde_json::json!({"title": "Test Incident"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };
        execute_transaction(&db, &schema, &insert_request, None).unwrap();

        // Unauthenticated read attempt (no capabilities)
        let unauth_context = AuthorizationContext {
            subject: "anonymous".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: BTreeSet::new(), // No capabilities
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let context = begin_read(&db, &schema, "app", unauth_context).unwrap();
        let query = CanonicalQuery {
            collection: "incidents".into(),
            filter: None,
            order_by: vec![],
            limit: Some(100),
            offset: 0,
            cursor: None,
            projection: vec![],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };

        let result = execute_query(&db, &schema, &context, &query, None);
        assert!(result.is_err(), "Unauthenticated read must be denied");
        assert_eq!(result.unwrap_err().code, "AUTHORIZATION_DENIED");
    }

    #[test]
    fn policy_enforcement_unauthenticated_write_denied() {
        // Test: Unauthenticated actor cannot write
        let db = db("policy_unauth_write");
        let schema = schema();

        let unauth_context = AuthorizationContext {
            subject: "anonymous".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: BTreeSet::new(), // No capabilities
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: unauth_context,
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "inc-1".into(),
                value: serde_json::json!({"title": "Incident"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };

        let result = execute_transaction(&db, &schema, &request, None);
        assert!(result.is_err(), "Unauthenticated write must be denied");
        assert_eq!(result.unwrap_err().code, "AUTHORIZATION_DENIED");
    }

    #[test]
    fn policy_enforcement_authenticated_read_allowed() {
        // Test: Authenticated actor can read with proper capability
        let db = db("policy_auth_read_ok");
        let schema = schema();

        // Pre-populate data
        let insert_request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth(),
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "inc-1".into(),
                value: serde_json::json!({"title": "Test Incident"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };
        execute_transaction(&db, &schema, &insert_request, None).unwrap();

        // Authenticated read (with state:read capability)
        let auth_context = AuthorizationContext {
            subject: "user:alice".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:read".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let context = begin_read(&db, &schema, "app", auth_context).unwrap();
        let query = CanonicalQuery {
            collection: "incidents".into(),
            filter: None,
            order_by: vec![],
            limit: Some(100),
            offset: 0,
            cursor: None,
            projection: vec![],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };

        let result = execute_query(&db, &schema, &context, &query, None);
        assert!(result.is_ok(), "Authenticated read must be allowed");
        let query_result = result.unwrap();
        assert!(
            query_result.records.len() >= 1,
            "Query should return the inserted record"
        );
    }

    #[test]
    fn policy_enforcement_authenticated_write_allowed() {
        // Test: Authenticated actor can write with proper capability
        let db = db("policy_auth_write_ok");
        let schema = schema();

        let auth_context = AuthorizationContext {
            subject: "user:bob".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:write".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth_context,
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "inc-1".into(),
                value: serde_json::json!({"title": "New Incident"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };

        let result = execute_transaction(&db, &schema, &request, None);
        assert!(result.is_ok(), "Authenticated write must be allowed");
        assert!(
            result.unwrap().state_after > 0,
            "Transaction should have succeeded"
        );
    }

    #[test]
    fn policy_enforcement_transaction_atomicity_with_auth() {
        // Test: Transaction authorization is checked before any operations commit
        let db = db("policy_tx_atomic");
        let schema = schema();

        // Start with one record
        let insert_initial = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: auth(),
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "initial".into(),
                value: serde_json::json!({"title": "Initial"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };
        let initial_result = execute_transaction(&db, &schema, &insert_initial, None).unwrap();
        let state_before_denied_tx = initial_result.state_after;

        // Attempt transaction without authorization
        let unauth_context = AuthorizationContext {
            subject: "anonymous".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: BTreeSet::new(), // No capabilities
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let denied_request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: unauth_context,
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "denied".into(),
                value: serde_json::json!({"title": "Should Not Commit"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };

        let denied_result = execute_transaction(&db, &schema, &denied_request, None);
        assert!(denied_result.is_err(), "Transaction should be denied");

        // Verify state hasn't changed
        let verify_auth = auth();
        let verify_context = begin_read(&db, &schema, "app", verify_auth).unwrap();
        let verify_query = CanonicalQuery {
            collection: "incidents".into(),
            filter: None,
            order_by: vec![],
            limit: Some(100),
            offset: 0,
            cursor: None,
            projection: vec![],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };

        let verify_result =
            execute_query(&db, &schema, &verify_context, &verify_query, None).unwrap();
        assert_eq!(
            verify_result.records.len(),
            1,
            "Only initial record should exist; denied transaction must not partially commit"
        );
    }

    #[test]
    fn policy_enforcement_capability_must_match_operation() {
        // Test: Having state:read doesn't allow writes
        let db = db("policy_cap_mismatch");
        let schema = schema();

        let read_only_context = AuthorizationContext {
            subject: "user:charlie".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:read".into()].into(), // Only read capability
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let write_request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: read_only_context,
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "inc-1".into(),
                value: serde_json::json!({"title": "Test"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };

        let result = execute_transaction(&db, &schema, &write_request, None);
        assert!(
            result.is_err(),
            "Write must be denied even with read capability"
        );
        assert_eq!(result.unwrap_err().code, "AUTHORIZATION_DENIED");
    }

    #[test]
    fn member_policy_read_allowed_when_member() {
        // Test: Member policy allows reads when actor is a member of the org
        let db = db("member_policy_read_allow");
        let schema = schema();

        // Pre-populate a record with organization_id
        let insert_context = AuthorizationContext {
            subject: "user:admin".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:write".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let insert_request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: insert_context,
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "proj-1".into(),
                value: serde_json::json!({"title": "Test Project"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };

        let _ = execute_transaction(&db, &schema, &insert_request, None);

        // Now test member policy read with capability
        let member_context = AuthorizationContext {
            subject: "user:alice".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:read".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let read_context = begin_read(&db, &schema, "app", member_context).unwrap();
        let query = CanonicalQuery {
            collection: "incidents".into(),
            filter: None,
            order_by: vec![],
            limit: Some(100),
            offset: 0,
            cursor: None,
            projection: vec![],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };

        let result = execute_query(&db, &schema, &read_context, &query, None);
        assert!(
            result.is_ok(),
            "Member policy read should be allowed with state:read"
        );
    }

    #[test]
    fn member_policy_read_denied_without_capability() {
        // Test: Member policy denies reads without state:read capability
        let db = db("member_policy_read_deny");
        let schema = schema();

        let empty_context = AuthorizationContext {
            subject: "user:bob".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: BTreeSet::new(), // No capabilities
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let read_context = begin_read(&db, &schema, "app", empty_context).unwrap();
        let query = CanonicalQuery {
            collection: "incidents".into(),
            filter: None,
            order_by: vec![],
            limit: Some(100),
            offset: 0,
            cursor: None,
            projection: vec![],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };

        let result = execute_query(&db, &schema, &read_context, &query, None);
        assert!(
            result.is_err(),
            "Member policy read must be denied without state:read"
        );
        assert_eq!(result.unwrap_err().code, "AUTHORIZATION_DENIED");
    }

    #[test]
    fn member_policy_write_allowed_when_member() {
        // Test: Member policy allows writes when actor is a member of the org
        let db = db("member_policy_write_allow");
        let schema = schema();

        let member_context = AuthorizationContext {
            subject: "user:alice".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:write".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let write_request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: member_context,
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "inc-member-1".into(),
                value: serde_json::json!({"title": "Member Project"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };

        let result = execute_transaction(&db, &schema, &write_request, None);
        assert!(
            result.is_ok(),
            "Member policy write should be allowed with state:write"
        );
    }

    #[test]
    fn member_policy_write_denied_without_capability() {
        // Test: Member policy denies writes without state:write capability
        let db = db("member_policy_write_deny");
        let schema = schema();

        let empty_context = AuthorizationContext {
            subject: "user:bob".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: BTreeSet::new(), // No capabilities
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let write_request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: empty_context,
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "inc-member-2".into(),
                value: serde_json::json!({"title": "Denied Project"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };

        let result = execute_transaction(&db, &schema, &write_request, None);
        assert!(
            result.is_err(),
            "Member policy write must be denied without state:write"
        );
        assert_eq!(result.unwrap_err().code, "AUTHORIZATION_DENIED");
    }

    #[test]
    fn member_policy_cross_org_mutation_prevented() {
        // Test: Cross-organization mutation attack is prevented by lack of state:write
        // Scenario: user:alice is member of org-1 but tries to create/modify resource in org-2
        // Expected: Denied because server wouldn't grant state:write capability for org-2
        let db = db("member_policy_cross_org");
        let schema = schema();

        // Admin inserts record in org-2
        let admin_context = AuthorizationContext {
            subject: "user:admin".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:write".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let insert_in_org2 = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: admin_context,
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "inc-org2-1".into(),
                value: serde_json::json!({"title": "Org2 Resource"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };

        let _ = execute_transaction(&db, &schema, &insert_in_org2, None);

        // Now user:alice (member of org-1 only) tries to modify org-2 resource
        // Without state:write capability (which would only be granted for org-1),
        // the mutation is denied
        let alice_context = AuthorizationContext {
            subject: "user:alice".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: BTreeSet::new(), // No state:write for org-2
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let cross_org_mutation = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: alice_context,
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Update,
                collection: "incidents".into(),
                id: "inc-org2-1".into(),
                value: serde_json::json!({"title": "Hacked by alice"}),
                if_version: Some(0),
            }],
            preconditions: vec![],
            fences: vec![],
        };

        let result = execute_transaction(&db, &schema, &cross_org_mutation, None);
        assert!(
            result.is_err(),
            "Cross-org mutation must be denied (member of org-1 only)"
        );
        assert_eq!(result.unwrap_err().code, "AUTHORIZATION_DENIED");
    }

    #[test]
    fn record_authorization_owner_policy_filters_owned_records() {
        let db = db("record_auth_owner_filter");
        let schema = schema_with_owner();

        let alice_context = AuthorizationContext {
            subject: "user:alice".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:read".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let bob_context = AuthorizationContext {
            subject: "user:bob".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:read".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let admin_context = AuthorizationContext {
            subject: "user:admin".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:write".into(), "state:read".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let insert_records = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: admin_context.clone(),
            operations: vec![
                TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: "inc-alice-1".into(),
                    value: serde_json::json!({"title": "Alice 1", "owner_id": "alice"}),
                    if_version: None,
                },
                TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: "inc-bob-1".into(),
                    value: serde_json::json!({"title": "Bob 1", "owner_id": "bob"}),
                    if_version: None,
                },
                TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: "inc-alice-2".into(),
                    value: serde_json::json!({"title": "Alice 2", "owner_id": "alice"}),
                    if_version: None,
                },
            ],
            preconditions: vec![],
            fences: vec![],
        };

        execute_transaction(&db, &schema, &insert_records, None).expect("insert should succeed");

        let alice_read = begin_read(&db, &schema, "app", alice_context).unwrap();
        let query = CanonicalQuery {
            collection: "incidents".into(),
            filter: None,
            order_by: vec![],
            limit: None,
            offset: 0,
            cursor: None,
            projection: vec!["title".into(), "owner_id".into()],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };

        let alice_result = execute_query(
            &db,
            &schema,
            &alice_read,
            &query,
            Some(PolicySubject::Owner),
        )
        .expect("query should succeed");

        assert_eq!(
            alice_result.records.len(),
            2,
            "Alice should see only 2 incidents she owns"
        );
        for record in &alice_result.records {
            assert_eq!(record.get("owner_id").unwrap(), "alice");
        }

        let bob_read = begin_read(&db, &schema, "app", bob_context).unwrap();
        let bob_result = execute_query(&db, &schema, &bob_read, &query, Some(PolicySubject::Owner))
            .expect("query should succeed");

        assert_eq!(
            bob_result.records.len(),
            1,
            "Bob should see only 1 incident he owns"
        );
        assert_eq!(bob_result.records[0].get("owner_id").unwrap(), "bob");
    }

    #[test]
    fn record_authorization_owner_policy_denies_direct_access() {
        let db = db("record_auth_owner_direct");
        let schema = schema_with_owner();

        let admin_context = AuthorizationContext {
            subject: "user:admin".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:write".into(), "state:read".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let bob_context = AuthorizationContext {
            subject: "user:bob".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:read".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let insert_request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: admin_context,
            operations: vec![TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: "inc-alice-secret".into(),
                value: serde_json::json!({"title": "Secret", "owner_id": "alice"}),
                if_version: None,
            }],
            preconditions: vec![],
            fences: vec![],
        };

        execute_transaction(&db, &schema, &insert_request, None).expect("insert should succeed");

        let bob_read = begin_read(&db, &schema, "app", bob_context).unwrap();
        let direct_query = CanonicalQuery {
            collection: "incidents".into(),
            filter: Some(QueryFilter::Eq {
                field: "_id".into(),
                value: Value::String("inc-alice-secret".into()),
            }),
            order_by: vec![],
            limit: None,
            offset: 0,
            cursor: None,
            projection: vec!["title".into(), "owner_id".into()],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };

        let result = execute_query(
            &db,
            &schema,
            &bob_read,
            &direct_query,
            Some(PolicySubject::Owner),
        )
        .expect("query should succeed");

        assert_eq!(
            result.records.len(),
            0,
            "Bob should not see Alice's record even with direct _id filter"
        );
    }

    #[test]
    fn record_authorization_authenticated_policy_allows_all() {
        let db = db("record_auth_authenticated");
        let schema = schema();

        let alice_context = AuthorizationContext {
            subject: "user:alice".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:write".into(), "state:read".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let bob_context = AuthorizationContext {
            subject: "user:bob".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:read".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let insert_records = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: alice_context,
            operations: vec![
                TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: "inc-alice-auth".into(),
                    value: serde_json::json!({"title": "Alice's incident"}),
                    if_version: None,
                },
                TransactionOperation {
                    kind: TransactionOperationKind::Insert,
                    collection: "incidents".into(),
                    id: "inc-bob-auth".into(),
                    value: serde_json::json!({"title": "Bob's incident"}),
                    if_version: None,
                },
            ],
            preconditions: vec![],
            fences: vec![],
        };

        execute_transaction(&db, &schema, &insert_records, None).expect("insert should succeed");

        let bob_read = begin_read(&db, &schema, "app", bob_context).unwrap();
        let query = CanonicalQuery {
            collection: "incidents".into(),
            filter: None,
            order_by: vec![],
            limit: None,
            offset: 0,
            cursor: None,
            projection: vec!["title".into()],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };

        let result = execute_query(
            &db,
            &schema,
            &bob_read,
            &query,
            Some(PolicySubject::Authenticated),
        )
        .expect("query should succeed");

        assert_eq!(
            result.records.len(),
            2,
            "Authenticated policy should allow access to all records for authenticated actor"
        );
    }

    #[test]
    fn record_authorization_pagination_respects_authorization() {
        let db = db("record_auth_pagination");
        let schema = schema_with_owner();

        let admin_context = AuthorizationContext {
            subject: "user:admin".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:write".into(), "state:read".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let alice_context = AuthorizationContext {
            subject: "user:alice".into(),
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            capabilities: ["state:read".into()].into(),
            readable_collections: Default::default(),
            writable_collections: Default::default(),
            field_projections: Default::default(),
        };

        let mut ops = vec![];
        for i in 0..5 {
            ops.push(TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "incidents".into(),
                id: format!("incident-{}", i),
                value: serde_json::json!({
                    "title": format!("Incident {}", i),
                    "owner_id": if i % 2 == 0 { "alice" } else { "bob" }
                }),
                if_version: None,
            });
        }

        let insert_request = TransactionRequest {
            transaction_id: None,
            tenant_id: "tenant".into(),
            application_id: "app".into(),
            revision_id: "rev".into(),
            schema_version: 1,
            state_namespace: None,
            causal_parent: None,
            authorization: admin_context,
            operations: ops,
            preconditions: vec![],
            fences: vec![],
        };

        execute_transaction(&db, &schema, &insert_request, None).expect("insert should succeed");

        let alice_read = begin_read(&db, &schema, "app", alice_context).unwrap();
        let query_no_limit = CanonicalQuery {
            collection: "incidents".into(),
            filter: None,
            order_by: vec![QueryOrder {
                field: "_id".into(),
                direction: SortDirection::Asc,
            }],
            limit: None,
            offset: 0,
            cursor: None,
            projection: vec!["title".into(), "owner_id".into()],
            group_by: vec![],
            aggregates: vec![],
            references: vec![],
            semantic: None,
        };

        let result = execute_query(
            &db,
            &schema,
            &alice_read,
            &query_no_limit,
            Some(PolicySubject::Owner),
        )
        .expect("query should succeed");

        assert_eq!(
            result.records.len(),
            3,
            "Alice should see 3 incidents she owns (0, 2, 4)"
        );
        for record in &result.records {
            assert_eq!(record.get("owner_id").unwrap(), "alice");
        }
    }
}
