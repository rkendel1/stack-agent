/// Full-text Search Capability
///
/// A concrete implementation of FlowCapability that maintains a full-text search index.
/// This demonstrates the capability extension model with a projection that doesn't own
/// canonical state but derives from it.
use crate::capability::{
    CapabilityAccessModel, CapabilityContext, CapabilityId, CapabilityMetadata,
    CapabilityOperationResult, CapabilityQuery, CapabilityQueryResult, CapabilityRole,
    FlowCapability,
};
use crate::{Operation, OperationType, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Search index document representation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchDocument {
    pub key: String,
    pub content: String,
    pub metadata: Value,
}

/// Search result from the search capability
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub key: String,
    pub content: String,
    pub score: f32,
    pub metadata: Value,
}

/// Full-text search capability implementation
pub struct SearchCapability {
    capability_id: CapabilityId,
    metadata: CapabilityMetadata,
    /// In-memory document store
    documents: HashMap<String, SearchDocument>,
}

impl SearchCapability {
    pub fn new() -> Self {
        let capability_id = CapabilityId::new("search".to_string());
        let access_model = CapabilityAccessModel::new(
            vec!["*".to_string()],
            vec!["search.index".to_string()],
            vec![],
        );

        let metadata = CapabilityMetadata::new(
            capability_id.clone(),
            "Full-Text Search".to_string(),
            "Maintains a full-text search index over documents".to_string(),
            CapabilityRole::Projection,
            access_model,
        );

        Self {
            capability_id,
            metadata,
            documents: HashMap::new(),
        }
    }

    /// Add or update a document in the search index
    fn index_document(&mut self, doc: &SearchDocument) -> Result<()> {
        self.documents.insert(doc.key.clone(), doc.clone());
        Ok(())
    }

    /// Remove a document from the search index
    fn remove_document(&mut self, key: &str) -> Result<()> {
        self.documents.remove(key);
        Ok(())
    }

    /// Search the index - simple term matching
    fn search(&self, query_str: &str, limit: usize) -> Result<Vec<SearchResult>> {
        // Simple tokenization
        let query_terms: Vec<&str> = query_str
            .split_whitespace()
            .filter(|s| !s.is_empty())
            .collect();

        let mut results = Vec::new();

        // Search through all documents
        for doc in self.documents.values() {
            let mut score = 0.0f32;
            let content_lower = doc.content.to_lowercase();

            // Score based on term matches
            for term in &query_terms {
                let term_lower = term.to_lowercase();
                if content_lower.contains(&term_lower) {
                    // Count occurrences
                    let count = content_lower.matches(&term_lower).count() as f32;
                    score += count * 1.0;
                }
            }

            if score > 0.0 {
                results.push(SearchResult {
                    key: doc.key.clone(),
                    content: doc.content.clone(),
                    score,
                    metadata: doc.metadata.clone(),
                });
            }
        }

        // Sort by score descending
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        results.truncate(limit);

        Ok(results)
    }
}

impl Default for SearchCapability {
    fn default() -> Self {
        Self::new()
    }
}

impl FlowCapability for SearchCapability {
    fn id(&self) -> CapabilityId {
        self.capability_id.clone()
    }

    fn metadata(&self) -> CapabilityMetadata {
        self.metadata.clone()
    }

    fn initialize(&mut self, _context: &CapabilityContext) -> Result<()> {
        Ok(())
    }

    fn on_operation(
        &mut self,
        operation: &Operation,
        _context: &CapabilityContext,
    ) -> Result<CapabilityOperationResult> {
        // Only index Insert and Update operations
        match operation.op_type {
            OperationType::Insert | OperationType::Update => {
                if let Some(value) = &operation.value {
                    let content = if value.is_string() {
                        value.as_str().unwrap_or("").to_string()
                    } else {
                        value.to_string()
                    };

                    let doc = SearchDocument {
                        key: operation.key.clone(),
                        content,
                        metadata: json!({
                            "source_operation": operation.op_id,
                            "timestamp": operation.timestamp_ms,
                        }),
                    };

                    self.index_document(&doc)?;
                    Ok(CapabilityOperationResult::with_derived_state(json!({
                        "indexed": true,
                        "key": operation.key.clone(),
                    })))
                } else {
                    Ok(CapabilityOperationResult::success())
                }
            }
            OperationType::Delete => {
                self.remove_document(&operation.key)?;
                Ok(CapabilityOperationResult::success())
            }
        }
    }

    fn query(
        &self,
        query: &CapabilityQuery,
        _context: &CapabilityContext,
    ) -> Result<CapabilityQueryResult> {
        if query.query_type != "search" {
            return Ok(CapabilityQueryResult::error(format!(
                "Unknown query type: {}",
                query.query_type
            )));
        }

        let query_str = query.params.get("q").and_then(|v| v.as_str()).unwrap_or("");

        let limit = query
            .params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        let results = self.search(query_str, limit)?;
        let result_values: Vec<Value> = results
            .iter()
            .map(|r| {
                json!({
                    "key": r.key,
                    "content": r.content,
                    "score": r.score,
                    "metadata": r.metadata,
                })
            })
            .collect();

        Ok(CapabilityQueryResult::success(result_values))
    }

