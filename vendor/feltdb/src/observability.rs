/// Observability Layer
///
/// Comprehensive monitoring and observability:
/// - Replication lag tracking
/// - Convergence status
/// - Query latency monitoring
/// - Rejected operations tracking
/// - Index health metrics
/// - Recovery state visibility
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationLagMetric {
    pub source_node: String,
    pub target_node: String,
    pub lag_ms: u64,
    pub pending_operations: u64,
    pub last_update: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceStatus {
    pub node_id: String,
    pub status: String,
    pub synchronized_peers: Vec<String>,
    pub diverged_peers: Vec<String>,
    pub convergence_percentage: f64,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedOperation {
    pub operation_id: String,
    pub reason: String,
    pub timestamp: u64,
    pub node_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexHealth {
    pub index_name: String,
    pub status: String,
    pub entries_count: u64,
    pub memory_bytes: u64,
    pub last_compaction: u64,
    pub fragmentation: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryState {
    pub node_id: String,
    pub status: String,
    pub operations_replayed: u64,
    pub operations_remaining: u64,
    pub recovery_start: u64,
    pub estimated_completion: u64,
    pub progress_percentage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub timestamp: u64,
    pub overall_status: String,
    pub replication_lag: Vec<ReplicationLagMetric>,
    pub convergence: Vec<ConvergenceStatus>,
    pub rejected_operations: Vec<RejectedOperation>,
    pub index_health: Vec<IndexHealth>,
    pub recovery_states: Vec<RecoveryState>,
    pub query_latency_p99: u64,
    pub operation_throughput: u64,
}

pub struct ObservabilityManager {
    node_id: String,
    replication_lags: HashMap<String, ReplicationLagMetric>,
    convergence_status: HashMap<String, ConvergenceStatus>,
    rejected_operations: Vec<RejectedOperation>,
    index_health: HashMap<String, IndexHealth>,
    recovery_states: HashMap<String, RecoveryState>,

    query_latency_p99: Arc<AtomicU64>,
    operation_throughput: Arc<AtomicU64>,
    total_rejected: Arc<AtomicU64>,
}

impl ObservabilityManager {
    pub fn new(node_id: String) -> Self {
        ObservabilityManager {
            node_id,
            replication_lags: HashMap::new(),
            convergence_status: HashMap::new(),
            rejected_operations: Vec::new(),
            index_health: HashMap::new(),
            recovery_states: HashMap::new(),
            query_latency_p99: Arc::new(AtomicU64::new(0)),
            operation_throughput: Arc::new(AtomicU64::new(0)),
            total_rejected: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn record_replication_lag(
        &mut self,
        source: String,
        target: String,
        lag_ms: u64,
        pending: u64,
    ) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.replication_lags.insert(
            format!("{}:{}", source.clone(), target.clone()),
            ReplicationLagMetric {
                source_node: source,
                target_node: target,
                lag_ms,
                pending_operations: pending,
                last_update: now,
            },
        );
    }

    pub fn record_convergence_status(
        &mut self,
        node_id: String,
        synchronized_peers: Vec<String>,
        diverged_peers: Vec<String>,
    ) {
        let total = synchronized_peers.len() + diverged_peers.len();
        let convergence_percentage = if total > 0 {
            (synchronized_peers.len() as f64 / total as f64) * 100.0
        } else {
            100.0
        };

        let status = if diverged_peers.is_empty() {
            "converged"
        } else {
            "diverged"
        };

        self.convergence_status.insert(
            node_id.clone(),
            ConvergenceStatus {
                node_id,
                status: status.to_string(),
                synchronized_peers,
                diverged_peers,
                convergence_percentage,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            },
        );
    }

    pub fn record_rejected_operation(&mut self, operation_id: String, reason: String) {
        self.rejected_operations.push(RejectedOperation {
            operation_id,
            reason,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            node_id: self.node_id.clone(),
        });

        self.total_rejected.fetch_add(1, Ordering::Relaxed);

        if self.rejected_operations.len() > 10000 {
            self.rejected_operations.remove(0);
        }
    }

    pub fn record_index_health(
        &mut self,
        index_name: String,
        entries: u64,
        memory_bytes: u64,
        fragmentation: f64,
    ) {
        self.index_health.insert(
            index_name.clone(),
            IndexHealth {
                index_name,
                status: "healthy".to_string(),
                entries_count: entries,
                memory_bytes,
                last_compaction: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                fragmentation,
            },
        );
    }

    pub fn record_recovery_state(
        &mut self,
        node_id: String,
        replayed: u64,
        remaining: u64,
        start_time: u64,
    ) {
        let progress_percentage = if replayed + remaining > 0 {
            (replayed as f64 / (replayed + remaining) as f64) * 100.0
        } else {
            100.0
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let elapsed = now - start_time;
        let rate = if elapsed > 0 {
            replayed / elapsed.max(1)
        } else {
            0
        };

        let estimated_completion = if rate > 0 {
            now + ((remaining / rate.max(1)) as u64)
        } else {
            now + 3600
        };

        self.recovery_states.insert(
            node_id.clone(),
            RecoveryState {
                node_id,
                status: if remaining == 0 {
                    "complete".to_string()
                } else {
                    "in_progress".to_string()
                },
                operations_replayed: replayed,
                operations_remaining: remaining,
                recovery_start: start_time,
                estimated_completion,
                progress_percentage,
            },
        );
    }

    pub fn record_query_latency(&self, p99_us: u64) {
        self.query_latency_p99.store(p99_us, Ordering::Relaxed);
    }

    pub fn record_operation_throughput(&self, ops_per_sec: u64) {
        self.operation_throughput
            .store(ops_per_sec, Ordering::Relaxed);
    }

    pub fn get_health_report(&self) -> HealthReport {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let overall_status = if self
            .convergence_status
            .values()
            .all(|c| c.diverged_peers.is_empty())
        {
            "healthy"
        } else {
            "degraded"
        };

        HealthReport {
            timestamp: now,
            overall_status: overall_status.to_string(),
            replication_lag: self.replication_lags.values().cloned().collect(),
            convergence: self.convergence_status.values().cloned().collect(),
            rejected_operations: self
                .rejected_operations
                .iter()
                .rev()
                .take(100)
                .cloned()
                .collect(),
            index_health: self.index_health.values().cloned().collect(),
            recovery_states: self.recovery_states.values().cloned().collect(),
            query_latency_p99: self.query_latency_p99.load(Ordering::Relaxed),
            operation_throughput: self.operation_throughput.load(Ordering::Relaxed),
        }
    }

    pub fn export_prometheus_metrics(&self) -> String {
        let report = self.get_health_report();
        let mut metrics = String::new();

        metrics.push_str(&format!(
            "feltdb_query_latency_p99_us {}\n",
            report.query_latency_p99
        ));
        metrics.push_str(&format!(
            "feltdb_operation_throughput_ops_per_sec {}\n",
            report.operation_throughput
        ));
        metrics.push_str(&format!(
            "feltdb_rejected_operations_total {}\n",
            self.total_rejected.load(Ordering::Relaxed)
        ));

        for lag in &report.replication_lag {
            metrics.push_str(&format!(
                "feltdb_replication_lag_ms{{source=\"{}\",target=\"{}\"}} {}\n",
                lag.source_node, lag.target_node, lag.lag_ms
            ));
        }

        metrics
    }
}

impl Default for ObservabilityManager {
    fn default() -> Self {
        Self::new("node".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replication_lag_tracking() {
        let mut obs = ObservabilityManager::new("node-1".to_string());
        obs.record_replication_lag("node-1".to_string(), "node-2".to_string(), 150, 50);

        let report = obs.get_health_report();
        assert_eq!(report.replication_lag.len(), 1);
        assert_eq!(report.replication_lag[0].lag_ms, 150);
    }

    #[test]
    fn test_convergence_tracking() {
        let mut obs = ObservabilityManager::new("node-1".to_string());
        obs.record_convergence_status(
            "node-1".to_string(),
            vec!["node-2".to_string(), "node-3".to_string()],
            vec![],
        );

        let report = obs.get_health_report();
        assert_eq!(report.overall_status, "healthy");
        assert_eq!(report.convergence[0].convergence_percentage, 100.0);
    }

    #[test]
    fn test_rejected_operations() {
        let mut obs = ObservabilityManager::new("node-1".to_string());
        obs.record_rejected_operation("op-1".to_string(), "conflict".to_string());
        obs.record_rejected_operation("op-2".to_string(), "timeout".to_string());

        let report = obs.get_health_report();
        assert_eq!(report.rejected_operations.len(), 2);
    }

    #[test]
    fn test_prometheus_export() {
        let mut obs = ObservabilityManager::new("node-1".to_string());
        obs.record_query_latency(1200);
        obs.record_operation_throughput(5000);

        let metrics = obs.export_prometheus_metrics();
        assert!(metrics.contains("feltdb_query_latency_p99_us 1200"));
        assert!(metrics.contains("feltdb_operation_throughput_ops_per_sec 5000"));
    }
}
