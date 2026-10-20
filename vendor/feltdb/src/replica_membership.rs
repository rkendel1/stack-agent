//! Durable replica membership.
//!
//! The question this exists to answer, and which nothing in FeltDB could
//! answer before:
//!
//! > which replicas must be caught up before this node may reclaim
//! > replication history?
//!
//! `adrs/replication-retention-and-recovery-lag.md` found that "every required
//! replica" had no referent. `membership` in this codebase meant *authorization*
//! membership -- users and organisations -- and replication peers came from
//! `--send` / `--recv` flags at startup. Three things were conflated:
//!
//! ```text
//!     configured peer   !=  cluster member
//!     connected peer    !=  required replica
//!     removed replica   !=  temporarily offline replica
//! ```
//!
//! This module separates them. It stores membership durably, keyed to explicit
//! state transitions, and it is deliberately the *only* thing that can change
//! which replicas are required.
//!
//! ## The rule that matters
//!
//! **Only an explicit transition to `Removed` takes a replica out of the
//! reclamation set.** Not going offline, not being unreachable, not a stale
//! `last_seen_at`, not being absent from this node's connection flags. There is
//! no code path here that derives `required_for_reclamation` from liveness, and
//! that absence is the feature: otherwise a replica that is merely switched off
//! becomes irrelevant to reclamation, and the history it still needs is deleted
//! while it is not there to object.
//!
//! `Suspected` exists to record that a replica looks unreachable. It changes
//! nothing about whether it is required, and there are tests asserting exactly
//! that.
//!
//! ## Deliberately not here
//!
//! No compaction frontier, no acknowledgement tracking, no checkpoint, no
//! change to the operation log. This establishes the authority the next change
//! consumes; it reclaims nothing and deletes nothing.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Where a replica stands in the cluster.
///
/// Explicit rather than inferred. A status is changed by an operation someone
/// performed, never by this module observing a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReplicaStatus {
    /// Admitted to the cluster but not yet approved: it is being seeded and is
    /// not yet a replica anyone should be waiting for.
    Joining,
    /// A full member. Required for reclamation.
    Active,
    /// Looks unreachable. **Still required for reclamation** -- this status is
    /// an observation, not a decision.
    Suspected,
    /// Explicitly removed. The only status that leaves the reclamation set, and
    /// terminal: a removed replica rejoins by joining again, not by being seen.
    Removed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Replica {
    pub replica_id: String,
    pub node_id: String,
    pub status: ReplicaStatus,
    pub joined_at_ms: u64,
    pub last_seen_at_ms: Option<u64>,
    /// Whether history must be retained on this replica's behalf.
    ///
    /// Stored rather than derived from `status` at read time, so that the value
    /// which authorises deletion is one somebody wrote down and an audit can
    /// read back, rather than the output of a rule that could later be changed
    /// underneath the data.
    pub required_for_reclamation: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplicationCluster {
    pub cluster_id: String,
    pub created_at_ms: u64,
    /// Keyed by `replica_id`, ordered so that reads are deterministic.
    pub replicas: BTreeMap<String, Replica>,
}

#[derive(Debug)]
pub enum MembershipError {
    NoCluster,
    ClusterExists(String),
    UnknownReplica(String),
    ReplicaExists(String),
    NotJoining(String),
    Io(String),
}

impl std::fmt::Display for MembershipError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCluster => write!(out, "no cluster has been created"),
            Self::ClusterExists(id) => write!(out, "cluster {id} already exists"),
            Self::UnknownReplica(id) => write!(out, "unknown replica {id}"),
            Self::ReplicaExists(id) => write!(out, "replica {id} already exists"),
            Self::NotJoining(id) => write!(out, "replica {id} is not JOINING"),
            Self::Io(message) => write!(out, "membership store: {message}"),
        }
    }
}

/// Durable membership for one node.
///
/// Persisted beside the operation log and never reconstructed from connection
/// flags: a restart that rebuilt membership from `--send`/`--recv` would make
/// the reclamation set a function of how the process happened to be launched.
pub struct MembershipStore {
    path: PathBuf,
    cluster: Option<ReplicationCluster>,
}

