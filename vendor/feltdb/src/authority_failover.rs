//! Authority Failover Protocol
//!
//! This module implements automatic authority election, failover, and recovery
//! for distributed FeltDB clusters. The design follows these principles:
//!
//! 1. **Single Authority**: At any time, exactly one node holds authority.
//!    Operations flow through the authority, which coordinates replication.
//!
//! 2. **Fencing**: A stale authority cannot accept writes after a new authority
//!    is elected. Fencing tokens prevent split-brain scenarios.
//!
//! 3. **Automatic Failover**: When the authority fails, surviving nodes detect
//!    the failure and elect a new authority without application intervention.
//!
//! 4. **Recovery**: A previously-failed authority can rejoin as a replica and
//!    catch up to the current state.
//!
//! ## Consistency Contract
//!
//! - A write is **committed** when the authority has persisted it durably
//!   and at least one other active replica has acknowledged receipt.
//! - A committed write survives any single node failure.
//! - During failover, a brief period of unavailability occurs while
//!   the new authority is elected.
//! - The protocol prevents two nodes from believing they are authority
//!   simultaneously through monotonic fencing tokens.
//!
//! ## Failover Sequence
//!
//! ```text
//! Authority A fails
//!      │
//!      ▼
//! Replicas detect failure (heartbeat timeout)
//!      │
//!      ▼
//! Election: highest-priority surviving replica
//!      │
//!      ▼
//! New authority B claims with fencing token
//!      │
//!      ▼
//! B broadcasts authority change
//!      │
//!      ▼
//! Replicas acknowledge new authority
//!      │
//!      ▼
//! B begins accepting writes
//!      │
//!      ▼
//! Old authority A rejoins as replica
//! ```

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Authority role of a node in the cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuthorityRole {
    /// This node is the authority and accepts writes.
    Authority,
    /// This node is a replica and forwards writes to the authority.
    Replica,
    /// This node is a candidate seeking to become authority.
    Candidate,
    /// This node has been fenced (stale authority).
    Fenced,
}

/// A fencing token that prevents stale authorities from operating.
///
/// Monotonically increasing: a higher token always supersedes a lower one.
/// The fencing token is durably stored and survives restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FencingToken(pub u64);

impl FencingToken {
    pub fn initial() -> Self {
        Self(1)
    }

    pub fn next(&self) -> Self {
        Self(self.0.saturating_add(1))
    }

    pub fn supersedes(&self, other: &FencingToken) -> bool {
        self.0 > other.0
    }
}

impl std::fmt::Display for FencingToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FT:{}", self.0)
    }
}

/// Record of an authority election.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthorityRecord {
    /// The node ID of the authority.
    pub node_id: String,
    /// The fencing token for this authority term.
    pub fencing_token: FencingToken,
    /// When this authority was elected.
    pub elected_at_ms: u64,
    /// The previous authority (if any).
    pub previous_authority: Option<String>,
    /// Nodes that acknowledged this authority.
    pub acknowledgements: HashSet<String>,
}

impl AuthorityRecord {
    pub fn new(node_id: String, fencing_token: FencingToken, previous: Option<String>) -> Self {
        Self {
            node_id,
            fencing_token,
            elected_at_ms: now_ms(),
            previous_authority: previous,
            acknowledgements: HashSet::new(),
        }
    }
}

/// Heartbeat tracking for failure detection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeartbeatState {
    /// Last heartbeat received from each peer.
    pub last_seen: BTreeMap<String, u64>,
    /// Heartbeat interval in milliseconds.
    pub interval_ms: u64,
    /// How many missed heartbeats before a node is considered failed.
    pub failure_threshold: u32,
}

impl HeartbeatState {
    pub fn new(interval_ms: u64, failure_threshold: u32) -> Self {
        Self {
            last_seen: BTreeMap::new(),
            interval_ms,
            failure_threshold,
        }
    }

    /// Record a heartbeat from a peer.
    pub fn record_heartbeat(&mut self, node_id: &str) {
        self.last_seen.insert(node_id.to_string(), now_ms());
    }

