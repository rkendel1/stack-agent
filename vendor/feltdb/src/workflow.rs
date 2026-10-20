use crate::FlowRef;
/// Workflow module for durable, reactive distributed workflows
///
/// A workflow is a durable graph of intents and references that can execute
/// wherever the required capabilities exist, without the workflow owning execution location.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};

/// Reference to a workflow - addressable object in the fabric
///
/// WorkflowRef {
///     namespace: "workflows",
///     workflow_id: "order-processing",
///     version: 1,
/// }
///
/// For example:
/// - flow://workflow/order-processing@1
/// - flow://workflow/document-ingestion@2
/// - flow://workflow/research@1
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct WorkflowRef {
    /// Namespace for workflows (typically "workflow")
    pub namespace: String,
    /// Unique identifier for the workflow
    pub workflow_id: String,
    /// Version of the workflow
    pub version: u64,
}

impl WorkflowRef {
    /// Create a new WorkflowRef
    pub fn new(namespace: impl Into<String>, workflow_id: impl Into<String>, version: u64) -> Self {
        Self {
            namespace: namespace.into(),
            workflow_id: workflow_id.into(),
            version,
        }
    }

    /// Get canonical string representation
    pub fn canonical(&self) -> String {
        format!(
            "flow://{}/{}@{}",
            self.namespace, self.workflow_id, self.version
        )
    }

    /// Parse from canonical form
    pub fn from_canonical(s: &str) -> Option<Self> {
        if !s.starts_with("flow://") {
            return None;
        }
        let s = &s[7..]; // Remove "flow://"

        let (namespace, rest) = s.split_once('/')?;
        let (workflow_id, version_str) = rest.split_once('@')?;
        let version = version_str.parse().ok()?;

        Some(Self {
            namespace: namespace.to_string(),
            workflow_id: workflow_id.to_string(),
            version,
        })
    }
}

impl Display for WorkflowRef {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.canonical())
    }
}

/// Reference to a step within a workflow
///
/// Provides stable identity for workflow steps across peer failures and migrations.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct WorkflowStepRef {
    /// Reference to the parent workflow
    pub workflow_ref: WorkflowRef,
    /// Step identifier within the workflow (e.g., "step-1", "extract", "join-results")
    pub step_id: String,
}

impl WorkflowStepRef {
    /// Create a new WorkflowStepRef
    pub fn new(workflow_ref: WorkflowRef, step_id: impl Into<String>) -> Self {
        Self {
            workflow_ref,
            step_id: step_id.into(),
        }
    }

    /// Get canonical string representation
    pub fn canonical(&self) -> String {
        format!("{}/{}", self.workflow_ref.canonical(), self.step_id)
    }

    /// Parse from canonical form
    pub fn from_canonical(s: &str) -> Option<Self> {
        // Expected format: flow://workflow/workflow-id@version/step-id
        if !s.starts_with("flow://") {
            return None;
        }

        let s = &s[7..]; // Remove "flow://"

        // Find the last '/' to separate workflow part from step-id
        let last_slash_idx = s.rfind('/')?;
        let workflow_str = &s[..last_slash_idx];
        let step_id = &s[last_slash_idx + 1..];

        // Parse the workflow part
        let (namespace, rest) = workflow_str.split_once('/')?;
        let (workflow_id, version_str) = rest.split_once('@')?;
        let version = version_str.parse().ok()?;

        Some(Self {
            workflow_ref: WorkflowRef {
                namespace: namespace.to_string(),
                workflow_id: workflow_id.to_string(),
                version,
            },
            step_id: step_id.to_string(),
        })
    }
}

impl Display for WorkflowStepRef {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.canonical())
    }
}

/// State of a workflow step in the durable execution model
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkflowStepState {
    /// Waiting for dependencies to be satisfied
    Pending,
    /// Dependencies satisfied, ready to execute
    Ready,
    /// Step has been claimed by a peer for execution
    Claimed,
    /// Step is currently executing
    Running,
    /// Step completed successfully
    Completed,
    /// Step failed during execution
    Failed,
    /// Step scheduled for retry after failure
    Retrying,
    /// Step is blocked, waiting for external input
    Blocked,
}

