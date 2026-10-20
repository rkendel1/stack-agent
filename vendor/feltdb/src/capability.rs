use crate::Operation;
/// Capability and Extension Runtime module
///
/// Capabilities are extensible projections that consume the FeltDB operation stream
/// and maintain rebuildable derived state. They follow a generic contract:
///
/// canonical operation → capability → derived state → capability query
///
/// This keeps the core log generic while allowing specialized behavior.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;

/// Unique capability identifier
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CapabilityId(pub String);

impl fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl CapabilityId {
    pub fn new(id: String) -> Self {
        CapabilityId(id)
    }
}

/// The roles a capability can fulfill
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CapabilityRole {
    /// Triggers and executes business logic
    ExecutionEngine,
    /// Maintains a projection/index of canonical state
    Projection,
    /// Both executes and maintains a projection
    ExecutionWithProjection,
}

/// Lifecycle states for a capability
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CapabilityLifecycleState {
    /// Capability has been registered but not yet initialized
    Registered,
    /// Capability is initializing its state
    Initializing,
    /// Capability is catching up with historical operations
    CatchingUp,
    /// Capability is active and processing new operations
    Active,
    /// Capability is paused
    Paused,
    /// Capability is being rebuilt
    Rebuilding,
    /// Capability has been removed
    Removed,
}

/// Context passed to capability operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityContext {
    /// The capability ID
    pub capability_id: CapabilityId,
    /// Instance ID of the FeltDB instance
    pub instance_id: String,
    /// Current sequence number
    pub sequence: u64,
    /// Capability version
    pub version: u32,
}

/// Query structure for capability queries
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityQuery {
    /// Query type (e.g., "search", "vector_find", "analytics")
    pub query_type: String,
    /// Query parameters
    pub params: Value,
}

impl CapabilityQuery {
    pub fn new(query_type: String, params: Value) -> Self {
        Self { query_type, params }
    }
}

/// Checkpoint for capability progress tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityCheckpoint {
    /// The capability ID
    pub capability_id: CapabilityId,
    /// Last processed instance ID
    pub last_processed_instance_id: String,
    /// Last processed sequence number
    pub last_processed_sequence: u64,
    /// Current lifecycle state
    pub lifecycle_state: CapabilityLifecycleState,
    /// Timestamp of last checkpoint (ms since UNIX epoch)
    pub checkpoint_ms: u128,
    /// Capability version
    pub version: u32,
}

impl CapabilityCheckpoint {
    pub fn new(capability_id: CapabilityId, instance_id: String) -> Self {
        Self {
            capability_id,
            last_processed_instance_id: instance_id,
            last_processed_sequence: 0,
            lifecycle_state: CapabilityLifecycleState::Registered,
            checkpoint_ms: crate::now_ms(),
            version: 1,
        }
    }

    /// Update checkpoint with new processed position
    pub fn update_position(&mut self, instance_id: String, sequence: u64) {
        self.last_processed_instance_id = instance_id;
        self.last_processed_sequence = sequence;
        self.checkpoint_ms = crate::now_ms();
    }

    /// Set lifecycle state
    pub fn set_state(&mut self, state: CapabilityLifecycleState) {
        self.lifecycle_state = state;
        self.checkpoint_ms = crate::now_ms();
    }
}

/// Result of processing an operation through a capability
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityOperationResult {
    /// Whether the operation was successfully processed
    pub success: bool,
    /// Optional error message
    pub error: Option<String>,
    /// Optional result data
    pub result: Option<Value>,
    /// Whether this capability produced derived state
    pub produced_derived_state: bool,
}

impl CapabilityOperationResult {
    pub fn success() -> Self {
        Self {
            success: true,
            error: None,
            result: None,
            produced_derived_state: false,
        }
    }

    pub fn with_derived_state(result: Value) -> Self {
        Self {
            success: true,
            error: None,
            result: Some(result),
            produced_derived_state: true,
        }
    }