    /// Check if a node appears to have failed.
    pub fn is_failed(&self, node_id: &str) -> bool {
        let Some(last) = self.last_seen.get(node_id) else {
            // Never seen = not yet known, not necessarily failed
            return false;
        };
        let deadline = self.interval_ms * self.failure_threshold as u64;
        now_ms().saturating_sub(*last) > deadline
    }

    /// Get all nodes that appear to have failed.
    pub fn failed_nodes(&self) -> Vec<String> {
        self.last_seen
            .keys()
            .filter(|node_id| self.is_failed(node_id))
            .cloned()
            .collect()
    }
}

/// Election priority for a node.
/// Higher priority wins the election when multiple candidates exist.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ElectionPriority {
    /// Operations applied (higher is better - more up-to-date).
    pub operations_applied: u64,
    /// Static priority assigned to the node (for tie-breaking).
    pub static_priority: u32,
    /// Node ID (for deterministic tie-breaking).
    pub node_id: String,
}

impl ElectionPriority {
    pub fn new(operations_applied: u64, static_priority: u32, node_id: String) -> Self {
        Self {
            operations_applied,
            static_priority,
            node_id,
        }
    }
}

/// The result of an authority claim attempt.
#[derive(Clone, Debug)]
pub enum ClaimResult {
    /// Successfully claimed authority with this fencing token.
    Granted { token: FencingToken },
    /// Another node already has a higher fencing token.
    Rejected {
        current_authority: String,
        current_token: FencingToken,
    },
    /// This node is fenced and cannot claim authority.
    Fenced { token: FencingToken },
}

/// Authority state that is durably persisted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthorityState {
    /// The cluster ID this authority state belongs to.
    pub cluster_id: String,
    /// Current authority record, if any.
    pub current_authority: Option<AuthorityRecord>,
    /// This node's role.
    pub local_role: AuthorityRole,
    /// This node's ID.
    pub local_node_id: String,
    /// The highest fencing token this node has seen.
    pub highest_seen_token: FencingToken,
    /// Election priority for this node.
    pub local_priority: u32,
}

impl AuthorityState {
    pub fn new(cluster_id: String, local_node_id: String, local_priority: u32) -> Self {
        Self {
            cluster_id,
            current_authority: None,
            local_role: AuthorityRole::Replica,
            local_node_id,
            highest_seen_token: FencingToken::initial(),
            local_priority,
        }
    }

    /// Check if this node is currently the authority.
    pub fn is_authority(&self) -> bool {
        self.local_role == AuthorityRole::Authority
    }

    /// Check if this node is fenced.
    pub fn is_fenced(&self) -> bool {
        self.local_role == AuthorityRole::Fenced
    }

    /// Get the current authority node ID.
    pub fn authority_node(&self) -> Option<&str> {
        self.current_authority.as_ref().map(|r| r.node_id.as_str())
    }

    /// Get the current fencing token.
    pub fn current_token(&self) -> FencingToken {
        self.current_authority
            .as_ref()
            .map(|r| r.fencing_token)
            .unwrap_or(FencingToken::initial())
    }
}

/// Persistent authority store.
pub struct AuthorityStore {
    path: PathBuf,
    state: AuthorityState,
    heartbeats: HeartbeatState,
}

#[derive(Debug)]
pub enum AuthorityError {
    NoCluster,
    AlreadyAuthority(String),
    NotAuthority,
    Fenced(FencingToken),
    StaleToken {
        provided: FencingToken,
        current: FencingToken,
    },
    QuorumUnavailable {
        live: usize,
        required: usize,
    },
    Io(String),
}

impl std::fmt::Display for AuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCluster => write!(f, "no cluster has been created"),
            Self::AlreadyAuthority(id) => write!(f, "node {id} is already authority"),
            Self::NotAuthority => write!(f, "this node is not the authority"),
            Self::Fenced(token) => write!(f, "this node is fenced at {token}"),
            Self::StaleToken { provided, current } => {
                write!(f, "stale token {provided}, current is {current}")
            }
            Self::QuorumUnavailable { live, required } => {
                write!(
                    f,
                    "authority quorum unavailable: {live} live, {required} required"
                )
            }
            Self::Io(msg) => write!(f, "authority store: {msg}"),
        }
    }
}

