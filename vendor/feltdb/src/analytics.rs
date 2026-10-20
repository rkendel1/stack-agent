use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryTrace {
    pub timestamp: u64,
    pub fields: Vec<String>,
    pub operator: String,
    pub index_used: Option<String>,
    pub execution_time_ms: f64,
    pub rows_scanned: u64,
    pub result_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMetrics {
    pub name: String,
    pub reads: u64,
    pub writes: u64,
    pub hit_rate: f64,
    pub avg_query_time_ms: f64,
    pub avg_write_time_ms: f64,
    pub entries: usize,
    pub last_used_at: u64,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningRecommendation {
    pub rec_type: String, // "create_index", "drop_index", "optimize_index"
    pub priority: String, // "high", "medium", "low"
    pub index_name: Option<String>,
    pub reason: String,
    pub expected_speedup: Option<f64>,
    pub estimated_roi: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceReport {
    pub collection_name: String,
    pub period: String, // "hourly", "daily", "weekly"
    pub total_queries: usize,
    pub indexed_queries: usize,
    pub full_scans: usize,
    pub avg_query_time_ms: f64,
    pub slowest_query: Option<QueryTrace>,
    pub index_metrics: Vec<IndexMetrics>,
    pub recommendations: Vec<TuningRecommendation>,
    pub timestamp: u64,
}

pub struct IndexAnalytics {
    metrics: HashMap<String, IndexMetrics>,
    query_traces: Vec<QueryTrace>,
    field_query_count: HashMap<String, u64>,
    max_traces: usize,
}

impl IndexAnalytics {
    pub fn new() -> Self {
        IndexAnalytics {
            metrics: HashMap::new(),
            query_traces: Vec::new(),
            field_query_count: HashMap::new(),
            max_traces: 10000,
        }
    }

    pub fn record_index_usage(&mut self, index_name: &str, operation: &str, time_ms: f64) {
        let metric = self
            .metrics
            .entry(index_name.to_string())
            .or_insert_with(|| IndexMetrics {
                name: index_name.to_string(),
                reads: 0,
                writes: 0,
                hit_rate: 0.0,
                avg_query_time_ms: 0.0,
                avg_write_time_ms: 0.0,
                entries: 0,
                last_used_at: current_time_ms(),
                created_at: current_time_ms(),
            });

        if operation == "read" {
            let old_avg = metric.avg_query_time_ms;
            metric.reads += 1;
            metric.avg_query_time_ms =
                (old_avg * (metric.reads - 1) as f64 + time_ms) / metric.reads as f64;
        } else {
            let old_avg = metric.avg_write_time_ms;
            metric.writes += 1;
            metric.avg_write_time_ms =
                (old_avg * (metric.writes - 1) as f64 + time_ms) / metric.writes as f64;
        }

        metric.last_used_at = current_time_ms();
    }

    pub fn record_query(
        &mut self,
        fields: Vec<String>,
        operator: String,
        execution_time_ms: f64,
        rows_scanned: u64,
        result_count: u64,
        index_used: Option<String>,
    ) {
        let trace = QueryTrace {
            timestamp: current_time_ms(),
            fields: fields.clone(),
            operator,
            index_used: index_used.clone(),
            execution_time_ms,
            rows_scanned,
            result_count,
        };

        self.query_traces.push(trace);
        if self.query_traces.len() > self.max_traces {
            self.query_traces.remove(0);
        }

        // Track field access patterns
        for field in &fields {
            *self.field_query_count.entry(field.clone()).or_insert(0) += 1;
        }

        // Update hit rate for used index
        if let Some(ref index_name) = index_used {
            if let Some(metric) = self.metrics.get_mut(index_name) {
                metric.reads += 1;
            }
        }
    }

    pub fn get_index_metrics(&self, index_name: &str) -> Option<IndexMetrics> {
        self.metrics.get(index_name).cloned()
    }

    pub fn get_all_metrics(&self) -> Vec<IndexMetrics> {
        self.metrics.values().cloned().collect()
    }

    pub fn get_slowest_queries(&self, limit: usize) -> Vec<QueryTrace> {
        let mut traces = self.query_traces.clone();
        traces.sort_by(|a, b| {
            b.execution_time_ms
                .partial_cmp(&a.execution_time_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        traces.truncate(limit);
        traces
    }

    pub fn get_frequent_fields(&self, limit: usize) -> Vec<(String, u64)> {
        let mut fields: Vec<_> = self
            .field_query_count
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        fields.sort_by(|a, b| b.1.cmp(&a.1));
        fields.truncate(limit);
        fields
    }

    fn calculate_hit_rate(&self, index_name: &str) -> f64 {
        let metric = match self.metrics.get(index_name) {
            Some(m) => m,
            None => return 0.0,
        };

        if metric.reads == 0 {
            return 0.0;
        }

        if self.query_traces.is_empty() {
            return 0.0;
        }

        let queries_using_index = self
            .query_traces
            .iter()
            .filter(|q| q.index_used.as_ref().map_or(false, |u| u == index_name))
            .count();

        let total_queries = self.query_traces.len();
        if total_queries == 0 {
            0.0
        } else {
            queries_using_index as f64 / total_queries as f64
        }
    }

    pub fn generate_report(&self, collection_name: &str, period: &str) -> PerformanceReport {
        let now = current_time_ms();
        let cutoff = match period {
            "hourly" => now - 3_600_000,
            "weekly" => now - 604_800_000,
            _ => now - 86_400_000, // daily
        };

        let recent_traces: Vec<_> = self
            .query_traces
            .iter()
            .filter(|t| t.timestamp > cutoff)
            .cloned()
            .collect();

        let indexed_queries = recent_traces
            .iter()
            .filter(|t| t.index_used.is_some())
            .count();
        let full_scans = recent_traces.len() - indexed_queries;

        let avg_query_time = if recent_traces.is_empty() {
            0.0
        } else {
            recent_traces
                .iter()
                .map(|t| t.execution_time_ms)
                .sum::<f64>()
                / recent_traces.len() as f64
        };

        let slowest_query = if recent_traces.is_empty() {
            None
        } else {
            let mut traces = recent_traces.clone();
            traces.sort_by(|a, b| {
                b.execution_time_ms
                    .partial_cmp(&a.execution_time_ms)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            traces.first().cloned()
        };

        let mut metrics = self.get_all_metrics();
        for metric in &mut metrics {
            metric.hit_rate = self.calculate_hit_rate(&metric.name);
        }

        let recommendations = self.generate_recommendations(&recent_traces, &metrics);

        PerformanceReport {
            collection_name: collection_name.to_string(),
            period: period.to_string(),
            total_queries: recent_traces.len(),
            indexed_queries,
            full_scans,
            avg_query_time_ms: avg_query_time,
            slowest_query,
            index_metrics: metrics,
            recommendations,
            timestamp: now,
        }
    }

    fn generate_recommendations(
        &self,
        traces: &[QueryTrace],
        metrics: &[IndexMetrics],
    ) -> Vec<TuningRecommendation> {
        let mut recommendations = Vec::new();

        if traces.is_empty() {
            return recommendations;
        }

        // Recommend indexes for frequently scanned fields
        let mut field_scans: HashMap<String, u64> = HashMap::new();
        for trace in traces {
            if trace.index_used.is_none() && trace.rows_scanned > 1000 {
                for field in &trace.fields {
                    *field_scans.entry(field.clone()).or_insert(0) += trace.rows_scanned;
                }
            }
        }

        for (field, total_rows_scanned) in field_scans {
            let avg_scanned_per_query = total_rows_scanned as f64 / traces.len() as f64;
            if avg_scanned_per_query > 1000.0 {
                recommendations.push(TuningRecommendation {
                    rec_type: "create_index".to_string(),
                    priority: "high".to_string(),
                    index_name: None,
                    reason: format!(
                        "Field '{}' scanned {:.0} rows per query",
                        field, avg_scanned_per_query
                    ),
                    expected_speedup: Some(avg_scanned_per_query / 10.0),
                    estimated_roi: None,
                });
            }
        }

        // Recommend dropping unused indexes
        let unused_threshold = 0.01;
        let now = current_time_ms();
        for metric in metrics {
            if metric.hit_rate < unused_threshold && (now - metric.last_used_at) > 86_400_000 {
                recommendations.push(TuningRecommendation {
                    rec_type: "drop_index".to_string(),
                    priority: "low".to_string(),
                    index_name: Some(metric.name.clone()),
                    reason: format!(
                        "Index has {:.1}% hit rate and unused for 24h",
                        metric.hit_rate * 100.0
                    ),
                    expected_speedup: None,
                    estimated_roi: Some(-0.01),
                });
            }
        }

        // Recommend compound indexes
        let mut field_pairs: HashMap<String, u64> = HashMap::new();
        for trace in traces {
            if trace.fields.len() == 2 {
                let mut sorted = trace.fields.clone();
                sorted.sort();
                let key = sorted.join("|");
                *field_pairs.entry(key).or_insert(0) += 1;
            }
        }

        for (fields, count) in field_pairs {
            if (count as f64) > traces.len() as f64 * 0.1 {
                recommendations.push(TuningRecommendation {
                    rec_type: "create_index".to_string(),
                    priority: "medium".to_string(),
                    index_name: None,
                    reason: format!(
                        "Fields '{}' queried together in {} queries",
                        fields.replace('|', ", "),
                        count
                    ),
                    expected_speedup: Some(5.0),
                    estimated_roi: None,
                });
            }
        }

        // Sort by priority
        let priority_order = |p: &str| match p {
            "high" => 0,
            "medium" => 1,
            _ => 2,
        };

        recommendations.sort_by(|a, b| {
            let priority_cmp = priority_order(&a.priority).cmp(&priority_order(&b.priority));
            if priority_cmp == std::cmp::Ordering::Equal {
                let speedup_a = a.expected_speedup.unwrap_or(0.0);
                let speedup_b = b.expected_speedup.unwrap_or(0.0);
                speedup_b
                    .partial_cmp(&speedup_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
            } else {
                priority_cmp
            }
        });

        recommendations.truncate(5);
        recommendations
    }

    pub fn reset(&mut self) {
        self.metrics.clear();
        self.query_traces.clear();
        self.field_query_count.clear();
    }
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Automatic tuning engine - applies recommendations automatically.
pub struct AutoTuner {
    applied_recommendations: std::collections::HashSet<String>,
}

impl AutoTuner {
    pub fn new(_analytics: IndexAnalytics) -> Self {
        AutoTuner {
            applied_recommendations: std::collections::HashSet::new(),
        }
    }

    /// Check if a recommendation should be auto-applied.
    pub fn should_auto_apply(&self, recommendation: &TuningRecommendation) -> bool {
        if recommendation.priority != "high" {
            return false;
        }

        let key = format!(
            "{}:{}",
            recommendation.rec_type,
            recommendation
                .index_name
                .as_ref()
                .unwrap_or(&recommendation.reason)
        );
        if self.applied_recommendations.contains(&key) {
            return false;
        }

        if (recommendation.expected_speedup.unwrap_or(0.0)) < 5.0 {
            return false;
        }

        true
    }

    /// Apply a recommendation.
    pub fn apply_recommendation(&mut self, recommendation: &TuningRecommendation) -> bool {
        if !self.should_auto_apply(recommendation) {
            return false;
        }

        let key = format!(
            "{}:{}",
            recommendation.rec_type,
            recommendation
                .index_name
                .as_ref()
                .unwrap_or(&recommendation.reason)
        );
        self.applied_recommendations.insert(key);
        true
    }

    /// Get recommendations that are ready for auto-apply.
    pub fn get_auto_applicable_recommendations(
        &self,
        report: &PerformanceReport,
    ) -> Vec<TuningRecommendation> {
        report
            .recommendations
            .iter()
            .filter(|r| self.should_auto_apply(r))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_index_usage() {
        let mut analytics = IndexAnalytics::new();
        analytics.record_index_usage("test_idx", "read", 2.5);

        let metrics = analytics.get_index_metrics("test_idx");
        assert!(metrics.is_some());
        assert_eq!(metrics.unwrap().reads, 1);
    }

    #[test]
    fn test_record_query() {
        let mut analytics = IndexAnalytics::new();
        analytics.record_query(
            vec!["status".to_string()],
            "eq".to_string(),
            1.5,
            100,
            50,
            Some("status_idx".to_string()),
        );

        assert_eq!(analytics.query_traces.len(), 1);
        assert_eq!(analytics.field_query_count.get("status"), Some(&1));
    }

    #[test]
    fn test_slowest_queries() {
        let mut analytics = IndexAnalytics::new();
        analytics.record_query(vec!["a".to_string()], "eq".to_string(), 1.0, 10, 5, None);
        analytics.record_query(vec!["b".to_string()], "eq".to_string(), 5.0, 20, 10, None);
        analytics.record_query(vec!["c".to_string()], "eq".to_string(), 3.0, 15, 8, None);

        let slowest = analytics.get_slowest_queries(2);
        assert_eq!(slowest.len(), 2);
        assert_eq!(slowest[0].execution_time_ms, 5.0);
        assert_eq!(slowest[1].execution_time_ms, 3.0);
    }

    #[test]
    fn test_generate_report() {
        let mut analytics = IndexAnalytics::new();
        analytics.record_query(
            vec!["status".to_string()],
            "eq".to_string(),
            2.5,
            100,
            50,
            Some("status_idx".to_string()),
        );
        analytics.record_query(
            vec!["created".to_string()],
            "range".to_string(),
            50.0,
            10000,
            100,
            None,
        );

        let report = analytics.generate_report("orders", "daily");
        assert_eq!(report.collection_name, "orders");
        assert_eq!(report.total_queries, 2);
        assert_eq!(report.indexed_queries, 1);
        assert_eq!(report.full_scans, 1);
        assert!(report.recommendations.len() > 0);
    }
}