    pub fn error(msg: String) -> Self {
        Self {
            success: false,
            error: Some(msg),
            result: None,
            produced_derived_state: false,
        }
    }
}

/// Result of a capability query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityQueryResult {
    /// Whether the query succeeded
    pub success: bool,
    /// Query results
    pub results: Vec<Value>,
    /// Optional error message
    pub error: Option<String>,
}

impl CapabilityQueryResult {
    pub fn success(results: Vec<Value>) -> Self {
        Self {
            success: true,
            results,
            error: None,
        }
    }

    pub fn empty() -> Self {
        Self {
            success: true,
            results: Vec::new(),
            error: None,
        }
    }

    pub fn error(msg: String) -> Self {
        Self {
            success: false,
            results: Vec::new(),
            error: Some(msg),
        }
    }
}

/// Access model for capability security
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityAccessModel {
    /// Collections this capability can read from
    pub reads: Vec<String>,
    /// Collections this capability can write to
    pub writes: Vec<String>,
    /// External systems this capability can execute
    pub executes: Vec<String>,
}

impl CapabilityAccessModel {
    pub fn new(reads: Vec<String>, writes: Vec<String>, executes: Vec<String>) -> Self {
        Self {
            reads,
            writes,
            executes,
        }
    }

    /// Check if capability can read from collection
    pub fn can_read(&self, collection: &str) -> bool {
        self.reads.contains(&collection.to_string()) || self.reads.iter().any(|r| r == "*")
    }

    /// Check if capability can write to collection
    pub fn can_write(&self, collection: &str) -> bool {
        self.writes.contains(&collection.to_string()) || self.writes.iter().any(|w| w == "*")
    }

    /// Check if capability can execute external system
    pub fn can_execute(&self, system: &str) -> bool {
        self.executes.contains(&system.to_string()) || self.executes.iter().any(|e| e == "*")
    }
}

/// Capability metadata and configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityMetadata {
    pub id: CapabilityId,
    pub name: String,
    pub description: String,
    pub role: CapabilityRole,
    pub access_model: CapabilityAccessModel,
    pub version: u32,
    pub created_ms: u128,
}

impl CapabilityMetadata {
    pub fn new(
        id: CapabilityId,
        name: String,
        description: String,
        role: CapabilityRole,
        access_model: CapabilityAccessModel,
    ) -> Self {
        Self {
            id,
            name,
            description,
            role,
            access_model,
            version: 1,
            created_ms: crate::now_ms(),
        }
    }
}

/// Capability registry - tracks all capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRegistry {
    /// All capabilities by ID
    #[serde(default)]
    capabilities: HashMap<CapabilityId, CapabilityMetadata>,
    /// Checkpoints by capability ID
    #[serde(default)]
    checkpoints: HashMap<CapabilityId, CapabilityCheckpoint>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self {
            capabilities: HashMap::new(),
            checkpoints: HashMap::new(),
        }
    }

    /// Register a new capability
    pub fn register(&mut self, metadata: CapabilityMetadata) -> crate::Result<()> {
        let id = metadata.id.clone();
        if self.capabilities.contains_key(&id) {
            return Err(crate::FlowError::CapabilityError(
                "Capability already registered".to_string(),
            ));
        }
        let checkpoint = CapabilityCheckpoint::new(id.clone(), String::new());
        self.capabilities.insert(id.clone(), metadata);
        self.checkpoints.insert(id, checkpoint);
        Ok(())
    }

    /// Get capability metadata
    pub fn get(&self, id: &CapabilityId) -> Option<&CapabilityMetadata> {
        self.capabilities.get(id)
    }

    /// Get capability checkpoint
    pub fn get_checkpoint(&self, id: &CapabilityId) -> Option<&CapabilityCheckpoint> {
        self.checkpoints.get(id)
    }

    /// Update capability checkpoint
    pub fn update_checkpoint(&mut self, checkpoint: CapabilityCheckpoint) {
        self.checkpoints
            .insert(checkpoint.capability_id.clone(), checkpoint);
    }

    /// List all capabilities
    pub fn list(&self) -> Vec<&CapabilityMetadata> {
        self.capabilities.values().collect()
    }

    /// Remove a capability
    pub fn remove(&mut self, id: &CapabilityId) -> Option<CapabilityMetadata> {
        self.checkpoints.remove(id);
        self.capabilities.remove(id)
    }
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait for implementing capabilities
/// This is the core contract for extending FeltDB with specialized behavior
pub trait FlowCapability: Send + Sync {
    /// Get the capability ID
    fn id(&self) -> CapabilityId;

