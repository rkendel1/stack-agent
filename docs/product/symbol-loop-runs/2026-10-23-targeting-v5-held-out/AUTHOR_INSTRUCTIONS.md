# Instructions for the independent fixture author (v5 held-out set)

You are writing a held-out benchmark of small, realistic Rust projects. Each contains one or more
deliberate bugs, and the benchmark evaluates an automated fault-localization tool.

## Isolation rules

- **Do not look at how the tool works.** Do not read anything under `/home/user/stack-agent`. Under
  `/tmp/claude-0/-home-user-stack-agent/09a178b7-2c04-50ed-be6c-71f941dbb8dd/scratchpad`, read
  nothing except your output directory. In particular, do not open `symbol-targeting/`,
  `heldout-fixtures/`, `t1/` or `t3/`.
- **Write code the way a real engineer writes a small library or service.** Do not make bugs easy
  or hard for any particular tool, and do not name functions after the bug.

## Where and how to build

- **Output directory:**
  `/tmp/claude-0/-home-user-stack-agent/09a178b7-2c04-50ed-be6c-71f941dbb8dd/scratchpad/heldout-v5-fixtures/`
  (create it). Use one subdirectory per fixture, named `g01` … `g12`. Do not put the category in
  the name.
- **No network.** Use only the standard library (no dependencies at all), and always pass
  `--offline`. Edition 2021.
- **Build directories.** Give every cargo command its own `CARGO_TARGET_DIR`, under
  `/tmp/claude-0/-home-user-stack-agent/09a178b7-2c04-50ed-be6c-71f941dbb8dd/scratchpad/v5-author-build/`.
  The buggy copy and the fixed copy of one fixture must use **different** build directories.
  Delete them when you are done (disk is limited).
- **Package names.** Every fixture's package (or packages) must have a name unique across all 12.

## Each fixture

A fixture is one Cargo package, or a small workspace of two or three crates, containing:

- **Source.** Several modules under `src/`: roughly 150–600 lines of real-looking code (one fixture
  is larger, see "large search" below).
- **Tests.** One integration-test file under `tests/`, with `#[test]` functions that **fail**
  because of the bug(s), plus some tests that pass.
