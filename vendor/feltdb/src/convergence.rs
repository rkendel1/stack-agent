/// Convergence layer module for distributed state consistency
/// Provides causal state versioning, conflict resolution, and deterministic merge
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;

/// Vector clock for tracking causality in distributed systems
/// Each instance maintains a logical clock value
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct VectorClock {
    /// Map of instance_id to logical clock value
    pub clocks: HashMap<String, u64>,
}

impl VectorClock {
    /// Create a new vector clock
    pub fn new() -> Self {
        Self {
            clocks: HashMap::new(),
        }
    }

    /// Create a vector clock from a HashMap
    pub fn from_clocks(clocks: HashMap<String, u64>) -> Self {
        Self { clocks }
    }

    /// Increment the clock for a specific instance
    pub fn increment(&mut self, instance_id: &str) {
        let entry = self.clocks.entry(instance_id.to_string()).or_insert(0);
        *entry += 1;
    }

    /// Get the clock value for an instance
    pub fn get(&self, instance_id: &str) -> u64 {
        *self.clocks.get(instance_id).unwrap_or(&0)
    }

    /// Merge with another vector clock (takes maximum for each instance)
    pub fn merge(&mut self, other: &VectorClock) {
        for (instance_id, other_clock) in &other.clocks {
            let entry = self.clocks.entry(instance_id.clone()).or_insert(0);
            *entry = (*entry).max(*other_clock);
        }
    }

    /// Check if this clock causally precedes another
    pub fn happens_before(&self, other: &VectorClock) -> bool {
        let mut at_least_one_less = false;

        for (instance_id, other_value) in &other.clocks {
            let self_value = self.get(instance_id);
            if self_value > *other_value {
                return false; // This clock is ahead in some dimension
            }
            if self_value < *other_value {
                at_least_one_less = true;
            }
        }

        // Also check if self has clocks that other doesn't (or other is behind on them)
        for (instance_id, self_value) in &self.clocks {
            let other_value = other.get(instance_id);
            if self_value > &other_value {
                return false;
            }
        }

        at_least_one_less
    }

    /// Check if two clocks are concurrent (neither happens before the other)
    pub fn is_concurrent(&self, other: &VectorClock) -> bool {
        !self.happens_before(other) && !other.happens_before(self)
    }

    /// True when this clock has seen at least everything `other` has, in every
    /// dimension.
    ///
    /// This is the test a causal receiver needs, and it is not `happens_before`.
    /// `happens_before` answers a question about ordering and returns false for
    /// concurrent clocks, so a receiver using it would admit an operation whose
    /// dependencies are genuinely unmet whenever the frontier happened to be
    /// ahead in some unrelated dimension. Dominance asks only "have I seen
    /// everything this operation needs", which is what eligibility means.
    pub fn dominates(&self, other: &VectorClock) -> bool {
        other
            .clocks
            .iter()
            .all(|(instance_id, required)| self.get(instance_id) >= *required)
    }

    /// Compare two vector clocks for causal ordering
    pub fn causal_cmp(&self, other: &VectorClock) -> Ordering {
        if self == other {
            return Ordering::Equal;
        }

        if self.happens_before(other) {
            Ordering::Less
        } else if other.happens_before(self) {
            Ordering::Greater
        } else {
            // Concurrent - use lexicographic comparison of instance IDs as tiebreaker
            let all_instances: std::collections::BTreeSet<_> =
                self.clocks.keys().chain(other.clocks.keys()).collect();

            for instance_id in all_instances {
                let self_val = self.get(instance_id);
                let other_val = other.get(instance_id);
                if self_val != other_val {
                    return if self_val < other_val {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    };
                }
            }
            Ordering::Equal
        }
    }
}

impl Default for VectorClock {
    fn default() -> Self {
        Self::new()
    }
}

