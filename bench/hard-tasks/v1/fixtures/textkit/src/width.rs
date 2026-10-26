/// Ranges of East Asian wide characters (two columns), sorted, inclusive.
const WIDE: &[(u32, u32)] = &[
    (0x1100, 0x115F),   // Hangul Jamo
    (0x2E80, 0x303E),   // CJK Radicals .. CJK Symbols and Punctuation
    (0x3041, 0x33FF),   // Hiragana .. CJK Compatibility
    (0x3400, 0x4DBF),   // CJK Unified Ideographs Extension A
    (0x4E00, 0x9FFF),   // CJK Unified Ideographs
    (0xA960, 0xA97F),   // Hangul Jamo Extended-A
    (0xAC00, 0xD7A3),   // Hangul Syllables
    (0xF900, 0xFAFF),   // CJK Compatibility Ideographs
    (0xFE30, 0xFE4F),   // CJK Compatibility Forms
    (0xFF00, 0xFF60),   // Fullwidth Forms
    (0xFFE0, 0xFFE6),   // Fullwidth Signs
    (0x1F300, 0x1F64F), // Misc Symbols and Pictographs, Emoticons
    (0x20000, 0x3FFFD), // CJK Unified Ideographs Extension B ..
];

/// Ranges of combining marks (zero columns), sorted, inclusive.
const COMBINING: &[(u32, u32)] = &[(0x0300, 0x036F), (0x1AB0, 0x1AFF), (0x1DC0, 0x1DFF), (0x20D0, 0x20FF), (0xFE20, 0xFE2F)];

fn in_table(c: u32, table: &[(u32, u32)]) -> bool {
    table
        .binary_search_by(|&(lo, hi)| {
            if c < lo {
                std::cmp::Ordering::Greater
            } else if c > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Columns one character occupies: 0, 1 or 2. Control characters count as 0.
pub fn char_width(c: char) -> usize {
    let u = c as u32;
    if u < 0x20 || (0x7F..0xA0).contains(&u) {
        0
    } else if in_table(u, COMBINING) {
        0
    } else if in_table(u, WIDE) {
        2
    } else {
        1
    }
}

/// Columns a string occupies.
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}
