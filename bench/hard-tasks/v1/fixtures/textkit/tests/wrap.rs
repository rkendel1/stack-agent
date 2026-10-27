use textkit::{display_width, wrap};

#[test]
fn wraps_ascii() {
    assert_eq!(wrap("the quick brown fox jumps", 10), vec!["the quick", "brown fox", "jumps"]);
}

#[test]
fn long_words_get_their_own_line() {
    assert_eq!(wrap("a supercalifragilistic b", 5), vec!["a", "supercalifragilistic", "b"]);
}

#[test]
fn wide_text_respects_the_width() {
    for line in wrap("漢字 かな 한국어 abc 日本語 テキスト", 8) {
        assert!(display_width(&line) <= 8 || !line.contains(' '), "{line:?}");
    }
}

#[test]
#[ignore = "flaky on CI: depends on locale (see issue #41)"]
fn locale_sensitive_width() {
    assert_eq!(display_width("ä"), 1);
}
