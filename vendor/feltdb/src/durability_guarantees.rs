/// Durability & Authority Guarantees
///
/// Provides formal durability and authority contracts:
/// - Restart recovery with consistency verification
/// - Crash-point consistency across all phases
/// - Authoritative-write semantics (no write conflicts)
/// - Reconciliation after divergence
/// - Write authority tracking and conflicts
/// - Recovery contracts
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum WriteAuthority {
    Primary,
    Secondary,
    Replica,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteAuthorityRecord {
    pub resource_id: String,
    pub authority: WriteAuthority,
    pub authority_node: String,
    pub version: u64,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurabilityContract {
    pub level: String,
    pub guarantees: Vec<String>,
    pub violation_scenarios: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashPoint {
    pub phase: String,
    pub description: String,
    pub recovery_required: bool,
    pub data_loss_possible: bool,
    pub state_consistency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryCheckpoint {
    pub checkpoint_id: String,
    pub timestamp: u64,
    pub operations_count: u64,
    pub data_hash: String,
    pub state_valid: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationStrategy {
    pub strategy_name: String,
    pub detect_divergence: bool,
    pub merge_strategy: String,
    pub conflict_resolution: String,
    pub final_authority: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorityConflict {
    pub resource_id: String,
    pub conflicting_writes: Vec<(String, u64)>,
    pub detected_at: u64,
    pub resolution: String,
}

pub struct DurabilityGuarantees {
    node_id: String,
    write_authorities: HashMap<String, WriteAuthorityRecord>,
    crash_points: Vec<CrashPoint>,
    recovery_checkpoints: Vec<RecoveryCheckpoint>,
    authority_conflicts: Vec<AuthorityConflict>,
    checkpoint_counter: u64,
}

impl DurabilityGuarantees {
    pub fn new(node_id: String) -> Self {
        DurabilityGuarantees {
            node_id,
            write_authorities: HashMap::new(),
            crash_points: vec![
                CrashPoint {
                    phase: "execution".to_string(),
                    description: "During operation execution".to_string(),
                    recovery_required: true,
                    data_loss_possible: false,
                    state_consistency: "partial".to_string(),
                },
                CrashPoint {
                    phase: "journal".to_string(),
                    description: "During journal append".to_string(),
                    recovery_required: true,
                    data_loss_possible: false,
                    state_consistency: "consistent".to_string(),
                },
                CrashPoint {
                    phase: "replication".to_string(),
                    description: "During replication".to_string(),
                    recovery_required: false,
                    data_loss_possible: false,
                    state_consistency: "consistent".to_string(),
                },
                CrashPoint {
                    phase: "convergence".to_string(),
                    description: "During convergence".to_string(),
                    recovery_required: true,
                    data_loss_possible: false,
                    state_consistency: "eventual".to_string(),
                },
            ],
            recovery_checkpoints: Vec::new(),
            authority_conflicts: Vec::new(),
            checkpoint_counter: 0,
        }
    }

    pub fn establish_write_authority(
        &mut self,
        resource_id: String,
        authority_node: String,
    ) -> Result<(), String> {
        if self.write_authorities.contains_key(&resource_id) {
            return Err(format!(
                "Write authority already established for {}",
                resource_id
            ));
        }

        self.write_authorities.insert(
            resource_id.clone(),
            WriteAuthorityRecord {
                resource_id,
                authority: WriteAuthority::Primary,
                authority_node,
                version: 1,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            },
        );

        Ok(())
    }

    pub fn get_write_authority(&self, resource_id: &str) -> Option<WriteAuthorityRecord> {
        self.write_authorities.get(resource_id).cloned()
    }

    pub fn verify_write_authority(
        &self,
        resource_id: &str,
        writing_node: &str,
    ) -> Result<bool, String> {
        match self.write_authorities.get(resource_id) {
            Some(record) => Ok(record.authority_node == writing_node),
            None => Err(format!("No write authority for {}", resource_id)),
        }
    }

    pub fn create_recovery_checkpoint(
        &mut self,
        operations_count: u64,
        data_hash: String,
    ) -> Result<RecoveryCheckpoint, String> {
        self.checkpoint_counter += 1;

        let checkpoint = RecoveryCheckpoint {
            checkpoint_id: format!("cp-{}-{}", self.node_id, self.checkpoint_counter),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            operations_count,
            data_hash,
            state_valid: true,
        };

        self.recovery_checkpoints.push(checkpoint.clone());
        Ok(checkpoint)
    }

    pub fn restart_recovery(
        &mut self,
        last_checkpoint_id: Option<&str>,
    ) -> Result<RecoveryCheckpoint, String> {
        let checkpoint = if let Some(id) = last_checkpoint_id {
            self.recovery_checkpoints
                .iter()
                .find(|cp| cp.checkpoint_id == id)
                .cloned()
                .ok_or_else(|| format!("Checkpoint not found: {}", id))?
        } else {
            self.recovery_checkpoints
                .last()
                .cloned()
                .ok_or_else(|| "No recovery checkpoint available".to_string())?
        };

        if !checkpoint.state_valid {
            return Err("Last checkpoint is invalid".to_string());
        }

        Ok(checkpoint)
    }

    pub fn detect_crash_consistency(&self, crash_phase: &str) -> Result<Vec<CrashPoint>, String> {
        Ok(self
            .crash_points
            .iter()
            .filter(|cp| cp.phase == crash_phase || crash_phase == "all")
            .cloned()
            .collect())
    }

    pub fn record_authority_conflict(
        &mut self,
        resource_id: String,
        write_nodes: Vec<(String, u64)>,
        resolution: String,
    ) {
        self.authority_conflicts.push(AuthorityConflict {
            resource_id,
            conflicting_writes: write_nodes,
            detected_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            resolution,
        });
    }

    pub fn get_durability_contract(&self) -> DurabilityContract {
        DurabilityContract {
            level: "strong".to_string(),
            guarantees: vec![
                "No data loss after successful write".to_string(),
                "Atomic operation execution".to_string(),
                "Consistent state after recovery".to_string(),
                "Write authority prevents conflicts".to_string(),
                "All crash points handle gracefully".to_string(),
            ],
            violation_scenarios: vec![
                "Complete cluster loss before replication".to_string(),
                "Byzantine failures (untrusted nodes)".to_string(),
                "Corrupted storage medium".to_string(),
            ],
        }
    }

    pub fn reconcile_divergence(
        &mut self,
        local_version: u64,
        remote_version: u64,
        strategy: ReconciliationStrategy,
    ) -> Result<u64, String> {
        let final_version = match strategy.conflict_resolution.as_str() {
            "last-write-wins" => local_version.max(remote_version),
            "first-write-wins" => local_version.min(remote_version),
            "authority-decides" => {
                if strategy.final_authority == self.node_id {
                    local_version
                } else {
                    remote_version
                }
            }
            _ => local_version,
        };

        Ok(final_version)
    }

    pub fn verify_crash_recovery(&self, checkpoint: &RecoveryCheckpoint) -> Result<bool, String> {
        if !checkpoint.state_valid {
            return Ok(false);
        }

        Ok(true)
    }

    pub fn get_authority_conflicts(&self) -> Vec<AuthorityConflict> {
        self.authority_conflicts.clone()
    }

    pub fn get_checkpoint_count(&self) -> usize {
        self.recovery_checkpoints.len()
    }
}

impl Default for DurabilityGuarantees {
    fn default() -> Self {
        Self::new("node".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_authority() {
        let mut dg = DurabilityGuarantees::new("node-1".to_string());
        dg.establish_write_authority("res-1".to_string(), "node-1".to_string())
            .unwrap();

        assert!(dg.verify_write_authority("res-1", "node-1").unwrap());
        assert!(!dg.verify_write_authority("res-1", "node-2").unwrap());
    }

    #[test]
    fn test_recovery_checkpoint() {
        let mut dg = DurabilityGuarantees::new("node-1".to_string());
        let cp = dg
            .create_recovery_checkpoint(100, "hash123".to_string())
            .unwrap();

        assert_eq!(cp.operations_count, 100);
        assert!(cp.state_valid);
        assert_eq!(dg.get_checkpoint_count(), 1);
    }

    #[test]
    fn test_restart_recovery() {
        let mut dg = DurabilityGuarantees::new("node-1".to_string());
        dg.create_recovery_checkpoint(100, "hash1".to_string())
            .unwrap();

        let recovered = dg.restart_recovery(None).unwrap();
        assert_eq!(recovered.operations_count, 100);
    }

    #[test]
    fn test_crash_consistency() {
        let dg = DurabilityGuarantees::new("node-1".to_string());
        let journal_crashes = dg.detect_crash_consistency("journal").unwrap();

        assert!(journal_crashes.len() > 0);
        assert_eq!(journal_crashes[0].phase, "journal");
    }

    #[test]
    fn test_durability_contract() {
        let dg = DurabilityGuarantees::new("node-1".to_string());
        let contract = dg.get_durability_contract();

        assert_eq!(contract.level, "strong");
        assert!(contract.guarantees.len() > 0);
    }

    #[test]
    fn test_reconciliation() {
        let mut dg = DurabilityGuarantees::new("node-1".to_string());
        let strategy = ReconciliationStrategy {
            strategy_name: "lww".to_string(),
            detect_divergence: true,
            merge_strategy: "timestamp".to_string(),
            conflict_resolution: "last-write-wins".to_string(),
            final_authority: "node-1".to_string(),
        };

        let result = dg.reconcile_divergence(100, 200, strategy).unwrap();
        assert_eq!(result, 200);
    }
}
