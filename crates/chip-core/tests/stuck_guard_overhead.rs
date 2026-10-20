//! The cost of one stuck-guard evaluation against the length of the trajectory.
//!
//! Not part of the ordinary run (it measures time): `cargo test --release -p chip-core --test
//! stuck_guard_overhead -- --ignored --nocapture`. The assertion that always runs is the bound: an
//! evaluation reads at most `StuckThresholds::window()` pairs, however long the trajectory is.

use std::time::Instant;

use chip_core::{
    CapabilityId, ExecutionId, ExecutionStatus, Observation, ObservationKind, ObservationOrigin,
    StuckThresholds, detect_stuck,
};

fn trajectory(len: usize) -> (Vec<ObservationOrigin>, Vec<Observation>) {
    // The worst case for the scan: the end is one pair short of every pattern, so each comparison
    // is made. Pairs before it all differ.
    (0..len)
        .map(|i| {
            let k = if i + 3 >= len { i % 2 } else { i + 10 };
            (
                ObservationOrigin {
                    capability: CapabilityId::new("world.read").unwrap(),
                    invocation: format!("world.read|path={k}"),
                    reusable: false,
                    provider_response_id: None,
                },
                Observation {
                    execution_id: ExecutionId::new(format!("w-exec-{i}")),
                    kind: ObservationKind::ExecutionCompleted,
                    status: ExecutionStatus::Success,
                    output: Some(format!("output of {k} {}", "x".repeat(200))),
                    receipt_id: Some(format!("sha256:{i}")),
                    evidence: None,
                },
            )
        })
        .unzip()
}

#[test]
fn an_evaluation_reads_a_fixed_number_of_pairs() {
    let t = StuckThresholds::default();
    assert_eq!(t.window(), 4);
    // Wider thresholds read more, still independent of the trajectory.
    assert_eq!(
        StuckThresholds {
            repeated: 4,
            alternating: 6
        }
        .window(),
        6
    );
    for len in [0, 3, 4, 1_000] {
        let (o, b) = trajectory(len);
        assert_eq!(detect_stuck(&o, &b, &t), None, "{len}");
    }
}

#[test]
#[ignore = "measures time"]
fn measure_one_evaluation_against_trajectory_length() {
    let t = StuckThresholds::default();
    println!("pairs examined per evaluation: at most {}", t.window());
    println!("{:>10} {:>14}", "trajectory", "ns/evaluation");
    for len in [4usize, 8, 100, 1_000, 10_000, 100_000] {
        let (o, b) = trajectory(len);
        let iterations = 200_000u32;
        let started = Instant::now();
        let mut found = 0;
        for _ in 0..iterations {
            found += usize::from(std::hint::black_box(detect_stuck(&o, &b, &t)).is_some());
        }
        let ns = started.elapsed().as_nanos() as f64 / f64::from(iterations);
        println!("{len:>10} {ns:>14.1}");
        assert_eq!(found, 0);
    }
}
