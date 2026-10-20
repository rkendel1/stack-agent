/// Distributed Capability Routing module
///
/// Handles routing capability execution requests to appropriate peers based on:
/// - Local vs remote capability availability
/// - Required state version compatibility
/// - Peer state acquisition capabilities
/// - Execution ownership semantics
use crate::{CapabilityExecutionResult, CapabilityRef, DistributedCapabilityAdvertisement};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Route decision for capability execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RouteDecision {
    /// Execute locally
    Local,
    /// Execute remotely at specific peer
    Remote(String), // peer_id
    /// No suitable provider found
    Unavailable,
}

/// State version tracking for a peer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerStateVersion {
    /// Peer ID
    pub peer_id: String,
    /// Namespace
    pub namespace: String,
    /// Current version at this peer
    pub version: u64,
    /// When the version was last updated
    pub updated_ms: u128,
}

impl PeerStateVersion {
    pub fn new(peer_id: String, namespace: String, version: u64) -> Self {
        Self {
            peer_id,
            namespace,
            version,
            updated_ms: crate::now_ms(),
        }
    }
}

/// State-aware capability routing engine
pub struct CapabilityRouter {
    /// Local capabilities
    local_capabilities: HashMap<String, DistributedCapabilityAdvertisement>,
    /// Remote capabilities by peer
    remote_capabilities: HashMap<String, Vec<DistributedCapabilityAdvertisement>>,
    /// Peer state versions
    peer_state_versions: HashMap<String, PeerStateVersion>,
}

impl CapabilityRouter {
    pub fn new() -> Self {
        Self {
            local_capabilities: HashMap::new(),
            remote_capabilities: HashMap::new(),
            peer_state_versions: HashMap::new(),
        }
    }

    /// Register a local capability
    pub fn register_local(&mut self, ad: DistributedCapabilityAdvertisement) {
        self.local_capabilities
            .insert(ad.capability_ref.canonical(), ad);
    }

    /// Register a remote capability
    pub fn register_remote(&mut self, ad: DistributedCapabilityAdvertisement) {
        let peer_id = ad.peer_id.clone();
        self.remote_capabilities
            .entry(peer_id)
            .or_insert_with(Vec::new)
            .push(ad);
    }

    /// Update peer state version
    pub fn update_peer_state(&mut self, peer_id: String, namespace: String, version: u64) {
        let key = format!("{}:{}", peer_id, namespace);
        self.peer_state_versions
            .insert(key, PeerStateVersion::new(peer_id, namespace, version));
    }

    /// Get the state version a peer has for a namespace
    pub fn get_peer_state_version(&self, peer_id: &str, namespace: &str) -> u64 {
        let key = format!("{}:{}", peer_id, namespace);
        self.peer_state_versions
            .get(&key)
            .map(|v| v.version)
            .unwrap_or(0)
    }

    /// Route a capability execution request
    pub fn route(
        &self,
        cap_ref: &CapabilityRef,
        required_state_version: u64,
        local_peer_id: &str,
    ) -> RouteDecision {
        let canonical = cap_ref.canonical();

        // Check if capability is available locally
        if self.local_capabilities.contains_key(&canonical) {
            // Check if local state meets requirements
            if self.check_state_compatibility(local_peer_id, cap_ref, required_state_version) {
                return RouteDecision::Local;
            }
        }

        // Find remote providers with compatible state
        let providers = self.find_compatible_providers(cap_ref, required_state_version);

        if let Some(provider) = providers.first() {
            RouteDecision::Remote(provider.peer_id.clone())
        } else {
            RouteDecision::Unavailable
        }
    }

    /// Check if a peer has compatible state version
    fn check_state_compatibility(
        &self,
        peer_id: &str,
        cap_ref: &CapabilityRef,
        required_version: u64,
    ) -> bool {
        let peer_version = self.get_peer_state_version(peer_id, &cap_ref.namespace);
        peer_version >= required_version
    }