- **`GROUND_TRUTH.json`**, in the format below.
- **`solution.patch`.** A unified diff, from the fixture root and `git apply`-able, that is the
  reference fix. It changes only files under `src/` (or the crates' `src/`), never tests.

Leave the fixture directory in the **buggy** state.

## Categories: 12 fixtures, 3 per category

1. **deep.** The bug is in one function body. The failing test reaches that function only through
   at least three levels of ordinary calls. The fix changes that one body.
2. **dispatch.** The bug is in the body of one implementation of a trait, reached through a trait
   object, a generic parameter, or a value created by a factory or helper function. Production code
   has at least two implementations of the trait. The fix changes that one body.
3. **multi.** Fixing the failing tests requires changing the bodies of two or more different
   functions. No signature or type changes.
4. **declaration.** The fix requires changing a declaration (a struct or enum field or variant
   type, a new field or variant, or a function signature), not only bodies.

## Required situations

Each situation must appear in **at least one** fixture. A fixture may cover several. Spread them
across categories as fits naturally.

| Tag | Situation |
|---|---|
| `producer_consumer` | The failing test panics inside one production function (the consumer), but the bug is in a different function that computes a value the consumer uses (the producer). The consumer is correct given correct input. |
| `implicit_dispatch` | The bug is reachable from the test only through an overloaded operator or implicit trait call: a comparison, equality or arithmetic operator on a user type, or a standard method that calls a user trait implementation (sorting, `max`/`min`, `==` in `contains`, `Display` through `format!`, `Iterator::next` in a `for` loop, `Drop`, …). |
| `factory_or_helper_type` | The concrete type of the object whose method has the bug is known only from what a helper or factory function returns. For example, a test helper builds the object, or a registry function returns `Box<dyn Trait>`. |
| `uncompiled_test` | The acceptance test file does not compile against the buggy code. The functions the fix changes are exercised by tests in that file that do not themselves contain the compile errors, so they can never be observed running. |
| `ambiguous` | Two or more functions are such that changing **either one alone** is a complete, valid fix, and nothing in the test output says which. List all of them, and set `"any_of": true`. |
| `large_search` | The failing test exercises a large codebase: at least 800 distinct functions are reachable from the failing test through ordinary calls, with the bug far from the test. Generated code is fine, but it must be plain Rust source in the fixture (no build scripts, no macros that generate the functions). |

## GROUND_TRUTH.json format

```json
{
  "fixture": "g01",
  "category": "deep" | "dispatch" | "multi" | "declaration",
  "fix_kind": "single_body" | "multi_body" | "declaration",
  "true_targets": [ {"file": "src/service.rs", "function": "Billing::total"} ],
  "any_of": false,
  "declarations": [ {"file": "src/model.rs", "item": "enum Source"} ],
  "acceptance": ["cargo", "test", "--offline"],
  "tags": ["producer_consumer"],
  "panic_site": {"file": "src/pages.rs", "function": "paginate"},
  "paths": [
    {"target": 0, "steps": [
      {"file": "tests/api.rs", "function": "last_page_is_partial", "via": "start"},
      {"file": "src/lib.rs", "function": "Api::list", "via": "method"},
      {"file": "src/pages.rs", "function": "paginate", "via": "call"},
      {"file": "src/pages.rs", "function": "page_bounds", "via": "call"}
    ]}
  ],
  "description": "one or two sentences: the bug and the fix"
}
```

- **`function`** is the function's path inside its file:
  - a free function is its name (`parse_line`);
  - a method is `Type::method`, using the type's last path segment as written in the impl. For
    `impl Trait for Type`, use `Type::method`;
  - a trait's default method is `Trait::method`;
  - add enclosing inline `mod` names in front (`inner::helper`).
- **`file`** paths are relative to the fixture root.
- **`true_targets`.** List every function whose body the reference fix changes. For an
  `ambiguous` fixture, list every function that alone would be a valid fix, set `"any_of": true`,
  and make the reference fix change one of them.
- **`panic_site`.** The production function where the failing test's panic occurs, when that is
  not one of the true targets. Use `null` otherwise, and for compile failures.
- **`paths`.** For each true target, give at least one real call path from a failing test function
  to it, one step per function, as the code actually executes. Test helper functions are steps too.
  `via` says how the previous step reaches this one: `start` (the test itself), `call`, `method`,
  `trait_object`, `generic`, `operator`, `implicit_trait` (a standard method calling a user trait
  implementation), `helper_return`, `factory_return`, `closure`, or `other`.

## Verification

Do this for every fixture, before you report, and fix the fixture until it holds:

1. In the buggy state, `cargo test --offline` (from the fixture root, with its own build directory)
   fails in the intended way. For an `uncompiled_test` fixture, the test file fails to compile.
2. In a fresh copy, `git apply solution.patch` (or `patch -p1`), and run `cargo test --offline` with
   a different build directory. It passes, with every test passing. Delete the copy.
3. For `ambiguous` fixtures, also show that the alternative fix (changing the other listed function
   instead) makes every test pass. Describe it in `VERIFICATION.md`.
4. For `multi` fixtures, check by actually applying each part of the fix alone that every partial
   fix leaves at least one test failing.

Write `heldout-v5-fixtures/VERIFICATION.md`, recording for each fixture:
- the category and tags;
- the failing test names before the fix;
- the exact summary or compile-error lines before the fix, and the passing summary lines after;
- for `ambiguous` and `multi` fixtures, the extra checks above.

When you are done, report back briefly:
- the list of fixtures, each with its category, tags and a one-line description;
- that every fixture verified;
- which situation tags each fixture covers.

Do not report anything about tools, or about how a tool might find the bugs.
