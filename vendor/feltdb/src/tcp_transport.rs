/// TCP-based replication transport
///
/// Implements ProtocolTransport over real TCP sockets using message framing.
///
/// Wire Format: serde_json
/// - Chosen for compatibility with Operation::command.fields (serde_json::Value)
/// - bincode incompatible with untagged enums used by serde_json internally
/// - JSON is self-describing and provides stable framing for protocol evolution
/// - Length-prefix framing: 4-byte big-endian length + JSON payload
///
/// Transport Guarantees:
/// - TCP connection establishment/closure
/// - Message framing (length prefix + payload)
/// - Ordered delivery within a single TCP connection (TCP's byte-stream ordering)
/// - Serialization integrity (JSON encoder/decoder with bincode-compatible semantics)
/// - Connection state tracking (connect, disconnect, is_connected)
/// - Bounded buffers (prevent unbounded growth)
///
/// Deduplication/idempotency/retry logic is the protocol's responsibility via DistributedTransactionExecutor.
use crate::distributed_transactions::ReplicationMessage;
use crate::replica_acknowledgements::ReplicaAcknowledgement;

/// What actually goes on the wire.
///
/// Tagged, so a connection can carry envelopes and acknowledgements without
/// either being able to masquerade as the other. The alternative -- an
/// acknowledgement riding inside a `ReplicationMessage` with a placeholder
/// envelope -- would put a value on the receive path that only a flag
/// distinguishes from a real operation, and the receive path's whole job is
/// deciding what to apply.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Frame {
    Replication(Box<ReplicationMessage>),
    Acknowledgement(Box<ReplicaAcknowledgement>),
}
use crate::replication_protocol::{ProtocolError, ProtocolMetrics, ProtocolTransport};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

const MESSAGE_BUFFER_SIZE: usize = 100;

/// TCP-based transport for ReplicationMessage
#[derive(Clone)]
pub struct TcpTransport {
    local_addr: String,
    peer_addr: Arc<Mutex<Option<String>>>,
    stream: Arc<Mutex<Option<TcpStream>>>,
    metrics: Arc<ProtocolMetrics>,
    is_connected: Arc<AtomicBool>,
    receive_buffer: Arc<Mutex<Vec<ReplicationMessage>>>,
    /// Acknowledgements read off the socket, waiting to be drained.
    ///
    /// They arrive interleaved with envelopes on the same connection, so
    /// `receive` parks them here rather than changing its return type and the
    /// trait every caller implements against.
    ack_buffer: Arc<Mutex<Vec<ReplicaAcknowledgement>>>,
}

