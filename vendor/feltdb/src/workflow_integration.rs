/// Workflow Integration module - connects workflows with FeltDB systems
///
/// Integrates workflows with:
/// - Triggers (event-triggered workflows)
/// - Cron (time-triggered workflows)
/// - Provenance (workflow lineage tracking)
/// - Operations (durable workflow state)
use crate::{CronSchedule, FlowRef, Operation, TriggerFilter, WorkflowOperation, WorkflowRef};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Workflow trigger - creates workflow instances from events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTrigger {
    /// The workflow to create when triggered
    pub workflow_ref: WorkflowRef,
    /// Optional trigger filter (which events match)
    pub trigger_filter: Option<TriggerFilter>,
    /// Input mapping - how to extract workflow inputs from trigger event
    pub input_mapping: HashMap<String, String>,
    /// Metadata
    pub metadata: HashMap<String, serde_json::Value>,
}

impl WorkflowTrigger {
    /// Create a new workflow trigger
    pub fn new(workflow_ref: WorkflowRef) -> Self {
        Self {
            workflow_ref,
            trigger_filter: None,
            input_mapping: HashMap::new(),
            metadata: HashMap::new(),
        }
    }

    /// Set the trigger filter
    pub fn with_filter(mut self, filter: TriggerFilter) -> Self {
        self.trigger_filter = Some(filter);
        self
    }

    /// Add input mapping
    pub fn with_input_mapping(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.input_mapping.insert(key.into(), value.into());
        self
    }
}

/// Workflow cron trigger - creates workflow instances from cron schedule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowCronTrigger {
    /// The workflow to create on schedule
    pub workflow_ref: WorkflowRef,
    /// Cron schedule
    pub schedule: CronSchedule,
    /// Initial inputs for the workflow
    pub inputs: Vec<FlowRef>,
    /// Metadata
    pub metadata: HashMap<String, serde_json::Value>,
}

impl WorkflowCronTrigger {
    /// Create a new workflow cron trigger
    pub fn new(workflow_ref: WorkflowRef, schedule: CronSchedule) -> Self {
        Self {
            workflow_ref,
            schedule,
            inputs: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Add input
    pub fn with_input(mut self, input: FlowRef) -> Self {
        self.inputs.push(input);
        self
    }
}

/// Workflow provenance - tracks why an object exists through workflow execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowProvenance {
    /// The output reference that was created
    pub output_ref: FlowRef,
    /// Which workflow created it
    pub workflow_ref: WorkflowRef,
    /// Which workflow instance
    pub instance_id: String,
    /// Which step created it
    pub step_id: String,
    /// What capability executed the step
    pub capability: String,
    /// Input references that led to this output
    pub input_refs: Vec<FlowRef>,
    /// When it was created
    pub created_ms: u128,
}

impl WorkflowProvenance {
    /// Create workflow provenance record
    pub fn new(
        output_ref: FlowRef,
        workflow_ref: WorkflowRef,
        instance_id: impl Into<String>,
        step_id: impl Into<String>,
        capability: impl Into<String>,
    ) -> Self {
        Self {
            output_ref,
            workflow_ref,
            instance_id: instance_id.into(),
            step_id: step_id.into(),
            capability: capability.into(),
            input_refs: Vec::new(),
            created_ms: crate::now_ms(),
        }
    }

    /// Add an input reference
    pub fn with_input(mut self, input_ref: FlowRef) -> Self {
        self.input_refs.push(input_ref);
        self
    }
}

/// Workflow operations as durable operations for the operation log
impl WorkflowOperation {
    /// Convert to a durable operation
    pub fn to_operation(&self, workflow_ref: &WorkflowRef) -> Operation {
        let serialized = serde_json::to_value(self).unwrap_or(serde_json::json!({}));

        Operation::insert(
            0, // op_id will be assigned by storage
            "workflow-runtime".to_string(),
            0, // sequence will be assigned by storage
            format!("workflow:{}", workflow_ref.canonical()),
            serialized,
            "WorkflowOperation".to_string(),
            "workflow".to_string(),
        )
    }
}

/// Workflow integration manager
#[derive(Debug, Clone)]
pub struct WorkflowIntegration {
    /// Triggers that create workflows
    pub triggers: HashMap<String, WorkflowTrigger>,
    /// Cron triggers that create workflows
    pub cron_triggers: HashMap<String, WorkflowCronTrigger>,
    /// Provenance records
    pub provenance: HashMap<String, Vec<WorkflowProvenance>>,
}

impl WorkflowIntegration {
    /// Create a new workflow integration manager
    pub fn new() -> Self {
        Self {
            triggers: HashMap::new(),
            cron_triggers: HashMap::new(),
            provenance: HashMap::new(),
        }
    }

    /// Register a workflow trigger
    pub fn register_trigger(&mut self, trigger_id: impl Into<String>, trigger: WorkflowTrigger) {
        self.triggers.insert(trigger_id.into(), trigger);
    }

    /// Register a cron trigger
    pub fn register_cron_trigger(
        &mut self,
        trigger_id: impl Into<String>,
        trigger: WorkflowCronTrigger,
    ) {
        self.cron_triggers.insert(trigger_id.into(), trigger);
    }