impl AuthorityStore {
    /// Open or create an authority store.
    pub fn open<P: AsRef<Path>>(
        path: P,
        cluster_id: &str,
        local_node_id: &str,
        local_priority: u32,
    ) -> Result<Self, AuthorityError> {
        Self::open_with_failure_detector(path, cluster_id, local_node_id, local_priority, 1_000, 5)
    }

    /// Open with an explicitly configured failure detector. Production keeps
    /// the conservative default above; process-level acceptance tests use a
    /// shorter interval so failure is event-driven without multi-second sleeps.
    pub fn open_with_failure_detector<P: AsRef<Path>>(
        path: P,
        cluster_id: &str,
        local_node_id: &str,
        local_priority: u32,
        heartbeat_interval_ms: u64,
        failure_threshold: u32,
    ) -> Result<Self, AuthorityError> {
        let path = path.as_ref().to_path_buf();
        let state = if path.exists() {
            let bytes = std::fs::read(&path).map_err(|e| AuthorityError::Io(e.to_string()))?;
            serde_json::from_slice(&bytes).map_err(|e| AuthorityError::Io(e.to_string()))?
        } else {
            AuthorityState::new(
                cluster_id.to_string(),
                local_node_id.to_string(),
                local_priority,
            )
        };
        Ok(Self {
            path,
            state,
            heartbeats: HeartbeatState::new(heartbeat_interval_ms, failure_threshold),
        })
    }

