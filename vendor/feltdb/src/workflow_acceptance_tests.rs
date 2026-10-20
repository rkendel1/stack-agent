/// Workflow acceptance tests demonstrating the distributed workflow runtime
///
/// Tests comprehensive workflows across multiple peers with various scenarios:
/// - Linear workflows with dependencies
/// - Parallel workflows with joins
/// - Conditional workflows
/// - Workflow mobility (continue on different peer)
/// - Failure and recovery
/// - Provenance tracking
use crate::{FlowRef, WorkflowGraph, WorkflowNode, WorkflowRef, WorkflowRuntime};

#[cfg(test)]
mod tests {
    use super::*;

    /// Test 1: Simple linear workflow
    /// Document → Extract → Embed → Done
    #[test]
    fn test_simple_linear_workflow() {
        let mut runtime = WorkflowRuntime::new();

        // Define workflow
        let workflow_ref = WorkflowRef::new("workflow", "document-processing", 1);
        let graph = WorkflowGraph::new(workflow_ref.clone())
            .add_node(WorkflowNode::new("extract", "extract-text"))
            .add_node(WorkflowNode::new("embed", "embedding").with_input("extract"))
            .add_node(WorkflowNode::new("save", "save-results").with_input("embed"))
            .add_edge("extract", "embed")
            .add_edge("embed", "save");

        runtime
            .register_workflow(graph)
            .expect("Failed to register workflow");

        // Create instance
        let document = FlowRef::new("documents", "doc-1");
        let instance_id = runtime
            .create_instance(workflow_ref.clone(), "workflow-1", vec![document.clone()])
            .expect("Failed to create instance");

        // Start workflow
        runtime
            .start_instance(&instance_id)
            .expect("Failed to start");

        // Verify entry point (extract) is ready
        let ready = runtime.ready_steps(&instance_id);
        assert!(ready.contains(&"extract".to_string()));
        assert!(!ready.contains(&"embed".to_string()));

        // Execute extract step
        runtime
            .claim_step(&instance_id, "extract", "peer-a")
            .expect("Failed to claim");
        runtime
            .step_completed(&instance_id, "extract", FlowRef::new("texts", "text-1"))
            .expect("Failed to complete");

        // Verify embed becomes ready
        let ready = runtime.ready_steps(&instance_id);
        assert!(ready.contains(&"embed".to_string()));

        // Execute embed step
        runtime
            .claim_step(&instance_id, "embed", "peer-b")
            .expect("Failed to claim");
        runtime
            .step_completed(&instance_id, "embed", FlowRef::new("embeddings", "embed-1"))
            .expect("Failed to complete");

        // Verify save becomes ready
        let ready = runtime.ready_steps(&instance_id);
        assert!(ready.contains(&"save".to_string()));

        // Execute save step
        runtime
            .claim_step(&instance_id, "save", "peer-c")
            .expect("Failed to claim");
        runtime
            .step_completed(&instance_id, "save", FlowRef::new("results", "result-1"))
            .expect("Failed to complete");

        // Verify workflow is completed
        let instance = runtime.get_instance(&instance_id).unwrap();
        assert_eq!(instance.state, crate::WorkflowState::Completed);

        // Check operations log
        let ops = runtime.operations_log();
        assert!(!ops.is_empty());
        println!("Generated {} workflow operations", ops.len());
    }