    /// Record workflow provenance
    pub fn record_provenance(&mut self, provenance: WorkflowProvenance) {
        let key = provenance.output_ref.canonical();
        self.provenance
            .entry(key)
            .or_insert_with(Vec::new)
            .push(provenance);
    }

    /// Get provenance for an output reference
    pub fn get_provenance(&self, output_ref: &FlowRef) -> Option<Vec<WorkflowProvenance>> {
        self.provenance.get(&output_ref.canonical()).cloned()
    }

    /// Build complete provenance lineage (all upstream objects)
    pub fn build_lineage(&self, output_ref: &FlowRef) -> Vec<(FlowRef, String)> {
        let mut lineage = Vec::new();
        let mut to_process = vec![output_ref.clone()];
        let mut processed = std::collections::HashSet::new();

        while let Some(ref_to_process) = to_process.pop() {
            if processed.contains(&ref_to_process.canonical()) {
                continue;
            }
            processed.insert(ref_to_process.canonical());

            if let Some(provenances) = self.get_provenance(&ref_to_process) {
                for prov in provenances {
                    lineage.push((
                        ref_to_process.clone(),
                        format!("{}/{}", prov.workflow_ref, prov.step_id),
                    ));
                    for input_ref in &prov.input_refs {
                        to_process.push(input_ref.clone());
                    }
                }
            }
        }

        lineage
    }
}

impl Default for WorkflowIntegration {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_trigger_creation() {
        let workflow_ref = WorkflowRef::new("workflow", "test", 1);
        let trigger = WorkflowTrigger::new(workflow_ref);
        assert_eq!(trigger.workflow_ref.workflow_id, "test");
    }

    #[test]
    fn test_workflow_trigger_with_mapping() {
        let workflow_ref = WorkflowRef::new("workflow", "test", 1);
        let trigger = WorkflowTrigger::new(workflow_ref)
            .with_input_mapping("document", "event.payload.document");

        assert_eq!(
            trigger.input_mapping.get("document"),
            Some(&"event.payload.document".to_string())
        );
    }

    #[test]
    fn test_workflow_cron_trigger() {
        let workflow_ref = WorkflowRef::new("workflow", "scheduled", 1);
        let schedule = CronSchedule::new(
            "cron-1".to_string(),
            "Daily trigger".to_string(),
            "0 0 * * *".to_string(),
            "schedule-capability".to_string(),
        );
        let trigger = WorkflowCronTrigger::new(workflow_ref, schedule);

        assert_eq!(trigger.workflow_ref.workflow_id, "scheduled");
    }

    #[test]
    fn test_workflow_provenance_creation() {
        let output_ref = FlowRef::new("results", "result-1");
        let workflow_ref = WorkflowRef::new("workflow", "test", 1);

        let prov = WorkflowProvenance::new(
            output_ref,
            workflow_ref,
            "instance-1",
            "step-1",
            "extract-capability",
        );

        assert_eq!(prov.step_id, "step-1");
        assert_eq!(prov.capability, "extract-capability");
    }

    #[test]
    fn test_workflow_provenance_with_inputs() {
        let output_ref = FlowRef::new("results", "result-1");
        let input_ref = FlowRef::new("documents", "doc-1");
        let workflow_ref = WorkflowRef::new("workflow", "test", 1);

        let prov = WorkflowProvenance::new(
            output_ref,
            workflow_ref,
            "instance-1",
            "step-1",
            "extract-capability",
        )
        .with_input(input_ref.clone());

        assert_eq!(prov.input_refs.len(), 1);
        assert_eq!(prov.input_refs[0], input_ref);
    }

    #[test]
    fn test_workflow_integration_manager() {
        let mut integration = WorkflowIntegration::new();
        let workflow_ref = WorkflowRef::new("workflow", "test", 1);

        let trigger = WorkflowTrigger::new(workflow_ref);
        integration.register_trigger("trigger-1", trigger);

        assert!(integration.triggers.contains_key("trigger-1"));
    }

    #[test]
    fn test_workflow_provenance_tracking() {
        let mut integration = WorkflowIntegration::new();

        let doc_ref = FlowRef::new("documents", "doc-1");
        let text_ref = FlowRef::new("texts", "text-1");
        let embed_ref = FlowRef::new("embeddings", "embed-1");

        let workflow_ref = WorkflowRef::new("workflow", "processing", 1);

        // Extract step: document → text
        let prov1 = WorkflowProvenance::new(
            text_ref.clone(),
            workflow_ref.clone(),
            "instance-1",
            "extract",
            "extract-text",
        )
        .with_input(doc_ref.clone());

        // Embed step: text → embedding
        let prov2 = WorkflowProvenance::new(
            embed_ref.clone(),
            workflow_ref.clone(),
            "instance-1",
            "embed",
            "embedding",
        )
        .with_input(text_ref.clone());

        integration.record_provenance(prov1);
        integration.record_provenance(prov2);

        // Check direct provenance
        let text_prov = integration.get_provenance(&text_ref);
        assert!(text_prov.is_some());
        assert_eq!(text_prov.unwrap()[0].input_refs[0], doc_ref);

        // Build lineage
        let lineage = integration.build_lineage(&embed_ref);
        assert!(!lineage.is_empty());
    }
}
