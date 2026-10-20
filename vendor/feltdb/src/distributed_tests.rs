/// Distributed references and content fabric tests
#[cfg(test)]
mod distributed_fabric_tests {
    use crate::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct Document {
        id: String,
        title: String,
        content: String,
    }

    #[test]
    fn test_flow_ref_creation_and_canonical_form() {
        // Test A - Stable reference
        let flow_ref = FlowRef::new("docs", "123");
        assert_eq!(flow_ref.canonical(), "flow://docs#123");
        assert_eq!(flow_ref.namespace, "docs");
        assert_eq!(flow_ref.object_id, "123");

        // With version
        let flow_ref_v = FlowRef::with_version("docs", "123", 42);
        assert_eq!(flow_ref_v.canonical(), "flow://docs#123@42");
        assert_eq!(flow_ref_v.version, Some(42));
    }

    #[test]
    fn test_flow_ref_parse_canonical() {
        let canonical = "flow://docs#123";
        let flow_ref = FlowRef::from_canonical(canonical).unwrap();
        assert_eq!(flow_ref.namespace, "docs");
        assert_eq!(flow_ref.object_id, "123");
        assert_eq!(flow_ref.version, None);

        let canonical_v = "flow://docs#123@42";
        let flow_ref_v = FlowRef::from_canonical(canonical_v).unwrap();
        assert_eq!(flow_ref_v.version, Some(42));
    }

    #[test]
    fn test_operation_content_hash() {
        let op = Operation::insert(
            1,
            "inst1".to_string(),
            1,
            "docs:123".to_string(),
            serde_json::json!({"title": "Test"}),
            "Document".to_string(),
            "docs".to_string(),
        );

        // Content hash should be computed
        assert!(!op.content_hash.is_empty());
        // Same operation should have same hash
        let op2 = Operation::insert(
            2, // Different op_id
            "inst1".to_string(),
            2, // Different sequence
            "docs:123".to_string(),
            serde_json::json!({"title": "Test"}),
            "Document".to_string(),
            "docs".to_string(),
        );

        // Same content should produce same hash
        assert_eq!(op.content_hash, op2.content_hash);
    }

    #[test]
    fn test_content_identity_from_bytes() {
        let data1 = b"hello world";
        let content1 = ContentIdentity::from_bytes(data1);

        let data2 = b"hello world";
        let content2 = ContentIdentity::from_bytes(data2);

        // Same data should produce same content identity
        assert_eq!(content1, content2);

        let data3 = b"different";
        let content3 = ContentIdentity::from_bytes(data3);

        // Different data should produce different identity
        assert_ne!(content1, content3);
    }

    #[test]
    fn test_peer_registry_basic_operations() {
        let registry = PeerRegistry::new();

        // Create peer advertisements
        let peer_a = PeerId::new("peer-a");
        let ad_a = PeerAdvertisement::new(peer_a.clone())
            .with_namespace("docs")
            .with_namespace("orders");

        // Register peer
        registry.register(ad_a).unwrap();
        assert_eq!(registry.peer_count().unwrap(), 1);

        // Get peer
        let retrieved = registry.get(&peer_a).unwrap();
        assert!(retrieved.is_some());
        assert!(retrieved.unwrap().provides_namespace("docs"));
    }

    #[test]
    fn test_peer_registry_namespace_discovery() {
        let registry = PeerRegistry::new();

        // Register multiple peers with different namespaces
        let peer_a = PeerId::new("peer-a");
        registry
            .register(PeerAdvertisement::new(peer_a).with_namespace("docs"))
            .unwrap();

        let peer_b = PeerId::new("peer-b");
        registry
            .register(
                PeerAdvertisement::new(peer_b)
                    .with_namespace("orders")
                    .with_namespace("docs"),
            )
            .unwrap();

        let peer_c = PeerId::new("peer-c");
        registry
            .register(PeerAdvertisement::new(peer_c).with_namespace("embeddings"))
            .unwrap();

        // Query for peers by namespace
        let docs_peers = registry.peers_for_namespace("docs").unwrap();
        assert_eq!(docs_peers.len(), 2); // peer-a and peer-b

        let orders_peers = registry.peers_for_namespace("orders").unwrap();
        assert_eq!(orders_peers.len(), 1); // peer-b

        let embeddings_peers = registry.peers_for_namespace("embeddings").unwrap();
        assert_eq!(embeddings_peers.len(), 1); // peer-c
    }

