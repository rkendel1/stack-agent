use ledger_core::Amount;

fn split(c: i64, n: usize) -> Vec<i64> {
    Amount::from_cents(c).split(n).into_iter().map(Amount::cents).collect()
}

#[test]
fn first_parts_get_the_extra_cents() {
    assert_eq!(split(1000, 3), vec![334, 333, 333]);
    assert_eq!(split(1001, 3), vec![334, 334, 333]);
    assert_eq!(split(2, 3), vec![1, 1, 0]);
    assert_eq!(split(999, 3), vec![333, 333, 333]);
    assert_eq!(split(7, 1), vec![7]);
}

#[test]
fn sums_are_exact() {
    for c in [0i64, 1, 99, 100, 101, 12345] {
        for n in 1..9 {
            assert_eq!(split(c, n).iter().sum::<i64>(), c);
        }
    }
}