/// State version representing a causal snapshot of the system
/// Tracks which instance is at which logical clock value
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct StateVersion {
    /// Vector clock representing this state
    pub vector_clock: VectorClock,
    /// Timestamp when this version was created (ms since UNIX epoch)
    pub timestamp_ms: u128,
}

impl StateVersion {
    /// Create a new state version
    pub fn new(vector_clock: VectorClock, timestamp_ms: u128) -> Self {
        Self {
            vector_clock,
            timestamp_ms,
        }
    }

    /// Create a state version from clocks
    pub fn from_clocks(clocks: HashMap<String, u64>, timestamp_ms: u128) -> Self {
        Self {
            vector_clock: VectorClock::from_clocks(clocks),
            timestamp_ms,
        }
    }

    /// Check if this state version causally precedes another
    pub fn happens_before(&self, other: &StateVersion) -> bool {
        self.vector_clock.happens_before(&other.vector_clock)
    }

    /// Check if two state versions are concurrent
    pub fn is_concurrent(&self, other: &StateVersion) -> bool {
        self.vector_clock.is_concurrent(&other.vector_clock)
    }

    /// Get causal ordering between two state versions
    pub fn causal_cmp(&self, other: &StateVersion) -> Ordering {
        self.vector_clock.causal_cmp(&other.vector_clock)
    }
}

/// Tracks the causal history of an operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalHistory {
    /// The vector clock when this operation occurred
    pub state_version: StateVersion,
    /// Instance that originated this operation
    pub instance_id: String,
    /// Sequence number from that instance
    pub sequence: u64,
    /// Any dependencies on other operations (by their vector clocks)
    pub dependencies: Vec<VectorClock>,
}

impl CausalHistory {
    /// Create a new causal history entry
    pub fn new(state_version: StateVersion, instance_id: String, sequence: u64) -> Self {
        Self {
            state_version,
            instance_id,
            sequence,
            dependencies: Vec::new(),
        }
    }

    /// Add a dependency on another operation
    pub fn add_dependency(&mut self, dep_clock: VectorClock) {
        if !self.dependencies.contains(&dep_clock) {
            self.dependencies.push(dep_clock);
        }
    }

    /// Check if this operation depends on another (transitively)
    pub fn depends_on(&self, other: &CausalHistory) -> bool {
        // Direct dependency
        if self
            .dependencies
            .contains(&other.state_version.vector_clock)
        {
            return true;
        }

        // Indirect dependency would require full history traversal
        // For now, check if other happens before self
        other.state_version.happens_before(&self.state_version)
    }
}

/// Conflict resolution strategy for concurrent operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictResolutionStrategy {
    /// Last-Write-Wins based on timestamp with instance_id as tiebreaker
    LastWriteWins,
    /// Causally consistent - apply in causal order, keep concurrent
    CausalConsistent,
    /// Application-defined (returned as unresolved for app to handle)
    ApplicationDefined,
}

/// Result of conflict resolution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictResolution {
    /// Which operation "won" (Some(index)) or if unresolved (None)
    pub winner_index: Option<usize>,
    /// The resolution strategy used
    pub strategy: ConflictResolutionStrategy,
    /// Whether operations are concurrent
    pub is_concurrent: bool,
    /// Whether operations have causal ordering
    pub is_causal: bool,
}