impl Display for WorkflowStepState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Ready => write!(f, "Ready"),
            Self::Claimed => write!(f, "Claimed"),
            Self::Running => write!(f, "Running"),
            Self::Completed => write!(f, "Completed"),
            Self::Failed => write!(f, "Failed"),
            Self::Retrying => write!(f, "Retrying"),
            Self::Blocked => write!(f, "Blocked"),
        }
    }
}

/// Reason a workflow step is blocked
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BlockedReason {
    /// Awaiting human approval
    AwaitingApproval(String),
    /// Awaiting human input
    AwaitingInput(String),
    /// Required capability not available
    AwaitingCapability(String),
    /// Insufficient resources
    AwaitingResource(String),
    /// Custom blocking reason
    Custom(String),
}

impl Display for BlockedReason {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AwaitingApproval(msg) => write!(f, "AwaitingApproval: {}", msg),
            Self::AwaitingInput(msg) => write!(f, "AwaitingInput: {}", msg),
            Self::AwaitingCapability(cap) => write!(f, "AwaitingCapability: {}", cap),
            Self::AwaitingResource(res) => write!(f, "AwaitingResource: {}", res),
            Self::Custom(msg) => write!(f, "Custom: {}", msg),
        }
    }
}

/// State of a workflow
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkflowState {
    /// Workflow has been created but not started
    Created,
    /// Workflow is currently executing
    Running,
    /// Workflow completed successfully
    Completed,
    /// Workflow failed
    Failed,
}

impl Display for WorkflowState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "Created"),
            Self::Running => write!(f, "Running"),
            Self::Completed => write!(f, "Completed"),
            Self::Failed => write!(f, "Failed"),
        }
    }
}

/// Workflow lifecycle event - becomes a durable operation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkflowOperation {
    /// Workflow was created
    Created { workflow_ref: WorkflowRef },
    /// Workflow started execution
    Started { workflow_ref: WorkflowRef },
    /// A step became ready to execute
    StepReady { step_ref: WorkflowStepRef },
    /// A step was claimed by a peer
    StepClaimed {
        step_ref: WorkflowStepRef,
        claimed_by: String,
    },
    /// A step started executing
    StepExecuting {
        step_ref: WorkflowStepRef,
        claimed_by: String,
    },
    /// A step completed successfully
    StepCompleted {
        step_ref: WorkflowStepRef,
        result_ref: Option<FlowRef>,
    },
    /// A step failed
    StepFailed {
        step_ref: WorkflowStepRef,
        error: String,
        attempt: u32,
    },
    /// A step is blocked pending external input
    StepBlocked {
        step_ref: WorkflowStepRef,
        reason: BlockedReason,
    },
    /// A step retry was scheduled
    StepRetrying {
        step_ref: WorkflowStepRef,
        attempt: u32,
    },
    /// Workflow completed
    Completed { workflow_ref: WorkflowRef },
    /// Workflow failed
    Failed {
        workflow_ref: WorkflowRef,
        error: String,
    },
}

impl WorkflowOperation {
    /// Get the affected workflow reference
    pub fn workflow_ref(&self) -> Option<&WorkflowRef> {
        match self {
            Self::Created { workflow_ref } => Some(workflow_ref),
            Self::Started { workflow_ref } => Some(workflow_ref),
            Self::Completed { workflow_ref } => Some(workflow_ref),
            Self::Failed { workflow_ref, .. } => Some(workflow_ref),
            _ => None,
        }
    }

    /// Get the affected step reference
    pub fn step_ref(&self) -> Option<&WorkflowStepRef> {
        match self {
            Self::StepReady { step_ref } => Some(step_ref),
            Self::StepClaimed { step_ref, .. } => Some(step_ref),
            Self::StepExecuting { step_ref, .. } => Some(step_ref),
            Self::StepCompleted { step_ref, .. } => Some(step_ref),
            Self::StepFailed { step_ref, .. } => Some(step_ref),
            Self::StepBlocked { step_ref, .. } => Some(step_ref),
            Self::StepRetrying { step_ref, .. } => Some(step_ref),
            _ => None,
        }
    }
}

