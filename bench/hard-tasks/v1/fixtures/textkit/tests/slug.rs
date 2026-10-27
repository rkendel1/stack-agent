use textkit::slugify;

#[test]
fn basic_slugs() {
    assert_eq!(slugify("Hello, World!", 40), "hello-world");
    assert_eq!(slugify("  Rust   2024 edition ", 40), "rust-2024-edition");
}

#[test]
fn transliterates() {
    assert_eq!(slugify("Crème brûlée", 40), "creme-brulee");
    assert_eq!(slugify("Straße", 40), "strasse");
}

#[test]
fn cuts_at_word_boundaries() {
    assert_eq!(slugify("one two three four", 13), "one-two-three");
    assert_eq!(slugify("one two three four", 12), "one-two");
}

#[test]
fn uppercase_accents() {
    assert_eq!(slugify("École Élan", 40), "ecole-elan");
}
