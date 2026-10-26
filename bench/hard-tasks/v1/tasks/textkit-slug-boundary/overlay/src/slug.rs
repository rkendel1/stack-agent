use crate::transliterate;

/// A URL slug of at most `max_len` bytes: lowercase ASCII letters and digits in words separated by
/// single dashes. Accented letters are transliterated (`É` → `e`); anything else separates words.
/// A slug longer than `max_len` is cut after the last whole word that fits; if not even the first
/// word fits, it is cut to `max_len`.
pub fn slugify(text: &str, max_len: usize) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            word.push(c);
        } else if let Some(t) = transliterate(c) {
            word.push_str(t);
        } else if !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let mut slug = words.join("-");
    slug.truncate(max_len);
    slug
}
