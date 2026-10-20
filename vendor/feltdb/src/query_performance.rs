/// Query Performance Tracking & Latency Contracts
///
/// Manages query performance benchmarks and latency SLAs:
/// - Query latency tracking (p50, p95, p99)
/// - Latency contract enforcement
/// - Performance degradation detection
/// - Query optimization recommendations
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyContract {
    pub query_type: String,
    pub p50_target_us: u64,
    pub p95_target_us: u64,
    pub p99_target_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryMetric {
    pub query_type: String,
    pub duration_us: u64,
    pub timestamp: u64,
    pub index_name: String,
    pub result_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryLatencyStats {
    pub query_type: String,
    pub count: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub min_us: u64,
    pub max_us: u64,
    pub avg_us: u64,
}

#[derive(Debug, Clone)]
struct QueryTimer {
    start: Instant,
}

impl QueryTimer {
    fn new() -> Self {
        QueryTimer {
            start: Instant::now(),
        }
    }

    fn elapsed_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }
}

pub struct QueryPerformanceTracker {
    contracts: HashMap<String, LatencyContract>,
    metrics: Vec<QueryMetric>,
    violations: Vec<String>,
}

impl QueryPerformanceTracker {
    pub fn new() -> Self {
        QueryPerformanceTracker {
            contracts: HashMap::new(),
            metrics: Vec::new(),
            violations: Vec::new(),
        }
    }

    pub fn register_contract(&mut self, contract: LatencyContract) {
        self.contracts.insert(contract.query_type.clone(), contract);
    }

    pub fn start_timer(&self) -> QueryTimer {
        QueryTimer::new()
    }

    pub fn record_query(
        &mut self,
        query_type: String,
        index_name: String,
        result_count: usize,
        duration_us: u64,
    ) {
        let metric = QueryMetric {
            query_type: query_type.clone(),
            duration_us,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            index_name,
            result_count,
        };

        if let Some(contract) = self.contracts.get(&query_type) {
            if duration_us > contract.p99_target_us {
                self.violations.push(format!(
                    "Query {} violated p99 contract: {} us > {} us",
                    query_type, duration_us, contract.p99_target_us
                ));
            }
        }

        self.metrics.push(metric);
    }

    pub fn get_stats(&self, query_type: &str) -> Option<QueryLatencyStats> {
        let relevant: Vec<&QueryMetric> = self
            .metrics
            .iter()
            .filter(|m| m.query_type == query_type)
            .collect();

        if relevant.is_empty() {
            return None;
        }

        let mut durations: Vec<u64> = relevant.iter().map(|m| m.duration_us).collect();
        durations.sort();

        let count = durations.len() as u64;
        let sum: u64 = durations.iter().sum();
        let avg = sum / count;

        let p50_idx = (count / 2) as usize;
        let p95_idx = ((count * 95) / 100) as usize;
        let p99_idx = ((count * 99) / 100) as usize;

        Some(QueryLatencyStats {
            query_type: query_type.to_string(),
            count,
            p50_us: durations.get(p50_idx).copied().unwrap_or(0),
            p95_us: durations.get(p95_idx).copied().unwrap_or(0),
            p99_us: durations.get(p99_idx).copied().unwrap_or(0),
            min_us: *durations.first().unwrap_or(&0),
            max_us: *durations.last().unwrap_or(&0),
            avg_us: avg,
        })
    }

    pub fn get_violations(&self) -> Vec<String> {
        self.violations.clone()
    }

    pub fn clear_violations(&mut self) {
        self.violations.clear();
    }

    pub fn export_metrics_csv(&self) -> String {
        let mut csv = String::from("timestamp,query_type,duration_us,index_name,result_count\n");
        for metric in &self.metrics {
            csv.push_str(&format!(
                "{},{},{},{},{}\n",
                metric.timestamp,
                metric.query_type,
                metric.duration_us,
                metric.index_name,
                metric.result_count
            ));
        }
        csv
    }
}

impl Default for QueryPerformanceTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_latency_contract() {
        let mut tracker = QueryPerformanceTracker::new();
        tracker.register_contract(LatencyContract {
            query_type: "index_lookup".to_string(),
            p50_target_us: 100,
            p95_target_us: 500,
            p99_target_us: 1000,
        });

        let timer = tracker.start_timer();
        std::thread::sleep(Duration::from_micros(50));
        let elapsed = timer.elapsed_us();
        // A sleep lasts at least as long as asked, and on a loaded machine
        // arbitrarily longer, so only the lower bound is a property.
        assert!(elapsed >= 50, "timer measured {elapsed}us for a 50us sleep");

        tracker.record_query("index_lookup".to_string(), "idx_1".to_string(), 10, elapsed);

        let stats = tracker.get_stats("index_lookup").unwrap();
        assert_eq!(stats.count, 1);
        assert_eq!(stats.p50_us, elapsed);
    }

    #[test]
    fn test_violation_detection() {
        let mut tracker = QueryPerformanceTracker::new();
        tracker.register_contract(LatencyContract {
            query_type: "slow_query".to_string(),
            p50_target_us: 100,
            p95_target_us: 200,
            p99_target_us: 500,
        });

        tracker.record_query("slow_query".to_string(), "idx_1".to_string(), 100, 600);

        let violations = tracker.get_violations();
        assert_eq!(violations.len(), 1);
    }
}
