/// Diagnostic utilities for investigating cardinality reconstruction issues.
///
/// This module provides tools to understand how collection cardinality is reconstructed
/// during startup and how it compares to query-time lookups.
use crate::{AtomicMutation, FeltDb};
use std::collections::HashMap;

/// Diagnostic information about a single collection's cardinality.
#[derive(Debug, Clone)]
pub struct CardinalityDiagnostic {
    /// Collection name (e.g., "github_webhook_deliveries")
    pub collection: String,

    /// Capability keys actually found in persisted rows during replay_log
    pub persisted_capabilities: Vec<String>,

    /// Count of non-deleted rows found under each capability
    pub persisted_row_counts: HashMap<String, u64>,

    /// Total rows for this collection across all capability variants
    pub total_persisted_rows: u64,

    /// Maintained cardinality value stored in collection_cardinality HashMap
    pub maintained_count: u64,

    /// What capability string query execution looks for
    pub query_capability: String,

    /// Whether query_capability matches any persisted capability
    pub match_status: String,

    /// Delta between total persisted and maintained cardinality
    pub delta: i64,
}

/// Analyze the cardinality mismatch between persisted state and query-time lookups.
///
/// Returns diagnostic information for collections with mismatches.
///
/// # Arguments
/// * `db` - The FeltDb instance
/// * `state_namespace` - The state namespace from query context
/// * `application_id` - The application ID
/// * `collection_name` - The collection to diagnose (without namespace)
pub fn diagnose_cardinality(
    db: &FeltDb,
    state_namespace: &str,
    application_id: &str,
    collection_name: &str,
) -> CardinalityDiagnostic {
    // What the query executor will look for
    let query_capability = format!("{}:{}", state_namespace, collection_name);

    // What the maintained cardinality HashMap contains
    let maintained_count = db.collection_cardinality(&query_capability).unwrap_or(0);

    // We can't directly inspect inner.rows from outside, but we can reason about it:
    // During replay_log, rows are loaded with whatever capability was in the persisted log.
    // If there's a mismatch, the query_capability won't find anything.

    CardinalityDiagnostic {
        collection: collection_name.to_string(),
        persisted_capabilities: vec![], // Would need access to inner.rows to populate
        persisted_row_counts: HashMap::new(),
        total_persisted_rows: 0, // Would need to scan inner.rows
        maintained_count,
        query_capability,
        match_status: if maintained_count == 0 {
            "MISMATCH: maintained_count is 0".to_string()
        } else {
            "OK".to_string()
        },
        delta: 0, // Can't compute without authoritative row count
    }
}

