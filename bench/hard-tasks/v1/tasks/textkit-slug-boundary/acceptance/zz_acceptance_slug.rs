use textkit::slugify;

#[test]
fn never_cuts_inside_a_word_or_ends_with_a_dash() {
    let text = "Crème brûlée is a dessert of rich custard topped with caramel";
    let full = slugify(text, 1_000);
    let first = full.split('-').next().unwrap();
    for max in 1..70 {
        let s = slugify(text, max);
        assert!(s.len() <= max, "{s:?} longer than {max}");
        assert!(!s.ends_with('-') && !s.starts_with('-'), "{s:?}");
        if max < first.len() {
            assert_eq!(s, &first[..max], "an over-long first word is cut to the limit");
        } else {
            assert!(s == full || full.starts_with(&format!("{s}-")), "{s:?} cuts a word of {full:?}");
            // ...and keeps every whole word that fits.
            let next = full[s.len()..].trim_start_matches('-').split('-').next().unwrap_or("");
            assert!(s == full || s.len() + 1 + next.len() > max, "{s:?} could keep {next:?} within {max}");
        }
    }
}

#[test]
fn whole_words_that_fit_are_kept() {
    assert_eq!(slugify("one two three four", 13), "one-two-three");
    assert_eq!(slugify("one two three four", 12), "one-two");
    assert_eq!(slugify("one two three four", 18), "one-two-three-four");
}

#[test]
fn a_first_word_longer_than_the_limit_is_cut() {
    assert_eq!(slugify("Supercalifragilistic expialidocious", 5), "super");
}

#[test]
fn unchanged_behaviour() {
    assert_eq!(slugify("Hello, World!", 40), "hello-world");
    assert_eq!(slugify("Straße", 40), "strasse");
    assert_eq!(slugify("", 10), "");
}
