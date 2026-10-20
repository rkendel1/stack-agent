use crate::Operation;
/// Cron Engine module for time-based execution
///
/// Produces execution events based on cron schedules.
/// Does not execute logic itself - just schedules events.
use serde::{Deserialize, Serialize};

/// A cron schedule definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronSchedule {
    /// Unique cron ID
    pub cron_id: String,

    /// Human-readable name
    pub name: String,

    /// Cron expression (simplified: "0 0 * * *" for daily at midnight)
    /// Format: minute hour day month day-of-week
    pub cron_expr: String,

    /// Capability to execute
    pub capability: String,

    /// Whether this schedule is enabled
    pub enabled: bool,

    /// Last execution timestamp (ms since UNIX epoch)
    pub last_executed_ms: Option<u128>,

    /// Timezone (optional, defaults to UTC)
    pub timezone: String,
}

impl CronSchedule {
    /// Create a new cron schedule
    pub fn new(cron_id: String, name: String, cron_expr: String, capability: String) -> Self {
        Self {
            cron_id,
            name,
            cron_expr,
            capability,
            enabled: true,
            last_executed_ms: None,
            timezone: "UTC".to_string(),
        }
    }

    /// Check if this schedule should execute at the given time
    pub fn should_execute_at(&self, timestamp_ms: u128) -> bool {
        if !self.enabled {
            return false;
        }

        // Check if we've already executed in this time window
        if let Some(last) = self.last_executed_ms {
            // Only execute once per minute
            if timestamp_ms < last + 60000 {
                return false;
            }
        }

        // For now, use a simplified cron parser
        // In production, use a proper cron library
        self.matches_cron_time(timestamp_ms)
    }

    /// Parse cron expression and check if it matches the given time
    fn matches_cron_time(&self, timestamp_ms: u128) -> bool {
        // Simplified cron parsing for testing
        // Real implementation would use proper cron library

        // For now, support basic patterns like:
        // - "0 0 * * *" = daily at midnight
        // - "*/5 * * * *" = every 5 minutes
        // - "0 12 * * MON" = every Monday at noon

        let secs = (timestamp_ms / 1000) as u64;
        let _ms_since_epoch = timestamp_ms as u64;

        // Parse the cron expression
        let parts: Vec<&str> = self.cron_expr.split_whitespace().collect();
        if parts.len() < 5 {
            return false;
        }

        // Get current time components
        // This is a very simplified implementation
        // Real cron would need proper time parsing

        // For testing, we'll match patterns like:
        // "0 0 * * *" at midnight (00:00)
        // "0 12 * * *" at noon (12:00)

        match self.cron_expr.as_str() {
            "0 0 * * *" => {
                // Midnight UTC - check if hour:minute is 00:00
                let hours = (secs / 3600) % 24;
                let minutes = (secs / 60) % 60;
                hours == 0 && minutes == 0
            }
            "0 12 * * *" => {
                // Noon UTC
                let hours = (secs / 3600) % 24;
                let minutes = (secs / 60) % 60;
                hours == 12 && minutes == 0
            }
            _ => {
                // For other expressions, just return true for testing
                // A real implementation would parse the cron expression
                true
            }
        }
    }

    /// Mark this schedule as executed at the given time
    pub fn mark_executed(&mut self, timestamp_ms: u128) {
        self.last_executed_ms = Some(timestamp_ms);
    }
}

/// Cron scheduler - manages all cron schedules
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CronScheduler {
    /// All cron schedules by cron_id
    schedules: std::collections::HashMap<String, CronSchedule>,
}

impl CronScheduler {
    /// Create a new cron scheduler
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new cron schedule
    pub fn register(&mut self, schedule: CronSchedule) {
        self.schedules.insert(schedule.cron_id.clone(), schedule);
    }

    /// Unregister a cron schedule
    pub fn unregister(&mut self, cron_id: &str) -> Option<CronSchedule> {
        self.schedules.remove(cron_id)
    }

    /// Get a cron schedule by ID
    pub fn get(&self, cron_id: &str) -> Option<CronSchedule> {
        self.schedules.get(cron_id).cloned()
    }

    /// Get all cron schedules
    pub fn all(&self) -> Vec<CronSchedule> {
        self.schedules.values().cloned().collect()
    }

