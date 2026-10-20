//! A deterministic replay of recorded trajectories through the stuck guard.
//!
//! Each trajectory is a list of pairs (`"capability|inputs=>output"`, a leading `!` on the output
//! marking a failed execution) with a class a person assigned and what the guard should do:
//!
//! * `productive`: the work is moving; the guard must not fire.
//! * `repetitive_progress`: it repeats, and is still making progress; the guard must not fire, except
//!   where `known_limitation` records that it does and why that is not appropriate.
//! * `stuck`: it is going nowhere; the guard must fire, and when.
//!
//! The guard is asked after every pair, as the work loop asks it after every observation, and the
//! first answer is the trigger. The corpus in `fixtures/stuck_replay/synthetic.json` is **synthetic**:
//! it was written to probe the thresholds, not recorded from a model. Trajectories recorded with
//! their inputs and observations can be replayed through the same code, in the same format:
//!
//! ```text
//! STUCK_REPLAY_FILE=path/to/trajectories.json cargo test -p chip-core --test stuck_guard_replay -- --nocapture
//! ```
//!
//! The summary is printed with `--nocapture`.

use chip_core::{
    CapabilityId, ExecutionId, ExecutionStatus, Observation, ObservationKind, ObservationOrigin,
    StuckPattern, StuckThresholds, detect_stuck,
};
use serde_json::Value;

struct Replayed {
    id: String,
    class: String,
    pairs: usize,
    fired: Option<(&'static str, usize)>,
    expected: Option<(String, usize)>,
    known_limitation: bool,
}

fn parse_pair(n: usize, pair: &str) -> (ObservationOrigin, Observation) {
    let (invocation, output) = pair
        .split_once("=>")
        .expect("a pair is `invocation=>output`");
    let (failed, output) = match output.strip_prefix('!') {
        Some(o) => (true, o),
        None => (false, output),
    };
    let capability = invocation.split('|').next().unwrap();
    (
        ObservationOrigin {
            capability: CapabilityId::new(capability).unwrap(),
            invocation: invocation.to_string(),
            reusable: false,
            provider_response_id: None,
        },
        Observation {
            execution_id: ExecutionId::new(format!("w-exec-{n}")),
            kind: if failed {
                ObservationKind::ExecutionFailed
            } else {
                ObservationKind::ExecutionCompleted
            },
            status: if failed {
                ExecutionStatus::Failure
            } else {
                ExecutionStatus::Success
            },
            output: Some(output.to_string()),
            receipt_id: Some(format!("sha256:{n}")),
            evidence: None,
        },
    )
}

fn replay(corpus: &Value, thresholds: &StuckThresholds) -> Vec<Replayed> {
    corpus
        .as_array()
        .expect("a list of trajectories")
        .iter()
        .map(|t| {
            let pairs: Vec<(ObservationOrigin, Observation)> = t["pairs"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(n, p)| parse_pair(n, p.as_str().unwrap()))
                .collect();
            let (origins, observations): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
            let mut fired = None;
            for k in 1..=origins.len() {
                if let Some(p) = detect_stuck(&origins[..k], &observations[..k], thresholds) {
                    fired = Some((
                        match p {
                            StuckPattern::Repeated { .. } => "repeated",
                            StuckPattern::Alternating { .. } => "alternating",
                        },
                        k,
                    ));
                    break;
                }
            }
            let expect = &t["expect"];
            Replayed {
                id: t["id"].as_str().unwrap().to_string(),
                class: t["class"].as_str().unwrap().to_string(),
                pairs: origins.len(),
                fired,
                expected: expect["pattern"].as_str().map(|p| {
                    (
                        p.to_string(),
                        expect["after_pairs"].as_u64().unwrap() as usize,
                    )
                }),
                known_limitation: t["known_limitation"].as_bool().unwrap_or(false),
            }
        })
        .collect()
}

/// Whether what the guard did suits the trajectory's class. `None`: the class says nothing.
fn appropriate(r: &Replayed) -> Option<bool> {
    match r.class.as_str() {
        "stuck" => Some(r.fired.is_some()),
        "productive" | "repetitive_progress" => Some(r.fired.is_none()),
        _ => None,
    }
}

fn corpus() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/stuck_replay/synthetic.json"
    );
    let mut all: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    if let Ok(extra) = std::env::var("STUCK_REPLAY_FILE") {
        let more: Vec<Value> =
            serde_json::from_str(&std::fs::read_to_string(extra).unwrap()).unwrap();
        all.extend(more);
    }
    Value::Array(all)
}

#[test]
fn the_guard_does_what_the_corpus_says_and_the_summary_is_honest_about_where_it_is_wrong() {
    let thresholds = StuckThresholds::default();
    let results = replay(&corpus(), &thresholds);
    assert!(results.len() >= 25);

    // The guard does exactly what each trajectory records it should.
    for r in &results {
        let got = r.fired.map(|(p, k)| (p.to_string(), k));
        assert_eq!(got, r.expected, "{} ({})", r.id, r.class);
        // Where the guard's action does not suit the class, the trajectory must say so.
        if appropriate(r) == Some(false) {
            assert!(
                r.known_limitation,
                "{}: an inappropriate result is recorded as a limitation",
                r.id
            );
        } else {
            assert!(
                !r.known_limitation,
                "{}: marked a limitation but the result is appropriate",
                r.id
            );
        }
    }

    // The summary.
    println!(
        "\n{:<10} {:>4} {:>8} {:>13} {:>18}",
        "class", "n", "fired", "appropriate", "inappropriate"
    );
    for class in ["productive", "repetitive_progress", "stuck"] {
        let rows: Vec<&Replayed> = results.iter().filter(|r| r.class == class).collect();
        let fired = rows.iter().filter(|r| r.fired.is_some()).count();
        let ok = rows.iter().filter(|r| appropriate(r) == Some(true)).count();
        println!(
            "{class:<10} {:>4} {fired:>8} {ok:>13} {:>18}",
            rows.len(),
            rows.len() - ok
        );
    }
    println!("\ninappropriate results (known limitations):");
    for r in results.iter().filter(|r| appropriate(r) == Some(false)) {
        println!(
            "  {} fired {:?} after {} pairs of {}",
            r.id,
            r.fired.unwrap().0,
            r.fired.unwrap().1,
            r.pairs
        );
    }

    // The two synthetic classes that must never fire do not, except the recorded limitations.
    let wrongly: Vec<&str> = results
        .iter()
        .filter(|r| r.class != "stuck" && r.fired.is_some() && !r.known_limitation)
        .map(|r| r.id.as_str())
        .collect();
    assert!(wrongly.is_empty(), "{wrongly:?}");
}
