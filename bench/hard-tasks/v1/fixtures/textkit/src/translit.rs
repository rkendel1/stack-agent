// GENERATED from data/translit.tsv by `make translit`. DO NOT EDIT BY HAND.
//! Transliteration of common accented Latin letters to ASCII.

/// The ASCII spelling of a lowercase accented letter, if it has one.
pub fn transliterate(c: char) -> Option<&'static str> {
    match c {
        'à' => Some("a"),
        'á' => Some("a"),
        'â' => Some("a"),
        'ä' => Some("a"),
        'ç' => Some("c"),
        'è' => Some("e"),
        'é' => Some("e"),
        'ê' => Some("e"),
        'ë' => Some("e"),
        'ì' => Some("i"),
        'í' => Some("i"),
        'î' => Some("i"),
        'ï' => Some("i"),
        'ñ' => Some("n"),
        'ò' => Some("o"),
        'ó' => Some("o"),
        'ô' => Some("o"),
        'ö' => Some("o"),
        'ù' => Some("u"),
        'ú' => Some("u"),
        'û' => Some("u"),
        'ü' => Some("u"),
        'ß' => Some("ss"),
        'æ' => Some("ae"),
        'ø' => Some("o"),
        'å' => Some("a"),
        _ => None,
    }
}
