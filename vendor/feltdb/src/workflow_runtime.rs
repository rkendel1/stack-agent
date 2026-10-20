/// Workflow Runtime module for managing workflow execution and state
///
/// Provides durable state management through FeltDB operations,
/// reactive advancement, and distributed execution coordination.
use crate::{
    BlockedReason, FlowRef, WorkflowGraph, WorkflowInstance, WorkflowOperation, WorkflowRef,
    WorkflowState, WorkflowStepRef, WorkflowStepState,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Workflow runtime - manages workflow execution and state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRuntime {
    /// Workflow instances by instance ID
    instances: HashMap<String, WorkflowInstance>,
    /// Workflow definitions by workflow ref
    graphs: HashMap<String, WorkflowGraph>,
    /// Pending state transitions
    pending_transitions: VecDeque<WorkflowStateTransition>,
    /// Emitted operations (for testing and debugging)
    operations_log: Vec<WorkflowOperationEntry>,
}

/// An emitted workflow operation with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowOperationEntry {
    /// The operation that occurred
    pub operation: WorkflowOperation,
    /// Timestamp in ms
    pub timestamp_ms: u128,
    /// Instance ID that emitted this operation
    pub instance_id: String,
}

/// State transition to process reactively
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkflowStateTransition {
    /// Instance ID
    pub instance_id: String,
    /// Step ID that completed or changed
    pub step_id: String,
    /// New state
    pub new_state: WorkflowStepState,
    /// Optional result or error
    pub data: Option<TransitionData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum TransitionData {
    Result(FlowRef),
    Error(String),
    Claim(String),
}

impl WorkflowRuntime {
    /// Create a new workflow runtime
    pub fn new() -> Self {
        Self {
            instances: HashMap::new(),
            graphs: HashMap::new(),
            pending_transitions: VecDeque::new(),
            operations_log: Vec::new(),
        }
    }

    /// Register a workflow definition
    pub fn register_workflow(&mut self, graph: WorkflowGraph) -> Result<(), String> {
        graph.validate()?;
        self.graphs.insert(graph.workflow_ref.canonical(), graph);
        Ok(())
    }

    /// Create a new workflow instance
    pub fn create_instance(
        &mut self,
        workflow_ref: WorkflowRef,
        instance_id: impl Into<String>,
        _inputs: Vec<FlowRef>,
    ) -> Result<String, String> {
        let instance_id = instance_id.into();
        let graph = self
            .graphs
            .get(&workflow_ref.canonical())
            .ok_or_else(|| format!("Workflow not found: {}", workflow_ref))?
            .clone();

        let mut instance = WorkflowInstance::new(workflow_ref.clone(), instance_id.clone());

        // Initialize all steps to Pending
        for node_id in graph.nodes.keys() {
            instance.set_step_state(node_id, WorkflowStepState::Pending);
        }

        // Emit WorkflowCreated operation
        self.emit_operation(
            instance_id.clone(),
            WorkflowOperation::Created {
                workflow_ref: workflow_ref.clone(),
            },
        );

        // Store instance
        self.instances.insert(instance_id.clone(), instance);

        // Make initial transitions - start with entry nodes
        self.advance_workflow(&instance_id, &graph)?;

        Ok(instance_id)
    }

    /// Start a workflow instance
    pub fn start_instance(&mut self, instance_id: &str) -> Result<(), String> {
        let instance = self
            .instances
            .get_mut(instance_id)
            .ok_or_else(|| format!("Instance not found: {}", instance_id))?;

        if instance.state != WorkflowState::Created {
            return Err(format!(
                "Cannot start instance in state {:?}",
                instance.state
            ));
        }

        let workflow_ref = instance.workflow_ref.clone();
        instance.state = WorkflowState::Running;
        instance.started_ms = Some(crate::now_ms());

        self.emit_operation(
            instance_id.to_string(),
            WorkflowOperation::Started { workflow_ref },
        );

        Ok(())
    }

    /// Claim a step for execution (single-owner execution)
    pub fn claim_step(
        &mut self,
        instance_id: &str,
        step_id: &str,
        peer_id: impl Into<String>,
    ) -> Result<(), String> {
        let peer_id = peer_id.into();
        let instance = self
            .instances
            .get_mut(instance_id)
            .ok_or_else(|| format!("Instance not found: {}", instance_id))?;

        instance.claim_step(step_id, peer_id.clone())?;

        let step_ref = WorkflowStepRef::new(instance.workflow_ref.clone(), step_id);
        self.emit_operation(
            instance_id.to_string(),
            WorkflowOperation::StepClaimed {
                step_ref,
                claimed_by: peer_id,
            },
        );

        Ok(())
    }

    /// Mark a step as executing
    pub fn step_executing(&mut self, instance_id: &str, step_id: &str) -> Result<(), String> {
        let instance = self
            .instances
            .get_mut(instance_id)
            .ok_or_else(|| format!("Instance not found: {}", instance_id))?;

        let claimed_by = instance
            .step_claims
            .get(step_id)
            .cloned()
            .ok_or_else(|| format!("Step not claimed: {}", step_id))?;

        instance.set_step_state(step_id, WorkflowStepState::Running);

        let step_ref = WorkflowStepRef::new(instance.workflow_ref.clone(), step_id);
        self.emit_operation(
            instance_id.to_string(),
            WorkflowOperation::StepExecuting {
                step_ref,
                claimed_by,
            },
        );

        Ok(())
    }

    /// Mark a step as completed with a result
    pub fn step_completed(
        &mut self,
        instance_id: &str,
        step_id: &str,
        result_ref: FlowRef,
    ) -> Result<(), String> {
        // Collect data before releasing borrows
        let (workflow_ref, graph_canonical, step_ref) = {
            let instance = self
                .instances
                .get_mut(instance_id)
                .ok_or_else(|| format!("Instance not found: {}", instance_id))?;

            instance.complete_step(step_id, result_ref.clone())?;

            let step_ref = WorkflowStepRef::new(instance.workflow_ref.clone(), step_id);
            (
                instance.workflow_ref.clone(),
                instance.workflow_ref.canonical(),
                step_ref,
            )
        };

        // Now emit after releasing the mutable borrow on instance
        self.emit_operation(
            instance_id.to_string(),
            WorkflowOperation::StepCompleted {
                step_ref,
                result_ref: Some(result_ref),
            },
        );

        // Check if this advancement triggers other steps
        let graph = self
            .graphs
            .get(&graph_canonical)
            .ok_or_else(|| format!("Workflow not found: {}", workflow_ref))?
            .clone();

        self.advance_workflow(instance_id, &graph)?;

        Ok(())
    }

    /// Mark a step as failed
    pub fn step_failed(
        &mut self,
        instance_id: &str,
        step_id: &str,
        error: String,
    ) -> Result<(), String> {
        let error_msg = {
            let instance = self
                .instances
                .get_mut(instance_id)
                .ok_or_else(|| format!("Instance not found: {}", instance_id))?;

            let attempt = instance
                .step_failures
                .get(step_id)
                .map(|errs| errs.len() as u32 + 1)
                .unwrap_or(1);

            instance.fail_step(step_id, error)?;

            let error_str = instance
                .step_failures
                .get(step_id)
                .map(|e| e.last())
                .flatten()
                .unwrap_or(&"unknown".to_string())
                .clone();
            format!("Attempt {}: {}", attempt, error_str)
        };

        let step_ref = WorkflowStepRef::new(
            self.instances
                .get(instance_id)
                .unwrap()
                .workflow_ref
                .clone(),
            step_id,
        );

        let attempt = self
            .instances
            .get(instance_id)
            .map(|i| {
                i.step_failures
                    .get(step_id)
                    .map(|e| e.len() as u32)
                    .unwrap_or(1)
            })
            .unwrap_or(1);

        self.emit_operation(
            instance_id.to_string(),
            WorkflowOperation::StepFailed {
                step_ref,
                error: error_msg,
                attempt,
            },
        );

        Ok(())
    }

    /// Mark a step as blocked
    pub fn step_blocked(
        &mut self,
        instance_id: &str,
        step_id: &str,
        reason: BlockedReason,
    ) -> Result<(), String> {
        let instance = self
            .instances
            .get_mut(instance_id)
            .ok_or_else(|| format!("Instance not found: {}", instance_id))?;

        instance.set_step_state(step_id, WorkflowStepState::Blocked);
        instance
            .step_blocked_reasons
            .insert(step_id.to_string(), reason.clone());

        let step_ref = WorkflowStepRef::new(instance.workflow_ref.clone(), step_id);
        self.emit_operation(
            instance_id.to_string(),
            WorkflowOperation::StepBlocked { step_ref, reason },
        );

        Ok(())
    }

    /// Unblock a step and continue workflow
    pub fn unblock_step(&mut self, instance_id: &str, step_id: &str) -> Result<(), String> {
        let instance = self
            .instances
            .get_mut(instance_id)
            .ok_or_else(|| format!("Instance not found: {}", instance_id))?;

        if instance.get_step_state(step_id) != Some(WorkflowStepState::Blocked) {
            return Err(format!("Step not blocked: {}", step_id));
        }

        instance.set_step_state(step_id, WorkflowStepState::Ready);
        instance.step_blocked_reasons.remove(step_id);

        // Check if this advancement triggers other steps
        let graph = self
            .graphs
            .get(&instance.workflow_ref.canonical())
            .ok_or_else(|| format!("Workflow not found: {}", instance.workflow_ref))?
            .clone();

        self.advance_workflow(instance_id, &graph)?;

        Ok(())
    }

    /// Advance workflow reactively - make ready steps available for execution
    fn advance_workflow(&mut self, instance_id: &str, graph: &WorkflowGraph) -> Result<(), String> {
        // Collect operations to emit to avoid borrow checker issues
        let mut ops_to_emit = Vec::new();

        {
            let instance = self
                .instances
                .get_mut(instance_id)
                .ok_or_else(|| format!("Instance not found: {}", instance_id))?;

            // Find steps that can transition to Ready
            for (step_id, node) in &graph.nodes {
                let current_state = instance.get_step_state(step_id);

                // Only transition from Pending to Ready
                if current_state != Some(WorkflowStepState::Pending) {
                    continue;
                }

                // Check if all dependencies are completed
                let all_deps_satisfied = node.inputs.iter().all(|dep| {
                    if dep.starts_with("input:") {
                        // Input dependencies are always satisfied
                        true
                    } else {
                        // Check if dependency step is completed
                        instance.get_step_state(dep) == Some(WorkflowStepState::Completed)
                    }
                });

                if all_deps_satisfied {
                    instance.set_step_state(step_id, WorkflowStepState::Ready);

                    let step_ref = WorkflowStepRef::new(instance.workflow_ref.clone(), step_id);
                    ops_to_emit.push(WorkflowOperation::StepReady { step_ref });
                }
            }

            // Check if workflow is complete
            let all_completed = graph.nodes.keys().all(|step_id| {
                instance.get_step_state(step_id) == Some(WorkflowStepState::Completed)
            });

            if all_completed {
                instance.state = WorkflowState::Completed;
                instance.completed_ms = Some(crate::now_ms());

                ops_to_emit.push(WorkflowOperation::Completed {
                    workflow_ref: instance.workflow_ref.clone(),
                });
            }
        }

        // Now emit the operations after the borrow is released
        for op in ops_to_emit {
            self.emit_operation(instance_id.to_string(), op);
        }

        Ok(())
    }

    /// Get a workflow instance
    pub fn get_instance(&self, instance_id: &str) -> Option<WorkflowInstance> {
        self.instances.get(instance_id).cloned()
    }

    /// Get workflow graph
    pub fn get_graph(&self, workflow_ref: &WorkflowRef) -> Option<WorkflowGraph> {
        self.graphs.get(&workflow_ref.canonical()).cloned()
    }

    /// Get all operations logged so far
    pub fn operations_log(&self) -> &[WorkflowOperationEntry] {
        &self.operations_log
    }

    /// Get ready steps for a workflow instance
    pub fn ready_steps(&self, instance_id: &str) -> Vec<String> {
        self.instances
            .get(instance_id)
            .map(|instance| {
                instance
                    .step_states
                    .iter()
                    .filter(|(_, state)| *state == &WorkflowStepState::Ready)
                    .map(|(step_id, _)| step_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Emit a workflow operation
    fn emit_operation(&mut self, instance_id: String, operation: WorkflowOperation) {
        self.operations_log.push(WorkflowOperationEntry {
            operation,
            timestamp_ms: crate::now_ms(),
            instance_id,
        });
    }
}

impl Default for WorkflowRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WorkflowGraph, WorkflowNode};

    fn create_test_workflow() -> WorkflowGraph {
        let wf = WorkflowRef::new("workflow", "test", 1);
        WorkflowGraph::new(wf)
            .add_node(WorkflowNode::new("step-1", "capability-a"))
            .add_node(WorkflowNode::new("step-2", "capability-b").with_input("step-1"))
            .add_edge("step-1", "step-2")
    }

    #[test]
    fn test_workflow_runtime_creation() {
        let mut runtime = WorkflowRuntime::new();
        let graph = create_test_workflow();
        assert!(runtime.register_workflow(graph).is_ok());
    }

    #[test]
    fn test_workflow_instance_creation() {
        let mut runtime = WorkflowRuntime::new();
        let graph = create_test_workflow();
        let wf = graph.workflow_ref.clone();
        runtime.register_workflow(graph).unwrap();

        let instance_id = runtime.create_instance(wf, "inst-1", Vec::new()).unwrap();

        let instance = runtime.get_instance(&instance_id).unwrap();
        assert_eq!(instance.state, WorkflowState::Created);
    }

    #[test]
    fn test_workflow_reactive_advancement() {
        let mut runtime = WorkflowRuntime::new();
        let graph = create_test_workflow();
        let wf = graph.workflow_ref.clone();
        runtime.register_workflow(graph).unwrap();

        let instance_id = runtime.create_instance(wf, "inst-1", Vec::new()).unwrap();

        runtime.start_instance(&instance_id).unwrap();

        // Initially, step-1 should be ready (no dependencies)
        let ready = runtime.ready_steps(&instance_id);
        assert!(ready.contains(&"step-1".to_string()));
        assert!(!ready.contains(&"step-2".to_string()));

        // Claim and complete step-1
        runtime
            .claim_step(&instance_id, "step-1", "peer-a")
            .unwrap();
        runtime
            .step_completed(&instance_id, "step-1", FlowRef::new("results", "result-1"))
            .unwrap();

        // Now step-2 should be ready
        let ready = runtime.ready_steps(&instance_id);
        assert!(ready.contains(&"step-2".to_string()));

        // Check operations log
        let ops = runtime.operations_log();
        assert!(ops
            .iter()
            .any(|op| matches!(op.operation, WorkflowOperation::StepReady { .. })));
    }

    #[test]
    fn test_workflow_completion() {
        let mut runtime = WorkflowRuntime::new();
        let graph = create_test_workflow();
        let wf = graph.workflow_ref.clone();
        runtime.register_workflow(graph).unwrap();

        let instance_id = runtime.create_instance(wf, "inst-1", Vec::new()).unwrap();

        runtime.start_instance(&instance_id).unwrap();

        // Complete step-1
        runtime
            .claim_step(&instance_id, "step-1", "peer-a")
            .unwrap();
        runtime
            .step_completed(&instance_id, "step-1", FlowRef::new("results", "result-1"))
            .unwrap();

        // Complete step-2
        runtime
            .claim_step(&instance_id, "step-2", "peer-b")
            .unwrap();
        runtime
            .step_completed(&instance_id, "step-2", FlowRef::new("results", "result-2"))
            .unwrap();

        // Workflow should be completed
        let instance = runtime.get_instance(&instance_id).unwrap();
        assert_eq!(instance.state, WorkflowState::Completed);
    }

    #[test]
    fn test_step_blocking() {
        let mut runtime = WorkflowRuntime::new();
        let graph = create_test_workflow();
        let wf = graph.workflow_ref.clone();
        runtime.register_workflow(graph).unwrap();

        let instance_id = runtime.create_instance(wf, "inst-1", Vec::new()).unwrap();

        runtime.start_instance(&instance_id).unwrap();

        // Block step-1
        runtime
            .step_blocked(
                &instance_id,
                "step-1",
                BlockedReason::AwaitingApproval("Needs manager approval".to_string()),
            )
            .unwrap();

        let instance = runtime.get_instance(&instance_id).unwrap();
        assert_eq!(
            instance.get_step_state("step-1"),
            Some(WorkflowStepState::Blocked)
        );

        // Unblock step-1
        runtime.unblock_step(&instance_id, "step-1").unwrap();
        let instance = runtime.get_instance(&instance_id).unwrap();
        assert_eq!(
            instance.get_step_state("step-1"),
            Some(WorkflowStepState::Ready)
        );
    }
}