/// Workflow graph node - represents a capability to execute
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNode {
    /// Step identifier (e.g., "extract", "embed", "classify")
    pub step_id: String,
    /// Capability to execute (reference to a CapabilityRef)
    pub capability: String,
    /// Input references required for this step
    pub inputs: Vec<String>, // Step IDs or input slot names
    /// Output slot names produced by this step
    pub outputs: Vec<String>,
    /// State requirements for this step
    pub state_requirements: HashMap<String, String>,
    /// Metadata about this node
    pub metadata: HashMap<String, serde_json::Value>,
}

impl WorkflowNode {
    /// Create a new workflow node
    pub fn new(step_id: impl Into<String>, capability: impl Into<String>) -> Self {
        Self {
            step_id: step_id.into(),
            capability: capability.into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            state_requirements: HashMap::new(),
            metadata: HashMap::new(),
        }
    }

    /// Add an input dependency
    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.inputs.push(input.into());
        self
    }

    /// Add an output slot
    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.outputs.push(output.into());
        self
    }

    /// Add a state requirement
    pub fn with_state_requirement(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.state_requirements.insert(key.into(), value.into());
        self
    }
}

/// Workflow graph definition - durable DAG structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowGraph {
    /// Workflow reference
    pub workflow_ref: WorkflowRef,
    /// Nodes in the workflow
    pub nodes: HashMap<String, WorkflowNode>,
    /// Edges: from_step_id -> to_step_id
    pub edges: HashMap<String, Vec<String>>,
    /// Input references for the entire workflow
    pub inputs: Vec<FlowRef>,
    /// Output slot definitions
    pub outputs: Vec<String>,
    /// Metadata
    pub metadata: HashMap<String, serde_json::Value>,
}

impl WorkflowGraph {
    /// Create a new workflow graph
    pub fn new(workflow_ref: WorkflowRef) -> Self {
        Self {
            workflow_ref,
            nodes: HashMap::new(),
            edges: HashMap::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Add a node to the graph
    pub fn add_node(mut self, node: WorkflowNode) -> Self {
        self.nodes.insert(node.step_id.clone(), node);
        self
    }

    /// Add an edge from one step to another
    pub fn add_edge(mut self, from_step: impl Into<String>, to_step: impl Into<String>) -> Self {
        let from = from_step.into();
        let to = to_step.into();
        self.edges.entry(from).or_insert_with(Vec::new).push(to);
        self
    }

    /// Add an input reference
    pub fn with_input(mut self, input: FlowRef) -> Self {
        self.inputs.push(input);
        self
    }

    /// Validate the graph structure (basic DAG check)
    pub fn validate(&self) -> Result<(), String> {
        // Check all edges reference existing nodes
        for (from, tos) in &self.edges {
            if !self.nodes.contains_key(from) {
                return Err(format!("Edge source '{}' does not exist", from));
            }
            for to in tos {
                if !self.nodes.contains_key(to) {
                    return Err(format!("Edge target '{}' does not exist", to));
                }
            }
        }

        // Check input dependencies reference existing nodes or inputs
        for node in self.nodes.values() {
            for input in &node.inputs {
                if !self.nodes.contains_key(input) && !input.starts_with("input:") {
                    return Err(format!(
                        "Node '{}' has input dependency '{}' that doesn't exist",
                        node.step_id, input
                    ));
                }
            }
        }

        // Simple cycle detection using DFS
        self.detect_cycles()?;

        Ok(())
    }

    /// Detect cycles in the graph using DFS
    fn detect_cycles(&self) -> Result<(), String> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut rec_stack: HashSet<String> = HashSet::new();

        for node_id in self.nodes.keys() {
            if !visited.contains(node_id) {
                self.dfs(node_id, &mut visited, &mut rec_stack)?;
            }
        }

        Ok(())
    }

    /// DFS helper for cycle detection
    fn dfs(
        &self,
        node_id: &str,
        visited: &mut HashSet<String>,
        rec_stack: &mut HashSet<String>,
    ) -> Result<(), String> {
        visited.insert(node_id.to_string());
        rec_stack.insert(node_id.to_string());

        if let Some(neighbors) = self.edges.get(node_id) {
            for neighbor in neighbors {
                if !visited.contains(neighbor) {
                    self.dfs(neighbor, visited, rec_stack)?;
                } else if rec_stack.contains(neighbor) {
                    return Err(format!("Cycle detected: {} -> {}", node_id, neighbor));
                }
            }
        }

        rec_stack.remove(node_id);
        Ok(())
    }

    /// Get nodes with no dependencies (entry points)
    pub fn entry_nodes(&self) -> Vec<String> {
        self.nodes
            .keys()
            .filter(|node_id| {
                if let Some(node) = self.nodes.get(*node_id) {
                    // A node is an entry point if all its inputs come from workflow inputs
                    node.inputs.is_empty()
                        || node.inputs.iter().all(|inp| inp.starts_with("input:"))
                } else {
                    false
                }
            })
            .cloned()
            .collect()
    }

    /// Get nodes that depend on a given step
    pub fn dependents(&self, step_id: &str) -> Vec<String> {
        self.edges
            .get(step_id)
            .map(|deps| deps.clone())
            .unwrap_or_default()
    }

    /// Get nodes that a given step depends on
    pub fn dependencies(&self, step_id: &str) -> Vec<String> {
        if let Some(node) = self.nodes.get(step_id) {
            node.inputs
                .iter()
                .filter(|inp| !inp.starts_with("input:"))
                .cloned()
                .collect()
        } else {
            Vec::new()
        }
    }
}

/// Runtime state for a workflow instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowInstance {
    /// Reference to the workflow definition
    pub workflow_ref: WorkflowRef,
    /// Unique instance ID for this execution
    pub instance_id: String,
    /// Overall workflow state
    pub state: WorkflowState,
    /// Current state of each step
    pub step_states: HashMap<String, WorkflowStepState>,
    /// Which peer claimed each step
    pub step_claims: HashMap<String, String>,
    /// Results produced by each step
    pub step_results: HashMap<String, FlowRef>,
    /// Failed attempts for each step
    pub step_failures: HashMap<String, Vec<String>>,
    /// Blocked reason for each step
    pub step_blocked_reasons: HashMap<String, BlockedReason>,
    /// Timestamp created (ms since UNIX epoch)
    pub created_ms: u128,
    /// Timestamp started (ms since UNIX epoch)
    pub started_ms: Option<u128>,
    /// Timestamp completed (ms since UNIX epoch)
    pub completed_ms: Option<u128>,
}