    /// Persist the authority state durably.
    fn persist(&self) -> Result<(), AuthorityError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AuthorityError::Io(e.to_string()))?;
        }
        let temp = self.path.with_extension("tmp");
        let bytes = serde_json::to_vec_pretty(&self.state)
            .map_err(|e| AuthorityError::Io(e.to_string()))?;
        {
            use std::io::Write;
            let mut file =
                std::fs::File::create(&temp).map_err(|e| AuthorityError::Io(e.to_string()))?;
            file.write_all(&bytes)
                .map_err(|e| AuthorityError::Io(e.to_string()))?;
            file.sync_all()
                .map_err(|e| AuthorityError::Io(e.to_string()))?;
        }
        std::fs::rename(&temp, &self.path).map_err(|e| AuthorityError::Io(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }

    /// Get the current authority state.
    pub fn state(&self) -> &AuthorityState {
        &self.state
    }

    /// Record a heartbeat from a peer.
    pub fn heartbeat(&mut self, node_id: &str) {
        self.heartbeats.record_heartbeat(node_id);
    }

    /// Check if the current authority has failed.
    pub fn authority_failed(&self) -> bool {
        if let Some(ref authority) = self.state.current_authority {
            if authority.node_id == self.state.local_node_id {
                return false; // We are authority, we haven't failed
            }
            return self.heartbeats.is_failed(&authority.node_id);
        }
        false
    }

    /// Get all nodes that appear to have failed.
    pub fn failed_nodes(&self) -> Vec<String> {
        self.heartbeats.failed_nodes()
    }

    /// Claim authority with a new fencing token.
    ///
    /// This node attempts to become the authority. If successful:
    /// - A new fencing token is assigned (one higher than any seen)
    /// - This node becomes authority
    /// - The previous authority (if any) is recorded
    ///
    /// If another node has a higher token, the claim is rejected.
    pub fn claim_authority(&mut self) -> Result<ClaimResult, AuthorityError> {
        if self.state.is_fenced() {
            return Ok(ClaimResult::Fenced {
                token: self.state.highest_seen_token,
            });
        }

        // If there's already an authority with a higher token, reject
        if let Some(ref current) = self.state.current_authority {
            if current.node_id != self.state.local_node_id
                && !self.heartbeats.is_failed(&current.node_id)
            {
                return Ok(ClaimResult::Rejected {
                    current_authority: current.node_id.clone(),
                    current_token: current.fencing_token,
                });
            }
        }

        // Claim with a new token
        let new_token = self.state.highest_seen_token.next();
        let previous = self
            .state
            .current_authority
            .as_ref()
            .map(|r| r.node_id.clone());

        let mut record =
            AuthorityRecord::new(self.state.local_node_id.clone(), new_token, previous);
        record
            .acknowledgements
            .insert(self.state.local_node_id.clone());

        self.state.current_authority = Some(record);
        self.state.local_role = AuthorityRole::Authority;
        self.state.highest_seen_token = new_token;

        self.persist()?;

        Ok(ClaimResult::Granted { token: new_token })
    }

    /// Accept a new authority announcement.
    ///
    /// If the fencing token is higher than any we've seen, accept the new
    /// authority and update our role accordingly.
    pub fn accept_authority(
        &mut self,
        authority_node: &str,
        token: FencingToken,
    ) -> Result<(), AuthorityError> {
        // Check if token is valid
        if !token.supersedes(&self.state.highest_seen_token)
            && token != self.state.highest_seen_token
        {
            return Err(AuthorityError::StaleToken {
                provided: token,
                current: self.state.highest_seen_token,
            });
        }

        // Update our state
        self.state.highest_seen_token = token;

        // If we were authority with a lower token, we are now fenced
        if self.state.local_role == AuthorityRole::Authority
            && authority_node != self.state.local_node_id
        {
            self.state.local_role = AuthorityRole::Fenced;
        } else if authority_node != self.state.local_node_id {
            self.state.local_role = AuthorityRole::Replica;
        }

        // Record the new authority
        self.state.current_authority = Some(AuthorityRecord::new(
            authority_node.to_string(),
            token,
            self.state
                .current_authority
                .as_ref()
                .map(|r| r.node_id.clone()),
        ));

        // Start tracking heartbeats from the new authority.
        // This allows failure detection to work even if we've never received
        // a heartbeat yet - the clock starts now.
        if authority_node != self.state.local_node_id {
            self.heartbeats.record_heartbeat(authority_node);
        }

        self.persist()
    }

    /// Acknowledge the current authority.
    pub fn acknowledge_authority(&mut self, from_node: &str) -> Result<(), AuthorityError> {
        if let Some(ref mut authority) = self.state.current_authority {
            authority.acknowledgements.insert(from_node.to_string());
            self.persist()?;
        }
        Ok(())
    }

    /// Validate a write operation against the current fencing token.
    ///
    /// Returns Ok if the token is valid (current or not provided for local writes).
    /// Returns Err if the token is stale.
    pub fn validate_write(&self, token: Option<FencingToken>) -> Result<(), AuthorityError> {
        if self.state.is_fenced() {
            return Err(AuthorityError::Fenced(self.state.highest_seen_token));
        }

        if !self.state.is_authority() {
            return Err(AuthorityError::NotAuthority);
        }

        if let Some(provided) = token {
            if provided != self.state.current_token() {
                return Err(AuthorityError::StaleToken {
                    provided,
                    current: self.state.current_token(),
                });
            }
        }

        Ok(())
    }

    /// Validate an authoritative write against both its fencing term and a
    /// live quorum. A partitioned former authority may not have observed the
    /// replacement's newer token yet, so fencing alone is insufficient until
    /// healing; expiry of its quorum evidence closes that split-brain window.
    pub fn validate_quorum_write(
        &self,
        token: FencingToken,
        required_quorum: usize,
    ) -> Result<(), AuthorityError> {
        self.validate_write(Some(token))?;
        let live = 1 + self
            .heartbeats
            .last_seen
            .keys()
            .filter(|node_id| !self.heartbeats.is_failed(node_id))
            .count();
        if live < required_quorum {
            return Err(AuthorityError::QuorumUnavailable {
                live,
                required: required_quorum,
            });
        }
        Ok(())
    }

    /// Step down from authority role (voluntary).
    pub fn step_down(&mut self) -> Result<(), AuthorityError> {
        if self.state.local_role == AuthorityRole::Authority {
            self.state.local_role = AuthorityRole::Replica;
            self.persist()?;
        }
        Ok(())
    }

    /// Recover from fenced state (after catching up).
    pub fn recover_from_fenced(&mut self) -> Result<(), AuthorityError> {
        if self.state.local_role == AuthorityRole::Fenced {
            self.state.local_role = AuthorityRole::Replica;
            self.persist()?;
        }
        Ok(())
    }

    /// Compute election priority for this node.
    pub fn election_priority(&self, operations_applied: u64) -> ElectionPriority {
        ElectionPriority::new(
            operations_applied,
            self.state.local_priority,
            self.state.local_node_id.clone(),
        )
    }

    /// Determine the winner of an election given a set of candidates.
    pub fn elect_authority(candidates: &[ElectionPriority]) -> Option<&ElectionPriority> {
        candidates.iter().max()
    }
}