impl ConflictResolution {
    /// Resolve concurrent operations using Last-Write-Wins strategy
    pub fn resolve_lww(histories: &[CausalHistory]) -> Self {
        if histories.is_empty() {
            return Self {
                winner_index: None,
                strategy: ConflictResolutionStrategy::LastWriteWins,
                is_concurrent: false,
                is_causal: false,
            };
        }

        if histories.len() == 1 {
            return Self {
                winner_index: Some(0),
                strategy: ConflictResolutionStrategy::LastWriteWins,
                is_concurrent: false,
                is_causal: false,
            };
        }

        // Check for causal ordering
        let mut is_causal = false;
        for i in 0..histories.len() {
            for j in 0..histories.len() {
                if i != j
                    && histories[i]
                        .state_version
                        .happens_before(&histories[j].state_version)
                {
                    is_causal = true;
                    break;
                }
            }
            if is_causal {
                break;
            }
        }

        // If there's causal ordering, return the one that happens after all others
        if is_causal {
            let mut winner_idx = 0;
            for i in 1..histories.len() {
                // If winner happens before i, then i is later
                if histories[winner_idx]
                    .state_version
                    .happens_before(&histories[i].state_version)
                {
                    winner_idx = i;
                }
            }
            return Self {
                winner_index: Some(winner_idx),
                strategy: ConflictResolutionStrategy::LastWriteWins,
                is_concurrent: false,
                is_causal: true,
            };
        }

        // Concurrent operations - use LWW with tiebreaker
        let mut winner_idx = 0;
        for i in 1..histories.len() {
            let ts_cmp = histories[i]
                .state_version
                .timestamp_ms
                .cmp(&histories[winner_idx].state_version.timestamp_ms);

            if ts_cmp == std::cmp::Ordering::Greater {
                winner_idx = i;
            } else if ts_cmp == std::cmp::Ordering::Equal {
                // Tiebreaker: lexicographic comparison of instance_id
                if histories[i].instance_id > histories[winner_idx].instance_id {
                    winner_idx = i;
                }
            }
        }

        Self {
            winner_index: Some(winner_idx),
            strategy: ConflictResolutionStrategy::LastWriteWins,
            is_concurrent: true,
            is_causal: false,
        }
    }
}

/// Deterministic merge result for reference graph convergence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeResult {
    /// Merged vector clock representing the combined state
    pub merged_clock: VectorClock,
    /// Conflicts detected during merge
    pub conflicts: Vec<(usize, usize, ConflictResolution)>,
    /// Whether merge resulted in a new state or kept existing
    pub changed: bool,
}