impl WorkflowInstance {
    /// Create a new workflow instance
    pub fn new(workflow_ref: WorkflowRef, instance_id: impl Into<String>) -> Self {
        Self {
            workflow_ref,
            instance_id: instance_id.into(),
            state: WorkflowState::Created,
            step_states: HashMap::new(),
            step_claims: HashMap::new(),
            step_results: HashMap::new(),
            step_failures: HashMap::new(),
            step_blocked_reasons: HashMap::new(),
            created_ms: crate::now_ms(),
            started_ms: None,
            completed_ms: None,
        }
    }

    /// Get the state of a step
    pub fn get_step_state(&self, step_id: &str) -> Option<WorkflowStepState> {
        self.step_states.get(step_id).copied()
    }

    /// Set the state of a step
    pub fn set_step_state(&mut self, step_id: impl Into<String>, state: WorkflowStepState) {
        self.step_states.insert(step_id.into(), state);
    }

    /// Mark a step as claimed
    pub fn claim_step(
        &mut self,
        step_id: impl Into<String>,
        claimed_by: impl Into<String>,
    ) -> Result<(), String> {
        let step_id = step_id.into();
        let claimed_by = claimed_by.into();

        match self.get_step_state(&step_id) {
            Some(WorkflowStepState::Ready) => {
                self.set_step_state(step_id.clone(), WorkflowStepState::Claimed);
                self.step_claims.insert(step_id, claimed_by);
                Ok(())
            }
            Some(state) => Err(format!("Cannot claim step in state {}", state)),
            None => Err(format!("Step not found: {}", step_id)),
        }
    }

    /// Record a step completion
    pub fn complete_step(
        &mut self,
        step_id: impl Into<String>,
        result_ref: FlowRef,
    ) -> Result<(), String> {
        let step_id = step_id.into();
        self.set_step_state(step_id.clone(), WorkflowStepState::Completed);
        self.step_results.insert(step_id, result_ref);
        Ok(())
    }

