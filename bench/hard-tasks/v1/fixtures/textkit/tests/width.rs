use textkit::{char_width, display_width};

#[test]
fn ascii_is_one_column() {
    assert_eq!(display_width("hello"), 5);
}

#[test]
fn cjk_is_two_columns() {
    assert_eq!(char_width('漢'), 2);
    assert_eq!(display_width("日本語"), 6);
    assert_eq!(char_width('한'), 2);
}

#[test]
fn combining_marks_are_zero() {
    assert_eq!(display_width("e\u{301}"), 1);
}
