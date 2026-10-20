/// HTTP diagnostic endpoint for cardinality state inspection
///
/// This module provides a debug/diagnostic HTTP endpoint that exposes cardinality
/// reconstruction state without returning actual row data.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardinalityDiagnosticResponse {
    /// Name of the collection being diagnosed
    pub collection_pattern: String,

    /// Actual row count for collections matching the pattern
    pub actual_row_count: usize,

    /// Maintained cardinality value stored in HashMap
    pub maintained_cardinality: u64,

    /// Mismatch indicator
    pub mismatch: bool,

    /// All capability keys found in HashMap that contain this pattern
    pub cardinality_keys: Vec<(String, u64)>,

    /// Context information for debugging
    pub context: CardinalityContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardinalityContext {
    /// List of all non-zero cardinalities (collection_name = count)
    /// Sorted by count descending
    pub all_cardinalities: Vec<(String, u64)>,

    /// Actual persisted capability strings matching pattern from inner.rows
    pub persisted_capability_keys: Vec<String>,

    /// Capability keys in cardinality HashMap matching pattern
    pub cardinality_map_keys: Vec<String>,

    /// Timestamp of diagnostic
    pub timestamp_ms: u64,

    /// Total collections with data
    pub total_collections: usize,

    /// Collections with cardinality 0
    pub zero_cardinality_count: usize,

    /// Total persisted rows across all collections
    pub total_rows: usize,
}

impl CardinalityDiagnosticResponse {
    pub fn new(
        collection_pattern: String,
        actual_row_count: usize,
        maintained_cardinality: u64,
        cardinality_keys: Vec<(String, u64)>,
    ) -> Self {
        Self {
            collection_pattern,
            actual_row_count,
            maintained_cardinality,
            mismatch: actual_row_count > 0 && maintained_cardinality == 0,
            cardinality_keys,
            context: CardinalityContext {
                all_cardinalities: vec![],
                persisted_capability_keys: vec![],
                cardinality_map_keys: vec![],
                timestamp_ms: 0,
                total_collections: 0,
                zero_cardinality_count: 0,
                total_rows: 0,
            },
        }
    }
}