/// Create a test scenario with mixed capability formats to reproduce the issue.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{open, AtomicMutation};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn temp_file() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "feltdb-diag-{}-{nanos}-{sequence}.data",
            std::process::id()
        ))
    }

    #[test]
    fn test_capability_mismatch_scenario() {
        // This test demonstrates the hypothesis:
        // If rows are persisted under "collection_name" but queried as "namespace:collection_name",
        // cardinality will be 0 on restart.

        let path = temp_file();

        {
            let db = open(&path).unwrap();
            let capability = "github_webhook_deliveries"; // Just collection name, no namespace

            let mutations = vec![
                AtomicMutation {
                    capability: capability.to_string(),
                    key: "webhook-1".to_string(),
                    value: Some(serde_json::json!({"id": 1})),
                },
                AtomicMutation {
                    capability: capability.to_string(),
                    key: "webhook-2".to_string(),
                    value: Some(serde_json::json!({"id": 2})),
                },
            ];

            db.apply_atomic_transaction("txn-1", None, &[], &mutations, None)
                .unwrap();

            // At this point, cardinality should be 2 for the key "github_webhook_deliveries"
            let count = db.collection_cardinality(capability).unwrap();
            assert_eq!(count, 2, "Cardinality before restart");
        }

        // After restart, cardinality is recomputed
        let reopened = open(&path).unwrap();
        let capability = "github_webhook_deliveries";
        let count_after = reopened.collection_cardinality(capability).unwrap();
        assert_eq!(count_after, 2, "Cardinality after restart should match");

        // But if query looks for a DIFFERENT capability:
        let namespaced_capability = "default:github_webhook_deliveries";
        let count_namespaced = reopened
            .collection_cardinality(namespaced_capability)
            .unwrap();

        // If the persisted rows were stored under the non-namespaced key,
        // this will return 0 - demonstrating the mismatch
        println!("Rows under '{}': {}", capability, count_after);
        println!(
            "Rows under '{}': {}",
            namespaced_capability, count_namespaced
        );

        // This is the mismatch that causes Gate A to fail
        if count_namespaced != count_after {
            println!(
                "CAPABILITY MISMATCH DETECTED: {} != {}",
                count_after, count_namespaced
            );
        }

        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_mixed_capability_formats_in_single_db() {
        // This test shows the exact problem:
        // Some rows use "app:collection", others use "namespace:collection"
        // Cardinality is recomputed based on what's in the persisted log
        // But queries might look for a different format

        let path = temp_file();

        {
            let db = open(&path).unwrap();

            // Simulate old rows stored under application_id format
            let app_id = "my-app";
            let namespace = "default";
            let collection = "github_webhook_deliveries";

            // Write rows with one format
            let old_format_capability = format!("{}:{}", app_id, collection);
            let mutations_old = vec![
                AtomicMutation {
                    capability: old_format_capability.clone(),
                    key: "old-webhook-1".to_string(),
                    value: Some(serde_json::json!({"id": 1})),
                },
                AtomicMutation {
                    capability: old_format_capability.clone(),
                    key: "old-webhook-2".to_string(),
                    value: Some(serde_json::json!({"id": 2})),
                },
            ];

            db.apply_atomic_transaction("txn-old", None, &[], &mutations_old, None)
                .unwrap();

            // Write rows with new format (namespace-based)
            let new_format_capability = format!("{}:{}", namespace, collection);
            let mutations_new = vec![
                AtomicMutation {
                    capability: new_format_capability.clone(),
                    key: "new-webhook-3".to_string(),
                    value: Some(serde_json::json!({"id": 3})),
                },
                AtomicMutation {
                    capability: new_format_capability.clone(),
                    key: "new-webhook-4".to_string(),
                    value: Some(serde_json::json!({"id": 4})),
                },
            ];

            db.apply_atomic_transaction("txn-new", None, &[], &mutations_new, None)
                .unwrap();

            // Before restart, both formats should be queryable
            assert_eq!(
                db.collection_cardinality(&old_format_capability).unwrap(),
                2
            );
            assert_eq!(
                db.collection_cardinality(&new_format_capability).unwrap(),
                2
            );
            println!("Before restart: old_format={}, new_format={}", 2, 2);
        }

        // After restart, cardinality is recomputed from persisted log
        let reopened = open(&path).unwrap();
        let old_format = format!("{}:{}", "my-app", "github_webhook_deliveries");
        let new_format = format!("{}:{}", "default", "github_webhook_deliveries");

        let old_count = reopened.collection_cardinality(&old_format).unwrap();
        let new_count = reopened.collection_cardinality(&new_format).unwrap();

        println!(
            "After restart: old_format={}, new_format={}",
            old_count, new_count
        );
        println!(
            "Cardinalities after restart: {}",
            reopened
                .list_cardinalities()
                .unwrap()
                .iter()
                .map(|(k, v)| format!("'{}': {}", k, v))
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Both should still exist with their own counts
        assert_eq!(old_count, 2, "Old format rows should be reconstructed");
        assert_eq!(new_count, 2, "New format rows should be reconstructed");

        // But if query only looks for new_format and old data was migrated incorrectly,
        // the query would see 0 instead of the total.

        let _ = fs::remove_file(path);
    }
}
