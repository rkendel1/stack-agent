//! Durable operation identity.
//!
//! `(origin, sequence)` is an identity, not a counter. Receivers deduplicate on
//! it, so two operations must never be assigned the same one -- and that has to
//! hold across a process restart, which is where it did not.
//!
//! `dedup_bound_investigation.rs` established the failure: `feltdb_node` kept
//! the counter in memory and started it at 1 on every launch, while the causal
//! clock was rebuilt from the log. A restarted origin came back knowing its
//! causal position and not its identity position, re-issued identities its own
//! earlier operations already held, and its peers discarded the new writes as
//! duplicates -- with no error and no divergence signal.
//!
//! The authority is now the durable log, and specifically the log rather than
//! the causal clock. Those are **different identity domains**: the clock
//! component advances only on a successful apply, the sequence is burned by an
//! attempt, and they disagree by exactly the number of refused transactions.
//! `replication-convergence.md` fixes that the sequence is sparse and that
//! making it dense is a separate deferred decision, so nothing here derives
//! identity from the clock and nothing here makes the sequence dense.
//!
//! The invariant, stated precisely, because the weaker version is not enough:
//!
//! > no two **persisted** envelopes from one origin ever share a sequence.
//!
//! Not "restart usually finds the highest sequence". An identity is reserved
//! forever exactly when an envelope bearing it becomes durable, which is why a
//! number burned and then lost to a crash is safe to re-issue -- nothing
//! carrying it was ever written.

#[cfg(test)]
mod tests {
    use crate::convergence::VectorClock;
    use crate::distributed_transactions::{
        DistributedTransactionExecutor, ReceiveOutcome, ReplicationMessage, SubmitOutcome,
        TransactionEnvelope,
    };
    use crate::state_hash::StateHash;
    use crate::transactions::{
        ConsistencyContract, Operation, OperationCommand, OperationId, StateVersion,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn zero_version() -> StateVersion {
        StateVersion::new(VectorClock::new(), "0".repeat(64))
    }

    fn operation_at(node: &str, sequence: u64, payload: &str) -> Operation {
        let mut fields = HashMap::new();
        fields.insert("record".to_string(), serde_json::json!(payload));
        Operation::new(
            OperationId::new(node.to_string(), sequence),
            zero_version(),
            format!("tx_{payload}"),
            OperationCommand {
                op_type: "set".to_string(),
                collection: "items".to_string(),
                record_id: payload.to_string(),
                fields,
            },
            node.to_string(),
        )
    }

    /// Open a node over a log path, replaying it when it already exists --
    /// which is what the node binary does on start.
    fn open(node: &str, path: &PathBuf) -> DistributedTransactionExecutor {
        let mut executor = DistributedTransactionExecutor::with_log(
            node.to_string(),
            StateHash::from_hex("0".repeat(64)),
            Some(path.clone()),
        )
        .expect("executor");
        if path.exists() {
            executor.load_from_disk(path.clone()).expect("recover");
        }
        executor
    }

    /// Commit as the node binary does: chain the parent version forward on
    /// success, and give each operation a distinct id.
    ///
    /// The operation id is taken from the executor's next identity, which is
    /// exactly what the binary does now that the executor owns allocation.
    fn commit(
        executor: &mut DistributedTransactionExecutor,
        node: &str,
        payload: &str,
    ) -> TransactionEnvelope {
        let sequence = executor.next_origin_sequence();
        let parent = chained_version(executor, node);
        match executor
            .submit_local_transaction(
                format!("tx_{payload}"),
                parent,
                vec![operation_at(node, sequence, payload)],
                ConsistencyContract::local(),
            )
            .expect("submit")
        {
            SubmitOutcome::Admitted(envelope) => envelope,
            SubmitOutcome::CapacityExceeded { .. } => panic!("unexpected capacity refusal"),
        }
    }

    /// The parent version this node would commit on top of.
    fn chained_version(executor: &DistributedTransactionExecutor, node: &str) -> StateVersion {
        match executor.get_replica_state(node) {
            Some(replica) => {
                StateVersion::new(replica.vector_clock.clone(), replica.state_hash.to_string())
            }
            None => zero_version(),
        }
    }

    fn message(envelope: TransactionEnvelope) -> ReplicationMessage {
        let origin = envelope.originating_node.clone();
        let sequence = envelope.envelope_id.sequence;
        ReplicationMessage::new(envelope, origin, "receiver".to_string(), sequence)
    }

    // -- allocation --------------------------------------------------------

    #[test]
    fn identity_a_fresh_origin_starts_at_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut node = open("A", &dir.path().join("operations.jsonl"));
        assert_eq!(
            node.next_origin_sequence(),
            1,
            "a brand-new origin legitimately starts at 1"
        );
        assert_eq!(commit(&mut node, "A", "first").envelope_id.sequence, 1);
    }

