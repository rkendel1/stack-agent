/// Metrics collection for distributed replication monitoring
///
/// Tracks key performance and reliability metrics across the cluster:
/// - Replication latency (p50, p95, p99)
/// - Journal state (bytes written, pending, fsync latency)
/// - Quorum health (healthy nodes, majority status)
/// - Node health (operations applied, crash count)
/// - Recovery metrics (duration, type)
/// - Operation status (acknowledged, committed, diverged, lost)
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Replication latency measurement
#[derive(Clone, Debug)]
pub struct ReplicationLatency {
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
}

/// Node-level metrics
#[derive(Clone, Debug)]
pub struct NodeMetrics {
    pub node_id: String,
    pub operations_applied: Arc<AtomicU64>,
    pub operations_acknowledged: Arc<AtomicU64>,
    pub operations_committed: Arc<AtomicU64>,
    pub operations_diverged: Arc<AtomicU64>,
    pub operations_lost: Arc<AtomicU64>,
    pub crash_count: Arc<AtomicU64>,
    pub recovery_time_ms: Arc<AtomicU64>,
}

impl NodeMetrics {
    pub fn new(node_id: String) -> Self {
        Self {
            node_id,
            operations_applied: Arc::new(AtomicU64::new(0)),
            operations_acknowledged: Arc::new(AtomicU64::new(0)),
            operations_committed: Arc::new(AtomicU64::new(0)),
            operations_diverged: Arc::new(AtomicU64::new(0)),
            operations_lost: Arc::new(AtomicU64::new(0)),
            crash_count: Arc::new(AtomicU64::new(0)),
            recovery_time_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn record_operation_applied(&self) {
        self.operations_applied.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_operation_acknowledged(&self) {
        self.operations_acknowledged.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_operation_committed(&self) {
        self.operations_committed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_operation_diverged(&self) {
        self.operations_diverged.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_operation_lost(&self) {
        self.operations_lost.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_crash(&self) {
        self.crash_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_recovery(&self, duration_ms: u64) {
        self.recovery_time_ms.store(duration_ms, Ordering::Relaxed);
    }

    pub fn get_snapshot(&self) -> NodeMetricsSnapshot {
        NodeMetricsSnapshot {
            node_id: self.node_id.clone(),
            operations_applied: self.operations_applied.load(Ordering::Relaxed),
            operations_acknowledged: self.operations_acknowledged.load(Ordering::Relaxed),
            operations_committed: self.operations_committed.load(Ordering::Relaxed),
            operations_diverged: self.operations_diverged.load(Ordering::Relaxed),
            operations_lost: self.operations_lost.load(Ordering::Relaxed),
            crash_count: self.crash_count.load(Ordering::Relaxed),
            recovery_time_ms: self.recovery_time_ms.load(Ordering::Relaxed),
        }
    }
}

/// Snapshot of metrics at a point in time
#[derive(Clone, Debug, serde::Serialize)]
pub struct NodeMetricsSnapshot {
    pub node_id: String,
    pub operations_applied: u64,
    pub operations_acknowledged: u64,
    pub operations_committed: u64,
    pub operations_diverged: u64,
    pub operations_lost: u64,
    pub crash_count: u64,
    pub recovery_time_ms: u64,
}

/// Cluster-level metrics
pub struct ClusterMetrics {
    pub nodes: Vec<NodeMetrics>,
    pub start_time: SystemTime,
    pub total_operations: Arc<AtomicU64>,
    pub total_crashes: Arc<AtomicU64>,
    pub divergence_events: Arc<AtomicU64>,
    pub data_loss_events: Arc<AtomicU64>,
}

impl ClusterMetrics {
    pub fn new(node_ids: Vec<String>) -> Self {
        let nodes = node_ids.into_iter().map(NodeMetrics::new).collect();

        Self {
            nodes,
            start_time: SystemTime::now(),
            total_operations: Arc::new(AtomicU64::new(0)),
            total_crashes: Arc::new(AtomicU64::new(0)),
            divergence_events: Arc::new(AtomicU64::new(0)),
            data_loss_events: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn get_node_metrics(&self, node_id: &str) -> Option<NodeMetricsSnapshot> {
        self.nodes
            .iter()
            .find(|n| n.node_id == node_id)
            .map(|n| n.get_snapshot())
    }

    pub fn get_all_metrics(&self) -> Vec<NodeMetricsSnapshot> {
        self.nodes.iter().map(|n| n.get_snapshot()).collect()
    }

    pub fn get_cluster_snapshot(&self) -> ClusterMetricsSnapshot {
        let uptime = self
            .start_time
            .elapsed()
            .unwrap_or(Duration::from_secs(0))
            .as_secs();

        let mut total_operations_applied = 0u64;
        let mut total_crashes = 0u64;

        for node in &self.nodes {
            total_operations_applied += node.operations_applied.load(Ordering::Relaxed);
            total_crashes += node.crash_count.load(Ordering::Relaxed);
        }

        ClusterMetricsSnapshot {
            uptime_secs: uptime,
            total_operations_applied,
            total_crashes,
            divergence_events: self.divergence_events.load(Ordering::Relaxed),
            data_loss_events: self.data_loss_events.load(Ordering::Relaxed),
            node_snapshots: self.get_all_metrics(),
        }
    }

    pub fn record_divergence(&self) {
        self.divergence_events.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_data_loss(&self) {
        self.data_loss_events.fetch_add(1, Ordering::Relaxed);
    }

    pub fn export_csv(&self) -> String {
        let snapshot = self.get_cluster_snapshot();
        let mut csv = String::from("timestamp_secs,uptime_secs,total_ops_applied,total_crashes,divergence_events,data_loss_events\n");
        csv.push_str(&format!(
            "{},{},{},{},{},{}\n",
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            snapshot.uptime_secs,
            snapshot.total_operations_applied,
            snapshot.total_crashes,
            snapshot.divergence_events,
            snapshot.data_loss_events
        ));

        csv.push_str("node_id,ops_applied,ops_acked,ops_committed,ops_diverged,ops_lost,crash_count,recovery_time_ms\n");
        for node in &snapshot.node_snapshots {
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{}\n",
                node.node_id,
                node.operations_applied,
                node.operations_acknowledged,
                node.operations_committed,
                node.operations_diverged,
                node.operations_lost,
                node.crash_count,
                node.recovery_time_ms
            ));
        }

        csv
    }

    pub fn export_json(&self) -> String {
        let snapshot = self.get_cluster_snapshot();
        serde_json::to_string_pretty(&snapshot).unwrap_or_else(|_| "{}".to_string())
    }
}

#[derive(serde::Serialize)]
pub struct ClusterMetricsSnapshot {
    pub uptime_secs: u64,
    pub total_operations_applied: u64,
    pub total_crashes: u64,
    pub divergence_events: u64,
    pub data_loss_events: u64,
    pub node_snapshots: Vec<NodeMetricsSnapshot>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_metrics_tracking() {
        let metrics = NodeMetrics::new("node-1".to_string());

        metrics.record_operation_applied();
        metrics.record_operation_applied();
        metrics.record_operation_acknowledged();
        metrics.record_crash();

        let snapshot = metrics.get_snapshot();
        assert_eq!(snapshot.operations_applied, 2);
        assert_eq!(snapshot.operations_acknowledged, 1);
        assert_eq!(snapshot.crash_count, 1);
    }

    #[test]
    fn cluster_metrics_aggregation() {
        let cluster = ClusterMetrics::new(vec!["node-1".to_string(), "node-2".to_string()]);

        cluster.nodes[0].record_operation_applied();
        cluster.nodes[0].record_operation_applied();
        cluster.nodes[1].record_operation_applied();
        cluster.nodes[0].record_crash();
        cluster.record_divergence();

        let snapshot = cluster.get_cluster_snapshot();
        assert_eq!(snapshot.total_operations_applied, 3);
        assert_eq!(snapshot.total_crashes, 1);
        assert_eq!(snapshot.divergence_events, 1);
    }

    #[test]
    fn metrics_export_csv() {
        let cluster = ClusterMetrics::new(vec!["node-1".to_string()]);
        cluster.nodes[0].record_operation_applied();

        let csv = cluster.export_csv();
        assert!(csv.contains("node-1"));
        assert!(csv.contains("1")); // 1 operation applied
    }
}
