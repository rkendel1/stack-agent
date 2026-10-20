//! A FeltDB replication node, as an operating-system process.
//!
//! This exists so replication can be proved across a real process and network
//! boundary rather than between executors sharing a heap. It is a harness, not
//! a product surface: it speaks a line protocol on stdin/stdout so a test can
//! drive it, and it implements no replication semantics of its own. Every
//! decision about ordering, deduplication, durability, and convergence is made
//! by `DistributedTransactionExecutor`; this binary only moves bytes between
//! processes and reports what happened.
//!
//! ## Links are directed, one TCP connection each
//!
//! `TcpTransport` holds a single stream, and its `receive()` holds the stream
//! lock while awaiting the next message. A bidirectional link would therefore
//! deadlock the first `send()` behind the receive that is parked on the socket.
//!
//! So each direction gets its own connection: for a link X -> Y, Y binds a port
//! dedicated to X and only ever receives on it, and X connects to that port and
//! only ever sends. Three nodes in a full mesh is six directed links.
//!
//! That is a topology choice made here in the harness. It changes nothing about
//! the transport or the protocol.
//!
//! ## Usage
//!
//! ```text
//! feltdb_node --node-id A --data DIR --recv B=127.0.0.1:9111 --send B=127.0.0.1:9121
//! ```
//!
//! `--recv PEER=ADDR` binds ADDR and accepts the connection PEER makes to it.
//! `--send PEER=ADDR` connects to the address PEER is listening on for us.
//!
//! Commands on stdin, one per line:
//!
//! ```text
//! execute-tx RECORD   commit a local transaction and replicate it to every peer
//! resend-last         put the identical envelope back on the wire, to test dedup
//! query-state         the local replica's state hash and applied-operation count
//! metrics             per-link byte and message counters, straight from the transport
//! causal-state        applied frontier, and what is durable but held back
//! hold-inbound PEER   park messages from PEER instead of delivering them
//! release-inbound PEER [reverse]
//!                     deliver what was parked, optionally newest-first
//! peers               per-link connection state
//! shutdown            exit cleanly
//! ```

use feltdb::authority_failover::{AuthorityStore, ClaimResult, FencingToken, Heartbeat};
use feltdb::convergence::VectorClock;
use feltdb::distributed_transactions::{
    CausalCapacity, DistributedTransactionExecutor, ReceiveOutcome, ReplicationMessage,
    SubmitOutcome,
};
use feltdb::replica_acknowledgements::{
    AcknowledgementStore, ReclamationEvidence, ReplicaAcknowledgement,
};
use feltdb::replica_membership::MembershipStore;
use feltdb::replication_protocol::ProtocolTransport;
use feltdb::state_hash::StateHash;
use feltdb::tcp_transport::TcpTransport;
use feltdb::transactions::{
    ConsistencyContract, Operation, OperationCommand, OperationId, StateVersion,
};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep, Duration};

/// One outbound connection to a peer, used only for sending.
///
/// `control` exists because `connect` needs `&mut self`. Everything else goes
/// through `io`, a clone: `TcpTransport` holds its stream and metrics behind
/// `Arc`s, so a clone is the same connection, and reading counters or sending
/// does not queue behind whatever else holds the mutex.
#[derive(Clone)]
struct SendLink {
    peer: String,
    addr: String,
    control: Arc<Mutex<TcpTransport>>,
    io: TcpTransport,
}

/// One inbound connection from a peer, used only for receiving.
///
/// The distinction matters more here: `receive()` parks on the socket holding
/// the transport's internal stream lock, so anything sharing an outer mutex
/// with it would block until a message happened to arrive.
#[derive(Clone)]
struct RecvLink {
    peer: String,
    #[allow(dead_code)]
    addr: String,
    control: Arc<Mutex<TcpTransport>>,
    io: TcpTransport,
}

/// Hand one replicated message to the executor and record what it decided.
///
/// The harness makes no ordering decision of its own here. It calls the same
/// `receive_replicated` the shipping receive path calls, and reports the typed
/// outcome back so a test can assert on "applied" versus "held back" rather
/// than inferring it from state.
fn deliver(
    executor: &mut DistributedTransactionExecutor,
    version: &mut StateVersion,
    message: ReplicationMessage,
) -> serde_json::Value {
    let origin = message.envelope.originating_node.clone();
    let key = format!(
        "{}:{}",
        message.envelope.envelope_id.originating_node, message.envelope.envelope_id.sequence
    );
    let parent = version.clone();
    match executor.receive_replicated(message, parent) {
        Ok(ReceiveOutcome::Applied { state_hash }) => {
            // Only a real application advances the chained version, so a held
            // or duplicated message cannot make the node look like it moved.
            version.state_hash = state_hash.to_string();
            version.vector_clock.increment(&origin);
            json!({ "envelope": key, "outcome": "applied" })
        }
        Ok(ReceiveOutcome::PendingDependencies { missing }) => {
            json!({ "envelope": key, "outcome": "pending", "missing": missing })
        }
        Ok(ReceiveOutcome::AlreadyKnown) => {
            json!({ "envelope": key, "outcome": "already_known" })
        }
        Ok(ReceiveOutcome::CapacityDeferred {
            missing,
            pending_entries,
            pending_bytes,
        }) => {
            // Durable, but not buffered. The caller's obligation is to stop
            // reading from this peer; the envelope is admitted from the log
            // once the backlog drains.
            json!({
                "envelope": key,
                "outcome": "capacity_deferred",
                "missing": missing,
                "pending_entries": pending_entries,
                "pending_bytes": pending_bytes,
            })
        }
        Err(error) => json!({ "envelope": key, "outcome": "error", "error": error }),
    }
}

fn parse_link(value: &str) -> Result<(String, String), String> {
    let (peer, addr) = value
        .split_once('=')
        .ok_or_else(|| format!("expected PEER=ADDR, got {value}"))?;
    if peer.is_empty() || addr.is_empty() {
        return Err(format!("expected PEER=ADDR, got {value}"));
    }
    Ok((peer.to_string(), addr.to_string()))
}