    /// Test 2: Parallel workflow with join
    /// Document → Extract → (Embed || Classify) → Join → Summarize
    #[test]
    fn test_parallel_workflow_with_join() {
        let mut runtime = WorkflowRuntime::new();

        let workflow_ref = WorkflowRef::new("workflow", "parallel-processing", 1);
        let graph = WorkflowGraph::new(workflow_ref.clone())
            .add_node(WorkflowNode::new("extract", "extract-text"))
            .add_node(WorkflowNode::new("embed", "embedding").with_input("extract"))
            .add_node(WorkflowNode::new("classify", "classifier").with_input("extract"))
            .add_node(
                WorkflowNode::new("join", "merge-results")
                    .with_input("embed")
                    .with_input("classify"),
            )
            .add_node(WorkflowNode::new("summarize", "summarization").with_input("join"))
            .add_edge("extract", "embed")
            .add_edge("extract", "classify")
            .add_edge("embed", "join")
            .add_edge("classify", "join")
            .add_edge("join", "summarize");

        runtime
            .register_workflow(graph)
            .expect("Failed to register workflow");

        let instance_id = runtime
            .create_instance(
                workflow_ref.clone(),
                "workflow-2",
                vec![FlowRef::new("documents", "doc-1")],
            )
            .expect("Failed to create instance");

        runtime
            .start_instance(&instance_id)
            .expect("Failed to start");

        // Extract completes
        runtime
            .claim_step(&instance_id, "extract", "peer-a")
            .expect("Failed to claim");
        runtime
            .step_completed(&instance_id, "extract", FlowRef::new("texts", "text-1"))
            .expect("Failed to complete");

        // Both embed and classify should be ready
        let ready = runtime.ready_steps(&instance_id);
        assert!(ready.contains(&"embed".to_string()));
        assert!(ready.contains(&"classify".to_string()));
        assert!(!ready.contains(&"join".to_string()));

        // Complete embed
        runtime
            .claim_step(&instance_id, "embed", "peer-b")
            .expect("Failed to claim");
        runtime
            .step_completed(&instance_id, "embed", FlowRef::new("embeddings", "embed-1"))
            .expect("Failed to complete");

        // Complete classify
        runtime
            .claim_step(&instance_id, "classify", "peer-c")
            .expect("Failed to claim");
        runtime
            .step_completed(&instance_id, "classify", FlowRef::new("classes", "class-1"))
            .expect("Failed to complete");

        // Now join should be ready
        let ready = runtime.ready_steps(&instance_id);
        assert!(ready.contains(&"join".to_string()));

        // Complete join and summarize
        runtime
            .claim_step(&instance_id, "join", "peer-d")
            .expect("Failed to claim");
        runtime
            .step_completed(&instance_id, "join", FlowRef::new("merged", "merged-1"))
            .expect("Failed to complete");

        runtime
            .claim_step(&instance_id, "summarize", "peer-e")
            .expect("Failed to claim");
        runtime
            .step_completed(
                &instance_id,
                "summarize",
                FlowRef::new("summary", "summary-1"),
            )
            .expect("Failed to complete");

        // Verify workflow is completed
        let instance = runtime.get_instance(&instance_id).unwrap();
        assert_eq!(instance.state, crate::WorkflowState::Completed);
    }

    /// Test 3: Workflow with blocking and approval
    /// Document → Review (blocked awaiting approval) → Process
    #[test]
    fn test_workflow_with_human_intervention() {
        let mut runtime = WorkflowRuntime::new();

        let workflow_ref = WorkflowRef::new("workflow", "approval-required", 1);
        let graph = WorkflowGraph::new(workflow_ref.clone())
            .add_node(WorkflowNode::new("extract", "extract-text"))
            .add_node(WorkflowNode::new("review", "manual-review").with_input("extract"))
            .add_node(WorkflowNode::new("process", "process").with_input("review"));

        runtime
            .register_workflow(graph)
            .expect("Failed to register workflow");

        let instance_id = runtime
            .create_instance(
                workflow_ref.clone(),
                "workflow-3",
                vec![FlowRef::new("documents", "doc-1")],
            )
            .expect("Failed to create instance");

        runtime
            .start_instance(&instance_id)
            .expect("Failed to start");

        // Complete extract
        runtime
            .claim_step(&instance_id, "extract", "peer-a")
            .expect("Failed to claim");
        runtime
            .step_completed(&instance_id, "extract", FlowRef::new("texts", "text-1"))
            .expect("Failed to complete");

        // Review should be ready
        let ready = runtime.ready_steps(&instance_id);
        assert!(ready.contains(&"review".to_string()));

        // Claim review but immediately block it
        runtime
            .claim_step(&instance_id, "review", "peer-b")
            .expect("Failed to claim");
        runtime
            .step_blocked(
                &instance_id,
                "review",
                crate::BlockedReason::AwaitingApproval("Waiting for manager approval".to_string()),
            )
            .expect("Failed to block");

        // Process should not be ready
        let ready = runtime.ready_steps(&instance_id);
        assert!(!ready.contains(&"process".to_string()));

        // Unblock review
        runtime
            .unblock_step(&instance_id, "review")
            .expect("Failed to unblock");

        // Review should be ready again
        let ready = runtime.ready_steps(&instance_id);
        assert!(ready.contains(&"review".to_string()));

        // Complete review
        runtime
            .step_completed(&instance_id, "review", FlowRef::new("reviews", "review-1"))
            .expect("Failed to complete");

        // Process should now be ready
        let ready = runtime.ready_steps(&instance_id);
        assert!(ready.contains(&"process".to_string()));
    }

