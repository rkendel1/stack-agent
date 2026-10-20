//! Is `envelopes_seen` bounded by the causal backlog, or is it independent?
//!
//! An investigation, not a change. Nothing here modifies the implementation;
//! every test asserts what the code does today, including where that is wrong.
//!
//! `adrs/replication-retention-and-recovery-lag.md` §8 raised this as the
//! cheapest unresolved question under P5.3, and recorded it as a question
//! rather than a decision because it argues against a frozen contract. The
//! hypothesis was:
//!
//! > since causal enforcement landed, `frontier[origin] = n` may imply that
//! > every clock position `1..n` from that origin has been applied, in which
//! > case the frontier is a complete deduplication test for everything it
//! > covers and `envelopes_seen` need only hold what is above it -- which the
//! > backlog bound already bounds.
//!
//! The tests below establish the four steps that hypothesis needs, find that it
//! holds, and then find that the deduplication *key* did not -- it was unsound
//! across a restart of the originating node, and lost writes silently.
//!
//! That second finding has since been fixed: identity is now allocated by the
//! executor from the durable log, in `durable_operation_identity.rs`. The two
//! tests at the bottom are kept and still pass, because what they assert is the
//! *receiver's* behaviour when it is handed a colliding identity, and that has
//! deliberately not changed -- a receiver cannot tell a re-used identity from a
//! retransmission, which is precisely why the origin must never produce one.
//! They are the reason durable identity allocation is load-bearing rather than
//! tidy.
//!
//! Two counters are involved and the whole question turns on their difference:
//!
//! ```text
//!   envelope_id.sequence   caller-supplied; the node binary resets it to 1
//!                          on restart and never restores it from the log
//!   vector_clock[origin]   frontier.increment(origin) at stamp time; the
//!                          frontier IS rebuilt from the log on restart
//! ```

#[cfg(test)]
mod tests {
    use crate::convergence::VectorClock;
    use crate::distributed_transactions::{
        DistributedTransactionExecutor, ReceiveOutcome, ReplicationMessage, TransactionEnvelope,
    };
    use crate::state_hash::StateHash;
    use crate::transactions::{
        ConsistencyContract, Operation, OperationCommand, OperationId, StateVersion,
    };
    use std::collections::HashMap;

    fn zero_version() -> StateVersion {
        StateVersion::new(VectorClock::new(), "0".repeat(64))
    }

    /// Build an envelope with an explicitly chosen envelope sequence and an
    /// explicitly chosen causal clock, so the two counters can be moved
    /// independently -- which is exactly what a restart of the origin does.
    fn envelope(
        origin: &str,
        sequence: u64,
        clock: &[(&str, u64)],
        payload: &str,
    ) -> TransactionEnvelope {
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
        let mut built = TransactionEnvelope::new(
            origin.to_string(),
            sequence,
            format!("tx_{payload}"),
            vec![operation],
            ConsistencyContract::local(),
        );
        let mut vector_clock = VectorClock::new();
        for (node, value) in clock {
            vector_clock.clocks.insert((*node).to_string(), *value);
        }
        built.vector_clock = vector_clock;
        built
    }

    fn message(envelope: TransactionEnvelope) -> ReplicationMessage {
        let origin = envelope.originating_node.clone();
        let sequence = envelope.envelope_id.sequence;
        ReplicationMessage::new(envelope, origin, "receiver".to_string(), sequence)
    }

    fn receiver() -> (DistributedTransactionExecutor, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let executor = DistributedTransactionExecutor::with_log(
            "receiver".to_string(),
            StateHash::from_hex("0".repeat(64)),
            Some(dir.path().join("operations.jsonl")),
        )
        .expect("executor");
        (executor, dir)
    }

    // ---------------------------------------------------------------------
    // Step 1: the frontier only advances one position at a time, per origin.
    // ---------------------------------------------------------------------

