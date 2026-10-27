use textkit::{slugify, transliterate};

#[test]
fn uppercase_accented_letters_transliterate() {
    assert_eq!(slugify("École Élan", 40), "ecole-elan");
    assert_eq!(slugify("ÜBER Straße", 40), "uber-strasse");
    assert_eq!(slugify("ÆØÅ", 40), "aeoa");
    assert_eq!(slugify("Ça va", 40), "ca-va");
}

#[test]
fn the_generated_table_is_unchanged() {
    assert_eq!(transliterate('é'), Some("e"));
    assert_eq!(transliterate('É'), None);
    assert_eq!(transliterate('ß'), Some("ss"));
}

#[test]
fn lowercase_unchanged() {
    assert_eq!(slugify("Crème brûlée", 40), "creme-brulee");
    assert_eq!(slugify("Hello, World!", 40), "hello-world");
}
