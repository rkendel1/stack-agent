use crate::display_width;

/// Greedy word wrapping: each line holds as many whole words as fit in `width` columns, words
/// separated by one space. A word wider than `width` gets a line of its own. Runs of whitespace,
/// including newlines, separate words like a single space.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    assert!(width > 0, "a line is at least one column wide");
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_width = 0;
    for word in text.split_whitespace() {
        let w = display_width(word);
        if line.is_empty() {
            line.push_str(word);
            line_width = w;
        } else if line_width + 1 + word.chars().count() <= width {
            line.push(' ');
            line.push_str(word);
            line_width += 1 + w;
        } else {
            lines.push(std::mem::take(&mut line));
            line.push_str(word);
            line_width = w;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}
