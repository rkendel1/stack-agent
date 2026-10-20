//! The causal backlog bound, exercised through the production receive path.
//!
//! Every test here drives `DistributedTransactionExecutor::receive_replicated`
//! and `try_execute_local_transaction` -- the entry points the shipping node
//! binary calls. None of them constructs a `CausalDependencyBarrier` and fills
//! it directly.
//!
//! That restriction is the point, and it is written down because the previous
//! evidence for this property did exactly the opposite: Gate 14B built a
//! barrier, filled it, computed `backlog_depth * 1500 / 1024`, and reported the
//! result as a memory measurement. It found linear growth because linear growth
//! was the arithmetic it had been given. A new harness that also constructed a
//! barrier would repeat that defect in a fresh file, so these tests reach the
//! backlog the way an operation reaches it.
//!
//! What is asserted is behaviour at the bound, not a predicted footprint. Where
//! a number is reported it is one that was observed.
//!
//! The contract under test is `docs/architecture/causal-backlog-bound.md` §4.

#[cfg(test)]
mod tests {
    use crate::convergence::VectorClock;
    use crate::distributed_transactions::{
        CausalCapacity, DistributedTransactionExecutor, ReceiveOutcome, ReplicationMessage,
        SubmitOutcome, TransactionEnvelope,
    };
    use crate::state_hash::StateHash;
    use crate::transactions::{
        ConsistencyContract, Operation, OperationCommand, OperationId, StateVersion,
    };
    use std::collections::HashMap;

    fn zero_version() -> StateVersion {
        StateVersion::new(VectorClock::new(), "0".repeat(64))
    }

    /// An envelope from `origin` at `sequence`, carrying the causal history
    /// `{origin: sequence}` -- so its dependency is `{origin: sequence - 1}`,
    /// exactly as the production stamping produces.
    fn envelope_at(origin: &str, sequence: u64, payload: &str) -> TransactionEnvelope {
        let mut fields = HashMap::new();
        fields.insert("record".to_string(), serde_json::json!(payload));
        let operation = Operation::new(
            OperationId::new(origin.to_string(), sequence),
            zero_version(),
            format!("tx_{payload}"),
            OperationCommand {
                op_type: "set".to_string(),
                collection: "items".to_string(),
                record_id: payload.to_string(),
                fields,
            },
            origin.to_string(),
        );

        let mut envelope = TransactionEnvelope::new(
            origin.to_string(),
            sequence,
            format!("tx_{payload}"),
            vec![operation],
            ConsistencyContract::local(),
        );
        let mut clock = VectorClock::new();
        clock.clocks.insert(origin.to_string(), sequence);
        envelope.vector_clock = clock;
        envelope
    }

    fn message(envelope: TransactionEnvelope) -> ReplicationMessage {
        let origin = envelope.originating_node.clone();
        let sequence = envelope.envelope_id.sequence;
        ReplicationMessage::new(envelope, origin, "receiver".to_string(), sequence)
    }

    /// A receiver with a durable log, which every one of these tests needs:
    /// a deferred or pending envelope is replayed from the log, not from RAM.
    fn receiver(capacity: CausalCapacity) -> (DistributedTransactionExecutor, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut executor = DistributedTransactionExecutor::with_log(
            "receiver".to_string(),
            StateHash::from_hex("0".repeat(64)),
            Some(dir.path().join("operations.jsonl")),
        )
        .expect("executor with log");
        executor.set_causal_capacity(capacity);
        (executor, dir)
    }

    /// Deliver `count` envelopes that can never become eligible, because the
    /// dependency they name is never sent. This is how the backlog is filled
    /// through the real path.
    fn fill_with_blocked(
        executor: &mut DistributedTransactionExecutor,
        origin: &str,
        count: usize,
    ) -> Vec<ReceiveOutcome> {
        (0..count)
            .map(|index| {
                // Sequence starts at 2 so the dependency {origin: 1} is never
                // satisfied: envelope 1 is deliberately never delivered.
                let sequence = (index as u64) + 2;
                executor
                    .receive_replicated(
                        message(envelope_at(origin, sequence, &format!("r{sequence}"))),
                        zero_version(),
                    )
                    .expect("receive")
            })
            .collect()
    }