    #[test]
    fn dedup_frontier_advances_exactly_one_position_per_applied_envelope() {
        let (mut executor, _dir) = receiver();
        for position in 1..=5u64 {
            let outcome = executor
                .receive_replicated(
                    message(envelope(
                        "A",
                        position,
                        &[("A", position)],
                        &format!("r{position}"),
                    )),
                    zero_version(),
                )
                .expect("receive");
            assert!(matches!(outcome, ReceiveOutcome::Applied { .. }));
            assert_eq!(
                executor.causal_frontier().get("A"),
                position,
                "the frontier tracks applied positions exactly, with no jumps"
            );
        }
    }

    // ---------------------------------------------------------------------
    // Step 2: a foreign envelope cannot raise the frontier for another origin.
    //
    // This is the step the hypothesis actually rests on. `record_applied` calls
    // `VectorClock::merge`, which takes the maximum per component, so an
    // envelope from B carrying {A: 5} would raise this replica's frontier for A
    // to 5 -- inventing knowledge of five A operations it never applied. Causal
    // enforcement is what prevents it: that envelope's dependencies include
    // A@5, so it cannot be applied until A@5 already has been, at which point
    // the merge is a no-op for A.
    // ---------------------------------------------------------------------

    #[test]
    fn dedup_a_foreign_envelope_cannot_invent_frontier_for_another_origin() {
        let (mut executor, _dir) = receiver();

        // B's envelope claims to have seen A through position 5.
        let outcome = executor
            .receive_replicated(
                message(envelope("B", 1, &[("A", 5), ("B", 1)], "b1")),
                zero_version(),
            )
            .expect("receive");

        assert!(
            matches!(outcome, ReceiveOutcome::PendingDependencies { .. }),
            "an envelope that depends on five unseen A operations must be held, got {outcome:?}"
        );
        assert_eq!(
            executor.causal_frontier().get("A"),
            0,
            "and it must not have advanced the frontier for A: merge takes a maximum, \
             so applying this would have invented knowledge of A@1..5"
        );
    }

    #[test]
    fn dedup_a_foreign_envelope_merge_is_a_noop_for_an_origin_already_covered() {
        let (mut executor, _dir) = receiver();
        for position in 1..=3u64 {
            executor
                .receive_replicated(
                    message(envelope(
                        "A",
                        position,
                        &[("A", position)],
                        &format!("a{position}"),
                    )),
                    zero_version(),
                )
                .expect("receive");
        }
        assert_eq!(executor.causal_frontier().get("A"), 3);

        // B has seen A through 3, which this replica has too.
        executor
            .receive_replicated(
                message(envelope("B", 1, &[("A", 3), ("B", 1)], "b1")),
                zero_version(),
            )
            .expect("receive");

        assert_eq!(
            executor.causal_frontier().get("A"),
            3,
            "applying a foreign envelope leaves the covered origin where it was"
        );
    }

    // ---------------------------------------------------------------------
    // Step 3: therefore the frontier covers exactly what was applied.
    // ---------------------------------------------------------------------

    #[test]
    fn dedup_every_applied_envelope_is_covered_by_the_frontier() {
        let (mut executor, _dir) = receiver();

        // Two origins, interleaved, plus an out-of-order arrival that is held
        // and later released -- so the applied set is not simply arrival order.
        executor
            .receive_replicated(message(envelope("A", 1, &[("A", 1)], "a1")), zero_version())
            .unwrap();
        executor
            .receive_replicated(message(envelope("A", 3, &[("A", 3)], "a3")), zero_version())
            .unwrap();
        executor
            .receive_replicated(message(envelope("A", 2, &[("A", 2)], "a2")), zero_version())
            .unwrap();
        executor
            .receive_replicated(
                message(envelope("B", 1, &[("A", 3), ("B", 1)], "b1")),
                zero_version(),
            )
            .unwrap();

        assert_eq!(
            executor.pending_causal_count(),
            0,
            "everything was released"
        );
        assert_eq!(executor.causal_frontier().get("A"), 3);
        assert_eq!(executor.causal_frontier().get("B"), 1);

        // The claim: for every envelope this replica considers seen, the
        // frontier already covers its clock position. So a frontier test would
        // answer "already applied" for all of them.
        let frontier = executor.causal_frontier().clone();
        let log = executor
            .operation_log
            .as_ref()
            .expect("log")
            .load_all()
            .expect("load");
        for envelope in &log {
            let origin = &envelope.originating_node;
            let position = envelope.vector_clock.get(origin);
            assert!(
                frontier.get(origin) >= position,
                "{origin}@{position} was applied but the frontier reports {}",
                frontier.get(origin)
            );
        }
        assert_eq!(log.len(), 4);
    }

