//! The state machine over a real Cargo project, the real acceptance command and a real FeltDB
//! checkpoint store. Only the replies are scripted ([`ScriptedFx`]); every safety check, the edit,
//! the test run and the checkpoints are the real ones. Success is checked independently by running
//! the project's tests again after the loop.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use async_trait::async_trait;
use chip_symbol_loop::acceptance::RequiredEvidence;
use chip_symbol_loop::contract::{ClassifyPacket, TaskPacket};
use chip_symbol_loop::fx::{Fx, FxFailure, FxReply};
use chip_symbol_loop::machine::{
    Budgets, LoopConfig, Phase, TASK_SCHEMA, TaskDefinition, TerminationReason,
};
use chip_symbol_loop::store::LoopStore;
use chip_symbol_loop::symbols::sha256;

const ORIGINAL: &str = "/// The most times a failed call is retried.\npub fn max_retries() -> u32 {\n    3\n}\n\npub fn backoff_ms(attempt: u32) -> u64 {\n    100 * u64::from(attempt)\n}\n";
const TARGET: &str = "symbol:src/lib.rs:max_retries";

/// Replies in order, recorded packets; an `Err` is a provider failure.
struct ScriptedFx {
    replies: Mutex<VecDeque<Result<String, String>>>,
    packets: Mutex<Vec<TaskPacket>>,
    classified: Mutex<Vec<ClassifyPacket>>,
}

impl ScriptedFx {
    fn new(replies: Vec<Result<&str, &str>>) -> Self {
        Self {
            replies: Mutex::new(
                replies
                    .into_iter()
                    .map(|r| r.map(str::to_string).map_err(str::to_string))
                    .collect(),
            ),
            packets: Mutex::new(Vec::new()),
            classified: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> usize {
        self.packets.lock().unwrap().len() + self.classified.lock().unwrap().len()
    }
    fn next(&self) -> Result<FxReply, FxFailure> {
        match self.replies.lock().unwrap().pop_front() {
            Some(Ok(text)) => Ok(FxReply {
                text,
                input_tokens: Some(100),
                output_tokens: Some(10),
            }),
            Some(Err(message)) => Err(FxFailure { message }),
            None => Err(FxFailure {
                message: "the script ran out of replies".into(),
            }),
        }
    }
}

#[async_trait]
impl Fx for ScriptedFx {
    async fn decide(&self, packet: &TaskPacket, _bytes: &[u8]) -> Result<FxReply, FxFailure> {
        self.packets.lock().unwrap().push(packet.clone());
        self.next()
    }
    async fn classify(&self, packet: &ClassifyPacket) -> Result<FxReply, FxFailure> {
        self.classified.lock().unwrap().push(packet.clone());
        self.next()
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: LoopStore,
}

fn fixture(tag: &str, lib: &str, test: &str) -> Fixture {
    let dir = tempfile::Builder::new()
        .prefix(&format!("chip-symbol-loop-{tag}-"))
        .tempdir()
        .unwrap();
    let root = dir.path().join("worktree");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"retry_policy\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "target\nCargo.lock\n").unwrap();
    fs::write(root.join("src/lib.rs"), lib).unwrap();
    fs::write(root.join("tests/policy.rs"), test).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "-A"]);
    git(
        &root,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "-m",
            "base",
        ],
    );
    let store = LoopStore::open(dir.path().join("checkpoints.feltdb"), "test").unwrap();
    Fixture {
        _dir: dir,
        root,
        store,
    }
}

fn retry_fixture(tag: &str) -> Fixture {
    fixture(
        tag,
        ORIGINAL,
        "#[test]\nfn allows_five_retries() {\n    assert_eq!(retry_policy::max_retries(), 5);\n}\n",
    )
}

fn task() -> TaskDefinition {
    TaskDefinition {
        schema: TASK_SCHEMA.into(),
        task_id: "retry-five".into(),
        goal: "Make the retry policy allow five retries without editing the tests.".into(),
        acceptance: vec!["cargo".into(), "test".into(), "--offline".into()],
        evidence: RequiredEvidence::CargoTestPassed { min_passed: 1 },
        signature_frozen: true,
    }
}

fn config(run: &str) -> LoopConfig {
    LoopConfig {
        experiment_id: "test".into(),
        run_id: run.into(),
        budgets: Budgets::default(),
        halt_after_transitions: None,
    }
}

