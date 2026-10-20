/// References module for distributed content fabric
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};

/// A stable reference primitive for distributed FeltDB fabric
///
/// FlowRef separates logical identity from physical location.
/// The same reference can be resolved on different peers.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct FlowRef {
    /// Namespace containing the object (e.g., "docs", "orders", "embeddings")
    pub namespace: String,
    /// Object identifier within the namespace
    pub object_id: String,
    /// Version of the object (for content addressing)
    pub version: Option<u64>,
}

impl FlowRef {
    /// Create a new FlowRef with namespace and object_id
    pub fn new(namespace: impl Into<String>, object_id: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            object_id: object_id.into(),
            version: None,
        }
    }

    /// Create a FlowRef with explicit version
    pub fn with_version(
        namespace: impl Into<String>,
        object_id: impl Into<String>,
        version: u64,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            object_id: object_id.into(),
            version: Some(version),
        }
    }

    /// Get a canonical string representation
    pub fn canonical(&self) -> String {
        match self.version {
            Some(v) => format!("flow://{}#{}@{}", self.namespace, self.object_id, v),
            None => format!("flow://{}#{}", self.namespace, self.object_id),
        }
    }

    /// Parse from canonical form
    pub fn from_canonical(s: &str) -> Option<Self> {
        if !s.starts_with("flow://") {
            return None;
        }
        let s = &s[7..]; // Remove "flow://"

        let (namespace, rest) = s.split_once('#')?;

        if let Some((object_id, version_str)) = rest.split_once('@') {
            let version = version_str.parse().ok()?;
            Some(Self {
                namespace: namespace.to_string(),
                object_id: object_id.to_string(),
                version: Some(version),
            })
        } else {
            Some(Self {
                namespace: namespace.to_string(),
                object_id: rest.to_string(),
                version: None,
            })
        }
    }
}

impl Display for FlowRef {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.canonical())
    }
}

impl Hash for FlowRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.namespace.hash(state);
        self.object_id.hash(state);
        self.version.hash(state);
    }
}

/// Content identity based on hash
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct ContentIdentity {
    /// SHA-256 hash of the content
    pub hash: String,
}

impl ContentIdentity {
    /// Create a new ContentIdentity from a hash string
    pub fn new(hash: impl Into<String>) -> Self {
        Self { hash: hash.into() }
    }

    /// Compute content hash from serialized data
    pub fn from_bytes(data: &[u8]) -> Self {
        let hash = sha256(data);
        Self { hash }
    }

    /// Get the hash as a hex string
    pub fn as_str(&self) -> &str {
        &self.hash
    }
}

impl Display for ContentIdentity {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "sha256:{}", self.hash)
    }
}

/// Compute SHA-256 hash of the data
fn sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    format!("{:x}", result)
}

/// Peer identifier in the distributed fabric
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct PeerId {
    /// Unique identifier for a peer
    pub id: String,
}

impl PeerId {
    /// Create a new PeerId
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

impl Display for PeerId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.id)
    }
}

/// Advertisement of what a peer provides
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerAdvertisement {
    /// The peer being advertised
    pub peer_id: PeerId,
    /// Namespaces this peer provides
    pub namespaces: Vec<String>,
    /// Content ranges available (can be extended to include ranges)
    pub content_ranges: Vec<ContentRange>,
    /// Capabilities this peer provides (e.g., "search:docs", "vector:embeddings")
    pub capabilities: Vec<String>,
    /// Timestamp when this advertisement was created
    pub advertised_at_ms: u128,
}

impl PeerAdvertisement {
    /// Create a new peer advertisement
    pub fn new(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            namespaces: Vec::new(),
            content_ranges: Vec::new(),
            capabilities: Vec::new(),
            advertised_at_ms: crate::now_ms(),
        }
    }

    /// Add a namespace this peer provides
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespaces.push(namespace.into());
        self
    }

    /// Add multiple namespaces
    pub fn with_namespaces(mut self, namespaces: Vec<String>) -> Self {
        self.namespaces.extend(namespaces);
        self
    }

    /// Add a capability this peer provides
    pub fn with_capability(mut self, capability: impl Into<String>) -> Self {
        self.capabilities.push(capability.into());
        self
    }

    /// Add multiple capabilities
    pub fn with_capabilities(mut self, capabilities: Vec<String>) -> Self {
        self.capabilities.extend(capabilities);
        self
    }

    /// Add a content range
    pub fn with_range(mut self, range: ContentRange) -> Self {
        self.content_ranges.push(range);
        self
    }

    /// Check if this peer provides a specific namespace
    pub fn provides_namespace(&self, namespace: &str) -> bool {
        self.namespaces.iter().any(|ns| ns == namespace)
    }

    /// Check if this peer provides a specific capability
    pub fn provides_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|cap| cap == capability)
    }
}

/// A range of content available on a peer
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ContentRange {
    /// Start of the range (inclusive)
    pub start: String,
    /// End of the range (inclusive)
    pub end: String,
    /// Number of items in this range
    pub count: u64,
}