impl MergeResult {
    /// Create a new merge result
    pub fn new(merged_clock: VectorClock) -> Self {
        Self {
            merged_clock,
            conflicts: Vec::new(),
            changed: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_clock_increment() {
        let mut clock = VectorClock::new();
        clock.increment("inst1");
        assert_eq!(clock.get("inst1"), 1);
        assert_eq!(clock.get("inst2"), 0);

        clock.increment("inst1");
        assert_eq!(clock.get("inst1"), 2);
    }

    #[test]
    fn vector_clock_merge() {
        let mut clock1 = VectorClock::new();
        clock1.increment("inst1");
        clock1.increment("inst1");

        let mut clock2 = VectorClock::new();
        clock2.increment("inst2");
        clock2.increment("inst2");
        clock2.increment("inst2");

        clock1.merge(&clock2);
        assert_eq!(clock1.get("inst1"), 2);
        assert_eq!(clock1.get("inst2"), 3);
    }

    #[test]
    fn vector_clock_happens_before() {
        let mut clock1 = VectorClock::new();
        clock1.increment("inst1");

        let mut clock2 = VectorClock::new();
        clock2.increment("inst1");
        clock2.increment("inst1");

        assert!(clock1.happens_before(&clock2));
        assert!(!clock2.happens_before(&clock1));
    }

    #[test]
    fn vector_clock_concurrent() {
        let mut clock1 = VectorClock::new();
        clock1.increment("inst1");

        let mut clock2 = VectorClock::new();
        clock2.increment("inst2");

        assert!(clock1.is_concurrent(&clock2));
        assert!(clock2.is_concurrent(&clock1));
    }

    #[test]
    fn state_version_causal_ordering() {
        let mut clock1 = VectorClock::new();
        clock1.increment("inst1");
        let version1 = StateVersion::new(clock1, 1000);

        let mut clock2 = VectorClock::new();
        clock2.increment("inst1");
        clock2.increment("inst1");
        let version2 = StateVersion::new(clock2, 2000);

        assert!(version1.happens_before(&version2));
        assert!(version1.causal_cmp(&version2) == Ordering::Less);
    }

    #[test]
    fn conflict_resolution_lww_causal() {
        let mut clock1 = VectorClock::new();
        clock1.increment("inst1");
        let version1 = StateVersion::new(clock1, 1000);
        let history1 = CausalHistory::new(version1, "inst1".to_string(), 1);

        let mut clock2 = VectorClock::new();
        clock2.increment("inst1");
        clock2.increment("inst1");
        let version2 = StateVersion::new(clock2, 2000);
        let history2 = CausalHistory::new(version2, "inst1".to_string(), 2);

        let resolution = ConflictResolution::resolve_lww(&[history1, history2]);
        assert_eq!(resolution.winner_index, Some(1));
        assert!(resolution.is_causal);
        assert!(!resolution.is_concurrent);
    }

    #[test]
    fn conflict_resolution_lww_concurrent() {
        let mut clock1 = VectorClock::new();
        clock1.increment("inst1");
        let version1 = StateVersion::new(clock1, 1000);
        let history1 = CausalHistory::new(version1, "inst1".to_string(), 1);

        let mut clock2 = VectorClock::new();
        clock2.increment("inst2");
        let version2 = StateVersion::new(clock2, 2000);
        let history2 = CausalHistory::new(version2, "inst2".to_string(), 1);

        let resolution = ConflictResolution::resolve_lww(&[history1, history2]);
        assert!(resolution.winner_index.is_some());
        assert!(!resolution.is_causal);
        assert!(resolution.is_concurrent);
    }

    #[test]
    fn causal_history_dependency() {
        let mut clock1 = VectorClock::new();
        clock1.increment("inst1");
        let version1 = StateVersion::new(clock1.clone(), 1000);
        let history1 = CausalHistory::new(version1, "inst1".to_string(), 1);

        let mut clock2 = VectorClock::new();
        clock2.clone_from(&clock1);
        clock2.increment("inst2");
        let version2 = StateVersion::new(clock2, 2000);
        let mut history2 = CausalHistory::new(version2, "inst2".to_string(), 1);

        history2.add_dependency(clock1.clone());
        assert!(history2.depends_on(&history1));
    }

    #[test]
    fn vector_clock_lexicographic_tiebreaker() {
        let mut clock1 = VectorClock::new();
        clock1.clocks.insert("inst-a".to_string(), 1);
        clock1.clocks.insert("inst-b".to_string(), 1);

        let clock2 = VectorClock::from_clocks(clock1.clocks.clone());

        // Same clocks - should use lexicographic tiebreaker
        let cmp_result = clock1.causal_cmp(&clock2);
        assert_eq!(cmp_result, Ordering::Equal);
    }

    #[test]
    fn concurrent_mutations_deterministic_resolution() {
        // Scenario: Two instances concurrently modify the same entity
        // Result: Deterministic resolution based on LWW + instance_id tiebreaker

        // Instance A modifies at timestamp 1000
        let mut clock_a = VectorClock::new();
        clock_a.increment("inst-a");
        let version_a = StateVersion::new(clock_a, 1000);
        let history_a = CausalHistory::new(version_a, "inst-a".to_string(), 1);

        // Instance B modifies at timestamp 1000 (same time, but different instance)
        let mut clock_b = VectorClock::new();
        clock_b.increment("inst-b");
        let version_b = StateVersion::new(clock_b, 1000);
        let history_b = CausalHistory::new(version_b, "inst-b".to_string(), 1);

        // Resolve conflict
        let resolution = ConflictResolution::resolve_lww(&[history_a.clone(), history_b.clone()]);
        assert!(resolution.is_concurrent);
        assert!(!resolution.is_causal);
        assert!(resolution.winner_index.is_some());

        // Both resolutions should favor inst-b due to lexicographic ordering
        assert_eq!(
            resolution.winner_index,
            Some(1), // inst-b when b is second
        );
    }

    #[test]
    fn causal_chain_three_operations() {
        // Scenario: Three operations in a causal chain
        // inst1 -> inst2 -> inst3

        // Operation 1: inst1 creates something
        let mut clock1 = VectorClock::new();
        clock1.increment("inst1");
        let version1 = StateVersion::new(clock1.clone(), 1000);
        let history1 = CausalHistory::new(version1, "inst1".to_string(), 1);

        // Operation 2: inst2 observes inst1 and adds to it
        let mut clock2 = clock1.clone();
        clock2.increment("inst2");
        let version2 = StateVersion::new(clock2.clone(), 2000);
        let history2 = CausalHistory::new(version2, "inst2".to_string(), 1);

        // Operation 3: inst3 observes both and adds more
        let mut clock3 = clock2.clone();
        clock3.increment("inst3");
        let version3 = StateVersion::new(clock3, 3000);
        let history3 = CausalHistory::new(version3, "inst3".to_string(), 1);

        // Verify causal chain
        assert!(history1
            .state_version
            .happens_before(&history2.state_version));
        assert!(history2
            .state_version
            .happens_before(&history3.state_version));
        assert!(history1
            .state_version
            .happens_before(&history3.state_version));

        // Resolve in different orders - should always choose the one that happens last
        let resolution1 = ConflictResolution::resolve_lww(&[history1, history2, history3]);
        assert_eq!(resolution1.winner_index, Some(2)); // history3 is latest
        assert!(resolution1.is_causal);
    }

    #[test]
    fn merge_result_construction() {
        let mut merged_clock = VectorClock::new();
        merged_clock.increment("inst1");
        merged_clock.increment("inst2");

        let mut merge_result = MergeResult::new(merged_clock.clone());
        assert!(!merge_result.changed);
        assert_eq!(merge_result.conflicts.len(), 0);

        // Add a conflict
        let history_a = CausalHistory::new(
            StateVersion::new(VectorClock::new(), 1000),
            "inst-a".to_string(),
            1,
        );
        let history_b = CausalHistory::new(
            StateVersion::new(VectorClock::new(), 1000),
            "inst-b".to_string(),
            1,
        );
        let resolution = ConflictResolution::resolve_lww(&[history_a, history_b]);
        merge_result.conflicts.push((0, 1, resolution));

        assert_eq!(merge_result.conflicts.len(), 1);
    }

    #[test]
    fn vector_clock_partial_order() {
        // Test partial ordering properties of vector clocks

        let mut clock1 = VectorClock::new();
        clock1.increment("inst1");

        let mut clock2 = VectorClock::new();
        clock2.increment("inst1");
        clock2.increment("inst2");

        let mut clock3 = VectorClock::new();
        clock3.increment("inst2");
        clock3.increment("inst2");

        // clock1 happens before clock2
        assert!(clock1.happens_before(&clock2));
        assert!(!clock2.happens_before(&clock1));

        // clock1 and clock3 are concurrent
        assert!(clock1.is_concurrent(&clock3));
        assert!(clock3.is_concurrent(&clock1));

        // clock2 and clock3 are concurrent (neither is strictly before the other)
        assert!(clock2.is_concurrent(&clock3));
    }

    #[test]
    fn state_version_ordering() {
        // Test that StateVersion uses vector clock ordering

        let mut clock1 = VectorClock::new();
        clock1.increment("inst1");
        let version1 = StateVersion::new(clock1, 1000);

        let mut clock2 = VectorClock::new();
        clock2.increment("inst1");
        clock2.increment("inst1");
        let version2 = StateVersion::new(clock2, 2000);

        // version1 causally precedes version2
        assert!(version1.happens_before(&version2));

        // version1 is ordered before version2
        assert_eq!(version1.causal_cmp(&version2), Ordering::Less);
        assert_eq!(version2.causal_cmp(&version1), Ordering::Greater);
    }
}
