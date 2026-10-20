/// Adversarial Transport Layer (Gate 8)
///
/// Network fault injection that proves FeltDB converges despite:
///   delivery delays, reordering, duplication, drops,
///   partitions, asymmetric connectivity, and arbitrary restarts
///
/// Critical constraint: Transport has NO knowledge of FeltDB's replication logic.
/// Transport only controls what happens to a message:
///   send → maybe delay → maybe duplicate → maybe drop → eventually deliver
///
/// FeltDB must converge itself. Simulator cannot help.
use std::collections::{HashMap, VecDeque};

/// Reproducible fault seed
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct FaultSeed(pub u64);

/// Unique message identifier (for tracking through faults)
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct MessageId(pub u64);

/// Node identifier
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct NodeId(pub u64);

/// Connection between two nodes
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ConnectionId {
    pub from: NodeId,
    pub to: NodeId,
}

/// State of a network link
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkState {
    /// Connected, messages delivered
    Connected,
    /// Partitioned, all messages dropped
    Partitioned,
}

/// Explicit network event (deterministic from seed)
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkEvent {
    /// Delay a message by N ticks
    Delay { message_id: MessageId, ticks: u64 },
    /// Drop a message permanently
    Drop { message_id: MessageId },
    /// Duplicate a message
    Duplicate { message_id: MessageId },
    /// Partition a directional link
    Partition { from: NodeId, to: NodeId },
    /// Heal a directional link
    Heal { from: NodeId, to: NodeId },
    /// Reorder messages on a connection
    Reorder { connection: ConnectionId },
}

/// Deterministic network schedule (fully reproducible from seed)
#[derive(Clone, Debug)]
pub struct NetworkSchedule {
    pub seed: FaultSeed,
    pub events: Vec<NetworkEvent>,
}

impl NetworkSchedule {
    pub fn new(seed: FaultSeed) -> Self {
        Self {
            seed,
            events: Vec::new(),
        }
    }

    pub fn add_event(&mut self, event: NetworkEvent) {
        self.events.push(event);
    }

    pub fn get_seed(&self) -> FaultSeed {
        self.seed
    }
}

/// In-flight message
#[derive(Clone, Debug)]
struct InFlightMessage {
    id: MessageId,
    from: NodeId,
    to: NodeId,
    payload: Vec<u8>,
    delivery_tick: u64,
}

/// Connectivity matrix for asymmetric partitions
#[derive(Clone, Debug)]
pub struct ConnectivityMatrix {
    links: HashMap<ConnectionId, LinkState>,
}

impl ConnectivityMatrix {
    pub fn new() -> Self {
        Self {
            links: HashMap::new(),
        }
    }

    pub fn can_reach(&self, from: NodeId, to: NodeId) -> bool {
        let conn = ConnectionId { from, to };
        matches!(self.links.get(&conn), Some(LinkState::Connected) | None)
    }

    pub fn partition(&mut self, from: NodeId, to: NodeId) {
        self.links
            .insert(ConnectionId { from, to }, LinkState::Partitioned);
    }

    pub fn heal(&mut self, from: NodeId, to: NodeId) {
        self.links
            .insert(ConnectionId { from, to }, LinkState::Connected);
    }

    pub fn get_state(&self, from: NodeId, to: NodeId) -> LinkState {
        let conn = ConnectionId { from, to };
        *self.links.get(&conn).unwrap_or(&LinkState::Connected)
    }
}

impl Default for ConnectivityMatrix {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait: Replication transport (decoupled from FeltDB logic)
pub trait ReplicationTransport: Send + Sync {
    /// Send a message from one node to another
    fn send(&mut self, from: NodeId, to: NodeId, message: Vec<u8>) -> MessageId;

    /// Advance simulation by one tick
    fn tick(&mut self);

    /// Receive all messages waiting for a node
    fn receive(&mut self, node: NodeId) -> Vec<(NodeId, Vec<u8>)>;

    /// Apply a fault event
    fn apply_fault(&mut self, event: NetworkEvent);

    /// Get current tick
    fn current_tick(&self) -> u64;

    /// Get connectivity status
    fn get_connectivity(&self, from: NodeId, to: NodeId) -> LinkState;
}

/// In-memory transport with no faults (baseline)
pub struct InMemoryTransport {
    current_tick: u64,
    message_counter: u64,
    queues: HashMap<NodeId, VecDeque<(NodeId, Vec<u8>)>>,
}

impl InMemoryTransport {
    pub fn new() -> Self {
        Self {
            current_tick: 0,
            message_counter: 0,
            queues: HashMap::new(),
        }
    }
}

impl Default for InMemoryTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplicationTransport for InMemoryTransport {
    fn send(&mut self, from: NodeId, to: NodeId, message: Vec<u8>) -> MessageId {
        let id = MessageId(self.message_counter);
        self.message_counter += 1;

        self.queues
            .entry(to)
            .or_insert_with(VecDeque::new)
            .push_back((from, message));

        id
    }