    /// Find providers with compatible state versions
    pub fn find_compatible_providers(
        &self,
        cap_ref: &CapabilityRef,
        required_state_version: u64,
    ) -> Vec<&DistributedCapabilityAdvertisement> {
        let mut compatible = Vec::new();

        for (peer_id, capabilities) in &self.remote_capabilities {
            for cap_ad in capabilities {
                if cap_ad.capability_ref.canonical() == cap_ref.canonical() {
                    if self.check_state_compatibility(peer_id, cap_ref, required_state_version) {
                        compatible.push(cap_ad);
                    }
                }
            }
        }

        compatible
    }

    /// Check if capability requires state acquisition
    pub fn requires_state_acquisition(
        &self,
        provider_peer: &str,
        cap_ref: &CapabilityRef,
        required_state_version: u64,
    ) -> bool {
        let peer_version = self.get_peer_state_version(provider_peer, &cap_ref.namespace);
        peer_version < required_state_version
    }

    /// Plan state acquisition - list namespaces that need to be acquired
    pub fn plan_state_acquisition(
        &self,
        provider_peer: &str,
        cap_ref: &CapabilityRef,
        required_state_version: u64,
    ) -> Vec<String> {
        let peer_version = self.get_peer_state_version(provider_peer, &cap_ref.namespace);
        if peer_version < required_state_version {
            vec![cap_ref.namespace.clone()]
        } else {
            Vec::new()
        }
    }
}

impl Default for CapabilityRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Execution claim tracker for single-owner semantics
pub struct ExecutionClaimer {
    /// Claims by execution ID
    claims: HashMap<String, ExecutionClaim>,
}

/// A claim on an execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionClaim {
    /// Execution ID
    pub execution_id: String,
    /// Peer that claimed it
    pub claimant: String,
    /// Timestamp of claim
    pub claimed_ms: u128,
}

impl ExecutionClaim {
    pub fn new(execution_id: String, claimant: String) -> Self {
        Self {
            execution_id,
            claimant,
            claimed_ms: crate::now_ms(),
        }
    }
}

impl ExecutionClaimer {
    pub fn new() -> Self {
        Self {
            claims: HashMap::new(),
        }
    }

    /// Attempt to claim an execution
    /// Returns the claim if successful, None if already claimed
    pub fn try_claim(&mut self, execution_id: String, claimant: String) -> Option<ExecutionClaim> {
        if self.claims.contains_key(&execution_id) {
            None
        } else {
            let claim = ExecutionClaim::new(execution_id.clone(), claimant);
            self.claims.insert(execution_id, claim.clone());
            Some(claim)
        }
    }

    /// Get the claimant of an execution
    pub fn get_claimant(&self, execution_id: &str) -> Option<&str> {
        self.claims.get(execution_id).map(|c| c.claimant.as_str())
    }

    /// Check if execution is claimed by a specific peer
    pub fn is_claimed_by(&self, execution_id: &str, claimant: &str) -> bool {
        self.claims
            .get(execution_id)
            .map(|c| c.claimant == claimant)
            .unwrap_or(false)
    }
}

impl Default for ExecutionClaimer {
    fn default() -> Self {
        Self::new()
    }
}

/// Result publication tracker
pub struct ExecutionResultPublisher {
    /// Published results by execution ID
    results: HashMap<String, CapabilityExecutionResult>,
}

impl ExecutionResultPublisher {
    pub fn new() -> Self {
        Self {
            results: HashMap::new(),
        }
    }

    /// Publish an execution result
    pub fn publish(&mut self, result: CapabilityExecutionResult) {
        self.results
            .insert(result.execution_ref.execution_id.clone(), result);
    }

    /// Get a published result
    pub fn get_result(&self, execution_id: &str) -> Option<&CapabilityExecutionResult> {
        self.results.get(execution_id)
    }

    /// Check if result exists
    pub fn has_result(&self, execution_id: &str) -> bool {
        self.results.contains_key(execution_id)
    }
}