    /// Get the capability metadata
    fn metadata(&self) -> CapabilityMetadata;

    /// Initialize the capability (called on registration)
    fn initialize(&mut self, context: &CapabilityContext) -> crate::Result<()>;

    /// Process an operation - maintain derived state
    fn on_operation(
        &mut self,
        operation: &Operation,
        context: &CapabilityContext,
    ) -> crate::Result<CapabilityOperationResult>;

    /// Query the capability's derived state
    fn query(
        &self,
        query: &CapabilityQuery,
        context: &CapabilityContext,
    ) -> crate::Result<CapabilityQueryResult>;

    /// Rebuild the capability's derived state from scratch
    fn rebuild(
        &mut self,
        operations: &[Operation],
        context: &CapabilityContext,
    ) -> crate::Result<()>;

    /// Pause the capability
    fn pause(&mut self) -> crate::Result<()> {
        Ok(())
    }

    /// Resume the capability
    fn resume(&mut self) -> crate::Result<()> {
        Ok(())
    }

    /// Check if the capability is ready (fully caught up)
    fn is_ready(&self) -> bool {
        true
    }
}

/// Reference to a capability with namespace and version
/// Canonical form: flow://capability/namespace:name@version
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CapabilityRef {
    /// Namespace (e.g., "docs", "images")
    pub namespace: String,
    /// Capability name (e.g., "vector-search", "image-analysis")
    pub capability: String,
    /// Capability version
    pub version: u32,
}

impl CapabilityRef {
    /// Create a new capability reference
    pub fn new(namespace: String, capability: String, version: u32) -> Self {
        Self {
            namespace,
            capability,
            version,
        }
    }

    /// Get canonical form for serialization
    pub fn canonical(&self) -> String {
        format!(
            "flow://capability/{}/{}@{}",
            self.namespace, self.capability, self.version
        )
    }

    /// Parse from canonical form
    pub fn from_canonical(canonical: &str) -> crate::Result<Self> {
        if !canonical.starts_with("flow://capability/") {
            return Err(crate::FlowError::CapabilityError(
                "Invalid capability reference format".to_string(),
            ));
        }

        let rest = &canonical[18..]; // Skip "flow://capability/"
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() != 2 {
            return Err(crate::FlowError::CapabilityError(
                "Invalid capability reference format".to_string(),
            ));
        }

        let namespace = parts[0].to_string();
        let cap_parts: Vec<&str> = parts[1].split('@').collect();
        if cap_parts.len() != 2 {
            return Err(crate::FlowError::CapabilityError(
                "Invalid capability reference format".to_string(),
            ));
        }

        let capability = cap_parts[0].to_string();
        let version = cap_parts[1]
            .parse::<u32>()
            .map_err(|_| crate::FlowError::CapabilityError("Invalid version number".to_string()))?;

        Ok(Self::new(namespace, capability, version))
    }
}

impl fmt::Display for CapabilityRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.canonical())
    }
}

/// Represents a required state for a capability to execute
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateRequirement {
    /// Namespace containing the required state
    pub namespace: String,
    /// Minimum version required (0 means any)
    pub min_version: u64,
}

impl StateRequirement {
    pub fn new(namespace: String, min_version: u64) -> Self {
        Self {
            namespace,
            min_version,
        }
    }
}