fn replace(body: &str) -> String {
    serde_json::json!({"op": "replace_symbol_body", "symbol": TARGET, "body": body}).to_string()
}

fn lib(f: &Fixture) -> String {
    fs::read_to_string(f.root.join("src/lib.rs")).unwrap()
}

/// The independent check: the project's own tests, run again outside the loop, and the diff.
fn independently_passes(f: &Fixture) -> bool {
    Command::new("cargo")
        .args(["test", "--offline", "--quiet"])
        .current_dir(&f.root)
        .output()
        .unwrap()
        .status
        .success()
}

fn changed_files(f: &Fixture) -> Vec<String> {
    let out = Command::new("git")
        .args(["diff", "--name-only"])
        .current_dir(&f.root)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_passing_acceptance_ends_without_any_inference() {
    let f = fixture(
        "passing",
        ORIGINAL,
        "#[test]\nfn allows_three() {\n    assert_eq!(retry_policy::max_retries(), 3);\n}\n",
    );
    let fx = ScriptedFx::new(vec![]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("pass"))
        .await
        .unwrap();
    assert_eq!(r.termination.unwrap().reason, TerminationReason::Success);
    assert_eq!(fx.calls(), 0);
    assert_eq!(r.decisions, 0);
    assert!(r.patch_applied.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_valid_body_is_applied_verified_and_independently_confirmed() {
    let f = retry_fixture("repair");
    let fx = ScriptedFx::new(vec![Ok(&replace("{\n    5\n}"))]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("repair"))
        .await
        .unwrap();
    let t = r.termination.clone().unwrap();
    assert_eq!(
        (t.reason, t.class.as_str()),
        (TerminationReason::Success, "pass"),
        "{t:?}"
    );
    assert_eq!((r.decisions, r.valid_bodies, r.acceptance_runs), (1, 1, 2));
    // The packet: the target Chip resolved, the observed failure, neighbor signatures without bodies.
    let packet = fx.packets.lock().unwrap()[0].clone();
    assert_eq!(packet.symbol.id, TARGET);
    assert!(
        packet
            .acceptance
            .failure_excerpt
            .contains("allows_five_retries"),
        "{}",
        packet.acceptance.failure_excerpt
    );
    assert!(
        packet
            .neighbors
            .iter()
            .any(|n| n.id == "symbol:src/lib.rs:backoff_ms" && !n.signature.contains('{'))
    );
    // Independently: the tests pass, and the only change is the authorized body.
    assert!(independently_passes(&f));
    assert_eq!(changed_files(&f), vec!["src/lib.rs".to_string()]);
    assert_eq!(lib(&f), ORIGINAL.replace("{\n    3\n}", "{\n    5\n}"));
    let applied = r.patch_applied.unwrap();
    assert_eq!(applied.after_sha, sha256(lib(&f).as_bytes()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_body_is_rejected_reverted_and_shown_in_the_next_packet() {
    let f = retry_fixture("rejected");
    let fx = ScriptedFx::new(vec![Ok(&replace("{ 4 }")), Ok(&replace("{ 5 }"))]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("rejected"))
        .await
        .unwrap();
    assert_eq!(r.termination.unwrap().reason, TerminationReason::Success);
    let packets = fx.packets.lock().unwrap().clone();
    assert_eq!(packets.len(), 2);
    assert!(packets[0].rejected.is_empty());
    assert_eq!(packets[1].rejected.len(), 1);
    assert_eq!(packets[1].rejected[0].body, "{ 4 }");
    assert!(
        packets[1].rejected[0].error.contains("allows_five_retries"),
        "{}",
        packets[1].rejected[0].error
    );
    // The second packet was built from the restored source, not the rejected one.
    assert_eq!(packets[1].symbol.fingerprint, packets[0].symbol.fingerprint);
    assert_eq!(r.rejections[0].class, "test_still_fails");
    let events = f.store.events("rejected").unwrap();
    assert!(events.iter().any(|e| e.detail.get("rejected").is_some()));
    assert!(events.iter().any(|e| e.detail.get("reverted").is_some()));
}

#[tokio::test(flavor = "multi_thread")]
async fn two_rejected_bodies_stop_the_attempt_and_leave_the_source_restored() {
    let f = retry_fixture("two");
    let fx = ScriptedFx::new(vec![
        Ok(&replace("{ 4 }")),
        Ok(&replace("{ 6 }")),
        Ok(&replace("{ 5 }")),
    ]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("two"))
        .await
        .unwrap();
    let t = r.termination.unwrap();
    assert_eq!(
        (t.reason, t.class.as_str()),
        (TerminationReason::RepeatedRejection, "test_still_fails")
    );
    assert!(t.detail.contains("not widened"), "{}", t.detail);
    assert_eq!(
        fx.calls(),
        2,
        "no third decision after the attempt's two rejections"
    );
    assert_eq!(lib(&f), ORIGINAL);
    assert!(changed_files(&f).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn no_change_malformed_and_unauthorized_replies_are_rejected_with_their_class() {
    let f = retry_fixture("replies");
    let fx = ScriptedFx::new(vec![
        Ok(r#"{"op":"no_change","reason":"looks fine"}"#),
        Ok("I would change the 3 to a 5."),
    ]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("replies"))
        .await
        .unwrap();
    let classes: Vec<&str> = r.rejections.iter().map(|x| x.class.as_str()).collect();
    assert_eq!(classes, ["test_still_fails", "no_parse"]);
    assert_eq!(
        r.termination.unwrap().reason,
        TerminationReason::RepeatedRejection
    );
    assert_eq!(lib(&f), ORIGINAL);

    let f = retry_fixture("unauthorized");
    let other = serde_json::json!({"op": "replace_symbol_body", "symbol": "symbol:src/lib.rs:backoff_ms", "body": "{ 0 }"}).to_string();
    let unknown = r#"{"op":"need_symbol","symbol":"symbol:src/secret.rs:key"}"#;
    let fx = ScriptedFx::new(vec![Ok(&other), Ok(unknown)]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("unauthorized"))
        .await
        .unwrap();
    let classes: Vec<&str> = r.rejections.iter().map(|x| x.class.as_str()).collect();
    assert_eq!(classes, ["bad_symbol", "bad_symbol"]);
    assert_eq!(lib(&f), ORIGINAL);
}

#[tokio::test(flavor = "multi_thread")]
async fn need_symbol_adds_the_neighbor_source_to_the_next_packet() {
    let f = retry_fixture("context");
    let need = r#"{"op":"need_symbol","symbol":"symbol:src/lib.rs:backoff_ms"}"#;
    let fx = ScriptedFx::new(vec![Ok(need), Ok(&replace("{ 5 }"))]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("context"))
        .await
        .unwrap();
    assert_eq!(r.termination.unwrap().reason, TerminationReason::Success);
    let packets = fx.packets.lock().unwrap().clone();
    assert!(packets[0].context.is_empty());
    assert_eq!(packets[1].context.len(), 1);
    assert!(
        packets[1].context[0]
            .source
            .contains("100 * u64::from(attempt)")
    );
    assert_eq!(
        packets[1].constraints.context_requests_left,
        packets[0].constraints.context_requests_left - 1
    );
    assert_eq!(r.decisions, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn budgets_end_the_run_deterministically() {
    let f = retry_fixture("time");
    let fx = ScriptedFx::new(vec![]);
    let mut cfg = config("time");
    cfg.budgets.max_elapsed_ms = 0;
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &cfg)
        .await
        .unwrap();
    let t = r.termination.unwrap();
    assert_eq!(
        (t.reason, t.class.as_str()),
        (TerminationReason::BudgetExceeded, "budget_exceeded")
    );
    assert_eq!(fx.calls(), 0);

    let f = retry_fixture("decisions");
    let fx = ScriptedFx::new(vec![Ok("nope"), Ok("still nope"), Ok(&replace("{ 5 }"))]);
    let mut cfg = config("decisions");
    cfg.budgets.max_decisions = 2;
    cfg.budgets.max_rejected_per_attempt = 10;
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &cfg)
        .await
        .unwrap();
    let t = r.termination.unwrap();
    assert_eq!(t.reason, TerminationReason::BudgetExceeded, "{t:?}");
    assert_eq!(fx.calls(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn infrastructure_failures_are_not_model_failures() {
    let f = retry_fixture("provider");
    let fx = ScriptedFx::new(vec![Err("HTTP 503 from the local server")]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("provider"))
        .await
        .unwrap();
    let t = r.termination.unwrap();
    assert_eq!(
        (t.reason, t.class.as_str()),
        (
            TerminationReason::InfrastructureFailure,
            "infrastructure_failure"
        )
    );
    assert!(
        r.rejections.is_empty(),
        "a provider failure is not a rejected proposal"
    );

    let f = retry_fixture("command");
    let mut missing = task();
    missing.acceptance = vec!["/nonexistent/acceptance-runner".into()];
    let fx = ScriptedFx::new(vec![]);
    let r = chip_symbol_loop::run(&missing, &f.root, &f.store, &fx, &config("command"))
        .await
        .unwrap();
    assert_eq!(
        r.termination.unwrap().reason,
        TerminationReason::InfrastructureFailure
    );
    assert_eq!(fx.calls(), 0);

    let f = retry_fixture("dirty");
    fs::write(
        f.root.join("src/lib.rs"),
        "pub fn max_retries() -> u32 { 5 }\n",
    )
    .unwrap();
    let fx = ScriptedFx::new(vec![]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("dirty"))
        .await
        .unwrap();
    let t = r.termination.unwrap();
    assert_eq!(t.reason, TerminationReason::InfrastructureFailure);
    assert!(t.detail.contains("not clean"), "{}", t.detail);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_resumes_from_the_checkpoint_and_writes_the_patch_once() {
    let f = retry_fixture("resume");
    let body = replace("{ 5 }");
    // Advance one committed transition per process until the patch intent is committed, as a
    // crash right after each checkpoint would leave it.
    let mut halts = 0;
    loop {
        let fx = ScriptedFx::new(vec![Ok(&body)]);
        let mut cfg = config("resume");
        cfg.halt_after_transitions = Some(1);
        let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &cfg)
            .await
            .unwrap();
        assert!(!r.finished, "{r:?}");
        halts += 1;
        let (_, state) = f.store.load("resume").unwrap().unwrap();
        if state.phase == Phase::DecisionInFlight {
            panic!("halting after the inference intent ends the run; this test stops before it");
        }
        if state.phase == Phase::Decide {
            break;
        }
        assert!(halts < 10);
    }
    // One process makes the decision and commits the patch intent, then "crashes".
    let fx = ScriptedFx::new(vec![Ok(&body)]);
    let mut cfg = config("resume");
    cfg.halt_after_transitions = Some(2);
    chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &cfg)
        .await
        .unwrap();
    let (_, state) = f.store.load("resume").unwrap().unwrap();
    assert_eq!(state.phase, Phase::Apply);
    let patch = state.pending_patch.clone().unwrap();
    assert_eq!(
        lib(&f),
        ORIGINAL,
        "the intent is committed before the write"
    );
    // The crash happened after the file was written but before the commit.
    fs::write(f.root.join("src/lib.rs"), &patch.after_text).unwrap();
    // Resume: no new inference, the write is recognized as done, acceptance decides.
    let fx = ScriptedFx::new(vec![]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("resume"))
        .await
        .unwrap();
    assert_eq!(r.termination.unwrap().reason, TerminationReason::Success);
    assert_eq!(fx.calls(), 0);
    let events = f.store.events("resume").unwrap();
    let applied: Vec<_> = events
        .iter()
        .filter(|e| e.detail.get("applied").is_some())
        .collect();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].detail["already_applied"], true);
    assert!(independently_passes(&f));
    // Every transition has exactly one event, in sequence.
    assert!(
        events
            .iter()
            .enumerate()
            .all(|(i, e)| e.seq == i as u64 + 1)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_after_the_inference_intent_does_not_ask_again() {
    let f = retry_fixture("inflight");
    let body = replace("{ 5 }");
    loop {
        let fx = ScriptedFx::new(vec![Ok(&body)]);
        let mut cfg = config("inflight");
        cfg.halt_after_transitions = Some(1);
        let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &cfg)
            .await
            .unwrap();
        let (_, state) = f.store.load("inflight").unwrap().unwrap();
        if state.phase == Phase::DecisionInFlight {
            assert_eq!(fx.calls(), 0);
            break;
        }
        assert!(!r.finished);
    }
    let fx = ScriptedFx::new(vec![Ok(&body)]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("inflight"))
        .await
        .unwrap();
    let t = r.termination.unwrap();
    assert_eq!(t.reason, TerminationReason::InferenceOutcomeUnknown);
    assert_eq!(fx.calls(), 0);
    assert_eq!(lib(&f), ORIGINAL);
    // A finished run, resumed again, does nothing at all.
    let before = f.store.events("inflight").unwrap().len();
    let fx = ScriptedFx::new(vec![Ok(&body)]);
    let again = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("inflight"))
        .await
        .unwrap();
    assert_eq!(
        again.termination.unwrap().reason,
        TerminationReason::InferenceOutcomeUnknown
    );
    assert_eq!(f.store.events("inflight").unwrap().len(), before);
    assert_eq!(fx.calls(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn replaying_a_committed_transition_changes_nothing() {
    let f = retry_fixture("replay");
    let fx = ScriptedFx::new(vec![Ok(&replace("{ 5 }"))]);
    chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("replay"))
        .await
        .unwrap();
    let events = f.store.events("replay").unwrap();
    let (version, state) = f.store.load("replay").unwrap().unwrap();
    let last = events.last().unwrap().clone();
    // The same final commit again: recognized as already committed, nothing written.
    assert_eq!(
        f.store.commit(Some(version - 1), &state, &last).unwrap(),
        chip_symbol_loop::store::Commit::Replayed
    );
    assert_eq!(f.store.events("replay").unwrap(), events);
    assert_eq!(
        f.store.load("replay").unwrap().unwrap(),
        (version, state.clone())
    );
    // A different transition from a stale version is a conflict, not a replay.
    let mut forged = last.clone();
    forged.detail = serde_json::json!({"termination": "forged"});
    assert!(f.store.commit(Some(version - 1), &state, &forged).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ambiguous_target_gets_one_bounded_classification() {
    let lib =
        "pub fn max_retries() -> u32 {\n    3\n}\n\npub fn min_retries() -> u32 {\n    1\n}\n";
    let test = "#[test]\nfn bounds() {\n    assert_eq!(retry_policy::max_retries() + retry_policy::min_retries(), 6);\n}\n";
    let f = fixture("ambiguous", lib, test);
    let fx = ScriptedFx::new(vec![Ok(
        r#"{"op":"classify","symbol":"symbol:src/lib.rs:not_offered"}"#,
    )]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("ambiguous"))
        .await
        .unwrap();
    let t = r.termination.unwrap();
    assert_eq!(
        (t.reason, t.class.as_str()),
        (TerminationReason::InvalidDecision, "bad_symbol")
    );
    let offered: Vec<String> = fx.classified.lock().unwrap()[0]
        .candidates
        .iter()
        .map(|c| c.id.clone())
        .collect();
    assert_eq!(
        offered,
        [
            "symbol:src/lib.rs:max_retries",
            "symbol:src/lib.rs:min_retries"
        ]
    );

    let f = fixture("classified", lib, test);
    let fx = ScriptedFx::new(vec![
        Ok(r#"{"op":"classify","symbol":"symbol:src/lib.rs:max_retries"}"#),
        Ok(&replace("{ 5 }")),
    ]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("classified"))
        .await
        .unwrap();
    assert_eq!(r.termination.unwrap().reason, TerminationReason::Success);
    assert_eq!(
        r.decisions, 2,
        "the classification counts against the decision budget"
    );
    // The ranking that led to the classification is in the checkpoint, explained.
    let events = f.store.events("classified").unwrap();
    let ranked = events
        .iter()
        .find(|e| e.detail.get("classify").is_some())
        .unwrap();
    let top = ranked.detail["ranking"]["top"].as_array().unwrap();
    assert_eq!(top.len(), 2);
    assert!(
        top.iter()
            .all(|c| !c["reasons"].as_array().unwrap().is_empty())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failure_no_body_replacement_can_fix_stops_without_inference() {
    // The test calls `max_retries` with an argument its declaration does not take: the test does
    // not compile, and no change to any function body can make it.
    let f = fixture(
        "not-expressible",
        ORIGINAL,
        "#[test]\nfn allows_five_retries() {\n    assert_eq!(retry_policy::max_retries(5), 5);\n}\n",
    );
    let fx = ScriptedFx::new(vec![]);
    let r = chip_symbol_loop::run(&task(), &f.root, &f.store, &fx, &config("not-expressible"))
        .await
        .unwrap();
    let t = r.termination.unwrap();
    assert_eq!(
        (t.reason, t.class.as_str()),
        (TerminationReason::RepairNotExpressible, "not_expressible")
    );
    assert!(t.detail.contains("in test code"), "{}", t.detail);
    assert_eq!(fx.calls(), 0);
    assert_eq!(lib(&f), ORIGINAL);
}
