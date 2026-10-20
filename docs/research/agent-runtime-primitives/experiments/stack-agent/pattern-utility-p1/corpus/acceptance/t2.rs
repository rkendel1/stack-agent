use chip_core::{CompleteAfter, LocalWorkPolicy, WorkDecision, WorkGoal, WorkView};

fn view<'a>(goal: &'a WorkGoal, turn: usize) -> WorkView<'a> {
    WorkView {
        goal,
        turn,
        executions: 0,
        observations: &[],
        decisions: &[],
    }
}

#[test]
fn it_proposes_nothing_before_its_turn() {
    let goal = WorkGoal::new("g");
    let policy = CompleteAfter(2);
    assert!(policy.propose(&view(&goal, 0)).is_none());
    assert!(policy.propose(&view(&goal, 1)).is_none());
}

#[test]
fn it_completes_from_its_turn_on() {
    let goal = WorkGoal::new("g");
    let policy = CompleteAfter(2);
    for turn in [2, 3, 10] {
        assert!(matches!(
            policy.propose(&view(&goal, turn)),
            Some(WorkDecision::Complete { .. })
        ));
    }
}

#[test]
fn zero_completes_at_once() {
    let goal = WorkGoal::new("g");
    assert!(matches!(
        CompleteAfter(0).propose(&view(&goal, 0)),
        Some(WorkDecision::Complete { .. })
    ));
}