    #[test]
    fn causal_backlog_fills_to_just_under_the_entry_bound() {
        let (mut executor, _dir) = receiver(CausalCapacity::new(8, usize::MAX));
        let outcomes = fill_with_blocked(&mut executor, "A", 7);

        assert!(
            outcomes
                .iter()
                .all(|outcome| matches!(outcome, ReceiveOutcome::PendingDependencies { .. })),
            "everything below the bound is buffered"
        );
        assert_eq!(executor.pending_causal_count(), 7);
        assert_eq!(executor.deferred_causal_count(), 0);
    }

    #[test]
    fn causal_backlog_accepts_the_exact_entry_bound() {
        // The bound is inclusive: `<=`, not `<`. Filling exactly to it is
        // accepted, and the breach is the step past it.
        let (mut executor, _dir) = receiver(CausalCapacity::new(8, usize::MAX));
        let outcomes = fill_with_blocked(&mut executor, "A", 8);

        assert!(matches!(
            outcomes.last().expect("an outcome"),
            ReceiveOutcome::PendingDependencies { .. }
        ));
        assert_eq!(executor.pending_causal_count(), 8);
        assert_eq!(executor.deferred_causal_count(), 0);
    }

    #[test]
    fn causal_backlog_defers_one_entry_past_the_bound() {
        let (mut executor, _dir) = receiver(CausalCapacity::new(8, usize::MAX));
        let outcomes = fill_with_blocked(&mut executor, "A", 9);

        assert!(
            matches!(outcomes[8], ReceiveOutcome::CapacityDeferred { .. }),
            "the ninth entry breaches an eight-entry bound, got {:?}",
            outcomes[8]
        );
        assert_eq!(executor.pending_causal_count(), 8, "the bound holds");
        assert_eq!(executor.deferred_causal_count(), 1);
    }

    #[test]
    fn causal_backlog_defers_one_byte_past_the_byte_bound() {
        // Size the byte bound from a real envelope rather than a guessed
        // constant: measure what two envelopes actually occupy, then set the
        // bound one byte below it.
        let probe = envelope_at("A", 2, "r2");
        let probe_bytes = serde_json::to_string(&probe).expect("serialize").len();

        let (mut executor, _dir) = receiver(CausalCapacity::new(usize::MAX, probe_bytes * 2 - 1));
        let outcomes = fill_with_blocked(&mut executor, "A", 3);

        assert!(matches!(
            outcomes[0],
            ReceiveOutcome::PendingDependencies { .. }
        ));
        assert!(
            matches!(outcomes[1], ReceiveOutcome::CapacityDeferred { .. }),
            "the second envelope breaches a bound one byte short of two, got {:?}",
            outcomes[1]
        );
        assert_eq!(executor.pending_causal_count(), 1);
        assert!(executor.pending_causal_payload_bytes() <= probe_bytes * 2 - 1);
    }

    #[test]
    fn causal_backlog_charges_a_large_envelope_its_actual_bytes() {
        // A byte bound is only a bound if a big operation costs more than a
        // small one. Two envelopes of very different sizes, against a bound
        // that admits the small one and not the large one.
        let small = envelope_at("A", 2, "s");
        let large = envelope_at("A", 3, &"L".repeat(4096));
        let small_bytes = serde_json::to_string(&small).expect("serialize").len();
        let large_bytes = serde_json::to_string(&large).expect("serialize").len();
        assert!(
            large_bytes > small_bytes * 2,
            "the fixture must actually differ in size"
        );

        let (mut executor, _dir) = receiver(CausalCapacity::new(usize::MAX, small_bytes + 16));

        let first = executor
            .receive_replicated(message(small), zero_version())
            .expect("receive");
        assert!(matches!(first, ReceiveOutcome::PendingDependencies { .. }));

        let second = executor
            .receive_replicated(message(large), zero_version())
            .expect("receive");
        assert!(
            matches!(second, ReceiveOutcome::CapacityDeferred { .. }),
            "a large envelope must consume its actual byte count, got {second:?}"
        );
    }