impl ContentRange {
    /// Create a new content range
    pub fn new(start: impl Into<String>, end: impl Into<String>, count: u64) -> Self {
        Self {
            start: start.into(),
            end: end.into(),
            count,
        }
    }

    /// Check if an object_id is in this range
    pub fn contains(&self, object_id: &str) -> bool {
        object_id >= self.start.as_str() && object_id <= self.end.as_str()
    }
}

/// Trait for resolving FlowRefs to actual objects
pub trait ReferenceResolver: Send + Sync {
    /// Resolve a reference to its content
    /// Returns the serialized content if found
    fn resolve(&self, flow_ref: &FlowRef) -> Result<Option<Vec<u8>>, String>;

    /// Check if a reference can be resolved
    fn can_resolve(&self, flow_ref: &FlowRef) -> bool;

    /// List all references in a namespace
    fn list_namespace(&self, namespace: &str) -> Result<Vec<FlowRef>, String>;
}

/// Metadata about a resolved reference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionMetadata {
    /// Whether the content was found locally or acquired remotely
    pub is_local: bool,
    /// If acquired remotely, which peer provided it
    pub source_peer: Option<PeerId>,
    /// Timestamp when content was resolved/acquired (ms since UNIX epoch)
    pub resolved_at_ms: u128,
}

impl ResolutionMetadata {
    /// Create metadata for locally resolved content
    pub fn local() -> Self {
        Self {
            is_local: true,
            source_peer: None,
            resolved_at_ms: crate::now_ms(),
        }
    }

    /// Create metadata for remotely acquired content
    pub fn remote(source_peer: PeerId) -> Self {
        Self {
            is_local: false,
            source_peer: Some(source_peer),
            resolved_at_ms: crate::now_ms(),
        }
    }
}

/// Result of content resolution with metadata
#[derive(Debug, Clone)]
pub struct ResolutionResult {
    /// The actual content
    pub content: Vec<u8>,
    /// Metadata about how it was resolved
    pub metadata: ResolutionMetadata,
}

impl ResolutionResult {
    /// Create a resolution result for local content
    pub fn local(content: Vec<u8>) -> Self {
        Self {
            content,
            metadata: ResolutionMetadata::local(),
        }
    }

    /// Create a resolution result for remotely acquired content
    pub fn remote(content: Vec<u8>, source_peer: PeerId) -> Self {
        Self {
            content,
            metadata: ResolutionMetadata::remote(source_peer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flow_ref_canonical() {
        let r = FlowRef::new("docs", "123");
        assert_eq!(r.canonical(), "flow://docs#123");

        let r = FlowRef::with_version("docs", "123", 42);
        assert_eq!(r.canonical(), "flow://docs#123@42");
    }

    #[test]
    fn test_flow_ref_parse() {
        let s = "flow://docs#123";
        let r = FlowRef::from_canonical(s).unwrap();
        assert_eq!(r.namespace, "docs");
        assert_eq!(r.object_id, "123");
        assert_eq!(r.version, None);

        let s = "flow://docs#123@42";
        let r = FlowRef::from_canonical(s).unwrap();
        assert_eq!(r.namespace, "docs");
        assert_eq!(r.object_id, "123");
        assert_eq!(r.version, Some(42));
    }

    #[test]
    fn test_content_range_contains() {
        let range = ContentRange::new("a", "z", 26);
        assert!(range.contains("m"));
        assert!(range.contains("a"));
        assert!(range.contains("z"));
        assert!(!range.contains("0"));
    }

    #[test]
    fn test_peer_advertisement() {
        let peer = PeerId::new("peer-a");
        let ad = PeerAdvertisement::new(peer.clone())
            .with_namespace("docs")
            .with_namespace("orders")
            .with_capability("search:docs");

        assert!(ad.provides_namespace("docs"));
        assert!(ad.provides_capability("search:docs"));
        assert!(!ad.provides_namespace("embeddings"));
    }

    #[test]
    fn test_resolution_metadata_local() {
        let metadata = ResolutionMetadata::local();
        assert!(metadata.is_local);
        assert!(metadata.source_peer.is_none());
        assert!(metadata.resolved_at_ms > 0);
    }

    #[test]
    fn test_resolution_metadata_remote() {
        let peer = PeerId::new("peer-b");
        let metadata = ResolutionMetadata::remote(peer.clone());
        assert!(!metadata.is_local);
        assert_eq!(metadata.source_peer, Some(peer));
        assert!(metadata.resolved_at_ms > 0);
    }

    #[test]
    fn test_resolution_result_creation() {
        let content = b"test content".to_vec();
        let result = ResolutionResult::local(content.clone());

        assert_eq!(result.content, content);
        assert!(result.metadata.is_local);
        assert!(result.metadata.source_peer.is_none());
    }

    #[test]
    fn test_resolution_result_remote() {
        let content = b"acquired content".to_vec();
        let peer = PeerId::new("provider-peer");
        let result = ResolutionResult::remote(content.clone(), peer.clone());

        assert_eq!(result.content, content);
        assert!(!result.metadata.is_local);
        assert_eq!(result.metadata.source_peer, Some(peer));
    }
}