impl MembershipStore {
    /// Open a store, loading any cluster already on disk.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, MembershipError> {
        let path = path.as_ref().to_path_buf();
        let cluster = if path.exists() {
            let bytes = std::fs::read(&path).map_err(|e| MembershipError::Io(e.to_string()))?;
            Some(serde_json::from_slice(&bytes).map_err(|e| MembershipError::Io(e.to_string()))?)
        } else {
            None
        };
        Ok(Self { path, cluster })
    }

    /// Write the cluster durably: temp file, fsync, rename, fsync the
    /// directory. Membership is what will later authorise deleting history, so
    /// it is written with the same care as the log it will authorise deleting.
    fn persist(&self) -> Result<(), MembershipError> {
        let Some(cluster) = &self.cluster else {
            return Ok(());
        };
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| MembershipError::Io(e.to_string()))?;
        }
        let temp = self.path.with_extension("tmp");
        let bytes =
            serde_json::to_vec_pretty(cluster).map_err(|e| MembershipError::Io(e.to_string()))?;
        {
            use std::io::Write;
            let mut file =
                std::fs::File::create(&temp).map_err(|e| MembershipError::Io(e.to_string()))?;
            file.write_all(&bytes)
                .map_err(|e| MembershipError::Io(e.to_string()))?;
            file.sync_all()
                .map_err(|e| MembershipError::Io(e.to_string()))?;
        }
        std::fs::rename(&temp, &self.path).map_err(|e| MembershipError::Io(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                // Best effort: the rename is durable on the filesystems this
                // runs on once the directory entry is synced.
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }

    pub fn cluster(&self) -> Option<&ReplicationCluster> {
        self.cluster.as_ref()
    }

    pub fn create_cluster(&mut self, cluster_id: &str) -> Result<(), MembershipError> {
        if let Some(existing) = &self.cluster {
            return Err(MembershipError::ClusterExists(existing.cluster_id.clone()));
        }
        self.cluster = Some(ReplicationCluster {
            cluster_id: cluster_id.to_string(),
            created_at_ms: now_ms(),
            replicas: BTreeMap::new(),
        });
        self.persist()
    }

    fn cluster_mut(&mut self) -> Result<&mut ReplicationCluster, MembershipError> {
        self.cluster.as_mut().ok_or(MembershipError::NoCluster)
    }

    /// Admit a replica as `Joining`. It is not yet required for reclamation:
    /// it is being seeded and holds nothing anyone is waiting on.
    pub fn join_replica(&mut self, replica_id: &str, node_id: &str) -> Result<(), MembershipError> {
        let cluster = self.cluster_mut()?;
        if cluster.replicas.contains_key(replica_id) {
            return Err(MembershipError::ReplicaExists(replica_id.to_string()));
        }
        cluster.replicas.insert(
            replica_id.to_string(),
            Replica {
                replica_id: replica_id.to_string(),
                node_id: node_id.to_string(),
                status: ReplicaStatus::Joining,
                joined_at_ms: now_ms(),
                last_seen_at_ms: None,
                required_for_reclamation: false,
            },
        );
        self.persist()
    }

    /// Promote a `Joining` replica to `Active`, which is what makes it
    /// required. Approval is the moment history starts being retained on its
    /// behalf, so it is explicit.
    pub fn approve_replica(&mut self, replica_id: &str) -> Result<(), MembershipError> {
        let cluster = self.cluster_mut()?;
        let replica = cluster
            .replicas
            .get_mut(replica_id)
            .ok_or_else(|| MembershipError::UnknownReplica(replica_id.to_string()))?;
        if replica.status != ReplicaStatus::Joining {
            return Err(MembershipError::NotJoining(replica_id.to_string()));
        }
        replica.status = ReplicaStatus::Active;
        replica.required_for_reclamation = true;
        self.persist()
    }

    /// Record that a replica looks unreachable.
    ///
    /// This does **not** change `required_for_reclamation`, and that is the
    /// whole point of the status existing separately from removal.
    pub fn suspect_replica(&mut self, replica_id: &str) -> Result<(), MembershipError> {
        let cluster = self.cluster_mut()?;
        let replica = cluster
            .replicas
            .get_mut(replica_id)
            .ok_or_else(|| MembershipError::UnknownReplica(replica_id.to_string()))?;
        if replica.status == ReplicaStatus::Removed {
            // A removed replica is not suspected; it is gone.
            return Ok(());
        }
        replica.status = ReplicaStatus::Suspected;
        self.persist()
    }

    /// Record contact with a replica.
    ///
    /// Reinstates a `Suspected` replica, because suspicion is an observation
    /// and this is a contrary observation. It never resurrects a `Removed`
    /// one: removal is a decision, and only an explicit join reverses it.
    pub fn mark_seen(&mut self, replica_id: &str) -> Result<(), MembershipError> {
        let cluster = self.cluster_mut()?;
        let replica = cluster
            .replicas
            .get_mut(replica_id)
            .ok_or_else(|| MembershipError::UnknownReplica(replica_id.to_string()))?;
        replica.last_seen_at_ms = Some(now_ms());
        if replica.status == ReplicaStatus::Suspected {
            replica.status = ReplicaStatus::Active;
        }
        self.persist()
    }

    /// Remove a replica from the cluster. The only operation that takes a
    /// replica out of the reclamation set.
    pub fn remove_replica(&mut self, replica_id: &str) -> Result<(), MembershipError> {
        let cluster = self.cluster_mut()?;
        let replica = cluster
            .replicas
            .get_mut(replica_id)
            .ok_or_else(|| MembershipError::UnknownReplica(replica_id.to_string()))?;
        replica.status = ReplicaStatus::Removed;
        replica.required_for_reclamation = false;
        self.persist()
    }

    /// Every replica the cluster currently has, removed ones excluded.
    pub fn get_members(&self) -> Vec<Replica> {
        self.cluster
            .as_ref()
            .map(|cluster| {
                cluster
                    .replicas
                    .values()
                    .filter(|replica| replica.status != ReplicaStatus::Removed)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every replica ever recorded, including removed ones, for audit.
    pub fn all_replicas(&self) -> Vec<Replica> {
        self.cluster
            .as_ref()
            .map(|cluster| cluster.replicas.values().cloned().collect())
            .unwrap_or_default()
    }

    /// The replicas whose acknowledgement is required before history may be
    /// reclaimed on their behalf.
    ///
    /// Read straight from the stored flag. Nothing about connectivity, recency,
    /// or this process's configuration enters into it.
    pub fn get_required_replicas(&self) -> Vec<Replica> {
        self.cluster
            .as_ref()
            .map(|cluster| {
                cluster
                    .replicas
                    .values()
                    .filter(|replica| {
                        replica.required_for_reclamation && replica.status != ReplicaStatus::Removed
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &tempfile::TempDir) -> MembershipStore {
        MembershipStore::open(dir.path().join("membership.json")).expect("open")
    }

    fn ids(replicas: &[Replica]) -> Vec<String> {
        let mut out: Vec<String> = replicas.iter().map(|r| r.replica_id.clone()).collect();
        out.sort();
        out
    }

    // -- the headline acceptance test -------------------------------------

    #[test]
    fn membership_survives_restart_and_only_removal_changes_the_reclamation_set() {
        let dir = tempfile::tempdir().unwrap();

        {
            let mut cluster = store(&dir);
            cluster.create_cluster("c1").expect("create");
            for (replica, node) in [("ra", "A"), ("rb", "B"), ("rc", "C")] {
                cluster.join_replica(replica, node).expect("join");
                cluster.approve_replica(replica).expect("approve");
            }
            assert_eq!(ids(&cluster.get_required_replicas()), ["ra", "rb", "rc"]);
        }

        // Restart: a new store over the same path, with no configuration
        // passed to it at all.
        let mut restarted = store(&dir);
        assert_eq!(
            ids(&restarted.get_members()),
            ["ra", "rb", "rc"],
            "membership is durable, not reconstructed from how the process was launched"
        );
        assert_eq!(restarted.cluster().expect("cluster").cluster_id, "c1");

        // C goes offline. It is suspected, and it stays required.
        restarted.suspect_replica("rc").expect("suspect");
        let suspected = restarted
            .get_members()
            .into_iter()
            .find(|r| r.replica_id == "rc")
            .expect("rc");
        assert_eq!(suspected.status, ReplicaStatus::Suspected);
        assert!(suspected.required_for_reclamation);
        assert_eq!(
            ids(&restarted.get_required_replicas()),
            ["ra", "rb", "rc"],
            "an offline replica is still owed its history: being unreachable is not consent"
        );

        // Only an explicit removal changes it.
        restarted.remove_replica("rc").expect("remove");
        assert_eq!(
            ids(&restarted.get_required_replicas()),
            ["ra", "rb"],
            "removal is the only thing that takes a replica out of the reclamation set"
        );
        assert_eq!(
            ids(&restarted.get_members()),
            ["ra", "rb"],
            "and out of the membership"
        );

        // And that decision is durable too.
        let after_removal = store(&dir);
        assert_eq!(ids(&after_removal.get_required_replicas()), ["ra", "rb"]);
        assert_eq!(
            after_removal.all_replicas().len(),
            3,
            "the removed replica is retained for audit, not erased"
        );
    }

    // -- the critical rule, from several directions ------------------------

    #[test]
    fn membership_a_stale_last_seen_never_changes_what_is_required() {
        // There is no code path that derives `required_for_reclamation` from
        // liveness, and this asserts the absence: a replica untouched since
        // the epoch is still required.
        let dir = tempfile::tempdir().unwrap();
        let mut cluster = store(&dir);
        cluster.create_cluster("c1").unwrap();
        cluster.join_replica("rc", "C").unwrap();
        cluster.approve_replica("rc").unwrap();

        cluster.suspect_replica("rc").unwrap();
        let ancient = cluster.all_replicas().into_iter().next().unwrap();
        assert_eq!(ancient.last_seen_at_ms, None, "never seen at all");
        assert!(
            ancient.required_for_reclamation,
            "a replica that has never been seen is still required: silence is not removal"
        );
        assert_eq!(ids(&cluster.get_required_replicas()), ["rc"]);
    }

    #[test]
    fn membership_suspicion_is_reversible_and_removal_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let mut cluster = store(&dir);
        cluster.create_cluster("c1").unwrap();
        cluster.join_replica("rc", "C").unwrap();
        cluster.approve_replica("rc").unwrap();

        cluster.suspect_replica("rc").unwrap();
        cluster.mark_seen("rc").unwrap();
        let seen = cluster.get_members().into_iter().next().unwrap();
        assert_eq!(
            seen.status,
            ReplicaStatus::Active,
            "contact reinstates a suspected replica"
        );
        assert!(seen.last_seen_at_ms.is_some());

        cluster.remove_replica("rc").unwrap();
        cluster.mark_seen("rc").unwrap();
        let after = cluster.all_replicas().into_iter().next().unwrap();
        assert_eq!(
            after.status,
            ReplicaStatus::Removed,
            "contact does NOT resurrect a removed replica: removal is a decision, not an observation"
        );
        assert!(!after.required_for_reclamation);
        assert!(cluster.get_required_replicas().is_empty());
    }

    #[test]
    fn membership_a_joining_replica_is_a_member_but_not_yet_required() {
        // A replica being seeded holds nothing anyone is waiting on, so it does
        // not pin history. Approval is the moment retention starts on its
        // behalf, which is why approval is explicit.
        let dir = tempfile::tempdir().unwrap();
        let mut cluster = store(&dir);
        cluster.create_cluster("c1").unwrap();
        cluster.join_replica("rd", "D").unwrap();

        assert_eq!(ids(&cluster.get_members()), ["rd"]);
        assert!(cluster.get_required_replicas().is_empty());

        cluster.approve_replica("rd").unwrap();
        assert_eq!(ids(&cluster.get_required_replicas()), ["rd"]);
    }

    #[test]
    fn membership_a_removed_replica_rejoins_by_joining_not_by_appearing() {
        let dir = tempfile::tempdir().unwrap();
        let mut cluster = store(&dir);
        cluster.create_cluster("c1").unwrap();
        cluster.join_replica("rc", "C").unwrap();
        cluster.approve_replica("rc").unwrap();
        cluster.remove_replica("rc").unwrap();

        // Its identity is still recorded, so a re-join is a deliberate act
        // rather than an accident of reconnecting.
        assert!(matches!(
            cluster.join_replica("rc", "C"),
            Err(MembershipError::ReplicaExists(_))
        ));
        assert!(cluster.get_required_replicas().is_empty());
    }

    // -- configured peer is not a cluster member ---------------------------

    #[test]
    fn membership_is_empty_until_someone_creates_it() {
        // A node with peers configured has no members. This is the separation
        // the ADR found missing: connection configuration says nothing about
        // who is owed history.
        let dir = tempfile::tempdir().unwrap();
        let cluster = store(&dir);
        assert!(cluster.cluster().is_none());
        assert!(cluster.get_members().is_empty());
        assert!(cluster.get_required_replicas().is_empty());
    }

    #[test]
    fn membership_operations_need_a_cluster() {
        let dir = tempfile::tempdir().unwrap();
        let mut cluster = store(&dir);
        assert!(matches!(
            cluster.join_replica("ra", "A"),
            Err(MembershipError::NoCluster)
        ));
        cluster.create_cluster("c1").unwrap();
        assert!(matches!(
            cluster.create_cluster("c2"),
            Err(MembershipError::ClusterExists(_))
        ));
    }

    #[test]
    fn membership_approval_requires_joining_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut cluster = store(&dir);
        cluster.create_cluster("c1").unwrap();
        assert!(matches!(
            cluster.approve_replica("ghost"),
            Err(MembershipError::UnknownReplica(_))
        ));
        cluster.join_replica("ra", "A").unwrap();
        cluster.approve_replica("ra").unwrap();
        assert!(
            matches!(
                cluster.approve_replica("ra"),
                Err(MembershipError::NotJoining(_))
            ),
            "approving twice is not a silent no-op"
        );
    }

    // -- replication membership is not authorization membership ------------

    #[test]
    fn membership_is_not_the_authorization_membership_of_the_same_name() {
        // The ADR found that `membership` in this codebase meant users and
        // organisations, in policy_evaluation.rs, and that replication had no
        // membership at all. These are different concepts with the same word,
        // and the failure mode is that one silently starts standing in for the
        // other.
        //
        // The behavioural separation: this store's contents are exactly what
        // was explicitly joined here. Authorization records live in the
        // application's own collections and cannot reach this file -- there is
        // no read path from one to the other, and the reclamation set below is
        // unchanged by any amount of authorization data.
        let dir = tempfile::tempdir().unwrap();
        let mut cluster = store(&dir);
        cluster.create_cluster("c1").unwrap();
        cluster.join_replica("ra", "A").unwrap();
        cluster.approve_replica("ra").unwrap();

        // An application's authorization membership: a user in an organisation.
        // Written where application state lives, which is a different place
        // entirely.
        let mut application_state = crate::state_hash::CanonicalState::new();
        application_state.set_record(
            "memberships".to_string(),
            "user-1:org-1".to_string(),
            serde_json::json!({ "user_id": "user-1", "organization_id": "org-1", "role": "admin" }),
        );
        assert_eq!(application_state.records.len(), 1);

        assert_eq!(
            ids(&cluster.get_required_replicas()),
            ["ra"],
            "the reclamation set is what was joined as a replica, and nothing else"
        );
        let reopened = store(&dir);
        assert_eq!(ids(&reopened.get_required_replicas()), ["ra"]);
        assert!(
            reopened
                .all_replicas()
                .iter()
                .all(|replica| replica.node_id == "A"),
            "no authorization principal became a replica"
        );
    }

    // -- durability and concurrency ----------------------------------------

    #[test]
    fn membership_every_transition_is_durable_immediately() {
        // Not just at shutdown: each operation persists, because the process
        // that is about to be killed is the one whose membership matters.
        let dir = tempfile::tempdir().unwrap();
        let mut cluster = store(&dir);
        cluster.create_cluster("c1").unwrap();
        assert_eq!(store(&dir).cluster().expect("cluster").cluster_id, "c1");

        cluster.join_replica("ra", "A").unwrap();
        assert_eq!(ids(&store(&dir).get_members()), ["ra"]);

        cluster.approve_replica("ra").unwrap();
        assert_eq!(ids(&store(&dir).get_required_replicas()), ["ra"]);

        cluster.suspect_replica("ra").unwrap();
        assert_eq!(
            ids(&store(&dir).get_required_replicas()),
            ["ra"],
            "suspicion is durable and still does not change what is required"
        );

        cluster.remove_replica("ra").unwrap();
        assert!(store(&dir).get_required_replicas().is_empty());
    }

    #[test]
    fn membership_concurrent_transitions_do_not_lose_updates() {
        use std::sync::{Arc, Mutex};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("membership.json");
        let shared = Arc::new(Mutex::new(MembershipStore::open(&path).unwrap()));
        shared.lock().unwrap().create_cluster("c1").unwrap();

        let mut threads = Vec::new();
        for index in 0..8 {
            let shared = Arc::clone(&shared);
            threads.push(std::thread::spawn(move || {
                let replica = format!("r{index}");
                let mut guard = shared.lock().unwrap();
                guard.join_replica(&replica, &format!("N{index}")).unwrap();
                guard.approve_replica(&replica).unwrap();
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }

        assert_eq!(shared.lock().unwrap().get_required_replicas().len(), 8);

        // And the file on disk agrees: no interleaved write left it torn or
        // holding a partial set.
        let reloaded = MembershipStore::open(&path).unwrap();
        assert_eq!(
            reloaded.get_required_replicas().len(),
            8,
            "the durable record holds every transition, not the last writer's view"
        );
    }

    #[test]
    fn membership_a_torn_write_cannot_be_observed() {
        // The write is temp-file plus rename, so a reader either sees the
        // previous cluster or the next one. This asserts the temp file is not
        // left behind as a readable half-state at the real path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("membership.json");
        let mut cluster = MembershipStore::open(&path).unwrap();
        cluster.create_cluster("c1").unwrap();
        for index in 0..20 {
            cluster
                .join_replica(&format!("r{index}"), &format!("N{index}"))
                .unwrap();
            let observed = MembershipStore::open(&path).unwrap();
            assert_eq!(
                observed.get_members().len(),
                index + 1,
                "every observation is of a complete cluster"
            );
        }
        assert!(
            !path.with_extension("tmp").exists(),
            "no temp file is left at rest"
        );
    }
}