impl TcpTransport {
    /// Create a new TCP transport that will listen on the given address
    pub fn new(local_addr: String) -> Self {
        Self {
            local_addr,
            peer_addr: Arc::new(Mutex::new(None)),
            stream: Arc::new(Mutex::new(None)),
            metrics: Arc::new(ProtocolMetrics::new()),
            is_connected: Arc::new(AtomicBool::new(false)),
            receive_buffer: Arc::new(Mutex::new(Vec::with_capacity(MESSAGE_BUFFER_SIZE))),
            ack_buffer: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Listen for incoming TCP connections
    pub async fn listen(&mut self) -> Result<(), ProtocolError> {
        let listener = TcpListener::bind(&self.local_addr)
            .await
            .map_err(|e| ProtocolError::ConnectionError(e.to_string()))?;

        let stream = listener
            .accept()
            .await
            .map_err(|e| ProtocolError::ConnectionError(e.to_string()))?
            .0;

        *self.stream.lock().await = Some(stream);
        self.is_connected.store(true, Ordering::Relaxed);
        self.metrics.record_connection_state_change();

        Ok(())
    }

    pub fn metrics(&self) -> &ProtocolMetrics {
        &self.metrics
    }
}

impl TcpTransport {
    /// Send an acknowledgement over the same connection envelopes use.
    ///
    /// Not part of `ProtocolTransport`: that trait is the replication protocol,
    /// and an acknowledgement is a statement about what has been applied rather
    /// than something to apply.
    pub async fn send_acknowledgement(
        &self,
        ack: ReplicaAcknowledgement,
    ) -> Result<(), ProtocolError> {
        let mut stream_guard = self.stream.lock().await;
        let stream = stream_guard
            .as_mut()
            .ok_or(ProtocolError::ConnectionError("not connected".to_string()))?;
        let payload = serde_json::to_vec(&Frame::Acknowledgement(Box::new(ack)))
            .map_err(|e| ProtocolError::SerializationError(e.to_string()))?;
        stream
            .write_all(&(payload.len() as u32).to_be_bytes())
            .await
            .map_err(|e| ProtocolError::TransportError(e.to_string()))?;
        stream
            .write_all(&payload)
            .await
            .map_err(|e| ProtocolError::TransportError(e.to_string()))?;
        stream
            .flush()
            .await
            .map_err(|e| ProtocolError::TransportError(e.to_string()))?;
        self.metrics.record_send(4 + payload.len());
        Ok(())
    }

    /// Take the acknowledgements read off the socket since the last drain.
    pub async fn take_acknowledgements(&self) -> Vec<ReplicaAcknowledgement> {
        std::mem::take(&mut *self.ack_buffer.lock().await)
    }
}

#[async_trait::async_trait]
impl ProtocolTransport for TcpTransport {
    async fn send(&self, message: ReplicationMessage) -> Result<(), ProtocolError> {
        let mut stream_guard = self.stream.lock().await;
        let stream = stream_guard
            .as_mut()
            .ok_or(ProtocolError::ConnectionError("not connected".to_string()))?;

        // Serialize message using serde_json (bincode has issues with serde_json::Value)
        let payload = serde_json::to_vec(&Frame::Replication(Box::new(message)))
            .map_err(|e| ProtocolError::SerializationError(e.to_string()))?;

        let payload_len = payload.len() as u32;

        // Send length prefix (4 bytes, big-endian)
        stream
            .write_all(&payload_len.to_be_bytes())
            .await
            .map_err(|e| ProtocolError::TransportError(e.to_string()))?;

        // Send payload
        stream
            .write_all(&payload)
            .await
            .map_err(|e| ProtocolError::TransportError(e.to_string()))?;

        stream
            .flush()
            .await
            .map_err(|e| ProtocolError::TransportError(e.to_string()))?;

        self.metrics.record_send(4 + payload.len());

        Ok(())
    }

    async fn receive(&self) -> Result<Vec<ReplicationMessage>, ProtocolError> {
        let mut buffer = self.receive_buffer.lock().await;

        // If we have buffered messages, return them
        if !buffer.is_empty() {
            let messages = std::mem::take(&mut *buffer);
            return Ok(messages);
        }

        // Try to receive one message from the socket
        let mut stream_guard = self.stream.lock().await;
        let stream = stream_guard
            .as_mut()
            .ok_or(ProtocolError::ConnectionError("not connected".to_string()))?;

        // Set non-blocking read
        let mut len_bytes = [0u8; 4];
        match stream.read_exact(&mut len_bytes).await {
            Ok(0) => {
                // Connection closed
                return Err(ProtocolError::ConnectionError(
                    "connection closed by peer".to_string(),
                ));
            }
            Ok(_) => {
                let payload_len = u32::from_be_bytes(len_bytes) as usize;

                if payload_len > 1024 * 1024 {
                    // Sanity check: don't allow messages larger than 1MB
                    return Err(ProtocolError::TransportError(
                        "message too large".to_string(),
                    ));
                }

                let mut payload = vec![0u8; payload_len];
                stream
                    .read_exact(&mut payload)
                    .await
                    .map_err(|e| ProtocolError::TransportError(e.to_string()))?;

                let frame: Frame = serde_json::from_slice(&payload)
                    .map_err(|e| ProtocolError::SerializationError(e.to_string()))?;

                self.metrics.record_receive(4 + payload_len);

                match frame {
                    Frame::Replication(message) => Ok(vec![*message]),
                    Frame::Acknowledgement(ack) => {
                        // Read off the socket and parked. The bytes crossed the
                        // network exactly as an envelope's do, and the counters
                        // above record them.
                        self.ack_buffer.lock().await.push(*ack);
                        Ok(Vec::new())
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No data available right now
                Ok(Vec::new())
            }
            Err(e) => Err(ProtocolError::TransportError(e.to_string())),
        }
    }

    async fn connect(&mut self, peer_addr: &str) -> Result<(), ProtocolError> {
        let stream = TcpStream::connect(peer_addr)
            .await
            .map_err(|e| ProtocolError::ConnectionError(e.to_string()))?;

        *self.stream.lock().await = Some(stream);
        *self.peer_addr.lock().await = Some(peer_addr.to_string());
        self.is_connected.store(true, Ordering::Relaxed);
        self.metrics.record_connection_state_change();

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProtocolError> {
        *self.stream.lock().await = None;
        self.is_connected.store(false, Ordering::Relaxed);
        self.metrics.record_connection_state_change();

        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::task;

    #[tokio::test]
    async fn tcp_bidirectional_message_exchange() {
        let server_addr = "127.0.0.1:19001";

        let mut server = TcpTransport::new(server_addr.to_string());
        let mut client = TcpTransport::new("127.0.0.1:19002".to_string());

        // Spawn server listening task
        let _server_handle = {
            let mut srv = server.clone();
            task::spawn(async move {
                srv.listen().await.expect("listen");
            })
        };

        // Give server time to start listening
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Client connects
        client.connect(server_addr).await.expect("client connect");

        // Wait for server to process connection
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Create a test message
        let msg = ReplicationMessage::new(
            crate::distributed_transactions::TransactionEnvelope::new(
                "test_node".to_string(),
                1,
                "tx-1".to_string(),
                vec![],
                crate::transactions::ConsistencyContract::local(),
            ),
            "test_node".to_string(),
            "peer_node".to_string(),
            1,
        );

        // Client sends
        client.send(msg.clone()).await.expect("send");

        // Server receives (wait briefly for message to arrive)
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let received = server.receive().await.expect("receive");

        println!("Server received {} messages", received.len());
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].from_node, "test_node");
        assert_eq!(received[0].to_node, "peer_node");

        // Now try bidirectional: server sends back to client
        let response = ReplicationMessage::new(
            crate::distributed_transactions::TransactionEnvelope::new(
                "server_node".to_string(),
                1,
                "tx-2".to_string(),
                vec![],
                crate::transactions::ConsistencyContract::local(),
            ),
            "server_node".to_string(),
            "client_node".to_string(),
            1,
        );

        server.send(response.clone()).await.expect("server send");

        // Client receives (wait briefly for message to arrive)
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let client_received = client.receive().await.expect("client receive");

        println!("Client received {} messages", client_received.len());
        assert_eq!(client_received.len(), 1);
        assert_eq!(client_received[0].from_node, "server_node");
    }

    #[tokio::test]
    async fn tcp_connection_status_tracked() {
        let mut transport = TcpTransport::new("127.0.0.1:19003".to_string());

        assert!(!transport.is_connected());

        transport
            .connect("127.0.0.1:9999")
            .await
            .expect_err("should fail - nobody listening");

        assert!(!transport.is_connected());
        assert_eq!(transport.metrics().get_connection_state_changes(), 0);
    }

    #[test]
    fn bincode_serialization_roundtrip() {
        let msg = ReplicationMessage::new(
            crate::distributed_transactions::TransactionEnvelope::new(
                "test_node".to_string(),
                1,
                "tx-1".to_string(),
                vec![],
                crate::transactions::ConsistencyContract::local(),
            ),
            "test_node".to_string(),
            "peer_node".to_string(),
            1,
        );

        let serialized = serde_json::to_vec(&msg).expect("serialize");
        let deserialized: ReplicationMessage =
            serde_json::from_slice(&serialized).expect("deserialize");

        assert_eq!(msg.from_node, deserialized.from_node);
        assert_eq!(msg.to_node, deserialized.to_node);
        assert_eq!(msg.sequence_number, deserialized.sequence_number);
    }

    #[test]
    fn bincode_serialization_with_operation() {
        use crate::convergence::VectorClock;
        use crate::transactions::{Operation, OperationCommand, OperationId, StateVersion};
        use serde_json::json;
        use std::collections::HashMap;

        let mut fields = HashMap::new();
        fields.insert("key1".to_string(), json!("value1"));

        let mut parent_vc = VectorClock::new();
        parent_vc.increment("node_a");
        let parent_version = StateVersion::new(parent_vc, "hash_initial_a".to_string());

        let op = Operation::new(
            OperationId::new("node_a".to_string(), 1),
            parent_version.clone(),
            "set_operation".to_string(),
            OperationCommand {
                op_type: "set".to_string(),
                collection: "items".to_string(),
                record_id: "item1".to_string(),
                fields,
            },
            "node_a".to_string(),
        );

        let msg = ReplicationMessage::new(
            crate::distributed_transactions::TransactionEnvelope::new(
                "node_a".to_string(),
                1,
                "tx-1".to_string(),
                vec![op],
                crate::transactions::ConsistencyContract::local(),
            ),
            "node_a".to_string(),
            "node_b".to_string(),
            1,
        );

        let serialized = serde_json::to_vec(&msg).expect("serialize");
        println!("Serialized {} bytes", serialized.len());

        let deserialized: ReplicationMessage =
            serde_json::from_slice(&serialized).expect("deserialize");

        assert_eq!(msg.from_node, deserialized.from_node);
        assert_eq!(msg.to_node, deserialized.to_node);
        assert_eq!(deserialized.envelope.operations.len(), 1);
    }
}
