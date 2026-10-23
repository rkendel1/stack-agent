# Targeting scorer v4

This replaces the v3 scorer for future evaluations. The v3 scorer
(`../2026-10-22-targeting-v3-held-out/scorer/eval_v3.py`) and its recorded results stay as they
were.

## Why a new version

v3 split a candidate id with `rsplit(":", 1)`. For `symbol:src/version.rs:Version::cmp` that gives
the path `src/version.rs:Version:` and the name `cmp`, so **no method target could ever match**.
v4 changes only how ids are parsed and matched. The per-fixture measurements and the P1–P5 criteria
are the ones `../2026-10-22-targeting-v3-held-out/PREREGISTRATION.md` defines.

## Symbol ids

```
symbol:<path>:<qualified>[#n]
```

| Part | Meaning |
|---|---|
| `path` | The repository-relative source file. It is the **shortest** prefix ending in `.rs` after which the rest parses as a qualified name, so a directory named `x.rs/` does not end it early. |
| `qualified` | The function's path inside its file. Segments are separated by `::`; none is empty and none contains `:`. Examples: `f`, `Type::method`, `Trait::method` (a default method), `outer::inner::Type::method`. |
| `#n` | The index's suffix for the n-th function with the same qualified name in one file. It is dropped for matching. |

A malformed id raises `BadId`. It is never matched loosely.

## Matching a candidate to a ground-truth target `{file, function}`

- **File.** The paths are equal, or the id's path ends with `/<file>`. Backslashes and leading `./`
  are normalized first.
- **Function.** The qualified name, without `#n` and with whitespace removed, must equal `function`
  exactly. Names are compared whole, never by their last segment:
  - `Version::cmp` does not match `cmp`;
  - `Other::cmp` does not match `Version::cmp`;
  - `src/my_version.rs` does not match `version.rs`.

## Use

```
python3 -I score_v4.py run BINARY FIXTURES_DIR OUT_DIR   # rank every fixture with BINARY, then score
python3 -I score_v4.py rescore FIXTURES_DIR RAW_DIR      # score raw rank_target outputs already saved
python3 -I test_score_v4.py                              # regression tests
```

## Validation

- `test_score_v4.py` has 17 tests with known answers:
  - id parsing: methods, nested module and type paths, ordinary functions, `#n` suffixes, other
    delimiters, malformed ids, and the v3 defect itself;
  - candidate-to-target matching, including names that must not match;
  - each of P1–P5, on synthetic results;
  - the guard that refuses to build fixture copies inside an enclosing Cargo workspace.
- Rescoring the frozen v3 raw outputs with v4 gives exactly the labeled id-fix rescoring
  (`../2026-10-22-targeting-v3-held-out/raw/results-idfix.json`): the same criteria and the same
  per-fixture target ranks.