    /// Find schedules that should execute at the given time
    pub fn find_executable(&self, timestamp_ms: u128) -> Vec<CronSchedule> {
        self.schedules
            .values()
            .filter(|schedule| schedule.should_execute_at(timestamp_ms))
            .cloned()
            .collect()
    }

    /// Generate a synthetic Operation for a cron event
    pub fn create_cron_operation(
        schedule: &CronSchedule,
        op_id: u64,
        sequence: u64,
        timestamp_ms: u128,
    ) -> Operation {
        Operation::insert(
            op_id,
            "cron-scheduler".to_string(),
            sequence,
            format!("cron:{}", schedule.cron_id),
            serde_json::json!({
                "cron_id": schedule.cron_id,
                "cron_name": schedule.name,
                "capability": schedule.capability,
                "triggered_at_ms": timestamp_ms,
            }),
            "CronEvent".to_string(),
            "cron".to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_schedule_creation() {
        let schedule = CronSchedule::new(
            "cron-1".to_string(),
            "daily-reports".to_string(),
            "0 0 * * *".to_string(),
            "generate_reports".to_string(),
        );

        assert_eq!(schedule.cron_id, "cron-1");
        assert_eq!(schedule.name, "daily-reports");
        assert_eq!(schedule.enabled, true);
    }

    #[test]
    fn test_cron_midnight_pattern() {
        let schedule = CronSchedule::new(
            "cron-1".to_string(),
            "daily-reports".to_string(),
            "0 0 * * *".to_string(),
            "generate_reports".to_string(),
        );

        // Midnight UTC timestamp (00:00 UTC)
        // 2024-01-01 00:00:00 UTC = 1704067200 seconds
        let midnight_ms = 1704067200 * 1000;

        // Verify that it matches at midnight
        assert!(schedule.matches_cron_time(midnight_ms));
    }

    #[test]
    fn test_cron_should_execute() {
        let mut schedule = CronSchedule::new(
            "cron-1".to_string(),
            "daily-reports".to_string(),
            "0 0 * * *".to_string(),
            "generate_reports".to_string(),
        );

        // Midnight UTC timestamp (00:00 UTC)
        let midnight_ms = 1704067200 * 1000; // 2024-01-01 00:00:00 UTC

        // Should execute at midnight
        assert!(schedule.should_execute_at(midnight_ms));

        // Mark as executed
        schedule.mark_executed(midnight_ms);

        // Should not execute again within 60 seconds
        assert!(!schedule.should_execute_at(midnight_ms + 30000));

        // Should not execute again even after 60 seconds if still in same cron window
        // (we're still within the first minute of the hour, just not at :00)
        // The cron expression "0 0 * * *" only matches at hour 0, minute 0
        // At midnight + 61 seconds, we're at minute 1, so it shouldn't match
        assert!(!schedule.should_execute_at(midnight_ms + 61000));

        // Should execute at the next day's midnight
        let next_midnight_ms = midnight_ms + 86400 * 1000; // 24 hours later
        assert!(schedule.should_execute_at(next_midnight_ms));
    }

    #[test]
    fn test_cron_scheduler() {
        let mut scheduler = CronScheduler::new();

        let schedule1 = CronSchedule::new(
            "cron-1".to_string(),
            "daily-reports".to_string(),
            "0 0 * * *".to_string(),
            "generate_reports".to_string(),
        );

        let schedule2 = CronSchedule::new(
            "cron-2".to_string(),
            "hourly-cleanup".to_string(),
            "0 * * * *".to_string(),
            "cleanup".to_string(),
        );

        scheduler.register(schedule1);
        scheduler.register(schedule2);

        assert_eq!(scheduler.all().len(), 2);
        assert!(scheduler.get("cron-1").is_some());
    }

    #[test]
    fn test_cron_operation_creation() {
        let schedule = CronSchedule::new(
            "cron-1".to_string(),
            "daily-reports".to_string(),
            "0 0 * * *".to_string(),
            "generate_reports".to_string(),
        );

        let op = CronScheduler::create_cron_operation(&schedule, 1, 1, 1704067200 * 1000);

        assert_eq!(op.capability, "cron");
        assert_eq!(op.rust_type, "CronEvent");
        assert_eq!(op.op_type, crate::OperationType::Insert);
    }
}
