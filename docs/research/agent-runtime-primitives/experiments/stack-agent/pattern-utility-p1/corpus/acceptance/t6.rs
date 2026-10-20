use chip_graph::BoundedLog;

#[test]
fn a_zero_capacity_is_refused() {
    assert!(BoundedLog::<u32>::new(0).is_none());
}

#[test]
fn it_keeps_the_newest_items_oldest_first_and_says_what_it_dropped() {
    let mut log = BoundedLog::new(2).unwrap();
    assert!(log.is_empty());
    assert_eq!(log.push(1), None);
    assert_eq!(log.push(2), None);
    assert_eq!(log.len(), 2);
    assert_eq!(log.push(3), Some(1));
    assert_eq!(log.push(4), Some(2));
    assert_eq!(log.iter().copied().collect::<Vec<_>>(), [3, 4]);
    assert_eq!((log.len(), log.capacity(), log.is_empty()), (2, 2, false));
}

#[test]
fn a_capacity_of_one_holds_only_the_latest() {
    let mut log = BoundedLog::new(1).unwrap();
    log.push("a");
    assert_eq!(log.push("b"), Some("a"));
    assert_eq!(log.iter().collect::<Vec<_>>(), [&"b"]);
}
