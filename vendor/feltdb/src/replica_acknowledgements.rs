//! Durable replica acknowledgements.
//!
//! An acknowledgement is a statement with a precise meaning, and the precision
//! is the point:
//!
//! > this replica has **durably applied** everything the frontier represents.
//!
//! Receiving a message is not an acknowledgement. A frontier a peer *claims* is
//! not one either, until it is durable on that peer. FeltDB's applied frontier
//! already has exactly this property -- it advances only in `record_applied`,
//! after the envelope has been fsynced and applied -- so an acknowledgement is
//! read from it rather than assembled.
//!
//! ## The conservative rule
//!
//! `adrs/membership-safety-and-conservative-reclamation.md` decides that a node
//! may act only on positive durable evidence, never on an absence:
//!
//! ```text
//!     unknown replica              unknown replica
//!          │                            │
//!          ▼                            ▼
//!     assume required             assume irrelevant     <- forbidden
//!          │                            │
//!          ▼                            ▼
//!     retain history               delete history
//! ```
//!
//! Two things follow, and both are enforced here rather than left to the
//! caller that will eventually reclaim:
//!
//! 1. the required set is the **union** of this node's membership and every
//!    replica named in any membership view observed from a peer. That is why an
//!    acknowledgement carries the sender's membership view: without it the
//!    union has no input, and a node could never learn that its view is short.
//! 2. a required replica with no acknowledgement **blocks**. There is no
//!    timeout after which a silent replica is treated as caught up, because a
//!    timeout is the forbidden inference with a clock attached.
//!
//! ## Deliberately not here
//!
//! Nothing is reclaimed, compacted, deleted or checkpointed. No consensus, no
//! epochs. This records what replicas have applied and computes what that
//! evidence does and does not permit; acting on it is a later change.

use crate::convergence::VectorClock;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// What one replica has durably applied, as it reported it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplicaAcknowledgement {
    pub replica_id: String,
    pub node_id: String,
    /// Everything this replica has durably applied. Read from its applied
    /// frontier, which advances only after an fsynced envelope is applied.
    pub applied_frontier: VectorClock,
    /// The highest identity this replica has committed per origin.
    ///
    /// Carried because the operation log is also the source from which
    /// `next_origin_sequence` is recovered, so a checkpoint that discards log
    /// must preserve these or identity allocation regresses.
    pub origin_sequence_high_water: BTreeMap<String, u64>,
    /// Every replica the sender's membership names. The input to the union
    /// rule: this is how a node discovers a replica it has never heard of.
    pub membership_view: Vec<String>,
    /// When the sender produced it. Recorded for operators, never used to
    /// decide anything -- a stale acknowledgement is not a missing one, and
    /// neither is treated as permission.
    pub produced_at_ms: u64,
}

impl ReplicaAcknowledgement {
    pub fn new(
        replica_id: String,
        node_id: String,
        applied_frontier: VectorClock,
        origin_sequence_high_water: BTreeMap<String, u64>,
        membership_view: Vec<String>,
    ) -> Self {
        Self {
            replica_id,
            node_id,
            applied_frontier,
            origin_sequence_high_water,
            membership_view,
            produced_at_ms: now_ms(),
        }
    }
}

/// What this node's evidence permits.
///
/// A typed outcome rather than an `Option<VectorClock>`, because "there is no
/// safe frontier yet" and "the safe frontier is empty" are different states and
/// a caller that confused them would reclaim on an absence.
#[derive(Clone, Debug, PartialEq)]
pub enum ReclamationEvidence {
    /// Some required replica has not acknowledged. Nothing may be reclaimed,
    /// and these are the replicas being waited on.
    Blocked { waiting_on: Vec<String> },
    /// Every required replica has acknowledged at least this much.
    SafeThrough {
        frontier: VectorClock,
        covered: Vec<String>,
    },
}

#[derive(Debug)]
pub enum AcknowledgementError {
    Io(String),
}

impl std::fmt::Display for AcknowledgementError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(message) => write!(out, "acknowledgement store: {message}"),
        }
    }
}

/// Durable acknowledgements, one per replica: the latest each has reported.
pub struct AcknowledgementStore {
    path: PathBuf,
    acknowledgements: BTreeMap<String, ReplicaAcknowledgement>,
}