/// Connect with retries: the peer's listener may not be bound yet, and after a
/// peer restarts its old socket is gone and this is how the link comes back.
async fn connect_with_retry(link: &SendLink, attempts: u32) -> bool {
    for attempt in 0..attempts {
        let mut control = link.control.lock().await;
        if control.connect(&link.addr).await.is_ok() {
            return true;
        }
        drop(control);
        sleep(Duration::from_millis(25 * (attempt as u64 + 1))).await;
    }
    false
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    let mut node_id = String::new();
    let mut data_dir: Option<PathBuf> = None;
    let mut send_specs: Vec<(String, String)> = Vec::new();
    let mut recv_specs: Vec<(String, String)> = Vec::new();
    let mut max_pending_entries: Option<usize> = None;
    let mut max_pending_bytes: Option<usize> = None;
    let mut authority_priority: u32 = 0;
    let mut heartbeat_interval_ms: u64 = 1_000;
    let mut heartbeat_failure_threshold: u32 = 5;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--node-id" => {
                index += 1;
                node_id = args.get(index).ok_or("--node-id requires a value")?.clone();
            }
            "--data" => {
                index += 1;
                data_dir = Some(PathBuf::from(
                    args.get(index).ok_or("--data requires a value")?,
                ));
            }
            "--send" => {
                index += 1;
                send_specs.push(parse_link(
                    args.get(index).ok_or("--send requires PEER=ADDR")?,
                )?);
            }
            "--recv" => {
                index += 1;
                recv_specs.push(parse_link(
                    args.get(index).ok_or("--recv requires PEER=ADDR")?,
                )?);
            }
            // The bound is settable so an acceptance test can reach it through
            // the production path. Filling the shipping default would mean
            // sending ten thousand blocked envelopes over TCP; a test that
            // instead constructed a barrier and filled it directly would be
            // measuring a component nothing runs, which is the defect this
            // whole line of work exists to stop repeating.
            "--max-pending-entries" => {
                index += 1;
                max_pending_entries = Some(
                    args.get(index)
                        .ok_or("--max-pending-entries requires a value")?
                        .parse::<usize>()
                        .map_err(|e| format!("--max-pending-entries: {e}"))?,
                );
            }
            "--max-pending-bytes" => {
                index += 1;
                max_pending_bytes = Some(
                    args.get(index)
                        .ok_or("--max-pending-bytes requires a value")?
                        .parse::<usize>()
                        .map_err(|e| format!("--max-pending-bytes: {e}"))?,
                );
            }
            // Static priority for authority election. Higher priority nodes are
            // preferred during failover when multiple candidates have equal state.
            "--authority-priority" => {
                index += 1;
                authority_priority = args
                    .get(index)
                    .ok_or("--authority-priority requires a value")?
                    .parse::<u32>()
                    .map_err(|e| format!("--authority-priority: {e}"))?;
            }
            "--heartbeat-interval-ms" => {
                index += 1;
                heartbeat_interval_ms = args
                    .get(index)
                    .ok_or("--heartbeat-interval-ms requires a value")?
                    .parse::<u64>()
                    .map_err(|e| format!("--heartbeat-interval-ms: {e}"))?;
            }
            "--heartbeat-failure-threshold" => {
                index += 1;
                heartbeat_failure_threshold = args
                    .get(index)
                    .ok_or("--heartbeat-failure-threshold requires a value")?
                    .parse::<u32>()
                    .map_err(|e| format!("--heartbeat-failure-threshold: {e}"))?;
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
        index += 1;
    }

    if node_id.is_empty() {
        return Err("--node-id is required".into());
    }

    // Durability is the executor's, not this binary's. The log path is per
    // node, so each process owns its own directory on disk.
    let log_path = data_dir.as_ref().map(|dir| {
        std::fs::create_dir_all(dir).ok();
        dir.join("operations.jsonl")
    });

    // Membership lives beside the log and is loaded from disk, never rebuilt
    // from --send/--recv. A node's connection flags say who it talks to; they
    // say nothing about who is owed history, and conflating the two is what
    // this store exists to stop.
    let mut membership = MembershipStore::open(
        data_dir
            .as_ref()
            .map(|dir| dir.join("membership.json"))
            .unwrap_or_else(|| PathBuf::from("membership.json")),
    )
    .map_err(|e| e.to_string())?;

    // Acknowledgements are durable and shared with the receive tasks, because
    // they arrive on the socket rather than through a command.
    let acknowledgements = Arc::new(Mutex::new(
        AcknowledgementStore::open(
            data_dir
                .as_ref()
                .map(|dir| dir.join("acknowledgements.json"))
                .unwrap_or_else(|| PathBuf::from("acknowledgements.json")),
        )
        .map_err(|e| e.to_string())?,
    ));

    // Authority state tracks who holds write authority in the cluster. The store
    // is durable and survives restarts, so a node that crashes can rejoin and
    // recover its previous role. The cluster ID is set by the first `authority-claim`
    // command, so it starts as the node's own ID.
    let authority = Arc::new(Mutex::new(
        AuthorityStore::open_with_failure_detector(
            data_dir
                .as_ref()
                .map(|dir| dir.join("authority.json"))
                .unwrap_or_else(|| PathBuf::from("authority.json")),
            &node_id, // cluster_id defaults to node_id until set
            &node_id,
            authority_priority,
            heartbeat_interval_ms,
            heartbeat_failure_threshold,
        )
        .map_err(|e| e.to_string())?,
    ));

    let initial_state = StateHash::from_hex("0".repeat(64));
    let mut executor = DistributedTransactionExecutor::with_log(
        node_id.clone(),
        initial_state.clone(),
        log_path.clone(),
    )?;

    // The bound is set before recovery, not after, because recovery restores
    // the backlog and has to respect the same limit the live path does.
    // Otherwise a node could restart into a backlog it would never have
    // accepted while running, and the bound would hold only until the first
    // crash.
    let default_capacity = CausalCapacity::default();
    executor.set_causal_capacity(CausalCapacity::new(
        max_pending_entries.unwrap_or(default_capacity.max_pending_entries),
        max_pending_bytes.unwrap_or(default_capacity.max_pending_bytes),
    ));

    // Replaying the log restores the deduplication set, so a restarted node
    // does not re-apply operations it already durably recorded.
    let mut replayed = 0usize;
    if let Some(path) = &log_path {
        if path.exists() {
            replayed = executor.load_from_disk(path.clone())?;
        }
    }

    for (peer, _) in send_specs.iter().chain(recv_specs.iter()) {
        if peer != &node_id {
            executor.register_replica(peer.clone(), initial_state.clone());
        }
    }

    // The running state version, advanced by every applied transaction.
    //
    // This matters more than it looks. `execute_transaction` derives the next
    // hash as H(parent_hash, command), so passing a fresh zero-hash parent each
    // time — as this binary and the old in-process tests both did — makes the
    // resulting "state hash" a function of the last command alone, carrying no
    // history. Two nodes that applied completely different sets of operations
    // then report identical hashes as long as the most recent one matches, and
    // a convergence check built on that comparison cannot fail.
    //
    // Chaining it makes the hash mean what its name says, and makes a node that
    // missed an operation observably divergent. It lives beside the executor
    // under one lock so a local commit and an arriving replication cannot
    // interleave and chain onto a parent that is already stale.
    let recovered_version = executor
        .get_replica_state(&node_id)
        .map(|replica| StateVersion::new(replica.vector_clock, replica.state_hash.to_string()))
        .unwrap_or_else(|| StateVersion::new(VectorClock::new(), "0".repeat(64)));
    let core = Arc::new(Mutex::new((executor, recovered_version)));
    let should_exit = Arc::new(AtomicBool::new(false));
    // Per-peer delivery hold.
    //
    // `Some(queue)` means messages arriving from that peer are received off the
    // socket as normal and parked instead of being handed to the executor.
    // This is how a test produces out-of-causal-order arrival without faking
    // the network: the bytes really do cross TCP, and only the order in which
    // the receiver is offered them is controlled. A real network reorders
    // delivery this way; the harness just does it on demand.
    let held: Arc<Mutex<HashMap<String, Vec<ReplicationMessage>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    // Peers this node has stopped reading from because the causal backlog is
    // at its bound.
    //
    // This is the backpressure the bounding decision commits to, and not
    // reading is the whole mechanism: the socket's receive buffer fills, the
    // TCP window closes, and the sender blocks. Nothing is dropped and nothing
    // is refused, so no retransmission protocol is needed -- which is good,
    // because there isn't one.
    let backpressured: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    // Kept so `resend-last` can put the identical envelope back on the wire.
    let last_envelope: Arc<Mutex<Option<feltdb::distributed_transactions::TransactionEnvelope>>> =
        Arc::new(Mutex::new(None));

    let send_links: Vec<SendLink> = send_specs
        .into_iter()
        .map(|(peer, addr)| {
            let transport = TcpTransport::new("127.0.0.1:0".to_string());
            SendLink {
                peer,
                addr,
                control: Arc::new(Mutex::new(transport.clone())),
                io: transport,
            }
        })
        .collect();

    let recv_links: Vec<RecvLink> = recv_specs
        .into_iter()
        .map(|(peer, addr)| {
            let transport = TcpTransport::new(addr.clone());
            RecvLink {
                peer,
                addr,
                control: Arc::new(Mutex::new(transport.clone())),
                io: transport,
            }
        })
        .collect();

    // Accept, serve, and accept again. One task per link, because each parks
    // on its own socket and must not block the others.
    //
    // The re-listen is what makes a peer restart survivable. `TcpTransport`'s
    // `listen()` binds, accepts exactly one connection, and drops the listener,
    // so after that first accept the port is closed and a peer that reconnects
    // is refused. Looping back to `listen()` once the connection ends rebinds
    // the port for the next one. That is a harness-level fix: the transport
    // keeps its single-connection shape and the protocol is untouched.
    for link in &recv_links {
        let control = link.control.clone();
        let transport = link.io.clone();
        let core = core.clone();
        let exit = should_exit.clone();
        let peer = link.peer.clone();
        let held = held.clone();
        let backpressured = backpressured.clone();
        let acknowledgements = acknowledgements.clone();
        tokio::spawn(async move {
            while !exit.load(Ordering::Relaxed) {
                {
                    let mut control = control.lock().await;
                    if let Err(error) = control.listen().await {
                        eprintln!("listen for {peer} failed: {error}");
                        drop(control);
                        sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                }

                // Serve this connection until it ends, then rebind for the next.
                loop {
                    if exit.load(Ordering::Relaxed) {
                        return;
                    }

                    // Do not read from a backpressured peer. While paused, keep
                    // trying to drain: the work that frees capacity may arrive
                    // on another connection, and a deferred envelope is already
                    // durable, so draining needs nothing from this socket.
                    while backpressured.lock().await.contains(&peer) {
                        if exit.load(Ordering::Relaxed) {
                            return;
                        }
                        {
                            let mut guard = core.lock().await;
                            let (executor, version) = &mut *guard;
                            let parent = version.clone();
                            let _ = executor.drain_backlog(parent);
                            if executor.pending_causal_count()
                                < executor.causal_capacity().max_pending_entries
                            {
                                backpressured.lock().await.remove(&peer);
                            }
                        }
                        sleep(Duration::from_millis(25)).await;
                    }

                    match transport.receive().await {
                        Ok(messages) => {
                            // Acknowledgements arrive interleaved with
                            // envelopes on the same connection. They are
                            // durable before they count as evidence.
                            for ack in transport.take_acknowledgements().await {
                                let mut store = acknowledgements.lock().await;
                                if let Err(error) = store.record(ack) {
                                    eprintln!("acknowledgement not recorded: {error}");
                                }
                            }
                            for message in messages {
                                {
                                    let mut hold = held.lock().await;
                                    if let Some(queue) = hold.get_mut(&peer) {
                                        queue.push(message);
                                        continue;
                                    }
                                }
                                let outcome = {
                                    let mut guard = core.lock().await;
                                    let (executor, version) = &mut *guard;
                                    deliver(executor, version, message)
                                };
                                if outcome.get("outcome").and_then(|value| value.as_str())
                                    == Some("capacity_deferred")
                                {
                                    backpressured.lock().await.insert(peer.clone());
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        });
    }

    // Establish outbound links before announcing readiness, so a test that
    // writes immediately is not racing the topology.
    for link in &send_links {
        connect_with_retry(link, 20).await;
    }

    println!(
        "STARTED {}",
        json!({
            "node": node_id,
            "replayed": replayed,
            "send": send_links.iter().map(|l| l.peer.clone()).collect::<Vec<_>>(),
            "recv": recv_links.iter().map(|l| l.peer.clone()).collect::<Vec<_>>(),
        })
    );
    io::stdout().flush().ok();

    let (commands, mut incoming) = mpsc::channel::<String>(100);
    tokio::task::spawn_blocking(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            let line = line.trim().to_string();
            if !line.is_empty() && commands.blocking_send(line).is_err() {
                break;
            }
        }
    });

    while let Some(command) = incoming.recv().await {
        let (verb, argument) = match command.split_once(' ') {
            Some((verb, rest)) => (verb, rest.trim().to_string()),
            None => (command.as_str(), String::new()),
        };

        match verb {
            "execute-tx" | "authority-write" => {
                let record = if verb == "authority-write" {
                    let mut parts = argument.splitn(2, ' ');
                    let token = parts.next().and_then(|value| value.parse::<u64>().ok());
                    let record = parts.next().unwrap_or("item").to_string();
                    let validation = match token {
                        Some(token) => authority
                            .lock()
                            .await
                            .validate_quorum_write(FencingToken(token), 2),
                        None => Err(feltdb::authority_failover::AuthorityError::StaleToken {
                            provided: FencingToken(0),
                            current: authority.lock().await.state().current_token(),
                        }),
                    };
                    if let Err(error) = validation {
                        println!(
                            "AUTHORITY_WRITE_REJECTED {}",
                            json!({ "record": record, "reason": error.to_string() })
                        );
                        io::stdout().flush().ok();
                        continue;
                    }
                    record
                } else if argument.is_empty() {
                    "item".to_string()
                } else {
                    argument
                };

                let mut fields = HashMap::new();
                fields.insert("record".to_string(), json!(record.clone()));

                // The identity comes from the executor, which derives it from the
                // durable log. This binary used to keep its own counter,
                // starting at 1 on every launch, so a restarted node re-issued
                // identities its own earlier operations already held.
                let (parent, seq) = {
                    let guard = core.lock().await;
                    (guard.1.clone(), guard.0.next_origin_sequence())
                };
                let operation = Operation::new(
                    OperationId::new(node_id.clone(), seq),
                    parent.clone(),
                    format!("tx_{record}"),
                    OperationCommand {
                        op_type: "set".to_string(),
                        collection: "items".to_string(),
                        record_id: record.clone(),
                        fields,
                    },
                    node_id.clone(),
                );

                let envelope = {
                    let mut guard = core.lock().await;
                    let (executor, version) = &mut *guard;
                    // The admitting entry point: local work is refused when the
                    // causal backlog is at its bound, before anything is
                    // executed or persisted, so a refusal leaves nothing to
                    // undo.
                    let result = executor.submit_local_transaction(
                        format!("tx_{record}"),
                        version.clone(),
                        vec![operation],
                        ConsistencyContract::local(),
                    );
                    if matches!(result, Ok(SubmitOutcome::Admitted(_))) {
                        if let Some(replica) = executor.get_replica_state(&node_id) {
                            version.state_hash = replica.state_hash.to_string();
                            version.vector_clock.increment(&node_id);
                        }
                    }
                    result
                };

                match envelope {
                    Ok(SubmitOutcome::CapacityExceeded {
                        pending_entries,
                        pending_bytes,
                        limit_entries,
                        limit_bytes,
                    }) => {
                        // A distinct outcome, not an error: the caller should
                        // retry, and nothing was admitted or lost.
                        println!(
                            "TX_REFUSED {}",
                            json!({
                                "record": record,
                                "reason": "CAPACITY_EXCEEDED",
                                "pending_entries": pending_entries,
                                "pending_bytes": pending_bytes,
                                "limit_entries": limit_entries,
                                "limit_bytes": limit_bytes,
                            })
                        );
                    }
                    Ok(SubmitOutcome::Admitted(envelope)) => {
                        *last_envelope.lock().await = Some(envelope.clone());
                        let mut delivered = Vec::new();
                        let mut failed = Vec::new();
                        for link in &send_links {
                            let message = ReplicationMessage::new(
                                envelope.clone(),
                                node_id.clone(),
                                link.peer.clone(),
                                seq,
                            );
                            let sent = link.io.send(message.clone()).await;
                            match sent {
                                Ok(()) => delivered.push(link.peer.clone()),
                                Err(_) => {
                                    // The peer may have restarted, which drops
                                    // our end of the socket. Reconnect and try
                                    // once more before reporting a failure.
                                    if connect_with_retry(link, 3).await {
                                        if link.io.send(message).await.is_ok() {
                                            delivered.push(link.peer.clone());
                                            continue;
                                        }
                                    }
                                    failed.push(link.peer.clone());
                                }
                            }
                        }
                        println!(
                            "TX_EXECUTED {}",
                            json!({
                                "record": record,
                                // The identity the executor actually issued.
                                "sequence": envelope.envelope_id.sequence,
                                "delivered": delivered,
                                "failed": failed,
                            })
                        );
                    }
                    Err(error) => println!("ERROR {}", json!({ "reason": error })),
                }
                io::stdout().flush().ok();
            }

            // Re-send the most recent envelope, unchanged, to every peer.
            //
            // Deduplication is a claimed property of the executor, and it is
            // only interesting if the duplicate actually crosses the wire.
            // Issuing another `execute-tx` would not test it: that produces a
            // new envelope id and is a different transaction.
            // `resend SEQUENCE` — put one specific envelope from this node's
            // durable log back on the wire.
            //
            // A peer re-sending an operation another node is missing is the
            // primitive catch-up is built from. This is not catch-up: nothing
            // decides *what* to send, a test does. It exists so the receiver's
            // behaviour can be exercised on a gap that a restart left behind.
            // `forward ORIGIN SEQUENCE` -- put an envelope this node holds, but
            // did not originate, back on the wire.
            //
            // `resend` deliberately only replays this node's own envelopes. A
            // peer relaying an operation that a *third* node is missing is a
            // different primitive, and it is the one a backpressured receiver
            // needs: when a node stops reading from the peer whose envelopes it
            // deferred, the dependency it is waiting for cannot arrive on that
            // link. It has to come from somewhere else, which is precisely the
            // case the read loop's keep-draining-while-paused behaviour exists
            // to serve.
            "forward" => {
                let mut parts = argument.split_whitespace();
                let origin = parts.next().unwrap_or_default().to_string();
                let wanted: Option<u64> = parts.next().and_then(|value| value.parse().ok());
                let envelope = {
                    let guard = core.lock().await;
                    let (executor, _) = &*guard;
                    executor.operation_log.as_ref().and_then(|log| {
                        log.load_all().ok().and_then(|envelopes| {
                            envelopes.into_iter().find(|candidate| {
                                candidate.envelope_id.originating_node == origin
                                    && Some(candidate.envelope_id.sequence) == wanted
                            })
                        })
                    })
                };
                match envelope {
                    Some(envelope) => {
                        let mut delivered = Vec::new();
                        for link in &send_links {
                            let message = ReplicationMessage::new(
                                envelope.clone(),
                                node_id.clone(),
                                link.peer.clone(),
                                envelope.envelope_id.sequence,
                            );
                            // Forwarding, like re-sending, is an explicit
                            // recovery operation. Prefer a fresh connection so
                            // a peer restart cannot leave this write accepted
                            // by a stale local socket and delivered nowhere.
                            let _ = connect_with_retry(link, 3).await;
                            if link.io.send(message).await.is_ok() {
                                delivered.push(link.peer.clone());
                            }
                        }
                        println!(
                            "FORWARDED {}",
                            json!({
                                "origin": origin,
                                "sequence": wanted,
                                "delivered": delivered,
                            })
                        );
                    }
                    None => println!(
                        "FORWARDED {}",
                        json!({ "origin": origin, "sequence": wanted, "delivered": [], "found": false })
                    ),
                }
                io::stdout().flush().ok();
            }

            "resend" => {
                let wanted: Option<u64> = argument.trim().parse().ok();
                let envelope = {
                    let guard = core.lock().await;
                    let (executor, _) = &*guard;
                    executor.operation_log.as_ref().and_then(|log| {
                        log.load_all().ok().and_then(|envelopes| {
                            envelopes.into_iter().find(|candidate| {
                                candidate.envelope_id.originating_node == node_id
                                    && Some(candidate.envelope_id.sequence) == wanted
                            })
                        })
                    })
                };
                match envelope {
                    Some(envelope) => {
                        let mut delivered = Vec::new();
                        for link in &send_links {
                            let message = ReplicationMessage::new(
                                envelope.clone(),
                                node_id.clone(),
                                link.peer.clone(),
                                envelope.envelope_id.sequence,
                            );
                            // Re-sending is the recovery primitive used after a
                            // peer restart. A write to the old TCP socket can
                            // still return success after the peer has exited,
                            // because that only confirms acceptance by the
                            // local kernel. Try to establish a fresh connection
                            // first. If the peer never restarted, the connect
                            // attempt leaves the established stream untouched.
                            let _ = connect_with_retry(link, 3).await;
                            if link.io.send(message).await.is_ok() {
                                delivered.push(link.peer.clone());
                            }
                        }
                        println!(
                            "RESENT {}",
                            json!({ "envelope": envelope.envelope_id, "delivered": delivered })
                        );
                    }
                    None => println!(
                        "ERROR {}",
                        json!({ "reason": format!("no envelope {argument} in this node's log") })
                    ),
                }
                io::stdout().flush().ok();
            }

            "resend-last" => {
                let envelope = last_envelope.lock().await.clone();
                match envelope {
                    Some(envelope) => {
                        let mut delivered = Vec::new();
                        for link in &send_links {
                            let message = ReplicationMessage::new(
                                envelope.clone(),
                                node_id.clone(),
                                link.peer.clone(),
                                envelope.envelope_id.sequence,
                            );
                            if link.io.send(message).await.is_ok() {
                                delivered.push(link.peer.clone());
                            }
                        }
                        println!(
                            "RESENT {}",
                            json!({ "envelope": envelope.envelope_id, "delivered": delivered })
                        );
                    }
                    None => println!("ERROR {}", json!({ "reason": "nothing sent yet" })),
                }
                io::stdout().flush().ok();
            }

            "query-state" => {
                let guard = core.lock().await;
                let (executor, _) = &*guard;
                let canonical = executor
                    .canonical_state()
                    .map(|state| state.hash().to_string())
                    .unwrap_or_else(|error| format!("error: {error}"));
                match executor.get_replica_state(&node_id) {
                    Some(replica) => println!(
                        "STATE {}",
                        json!({
                            "hash": replica.state_hash.to_string(),
                            "operations_applied": replica.operations_applied,
                            // Order-independent, and therefore the one to
                            // compare across replicas. The chained hash above
                            // detects a missing operation in an ordered
                            // history; it calls two nodes that applied the same
                            // concurrent operations in different orders
                            // divergent, which they are not.
                            "canonical_hash": canonical,
                        })
                    ),
                    None => println!("ERROR {}", json!({ "reason": "no local replica" })),
                }
                io::stdout().flush().ok();
            }

            // What the causal receiver is holding, and why.
            // Membership commands. Deliberately no compaction frontier and no
            // acknowledgement tracking: this establishes who exists and who is
            // required, and reclaims nothing.
            "cluster-create" => {
                let result = membership.create_cluster(argument.trim());
                println!(
                    "CLUSTER {}",
                    match result {
                        Ok(()) => json!({ "cluster_id": argument.trim(), "created": true }),
                        Err(error) => json!({ "created": false, "error": error.to_string() }),
                    }
                );
                io::stdout().flush().ok();
            }

            "replica-join" | "replica-approve" | "replica-suspect" | "replica-seen"
            | "replica-remove" => {
                let mut parts = argument.split_whitespace();
                let replica = parts.next().unwrap_or_default().to_string();
                let result = match verb {
                    "replica-join" => {
                        let node = parts.next().unwrap_or(&replica).to_string();
                        membership.join_replica(&replica, &node)
                    }
                    "replica-approve" => membership.approve_replica(&replica),
                    "replica-suspect" => membership.suspect_replica(&replica),
                    "replica-seen" => membership.mark_seen(&replica),
                    _ => membership.remove_replica(&replica),
                };
                println!(
                    "REPLICA {}",
                    match result {
                        Ok(()) => json!({ "replica_id": replica, "action": verb, "ok": true }),
                        Err(error) =>
                            json!({ "replica_id": replica, "action": verb, "ok": false, "error": error.to_string() }),
                    }
                );
                io::stdout().flush().ok();
            }

            // Report what this node has durably applied, to every peer.
            //
            // The frontier is read from the executor, where it advances only in
            // record_applied -- after an fsynced envelope is applied. So the
            // statement "durably applied through this frontier" is true by
            // construction rather than by assertion.
            "ack-broadcast" => {
                let (frontier, marks) = {
                    let guard = core.lock().await;
                    let (executor, _) = &*guard;
                    (
                        executor.causal_frontier().clone(),
                        executor.origin_sequence_high_water(),
                    )
                };
                let view: Vec<String> = membership
                    .get_members()
                    .into_iter()
                    .map(|replica| replica.replica_id)
                    .collect();
                let replica_id = membership
                    .get_members()
                    .into_iter()
                    .find(|replica| replica.node_id == node_id)
                    .map(|replica| replica.replica_id)
                    .unwrap_or_else(|| node_id.clone());

                let ack = ReplicaAcknowledgement::new(
                    replica_id.clone(),
                    node_id.clone(),
                    frontier,
                    marks,
                    view,
                );

                // A node's own acknowledgement is evidence it holds first-hand:
                // it knows its own durable frontier without being told. Without
                // this a node would be permanently blocked on itself, since
                // nothing else can report on its behalf.
                {
                    let mut store = acknowledgements.lock().await;
                    let _ = store.record(ack.clone());
                }

                let mut delivered = Vec::new();
                for link in &send_links {
                    // Acknowledgements are durable recovery evidence and are
                    // commonly rebroadcast after a peer restart. Refresh the
                    // connection first for the same reason as explicit
                    // envelope replay.
                    let _ = connect_with_retry(link, 3).await;
                    if link.io.send_acknowledgement(ack.clone()).await.is_ok() {
                        delivered.push(link.peer.clone());
                    }
                }
                println!(
                    "ACK_SENT {}",
                    json!({
                        "replica_id": replica_id,
                        "frontier": ack.applied_frontier.clocks,
                        "delivered": delivered,
                    })
                );
                io::stdout().flush().ok();
            }

            "acknowledgements" => {
                let store = acknowledgements.lock().await;
                println!("ACKS {}", json!({ "acknowledgements": store.all() }));
                io::stdout().flush().ok();
            }

            // What this node's evidence permits. Computes; reclaims nothing.
            "reclamation-evidence" => {
                let local: Vec<String> = membership
                    .get_required_replicas()
                    .into_iter()
                    .map(|replica| replica.replica_id)
                    .collect();
                let store = acknowledgements.lock().await;
                let report = match store.evidence(&local) {
                    ReclamationEvidence::Blocked { waiting_on } => json!({
                        "state": "BLOCKED",
                        "waiting_on": waiting_on,
                        "required_union": store.required_union(&local),
                    }),
                    ReclamationEvidence::SafeThrough { frontier, covered } => json!({
                        "state": "SAFE_THROUGH",
                        "frontier": frontier.clocks,
                        "covered": covered,
                        "required_union": store.required_union(&local),
                    }),
                };
                println!("EVIDENCE {}", report);
                io::stdout().flush().ok();
            }

            "members" => {
                println!(
                    "MEMBERS {}",
                    json!({
                        "cluster_id": membership.cluster().map(|c| c.cluster_id.clone()),
                        "members": membership.get_members(),
                        "all": membership.all_replicas(),
                    })
                );
                io::stdout().flush().ok();
            }

            "required-replicas" => {
                println!(
                    "REQUIRED {}",
                    json!({ "required": membership.get_required_replicas() })
                );
                io::stdout().flush().ok();
            }

            "causal-state" => {
                let guard = core.lock().await;
                let (executor, _) = &*guard;
                let paused = {
                    let mut peers: Vec<String> =
                        backpressured.lock().await.iter().cloned().collect();
                    peers.sort();
                    peers
                };
                println!(
                    "CAUSAL {}",
                    json!({
                        "frontier": executor.causal_frontier().clocks,
                        "pending": executor.pending_causal_keys(),
                        "pending_count": executor.pending_causal_count(),
                        "pending_metadata_bytes": executor.pending_causal_metadata_bytes(),
                        // Occupancy against the bound that is now enforced.
                        // Current and maximum, for both dimensions, because a
                        // backlog that filled and drained between two reads is
                        // invisible to a current-value gauge.
                        "pending_payload_bytes": executor.pending_causal_payload_bytes(),
                        "deferred_count": executor.deferred_causal_count(),
                        "deferred": executor.deferred_causal_keys(),
                        "max_pending_entries_seen": executor.max_causal_occupancy().0,
                        "max_pending_payload_bytes_seen": executor.max_causal_occupancy().1,
                        "limit_entries": executor.causal_capacity().max_pending_entries,
                        "limit_bytes": executor.causal_capacity().max_pending_bytes,
                        // Peers this node has stopped reading from. This is the
                        // observable for backpressure: the pause lives in the
                        // socket read loop, so without it a test can only infer
                        // the behaviour from timing.
                        //
                        // Locked after `core`, matching the order the read loop
                        // takes them in.
                        "backpressured": paused,
                    })
                );
                io::stdout().flush().ok();
            }

            // Park messages from a peer instead of delivering them, so a test
            // can choose the order the receiver is offered them in.
            "hold-inbound" => {
                let mut hold = held.lock().await;
                hold.entry(argument.clone()).or_default();
                println!("HELD {}", json!({ "peer": argument }));
                io::stdout().flush().ok();
            }

            // Deliver what was parked, optionally newest-first, then resume
            // normal delivery. Each message goes through the same production
            // receiver as an unheld one.
            // `release-inbound PEER [reverse] [COUNT]`
            //
            // COUNT releases only that many and keeps holding the rest, which
            // is what lets a test offer an operation while deliberately
            // withholding the dependency it needs.
            "release-inbound" => {
                let mut parts = argument.split_whitespace();
                let peer = parts.next().unwrap_or_default().to_string();
                let mut reverse = false;
                let mut limit: Option<usize> = None;
                for token in parts {
                    if token == "reverse" {
                        reverse = true;
                    } else if let Ok(value) = token.parse::<usize>() {
                        limit = Some(value);
                    }
                }

                let mut queued = { held.lock().await.remove(&peer).unwrap_or_default() };
                if reverse {
                    queued.reverse();
                }
                let take = limit.unwrap_or(queued.len()).min(queued.len());
                let remainder = queued.split_off(take);

                let mut outcomes = Vec::new();
                for message in queued {
                    let mut guard = core.lock().await;
                    let (executor, version) = &mut *guard;
                    outcomes.push(deliver(executor, version, message));
                }

                // Anything not released stays held, so the hold is still in
                // force for messages that arrive afterwards.
                if !remainder.is_empty() || limit.is_some() {
                    held.lock().await.insert(peer.clone(), remainder);
                }

                println!(
                    "RELEASED {}",
                    json!({ "peer": peer, "reverse": reverse, "outcomes": outcomes })
                );
                io::stdout().flush().ok();
            }

            // The proof that the network carried the work. Counters come from
            // the transport itself, so a test can assert bytes actually moved
            // rather than inferring it from converged state.
            "metrics" => {
                let mut sent = json!({});
                let mut received = json!({});
                for link in &send_links {
                    let metrics = link.io.metrics();
                    sent[&link.peer] = json!({
                        "bytes": metrics.get_bytes_sent(),
                        "messages": metrics.get_messages_sent(),
                        "connection_changes": metrics.get_connection_state_changes(),
                    });
                }
                for link in &recv_links {
                    let metrics = link.io.metrics();
                    received[&link.peer] = json!({
                        "bytes": metrics.get_bytes_received(),
                        "messages": metrics.get_messages_received(),
                        "connection_changes": metrics.get_connection_state_changes(),
                    });
                }
                println!("METRICS {}", json!({ "sent": sent, "received": received }));
                io::stdout().flush().ok();
            }

            "list-operations" => {
                let guard = core.lock().await;
                let (executor, _) = &*guard;

                // Get all operations from the log
                let mut all_ops = Vec::new();
                let mut applied_ops = Vec::new();
                let pending_ops = executor.pending_causal_keys();
                let deferred_ops = executor.deferred_causal_keys();

                if let Some(ref log) = executor.operation_log {
                    if let Ok(envelopes) = log.load_all() {
                        for env in &envelopes {
                            let key = format!(
                                "{}:{}",
                                env.envelope_id.originating_node, env.envelope_id.sequence
                            );
                            all_ops.push(key.clone());
                        }
                    }
                }

                // Get applied operations from the barrier
                for key in executor.causal_barrier.applied_keys() {
                    applied_ops.push(key.to_string());
                }

                println!(
                    "OPERATIONS {}",
                    json!({
                        "all": all_ops,
                        "applied": applied_ops,
                        "pending": pending_ops,
                        "deferred": deferred_ops,
                        "operations_applied": executor.get_replica_state(&node_id).map(|r| r.operations_applied).unwrap_or(0),
                    })
                );
                io::stdout().flush().ok();
            }

            "operation-inventory" => {
                let guard = core.lock().await;
                let (executor, _) = &*guard;

                // Get all operations from the log
                let mut operations = json!({
                    "applied": [],
                    "pending": [],
                    "deferred": [],
                });

                if let Some(ref log) = executor.operation_log {
                    if let Ok(envelopes) = log.load_all() {
                        let applied_keys = executor.causal_barrier.applied_keys();
                        let pending_keys = executor.pending_causal_keys();
                        let deferred_keys = executor.deferred_causal_keys();

                        for env in &envelopes {
                            let key = format!(
                                "{}:{}",
                                env.envelope_id.originating_node, env.envelope_id.sequence
                            );

                            if applied_keys.contains(&key) {
                                operations["applied"].as_array_mut().unwrap().push(json!({
                                    "id": key,
                                    "origin": env.envelope_id.originating_node,
                                    "sequence": env.envelope_id.sequence,
                                }));
                            } else if pending_keys.contains(&key) {
                                operations["pending"].as_array_mut().unwrap().push(json!({
                                    "id": key,
                                    "origin": env.envelope_id.originating_node,
                                    "sequence": env.envelope_id.sequence,
                                }));
                            } else if deferred_keys.contains(&key) {
                                operations["deferred"].as_array_mut().unwrap().push(json!({
                                    "id": key,
                                    "origin": env.envelope_id.originating_node,
                                    "sequence": env.envelope_id.sequence,
                                }));
                            }
                        }
                    }
                }

                println!("INVENTORY {}", operations);
                io::stdout().flush().ok();
            }

            "peers" => {
                let mut state = json!({});
                for link in &send_links {
                    state[format!("send:{}", link.peer)] = json!(link.io.is_connected());
                }
                for link in &recv_links {
                    state[format!("recv:{}", link.peer)] = json!(link.io.is_connected());
                }
                println!("PEERS {state}");
                io::stdout().flush().ok();
            }

            // Authority failover commands.
            //
            // These commands implement the distributed authority protocol that prevents
            // split-brain and allows automatic failover when the authority node fails.

            // Claim authority for this node. This generates a new fencing token that
            // supersedes any previous authority. The claim succeeds only if no other
            // node has a higher token and appears healthy.
            "authority-claim" => {
                let mut guard = authority.lock().await;
                let result = guard.claim_authority();
                match result {
                    Ok(ClaimResult::Granted { token }) => {
                        println!(
                            "AUTHORITY_CLAIMED {}",
                            json!({
                                "node": node_id,
                                "fencing_token": token.0,
                                "role": "AUTHORITY",
                            })
                        );
                    }
                    Ok(ClaimResult::Rejected {
                        current_authority,
                        current_token,
                    }) => {
                        println!(
                            "AUTHORITY_REJECTED {}",
                            json!({
                                "current_authority": current_authority,
                                "current_token": current_token.0,
                                "reason": "another authority is still active",
                            })
                        );
                    }
                    Ok(ClaimResult::Fenced { token }) => {
                        println!(
                            "AUTHORITY_FENCED {}",
                            json!({
                                "node": node_id,
                                "fencing_token": token.0,
                                "reason": "this node is fenced",
                            })
                        );
                    }
                    Err(e) => {
                        println!("ERROR {}", json!({ "reason": e.to_string() }));
                    }
                }
                io::stdout().flush().ok();
            }

            // Accept a new authority announced by another node.
            // Format: authority-accept NODE_ID TOKEN
            "authority-accept" => {
                let mut parts = argument.split_whitespace();
                let authority_node = parts.next().unwrap_or_default().to_string();
                let token_value: u64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
                let token = FencingToken(token_value);

                let mut guard = authority.lock().await;
                let result = guard.accept_authority(&authority_node, token);
                match result {
                    Ok(()) => {
                        let state = guard.state();
                        println!(
                            "AUTHORITY_ACCEPTED {}",
                            json!({
                                "authority_node": authority_node,
                                "fencing_token": token.0,
                                "local_role": format!("{:?}", state.local_role),
                            })
                        );
                    }
                    Err(e) => {
                        println!("ERROR {}", json!({ "reason": e.to_string() }));
                    }
                }
                io::stdout().flush().ok();
            }

            // Query the current authority state.
            "authority-state" => {
                let guard = authority.lock().await;
                let state = guard.state();
                let operations_applied = {
                    let core_guard = core.lock().await;
                    let (executor, _) = &*core_guard;
                    executor
                        .get_replica_state(&node_id)
                        .map(|r| r.operations_applied)
                        .unwrap_or(0)
                };
                println!(
                    "AUTHORITY {}",
                    json!({
                        "cluster_id": state.cluster_id,
                        "local_node": state.local_node_id,
                        "local_role": format!("{:?}", state.local_role),
                        "is_authority": state.is_authority(),
                        "is_fenced": state.is_fenced(),
                        "authority_node": state.authority_node(),
                        "current_token": state.current_token().0,
                        "highest_seen_token": state.highest_seen_token.0,
                        "operations_applied": operations_applied,
                    })
                );
                io::stdout().flush().ok();
            }

            // Record a heartbeat from a peer. This is used for failure detection.
            // Format: heartbeat NODE_ID [TOKEN] [OPS_APPLIED] [IS_AUTHORITY]
            "heartbeat" => {
                let mut parts = argument.split_whitespace();
                let from_node = parts.next().unwrap_or_default().to_string();

                if from_node.is_empty() {
                    println!(
                        "ERROR {}",
                        json!({ "reason": "heartbeat requires NODE_ID" })
                    );
                } else {
                    let mut guard = authority.lock().await;
                    guard.heartbeat(&from_node);
                    println!(
                        "HEARTBEAT_RECORDED {}",
                        json!({
                            "from_node": from_node,
                        })
                    );
                }
                io::stdout().flush().ok();
            }

            // Check if the current authority has failed based on heartbeat timeout.
            "authority-failed" => {
                let guard = authority.lock().await;
                let failed = guard.authority_failed();
                let failed_nodes = guard.failed_nodes();
                let state = guard.state();
                println!(
                    "AUTHORITY_CHECK {}",
                    json!({
                        "authority_failed": failed,
                        "current_authority": state.authority_node(),
                        "failed_nodes": failed_nodes,
                    })
                );
                io::stdout().flush().ok();
            }

            // Step down from authority role voluntarily.
            "authority-stepdown" => {
                let mut guard = authority.lock().await;
                let was_authority = guard.state().is_authority();
                let result = guard.step_down();
                match result {
                    Ok(()) => {
                        println!(
                            "AUTHORITY_STEPDOWN {}",
                            json!({
                                "node": node_id,
                                "was_authority": was_authority,
                                "now_role": format!("{:?}", guard.state().local_role),
                            })
                        );
                    }
                    Err(e) => {
                        println!("ERROR {}", json!({ "reason": e.to_string() }));
                    }
                }
                io::stdout().flush().ok();
            }

            // Recover from fenced state after catching up.
            "authority-recover" => {
                let mut guard = authority.lock().await;
                let was_fenced = guard.state().is_fenced();
                let result = guard.recover_from_fenced();
                match result {
                    Ok(()) => {
                        println!(
                            "AUTHORITY_RECOVERED {}",
                            json!({
                                "node": node_id,
                                "was_fenced": was_fenced,
                                "now_role": format!("{:?}", guard.state().local_role),
                            })
                        );
                    }
                    Err(e) => {
                        println!("ERROR {}", json!({ "reason": e.to_string() }));
                    }
                }
                io::stdout().flush().ok();
            }

            "shutdown" => {
                should_exit.store(true, Ordering::Relaxed);
                println!("SHUTDOWN");
                io::stdout().flush().ok();
                sleep(Duration::from_millis(50)).await;
                std::process::exit(0);
            }

            _ => {
                println!(
                    "ERROR {}",
                    json!({ "reason": format!("unknown command {verb}") })
                );
                io::stdout().flush().ok();
            }
        }
    }

    Ok(())
}