/// Requirements for a capability to execute
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRequirements {
    /// Required namespaces and minimum versions
    pub required_state: Vec<StateRequirement>,
    /// Maximum acceptable latency for state (ms)
    pub max_state_latency_ms: Option<u64>,
    /// Whether execution requires single-owner semantics
    pub requires_ownership: bool,
}

impl CapabilityRequirements {
    pub fn new() -> Self {
        Self {
            required_state: Vec::new(),
            max_state_latency_ms: None,
            requires_ownership: false,
        }
    }

    pub fn with_required_state(mut self, requirement: StateRequirement) -> Self {
        self.required_state.push(requirement);
        self
    }

    pub fn with_max_latency(mut self, latency_ms: u64) -> Self {
        self.max_state_latency_ms = Some(latency_ms);
        self
    }

    pub fn with_ownership_requirement(mut self, requires: bool) -> Self {
        self.requires_ownership = requires;
        self
    }
}

impl Default for CapabilityRequirements {
    fn default() -> Self {
        Self::new()
    }
}

/// Advertises a capability at a specific peer with its requirements
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedCapabilityAdvertisement {
    /// The capability reference
    pub capability_ref: CapabilityRef,
    /// Peer ID providing this capability
    pub peer_id: String,
    /// Requirements for this capability
    pub requirements: CapabilityRequirements,
    /// Timestamp of advertisement
    pub advertised_ms: u128,
}

impl DistributedCapabilityAdvertisement {
    pub fn new(capability_ref: CapabilityRef, peer_id: String) -> Self {
        Self {
            capability_ref,
            peer_id,
            requirements: CapabilityRequirements::new(),
            advertised_ms: crate::now_ms(),
        }
    }

    pub fn with_requirements(mut self, requirements: CapabilityRequirements) -> Self {
        self.requirements = requirements;
        self
    }
}

/// Context for executing a capability (local or remote)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityExecutionContext {
    /// Unique execution ID
    pub execution_id: String,
    /// The capability being executed
    pub capability_ref: CapabilityRef,
    /// References to input data (instead of serialized payloads)
    pub input_refs: Vec<String>,
    /// Required state version for execution
    pub required_state_version: u64,
    /// Requester instance/peer ID
    pub requester: String,
    /// Timestamp when execution was requested
    pub requested_ms: u128,
}

impl CapabilityExecutionContext {
    pub fn new(execution_id: String, capability_ref: CapabilityRef, requester: String) -> Self {
        Self {
            execution_id,
            capability_ref,
            input_refs: Vec::new(),
            required_state_version: 0,
            requester,
            requested_ms: crate::now_ms(),
        }
    }

    pub fn with_input_ref(mut self, input_ref: String) -> Self {
        self.input_refs.push(input_ref);
        self
    }

    pub fn with_required_state_version(mut self, version: u64) -> Self {
        self.required_state_version = version;
        self
    }
}

/// Reference to an execution result with provenance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRef {
    /// Unique execution ID
    pub execution_id: String,
    /// The capability that was executed
    pub capability_ref: CapabilityRef,
    /// Peer that executed the capability
    pub provider_peer: String,
    /// Input references used
    pub input_refs: Vec<String>,
    /// State version at time of execution
    pub input_state_version: u64,
    /// Content identity of the result
    pub result_content_identity: String,
    /// Operation that recorded this result
    pub created_operation: Option<String>,
}

impl ExecutionRef {
    pub fn new(execution_id: String, capability_ref: CapabilityRef, provider_peer: String) -> Self {
        Self {
            execution_id,
            capability_ref,
            provider_peer,
            input_refs: Vec::new(),
            input_state_version: 0,
            result_content_identity: String::new(),
            created_operation: None,
        }
    }
}

/// Result of a capability execution with full provenance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityExecutionResult {
    /// Reference to the execution
    pub execution_ref: ExecutionRef,
    /// The actual result data
    pub result: Value,
    /// Timestamp when execution completed
    pub completed_ms: u128,
    /// Any error that occurred
    pub error: Option<String>,
}