    fn tick(&mut self) {
        self.current_tick += 1;
    }

    fn receive(&mut self, node: NodeId) -> Vec<(NodeId, Vec<u8>)> {
        self.queues
            .remove(&node)
            .map(|q| q.into_iter().collect())
            .unwrap_or_default()
    }

    fn apply_fault(&mut self, _event: NetworkEvent) {
        // No-op for in-memory transport
    }

    fn current_tick(&self) -> u64 {
        self.current_tick
    }

    fn get_connectivity(&self, _from: NodeId, _to: NodeId) -> LinkState {
        LinkState::Connected
    }
}

/// Adversarial transport with full fault injection
pub struct AdversarialTransport {
    current_tick: u64,
    message_counter: u64,
    in_flight: Vec<InFlightMessage>,
    delivered_queues: HashMap<NodeId, VecDeque<(NodeId, Vec<u8>)>>,
    connectivity: ConnectivityMatrix,
    dropped_messages: std::collections::HashSet<MessageId>,
}

impl AdversarialTransport {
    pub fn new() -> Self {
        Self {
            current_tick: 0,
            message_counter: 0,
            in_flight: Vec::new(),
            delivered_queues: HashMap::new(),
            connectivity: ConnectivityMatrix::new(),
            dropped_messages: std::collections::HashSet::new(),
        }
    }

    pub fn get_in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    pub fn get_delivered_count(&self, node: NodeId) -> usize {
        self.delivered_queues
            .get(&node)
            .map(|q| q.len())
            .unwrap_or(0)
    }
}

impl Default for AdversarialTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplicationTransport for AdversarialTransport {
    fn send(&mut self, from: NodeId, to: NodeId, message: Vec<u8>) -> MessageId {
        let id = MessageId(self.message_counter);
        self.message_counter += 1;

        // Check if link is partitioned
        if !self.connectivity.can_reach(from, to) {
            // Message is dropped by partition
            self.dropped_messages.insert(id);
            return id;
        }

        // Message enters in-flight, scheduled for immediate delivery
        self.in_flight.push(InFlightMessage {
            id,
            from,
            to,
            payload: message,
            delivery_tick: self.current_tick,
        });

        id
    }

    fn tick(&mut self) {
        self.current_tick += 1;

        // Deliver messages that are ready
        let mut to_deliver = Vec::new();
        let mut remaining = Vec::new();

        for msg in self.in_flight.drain(..) {
            if msg.delivery_tick <= self.current_tick {
                to_deliver.push(msg);
            } else {
                remaining.push(msg);
            }
        }

        self.in_flight = remaining;

        // Queue delivered messages
        for msg in to_deliver {
            if !self.dropped_messages.contains(&msg.id) {
                self.delivered_queues
                    .entry(msg.to)
                    .or_insert_with(VecDeque::new)
                    .push_back((msg.from, msg.payload));
            }
        }
    }

    fn receive(&mut self, node: NodeId) -> Vec<(NodeId, Vec<u8>)> {
        self.delivered_queues
            .remove(&node)
            .map(|q| q.into_iter().collect())
            .unwrap_or_default()
    }

    fn apply_fault(&mut self, event: NetworkEvent) {
        match event {
            NetworkEvent::Delay { message_id, ticks } => {
                // Find message and delay delivery
                for msg in &mut self.in_flight {
                    if msg.id == message_id {
                        msg.delivery_tick += ticks;
                        break;
                    }
                }
            }
            NetworkEvent::Drop { message_id } => {
                self.dropped_messages.insert(message_id);
            }
            NetworkEvent::Duplicate { message_id } => {
                // Clone the in-flight message
                if let Some(orig) = self.in_flight.iter().find(|m| m.id == message_id) {
                    let dup = InFlightMessage {
                        id: MessageId(self.message_counter),
                        from: orig.from,
                        to: orig.to,
                        payload: orig.payload.clone(),
                        delivery_tick: orig.delivery_tick,
                    };
                    self.message_counter += 1;
                    self.in_flight.push(dup);
                }
            }
            NetworkEvent::Partition { from, to } => {
                self.connectivity.partition(from, to);
            }
            NetworkEvent::Heal { from, to } => {
                self.connectivity.heal(from, to);
            }
            NetworkEvent::Reorder { connection } => {
                // Reorder messages on this connection
                let mut msgs_on_conn: Vec<_> = self
                    .in_flight
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| m.from == connection.from && m.to == connection.to)
                    .collect();

                if msgs_on_conn.len() > 1 {
                    // Reverse order of messages on this connection
                    msgs_on_conn.reverse();
                    let indices: Vec<_> = msgs_on_conn.iter().map(|(i, _)| *i).collect();

                    for (new_pos, &old_pos) in indices.iter().enumerate() {
                        if new_pos != old_pos {
                            self.in_flight.swap(new_pos, old_pos);
                        }
                    }
                }
            }
        }
    }