    // ---------------------------------------------------------------------
    // Step 4: and it survives a restart, because the frontier is rebuilt from
    // the log while `envelopes_seen` is rebuilt from the same place.
    // ---------------------------------------------------------------------

    #[test]
    fn dedup_frontier_coverage_survives_restart_and_still_refuses_duplicates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("operations.jsonl");
        {
            let mut executor = DistributedTransactionExecutor::with_log(
                "receiver".to_string(),
                StateHash::from_hex("0".repeat(64)),
                Some(path.clone()),
            )
            .expect("executor");
            for position in 1..=3u64 {
                executor
                    .receive_replicated(
                        message(envelope(
                            "A",
                            position,
                            &[("A", position)],
                            &format!("a{position}"),
                        )),
                        zero_version(),
                    )
                    .expect("receive");
            }
        }

        let mut recovered = DistributedTransactionExecutor::with_log(
            "receiver".to_string(),
            StateHash::from_hex("0".repeat(64)),
            Some(path.clone()),
        )
        .expect("executor");
        recovered.load_from_disk(path).expect("recover");

        assert_eq!(
            recovered.causal_frontier().get("A"),
            3,
            "the frontier is rebuilt from the log"
        );

        // A retransmission of an already-applied operation after the restart.
        let again = recovered
            .receive_replicated(message(envelope("A", 2, &[("A", 2)], "a2")), zero_version())
            .expect("receive");
        assert_eq!(
            again,
            ReceiveOutcome::AlreadyKnown,
            "a duplicate after restart is refused"
        );
    }

    // ---------------------------------------------------------------------
    // Growth: does `envelopes_seen` grow independently of the bounded
    // pending/deferred population? It does, and that is the whole question.
    // ---------------------------------------------------------------------

    #[test]
    fn dedup_seen_set_grows_with_history_while_the_backlog_stays_empty() {
        let (mut executor, _dir) = receiver();
        for position in 1..=200u64 {
            executor
                .receive_replicated(
                    message(envelope(
                        "A",
                        position,
                        &[("A", position)],
                        &format!("a{position}"),
                    )),
                    zero_version(),
                )
                .expect("receive");
        }

        assert_eq!(
            executor.pending_causal_count(),
            0,
            "the bounded population is empty"
        );
        assert_eq!(executor.deferred_causal_count(), 0);
        assert_eq!(
            executor.envelopes_seen.len(),
            200,
            "yet the deduplication set holds one entry per operation ever applied: \
             it grows with history, not with the backlog, so the backlog bound does not bound it"
        );

        // And the frontier that could replace it is one integer per origin.
        assert_eq!(
            executor.causal_frontier().clocks.len(),
            1,
            "the frontier covering the same 200 operations is O(origins), not O(operations)"
        );
    }

    // ---------------------------------------------------------------------
    // Why identity must be durable: what a receiver does with a colliding one.
    //
    // These assert the receive path's behaviour when it is *handed* a re-used
    // identity. That behaviour is unchanged and correct in isolation -- a
    // receiver has no way to distinguish a re-used identity from a
    // retransmission. The defect was that origins produced such identities
    // after a restart, and that is fixed in durable_operation_identity.rs.
    // These remain as the demonstration of what that fix prevents.
    // ---------------------------------------------------------------------

    #[test]
    fn dedup_a_colliding_identity_is_dropped_silently_which_is_why_identity_must_be_durable() {
        // Deduplication keys on `(origin, sequence)`. Hand the receiver a
        // second operation carrying an identity it has already applied -- which
        // is what a restarted origin used to produce, before identity was
        // recovered from the durable log -- and it is discarded as a duplicate.
        //
        // The receiver is not wrong to do this. It cannot distinguish a re-used
        // identity from a retransmission, and treating retransmissions as
        // duplicates is a property the rest of the system depends on. The fix
        // belongs on the allocating side, and this is what it prevents.
        let (mut executor, _dir) = receiver();

        // Before the origin restarts: A's first operation, sequence 1, clock 1.
        let first = executor
            .receive_replicated(
                message(envelope("A", 1, &[("A", 1)], "before")),
                zero_version(),
            )
            .expect("receive");
        assert!(matches!(first, ReceiveOutcome::Applied { .. }));

        // A restarts. Its sequence counter resets to 1; its frontier is rebuilt
        // from its own log, so the next operation is clock position 2. This is
        // a genuinely new write, carrying different data.
        let after_restart = envelope("A", 1, &[("A", 2)], "after");
        let outcome = executor
            .receive_replicated(message(after_restart.clone()), zero_version())
            .expect("receive");

        assert_eq!(
            outcome,
            ReceiveOutcome::AlreadyKnown,
            "a write carrying an already-applied identity is discarded, silently"
        );
        assert_eq!(
            executor.causal_frontier().get("A"),
            1,
            "the receiver's frontier stalls, so every later A operation is blocked behind \
             a dependency that will never arrive"
        );
        let state = executor.canonical_state().expect("state");
        assert!(
            !state
                .records
                .contains_key(&("items".to_string(), "after".to_string())),
            "and the write is simply absent: no error, no divergence signal, no retry"
        );

        // Note that the frontier test the ADR proposed would not have made this
        // mistake -- the arriving clock position is 2 and the frontier is 1 --
        // which is a second, independent reason to prefer it. Identity
        // allocation was fixed first because it is the cheaper change and does
        // not alter what deduplication means on the receive path.
        let position = after_restart.vector_clock.get("A");
        assert!(
            position > executor.causal_frontier().get("A"),
            "a deduplication test keyed on the causal position would have admitted this write"
        );
    }

    #[test]
    fn dedup_a_partial_identity_collision_strands_the_replica_permanently() {
        // The compounding case, and the reason this was a correctness problem
        // rather than a lost write. Of two operations from a restarted origin,
        // the first collides and is dropped while the second does not collide
        // and is admitted -- so the replica ends up holding an operation whose
        // dependency was silently discarded, and waits for it forever. Which
        // operations collide depends only on how many the origin committed
        // before it died.
        let (mut executor, _dir) = receiver();
        executor
            .receive_replicated(
                message(envelope("A", 1, &[("A", 1)], "one")),
                zero_version(),
            )
            .expect("receive");

        // Origin restarts after one operation and commits two more. Sequences
        // 1 and 2; clock positions 2 and 3.
        let dropped = executor
            .receive_replicated(
                message(envelope("A", 1, &[("A", 2)], "two")),
                zero_version(),
            )
            .expect("receive");
        let admitted = executor
            .receive_replicated(
                message(envelope("A", 2, &[("A", 3)], "three")),
                zero_version(),
            )
            .expect("receive");

        assert_eq!(
            dropped,
            ReceiveOutcome::AlreadyKnown,
            "the first collides and is lost"
        );
        assert!(
            matches!(admitted, ReceiveOutcome::PendingDependencies { .. }),
            "the second does not collide, is admitted, and is then held forever waiting on \
             the operation that was silently dropped: {admitted:?}"
        );
        assert_eq!(executor.pending_causal_count(), 1);
        assert_eq!(
            executor.causal_frontier().get("A"),
            1,
            "the replica is permanently stuck one operation behind an origin that is still writing"
        );
    }
}
