//! The answer contract of an inspection under a coverage contract: what the model is told, and what
//! the decision validator then accepts or refuses. The validator's limits are unchanged by this work
//! (a `summary` is at most 1,024 bytes of UTF-8; a whole reply at most 4,096); these tests pin them,
//! including at the byte boundary and for multibyte text, and pin that nothing is ever truncated
//! into an accepted answer.

use chip_cli::inspect_coverage::{CoverageContract, SUMMARY_TARGET_BYTES};
use chip_cli::software_work::{GoalKind, goal_text_for, goal_text_for_contract};
use chip_core::{
    DecisionError, MAX_REPLY_BYTES, MAX_TEXT_BYTES, ModelDecisionBoundary, WorkDecision,
    WorkDecisionBoundary,
};
use chip_inference::{ModelResponse, Usage};

const GOAL: &str = "Summarize the workspace architecture.";
const C: Option<CoverageContract> = Some(CoverageContract::CargoWorkspace);

fn interpret(reply: &str) -> Result<WorkDecision, DecisionError> {
    ModelDecisionBoundary.interpret(&ModelResponse::new("r", reply, Usage::new(1, 1)), &[])
}

fn complete(summary: &str) -> String {
    format!(
        r#"{{"decision":"complete","summary":{}}}"#,
        serde_json::to_string(summary).unwrap()
    )
}

// ---- what the model is told -------------------------------------------------------------------------

