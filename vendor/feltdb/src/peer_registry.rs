/// Peer registry module for distributed FeltDB fabric
use crate::{PeerAdvertisement, PeerId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A registry of peers in the distributed fabric
#[derive(Clone)]
pub struct PeerRegistry {
    peers: Arc<Mutex<HashMap<String, PeerAdvertisement>>>,
}

impl PeerRegistry {
    /// Create a new peer registry
    pub fn new() -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a peer with its advertisement
    pub fn register(&self, advertisement: PeerAdvertisement) -> Result<(), String> {
        let mut peers = self.peers.lock().map_err(|e| e.to_string())?;
        peers.insert(advertisement.peer_id.id.clone(), advertisement);
        Ok(())
    }

    /// Unregister a peer
    pub fn unregister(&self, peer_id: &PeerId) -> Result<Option<PeerAdvertisement>, String> {
        let mut peers = self.peers.lock().map_err(|e| e.to_string())?;
        Ok(peers.remove(&peer_id.id))
    }

    /// Get a peer's advertisement
    pub fn get(&self, peer_id: &PeerId) -> Result<Option<PeerAdvertisement>, String> {
        let peers = self.peers.lock().map_err(|e| e.to_string())?;
        Ok(peers.get(&peer_id.id).cloned())
    }

    /// Find all peers that provide a specific namespace
    pub fn peers_for_namespace(&self, namespace: &str) -> Result<Vec<PeerAdvertisement>, String> {
        let peers = self.peers.lock().map_err(|e| e.to_string())?;
        let result = peers
            .values()
            .filter(|ad| ad.provides_namespace(namespace))
            .cloned()
            .collect();
        Ok(result)
    }

    /// Find all peers that provide a specific capability
    pub fn peers_for_capability(&self, capability: &str) -> Result<Vec<PeerAdvertisement>, String> {
        let peers = self.peers.lock().map_err(|e| e.to_string())?;
        let result = peers
            .values()
            .filter(|ad| ad.provides_capability(capability))
            .cloned()
            .collect();
        Ok(result)
    }

    /// Get all registered peers
    pub fn all_peers(&self) -> Result<Vec<PeerAdvertisement>, String> {
        let peers = self.peers.lock().map_err(|e| e.to_string())?;
        Ok(peers.values().cloned().collect())
    }

    /// Get the count of registered peers
    pub fn peer_count(&self) -> Result<usize, String> {
        let peers = self.peers.lock().map_err(|e| e.to_string())?;
        Ok(peers.len())
    }

    /// Update a peer's advertisement
    pub fn update(&self, advertisement: PeerAdvertisement) -> Result<(), String> {
        let mut peers = self.peers.lock().map_err(|e| e.to_string())?;
        peers.insert(advertisement.peer_id.id.clone(), advertisement);
        Ok(())
    }
}

impl Default for PeerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PeerAdvertisement;

    #[test]
    fn test_peer_registry_register() {
        let registry = PeerRegistry::new();
        let peer = PeerId::new("peer-a");
        let ad = PeerAdvertisement::new(peer.clone()).with_namespace("docs");

        registry.register(ad).unwrap();

        let retrieved = registry.get(&peer).unwrap();
        assert!(retrieved.is_some());
        assert!(retrieved.unwrap().provides_namespace("docs"));
    }

    #[test]
    fn test_peer_registry_peers_for_namespace() {
        let registry = PeerRegistry::new();

        let peer_a = PeerId::new("peer-a");
        let ad_a = PeerAdvertisement::new(peer_a).with_namespace("docs");
        registry.register(ad_a).unwrap();

        let peer_b = PeerId::new("peer-b");
        let ad_b = PeerAdvertisement::new(peer_b)
            .with_namespace("orders")
            .with_namespace("docs");
        registry.register(ad_b).unwrap();

        let peer_c = PeerId::new("peer-c");
        let ad_c = PeerAdvertisement::new(peer_c).with_namespace("embeddings");
        registry.register(ad_c).unwrap();

        let docs_peers = registry.peers_for_namespace("docs").unwrap();
        assert_eq!(docs_peers.len(), 2);

        let orders_peers = registry.peers_for_namespace("orders").unwrap();
        assert_eq!(orders_peers.len(), 1);

        let embeddings_peers = registry.peers_for_namespace("embeddings").unwrap();
        assert_eq!(embeddings_peers.len(), 1);
    }

    #[test]
    fn test_peer_registry_peers_for_capability() {
        let registry = PeerRegistry::new();

        let peer_a = PeerId::new("peer-a");
        let ad_a = PeerAdvertisement::new(peer_a).with_capability("search:docs");
        registry.register(ad_a).unwrap();

        let peer_b = PeerId::new("peer-b");
        let ad_b = PeerAdvertisement::new(peer_b)
            .with_capability("vector:embeddings")
            .with_capability("search:docs");
        registry.register(ad_b).unwrap();

        let search_peers = registry.peers_for_capability("search:docs").unwrap();
        assert_eq!(search_peers.len(), 2);

        let vector_peers = registry.peers_for_capability("vector:embeddings").unwrap();
        assert_eq!(vector_peers.len(), 1);
    }

    #[test]
    fn test_peer_registry_unregister() {
        let registry = PeerRegistry::new();
        let peer = PeerId::new("peer-a");
        let ad = PeerAdvertisement::new(peer.clone());

        registry.register(ad).unwrap();
        assert_eq!(registry.peer_count().unwrap(), 1);

        registry.unregister(&peer).unwrap();
        assert_eq!(registry.peer_count().unwrap(), 0);
    }
}