impl CapabilityExecutionResult {
    pub fn success(execution_ref: ExecutionRef, result: Value) -> Self {
        Self {
            execution_ref,
            result,
            completed_ms: crate::now_ms(),
            error: None,
        }
    }

    pub fn error(execution_ref: ExecutionRef, error: String) -> Self {
        Self {
            execution_ref,
            result: Value::Null,
            completed_ms: crate::now_ms(),
            error: Some(error),
        }
    }
}

/// Resolver for capabilities - routes to appropriate provider
pub struct CapabilityResolver {
    /// Registered capabilities by ref
    capabilities: HashMap<String, DistributedCapabilityAdvertisement>,
}

impl CapabilityResolver {
    pub fn new() -> Self {
        Self {
            capabilities: HashMap::new(),
        }
    }

    /// Register a capability advertisement
    pub fn register(&mut self, ad: DistributedCapabilityAdvertisement) {
        self.capabilities.insert(ad.capability_ref.canonical(), ad);
    }

    /// Find a provider for a capability
    pub fn resolve(&self, cap_ref: &CapabilityRef) -> Option<&DistributedCapabilityAdvertisement> {
        self.capabilities.get(&cap_ref.canonical())
    }

    /// Find all providers for a capability (by name only)
    pub fn resolve_all(
        &self,
        namespace: &str,
        capability: &str,
    ) -> Vec<&DistributedCapabilityAdvertisement> {
        self.capabilities
            .values()
            .filter(|ad| {
                ad.capability_ref.namespace == namespace
                    && ad.capability_ref.capability == capability
            })
            .collect()
    }
}

