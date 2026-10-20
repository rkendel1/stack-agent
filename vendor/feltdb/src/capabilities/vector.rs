/// Vector Search Capability
///
/// A concrete implementation of FlowCapability that maintains a vector search index.
/// This demonstrates the capability extension model with a different type of projection
/// compared to search - showing that the same capability contract works for diverse use cases.
use crate::capability::{
    CapabilityAccessModel, CapabilityContext, CapabilityId, CapabilityMetadata,
    CapabilityOperationResult, CapabilityQuery, CapabilityQueryResult, CapabilityRole,
    FlowCapability,
};
use crate::{Operation, OperationType, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Vector embedding representation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorDocument {
    pub key: String,
    pub text: String,
    pub embedding: Vec<f32>,
    pub metadata: Value,
}

/// Vector search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorSearchResult {
    pub key: String,
    pub text: String,
    pub distance: f32,
    pub metadata: Value,
}

/// Embedding provider trait for pluggable embeddings
pub trait EmbeddingProvider: Send + Sync {
    /// Generate embedding for text
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

/// Simple embedding provider using random vectors (for demo)
pub struct DemoEmbeddingProvider;

impl EmbeddingProvider for DemoEmbeddingProvider {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // Simple hash-based deterministic embedding for testing
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let seed = hasher.finish();

        // Generate deterministic vector using seed
        let mut embedding = Vec::new();
        for i in 0..10 {
            let val = (((seed ^ (i as u64 * 31)) % 1000) as f32 / 1000.0) * 2.0 - 1.0;
            embedding.push(val);
        }

        Ok(embedding)
    }
}

/// Vector search capability implementation
pub struct VectorCapability {
    capability_id: CapabilityId,
    metadata: CapabilityMetadata,
    /// In-memory vector store
    documents: HashMap<String, VectorDocument>,
    /// Embedding provider
    embedding_provider: Box<dyn EmbeddingProvider>,
}

impl VectorCapability {
    pub fn new(embedding_provider: Box<dyn EmbeddingProvider>) -> Self {
        let capability_id = CapabilityId::new("vector".to_string());
        let access_model = CapabilityAccessModel::new(
            vec!["*".to_string()],
            vec!["vector.index".to_string()],
            vec!["embedding-api".to_string()],
        );

        let metadata = CapabilityMetadata::new(
            capability_id.clone(),
            "Vector Search".to_string(),
            "Maintains a vector search index over documents".to_string(),
            CapabilityRole::Projection,
            access_model,
        );

        Self {
            capability_id,
            metadata,
            documents: HashMap::new(),
            embedding_provider,
        }
    }

    /// Create a new vector capability with default embedding provider
    pub fn with_default_provider() -> Self {
        Self::new(Box::new(DemoEmbeddingProvider))
    }

    /// Add or update a document with embedding
    fn index_document(&mut self, doc: &VectorDocument) -> Result<()> {
        self.documents.insert(doc.key.clone(), doc.clone());
        Ok(())
    }

    /// Remove a document from the vector index
    fn remove_document(&mut self, key: &str) -> Result<()> {
        self.documents.remove(key);
        Ok(())
    }

    /// Calculate cosine similarity between two vectors
    fn cosine_similarity(vec1: &[f32], vec2: &[f32]) -> f32 {
        let mut dot_product = 0.0f32;
        let mut norm1 = 0.0f32;
        let mut norm2 = 0.0f32;

        for i in 0..vec1.len().min(vec2.len()) {
            dot_product += vec1[i] * vec2[i];
            norm1 += vec1[i] * vec1[i];
            norm2 += vec2[i] * vec2[i];
        }

        if norm1 == 0.0 || norm2 == 0.0 {
            return 0.0;
        }

        dot_product / (norm1.sqrt() * norm2.sqrt())
    }

    /// Find similar vectors
    fn find_similar(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<VectorSearchResult>> {
        let mut results = Vec::new();

        for (_key, doc) in &self.documents {
            let similarity = Self::cosine_similarity(query_embedding, &doc.embedding);
            // Convert similarity (0-1) to distance (0-2)
            let distance = 2.0 - (similarity + 1.0);

            results.push(VectorSearchResult {
                key: doc.key.clone(),
                text: doc.text.clone(),
                distance,
                metadata: doc.metadata.clone(),
            });
        }

        // Sort by distance (ascending - smallest distance is best match)
        results.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap());
        results.truncate(top_k);

        Ok(results)
    }
}

impl FlowCapability for VectorCapability {
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
                    let text = if value.is_string() {
                        value.as_str().unwrap_or("").to_string()
                    } else {
                        value.to_string()
                    };

