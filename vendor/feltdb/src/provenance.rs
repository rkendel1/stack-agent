use crate::{CapabilityId, PeerId};
/// Provenance tracking for distinguishing canonical from derived state
///
/// This module ensures that derived state (from capabilities) is clearly
/// marked and can never be mistaken for canonical application state.
use serde::{Deserialize, Serialize};

/// Source of an operation - tracks where it originated
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OperationSource {
    /// User-initiated operation (canonical)
    User,
    /// Operation from sync/remote peer (canonical)
    Sync,
    /// Operation derived from a capability (derived)
    Capability(CapabilityId),
}

impl std::fmt::Display for OperationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperationSource::User => write!(f, "user"),
            OperationSource::Sync => write!(f, "sync"),
            OperationSource::Capability(id) => write!(f, "capability:{}", id),
        }
    }
}

impl OperationSource {
    /// Check if this source represents canonical state
    pub fn is_canonical(&self) -> bool {
        matches!(self, OperationSource::User | OperationSource::Sync)
    }

    /// Check if this source represents derived state
    pub fn is_derived(&self) -> bool {
        !self.is_canonical()
    }
}

/// Metadata tracking how content was acquired from the distributed fabric
/// This metadata is NOT canonical application state - it's purely informational
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcquisitionMetadata {
    /// Which peer provided this content
    pub source_peer: PeerId,
    /// When the content was acquired (ms since UNIX epoch)
    pub acquired_at_ms: u128,
    /// Whether this content is cached (vs canonical)
    pub is_cached: bool,
    /// Vector clock representing the causal state when content was acquired
    #[serde(default)]
    pub vector_clock: Option<std::collections::HashMap<String, u64>>,
}

impl AcquisitionMetadata {
    /// Create new acquisition metadata
    pub fn new(source_peer: PeerId, is_cached: bool) -> Self {
        Self {
            source_peer,
            acquired_at_ms: crate::now_ms(),
            is_cached,
            vector_clock: None,
        }
    }

    /// Mark when this acquisition occurred
    pub fn with_timestamp(mut self, acquired_at_ms: u128) -> Self {
        self.acquired_at_ms = acquired_at_ms;
        self
    }

    /// Set the vector clock for this acquisition
    pub fn with_vector_clock(
        mut self,
        vector_clock: std::collections::HashMap<String, u64>,
    ) -> Self {
        self.vector_clock = Some(vector_clock);
        self
    }
}

/// Provenance information for derived state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedStateProvenance {
    /// Which capability produced this derived state
    pub capability_id: CapabilityId,
    /// The canonical operation that triggered this derivation
    pub source_operation_id: u64,
    /// Timestamp when this derivation was created (ms since UNIX epoch)
    pub created_ms: u128,
}

impl DerivedStateProvenance {
    pub fn new(capability_id: CapabilityId, source_operation_id: u64) -> Self {
        Self {
            capability_id,
            source_operation_id,
            created_ms: crate::now_ms(),
        }
    }
}

/// Marker trait for values that represent derived state
/// This should be implemented by capability-generated state
pub trait DerivedState {
    /// Get the provenance of this derived state
    fn provenance(&self) -> &DerivedStateProvenance;

    /// Verify that this value is indeed derived state
    fn verify_derived(&self) -> bool {
        true
    }
}

/// Wrapper for derived state to ensure it's properly marked
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedValue<T> {
    /// The actual derived value
    pub value: T,
    /// Provenance information
    pub provenance: DerivedStateProvenance,
}

impl<T> DerivedValue<T> {
    pub fn new(value: T, capability_id: CapabilityId, source_operation_id: u64) -> Self {
        Self {
            value,
            provenance: DerivedStateProvenance::new(capability_id, source_operation_id),
        }
    }

    /// Unwrap and consume the derived value
    pub fn into_inner(self) -> T {
        self.value
    }

    /// Get a reference to the value
    pub fn inner(&self) -> &T {
        &self.value
    }

    /// Get a mutable reference to the value
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<T> DerivedState for DerivedValue<T> {
    fn provenance(&self) -> &DerivedStateProvenance {
        &self.provenance
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operation_source_canonical() {
        assert!(OperationSource::User.is_canonical());
        assert!(OperationSource::Sync.is_canonical());
    }

    #[test]
    fn test_operation_source_derived() {
        let derived = OperationSource::Capability(CapabilityId::new("search".to_string()));
        assert!(derived.is_derived());
        assert!(!derived.is_canonical());
    }

    #[test]
    fn test_derived_value_wrapper() {
        let cap_id = CapabilityId::new("search".to_string());
        let derived = DerivedValue::new(vec!["result1", "result2"], cap_id.clone(), 100);

        assert_eq!(derived.provenance().capability_id, cap_id);
        assert_eq!(derived.provenance().source_operation_id, 100);
        assert_eq!(derived.inner().len(), 2);
    }

    #[test]
    fn test_derived_value_into_inner() {
        let derived = DerivedValue::new(vec![1, 2, 3], CapabilityId::new("search".to_string()), 50);

        let inner = derived.into_inner();
        assert_eq!(inner, vec![1, 2, 3]);
    }

    #[test]
    fn test_acquisition_metadata_creation() {
        let peer = PeerId::new("peer-a");
        let metadata = AcquisitionMetadata::new(peer.clone(), true);

        assert_eq!(metadata.source_peer, peer);
        assert!(metadata.is_cached);
        assert!(metadata.acquired_at_ms > 0);
    }

    #[test]
    fn test_acquisition_metadata_with_timestamp() {
        let peer = PeerId::new("peer-a");
        let metadata = AcquisitionMetadata::new(peer, true).with_timestamp(12345);

        assert_eq!(metadata.acquired_at_ms, 12345);
    }
}