    /// Record a step failure
    pub fn fail_step(&mut self, step_id: impl Into<String>, error: String) -> Result<(), String> {
        let step_id = step_id.into();
        self.set_step_state(step_id.clone(), WorkflowStepState::Failed);
        self.step_failures
            .entry(step_id)
            .or_insert_with(Vec::new)
            .push(error);
        Ok(())
    }

    /// Check if workflow is terminal (won't change state)
    pub fn is_terminal(&self) -> bool {
        matches!(self.state, WorkflowState::Completed | WorkflowState::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_ref_canonical() {
        let r = WorkflowRef::new("workflow", "order-processing", 1);
        assert_eq!(r.canonical(), "flow://workflow/order-processing@1");
    }

    #[test]
    fn test_workflow_ref_parse() {
        let s = "flow://workflow/order-processing@1";
        let r = WorkflowRef::from_canonical(s).unwrap();
        assert_eq!(r.namespace, "workflow");
        assert_eq!(r.workflow_id, "order-processing");
        assert_eq!(r.version, 1);
    }

    #[test]
    fn test_workflow_step_ref_canonical() {
        let wf = WorkflowRef::new("workflow", "test", 1);
        let step = WorkflowStepRef::new(wf, "step-1");
        assert_eq!(step.canonical(), "flow://workflow/test@1/step-1");
    }

    #[test]
    fn test_workflow_step_ref_parse() {
        let s = "flow://workflow/test@1/step-1";
        let step = WorkflowStepRef::from_canonical(s).unwrap();
        assert_eq!(step.step_id, "step-1");
        assert_eq!(step.workflow_ref.workflow_id, "test");
    }

    #[test]
    fn test_workflow_graph_validation() {
        let wf = WorkflowRef::new("workflow", "test", 1);
        let graph = WorkflowGraph::new(wf)
            .add_node(WorkflowNode::new("step-1", "capability-a"))
            .add_node(WorkflowNode::new("step-2", "capability-b").with_input("step-1"))
            .add_edge("step-1", "step-2");

        assert!(graph.validate().is_ok());
    }

    #[test]
    fn test_workflow_graph_cycle_detection() {
        let wf = WorkflowRef::new("workflow", "test", 1);
        let graph = WorkflowGraph::new(wf)
            .add_node(WorkflowNode::new("step-1", "capability-a"))
            .add_node(WorkflowNode::new("step-2", "capability-b"))
            .add_edge("step-1", "step-2")
            .add_edge("step-2", "step-1");

        assert!(graph.validate().is_err());
    }

    #[test]
    fn test_workflow_graph_entry_nodes() {
        let wf = WorkflowRef::new("workflow", "test", 1);
        let graph = WorkflowGraph::new(wf)
            .add_node(WorkflowNode::new("step-1", "capability-a"))
            .add_node(WorkflowNode::new("step-2", "capability-b").with_input("step-1"))
            .add_edge("step-1", "step-2");

        let entries = graph.entry_nodes();
        assert_eq!(entries.len(), 1);
        assert!(entries.contains(&"step-1".to_string()));
    }

    #[test]
    fn test_workflow_instance_lifecycle() {
        let wf = WorkflowRef::new("workflow", "test", 1);
        let mut instance = WorkflowInstance::new(wf, "instance-1");

        instance.set_step_state("step-1", WorkflowStepState::Pending);
        assert_eq!(
            instance.get_step_state("step-1"),
            Some(WorkflowStepState::Pending)
        );

        instance.set_step_state("step-1", WorkflowStepState::Ready);
        let claim_result = instance.claim_step("step-1", "peer-a");
        assert!(claim_result.is_ok());
        assert_eq!(
            instance.get_step_state("step-1"),
            Some(WorkflowStepState::Claimed)
        );
    }

    #[test]
    fn test_workflow_blocked_reason() {
        let reason = BlockedReason::AwaitingApproval("Needs manager approval".to_string());
        assert_eq!(
            reason.to_string(),
            "AwaitingApproval: Needs manager approval"
        );
    }
}