    #[test]
    fn causal_backlog_refuses_local_work_at_the_bound_without_admitting_it() {
        let (mut executor, _dir) = receiver(CausalCapacity::new(4, usize::MAX));
        fill_with_blocked(&mut executor, "A", 4);
        assert_eq!(executor.pending_causal_count(), 4);

        let applied_before = executor
            .get_replica_state("receiver")
            .map(|replica| replica.operations_applied)
            .unwrap_or(0);

        let mut fields = HashMap::new();
        fields.insert("record".to_string(), serde_json::json!("local"));
        let operation = Operation::new(
            OperationId::new("receiver".to_string(), 1),
            zero_version(),
            "tx_local".to_string(),
            OperationCommand {
                op_type: "set".to_string(),
                collection: "items".to_string(),
                record_id: "local".to_string(),
                fields,
            },
            "receiver".to_string(),
        );

        let outcome = executor
            .try_execute_local_transaction(
                1,
                "tx_local".to_string(),
                zero_version(),
                vec![operation],
                ConsistencyContract::local(),
            )
            .expect("submit");

        match outcome {
            SubmitOutcome::CapacityExceeded {
                limit_entries,
                pending_entries,
                ..
            } => {
                assert_eq!(limit_entries, 4);
                assert_eq!(pending_entries, 4);
            }
            SubmitOutcome::Admitted(_) => panic!("local work must be refused at the bound"),
        }

        // Refused work never appears as applied, and never reaches the log.
        let applied_after = executor
            .get_replica_state("receiver")
            .map(|replica| replica.operations_applied)
            .unwrap_or(0);
        assert_eq!(applied_after, applied_before, "a refusal applies nothing");
        assert!(
            !executor
                .causal_barrier
                .pending_keys()
                .iter()
                .any(|key| key.starts_with("receiver:")),
            "a refusal buffers nothing"
        );
    }

    #[test]
    fn causal_backlog_never_evicts_and_admits_deferred_work_when_it_drains() {
        // The whole point of deferring rather than refusing: nothing is lost.
        // Once the dependency arrives and the backlog drains, the deferred
        // envelope is admitted from the log and applied.
        let (mut executor, _dir) = receiver(CausalCapacity::new(2, usize::MAX));

        let outcomes = fill_with_blocked(&mut executor, "A", 3);
        assert!(matches!(
            outcomes[2],
            ReceiveOutcome::CapacityDeferred { .. }
        ));
        assert_eq!(executor.deferred_causal_count(), 1);

        // Deliver the missing dependency A:1, which releases the whole chain.
        let released = executor
            .receive_replicated(message(envelope_at("A", 1, "r1")), zero_version())
            .expect("receive");
        assert!(matches!(released, ReceiveOutcome::Applied { .. }));

        assert_eq!(
            executor.pending_causal_count(),
            0,
            "nothing is left waiting"
        );
        assert_eq!(
            executor.deferred_causal_count(),
            0,
            "nothing is left deferred"
        );
        let applied = executor
            .get_replica_state("receiver")
            .map(|replica| replica.operations_applied)
            .unwrap_or(0);
        assert_eq!(applied, 4, "all four envelopes applied, none evicted");
    }

    #[test]
    fn causal_backlog_holds_order_under_pressure() {
        // Causal correctness is not traded away at capacity: nothing is applied
        // before its dependencies, whatever the bound is doing.
        let (mut executor, _dir) = receiver(CausalCapacity::new(2, usize::MAX));
        fill_with_blocked(&mut executor, "A", 4);

        let applied = executor
            .get_replica_state("receiver")
            .map(|replica| replica.operations_applied)
            .unwrap_or(0);
        assert_eq!(
            applied, 0,
            "nothing may be applied while its dependency is missing"
        );
        assert_eq!(executor.causal_frontier().get("A"), 0);
    }

    #[test]
    fn causal_backlog_deduplicates_a_redelivered_blocked_entry() {
        let (mut executor, _dir) = receiver(CausalCapacity::new(8, usize::MAX));

        let first = executor
            .receive_replicated(message(envelope_at("A", 2, "r2")), zero_version())
            .expect("receive");
        assert!(matches!(first, ReceiveOutcome::PendingDependencies { .. }));

        let again = executor
            .receive_replicated(message(envelope_at("A", 2, "r2")), zero_version())
            .expect("receive");
        assert_eq!(again, ReceiveOutcome::AlreadyKnown);
        assert_eq!(executor.pending_causal_count(), 1, "one logical operation");
    }

    #[test]
    fn causal_backlog_deduplicates_a_redelivered_deferred_entry() {
        // A deferred envelope is durable but unbuffered, so it needs its own
        // dedup answer: without one, a retransmission would append it to the
        // log a second time.
        let (mut executor, _dir) = receiver(CausalCapacity::new(1, usize::MAX));
        let outcomes = fill_with_blocked(&mut executor, "A", 2);
        assert!(matches!(
            outcomes[1],
            ReceiveOutcome::CapacityDeferred { .. }
        ));

        let again = executor
            .receive_replicated(message(envelope_at("A", 3, "r3")), zero_version())
            .expect("receive");
        assert_eq!(again, ReceiveOutcome::AlreadyKnown);
        assert_eq!(
            executor.deferred_causal_count(),
            1,
            "still one deferred entry"
        );
    }