impl Default for CapabilityResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_id_creation() {
        let id = CapabilityId::new("search".to_string());
        assert_eq!(id.0, "search");
    }

    #[test]
    fn test_capability_checkpoint() {
        let mut checkpoint = CapabilityCheckpoint::new(
            CapabilityId::new("search".to_string()),
            "instance-1".to_string(),
        );
        assert_eq!(checkpoint.last_processed_sequence, 0);

        checkpoint.update_position("instance-1".to_string(), 100);
        assert_eq!(checkpoint.last_processed_sequence, 100);
    }

    #[test]
    fn test_capability_access_model() {
        let access = CapabilityAccessModel::new(
            vec!["docs".to_string()],
            vec!["docs.search".to_string()],
            vec!["embedding-api".to_string()],
        );

        assert!(access.can_read("docs"));
        assert!(!access.can_read("users"));
        assert!(access.can_write("docs.search"));
        assert!(access.can_execute("embedding-api"));
    }

    #[test]
    fn test_capability_access_wildcard() {
        let access =
            CapabilityAccessModel::new(vec!["*".to_string()], vec!["*".to_string()], vec![]);

        assert!(access.can_read("anything"));
        assert!(access.can_write("anything"));
    }

    #[test]
    fn test_capability_registry() {
        let mut registry = CapabilityRegistry::new();
        let metadata = CapabilityMetadata::new(
            CapabilityId::new("search".to_string()),
            "Search Capability".to_string(),
            "Full-text search".to_string(),
            CapabilityRole::Projection,
            CapabilityAccessModel::new(
                vec!["docs".to_string()],
                vec!["docs.search".to_string()],
                vec![],
            ),
        );

        registry.register(metadata).unwrap();
        let cap_id = CapabilityId::new("search".to_string());
        assert!(registry.get(&cap_id).is_some());
        assert!(registry.get_checkpoint(&cap_id).is_some());
    }

    #[test]
    fn test_capability_ref_creation_and_canonical() {
        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        assert_eq!(cap_ref.namespace, "docs");
        assert_eq!(cap_ref.capability, "vector-search");
        assert_eq!(cap_ref.version, 1);
        assert_eq!(
            cap_ref.canonical(),
            "flow://capability/docs/vector-search@1"
        );
    }

    #[test]
    fn test_capability_ref_parse_canonical() {
        let canonical = "flow://capability/docs/vector-search@1";
        let cap_ref = CapabilityRef::from_canonical(canonical).unwrap();
        assert_eq!(cap_ref.namespace, "docs");
        assert_eq!(cap_ref.capability, "vector-search");
        assert_eq!(cap_ref.version, 1);
    }

    #[test]
    fn test_capability_ref_display() {
        let cap_ref = CapabilityRef::new("images".to_string(), "image-analysis".to_string(), 2);
        assert_eq!(
            cap_ref.to_string(),
            "flow://capability/images/image-analysis@2"
        );
    }

    #[test]
    fn test_state_requirement_creation() {
        let req = StateRequirement::new("docs".to_string(), 847);
        assert_eq!(req.namespace, "docs");
        assert_eq!(req.min_version, 847);
    }

    #[test]
    fn test_capability_requirements_builder() {
        let reqs = CapabilityRequirements::new()
            .with_required_state(StateRequirement::new("docs".to_string(), 847))
            .with_max_latency(5000)
            .with_ownership_requirement(true);

        assert_eq!(reqs.required_state.len(), 1);
        assert_eq!(reqs.max_state_latency_ms, Some(5000));
        assert!(reqs.requires_ownership);
    }

    #[test]
    fn test_distributed_capability_advertisement() {
        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let ad = DistributedCapabilityAdvertisement::new(cap_ref.clone(), "peer-b".to_string())
            .with_requirements(
                CapabilityRequirements::new()
                    .with_required_state(StateRequirement::new("docs".to_string(), 847)),
            );

        assert_eq!(ad.capability_ref, cap_ref);
        assert_eq!(ad.peer_id, "peer-b");
        assert_eq!(ad.requirements.required_state.len(), 1);
    }

    #[test]
    fn test_capability_execution_context() {
        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let ctx = CapabilityExecutionContext::new(
            "exec-123".to_string(),
            cap_ref.clone(),
            "requester".to_string(),
        )
        .with_input_ref("flow://docs#123".to_string())
        .with_required_state_version(851);

        assert_eq!(ctx.execution_id, "exec-123");
        assert_eq!(ctx.capability_ref, cap_ref);
        assert_eq!(ctx.input_refs.len(), 1);
        assert_eq!(ctx.required_state_version, 851);
    }

    #[test]
    fn test_execution_ref_creation() {
        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let exec_ref = ExecutionRef::new(
            "exec-123".to_string(),
            cap_ref.clone(),
            "peer-b".to_string(),
        );

        assert_eq!(exec_ref.execution_id, "exec-123");
        assert_eq!(exec_ref.capability_ref, cap_ref);
        assert_eq!(exec_ref.provider_peer, "peer-b");
    }

    #[test]
    fn test_capability_execution_result_success() {
        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let exec_ref = ExecutionRef::new("exec-123".to_string(), cap_ref, "peer-b".to_string());
        let result = serde_json::json!({"results": [1, 2, 3]});

        let exec_result = CapabilityExecutionResult::success(exec_ref.clone(), result.clone());
        assert!(exec_result.error.is_none());
        assert_eq!(exec_result.result, result);
    }

    #[test]
    fn test_capability_resolver_registration() {
        let mut resolver = CapabilityResolver::new();
        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let ad = DistributedCapabilityAdvertisement::new(cap_ref.clone(), "peer-b".to_string());

        resolver.register(ad);

        let resolved = resolver.resolve(&cap_ref);
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().peer_id, "peer-b");
    }

    #[test]
    fn test_capability_resolver_resolve_all() {
        let mut resolver = CapabilityResolver::new();

        let cap_ref_v1 = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let ad1 = DistributedCapabilityAdvertisement::new(cap_ref_v1, "peer-a".to_string());
        resolver.register(ad1);

        let cap_ref_v2 = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 2);
        let ad2 = DistributedCapabilityAdvertisement::new(cap_ref_v2, "peer-b".to_string());
        resolver.register(ad2);

        let all = resolver.resolve_all("docs", "vector-search");
        assert_eq!(all.len(), 2);
    }
}