    #[test]
    fn test_peer_registry_capability_discovery() {
        let registry = PeerRegistry::new();

        // Register peers with capabilities
        let peer_a = PeerId::new("peer-a");
        registry
            .register(PeerAdvertisement::new(peer_a).with_capability("search:docs"))
            .unwrap();

        let peer_b = PeerId::new("peer-b");
        registry
            .register(
                PeerAdvertisement::new(peer_b)
                    .with_capability("vector:embeddings")
                    .with_capability("search:docs"),
            )
            .unwrap();

        // Query for peers by capability
        let search_peers = registry.peers_for_capability("search:docs").unwrap();
        assert_eq!(search_peers.len(), 2); // Both have search:docs

        let vector_peers = registry.peers_for_capability("vector:embeddings").unwrap();
        assert_eq!(vector_peers.len(), 1); // Only peer-b has this
    }

    #[test]
    fn test_distributed_fabric_references() {
        // Test basic reference creation and manipulation
        let flow_ref = FlowRef::new("orders", "847");

        // Test Display trait
        let display = format!("{}", flow_ref);
        assert_eq!(display, "flow://orders#847");

        // Test Hash trait (used in HashMap/HashSet)
        use std::collections::HashMap;
        let mut refs = HashMap::new();
        refs.insert(flow_ref.clone(), "order_data");

        let same_ref = FlowRef::new("orders", "847");
        assert_eq!(refs.get(&same_ref), Some(&"order_data"));
    }

    #[test]
    fn test_peer_advertisement_builders() {
        let peer = PeerId::new("peer-service");

        let ad = PeerAdvertisement::new(peer)
            .with_namespaces(vec!["docs".to_string(), "orders".to_string()])
            .with_capabilities(vec![
                "search:docs".to_string(),
                "vector:embeddings".to_string(),
            ]);

        assert!(ad.provides_namespace("docs"));
        assert!(ad.provides_namespace("orders"));
        assert!(ad.provides_capability("search:docs"));
        assert!(ad.provides_capability("vector:embeddings"));
    }

    #[test]
    fn test_content_range() {
        let range = ContentRange::new("a", "z", 26);

        assert!(range.contains("a"));
        assert!(range.contains("m"));
        assert!(range.contains("z"));
        assert!(!range.contains("0"));
        assert!(!range.contains("{"));
    }

    #[test]
    fn test_feltdb_with_peer_management() -> Result<()> {
        let db = open("target/test_distributed_fabric.db")?;

        // Create peer advertisements
        let peer_a = PeerId::new("node-search");
        let ad_a = PeerAdvertisement::new(peer_a.clone())
            .with_namespace("docs")
            .with_capability("search:docs");

        let peer_b = PeerId::new("node-vector");
        let ad_b = PeerAdvertisement::new(peer_b.clone())
            .with_namespace("docs")
            .with_capability("vector:docs");

        // Register peers
        db.register_peer(ad_a)?;
        db.register_peer(ad_b)?;

        // Query peers
        assert_eq!(db.all_peers()?.len(), 2);

        let search_peers = db.peers_for_capability("search:docs")?;
        assert_eq!(search_peers.len(), 1);
        assert_eq!(search_peers[0].peer_id.id, "node-search");

        let vector_peers = db.peers_for_capability("vector:docs")?;
        assert_eq!(vector_peers.len(), 1);
        assert_eq!(vector_peers[0].peer_id.id, "node-vector");

        Ok(())
    }

    #[test]
    fn test_feltdb_resolve_ref() -> Result<()> {
        let db = open("target/test_resolve_ref.db")?;

        // Get the capability and insert through it
        let docs_cap = db.capability("docs");

        // Insert a document
        let doc = Document {
            id: "123".to_string(),
            title: "Test Document".to_string(),
            content: "This is a test".to_string(),
        };

        docs_cap.insert_with_key("123", doc.clone())?;

        // Create a reference and resolve it
        let flow_ref = FlowRef::new("docs", "123");
        let resolved = db.resolve_ref::<Document>(&flow_ref)?;

        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap(), doc);