                    // Generate embedding via provider
                    let embedding = self.embedding_provider.embed(&text)?;

                    let doc = VectorDocument {
                        key: operation.key.clone(),
                        text,
                        embedding,
                        metadata: json!({
                            "source_operation": operation.op_id,
                            "timestamp": operation.timestamp_ms,
                        }),
                    };

                    self.index_document(&doc)?;
                    Ok(CapabilityOperationResult::with_derived_state(json!({
                        "embedded": true,
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
        if query.query_type != "vectorFind" {
            return Ok(CapabilityQueryResult::error(format!(
                "Unknown query type: {}",
                query.query_type
            )));
        }

        let query_text = query
            .params
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let top_k = query
            .params
            .get("topK")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        // Generate embedding for query
        let query_embedding = self.embedding_provider.embed(query_text)?;

        // Find similar documents
        let results = self.find_similar(&query_embedding, top_k)?;
        let result_values: Vec<Value> = results
            .iter()
            .map(|r| {
                json!({
                    "key": r.key,
                    "text": r.text,
                    "distance": r.distance,
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
    fn test_vector_capability_creation() {
        let cap = VectorCapability::with_default_provider();
        assert_eq!(cap.id().0, "vector");
        assert!(cap.metadata().access_model.can_read("docs"));
        assert!(cap.metadata().access_model.can_write("vector.index"));
    }

    #[test]
    fn test_vector_capability_initialization() {
        let mut cap = VectorCapability::with_default_provider();
        let context = CapabilityContext {
            capability_id: cap.id(),
            instance_id: "test-instance".to_string(),
            sequence: 0,
            version: 1,
        };

        assert!(cap.initialize(&context).is_ok());
    }

    #[test]
    fn test_vector_capability_embedding() {
        let mut cap = VectorCapability::with_default_provider();
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
            json!("Rust is a systems programming language"),
            "Document".to_string(),
            "docs".to_string(),
        );

        let result = cap.on_operation(&op, &context).unwrap();
        assert!(result.success);
        assert!(result.produced_derived_state);
    }

    #[test]
    fn test_vector_capability_query() {
        let mut cap = VectorCapability::with_default_provider();
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
            json!("Rust programming language"),
            "Document".to_string(),
            "docs".to_string(),
        );

        let op2 = Operation::insert(
            2,
            "instance-1".to_string(),
            2,
            "docs:2".to_string(),
            json!("Python data science tutorial"),
            "Document".to_string(),
            "docs".to_string(),
        );

        cap.on_operation(&op1, &context).unwrap();
        cap.on_operation(&op2, &context).unwrap();

        let query = CapabilityQuery {
            query_type: "vectorFind".to_string(),
            params: json!({
                "query": "Rust systems",
                "topK": 5
            }),
        };

        let results = cap.query(&query, &context).unwrap();
        assert!(results.success);
        assert!(results.results.len() > 0);
    }

    #[test]
    fn test_vector_capability_delete() {
        let mut cap = VectorCapability::with_default_provider();
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
            json!("Test document"),
            "Document".to_string(),
            "docs".to_string(),
        );
        cap.on_operation(&op1, &context).unwrap();

        // Verify it's indexed
        let query = CapabilityQuery {
            query_type: "vectorFind".to_string(),
            params: json!({"query": "test", "topK": 10}),
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
    fn test_vector_similarity() {
        let vec1 = vec![1.0, 0.0, 0.0];
        let vec2 = vec![1.0, 0.0, 0.0];
        let vec3 = vec![0.0, 1.0, 0.0];

        let sim1 = VectorCapability::cosine_similarity(&vec1, &vec2);
        let sim2 = VectorCapability::cosine_similarity(&vec1, &vec3);

        assert!((sim1 - 1.0).abs() < 0.001); // Same vector - similarity = 1
        assert!((sim2 - 0.0).abs() < 0.001); // Orthogonal - similarity = 0
    }

    #[test]
    fn test_vector_capability_rebuild() {
        let mut cap = VectorCapability::with_default_provider();
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
                json!("Machine learning"),
                "Document".to_string(),
                "docs".to_string(),
            ),
            Operation::insert(
                2,
                "instance-1".to_string(),
                2,
                "docs:2".to_string(),
                json!("Deep neural networks"),
                "Document".to_string(),
                "docs".to_string(),
            ),
        ];

        cap.rebuild(&ops, &context).unwrap();

        let query = CapabilityQuery {
            query_type: "vectorFind".to_string(),
            params: json!({"query": "learning", "topK": 5}),
        };
        let results = cap.query(&query, &context).unwrap();
        assert!(results.success);
        assert!(results.results.len() > 0);
    }
}
