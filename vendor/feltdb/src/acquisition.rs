/// Content acquisition and materialization module
/// Handles distributed content transfer, verification, and local materialization
use crate::{ContentIdentity, FlowRef, PeerId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// Policy for acquiring content from the distributed fabric
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
pub enum AcquisitionPolicy {
    /// Fail if content is not available locally
    LocalOnly,
    /// Prefer local, fall back to remote if needed
    PreferLocal,
    /// Allow automatic remote acquisition
    RemoteAllowed,
    /// Block until content is available (required)
    Required,
}

impl Default for AcquisitionPolicy {
    fn default() -> Self {
        AcquisitionPolicy::PreferLocal
    }
}

/// Error types for content acquisition
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub enum ContentTransferError {
    /// Content not found on provider
    NotFound,
    /// Access denied to content
    Unauthorized,
    /// Provider temporarily unavailable
    Unavailable,
    /// Content hash does not match expected
    HashMismatch { expected: String, computed: String },
    /// Requested version not available
    VersionUnavailable,
    /// Generic acquisition error
    AcquisitionError(String),
}

impl std::fmt::Display for ContentTransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContentTransferError::NotFound => write!(f, "Content not found"),
            ContentTransferError::Unauthorized => write!(f, "Access denied"),
            ContentTransferError::Unavailable => write!(f, "Provider unavailable"),
            ContentTransferError::HashMismatch { expected, computed } => {
                write!(f, "Hash mismatch: expected {}, got {}", expected, computed)
            }
            ContentTransferError::VersionUnavailable => write!(f, "Version unavailable"),
            ContentTransferError::AcquisitionError(msg) => write!(f, "Acquisition error: {}", msg),
        }
    }
}

impl std::error::Error for ContentTransferError {}

/// Request for content from a remote peer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentRequest {
    /// Unique request ID for correlation
    pub request_id: String,
    /// Reference to the content being requested
    pub flow_ref: FlowRef,
    /// Expected content identity for verification
    pub content_identity: Option<ContentIdentity>,
}

impl ContentRequest {
    /// Create a new content request
    pub fn new(
        request_id: String,
        flow_ref: FlowRef,
        content_identity: Option<ContentIdentity>,
    ) -> Self {
        Self {
            request_id,
            flow_ref,
            content_identity,
        }
    }
}

/// Response containing acquired content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentResponse {
    /// Correlation ID matching the request
    pub request_id: String,
    /// Content identity of the payload
    pub content_identity: ContentIdentity,
    /// The actual content bytes
    pub payload: Vec<u8>,
    /// Peer that provided this content
    pub provider: PeerId,
    /// Timestamp when content was acquired
    pub acquired_at_ms: u128,
}

impl ContentResponse {
    /// Create a new content response
    pub fn new(
        request_id: String,
        content_identity: ContentIdentity,
        payload: Vec<u8>,
        provider: PeerId,
    ) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);

        Self {
            request_id,
            content_identity,
            payload,
            provider,
            acquired_at_ms: now_ms,
        }
    }
}

/// Result of content acquisition attempt
#[derive(Debug, Clone)]
pub struct AcquisitionResult {
    /// The acquired content
    pub payload: Vec<u8>,
    /// Where the content came from
    pub source_peer: PeerId,
    /// When it was acquired
    pub acquired_at_ms: u128,
}

impl AcquisitionResult {
    /// Create a new acquisition result
    pub fn new(payload: Vec<u8>, source_peer: PeerId) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);

        Self {
            payload,
            source_peer,
            acquired_at_ms: now_ms,
        }
    }
}

/// In-flight acquisition tracking for deduplication
#[derive(Clone)]
pub struct AcquisitionDeduplicator {
    /// Map from acquisition key to in-flight acquisitions
    in_flight: Arc<
        Mutex<
            HashMap<
                String,
                Vec<
                    tokio::sync::oneshot::Receiver<Result<AcquisitionResult, ContentTransferError>>,
                >,
            >,
        >,
    >,
}

impl AcquisitionDeduplicator {
    /// Create a new acquisition deduplicator
    pub fn new() -> Self {
        Self {
            in_flight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check if an acquisition is already in flight
    pub fn is_in_flight(&self, key: &str) -> Result<bool, String> {
        let in_flight = self.in_flight.lock().map_err(|e| e.to_string())?;
        Ok(in_flight.contains_key(key) && !in_flight[key].is_empty())
    }

    /// Register a new in-flight acquisition
    /// Returns true if this is the first acquisition, false if joining existing
    pub fn register_acquisition(
        &self,
        key: String,
        receiver: tokio::sync::oneshot::Receiver<Result<AcquisitionResult, ContentTransferError>>,
    ) -> Result<bool, String> {
        let mut in_flight = self.in_flight.lock().map_err(|e| e.to_string())?;
        let is_first = !in_flight.contains_key(&key) || in_flight[&key].is_empty();

        in_flight.entry(key).or_insert_with(Vec::new).push(receiver);

        Ok(is_first)
    }

    /// Mark an acquisition as complete and notify all waiters
    pub fn mark_complete(
        &self,
        key: &str,
        _result: Result<AcquisitionResult, ContentTransferError>,
    ) -> Result<(), String> {
        // In a real implementation, we would notify all waiting receivers
        // For now, we just remove the entry
        let mut in_flight = self.in_flight.lock().map_err(|e| e.to_string())?;
        in_flight.remove(key);
        Ok(())
    }
}

impl Default for AcquisitionDeduplicator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acquisition_policy_default() {
        assert_eq!(AcquisitionPolicy::default(), AcquisitionPolicy::PreferLocal);
    }

    #[test]
    fn test_content_request_creation() {
        let flow_ref = FlowRef::new("docs", "123");
        let content_id = ContentIdentity::new("abc123");

        let request = ContentRequest::new("req-1".to_string(), flow_ref.clone(), Some(content_id));

        assert_eq!(request.request_id, "req-1");
        assert_eq!(request.flow_ref.object_id, "123");
    }

    #[test]
    fn test_content_response_creation() {
        let content_id = ContentIdentity::new("abc123");
        let peer = PeerId::new("peer-a");
        let payload = vec![1, 2, 3];

        let response = ContentResponse::new(
            "req-1".to_string(),
            content_id,
            payload.clone(),
            peer.clone(),
        );

        assert_eq!(response.request_id, "req-1");
        assert_eq!(response.payload, payload);
        assert_eq!(response.provider, peer);
    }

    #[test]
    fn test_acquisition_result_creation() {
        let peer = PeerId::new("peer-a");
        let payload = vec![1, 2, 3];

        let result = AcquisitionResult::new(payload.clone(), peer.clone());

        assert_eq!(result.payload, payload);
        assert_eq!(result.source_peer, peer);
        assert!(result.acquired_at_ms > 0);
    }

    #[test]
    fn test_acquisition_deduplicator_creation() {
        let dedup = AcquisitionDeduplicator::new();
        assert!(!dedup.is_in_flight("key-1").unwrap());
    }

    #[test]
    fn test_content_transfer_error_display() {
        let err = ContentTransferError::NotFound;
        assert_eq!(err.to_string(), "Content not found");

        let err = ContentTransferError::HashMismatch {
            expected: "abc".to_string(),
            computed: "def".to_string(),
        };
        assert!(err.to_string().contains("abc"));
    }
}