impl Default for ExecutionResultPublisher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExecutionRef;

    fn make_cap_ref(namespace: &str, capability: &str, version: u32) -> CapabilityRef {
        CapabilityRef::new(namespace.to_string(), capability.to_string(), version)
    }

    fn make_capability_ad(
        namespace: &str,
        capability: &str,
        version: u32,
        peer_id: &str,
    ) -> DistributedCapabilityAdvertisement {
        DistributedCapabilityAdvertisement::new(
            make_cap_ref(namespace, capability, version),
            peer_id.to_string(),
        )
    }

    #[test]
    fn test_router_local_capability() {
        let mut router = CapabilityRouter::new();
        let ad = make_capability_ad("docs", "vector-search", 1, "local");
        router.register_local(ad);

        router.update_peer_state("local".to_string(), "docs".to_string(), 851);

        let cap_ref = make_cap_ref("docs", "vector-search", 1);
        let decision = router.route(&cap_ref, 851, "local");
        assert_eq!(decision, RouteDecision::Local);
    }

    #[test]
    fn test_router_remote_capability() {
        let mut router = CapabilityRouter::new();
        let ad = make_capability_ad("docs", "vector-search", 1, "peer-b");
        router.register_remote(ad);

        router.update_peer_state("peer-b".to_string(), "docs".to_string(), 851);

        let cap_ref = make_cap_ref("docs", "vector-search", 1);
        let decision = router.route(&cap_ref, 851, "peer-a");
        assert_eq!(decision, RouteDecision::Remote("peer-b".to_string()));
    }

    #[test]
    fn test_router_unavailable() {
        let router = CapabilityRouter::new();
        let cap_ref = make_cap_ref("docs", "vector-search", 1);
        let decision = router.route(&cap_ref, 851, "peer-a");
        assert_eq!(decision, RouteDecision::Unavailable);
    }

    #[test]
    fn test_router_state_mismatch() {
        let mut router = CapabilityRouter::new();
        let ad = make_capability_ad("docs", "vector-search", 1, "peer-b");
        router.register_remote(ad);

        // Peer only has version 800, but we need 851
        router.update_peer_state("peer-b".to_string(), "docs".to_string(), 800);

        let cap_ref = make_cap_ref("docs", "vector-search", 1);
        let decision = router.route(&cap_ref, 851, "peer-a");
        assert_eq!(decision, RouteDecision::Unavailable);
    }

    #[test]
    fn test_router_requires_state_acquisition() {
        let mut router = CapabilityRouter::new();
        router.update_peer_state("peer-b".to_string(), "docs".to_string(), 800);

        let cap_ref = make_cap_ref("docs", "vector-search", 1);
        assert!(router.requires_state_acquisition("peer-b", &cap_ref, 851));
    }

    #[test]
    fn test_execution_claimer_single_claim() {
        let mut claimer = ExecutionClaimer::new();
        let claim = claimer.try_claim("exec-123".to_string(), "peer-a".to_string());
        assert!(claim.is_some());
        assert_eq!(claim.unwrap().claimant, "peer-a");
    }

    #[test]
    fn test_execution_claimer_duplicate_claim() {
        let mut claimer = ExecutionClaimer::new();
        let claim1 = claimer.try_claim("exec-123".to_string(), "peer-a".to_string());
        let claim2 = claimer.try_claim("exec-123".to_string(), "peer-b".to_string());

        assert!(claim1.is_some());
        assert!(claim2.is_none());
    }

    #[test]
    fn test_execution_claimer_get_claimant() {
        let mut claimer = ExecutionClaimer::new();
        claimer.try_claim("exec-123".to_string(), "peer-a".to_string());

        let claimant = claimer.get_claimant("exec-123");
        assert_eq!(claimant, Some("peer-a"));
    }

    #[test]
    fn test_execution_result_publisher() {
        let mut publisher = ExecutionResultPublisher::new();
        let cap_ref = make_cap_ref("docs", "vector-search", 1);
        let exec_ref = ExecutionRef::new("exec-123".to_string(), cap_ref, "peer-b".to_string());
        let result = serde_json::json!({"results": [1, 2, 3]});
        let exec_result = CapabilityExecutionResult::success(exec_ref, result);

        publisher.publish(exec_result);

        assert!(publisher.has_result("exec-123"));
        assert!(publisher.get_result("exec-123").is_some());
    }
}