    #[test]
    fn identity_restart_after_three_operations_continues_at_four() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("operations.jsonl");
        {
            let mut node = open("A", &path);
            for payload in ["one", "two", "three"] {
                commit(&mut node, "A", payload);
            }
            assert_eq!(node.next_origin_sequence(), 4);
        }
        let restarted = open("A", &path);
        assert_eq!(
            restarted.next_origin_sequence(),
            4,
            "identity is recovered from the durable log, not reset"
        );
    }

    #[test]
    fn identity_is_recovered_even_without_replaying_the_log() {
        // `with_log` recovers identity too, so a caller that opens an existing
        // log and issues an operation without calling load_from_disk still
        // cannot re-issue an identity that log contains.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("operations.jsonl");
        {
            let mut node = open("A", &path);
            commit(&mut node, "A", "one");
            commit(&mut node, "A", "two");
        }
        let reopened = DistributedTransactionExecutor::with_log(
            "A".to_string(),
            StateHash::from_hex("0".repeat(64)),
            Some(path),
        )
        .expect("executor");
        assert_eq!(reopened.next_origin_sequence(), 3);
    }

    #[test]
    fn identity_spaces_are_independent_per_origin() {
        // A receiver that has applied another node's high sequences must not
        // inflate its own identity space: the log holds replicated envelopes
        // too, and another node's sequence says nothing about this one's.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("operations.jsonl");
        let mut receiver = open("receiver", &path);

        for position in 1..=9u64 {
            let mut envelope = TransactionEnvelope::new(
                "A".to_string(),
                position,
                format!("tx_a{position}"),
                vec![operation_at("A", position, &format!("a{position}"))],
                ConsistencyContract::local(),
            );
            let mut clock = VectorClock::new();
            clock.clocks.insert("A".to_string(), position);
            envelope.vector_clock = clock;
            receiver
                .receive_replicated(message(envelope), zero_version())
                .expect("receive");
        }

        assert_eq!(
            receiver.next_origin_sequence(),
            1,
            "nine applied envelopes from A leave this node's own identity space untouched"
        );
        assert_eq!(
            commit(&mut receiver, "receiver", "mine")
                .envelope_id
                .sequence,
            1
        );

        let restarted = open("receiver", &path);
        assert_eq!(
            restarted.next_origin_sequence(),
            2,
            "and recovery filters by origin too"
        );
    }

    // -- crash boundaries --------------------------------------------------

    #[test]
    fn identity_survives_a_crash_after_the_operation_became_durable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("operations.jsonl");
        {
            let mut node = open("A", &path);
            commit(&mut node, "A", "durable");
            // The process dies here: no clean shutdown, no flush of any
            // in-memory counter. The envelope is already fsynced.
        }
        let restarted = open("A", &path);
        assert_eq!(
            restarted.next_origin_sequence(),
            2,
            "the identity of a durable operation is never re-issued"
        );
    }

    #[test]
    fn identity_burned_without_persistence_is_safely_reissued_after_a_crash() {
        // allocate -> crash before persistence -> restart.
        //
        // The number is re-issued, and that is correct rather than a lapse: no
        // envelope bearing it was ever written, so nothing can collide with it.
        // Reserving it forever would leak identity on every refused
        // transaction and would make the durable sequence advance past
        // operations that never existed.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("operations.jsonl");
        {
            let mut node = open("A", &path);
            commit(&mut node, "A", "one");
            let burned = node.allocate_origin_sequence();
            assert_eq!(burned, 2, "claimed but never persisted");
        }
        let mut restarted = open("A", &path);
        assert_eq!(
            restarted.next_origin_sequence(),
            2,
            "the log holds only sequence 1, so 2 is free and is re-issued"
        );
        assert_eq!(commit(&mut restarted, "A", "two").envelope_id.sequence, 2);
    }

    #[test]
    fn identity_allocation_stays_sparse_because_the_frozen_contract_says_so() {
        // A burned number leaves a gap within a single process lifetime.
        // Making the sequence dense is a separate deferred decision
        // (replication-convergence.md) and is deliberately not made here.
        let dir = tempfile::tempdir().unwrap();
        let mut node = open("A", &dir.path().join("operations.jsonl"));
        commit(&mut node, "A", "one");
        let _burned = node.allocate_origin_sequence();
        assert_eq!(
            commit(&mut node, "A", "three").envelope_id.sequence,
            3,
            "the attempt burned 2; the sequence is sparse, as the contract fixes it"
        );
    }

    #[test]
    fn identity_is_not_burned_by_a_capacity_refusal() {
        // A refusal admits nothing, so it must not consume an identity either:
        // there is no operation for that number to account for.
        use crate::distributed_transactions::CausalCapacity;
        let dir = tempfile::tempdir().unwrap();
        let mut node = open("A", &dir.path().join("operations.jsonl"));
        node.set_causal_capacity(CausalCapacity::new(0, usize::MAX));

        let before = node.next_origin_sequence();
        let outcome = node
            .submit_local_transaction(
                "tx_refused".to_string(),
                zero_version(),
                vec![operation_at("A", 1, "refused")],
                ConsistencyContract::local(),
            )
            .expect("submit");
        assert!(matches!(outcome, SubmitOutcome::CapacityExceeded { .. }));
        assert_eq!(
            node.next_origin_sequence(),
            before,
            "a refusal consumes no identity"
        );
    }

    #[test]
    fn identity_holds_across_many_restart_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("operations.jsonl");
        let mut issued = Vec::new();
        for cycle in 0..25 {
            let mut node = open("A", &path);
            for index in 0..4 {
                issued.push(
                    commit(&mut node, "A", &format!("c{cycle}_{index}"))
                        .envelope_id
                        .sequence,
                );
            }
        }
        let mut unique = issued.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            issued.len(),
            "100 operations across 25 restart cycles produced no repeated identity"
        );
        assert_eq!(issued.len(), 100);
    }

    // -- the regression, at executor level ---------------------------------

    #[test]
    fn identity_a_receiver_accepts_writes_from_an_origin_that_restarted() {
        // The exact failure #210 found, now at the executor boundary.
        let origin_dir = tempfile::tempdir().unwrap();
        let origin_path = origin_dir.path().join("operations.jsonl");
        let receiver_dir = tempfile::tempdir().unwrap();
        let mut receiver = open("receiver", &receiver_dir.path().join("operations.jsonl"));

        let before = {
            let mut origin = open("A", &origin_path);
            commit(&mut origin, "A", "before")
        };
        assert!(matches!(
            receiver
                .receive_replicated(message(before), zero_version())
                .expect("receive"),
            ReceiveOutcome::Applied { .. }
        ));

        // The origin restarts and commits again.
        let after = {
            let mut origin = open("A", &origin_path);
            commit(&mut origin, "A", "after")
        };
        assert_eq!(
            after.envelope_id.sequence, 2,
            "the identity did not restart"
        );

        let outcome = receiver
            .receive_replicated(message(after), zero_version())
            .expect("receive");
        assert!(
            matches!(outcome, ReceiveOutcome::Applied { .. }),
            "a write from a restarted origin must be applied, not discarded: {outcome:?}"
        );
        assert_eq!(
            receiver.causal_frontier().get("A"),
            2,
            "and the frontier advances"
        );

        let state = receiver.canonical_state().expect("state");
        assert!(state
            .records
            .contains_key(&("items".to_string(), "after".to_string())));
        assert!(state
            .records
            .contains_key(&("items".to_string(), "before".to_string())));
    }

    #[test]
    fn identity_a_genuine_retransmission_is_still_refused() {
        // The fix must not weaken deduplication: the same envelope twice is
        // still one operation, and a *new* operation after it is still new.
        let origin_dir = tempfile::tempdir().unwrap();
        let receiver_dir = tempfile::tempdir().unwrap();
        let mut origin = open("A", &origin_dir.path().join("operations.jsonl"));
        let mut receiver = open("receiver", &receiver_dir.path().join("operations.jsonl"));

        let first = commit(&mut origin, "A", "one");
        receiver
            .receive_replicated(message(first.clone()), zero_version())
            .expect("receive");
        assert_eq!(
            receiver
                .receive_replicated(message(first), zero_version())
                .expect("receive"),
            ReceiveOutcome::AlreadyKnown,
            "a retransmission is refused"
        );

        let second = commit(&mut origin, "A", "two");
        assert!(
            matches!(
                receiver
                    .receive_replicated(message(second), zero_version())
                    .expect("receive"),
                ReceiveOutcome::Applied { .. }
            ),
            "and a new operation after a duplicate is still accepted"
        );
        assert_eq!(receiver.causal_frontier().get("A"), 2);
    }

    #[test]
    fn identity_no_collision_when_the_origin_restarts_while_the_receiver_holds_a_backlog() {
        // The two mechanisms have to compose: the receiver is holding causal
        // backlog from this origin when the origin restarts, so a colliding
        // identity would land on a pending entry rather than an applied one.
        let origin_dir = tempfile::tempdir().unwrap();
        let origin_path = origin_dir.path().join("operations.jsonl");
        let receiver_dir = tempfile::tempdir().unwrap();
        let mut receiver = open("receiver", &receiver_dir.path().join("operations.jsonl"));

        let (first, second) = {
            let mut origin = open("A", &origin_path);
            (
                commit(&mut origin, "A", "one"),
                commit(&mut origin, "A", "two"),
            )
        };

        // Only the second is delivered, so it is held waiting for the first.
        assert!(matches!(
            receiver
                .receive_replicated(message(second), zero_version())
                .expect("receive"),
            ReceiveOutcome::PendingDependencies { .. }
        ));
        assert_eq!(receiver.pending_causal_count(), 1);

        // The origin restarts, with the receiver still holding that backlog.
        let third = {
            let mut origin = open("A", &origin_path);
            commit(&mut origin, "A", "three")
        };
        assert_eq!(
            third.envelope_id.sequence, 3,
            "no collision with the held entry"
        );

        let held = receiver
            .receive_replicated(message(third), zero_version())
            .expect("receive");
        assert!(
            matches!(held, ReceiveOutcome::PendingDependencies { .. }),
            "it is held on its own merits, not mistaken for the entry already pending: {held:?}"
        );
        assert_eq!(receiver.pending_causal_count(), 2);

        // The gap closes and everything applies exactly once.
        receiver
            .receive_replicated(message(first), zero_version())
            .expect("receive");
        assert_eq!(receiver.pending_causal_count(), 0);
        assert_eq!(receiver.causal_frontier().get("A"), 3);
        let state = receiver.canonical_state().expect("state");
        for record in ["one", "two", "three"] {
            assert!(state
                .records
                .contains_key(&("items".to_string(), record.to_string())));
        }
    }
}