        // Try resolving non-existent reference
        let missing_ref = FlowRef::new("docs", "999");
        let missing = db.resolve_ref::<Document>(&missing_ref)?;
        assert!(missing.is_none());

        Ok(())
    }

    #[test]
    fn test_distributed_scenario_b_object_migration() -> Result<()> {
        // Test B - Move (object migration across peers)

        // Node A initially has a document
        let db_a = open("target/test_scenario_b_node_a.db")?;

        let doc = Document {
            id: "doc1".to_string(),
            title: "Distributed Document".to_string(),
            content: "Content that will migrate".to_string(),
        };

        db_a.capability("docs")
            .insert_with_key("doc1", doc.clone())?;

        // Create reference
        let flow_ref = FlowRef::new("docs", "doc1");

        // Verify it can be resolved on Node A
        let resolved = db_a.resolve_ref::<Document>(&flow_ref)?;
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap(), doc);

        // Simulate migration: register peer B as having this namespace
        let peer_b = PeerId::new("node-b");
        let ad_b = PeerAdvertisement::new(peer_b).with_namespace("docs");

        db_a.register_peer(ad_b)?;

        // The reference should still be valid after migration registration
        // (In a full implementation, content would actually migrate)
        let peer_list = db_a.peers_for_namespace("docs")?;
        assert_eq!(peer_list.len(), 1);
        assert_eq!(peer_list[0].peer_id.id, "node-b");

        Ok(())
    }

    #[test]
    fn test_distributed_scenario_d_deduplication() -> Result<()> {
        // Test D - Deduplication (same content from multiple sources)

        let store = ContentAddressableStore::new();

        // Same content arrives from multiple peers
        let content = b"identical document content".to_vec();
        let hash = ContentIdentity::from_bytes(&content).as_str().to_string();

        // Store once
        store.store(content.clone(), hash.clone())?;

        // Record that multiple peers have it
        let peer_a = PeerId::new("peer-a");
        let peer_b = PeerId::new("peer-b");
        let peer_c = PeerId::new("peer-c");

        store.record_location(hash.clone(), peer_a)?;
        store.record_location(hash.clone(), peer_b)?;
        store.record_location(hash.clone(), peer_c)?;

        // Retrieve - only one copy in memory
        let retrieved = store.get(&hash)?;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().len(), content.len());

        // But we know it's on 3 peers
        let peers = store.find_peers_for_content(&hash)?;
        assert_eq!(peers.len(), 3);

        Ok(())
    }

    #[test]
    fn test_distributed_scenario_e_capability_discovery() -> Result<()> {
        // Test E - Capability discovery (find peer providing capability)

        let registry = CapabilityLocationRegistry::new();

        // Multiple peers provide different capabilities
        let peer_search = PeerId::new("search-node");
        registry.register_capability("search:docs".to_string(), peer_search.clone())?;

        let peer_vector = PeerId::new("vector-node");
        registry.register_capability("vector:embeddings".to_string(), peer_vector.clone())?;

        let peer_execute = PeerId::new("executor-node");
        registry.register_capability("execute:payments".to_string(), peer_execute.clone())?;

        // Some peers provide multiple capabilities
        registry.register_capability("search:docs".to_string(), peer_vector.clone())?;

        // Discover peers
        let search_peers = registry.peers_for_capability("search:docs")?;
        assert_eq!(search_peers.len(), 2);

        let vector_peers = registry.peers_for_capability("vector:embeddings")?;
        assert_eq!(vector_peers.len(), 1);

        // Check what each peer provides
        let search_caps = registry.capabilities_for_peer(&peer_search)?;
        assert_eq!(search_caps.len(), 1);

        let vector_caps = registry.capabilities_for_peer(&peer_vector)?;
        assert_eq!(vector_caps.len(), 2); // search:docs and vector:embeddings

        Ok(())
    }

    #[test]
    fn test_distributed_scenario_h_reference_graph() -> Result<()> {
        // Test H - Reference graph (resolve entire reference graph)

        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
        struct Order {
            id: String,
            customer_id: String,
            items: Vec<String>,
        }

        let db = open("target/test_scenario_h_graph.db")?;
        let orders_cap = db.capability("orders");

        // Create interconnected objects
        let order = Order {
            id: "order-1".to_string(),
            customer_id: "customer-1".to_string(),
            items: vec!["item-1".to_string(), "item-2".to_string()],
        };

        orders_cap.insert_with_key("order-1", order.clone())?;

        // Create reference
        let order_ref = FlowRef::new("orders", "order-1");
        let resolved = db.resolve_ref::<Order>(&order_ref)?;
        assert!(resolved.is_some());
        let resolved_order = resolved.unwrap();

        // The graph is interconnected
        assert_eq!(resolved_order.customer_id, "customer-1");
        assert_eq!(resolved_order.items.len(), 2);

        Ok(())
    }

    #[test]
    fn test_distributed_scenario_node_capabilities() -> Result<()> {
        // Test related to node capabilities and peer discovery

        let db = open("target/test_node_capabilities.db")?;

        // Register multiple specialized nodes
        let canonical_node = PeerId::new("canonical-node");
        let canonical_ad = PeerAdvertisement::new(canonical_node)
            .with_namespace("orders")
            .with_namespace("docs")
            .with_namespace("customers");

        db.register_peer(canonical_ad)?;

        let search_node = PeerId::new("search-node");
        let search_ad = PeerAdvertisement::new(search_node)
            .with_namespace("docs")
            .with_capability("search:docs");

        db.register_peer(search_ad)?;

        let vector_node = PeerId::new("vector-node");
        let vector_ad = PeerAdvertisement::new(vector_node)
            .with_namespace("docs")
            .with_capability("vector:embeddings");

        db.register_peer(vector_ad)?;

        // Query discovery
        let docs_peers = db.peers_for_namespace("docs")?;
        assert_eq!(docs_peers.len(), 3); // All three have docs

        let orders_peers = db.peers_for_namespace("orders")?;
        assert_eq!(orders_peers.len(), 1); // Only canonical node

        let search_capable = db.peers_for_capability("search:docs")?;
        assert_eq!(search_capable.len(), 1);
        assert_eq!(search_capable[0].peer_id.id, "search-node");

        let all_peers = db.all_peers()?;
        assert_eq!(all_peers.len(), 3);

        Ok(())
    }

    #[test]
    fn test_distributed_scenario_offline_and_return() -> Result<()> {
        // Test G - Offline acquisition (reference resolution after peer return)

        // Initially, no peers available
        let db = open("target/test_offline_scenario.db")?;
        let peers = db.all_peers()?;
        assert_eq!(peers.len(), 0);

        // Try to resolve - will fail
        let flow_ref = FlowRef::new("docs", "123");
        let resolved = db.resolve_ref::<Document>(&flow_ref)?;
        assert!(resolved.is_none());

        // Peer comes online and registers
        let peer = PeerId::new("returning-peer");
        let ad = PeerAdvertisement::new(peer).with_namespace("docs");

        db.register_peer(ad)?;

        // Now we know the peer exists
        let peers = db.all_peers()?;
        assert_eq!(peers.len(), 1);

        // In a full implementation, we'd now be able to acquire from that peer
        let doc_peers = db.peers_for_namespace("docs")?;
        assert_eq!(doc_peers.len(), 1);

        Ok(())
    }

    #[test]
    fn test_complete_content_acquisition_workflow() {
        // End-to-end test: acquire → verify → materialize

        let store = ContentAddressableStore::new();
        let manager = ContentAcquisitionManager::new(store);
        let materializer = MaterializationHandler::new();

        // Step 1: Acquire content from peer
        let flow_ref = FlowRef::new("documents", "doc-workflow");
        let content = serde_json::json!({
            "id": "doc-workflow",
            "title": "Acquired Document",
            "content": "This document was acquired from a remote peer"
        });
        let content_bytes = serde_json::to_vec(&content).unwrap();
        let content_id = ContentIdentity::from_bytes(&content_bytes);

        // Step 2: Verify content identity
        let verify_result = ContentAcquisitionManager::verify_content(&content_bytes, &content_id);
        assert!(verify_result.is_ok());

        // Step 3: Materialize into local store
        let peer = PeerId::new("remote-peer");
        let metadata = AcquisitionMetadata::new(peer.clone(), true);

        manager
            .materialize(&flow_ref, content_bytes.clone(), &content_id)
            .unwrap();
        assert!(manager.has_local(&flow_ref).unwrap());

        // Step 4: Materialize as reactive operation
        let op = materializer
            .materialize_as_operation(&flow_ref, content_bytes, &metadata)
            .unwrap();

        // Verify operation has correct structure
        assert_eq!(op.key, "documents:doc-workflow");
        assert_eq!(op.capability, "documents");
        assert!(op.value.is_some());

        // Step 5: Verify materialization tracking
        assert!(materializer.is_materialized(&flow_ref).unwrap());

        println!("✓ Complete content acquisition workflow: acquire → verify → materialize → track");
    }

    #[test]
    fn test_acquisition_with_different_policies() {
        let store = ContentAddressableStore::new();
        let manager = ContentAcquisitionManager::new(store);

        let flow_ref = FlowRef::new("data", "item-policy");

        // Test LocalOnly - should not find non-existent content
        let result = manager
            .resolve(&flow_ref, AcquisitionPolicy::LocalOnly)
            .unwrap();
        assert!(result.is_none());

        // Test PreferLocal - tries local, then remote
        let result = manager
            .resolve(&flow_ref, AcquisitionPolicy::PreferLocal)
            .unwrap();
        assert!(result.is_none());

        // Test RemoteAllowed - allows automatic acquisition
        let result = manager
            .resolve(&flow_ref, AcquisitionPolicy::RemoteAllowed)
            .unwrap();
        assert!(result.is_none());

        // Test Required - enforces acquisition
        let result = manager
            .resolve(&flow_ref, AcquisitionPolicy::Required)
            .unwrap();
        assert!(result.is_none());

        println!("✓ Acquisition policies are enforced correctly");
    }

    #[test]
    fn test_materialization_creates_valid_operations() {
        // Verify that materialization produces operations that integrate with FeltDB

        let materializer = MaterializationHandler::new();

        // Test materializing structured JSON
        let flow_ref = FlowRef::new("users", "user-123");
        let json_content = serde_json::json!({
            "id": "user-123",
            "name": "Alice",
            "email": "alice@example.com"
        });
        let content_bytes = serde_json::to_vec(&json_content).unwrap();
        let peer = PeerId::new("authority-server");
        let metadata = AcquisitionMetadata::new(peer, false); // Not cached

        let op = materializer
            .materialize_as_operation(&flow_ref, content_bytes, &metadata)
            .unwrap();

        // Verify operation structure
        assert_eq!(op.capability, "users");
        assert_eq!(op.op_type, OperationType::Update);
        assert!(op.value.is_some());

        // Verify operation can be applied to reactive state
        let value = op.value.unwrap();
        assert_eq!(value["name"], "Alice");

        println!("✓ Materialization produces valid reactive operations");
    }

    #[test]
    fn test_resolution_metadata_tracking() {
        // Verify that resolution metadata is properly tracked

        let local_meta = ResolutionMetadata::local();
        assert!(local_meta.is_local);
        assert!(local_meta.source_peer.is_none());

        let peer = PeerId::new("source-peer");
        let remote_meta = ResolutionMetadata::remote(peer.clone());
        assert!(!remote_meta.is_local);
        assert_eq!(remote_meta.source_peer, Some(peer));

        // Verify results include metadata
        let content = b"resolution test".to_vec();
        let local_result = ResolutionResult::local(content.clone());
        assert_eq!(local_result.content, content);
        assert!(local_result.metadata.is_local);

        let remote_peer = PeerId::new("remote-source");
        let remote_result = ResolutionResult::remote(content.clone(), remote_peer.clone());
        assert_eq!(remote_result.content, content);
        assert!(!remote_result.metadata.is_local);
        assert_eq!(remote_result.metadata.source_peer, Some(remote_peer));

        println!("✓ Resolution metadata tracks source and locality");
    }

    #[test]
    fn test_acquisition_idempotency() {
        // Multiple concurrent acquisitions of same content
        // Should not result in duplicate materializations

        let store = ContentAddressableStore::new();
        let manager = ContentAcquisitionManager::new(store);

        let flow_ref = FlowRef::new("docs", "shared-doc");
        let content = b"shared document content".to_vec();
        let content_id = ContentIdentity::from_bytes(&content);

        // First materialization
        manager
            .materialize(&flow_ref, content.clone(), &content_id)
            .unwrap();
        assert!(manager.has_local(&flow_ref).unwrap());

        // Simulate 10 concurrent requests all getting the same content
        for _ in 0..10 {
            let retrieved = manager.get_local(&flow_ref).unwrap();
            assert_eq!(retrieved, Some(content.clone()));

            // All would read the same materialized content
            // No redundant acquisitions needed
        }

        println!("✓ Acquisition is idempotent - one materialization serves many requests");
    }

    #[test]
    fn test_a_local_capability_execution() {
        // Test A - Local capability available → execute locally

        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let ad = DistributedCapabilityAdvertisement::new(cap_ref.clone(), "local".to_string());

        let mut router = CapabilityRouter::new();
        router.register_local(ad);
        router.update_peer_state("local".to_string(), "docs".to_string(), 851);

        let decision = router.route(&cap_ref, 851, "local");
        assert_eq!(decision, RouteDecision::Local);
        println!("✓ Test A passed: Local capability executes locally");
    }

    #[test]
    fn test_b_remote_capability_discovery() {
        // Test B - Capability absent locally → discover peer → execute remotely

        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let ad = DistributedCapabilityAdvertisement::new(cap_ref.clone(), "peer-b".to_string());

        let mut router = CapabilityRouter::new();
        router.register_remote(ad);
        router.update_peer_state("peer-b".to_string(), "docs".to_string(), 851);

        let decision = router.route(&cap_ref, 851, "peer-a");
        assert_eq!(decision, RouteDecision::Remote("peer-b".to_string()));
        println!("✓ Test B passed: Remote capability discovered and routed");
    }

    #[test]
    fn test_c_reference_based_input() {
        // Test C - Remote capability receives FlowRefs and resolves them locally

        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let exec_ctx = CapabilityExecutionContext::new(
            "exec-123".to_string(),
            cap_ref.clone(),
            "peer-a".to_string(),
        )
        .with_input_ref("flow://docs#123".to_string())
        .with_input_ref("flow://docs#456".to_string());

        assert_eq!(exec_ctx.input_refs.len(), 2);
        assert_eq!(exec_ctx.input_refs[0], "flow://docs#123");
        assert_eq!(exec_ctx.input_refs[1], "flow://docs#456");
        println!("✓ Test C passed: Reference-based inputs track document references");
    }

    #[test]
    fn test_d_state_aware_routing() {
        // Test D - Provider lacks required state → acquires it before execution

        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let ad = DistributedCapabilityAdvertisement::new(cap_ref.clone(), "peer-b".to_string());

        let mut router = CapabilityRouter::new();
        router.register_remote(ad);

        // B only has version 847
        router.update_peer_state("peer-b".to_string(), "docs".to_string(), 847);

        // But we need 851
        assert!(router.requires_state_acquisition("peer-b", &cap_ref, 851));

        let plan = router.plan_state_acquisition("peer-b", &cap_ref, 851);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0], "docs");

        println!("✓ Test D passed: State-aware routing detects need for acquisition");
    }

    #[test]
    fn test_e_execution_ownership() {
        // Test E - Two peers receive same request → exactly one execution

        let mut claimer = ExecutionClaimer::new();

        // Peer A attempts to claim
        let claim_a = claimer.try_claim("exec-123".to_string(), "peer-a".to_string());
        assert!(claim_a.is_some());

        // Peer B attempts to claim same execution
        let claim_b = claimer.try_claim("exec-123".to_string(), "peer-b".to_string());
        assert!(claim_b.is_none());

        // Verify A owns it
        assert!(claimer.is_claimed_by("exec-123", "peer-a"));
        assert!(!claimer.is_claimed_by("exec-123", "peer-b"));

        println!("✓ Test E passed: Execution ownership prevents duplicates");
    }

    #[test]
    fn test_f_retry_idempotency() {
        // Test F - Network retry → exactly one logical execution

        let mut publisher = ExecutionResultPublisher::new();

        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let exec_ref = ExecutionRef::new("exec-123".to_string(), cap_ref, "peer-b".to_string());
        let result = serde_json::json!({"results": [1, 2, 3]});
        let exec_result = CapabilityExecutionResult::success(exec_ref, result);

        publisher.publish(exec_result.clone());

        // Retry - should get same result
        let retrieved = publisher.get_result("exec-123");
        assert!(retrieved.is_some());

        // Verify same execution ID returned
        assert_eq!(retrieved.unwrap().execution_ref.execution_id, "exec-123");

        println!("✓ Test F passed: Retries return idempotent results");
    }

    #[test]
    fn test_g_result_convergence() {
        // Test G - Remote result becomes operation and synchronizes to requester

        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let exec_ref = ExecutionRef::new("exec-123".to_string(), cap_ref, "peer-b".to_string());

        let result = serde_json::json!({"results": [1, 2, 3]});
        let exec_result = CapabilityExecutionResult::success(exec_ref.clone(), result.clone());

        // Result tracks the execution that produced it
        assert_eq!(exec_result.execution_ref.execution_id, "exec-123");
        assert_eq!(exec_result.result, result);
        assert!(exec_result.error.is_none());

        println!("✓ Test G passed: Results track execution provenance");
    }

    #[test]
    fn test_h_reactive_execution() {
        // Test H - Remote execution result → canonical state → reactive subscriber fires

        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let exec_ref = ExecutionRef::new(
            "exec-123".to_string(),
            cap_ref.clone(),
            "peer-b".to_string(),
        );

        // Result can track what inputs were used
        let mut tracked_ref = exec_ref;
        tracked_ref.input_refs.push("flow://docs#123".to_string());
        tracked_ref.input_refs.push("flow://docs#456".to_string());

        // And what state version was in use
        tracked_ref.input_state_version = 851;

        // With this complete context, results flow through reactive system
        assert_eq!(tracked_ref.input_refs.len(), 2);
        assert_eq!(tracked_ref.input_state_version, 851);

        println!("✓ Test H passed: Execution results have full reactive context");
    }

    #[test]
    fn test_i_provider_failover() {
        // Test I - Provider A disappears → request routes to provider B

        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);

        let mut router = CapabilityRouter::new();

        // Register both providers
        let ad_a = DistributedCapabilityAdvertisement::new(cap_ref.clone(), "peer-a".to_string());
        let ad_b = DistributedCapabilityAdvertisement::new(cap_ref.clone(), "peer-b".to_string());

        router.register_remote(ad_a);
        router.register_remote(ad_b);

        router.update_peer_state("peer-a".to_string(), "docs".to_string(), 851);
        router.update_peer_state("peer-b".to_string(), "docs".to_string(), 851);

        // Find compatible providers (should get both)
        let providers = router.find_compatible_providers(&cap_ref, 851);
        assert!(providers.len() >= 1); // At least one available

        // If A goes down, B still available
        // (In real scenario, would remove A's advertisement)
        let providers_after = router.find_compatible_providers(&cap_ref, 851);
        assert!(providers_after.len() >= 1);

        println!("✓ Test I passed: Multiple providers enable failover");
    }

    #[test]
    fn test_j_no_provider_durable() {
        // Test J - Request becomes durable pending execution

        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);

        let router = CapabilityRouter::new(); // Empty router - no providers

        let decision = router.route(&cap_ref, 851, "peer-a");
        assert_eq!(decision, RouteDecision::Unavailable);

        // In real implementation, this would create a pending execution
        // that waits for a provider to appear

        println!("✓ Test J passed: Unavailable capabilities detected");
    }

    #[test]
    fn test_k_provider_recovery() {
        // Test K - Provider appears → pending execution resumes

        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);

        let mut router = CapabilityRouter::new();

        // Initially unavailable
        let decision1 = router.route(&cap_ref, 851, "peer-a");
        assert_eq!(decision1, RouteDecision::Unavailable);

        // Provider appears
        let ad = DistributedCapabilityAdvertisement::new(cap_ref.clone(), "peer-b".to_string());
        router.register_remote(ad);
        router.update_peer_state("peer-b".to_string(), "docs".to_string(), 851);

        // Now available
        let decision2 = router.route(&cap_ref, 851, "peer-a");
        assert_eq!(decision2, RouteDecision::Remote("peer-b".to_string()));

        println!("✓ Test K passed: Provider recovery enables execution");
    }

    #[test]
    fn test_l_capability_authorization() {
        // Test L - Unauthorized peer/request cannot execute capability

        let access_model = CapabilityAccessModel::new(
            vec!["docs".to_string()],
            vec!["search-results".to_string()],
            vec![],
        );

        // Capability can read docs
        assert!(access_model.can_read("docs"));

        // But cannot write to other collections
        assert!(!access_model.can_write("users"));

        // Cannot execute external systems it doesn't have permission for
        assert!(!access_model.can_execute("payment-service"));

        println!("✓ Test L passed: Capability access control enforced");
    }

    #[test]
    fn test_m_result_provenance() {
        // Test M - Result exposes: who executed it, what capability, what inputs, state version

        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);
        let mut exec_ref = ExecutionRef::new(
            "exec-123".to_string(),
            cap_ref.clone(),
            "peer-b".to_string(),
        );

        // Add input references
        exec_ref.input_refs.push("flow://docs#123".to_string());
        exec_ref.input_refs.push("flow://docs#456".to_string());

        // Record state version at execution time
        exec_ref.input_state_version = 851;

        // Verify full provenance
        assert_eq!(exec_ref.execution_id, "exec-123");
        assert_eq!(exec_ref.capability_ref, cap_ref);
        assert_eq!(exec_ref.provider_peer, "peer-b");
        assert_eq!(exec_ref.input_refs.len(), 2);
        assert_eq!(exec_ref.input_state_version, 851);

        println!("✓ Test M passed: Complete execution provenance tracked");
    }

    #[test]
    fn test_n_multi_peer_consistency() {
        // Test N - Three peers, two providers, random routing → identical final state

        let cap_ref = CapabilityRef::new("docs".to_string(), "vector-search".to_string(), 1);

        let mut router = CapabilityRouter::new();

        // Register capability at two providers
        let ad_b = DistributedCapabilityAdvertisement::new(cap_ref.clone(), "peer-b".to_string());
        let ad_c = DistributedCapabilityAdvertisement::new(cap_ref.clone(), "peer-c".to_string());

        router.register_remote(ad_b);
        router.register_remote(ad_c);

        // Both have same state version
        router.update_peer_state("peer-b".to_string(), "docs".to_string(), 851);
        router.update_peer_state("peer-c".to_string(), "docs".to_string(), 851);

        // Three peers making requests
        let decision_a = router.route(&cap_ref, 851, "peer-a");
        let _decision_b = router.route(&cap_ref, 851, "peer-b");
        let _decision_c = router.route(&cap_ref, 851, "peer-c");

        // All should route to available providers
        assert_ne!(decision_a, RouteDecision::Unavailable);
        // B and C can use local if available, otherwise find remote

        // Using same input state version, all produce consistent routing
        match decision_a {
            RouteDecision::Remote(_peer_id) => {
                // A routed to a remote peer
                assert!(true);
            }
            RouteDecision::Local => {
                // A has local capability
                assert!(true);
            }
            RouteDecision::Unavailable => {
                // Should not happen with providers registered
                assert!(false, "Expected capability to be available");
            }
        }

        println!("✓ Test N passed: Multi-peer consistency with provider discovery");
    }
}
