# Rust projects

- A failing assertion names the test as `module::tests::name`; the test is usually in the same file
  as the code (`#[cfg(test)] mod tests`) or under `tests/`.
- A compile error after your write is a failed test run, not a passing one: read the first error,
  which names the file and line.
- Keep public signatures unchanged unless the goal asks for a change; other crates in the workspace
  may depend on them.
