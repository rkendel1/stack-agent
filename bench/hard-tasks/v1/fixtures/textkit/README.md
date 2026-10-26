# textkit

* `display_width(s)`: the number of terminal columns `s` occupies. East Asian wide characters
  (CJK ideographs, Hangul, full-width forms) take two columns; combining marks take none.
* `wrap(text, width)`: greedy word wrapping to at most `width` columns per line. Words are
  separated by single spaces in the output. A word wider than `width` is put on a line of its own.
* `slugify(text, max_len)`: a URL slug: lowercase ASCII letters and digits separated by single
  dashes, transliterating common accented letters (`é` → `e`, `ß` → `ss`). Slugs longer than
  `max_len` are cut at a word boundary, never inside a word, and never end with a dash.

The width table in `src/width.rs` was last updated for Unicode 15.
`src/translit.rs` is generated from `data/translit.tsv`; do not edit it by hand.
