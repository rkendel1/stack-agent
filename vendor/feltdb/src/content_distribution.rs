/// Content addressable storage and acquisition module
use crate::{
    AcquisitionPolicy, ContentIdentity, ContentTransferError, FlowRef, PeerAdvertisement, PeerId,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A content addressable store that tracks where content lives
#[derive(Clone)]
pub struct ContentAddressableStore {
    /// Map from content hash to actual content
    content: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Map from content hash to which peers have it
    content_locations: Arc<Mutex<HashMap<String, Vec<PeerId>>>>,
}

impl ContentAddressableStore {
    /// Create a new content addressable store
    pub fn new() -> Self {
        Self {
            content: Arc::new(Mutex::new(HashMap::new())),
            content_locations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Store content and track its hash
    pub fn store(&self, content: Vec<u8>, hash: String) -> Result<(), String> {
        let mut store = self.content.lock().map_err(|e| e.to_string())?;
        store.insert(hash, content);
        Ok(())
    }

    /// Retrieve content by hash
    pub fn get(&self, hash: &str) -> Result<Option<Vec<u8>>, String> {
        let store = self.content.lock().map_err(|e| e.to_string())?;
        Ok(store.get(hash).cloned())
    }

    /// Record that a peer has a piece of content
    pub fn record_location(&self, hash: String, peer: PeerId) -> Result<(), String> {
        let mut locations = self.content_locations.lock().map_err(|e| e.to_string())?;
        locations.entry(hash).or_insert_with(Vec::new).push(peer);
        Ok(())
    }

    /// Find peers that have a specific piece of content
    pub fn find_peers_for_content(&self, hash: &str) -> Result<Vec<PeerId>, String> {
        let locations = self.content_locations.lock().map_err(|e| e.to_string())?;
        Ok(locations.get(hash).cloned().unwrap_or_default())
    }

    /// Check if content exists locally
    pub fn has_content(&self, hash: &str) -> Result<bool, String> {
        let store = self.content.lock().map_err(|e| e.to_string())?;
        Ok(store.contains_key(hash))
    }
}

impl Default for ContentAddressableStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Reference resolution with fallback to peer acquisition
pub struct DistributedReferenceResolver {
    /// Local content store
    _local_store: ContentAddressableStore,
}

impl DistributedReferenceResolver {
    /// Create a new distributed reference resolver
    pub fn new(local_store: ContentAddressableStore) -> Self {
        Self {
            _local_store: local_store,
        }
    }

    /// Try to resolve a reference, first locally then from peers
    pub fn resolve(
        &self,
        flow_ref: &FlowRef,
        available_peers: &[PeerAdvertisement],
    ) -> Result<Option<Vec<u8>>, String> {
        // Step 1: Try local resolution
        // Check if we have this content hash locally
        // For now, just indicate it wasn't found
        // In a real implementation, we'd query from local content store

        // Step 2: Look for peers that might have this
        for peer_ad in available_peers {
            if peer_ad.provides_namespace(&flow_ref.namespace) {
                // This peer might have the content
                // In a full implementation, we'd make a remote call here
                return Ok(None); // Placeholder
            }
        }

        Ok(None)
    }
}

/// Projection distribution - tracks where projections are hosted
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectionDistribution {
    /// Namespace this projection is for
    pub namespace: String,
    /// Projection type (e.g., "search", "vector")
    pub projection_type: String,
    /// Peer hosting this projection
    pub hosted_by: PeerId,
    /// Last update timestamp
    pub updated_at_ms: u128,
}

impl ProjectionDistribution {
    /// Create a new projection distribution entry
    pub fn new(
        namespace: impl Into<String>,
        projection_type: impl Into<String>,
        hosted_by: PeerId,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            projection_type: projection_type.into(),
            hosted_by,
            updated_at_ms: crate::now_ms(),
        }
    }
}

/// Capability location registry - tracks where capabilities are available
#[derive(Clone)]
pub struct CapabilityLocationRegistry {
    /// Map from capability name to list of peers providing it
    capability_locations: Arc<Mutex<HashMap<String, Vec<PeerId>>>>,
    /// Map from peer ID to capabilities it provides
    peer_capabilities: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl CapabilityLocationRegistry {
    /// Create a new capability location registry
    pub fn new() -> Self {
        Self {
            capability_locations: Arc::new(Mutex::new(HashMap::new())),
            peer_capabilities: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register that a peer provides a capability
    pub fn register_capability(&self, capability: String, peer: PeerId) -> Result<(), String> {
        let mut locations = self
            .capability_locations
            .lock()
            .map_err(|e| e.to_string())?;
        locations
            .entry(capability.clone())
            .or_insert_with(Vec::new)
            .push(peer.clone());

        let mut peer_caps = self.peer_capabilities.lock().map_err(|e| e.to_string())?;
        peer_caps
            .entry(peer.id.clone())
            .or_insert_with(Vec::new)
            .push(capability);

        Ok(())
    }

    /// Find peers that provide a capability
    pub fn peers_for_capability(&self, capability: &str) -> Result<Vec<PeerId>, String> {
        let locations = self
            .capability_locations
            .lock()
            .map_err(|e| e.to_string())?;
        Ok(locations.get(capability).cloned().unwrap_or_default())
    }

    /// Get all capabilities provided by a peer
    pub fn capabilities_for_peer(&self, peer_id: &PeerId) -> Result<Vec<String>, String> {
        let peer_caps = self.peer_capabilities.lock().map_err(|e| e.to_string())?;
        Ok(peer_caps.get(&peer_id.id).cloned().unwrap_or_default())
    }
}

impl Default for CapabilityLocationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Manager for acquiring content from distributed peers
#[derive(Clone)]
pub struct ContentAcquisitionManager {
    /// Local content store
    content_store: ContentAddressableStore,
    /// Peer registry for discovering providers
    peer_registry: Arc<Mutex<HashMap<String, PeerAdvertisement>>>,
}

impl ContentAcquisitionManager {
    /// Create a new content acquisition manager
    pub fn new(content_store: ContentAddressableStore) -> Self {
        Self {
            content_store,
            peer_registry: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a peer for content acquisition
    pub fn register_peer(&self, advertisement: PeerAdvertisement) -> Result<(), String> {
        let mut registry = self.peer_registry.lock().map_err(|e| e.to_string())?;
        registry.insert(advertisement.peer_id.id.clone(), advertisement);
        Ok(())
    }

    /// Resolve a reference using the acquisition policy
    pub fn resolve(
        &self,
        flow_ref: &FlowRef,
        policy: AcquisitionPolicy,
    ) -> Result<Option<Vec<u8>>, String> {
        // Step 1: Always try local first
        let local_key = format!("{}:{}", flow_ref.namespace, flow_ref.object_id);

        // Check local content store by namespace:id key
        // First, try to get it directly
        if let Ok(Some(content)) = self.content_store.get(&local_key) {
            return Ok(Some(content));
        }

        // Step 2: Handle acquisition policies
        match policy {
            AcquisitionPolicy::LocalOnly => {
                // Fail if not available locally
                Ok(None)
            }
            AcquisitionPolicy::PreferLocal
            | AcquisitionPolicy::RemoteAllowed
            | AcquisitionPolicy::Required => {
                // Try to find a peer that has this content
                // In a real implementation, this would be async and perform network I/O
                self.find_and_acquire_from_peer(flow_ref)
            }
        }
    }

    /// Find a peer that can provide content and attempt acquisition
    fn find_and_acquire_from_peer(&self, flow_ref: &FlowRef) -> Result<Option<Vec<u8>>, String> {
        let registry = self.peer_registry.lock().map_err(|e| e.to_string())?;

        // Find peers that provide this namespace
        let providers: Vec<_> = registry
            .values()
            .filter(|ad| ad.provides_namespace(&flow_ref.namespace))
            .collect();

        if providers.is_empty() {
            return Ok(None);
        }

        // For now, try the first available provider
        // In a real implementation, this would use load-aware selection
        if providers.first().is_some() {
            // Simulate content acquisition by checking what we already have
            // In reality, this would perform a remote RPC call
            Ok(None)
        } else {
            Ok(None)
        }
    }

    /// Verify content against its content identity
    pub fn verify_content(
        content: &[u8],
        expected_identity: &ContentIdentity,
    ) -> Result<(), ContentTransferError> {
        let computed_identity = ContentIdentity::from_bytes(content);

        if computed_identity.hash == expected_identity.hash {
            Ok(())
        } else {
            Err(ContentTransferError::HashMismatch {
                expected: expected_identity.hash.clone(),
                computed: computed_identity.hash,
            })
        }
    }

    /// Materialize acquired content into local storage
    pub fn materialize(
        &self,
        flow_ref: &FlowRef,
        content: Vec<u8>,
        content_identity: &ContentIdentity,
    ) -> Result<(), String> {
        // Verify content hash
        Self::verify_content(&content, content_identity).map_err(|e| e.to_string())?;

        // Store in content store
        let key = format!("{}:{}", flow_ref.namespace, flow_ref.object_id);
        self.content_store.store(content, key)?;

        Ok(())
    }

    /// Get content from local store
    pub fn get_local(&self, flow_ref: &FlowRef) -> Result<Option<Vec<u8>>, String> {
        let key = format!("{}:{}", flow_ref.namespace, flow_ref.object_id);
        self.content_store.get(&key)
    }

    /// Check if content exists locally
    pub fn has_local(&self, flow_ref: &FlowRef) -> Result<bool, String> {
        let key = format!("{}:{}", flow_ref.namespace, flow_ref.object_id);
        self.content_store.has_content(&key)
    }
}

impl Default for ContentAcquisitionManager {
    fn default() -> Self {
        Self::new(ContentAddressableStore::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_addressable_store() {
        let store = ContentAddressableStore::new();
        let content = b"hello world".to_vec();
        let hash = "abc123".to_string();

        store.store(content.clone(), hash.clone()).unwrap();
        let retrieved = store.get(&hash).unwrap();
        assert_eq!(retrieved, Some(content));
    }

    #[test]
    fn test_content_addressable_store_locations() {
        let store = ContentAddressableStore::new();
        let hash = "abc123".to_string();
        let peer_a = PeerId::new("peer-a");
        let peer_b = PeerId::new("peer-b");

        store.record_location(hash.clone(), peer_a).unwrap();
        store.record_location(hash.clone(), peer_b).unwrap();

        let peers = store.find_peers_for_content(&hash).unwrap();
        assert_eq!(peers.len(), 2);
    }

    #[test]
    fn test_projection_distribution() {
        let peer = PeerId::new("search-node");
        let proj = ProjectionDistribution::new("docs", "search", peer.clone());

        assert_eq!(proj.namespace, "docs");
        assert_eq!(proj.projection_type, "search");
        assert_eq!(proj.hosted_by.id, "search-node");
    }

    #[test]
    fn test_capability_location_registry() {
        let registry = CapabilityLocationRegistry::new();
        let peer_a = PeerId::new("peer-a");
        let peer_b = PeerId::new("peer-b");

        registry
            .register_capability("search:docs".to_string(), peer_a.clone())
            .unwrap();
        registry
            .register_capability("search:docs".to_string(), peer_b.clone())
            .unwrap();
        registry
            .register_capability("vector:embeddings".to_string(), peer_b.clone())
            .unwrap();

        let search_peers = registry.peers_for_capability("search:docs").unwrap();
        assert_eq!(search_peers.len(), 2);

        let vector_peers = registry.peers_for_capability("vector:embeddings").unwrap();
        assert_eq!(vector_peers.len(), 1);

        let peer_a_caps = registry.capabilities_for_peer(&peer_a).unwrap();
        assert_eq!(peer_a_caps.len(), 1);

        let peer_b_caps = registry.capabilities_for_peer(&peer_b).unwrap();
        assert_eq!(peer_b_caps.len(), 2);
    }

    #[test]
    fn test_content_acquisition_manager_local_only() {
        let store = ContentAddressableStore::new();
        let manager = ContentAcquisitionManager::new(store);

        let flow_ref = FlowRef::new("docs", "123");

        // Should not find content with LocalOnly policy
        let result = manager
            .resolve(&flow_ref, AcquisitionPolicy::LocalOnly)
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_content_acquisition_manager_materialize() {
        let store = ContentAddressableStore::new();
        let manager = ContentAcquisitionManager::new(store);

        let flow_ref = FlowRef::new("docs", "123");
        let content = b"test content".to_vec();
        let content_id = ContentIdentity::from_bytes(&content);

        // Materialize content
        manager
            .materialize(&flow_ref, content.clone(), &content_id)
            .unwrap();

        // Verify it's stored locally
        let retrieved = manager.get_local(&flow_ref).unwrap();
        assert_eq!(retrieved, Some(content));
    }

    #[test]
    fn test_content_acquisition_manager_has_local() {
        let store = ContentAddressableStore::new();
        let manager = ContentAcquisitionManager::new(store);

        let flow_ref = FlowRef::new("docs", "123");

        // Initially should not have content
        assert!(!manager.has_local(&flow_ref).unwrap());

        // Materialize content
        let content = b"test content".to_vec();
        let content_id = ContentIdentity::from_bytes(&content);
        manager
            .materialize(&flow_ref, content, &content_id)
            .unwrap();

        // Now should have it
        assert!(manager.has_local(&flow_ref).unwrap());
    }

    #[test]
    fn test_verify_content_success() {
        let content = b"test content".to_vec();
        let content_id = ContentIdentity::from_bytes(&content);

        let result = ContentAcquisitionManager::verify_content(&content, &content_id);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_content_hash_mismatch() {
        let content = b"test content".to_vec();
        let wrong_id = ContentIdentity::new("wronghash");

        let result = ContentAcquisitionManager::verify_content(&content, &wrong_id);
        assert!(result.is_err());
    }
}
