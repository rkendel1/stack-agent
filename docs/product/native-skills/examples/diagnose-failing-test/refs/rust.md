# Rust projects

- A failing assertion names the test, for example `tests/policy.rs` or `module::tests::name`.
  Unit tests usually sit in the same file as the code (`#[cfg(test)] mod tests`); integration
  tests are under `tests/`.
- A compile error after your write is a failed test run, not a passing one. Read the first error,
  which names the file and line.
- Keep public signatures unchanged unless the goal asks otherwise.
