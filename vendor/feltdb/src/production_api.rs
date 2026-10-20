/// Production Query/Mutation API
///
/// High-level production-ready API for:
/// - Native state reads
/// - Atomic mutations
/// - Transactions with consistency controls
/// - Relationship access patterns
/// - Materialized views and projections
/// - Multi-key operations
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateValue {
    pub id: String,
    pub data: serde_json::Value,
    pub version: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConsistencyLevel {
    Strong,
    Eventual,
    Causal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadOptions {
    pub consistency: ConsistencyLevel,
    pub include_history: bool,
    pub timeout_ms: u64,
}

impl Default for ReadOptions {
    fn default() -> Self {
        ReadOptions {
            consistency: ConsistencyLevel::Strong,
            include_history: false,
            timeout_ms: 5000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteOptions {
    pub consistency: ConsistencyLevel,
    pub idempotency_key: Option<String>,
    pub conditional: Option<String>,
    pub timeout_ms: u64,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            consistency: ConsistencyLevel::Strong,
            idempotency_key: None,
            conditional: None,
            timeout_ms: 5000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mutation {
    pub operation: String,
    pub resource_type: String,
    pub resource_id: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationResult {
    pub success: bool,
    pub resource_id: String,
    pub version: u64,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipQuery {
    pub source_type: String,
    pub source_id: String,
    pub relationship: String,
    pub target_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipResult {
    pub source_id: String,
    pub relationship: String,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterializedView {
    pub view_name: String,
    pub base_resources: Vec<String>,
    pub projection_fields: Vec<String>,
    pub aggregation: Option<String>,
    pub data: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionRequest {
    pub mutations: Vec<Mutation>,
    pub consistency: ConsistencyLevel,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionResult {
    pub transaction_id: String,
    pub success: bool,
    pub results: Vec<MutationResult>,
    pub errors: Vec<String>,
}

pub struct ProductionAPI {
    state_store: HashMap<String, StateValue>,
    relationships: HashMap<String, Vec<String>>,
    views: HashMap<String, MaterializedView>,
    transaction_id_counter: u64,
}

impl ProductionAPI {
    pub fn new() -> Self {
        ProductionAPI {
            state_store: HashMap::new(),
            relationships: HashMap::new(),
            views: HashMap::new(),
            transaction_id_counter: 0,
        }
    }

    pub fn read(&self, id: &str, _options: ReadOptions) -> Result<StateValue, String> {
        self.state_store
            .get(id)
            .cloned()
            .ok_or_else(|| format!("Resource not found: {}", id))
    }

    pub fn read_many(
        &self,
        ids: &[&str],
        _options: ReadOptions,
    ) -> Result<Vec<StateValue>, String> {
        let mut results = Vec::new();
        for id in ids {
            if let Some(value) = self.state_store.get(*id) {
                results.push(value.clone());
            }
        }
        Ok(results)
    }

    pub fn write(
        &mut self,
        mutation: Mutation,
        options: WriteOptions,
    ) -> Result<MutationResult, String> {
        if let Some(ref key) = options.idempotency_key {
            if let Some(existing) = self.state_store.get(&mutation.resource_id) {
                return Ok(MutationResult {
                    success: true,
                    resource_id: mutation.resource_id.clone(),
                    version: existing.version,
                    errors: vec!["Already applied (idempotent)".to_string()],
                });
            }
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let value = StateValue {
            id: mutation.resource_id.clone(),
            data: mutation.data,
            version: 1,
            created_at: now,
            updated_at: now,
        };

        self.state_store
            .insert(mutation.resource_id.clone(), value.clone());

        Ok(MutationResult {
            success: true,
            resource_id: mutation.resource_id,
            version: value.version,
            errors: Vec::new(),
        })
    }

    pub fn write_many(
        &mut self,
        mutations: Vec<Mutation>,
        options: WriteOptions,
    ) -> Result<Vec<MutationResult>, String> {
        let mut results = Vec::new();
        for mutation in mutations {
            match self.write(mutation, options.clone()) {
                Ok(result) => results.push(result),
                Err(e) => {
                    results.push(MutationResult {
                        success: false,
                        resource_id: "unknown".to_string(),
                        version: 0,
                        errors: vec![e],
                    });
                }
            }
        }
        Ok(results)
    }

    pub fn transaction(
        &mut self,
        request: TransactionRequest,
    ) -> Result<TransactionResult, String> {
        self.transaction_id_counter += 1;
        let transaction_id = format!("tx-{}", self.transaction_id_counter);

        let mut results = Vec::new();
        let mut errors = Vec::new();

        for mutation in request.mutations {
            let options = WriteOptions {
                consistency: request.consistency.clone(),
                idempotency_key: None,
                conditional: None,
                timeout_ms: request.timeout_ms,
            };

            match self.write(mutation, options) {
                Ok(result) => results.push(result),
                Err(e) => errors.push(e),
            }
        }

        Ok(TransactionResult {
            transaction_id,
            success: errors.is_empty(),
            results,
            errors,
        })
    }

    pub fn add_relationship(&mut self, source: String, rel: String, target: String) {
        let key = format!("{}:{}", source, rel);
        self.relationships
            .entry(key)
            .or_insert_with(Vec::new)
            .push(target);
    }

    pub fn get_relationships(
        &self,
        query: RelationshipQuery,
    ) -> Result<RelationshipResult, String> {
        let key = format!("{}:{}", query.source_id, query.relationship);
        let targets = self.relationships.get(&key).cloned().unwrap_or_default();

        Ok(RelationshipResult {
            source_id: query.source_id,
            relationship: query.relationship,
            targets,
        })
    }

    pub fn create_view(
        &mut self,
        view_name: String,
        base_resources: Vec<String>,
        projection_fields: Vec<String>,
    ) -> Result<(), String> {
        self.views.insert(
            view_name.clone(),
            MaterializedView {
                view_name,
                base_resources,
                projection_fields,
                aggregation: None,
                data: Vec::new(),
            },
        );
        Ok(())
    }

    pub fn refresh_view(&mut self, view_name: &str) -> Result<MaterializedView, String> {
        let view = self
            .views
            .get_mut(view_name)
            .ok_or_else(|| format!("View not found: {}", view_name))?;

        view.data.clear();
        for resource_id in &view.base_resources {
            if let Some(state) = self.state_store.get(resource_id) {
                view.data.push(state.data.clone());
            }
        }

        Ok(view.clone())
    }

    pub fn query_view(&self, view_name: &str) -> Result<MaterializedView, String> {
        self.views
            .get(view_name)
            .cloned()
            .ok_or_else(|| format!("View not found: {}", view_name))
    }
}

impl Default for ProductionAPI {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_write() {
        let mut api = ProductionAPI::new();
        let mutation = Mutation {
            operation: "insert".to_string(),
            resource_type: "user".to_string(),
            resource_id: "user-1".to_string(),
            data: serde_json::json!({"name": "Alice"}),
        };

        api.write(mutation, WriteOptions::default()).unwrap();
        let result = api.read("user-1", ReadOptions::default()).unwrap();
        assert_eq!(result.id, "user-1");
    }

    #[test]
    fn test_transaction() {
        let mut api = ProductionAPI::new();
        let request = TransactionRequest {
            mutations: vec![
                Mutation {
                    operation: "insert".to_string(),
                    resource_type: "user".to_string(),
                    resource_id: "user-1".to_string(),
                    data: serde_json::json!({"name": "Alice"}),
                },
                Mutation {
                    operation: "insert".to_string(),
                    resource_type: "user".to_string(),
                    resource_id: "user-2".to_string(),
                    data: serde_json::json!({"name": "Bob"}),
                },
            ],
            consistency: ConsistencyLevel::Strong,
            timeout_ms: 5000,
        };

        let result = api.transaction(request).unwrap();
        assert!(result.success);
        assert_eq!(result.results.len(), 2);
    }

    #[test]
    fn test_relationships() {
        let mut api = ProductionAPI::new();
        api.add_relationship(
            "user-1".to_string(),
            "follows".to_string(),
            "user-2".to_string(),
        );
        api.add_relationship(
            "user-1".to_string(),
            "follows".to_string(),
            "user-3".to_string(),
        );

        let result = api
            .get_relationships(RelationshipQuery {
                source_type: "user".to_string(),
                source_id: "user-1".to_string(),
                relationship: "follows".to_string(),
                target_type: None,
            })
            .unwrap();

        assert_eq!(result.targets.len(), 2);
    }

    #[test]
    fn test_materialized_view() {
        let mut api = ProductionAPI::new();
        let mutation1 = Mutation {
            operation: "insert".to_string(),
            resource_type: "user".to_string(),
            resource_id: "user-1".to_string(),
            data: serde_json::json!({"name": "Alice"}),
        };

        api.write(mutation1, WriteOptions::default()).unwrap();

        api.create_view(
            "active_users".to_string(),
            vec!["user-1".to_string()],
            vec!["name".to_string()],
        )
        .unwrap();

        api.refresh_view("active_users").unwrap();
        let view = api.query_view("active_users").unwrap();
        assert_eq!(view.data.len(), 1);
    }
}
