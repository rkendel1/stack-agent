use ratelimit::{parse_policy, KeyedLimiter};

#[test]
fn keys_are_independent() {
    let mut k = KeyedLimiter::new(parse_policy("1/1s").unwrap());
    assert!(k.try_acquire("a", 0));
    assert!(!k.try_acquire("a", 0));
    assert!(k.try_acquire("b", 0));
    assert_eq!(k.keys(), 2);
}