    #[test]
    fn causal_backlog_reports_current_and_maximum_occupancy() {
        let (mut executor, _dir) = receiver(CausalCapacity::new(8, usize::MAX));
        fill_with_blocked(&mut executor, "A", 5);

        let (max_entries, max_bytes) = executor.max_causal_occupancy();
        assert_eq!(executor.pending_causal_count(), 5);
        assert_eq!(max_entries, 5);
        assert!(max_bytes > 0, "payload bytes are reported, not assumed");

        // Drain it, and the current gauge falls while the maximum does not --
        // which is the reason the maximum is reported at all.
        executor
            .receive_replicated(message(envelope_at("A", 1, "r1")), zero_version())
            .expect("receive");
        assert_eq!(executor.pending_causal_count(), 0);
        assert_eq!(
            executor.max_causal_occupancy().0,
            5,
            "a backlog that filled and drained between two reads must still be visible"
        );
    }

    #[test]
    fn causal_backlog_survives_restart_with_deferred_and_pending_entries() {
        // Durability is the property the bound must not cost. A blocked
        // envelope is fsynced before it is buffered, and a deferred one is
        // fsynced before it is deferred, so a restart rebuilds both from the
        // log rather than losing whichever the bound had excluded.
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("operations.jsonl");

        {
            let mut executor = DistributedTransactionExecutor::with_log(
                "receiver".to_string(),
                StateHash::from_hex("0".repeat(64)),
                Some(log_path.clone()),
            )
            .expect("executor");
            executor.set_causal_capacity(CausalCapacity::new(2, usize::MAX));
            let outcomes = fill_with_blocked(&mut executor, "A", 3);
            assert!(matches!(
                outcomes[2],
                ReceiveOutcome::CapacityDeferred { .. }
            ));
            assert_eq!(executor.pending_causal_count(), 2);
            assert_eq!(executor.deferred_causal_count(), 1);
        }

        // Restart: a fresh executor over the same log.
        let mut recovered = DistributedTransactionExecutor::with_log(
            "receiver".to_string(),
            StateHash::from_hex("0".repeat(64)),
            Some(log_path.clone()),
        )
        .expect("executor");
        recovered.set_causal_capacity(CausalCapacity::new(2, usize::MAX));
        let replayed = recovered.load_from_disk(log_path).expect("recover");

        assert_eq!(
            replayed, 3,
            "all three envelopes were durable before the restart"
        );
        assert_eq!(
            recovered.pending_causal_count(),
            2,
            "recovery honours the same bound the live path does"
        );
        assert_eq!(
            recovered.deferred_causal_count(),
            1,
            "the excess is deferred, not lost"
        );

        // And the dependency still releases everything after the restart.
        recovered
            .receive_replicated(message(envelope_at("A", 1, "r1")), zero_version())
            .expect("receive");
        assert_eq!(recovered.pending_causal_count(), 0);
        assert_eq!(recovered.deferred_causal_count(), 0);
        assert_eq!(
            recovered
                .get_replica_state("receiver")
                .map(|replica| replica.operations_applied)
                .unwrap_or(0),
            4,
            "every envelope that was durable before the crash is applied after it"
        );
    }

    #[test]
    fn causal_backlog_recovery_is_deterministic() {
        // Behaviour at capacity must not depend on iteration order. The same
        // log and the same bound must produce the same split twice.
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("operations.jsonl");

        {
            let mut executor = DistributedTransactionExecutor::with_log(
                "receiver".to_string(),
                StateHash::from_hex("0".repeat(64)),
                Some(log_path.clone()),
            )
            .expect("executor");
            executor.set_causal_capacity(CausalCapacity::new(10, usize::MAX));
            fill_with_blocked(&mut executor, "A", 6);
        }

        let split = |bound: usize| {
            let mut executor = DistributedTransactionExecutor::with_log(
                "receiver".to_string(),
                StateHash::from_hex("0".repeat(64)),
                Some(log_path.clone()),
            )
            .expect("executor");
            executor.set_causal_capacity(CausalCapacity::new(bound, usize::MAX));
            executor.load_from_disk(log_path.clone()).expect("recover");
            executor.causal_barrier.pending_keys()
        };

        assert_eq!(
            split(3),
            split(3),
            "the same log and bound recover identically"
        );
        assert_eq!(split(3).len(), 3);
    }
}
