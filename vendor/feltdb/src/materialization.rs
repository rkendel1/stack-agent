/// Materialization module for integrating acquired content into the reactive system
///
/// When content is acquired from the distributed fabric, it needs to be materialized
/// into the local reactive state graph so that:
/// 1. Application queries use the same db.get() API
/// 2. Reactive subscriptions automatically trigger updates
/// 3. Content becomes indistinguishable from locally-created content
use crate::{AcquisitionMetadata, FlowRef, Operation};
use serde_json::Value;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// Handler for materializing acquired content
#[derive(Clone)]
pub struct MaterializationHandler {
    /// Track which references have been materialized to avoid re-materialization
    materialized: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

impl MaterializationHandler {
    /// Create a new materialization handler
    pub fn new() -> Self {
        Self {
            materialized: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
        }
    }

    /// Materialize acquired content as a legitimate FeltDB operation
    /// This makes acquired content participate in the reactive system
    pub fn materialize_as_operation(
        &self,
        flow_ref: &FlowRef,
        content: Vec<u8>,
        metadata: &AcquisitionMetadata,
    ) -> Result<Operation, String> {
        // Deserialize content to JSON to integrate with FeltDB's operation model
        let value: Value = serde_json::from_slice(&content).unwrap_or_else(|_| {
            // If not JSON, wrap raw bytes as a string
            Value::String(String::from_utf8_lossy(&content).into_owned())
        });

        let key = format!("{}:{}", flow_ref.namespace, flow_ref.object_id);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);

        // Create an insert operation for the acquired content
        // This operation will flow through the reactive system
        let mut op = Operation::update(
            now_ms as u64,                   // Use timestamp as operation ID
            metadata.source_peer.id.clone(), // Mark source peer as originator
            1,                               // Initial sequence
            key.clone(),
            value,
            flow_ref.namespace.clone(),
            flow_ref.namespace.clone(),
        );

        // Attach vector clock from acquisition metadata to preserve causality
        if let Some(vector_clock) = &metadata.vector_clock {
            op = op.with_vector_clock(vector_clock.clone());
        }

        // Mark as materialized
        let mut materialized = self.materialized.lock().map_err(|e| e.to_string())?;
        materialized.insert(key);

        Ok(op)
    }

    /// Check if a reference has already been materialized
    pub fn is_materialized(&self, flow_ref: &FlowRef) -> Result<bool, String> {
        let key = format!("{}:{}", flow_ref.namespace, flow_ref.object_id);
        let materialized = self.materialized.lock().map_err(|e| e.to_string())?;
        Ok(materialized.contains(&key))
    }

    /// Get the count of materialized references
    pub fn materialized_count(&self) -> Result<usize, String> {
        let materialized = self.materialized.lock().map_err(|e| e.to_string())?;
        Ok(materialized.len())
    }
}

impl Default for MaterializationHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Reactive collection integration
/// This trait allows acquired content to participate in reactive collections
pub trait ReactiveCollectionIntegration {
    /// Notify the collection that new content was acquired and materialized
    fn on_content_materialized(&self, operation: &Operation) -> Result<(), String>;

    /// Check if this collection would be affected by materialization
    fn affects_collection(&self, namespace: &str) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PeerId;

    #[test]
    fn test_materialization_handler_creation() {
        let handler = MaterializationHandler::new();
        assert_eq!(handler.materialized_count().unwrap(), 0);
    }

    #[test]
    fn test_materialize_as_operation() {
        let handler = MaterializationHandler::new();
        let flow_ref = FlowRef::new("docs", "doc-123");
        let content = br#"{"title": "Test Document", "content": "Test content"}"#.to_vec();
        let peer = PeerId::new("peer-a");
        let metadata = AcquisitionMetadata::new(peer, true);

        let op = handler
            .materialize_as_operation(&flow_ref, content, &metadata)
            .unwrap();

        assert_eq!(op.key, "docs:doc-123");
        assert_eq!(op.capability, "docs");
        assert!(op.value.is_some());
    }

    #[test]
    fn test_materialize_non_json_content() {
        let handler = MaterializationHandler::new();
        let flow_ref = FlowRef::new("binary", "file-456");
        let content = b"raw binary data".to_vec();
        let peer = PeerId::new("peer-b");
        let metadata = AcquisitionMetadata::new(peer, true);

        let op = handler
            .materialize_as_operation(&flow_ref, content, &metadata)
            .unwrap();

        assert_eq!(op.key, "binary:file-456");
        // Raw binary should be wrapped as string
        assert!(op.value.is_some());
    }

    #[test]
    fn test_is_materialized() {
        let handler = MaterializationHandler::new();
        let flow_ref = FlowRef::new("docs", "doc-789");

        assert!(!handler.is_materialized(&flow_ref).unwrap());

        let content = b"content".to_vec();
        let peer = PeerId::new("peer-c");
        let metadata = AcquisitionMetadata::new(peer, true);

        handler
            .materialize_as_operation(&flow_ref, content, &metadata)
            .unwrap();

        assert!(handler.is_materialized(&flow_ref).unwrap());
    }

    #[test]
    fn test_materialized_count() {
        let handler = MaterializationHandler::new();

        for i in 0..5 {
            let flow_ref = FlowRef::new("docs", &format!("doc-{}", i));
            let content = b"content".to_vec();
            let peer = PeerId::new("peer-x");
            let metadata = AcquisitionMetadata::new(peer, true);

            handler
                .materialize_as_operation(&flow_ref, content, &metadata)
                .unwrap();
        }

        assert_eq!(handler.materialized_count().unwrap(), 5);
    }
}
