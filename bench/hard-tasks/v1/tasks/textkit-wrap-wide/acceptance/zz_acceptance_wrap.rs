use textkit::{char_width, display_width, wrap};

fn check(text: &str, width: usize) {
    let lines = wrap(text, width);
    for line in &lines {
        assert!(display_width(line) <= width || !line.contains(' '), "{line:?} wider than {width}");
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let back: Vec<&str> = lines.iter().flat_map(|l| l.split(' ')).collect();
    assert_eq!(words, back, "words lost or reordered");
}

#[test]
fn mixed_scripts_never_overflow() {
    for w in 2..20 {
        check("漢字 かな 한국어 abc 日本語 テキスト ｆｕｌｌ wide 全角 x", w);
        check("Ünïcödé 日本 e\u{301}e\u{301} 한 ab", w);
    }
}

#[test]
fn greedy_fill_is_unchanged() {
    assert_eq!(wrap("日本 語 ab", 7), vec!["日本 語", "ab"]);
    assert_eq!(wrap("the quick brown fox jumps", 10), vec!["the quick", "brown fox", "jumps"]);
}

#[test]
fn width_table_is_unchanged() {
    assert_eq!(char_width('漢'), 2);
    assert_eq!(char_width('ｆ'), 2);
    assert_eq!(char_width('a'), 1);
    assert_eq!(char_width('\u{301}'), 0);
    assert_eq!(char_width('한'), 2);
    assert_eq!(display_width("日本語abc"), 9);
}