    fn current_tick(&self) -> u64 {
        self.current_tick
    }

    fn get_connectivity(&self, from: NodeId, to: NodeId) -> LinkState {
        self.connectivity.get_state(from, to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_memory_transport_basic() {
        let mut transport = InMemoryTransport::new();

        let n1 = NodeId(1);
        let n2 = NodeId(2);

        transport.send(n1, n2, vec![1, 2, 3]);
        let messages = transport.receive(n2);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].0, n1);
        assert_eq!(messages[0].1, vec![1, 2, 3]);
    }

    #[test]
    fn test_adversarial_transport_basic() {
        let mut transport = AdversarialTransport::new();

        let n1 = NodeId(1);
        let n2 = NodeId(2);

        let msg_id = transport.send(n1, n2, vec![1, 2, 3]);
        assert!(msg_id.0 >= 0);

        transport.tick();
        let messages = transport.receive(n2);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].0, n1);
    }

    #[test]
    fn test_partition() {
        let mut transport = AdversarialTransport::new();

        let n1 = NodeId(1);
        let n2 = NodeId(2);

        // Partition n1 → n2
        transport.apply_fault(NetworkEvent::Partition { from: n1, to: n2 });

        // Message should be dropped
        let _msg_id = transport.send(n1, n2, vec![1, 2, 3]);
        transport.tick();

        let messages = transport.receive(n2);
        assert_eq!(messages.len(), 0);
    }

    #[test]
    fn test_delay() {
        let mut transport = AdversarialTransport::new();

        let n1 = NodeId(1);
        let n2 = NodeId(2);

        let msg_id = transport.send(n1, n2, vec![1, 2, 3]);

        // Delay the message by 5 ticks
        transport.apply_fault(NetworkEvent::Delay {
            message_id: msg_id,
            ticks: 5,
        });

        // Tick 1-4: no messages
        for _ in 0..4 {
            transport.tick();
            let messages = transport.receive(n2);
            assert_eq!(messages.len(), 0);
        }

        // Tick 5: message delivered
        transport.tick();
        let messages = transport.receive(n2);
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn test_duplicate() {
        let mut transport = AdversarialTransport::new();

        let n1 = NodeId(1);
        let n2 = NodeId(2);

        let msg_id = transport.send(n1, n2, vec![1, 2, 3]);

        // Duplicate the message
        transport.apply_fault(NetworkEvent::Duplicate { message_id: msg_id });

        transport.tick();
        let messages = transport.receive(n2);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].1, vec![1, 2, 3]);
        assert_eq!(messages[1].1, vec![1, 2, 3]);
    }

    #[test]
    fn test_drop() {
        let mut transport = AdversarialTransport::new();

        let n1 = NodeId(1);
        let n2 = NodeId(2);

        let msg_id = transport.send(n1, n2, vec![1, 2, 3]);

        transport.apply_fault(NetworkEvent::Drop { message_id: msg_id });

        transport.tick();
        let messages = transport.receive(n2);

        assert_eq!(messages.len(), 0);
    }

    #[test]
    fn test_asymmetric_partition() {
        let mut transport = AdversarialTransport::new();

        let n1 = NodeId(1);
        let n2 = NodeId(2);

        // Partition n1 → n2 only (asymmetric)
        transport.apply_fault(NetworkEvent::Partition { from: n1, to: n2 });

        // n1 → n2 blocked
        let _msg_id = transport.send(n1, n2, vec![1, 2, 3]);
        transport.tick();
        assert_eq!(transport.receive(n2).len(), 0);

        // n2 → n1 allowed
        let msg_id = transport.send(n2, n1, vec![4, 5, 6]);
        assert!(msg_id.0 >= 0);
        transport.tick();
        assert_eq!(transport.receive(n1).len(), 1);
    }

    #[test]
    fn test_connectivity_matrix() {
        let mut connectivity = ConnectivityMatrix::new();

        let n1 = NodeId(1);
        let n2 = NodeId(2);

        // Initially connected
        assert!(connectivity.can_reach(n1, n2));

        // Partition
        connectivity.partition(n1, n2);
        assert!(!connectivity.can_reach(n1, n2));

        // Heal
        connectivity.heal(n1, n2);
        assert!(connectivity.can_reach(n1, n2));
    }
}