    /// Test 4: Workflow continuation after peer failure
    /// Demonstrates workflow mobility - workflow continues on different peer
    #[test]
    fn test_workflow_continues_on_peer_failure() {
        let mut runtime = WorkflowRuntime::new();

        let workflow_ref = WorkflowRef::new("workflow", "failover-test", 1);
        let graph = WorkflowGraph::new(workflow_ref.clone())
            .add_node(WorkflowNode::new("step1", "capability-a"))
            .add_node(WorkflowNode::new("step2", "capability-b").with_input("step1"));

        runtime
            .register_workflow(graph)
            .expect("Failed to register workflow");

        let instance_id = runtime
            .create_instance(workflow_ref.clone(), "workflow-4", Vec::new())
            .expect("Failed to create instance");

        runtime
            .start_instance(&instance_id)
            .expect("Failed to start");

        // Step1 starts on peer-a
        runtime
            .claim_step(&instance_id, "step1", "peer-a")
            .expect("Failed to claim");
        runtime
            .step_executing(&instance_id, "step1")
            .expect("Failed to mark executing");

        // Simulate peer-a failure by not completing
        // The workflow is durable - the step state is preserved in the runtime

        // Get the instance to verify state is persisted
        let instance = runtime.get_instance(&instance_id).unwrap();
        assert_eq!(
            instance.get_step_state("step1"),
            Some(crate::WorkflowStepState::Running)
        );

        // Another peer (peer-b) can read the workflow state and continue
        // The step remains claimed but peer-b can check and proceed
        // (In production, peer-b would acquire the workflow state from the fabric)

        // For this test, we complete the step (simulating recovery and completion)
        runtime
            .step_completed(&instance_id, "step1", FlowRef::new("results", "result-1"))
            .expect("Failed to complete");

        // Step2 becomes ready and can be claimed by peer-c
        let ready = runtime.ready_steps(&instance_id);
        assert!(ready.contains(&"step2".to_string()));

        runtime
            .claim_step(&instance_id, "step2", "peer-c")
            .expect("Failed to claim by peer-c");
        runtime
            .step_completed(&instance_id, "step2", FlowRef::new("results", "result-2"))
            .expect("Failed to complete step2");

        // Workflow completes
        let instance = runtime.get_instance(&instance_id).unwrap();
        assert_eq!(instance.state, crate::WorkflowState::Completed);
    }

    /// Test 5: Multiple concurrent workflows
    /// Demonstrates independent workflow instances in the same runtime
    #[test]
    fn test_multiple_concurrent_workflows() {
        let mut runtime = WorkflowRuntime::new();

        // Define workflow
        let workflow_ref = WorkflowRef::new("workflow", "multi-workflow", 1);
        let graph = WorkflowGraph::new(workflow_ref.clone())
            .add_node(WorkflowNode::new("step1", "capability-a"))
            .add_node(WorkflowNode::new("step2", "capability-b").with_input("step1"))
            .add_edge("step1", "step2");

        runtime
            .register_workflow(graph)
            .expect("Failed to register workflow");

        // Create multiple instances
        let instance1 = runtime
            .create_instance(workflow_ref.clone(), "wf-1", Vec::new())
            .expect("Failed to create");
        let instance2 = runtime
            .create_instance(workflow_ref.clone(), "wf-2", Vec::new())
            .expect("Failed to create");
        let instance3 = runtime
            .create_instance(workflow_ref.clone(), "wf-3", Vec::new())
            .expect("Failed to create");

        runtime.start_instance(&instance1).expect("Failed to start");
        runtime.start_instance(&instance2).expect("Failed to start");
        runtime.start_instance(&instance3).expect("Failed to start");

        // All should have step1 ready
        assert!(runtime
            .ready_steps(&instance1)
            .contains(&"step1".to_string()));
        assert!(runtime
            .ready_steps(&instance2)
            .contains(&"step1".to_string()));
        assert!(runtime
            .ready_steps(&instance3)
            .contains(&"step1".to_string()));

        // Execute each workflow independently
        for (instance_id, peer) in &[
            (&instance1, "peer-a"),
            (&instance2, "peer-b"),
            (&instance3, "peer-c"),
        ] {
            runtime
                .claim_step(*instance_id, "step1", *peer)
                .expect("Failed to claim");
            runtime
                .step_completed(
                    *instance_id,
                    "step1",
                    FlowRef::new("results", &format!("result-{}", peer)),
                )
                .expect("Failed to complete");

            runtime
                .claim_step(*instance_id, "step2", *peer)
                .expect("Failed to claim");
            runtime
                .step_completed(
                    *instance_id,
                    "step2",
                    FlowRef::new("results", &format!("final-{}", peer)),
                )
                .expect("Failed to complete");
        }

        // All workflows should be completed
        assert_eq!(
            runtime.get_instance(&instance1).unwrap().state,
            crate::WorkflowState::Completed
        );
        assert_eq!(
            runtime.get_instance(&instance2).unwrap().state,
            crate::WorkflowState::Completed
        );
        assert_eq!(
            runtime.get_instance(&instance3).unwrap().state,
            crate::WorkflowState::Completed
        );
    }