    fn rebuild(&mut self, operations: &[Operation], context: &CapabilityContext) -> Result<()> {
        // Clear existing index
        self.documents.clear();

        // Replay all operations
        for op in operations {
            self.on_operation(op, context)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_capability_creation() {
        let cap = SearchCapability::new();
        assert_eq!(cap.id().0, "search");
        assert!(cap.metadata().access_model.can_read("docs"));
        assert!(cap.metadata().access_model.can_write("search.index"));
    }

    #[test]
    fn test_search_capability_initialization() {
        let mut cap = SearchCapability::new();
        let context = CapabilityContext {
            capability_id: cap.id(),
            instance_id: "test-instance".to_string(),
            sequence: 0,
            version: 1,
        };

        assert!(cap.initialize(&context).is_ok());
    }

    #[test]
    fn test_search_capability_indexing() {
        let mut cap = SearchCapability::new();
        let context = CapabilityContext {
            capability_id: cap.id(),
            instance_id: "test-instance".to_string(),
            sequence: 0,
            version: 1,
        };

        cap.initialize(&context).unwrap();

        let op = Operation::insert(
            1,
            "instance-1".to_string(),
            1,
            "docs:1".to_string(),
            json!({"title": "Rust Programming", "content": "Learn Rust"}),
            "Document".to_string(),
            "docs".to_string(),
        );

        let result = cap.on_operation(&op, &context).unwrap();
        assert!(result.success);
        assert!(result.produced_derived_state);
    }

    #[test]
    fn test_search_capability_query() {
        let mut cap = SearchCapability::new();
        let context = CapabilityContext {
            capability_id: cap.id(),
            instance_id: "test-instance".to_string(),
            sequence: 0,
            version: 1,
        };

        cap.initialize(&context).unwrap();

        let op1 = Operation::insert(
            1,
            "instance-1".to_string(),
            1,
            "docs:1".to_string(),
            json!("Rust Programming Language"),
            "Document".to_string(),
            "docs".to_string(),
        );

        let op2 = Operation::insert(
            2,
            "instance-1".to_string(),
            2,
            "docs:2".to_string(),
            json!("Python for Data Science"),
            "Document".to_string(),
            "docs".to_string(),
        );

        cap.on_operation(&op1, &context).unwrap();
        cap.on_operation(&op2, &context).unwrap();

        let query = CapabilityQuery {
            query_type: "search".to_string(),
            params: json!({
                "q": "Rust",
                "limit": 10
            }),
        };

        let results = cap.query(&query, &context).unwrap();
        assert!(results.success);
        assert!(results.results.len() > 0);
    }

    #[test]
    fn test_search_capability_delete() {
        let mut cap = SearchCapability::new();
        let context = CapabilityContext {
            capability_id: cap.id(),
            instance_id: "test-instance".to_string(),
            sequence: 0,
            version: 1,
        };

        cap.initialize(&context).unwrap();

        // Insert a document
        let op1 = Operation::insert(
            1,
            "instance-1".to_string(),
            1,
            "docs:1".to_string(),
            json!("Test Document"),
            "Document".to_string(),
            "docs".to_string(),
        );
        cap.on_operation(&op1, &context).unwrap();

        // Verify it's indexed
        let query = CapabilityQuery {
            query_type: "search".to_string(),
            params: json!({"q": "Test", "limit": 10}),
        };
        let results = cap.query(&query, &context).unwrap();
        assert_eq!(results.results.len(), 1);

        // Delete it
        let op2 = Operation::delete(
            2,
            "instance-1".to_string(),
            2,
            "docs:1".to_string(),
            "Document".to_string(),
            "docs".to_string(),
        );
        cap.on_operation(&op2, &context).unwrap();

        // Verify it's gone
        let results = cap.query(&query, &context).unwrap();
        assert_eq!(results.results.len(), 0);
    }

    #[test]
    fn test_search_capability_rebuild() {
        let mut cap = SearchCapability::new();
        let context = CapabilityContext {
            capability_id: cap.id(),
            instance_id: "test-instance".to_string(),
            sequence: 0,
            version: 1,
        };

        cap.initialize(&context).unwrap();

        let ops = vec![
            Operation::insert(
                1,
                "instance-1".to_string(),
                1,
                "docs:1".to_string(),
                json!("Rust Guide"),
                "Document".to_string(),
                "docs".to_string(),
            ),
            Operation::insert(
                2,
                "instance-1".to_string(),
                2,
                "docs:2".to_string(),
                json!("Go Tutorial"),
                "Document".to_string(),
                "docs".to_string(),
            ),
        ];

        cap.rebuild(&ops, &context).unwrap();

        let query = CapabilityQuery {
            query_type: "search".to_string(),
            params: json!({"q": "Rust", "limit": 10}),
        };
        let results = cap.query(&query, &context).unwrap();
        assert_eq!(results.results.len(), 1);
    }
}