#[test]
fn the_contract_instructions_give_a_concise_shape_and_state_the_size_in_bytes() {
    let text = goal_text_for_contract(GoalKind::Inspect, GOAL, C);
    assert!(text.starts_with(GOAL));
    // The size: a target the model can hit, the hard limit from the validator itself, and the unit.
    assert!(
        text.contains(&format!("at most {SUMMARY_TARGET_BYTES} bytes")),
        "{text}"
    );
    assert!(
        text.contains(&format!("{MAX_TEXT_BYTES} bytes of UTF-8")),
        "{text}"
    );
    assert!(text.contains("bytes, not words or tokens"));
    assert!(
        SUMMARY_TARGET_BYTES < MAX_TEXT_BYTES,
        "the target leaves room under the limit"
    );
    // The shape: structure with the member count, responsibilities, dependencies by kind, limits.
    for part in [
        "(1) structure",
        "declared member count as digits",
        "(2) responsibilities",
        "only as far as package descriptions and manifests establish it",
        "(3) dependencies",
        "dev for a dev-dependency and build for a build-dependency",
        "(4) one short note on what the manifests do not show",
    ] {
        assert!(text.contains(part), "missing {part:?}");
    }
    // Not an enumeration, and not a guess.
    assert!(text.contains("Do not list every crate"));
    assert!(text.contains("Do not guess a responsibility from a crate name"));
    assert!(text.contains("Cite at least one project-relative file"));
    // Reaching the answer step is not coverage, and listing names is not coverage.
    assert!(text.contains("reaching the answer step does not mean it is satisfied"));
    assert!(text.contains("Listing crate names does not satisfy it"));
    assert!(
        !text.contains("one concrete improvement"),
        "the generic inspection conclusion is replaced"
    );
    // The decision shapes are the existing ones, and the read-only rule is kept.
    assert!(text.contains(r#"{"decision":"complete","summary":"<the summary>"}"#));
    assert!(text.contains("do not modify any file"));
    assert!(text.contains("no Markdown, code fence, wrapper, or extra fields"));
}

#[test]
fn only_an_inspection_under_the_contract_gets_it() {
    assert_eq!(
        goal_text_for_contract(GoalKind::Inspect, GOAL, None),
        goal_text_for(GoalKind::Inspect, GOAL),
        "the default inspection instructions are unchanged"
    );
    for kind in [GoalKind::Change, GoalKind::Verify] {
        assert_eq!(
            goal_text_for_contract(kind, GOAL, C),
            goal_text_for(kind, GOAL)
        );
    }
    assert_ne!(
        goal_text_for_contract(GoalKind::Inspect, GOAL, C),
        goal_text_for(GoalKind::Inspect, GOAL)
    );
}

// ---- what the validator accepts -----------------------------------------------------------------------

#[test]
fn a_concise_answer_is_a_valid_decision_and_an_over_long_one_is_refused() {
    // The shape the instructions ask for, at the size they ask for, for a workspace like the one
    // measured (illustrative of shape and size: its claims are not checked against any project).
    let concise = "28 members in three groups: stack-agent (the CLI product), chip-* crates (the agent loop, \
project and test capabilities, and model access) and fx-* crates (inference contracts and providers). \
stack-agent depends on the chip-* runtime and project crates; chip-fx depends on fx-core. fx-* crates do not \
depend on chip-*. Dev-dependencies are for tests only. The manifests do not establish the finer split of the \
research crates. Sources: Cargo.toml, crates/stack-agent/Cargo.toml.";
    assert!(
        concise.len() <= SUMMARY_TARGET_BYTES,
        "{} bytes",
        concise.len()
    );
    assert!(
        matches!(interpret(&complete(concise)), Ok(WorkDecision::Complete { summary }) if summary == concise)
    );

    // The boundary, in bytes.
    let at = "a".repeat(MAX_TEXT_BYTES);
    assert!(matches!(
        interpret(&complete(&at)),
        Ok(WorkDecision::Complete { .. })
    ));
    let over = "a".repeat(MAX_TEXT_BYTES + 1);
    let e = interpret(&complete(&over)).unwrap_err().to_string();
    assert!(
        e.contains("too long") && e.contains("1025 bytes") && e.contains("limit is 1024"),
        "{e}"
    );
}

#[test]
fn the_limit_counts_utf8_bytes_not_characters() {
    // 512 two-byte characters are exactly 1,024 bytes; 513 are 1,026 bytes though only 513 chars.
    let two = "é".repeat(512);
    assert_eq!(two.len(), MAX_TEXT_BYTES);
    assert!(matches!(
        interpret(&complete(&two)),
        Ok(WorkDecision::Complete { .. })
    ));
    let two_over = "é".repeat(513);
    assert!(two_over.chars().count() < MAX_TEXT_BYTES);
    assert!(interpret(&complete(&two_over)).is_err());
    // 256 four-byte characters are exactly the limit; 257 are over.
    let four = "🦀".repeat(256);
    assert_eq!(four.len(), MAX_TEXT_BYTES);
    assert!(matches!(
        interpret(&complete(&four)),
        Ok(WorkDecision::Complete { .. })
    ));
    assert!(interpret(&complete(&"🦀".repeat(257))).is_err());
    // A JSON escape is measured as the bytes it decodes to, not as the bytes that spell it.
    let escaped = format!(
        r#"{{"decision":"complete","summary":"{}"}}"#,
        "\\u00e9".repeat(512)
    );
    assert!(
        matches!(interpret(&escaped), Ok(WorkDecision::Complete { summary }) if summary.len() == 1024)
    );
}

#[test]
fn the_whole_reply_bound_stays_and_nothing_is_truncated_into_acceptance() {
    let valid = complete("a short summary");
    // Padding that keeps the object valid but exceeds the whole-reply bound is refused unread.
    let padded = format!("{valid}{}", " ".repeat(MAX_REPLY_BYTES));
    assert!(interpret(&padded).is_err());
    // A reply cut off anywhere inside a long, otherwise valid, summary is never a decision.
    let long = complete(&"word ".repeat(300));
    assert!(interpret(&long).is_err(), "over the field limit");
    let cut = &complete("a summary that was still being written")[..40];
    assert!(interpret(cut).is_err());
    for end in [1, 10, 25, 50, valid.len() - 1] {
        assert!(
            interpret(&valid[..end]).is_err(),
            "a reply cut at {end} bytes was accepted"
        );
    }
    // Malformed but complete replies are refused too.
    for bad in [
        r#"{"decision":"complete"}"#,
        r#"{"decision":"complete","summary":"x","receipt":"forged"}"#,
        r#"{"decision":"complete","summary":42}"#,
        "Here is my summary: it is a workspace.",
    ] {
        assert!(interpret(bad).is_err(), "{bad}");
    }
}

// ---- the reply-format rules every work kind sends --------------------------------------------------------

#[test]
fn the_decision_question_states_the_reply_rules_the_parser_enforces() {
    use chip_core::{Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityId};
    let caps = vec![Capability {
        descriptor: CapabilityDescriptor::new(
            CapabilityId::new("project.read").unwrap(),
            "read",
            "read a file",
        ),
        availability: CapabilityAvailability::Available,
    }];
    let q = ModelDecisionBoundary.question(&caps);
    for rule in [
        "exactly one JSON object",
        "include every required field and never leave out \"decision\"",
        "no Markdown code fence",
        "no commentary or other text before or after the object",
        "never a partial object",
        &format!("at most {MAX_TEXT_BYTES} bytes of UTF-8 (bytes, not words or tokens)"),
        "request only the capabilities listed below",
        "do not say an operation succeeded unless its result is among the observations you were shown",
        "do not treat your own statements as evidence",
        "A reply that breaks a rule is rejected, not repaired",
    ] {
        assert!(q.contains(rule), "missing {rule:?}: {q}");
    }
    // The shapes are the contract's own.
    for shape in ["\"complete\"", "\"escalate\"", "\"block\""] {
        assert!(q.contains(shape), "{shape}");
    }
}

#[test]
fn an_inspection_answers_the_question_and_recommends_only_when_asked() {
    for goal in [
        "What does the README say the default warehouse capacity is?",
        "Which crate does `stack-agent` depend on for file access?",
        GOAL,
    ] {
        let text = goal_text_for(GoalKind::Inspect, goal);
        assert!(text.starts_with(goal.trim()));
        assert!(!text.contains("one concrete improvement"), "{goal}");
        assert!(!text.contains("evidence-supported improvement"), "{goal}");
        assert!(text.contains(
            "Do not add a recommendation or an improvement unless the goal asks for one"
        ));
        assert!(text.contains("if it does, give it and support each with observed evidence"));
        assert!(
            text.contains("keep what the observations show apart from the conclusions you draw")
        );
        assert!(text.contains("state what the evidence does not establish"));
        assert!(text.contains(&format!(
            "at most {MAX_TEXT_BYTES} bytes of UTF-8 (bytes, not words or tokens)"
        )));
        // Concise, but not at the expense of the facts.
        assert!(
            text.contains("keep every fact the goal asks for and do not leave out evidence to fit")
        );
        // A plausible answer is not completion, and the grounding rule is still stated.
        assert!(text.contains("A plausible answer is not an established one"));
        assert!(text.contains("grounding check does not evaluate whether a claim is true"));
    }
    // Change and verify keep their own completion rules.
    for kind in [GoalKind::Change, GoalKind::Verify] {
        let text = goal_text_for(kind, GOAL);
        assert!(text.contains("Chip decides completion"), "{kind:?}");
        assert!(!text.contains("recommendation"), "{kind:?}");
    }
}