impl AcknowledgementStore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, AcknowledgementError> {
        let path = path.as_ref().to_path_buf();
        let acknowledgements = if path.exists() {
            let bytes =
                std::fs::read(&path).map_err(|e| AcknowledgementError::Io(e.to_string()))?;
            serde_json::from_slice(&bytes).map_err(|e| AcknowledgementError::Io(e.to_string()))?
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            path,
            acknowledgements,
        })
    }

    fn persist(&self) -> Result<(), AcknowledgementError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AcknowledgementError::Io(e.to_string()))?;
        }
        let temp = self.path.with_extension("tmp");
        let bytes = serde_json::to_vec_pretty(&self.acknowledgements)
            .map_err(|e| AcknowledgementError::Io(e.to_string()))?;
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp)
                .map_err(|e| AcknowledgementError::Io(e.to_string()))?;
            file.write_all(&bytes)
                .map_err(|e| AcknowledgementError::Io(e.to_string()))?;
            file.sync_all()
                .map_err(|e| AcknowledgementError::Io(e.to_string()))?;
        }
        std::fs::rename(&temp, &self.path).map_err(|e| AcknowledgementError::Io(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }

    /// Record an acknowledgement, durably, before it counts as evidence.
    ///
    /// An acknowledgement that only ever existed in memory would be forgotten
    /// by the restart that most needs it, and the node would then be unable to
    /// say what it had been told.
    ///
    /// A report that does not advance the frontier is kept rather than
    /// discarded, so `produced_at_ms` still moves and an operator can see the
    /// replica is alive and stuck. A report that would move a frontier
    /// *backwards* is refused: acknowledgement is monotonic, and accepting a
    /// regression would let a peer talk a node into believing less is durable
    /// than it has already been told, which is the one direction that could
    /// later un-block a reclamation decision.
    pub fn record(&mut self, ack: ReplicaAcknowledgement) -> Result<bool, AcknowledgementError> {
        if let Some(existing) = self.acknowledgements.get(&ack.replica_id) {
            if !ack.applied_frontier.dominates(&existing.applied_frontier) {
                return Ok(false);
            }
        }
        self.acknowledgements.insert(ack.replica_id.clone(), ack);
        self.persist()?;
        Ok(true)
    }

    pub fn get(&self, replica_id: &str) -> Option<&ReplicaAcknowledgement> {
        self.acknowledgements.get(replica_id)
    }

    pub fn all(&self) -> Vec<ReplicaAcknowledgement> {
        self.acknowledgements.values().cloned().collect()
    }

    /// Every replica named by this node's membership or by any membership view
    /// it has been told about.
    ///
    /// The union is what makes "unknown means required" real: a replica this
    /// node has never enrolled, but which a peer names, is required here too.
    pub fn required_union(&self, local_required: &[String]) -> Vec<String> {
        let mut union: BTreeSet<String> = local_required.iter().cloned().collect();
        for ack in self.acknowledgements.values() {
            for replica in &ack.membership_view {
                union.insert(replica.clone());
            }
        }
        union.into_iter().collect()
    }

    /// What the evidence permits, given the replicas this node must retain for.
    ///
    /// `local_required` comes from durable membership. Replicas learned only
    /// from a peer's view are added by `required_union` and, having never
    /// acknowledged anything here, block -- which is the intended outcome and
    /// not an oversight.
    pub fn evidence(&self, local_required: &[String]) -> ReclamationEvidence {
        let required = self.required_union(local_required);
        if required.is_empty() {
            // No required replicas at all. Still not a licence: a node with no
            // membership has not established that it is alone, it has
            // established nothing.
            return ReclamationEvidence::Blocked {
                waiting_on: Vec::new(),
            };
        }

        let missing: Vec<String> = required
            .iter()
            .filter(|replica| !self.acknowledgements.contains_key(*replica))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return ReclamationEvidence::Blocked {
                waiting_on: missing,
            };
        }

        // The minimum over every required replica, component-wise. A frontier
        // is safe only where every one of them has reached it.
        let mut frontier = VectorClock::new();
        let mut origins: BTreeSet<String> = BTreeSet::new();
        for replica in &required {
            let ack = &self.acknowledgements[replica];
            for origin in ack.applied_frontier.clocks.keys() {
                origins.insert(origin.clone());
            }
        }
        for origin in origins {
            let lowest = required
                .iter()
                .map(|replica| self.acknowledgements[replica].applied_frontier.get(&origin))
                .min()
                .unwrap_or(0);
            if lowest > 0 {
                frontier.clocks.insert(origin, lowest);
            }
        }
        ReclamationEvidence::SafeThrough {
            frontier,
            covered: required,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(entries: &[(&str, u64)]) -> VectorClock {
        let mut clock = VectorClock::new();
        for (node, value) in entries {
            clock.clocks.insert((*node).to_string(), *value);
        }
        clock
    }

    fn ack(replica: &str, frontier: &[(&str, u64)], view: &[&str]) -> ReplicaAcknowledgement {
        ReplicaAcknowledgement::new(
            replica.to_string(),
            replica.to_uppercase(),
            clock(frontier),
            BTreeMap::new(),
            view.iter().map(|v| v.to_string()).collect(),
        )
    }

    fn store(dir: &tempfile::TempDir) -> AcknowledgementStore {
        AcknowledgementStore::open(dir.path().join("acknowledgements.json")).expect("open")
    }

    // -- durability --------------------------------------------------------

    #[test]
    fn acknowledgements_survive_restart() {
        // The acceptance criterion: after a restart a node can prove what its
        // replicas acknowledged. An in-memory record would be forgotten by
        // exactly the event that makes the question worth asking.
        let dir = tempfile::tempdir().unwrap();
        {
            let mut acks = store(&dir);
            acks.record(ack("rb", &[("A", 3)], &["ra", "rb"])).unwrap();
        }
        let recovered = store(&dir);
        let recorded = recovered.get("rb").expect("rb acknowledged");
        assert_eq!(recorded.applied_frontier.get("A"), 3);
        assert_eq!(recorded.membership_view, vec!["ra", "rb"]);
    }

    #[test]
    fn acknowledgement_is_monotonic_and_a_regression_is_refused() {
        // Accepting a lower frontier would let a peer talk this node into
        // believing less is durable than it has already been told -- the one
        // direction that could later un-block a decision that was blocked.
        let dir = tempfile::tempdir().unwrap();
        let mut acks = store(&dir);
        assert!(acks.record(ack("rb", &[("A", 5)], &["ra", "rb"])).unwrap());
        assert!(!acks.record(ack("rb", &[("A", 2)], &["ra", "rb"])).unwrap());
        assert_eq!(acks.get("rb").unwrap().applied_frontier.get("A"), 5);

        // A repeat at the same frontier is kept, so liveness stays visible.
        assert!(acks.record(ack("rb", &[("A", 5)], &["ra", "rb"])).unwrap());
    }

    // -- the conservative rule ---------------------------------------------

    #[test]
    fn a_required_replica_with_no_acknowledgement_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let mut acks = store(&dir);
        acks.record(ack("rb", &[("A", 3)], &["ra", "rb", "rc"]))
            .unwrap();

        let evidence = acks.evidence(&["ra".into(), "rb".into(), "rc".into()]);
        match evidence {
            ReclamationEvidence::Blocked { waiting_on } => {
                assert_eq!(
                    waiting_on,
                    vec!["ra", "rc"],
                    "and it names who it waits for"
                );
            }
            other => panic!("missing acknowledgement must block, got {other:?}"),
        }
    }

    #[test]
    fn a_replica_known_only_from_a_peers_view_is_required_and_blocks() {
        // This is "unknown means required" doing its work. This node's
        // membership does not contain rc at all; rb's acknowledgement names it,
        // and that alone is enough to stop reclamation.
        let dir = tempfile::tempdir().unwrap();
        let mut acks = store(&dir);
        acks.record(ack("ra", &[("A", 3)], &["ra", "rb"])).unwrap();
        acks.record(ack("rb", &[("A", 3)], &["ra", "rb", "rc"]))
            .unwrap();

        let local = vec!["ra".to_string(), "rb".to_string()];
        assert_eq!(acks.required_union(&local), vec!["ra", "rb", "rc"]);
        match acks.evidence(&local) {
            ReclamationEvidence::Blocked { waiting_on } => assert_eq!(waiting_on, vec!["rc"]),
            other => panic!("a replica this node has never heard of must block, got {other:?}"),
        }
    }

    #[test]
    fn no_membership_at_all_is_not_permission() {
        // A node that knows of no replicas has not established that it is
        // alone. It has established nothing, and nothing is what it may act on.
        let dir = tempfile::tempdir().unwrap();
        let acks = store(&dir);
        assert!(matches!(
            acks.evidence(&[]),
            ReclamationEvidence::Blocked { .. }
        ));
    }

    #[test]
    fn a_stale_acknowledgement_is_not_a_missing_one_and_neither_is_permission() {
        // Age is recorded and never consulted. An acknowledgement produced long
        // ago still says what it says; what it does not do is advance.
        let dir = tempfile::tempdir().unwrap();
        let mut acks = store(&dir);
        let mut ancient = ack("rb", &[("A", 2)], &["ra", "rb"]);
        ancient.produced_at_ms = 0;
        acks.record(ancient).unwrap();
        acks.record(ack("ra", &[("A", 7)], &["ra", "rb"])).unwrap();

        match acks.evidence(&["ra".into(), "rb".into()]) {
            ReclamationEvidence::SafeThrough { frontier, .. } => {
                assert_eq!(
                    frontier.get("A"),
                    2,
                    "the laggard sets the safe frontier, however old its report"
                );
            }
            other => panic!("expected a safe frontier, got {other:?}"),
        }
    }

    // -- the minimum -------------------------------------------------------

    #[test]
    fn the_safe_frontier_is_the_minimum_over_every_required_replica() {
        let dir = tempfile::tempdir().unwrap();
        let mut acks = store(&dir);
        acks.record(ack("ra", &[("A", 9), ("B", 4)], &["ra", "rb"]))
            .unwrap();
        acks.record(ack("rb", &[("A", 5), ("B", 6)], &["ra", "rb"]))
            .unwrap();

        match acks.evidence(&["ra".into(), "rb".into()]) {
            ReclamationEvidence::SafeThrough { frontier, covered } => {
                assert_eq!(frontier.get("A"), 5, "component-wise minimum, per origin");
                assert_eq!(frontier.get("B"), 4);
                assert_eq!(covered, vec!["ra", "rb"]);
            }
            other => panic!("expected a safe frontier, got {other:?}"),
        }
    }

    #[test]
    fn an_origin_one_replica_has_never_seen_is_not_safe_at_all() {
        // rb has applied nothing from C. The minimum for C is therefore zero,
        // and a zero component must not appear as though it were a position
        // anything is safe through.
        let dir = tempfile::tempdir().unwrap();
        let mut acks = store(&dir);
        acks.record(ack("ra", &[("A", 4), ("C", 9)], &["ra", "rb"]))
            .unwrap();
        acks.record(ack("rb", &[("A", 4)], &["ra", "rb"])).unwrap();

        match acks.evidence(&["ra".into(), "rb".into()]) {
            ReclamationEvidence::SafeThrough { frontier, .. } => {
                assert_eq!(frontier.get("A"), 4);
                assert_eq!(
                    frontier.get("C"),
                    0,
                    "one replica has none of C's history, so none of it is safe"
                );
                assert!(!frontier.clocks.contains_key("C"));
            }
            other => panic!("expected a safe frontier, got {other:?}"),
        }
    }

    #[test]
    fn evidence_survives_restart_and_still_blocks_for_the_same_replica() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut acks = store(&dir);
            acks.record(ack("rb", &[("A", 3)], &["ra", "rb", "rc"]))
                .unwrap();
        }
        let recovered = store(&dir);
        match recovered.evidence(&["ra".into(), "rb".into()]) {
            ReclamationEvidence::Blocked { waiting_on } => {
                assert_eq!(
                    waiting_on,
                    vec!["ra", "rc"],
                    "the union learned from rb's view survives the restart too"
                );
            }
            other => panic!("expected blocked, got {other:?}"),
        }
    }

    #[test]
    fn origin_sequence_high_water_marks_are_carried() {
        // Needed by the checkpoint that comes later: the operation log is also
        // the source from which next_origin_sequence is recovered.
        let dir = tempfile::tempdir().unwrap();
        let mut acks = store(&dir);
        let mut ack = ack("rb", &[("A", 3)], &["ra", "rb"]);
        ack.origin_sequence_high_water.insert("A".to_string(), 7);
        acks.record(ack).unwrap();

        let recovered = store(&dir);
        assert_eq!(
            recovered
                .get("rb")
                .unwrap()
                .origin_sequence_high_water
                .get("A"),
            Some(&7),
        );
    }
}
