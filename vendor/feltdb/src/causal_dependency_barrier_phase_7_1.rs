/// Phase 7.1: Release Economics Testing
///
/// Direct measurement of frontier advancement costs without criterion harness
/// This provides rapid feedback on release behavior

#[cfg(test)]
mod phase_7_1_release_economics {
    use crate::causal_dependency_barrier::CausalDependencyBarrier;
    use crate::convergence::VectorClock;
    use std::time::Instant;

    #[test]
    fn phase_7_1_release_single_scan_small() {
        // Test 1: Single release scan with small pending buffer
        println!("\n=== Phase 7.1 Test 1: Single Release Scan (Small) ===");

        for pending_size in [0, 10, 100].iter() {
            let mut barrier = CausalDependencyBarrier::new();

            // Fill with blocked operations
            for i in 0..*pending_size {
                let mut required = VectorClock::new();
                required
                    .clocks
                    .insert("node_a".to_string(), (1000 + i) as u64);
                let _ = barrier.receive_operation(format!("pending_{}", i), required);
            }

            // Measure one frontier advance
            let start = Instant::now();
            let mut frontier = VectorClock::new();
            frontier.clocks.insert("node_a".to_string(), 1);
            barrier.advance_frontier(&frontier);
            let elapsed = start.elapsed();

            let metrics = barrier.metrics();
            println!(
                "Pending={:5} | Cost={:?} | Entries examined={} | Released={}",
                pending_size,
                elapsed,
                metrics.pending_entries_examined,
                metrics.operations_released
            );
        }
    }

    #[test]
    fn phase_7_1_release_selectivity_test() {
        // Test 2: Does readiness % affect cost?
        println!("\n=== Phase 7.1 Test 2: Release Selectivity ===");

        for ready_percent in [0, 10, 50, 100].iter() {
            let mut barrier = CausalDependencyBarrier::new();
            let pending_size = 100;

            let ready_count = (pending_size * ready_percent) / 100;
            let blocked_count = pending_size - ready_count;

            // Add ready operations
            for i in 0..ready_count {
                let mut required = VectorClock::new();
                required.clocks.insert("node_a".to_string(), 100);
                let _ = barrier.receive_operation(format!("ready_{}", i), required);
            }

            // Add blocked operations
            for i in 0..blocked_count {
                let mut required = VectorClock::new();
                required.clocks.insert("node_a".to_string(), 5000);
                let _ = barrier.receive_operation(format!("blocked_{}", i), required);
            }

            // Measure frontier advance to release ready operations
            let start = Instant::now();
            let mut frontier = VectorClock::new();
            frontier.clocks.insert("node_a".to_string(), 2000);
            barrier.advance_frontier(&frontier);
            let elapsed = start.elapsed();

            let metrics = barrier.metrics();
            let released_pct = if metrics.pending_entries_examined > 0 {
                (metrics.operations_released as f64 / metrics.pending_entries_examined as f64)
                    * 100.0
            } else {
                0.0
            };

            println!(
                "Ready%={:3} | Cost={:?} | Examined={:3} | Released={:3} | Ratio={:.1}%",
                ready_percent,
                elapsed,
                metrics.pending_entries_examined,
                metrics.operations_released,
                released_pct
            );
        }
    }

    #[test]
    fn phase_7_1_release_pathological_simplified() {
        // Test 3: Simplified pathological test
        // Creates sequential dependencies but only advances frontier a few times
        println!("\n=== Phase 7.1 Test 3: Pathological (Simplified) ===");

        let pending_size = 100;
        let mut barrier = CausalDependencyBarrier::new();

        // Create sequential dependencies
        for i in 0..pending_size {
            let mut required = VectorClock::new();
            required.clocks.insert("node_a".to_string(), (i + 1) as u64);
            let _ = barrier.receive_operation(format!("seq_{}", i), required);
        }

        println!(
            "Initial pending: {}",
            barrier.get_pending_operations().len()
        );

        // Advance frontier multiple times
        let start = Instant::now();
        let mut total_entries_examined = 0;
        let mut total_released = 0;

        for step in 1..=10 {
            barrier.clear_metrics();
            let mut frontier = VectorClock::new();
            frontier.clocks.insert("node_a".to_string(), step as u64);
            barrier.advance_frontier(&frontier);

            let metrics = barrier.metrics();
            total_entries_examined += metrics.pending_entries_examined;
            total_released += metrics.operations_released;

            println!(
                "Step {:2}: frontier A:{} | examined={:3} | released={} | remaining={:3}",
                step,
                step,
                metrics.pending_entries_examined,
                metrics.operations_released,
                barrier.get_pending_operations().len()
            );
        }

        let elapsed = start.elapsed();
        println!(
            "\nTotal across 10 steps: elapsed={:?} | total_examined={} | total_released={}",
            elapsed, total_entries_examined, total_released
        );

        // Analysis
        if total_entries_examined > 500 {
            println!(
                "⚠️  WARNING: Examined {} entries for 100 pending (10 advances)",
                total_entries_examined
            );
            println!("   Expected if O(m): ~100-200");
            println!("   This suggests possible rescanning behavior");
        } else {
            println!("✅ GOOD: Linear scaling confirmed");
            println!("   Examined {} entries total", total_entries_examined);
        }
    }

    #[test]
    fn phase_7_1_release_mixed_advances() {
        // Test 4: Realistic mixed advances
        println!("\n=== Phase 7.1 Test 4: Mixed Frontier Advances ===");

        for pending_size in [50, 100].iter() {
            println!("\nPending size: {}", pending_size);
            let mut barrier = CausalDependencyBarrier::new();

            // Create mixed dependencies
            for i in 0..*pending_size {
                let mut required = VectorClock::new();
                let dependency_level = ((i * 997) % 1000) as u64 + 1;
                required
                    .clocks
                    .insert("node_a".to_string(), dependency_level);
                let _ = barrier.receive_operation(format!("mixed_{}", i), required);
            }

            let start = Instant::now();
            let mut total_examined = 0;
            let mut total_released = 0;

            // Series of frontier advances
            for level in (100..=500).step_by(100) {
                barrier.clear_metrics();
                let mut frontier = VectorClock::new();
                frontier.clocks.insert("node_a".to_string(), level as u64);
                barrier.advance_frontier(&frontier);

                let metrics = barrier.metrics();
                total_examined += metrics.pending_entries_examined;
                total_released += metrics.operations_released;

                println!(
                    "  Advance to A:{} | examined={:2} | released={:2}",
                    level, metrics.pending_entries_examined, metrics.operations_released
                );
            }

            let elapsed = start.elapsed();
            println!(
                "Total: elapsed={:?} | examined={} | released={}",
                elapsed, total_examined, total_released
            );
        }
    }
}