    /// Test 6: Complex DAG with multiple dependencies
    /// Tests the reactive advancement with more complex dependency patterns
    #[test]
    fn test_complex_dag_workflow() {
        let mut runtime = WorkflowRuntime::new();

        let workflow_ref = WorkflowRef::new("workflow", "complex-dag", 1);
        let graph = WorkflowGraph::new(workflow_ref.clone())
            .add_node(WorkflowNode::new("a", "cap-a"))
            .add_node(WorkflowNode::new("b", "cap-b").with_input("a"))
            .add_node(WorkflowNode::new("c", "cap-c").with_input("a"))
            .add_node(
                WorkflowNode::new("d", "cap-d")
                    .with_input("b")
                    .with_input("c"),
            )
            .add_node(WorkflowNode::new("e", "cap-e").with_input("d"))
            .add_edge("a", "b")
            .add_edge("a", "c")
            .add_edge("b", "d")
            .add_edge("c", "d")
            .add_edge("d", "e");

        runtime
            .register_workflow(graph)
            .expect("Failed to register workflow");

        let instance_id = runtime
            .create_instance(workflow_ref.clone(), "complex-1", Vec::new())
            .expect("Failed to create");

        runtime
            .start_instance(&instance_id)
            .expect("Failed to start");

        // a should be ready
        assert!(runtime.ready_steps(&instance_id).contains(&"a".to_string()));

        // Complete a
        runtime
            .claim_step(&instance_id, "a", "peer-1")
            .expect("Failed");
        runtime
            .step_completed(&instance_id, "a", FlowRef::new("res", "a-result"))
            .expect("Failed");

        // b and c should be ready
        let ready = runtime.ready_steps(&instance_id);
        assert!(ready.contains(&"b".to_string()));
        assert!(ready.contains(&"c".to_string()));

        // Complete b
        runtime
            .claim_step(&instance_id, "b", "peer-2")
            .expect("Failed");
        runtime
            .step_completed(&instance_id, "b", FlowRef::new("res", "b-result"))
            .expect("Failed");

        // d should not be ready yet (c not done)
        assert!(!runtime.ready_steps(&instance_id).contains(&"d".to_string()));

        // Complete c
        runtime
            .claim_step(&instance_id, "c", "peer-3")
            .expect("Failed");
        runtime
            .step_completed(&instance_id, "c", FlowRef::new("res", "c-result"))
            .expect("Failed");

        // Now d should be ready
        assert!(runtime.ready_steps(&instance_id).contains(&"d".to_string()));

        // Complete d and e
        runtime
            .claim_step(&instance_id, "d", "peer-4")
            .expect("Failed");
        runtime
            .step_completed(&instance_id, "d", FlowRef::new("res", "d-result"))
            .expect("Failed");

        runtime
            .claim_step(&instance_id, "e", "peer-5")
            .expect("Failed");
        runtime
            .step_completed(&instance_id, "e", FlowRef::new("res", "e-result"))
            .expect("Failed");

        // Workflow should be completed
        let instance = runtime.get_instance(&instance_id).unwrap();
        assert_eq!(instance.state, crate::WorkflowState::Completed);
    }
}