/// Authority announcement message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthorityAnnouncement {
    pub authority_node: String,
    pub fencing_token: FencingToken,
    pub elected_at_ms: u64,
    pub cluster_id: String,
}

/// Heartbeat message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Heartbeat {
    pub from_node: String,
    pub is_authority: bool,
    pub fencing_token: FencingToken,
    pub operations_applied: u64,
    pub timestamp_ms: u64,
}

impl Heartbeat {
    pub fn new(
        from_node: String,
        is_authority: bool,
        fencing_token: FencingToken,
        operations_applied: u64,
    ) -> Self {
        Self {
            from_node,
            is_authority,
            fencing_token,
            operations_applied,
            timestamp_ms: now_ms(),
        }
    }
}

/// Election request message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ElectionRequest {
    pub candidate_node: String,
    pub priority: ElectionPriority,
    pub proposed_token: FencingToken,
    pub timestamp_ms: u64,
}

/// Election response message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ElectionResponse {
    pub from_node: String,
    pub vote_granted: bool,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fencing_token_ordering() {
        let t1 = FencingToken::initial();
        let t2 = t1.next();
        let t3 = t2.next();

        assert!(t2.supersedes(&t1));
        assert!(t3.supersedes(&t2));
        assert!(t3.supersedes(&t1));
        assert!(!t1.supersedes(&t2));
    }

    #[test]
    fn authority_claim_and_fencing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authority.json");

        // Node A claims authority
        let mut store_a =
            AuthorityStore::open(&path, "test-cluster", "A", 10).expect("open store A");
        let result = store_a.claim_authority().expect("claim");
        match result {
            ClaimResult::Granted { token } => {
                assert_eq!(token, FencingToken(2)); // initial is 1, next is 2
            }
            _ => panic!("expected Granted"),
        }
        assert!(store_a.state().is_authority());

        // Node B sees A's authority
        let path_b = dir.path().join("authority_b.json");
        let mut store_b =
            AuthorityStore::open(&path_b, "test-cluster", "B", 5).expect("open store B");
        store_b
            .accept_authority("A", FencingToken(2))
            .expect("accept");
        assert!(!store_b.state().is_authority());
        assert_eq!(store_b.state().authority_node(), Some("A"));

        // Node B tries to claim - should be rejected since A is still up
        let result_b = store_b.claim_authority().expect("claim B");
        match result_b {
            ClaimResult::Rejected {
                current_authority,
                current_token,
            } => {
                assert_eq!(current_authority, "A");
                assert_eq!(current_token, FencingToken(2));
            }
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn failover_when_authority_dies() {
        let dir = tempfile::tempdir().unwrap();

        // Node A is authority
        let path_a = dir.path().join("authority_a.json");
        let mut store_a =
            AuthorityStore::open(&path_a, "test-cluster", "A", 10).expect("open store A");
        store_a.claim_authority().expect("claim A");

        // Node B knows about A
        let path_b = dir.path().join("authority_b.json");
        let mut store_b =
            AuthorityStore::open(&path_b, "test-cluster", "B", 5).expect("open store B");
        store_b
            .accept_authority("A", FencingToken(2))
            .expect("accept A");

        // Simulate A dying - mark heartbeat as very old
        store_b.heartbeats.last_seen.insert("A".to_string(), 0);

        // Now B should be able to claim
        assert!(store_b.authority_failed());
        let result_b = store_b.claim_authority().expect("claim B after failover");
        match result_b {
            ClaimResult::Granted { token } => {
                assert_eq!(token, FencingToken(3)); // higher than A's token
            }
            _ => panic!("expected Granted after failover"),
        }
    }

    #[test]
    fn stale_authority_is_fenced() {
        let dir = tempfile::tempdir().unwrap();

        // A is authority with token 2
        let path_a = dir.path().join("authority_a.json");
        let mut store_a =
            AuthorityStore::open(&path_a, "test-cluster", "A", 10).expect("open store A");
        store_a.claim_authority().expect("claim A");
        assert!(store_a.state().is_authority());

        // A receives announcement that B is now authority with token 3
        store_a
            .accept_authority("B", FencingToken(3))
            .expect("accept B");

        // A should now be fenced
        assert!(store_a.state().is_fenced());
        assert!(!store_a.state().is_authority());

        // A's writes should be rejected
        let result = store_a.validate_write(None);
        assert!(matches!(result, Err(AuthorityError::Fenced(_))));
    }

    #[test]
    fn election_priority_ordering() {
        let p1 = ElectionPriority::new(100, 5, "A".to_string());
        let p2 = ElectionPriority::new(100, 10, "B".to_string());
        let p3 = ElectionPriority::new(150, 5, "C".to_string());

        // Higher operations wins
        assert!(p3 > p1);
        assert!(p3 > p2);

        // Same operations, higher priority wins
        assert!(p2 > p1);

        // Election selects highest priority
        let candidates = vec![p1.clone(), p2.clone(), p3.clone()];
        let winner = AuthorityStore::elect_authority(&candidates);
        assert_eq!(winner.map(|w| &w.node_id), Some(&"C".to_string()));
    }

    #[test]
    fn heartbeat_failure_detection() {
        let mut heartbeats = HeartbeatState::new(1000, 3); // 1s interval, 3 missed = failure

        // Record a heartbeat
        heartbeats.record_heartbeat("A");
        assert!(!heartbeats.is_failed("A"));

        // Manually set an old timestamp
        heartbeats.last_seen.insert("A".to_string(), 0);

        // Should now be detected as failed (current time is way past 3 intervals)
        assert!(heartbeats.is_failed("A"));
    }

    #[test]
    fn partitioned_authority_loses_write_quorum() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = AuthorityStore::open_with_failure_detector(
            dir.path().join("authority.json"),
            "test-cluster",
            "A",
            10,
            1,
            1,
        )
        .expect("open authority store");
        let token = match store.claim_authority().expect("claim") {
            ClaimResult::Granted { token } => token,
            _ => panic!("authority claim was not granted"),
        };
        store.heartbeat("B");
        assert!(store.validate_quorum_write(token, 2).is_ok());
        store.heartbeats.last_seen.insert("B".to_string(), 0);
        assert!(matches!(
            store.validate_quorum_write(token, 2),
            Err(AuthorityError::QuorumUnavailable {
                live: 1,
                required: 2
            })
        ));
    }

    #[test]
    fn recovery_from_fenced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authority.json");

        let mut store = AuthorityStore::open(&path, "test-cluster", "A", 10).expect("open store");
        store.claim_authority().expect("claim");

        // Get fenced
        store
            .accept_authority("B", FencingToken(3))
            .expect("accept B");
        assert!(store.state().is_fenced());

        // Recover
        store.recover_from_fenced().expect("recover");
        assert!(!store.state().is_fenced());
        assert_eq!(store.state().local_role, AuthorityRole::Replica);
    }
}
