use tally::percent::clamp_percent;

#[test]
fn the_whole_range_behaves() {
    for (n, want) in [(0, 0), (1, 1), (40, 40), (99, 99), (100, 100), (101, 100), (150, 100), (u32::MAX, 100)] {
        assert_eq!(clamp_percent(n), want, "clamp_percent({n})");
    }
}
