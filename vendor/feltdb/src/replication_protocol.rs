/// High-level replication protocol abstraction
///
/// Decouples ReplicationMessage handling from transport details.
/// Works with typed messages (ReplicationMessage) rather than raw bytes.
use crate::distributed_transactions::ReplicationMessage;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Protocol-level transport abstraction that works with ReplicationMessage
#[async_trait::async_trait]
pub trait ProtocolTransport: Send + Sync {
    /// Send a replication message to a peer
    async fn send(&self, message: ReplicationMessage) -> Result<(), ProtocolError>;

    /// Receive replication messages (non-blocking, returns empty if none available)
    async fn receive(&self) -> Result<Vec<ReplicationMessage>, ProtocolError>;

    /// Connect to a peer (for real network transports)
    async fn connect(&mut self, peer_addr: &str) -> Result<(), ProtocolError>;

    /// Disconnect from a peer (for real network transports)
    async fn disconnect(&mut self) -> Result<(), ProtocolError>;

    /// Check if connected to a peer
    fn is_connected(&self) -> bool;
}

/// Protocol errors
#[derive(Clone, Debug)]
pub enum ProtocolError {
    SerializationError(String),
    TransportError(String),
    ConnectionError(String),
    Timeout,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SerializationError(s) => write!(f, "serialization error: {}", s),
            Self::TransportError(s) => write!(f, "transport error: {}", s),
            Self::ConnectionError(s) => write!(f, "connection error: {}", s),
            Self::Timeout => write!(f, "timeout"),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Metrics for monitoring replication protocol
#[derive(Clone, Debug, Default)]
pub struct ProtocolMetrics {
    messages_sent: Arc<AtomicU64>,
    messages_received: Arc<AtomicU64>,
    bytes_sent: Arc<AtomicU64>,
    bytes_received: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
    connection_state_changes: Arc<AtomicU64>,
}

impl ProtocolMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_send(&self, bytes: usize) {
        self.messages_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn record_receive(&self, bytes: usize) {
        self.messages_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_connection_state_change(&self) {
        self.connection_state_changes
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn get_messages_sent(&self) -> u64 {
        self.messages_sent.load(Ordering::Relaxed)
    }

    pub fn get_messages_received(&self) -> u64 {
        self.messages_received.load(Ordering::Relaxed)
    }

    pub fn get_bytes_sent(&self) -> u64 {
        self.bytes_sent.load(Ordering::Relaxed)
    }

    pub fn get_bytes_received(&self) -> u64 {
        self.bytes_received.load(Ordering::Relaxed)
    }

    pub fn get_errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }

    pub fn get_connection_state_changes(&self) -> u64 {
        self.connection_state_changes.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_track_messages() {
        let metrics = ProtocolMetrics::new();
        metrics.record_send(1024);
        metrics.record_send(2048);

        assert_eq!(metrics.get_messages_sent(), 2);
        assert_eq!(metrics.get_bytes_sent(), 3072);
    }

    #[test]
    fn metrics_track_errors() {
        let metrics = ProtocolMetrics::new();
        metrics.record_error();
        metrics.record_error();

        assert_eq!(metrics.get_errors(), 2);
    }
}
