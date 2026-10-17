# Migration provenance

stack-agent was assembled by copying existing Rust code from two repositories at fixed commits
(`git archive`, so only tracked files), then making the smallest changes needed for one workspace.
No agent loop, inference engine, provider abstraction, tool registry or persistence layer was
written anew. Git history was not imported: the fx history contains the Zig implementation, which
this repository must not carry. The original repositories, their branches and their histories were
not modified.

## Source commits

| Repository | Commit used | Why this commit |
| --- | --- | --- |
| `rkendel1/chip-rs` | `c14522a11e15c1b307db8ac0c274844adeac50a8` (merge of chip-rs#42) | `main` head at migration. Newer than the suggested `3bc8937`: it adds typed delivery on failed works, cancellation reaching FX calls, the explicit proxy (`005663a`), the fx pin at fx#31 (`d7f5b74`), and a PAX shim test fix (`a67413f`) |
| `rkendel1/fx` | `b2d11d79b51fcfced9de3e4cbea5452a7a15773b` (merge of fx#33) | `main` head at migration. Newer than the suggested `a32d4ef` (fx#30). Its `rust/` tree is identical to `0babf6a` (fx#31), the commit chip-rs pinned (tree `0bba60cee1b3aa60bcac80222e3d562780b775ab` at both). fx#32 and fx#33 changed only Zig code and docs, so the FX Rust code in stack-agent is exactly what chip-rs `c14522a` built against |

What was copied: every chip-rs crate except `chip-session-memory`, plus chip-rs's `Cargo.lock`,
`AGENTS.md`, `LICENSE`, `README.md` (now `docs/chip-README.md`), `RELEASE_NOTES.md`, `.gitignore`,
`docs/`, `scripts/` and `audit/`. Also the five crates under fx `rust/`, fx's `LICENSE` (as
`LICENSE-APACHE`) and fx's `rust-toolchain.toml` (as the base of the root one).

## Crates

The tree hash is `git rev-parse <commit>:<source path>` in the source repository. A crate copied
unchanged has the same content as that tree; the changes are listed below.

| stack-agent path | From | Source path | Source tree |
| --- | --- | --- | --- |
| `crates/stack-agent` | chip-rs | `crates/chip-cli` | `cfe2e9f476242a95c2dfe17c4ba7d2c543ed76be` |
| `crates/chip-compute` | chip-rs | `crates/chip-compute` | `95013ac545972fca8a24e2f7b1bfa835d600b153` |
| `crates/chip-core` | chip-rs | `crates/chip-core` | `51686c30f3daf0d9d6f4d055d03030e767ec3385` |
| `crates/chip-decision-corpus` | chip-rs | `crates/chip-decision-corpus` | `1fe02dcc2453606ace74503a91836e1c405f0541` |
| `crates/chip-fx` | chip-rs | `crates/chip-fx` | `9a9eee4884e4ead53d7fc555add421906a80b0c1` |
| `crates/chip-fx-native` | chip-rs | `crates/chip-fx-native` | `26aaf5bee7ba0c1a25992f685c40999ed61fa4a9` |
| `crates/chip-graph` | chip-rs | `crates/chip-graph` | `5215c84cb3a23d7ba24d58ac35493251d0d76e60` |
| `crates/chip-inference` | chip-rs | `crates/chip-inference` | `aedfeab91bdb645da0e5f811b8747813e14cf91c` |
| `crates/chip-laya-reasoner` | chip-rs | `crates/chip-laya-reasoner` | `963428c192ca3138e7ee0b15ee36dff341915ee1` |
| `crates/chip-local-decision` | chip-rs | `crates/chip-local-decision` | `f71394d666cef9355d28fb41732fefcd9210dc83` |
| `crates/chip-local-decision-train` | chip-rs | `crates/chip-local-decision-train` | `852025ba2941ba62f257640ef7545dee8feb7efb` |
| `crates/chip-local-ml` | chip-rs | `crates/chip-local-ml` | `0dc698484c45359f4df5b4f0c58a4375355203b1` |
| `crates/chip-pax` | chip-rs | `crates/chip-pax` | `d712de2761b0def143cc5a801724dba88663cb0d` |
| `crates/chip-project` | chip-rs | `crates/chip-project` | `f407789dbca90fedc07b36851f166d0a482e4abe` |
| `crates/chip-reasoning-corpus` | chip-rs | `crates/chip-reasoning-corpus` | `d52990dcc78cb545895310e86917c45a4d2014bd` |
| `crates/chip-remote-env` | chip-rs | `crates/chip-remote-env` | `5c00d2343e1ff1e341fabfba86d98cc0646a7d6a` |
| `crates/chip-session` | chip-rs | `crates/chip-session` | `a9e58ac8b38d67552240623367413af09c8c45ef` |
| `crates/chip-turns` | chip-rs | `crates/chip-turns` | `24e68d3cf88d2d9b45c6dbf3dd7d60a7c732c88c` |
| `crates/chip-wasm-decision` | chip-rs | `crates/chip-wasm-decision` | `45da076bf72e6b47c69da35601ca682fdcb81109` |
| `crates/chip-wasm-decision-host` | chip-rs | `crates/chip-wasm-decision-host` | `a995380cbc3320c620a878f6d81d3407ea4fbaab` |
| `crates/chip-wasm-host` | chip-rs | `crates/chip-wasm-host` | `bf931d395af88b83db7d8943f39533ffa69d7933` |
| `crates/chip-wasm-reasoner` | chip-rs | `crates/chip-wasm-reasoner` | `bec3ada3da3a76d960cffcd5d12017e1240c8a95` |
| `crates/fx-core` | fx | `rust/fx-core` | `ec04e6e6fd16dc795600385ad393744bdbdd7e32` |
| `crates/fx-host` | fx | `rust/fx-host` | `b415ea0b21f1095866edf7b8061854548ee933ef` |
| `crates/fx-native-provider` | fx | `rust/fx-native-provider` | `5030946bdc44331a38734cf0616af75fabea4039` |
| `crates/fx-single-attempt` | fx | `rust/fx-single-attempt` | `705fa99e56df4c04b67a611e6f89969fea92fcf0` |
| `crates/fx-wasm-abi` | fx | `rust/wasm-abi` | `1bda03cd1607adf34dc2dd9e3c90d12ae39d687c` |

### Not migrated

- **`chip-session-memory`.** It was excluded from the chip-rs workspace. It depends on the private
  `rkendel1/flow_db` git repository, and its CI job was opt-in. It is an experiment, and keeping it
  would have reintroduced a cross-repository git dependency.
- **chip-rs CI workflows** (`rust.yml`, `release.yml`, `live-*.yml`, `laya.yml`, `local-ml.yml`)
  and **fx's workflows**. One new workflow replaces them (see below).
- **chip-rs's `.pr/` directory**: PR scratch notes, not product.
- **Everything in fx outside `rust/` and `LICENSE`:**
  - the Zig implementation, `build.zig`, `build.zig.zon`, Zig tests and benchmarks
  - the npm/TypeScript SDK and host harnesses (`sdk/`, `host/`)
  - the retirement tooling and inventory (`docs/rebuild/`)
  - `scripts/`
- **The per-crate `Cargo.lock` files in fx `rust/*`**: one workspace lockfile replaces them.

## Changes made during migration

Every file that differs from its source is listed here; anything not listed is byte-identical.
Reproduce the list with `diff -r` against `git archive` of the two commits.

### Workspace and dependencies

- **New root `Cargo.toml`.** Lists the chip-rs members (with `crates/chip-cli` renamed to
  `crates/stack-agent`) plus the five FX crates. It keeps chip-rs's profiles, sets
  `default-members = ["crates/stack-agent"]`, and drops chip-rs's `exclude` (its only entry,
  `chip-session-memory`, was not migrated).
- **`Cargo.lock`.** Chip-rs's lockfile, re-resolved:
  - the four FX packages lost their `source = "git+https://github.com/rkendel1/fx?rev=0babf6a…"`
    lines and became path packages
  - `fx-host` was added (it was not in chip-rs's graph)
  - `chip-cli` became `stack-agent`
  - **No third-party package changed version.** Checked by comparing every `name`/`version`
    pair.
- **Git pins replaced by `path = "../fx-…"`** in `crates/chip-fx/Cargo.toml`,
  `crates/chip-fx-native/Cargo.toml`, `crates/chip-wasm-host/Cargo.toml` and
  `crates/stack-agent/Cargo.toml` (normal and dev dependencies). Comments that said "pinned by
  commit" now say "workspace crate".
- **`rust/wasm-abi` became `crates/fx-wasm-abi`**, so that the directory matches its package name.
  `crates/fx-host/Cargo.toml` (its path dependency) and the path references in
  `crates/fx-host/check-boundaries.sh`, `crates/fx-native-provider/check-boundaries.sh` and the
  header of `crates/fx-wasm-abi/check-boundaries.sh` were updated. The fx-native-provider gate had
  been passing vacuously on the missing old path.
- **`rust-toolchain.toml`.** fx's pin (1.97.0, `rustfmt`, `clippy`, `wasm32-unknown-unknown`) is
  now the workspace pin. Chip-rs had used `stable` in CI with no pin file.

### The product crate (`crates/stack-agent`, was `crates/chip-cli`)

- **`Cargo.toml`:**
  - package `chip-cli` became `stack-agent`
  - `[lib] name = "chip_cli"` was added, so every `use chip_cli::…` path is unchanged
  - `[[bin]] chip` became `[[bin]] stack-agent`
- **`src/main.rs`.** Behavioral changes, all to output text:
  - `--help` / `-h` is new. It prints a command summary to stdout and exits 0. Before, `--help`
    fell through to the one-shot prompt path and was sent to a model as a prompt.
  - `--version` prints `stack-agent <version>` instead of `chip <version>`.
  - The no-argument usage lines name `stack-agent` instead of `cargo run -p chip-cli`.
  - The `capability-exec` usage line names `stack-agent`.
- **User-facing strings now name `stack-agent` instead of `chip`.** Nothing else about these
  commands changed:
  - `src/software_work.rs`: `usage: stack-agent work …`
  - `src/verify.rs`: `usage: stack-agent verify …`
  - `src/service.rs`: `usage: stack-agent serve …`
  - `src/graph_cmd.rs`: `usage: stack-agent impact|slice …`, `Run \`stack-agent init\`.`,
    `stack-agent init failed: …`
  - `src/decision_state_cmd.rs`: `usage: stack-agent decision-state …`
- **Unchanged on purpose:**
  - `provider_selection.rs` still passes `*b"chip-cli"` as the native provider's identifier seed;
    changing it would change generated request identifiers.
  - The service's "Chip Runtime Service listening on …" line and the `CHIP_*` environment
    variables are unchanged.
  - Comments and docs that say `chip` were left as written (see the note at the top of
    `docs/product/crates.md`).
- **Tests:**
  - `env!("CARGO_BIN_EXE_chip")` became `env!("CARGO_BIN_EXE_stack-agent")` in 17 test files and
    `benches/baseline.rs`. In 14 of those files that is the only change, checked by diff.
  - `cli_acceptance.rs`: `cargo run -p chip-cli` became `-p stack-agent`, and the `chip init`
    expectation was updated.
  - `executable_interface.rs` and `verify.rs`: the version expectation is now `stack-agent <v>`.
  - `executable_interface.rs`, `work.rs` and `verify.rs`: the usage-string expectations were
    updated.
  - `slice_acceptance.rs`: the `chip init` expectation was updated.
  - `inventory.rs` and `product_path.rs`: the crate directory `chip-cli` became `stack-agent`.
  - `benches/*.rs`: the `cargo bench -p chip-cli` comments were updated.
- **New tests:**
  - `tests/product.rs`: the product path through the binary and a tool round trip, the
    model-claim refusal, an unreachable provider, the non-streaming stand-in regression, and
    typed delivery on a failed call
  - `tests/credential_redaction.rs`: the sentinel credential

### Chip crates

- **`chip-fx/tests/architecture.rs`.** The chip-rs rule "no FX crate in this workspace" became
  "exactly one copy of each FX crate in this workspace":
  - the adapter-only check skips the FX crates themselves
  - the HTTP-client check allows `ureq` only in `fx-native-provider`
  - `no_second_fx_core_exists_in_this_workspace` now asserts that each FX crate is declared exactly
    once
  - a new test, `fx_never_depends_on_chip_or_the_product`, keeps the dependency direction
- **`chip-remote-env/tests/architecture.rs`:**
  - `chip_is_the_one_rust_executable` became `stack_agent_is_the_one_rust_executable`
  - its other `chip-cli` paths became `stack-agent`
- **`chip-compute`, `chip-local-ml`, `chip-laya-reasoner` architecture tests:** the crate name
  `chip-cli` became `stack-agent`, with the same assertions.
- **`chip-core/src/reasoning.rs`:** one doc comment path was updated.

### Docs and scripts

- **`docs/product/crates.md`:**
  - a note at the top
  - the inventory row `chip-cli` became `stack-agent`
  - the `chip-session-memory` row was removed
  - FX rows were added: `fx-core`, `fx-single-attempt` and `fx-native-provider` as PRODUCT L2;
    `fx-host` as INTEGRATION L2; `fx-wasm-abi` as INTEGRATION L1. The levels were assigned from
    what their own tests exercise.
  - the test path was updated
- **`docs/product/release-limitations.md`.** The line "The native FX provider has no HTTP proxy or
  custom CA support" was already false at chip-rs `c14522a`, so it now states the actual
  `SSL_CERT_FILE` / `CHIP_PROXY` behavior.
- **`docs/product/atomic-agent-technique-audit.md`:** `crates/chip-cli/` links became
  `crates/stack-agent/`. Its two links to the removed `chip-legacy-http` were already broken in
  chip-rs and are left as they were.
- **`docs/chip-README.md`.** Chip-rs's README with a note at the top; its relative links were
  adjusted for the new location.
- **New:** `README.md`, `docs/architecture.md`, `docs/known-issues.md`, this file,
  `.github/workflows/ci.yml`.
- **`scripts/package-release.sh`:**
  - builds `--bin stack-agent`
  - packages `stack-agent-<version>-<target>.tar.gz`, adding `LICENSE-APACHE`
  - the manifest names `stack-agent`, and its `rust_fx` set now lists the FX crates
- **`scripts/smoke-test.sh`:** expects the `stack-agent` executable and its version line.
- **`scripts/audit-dependencies.sh`:** `chip-cli` became `stack-agent`.

## CI

The new workflow is `.github/workflows/ci.yml`: one job, one checkout. It runs:

1. Formatting.
2. Workspace check (`--all-targets`).
3. Workspace tests, with PAX v0.4.1 installed and `FX_REQUIRE_TLS_TESTS=1`.
4. The release binary.
5. A CLI smoke test (`--help`, `--version`, `--test`).
6. The product integration test.
7. The secret-redaction test.
8. Wasm builds, plus chip-rs's zero-import check on `chip_wasm_host.wasm`.
9. The packaged release smoke test.
10. FX's boundary gates.
11. A no-Zig gate.

Dropped from the old pipelines, all available as manual runs:

- From chip-rs: the bench `--quick`, the decision-corpus and local-model reports, and the work demo
  loop.
- From fx: clippy per crate, `check-licenses.py`, and the Node/V8 Wasm harnesses.

## Verification

Verification ran on 2026-10-17 in a Linux x86_64 container with Rust 1.97.0 (from
`rust-toolchain.toml`), PAX v0.4.1 built from `674f3b3`, and `FX_REQUIRE_TLS_TESTS=1`. The steps
mirror the CI workflow, run against this tree. Only these documentation files were edited
afterwards. The GitHub Actions workflow itself had not yet run when this was written.

### Baseline for comparison

The same machine ran the original suites:

- **chip-rs `c14522a`, `cargo test --workspace --locked` with PAX:** 1204 passed, 10 failed,
  1 ignored. All 10 failures are `error[E0463]: can't find crate for core` in
  `chip-wasm-decision-host`. chip-rs has no toolchain pin, and the machine's `stable` lacks the
  wasm32 target. Under stack-agent's pinned toolchain these tests pass.
- **fx `b2d11d7`, `cargo test --locked` in each `rust/*` crate:**
  - fx-core: 137 passed
  - fx-single-attempt: 31 passed
  - fx-native-provider: 76 passed
  - fx-host: 13 passed
  - wasm-abi: 4 passed
  - 0 failed in total

### Results

| Step | Command | Result |
| --- | --- | --- |
| Formatting | `cargo fmt --all -- --check` | pass |
| Compilation | `cargo check --workspace --locked --all-targets` | pass |
| Workspace tests | `cargo test --workspace --locked --no-fail-fast` | pass: **1486 passed, 0 failed, 1 ignored** across 193 test binaries |
| Release binary | `cargo build --locked --release --bin stack-agent` | pass |
| CLI smoke | `stack-agent --help`, `--version`, `--test` | pass |
| Product integration | `cargo test --locked -p stack-agent --test product` | pass: 6 passed |
| Secret redaction | `cargo test --locked -p stack-agent --test credential_redaction` | pass: 4 passed |
| Wasm builds | `fx-core`, `fx-single-attempt`, `fx-wasm-abi`, `fx-host`, `chip-wasm-host`, `chip-wasm-decision` built for `wasm32-unknown-unknown`; `chip-core` checked | pass |
| Wasm imports | no import section in `chip_wasm_host.wasm` | pass |
| Release package and smoke | `scripts/package-release.sh dist && scripts/smoke-test.sh dist/stack-agent-*.tar.gz` | pass ("SMOKE PASSED"; `serve` ran with real PAX) |
| FX boundary gates | `crates/fx-*/check-boundaries.sh` | pass (all five) |
| No Zig | no `*.zig`, `*.zig.zon` or `build.zig` file in the tree | pass |
| Dependency audit (not in CI) | `scripts/audit-dependencies.sh` | pass |
| Doc link check (not in CI) | `python3 scripts/check-design-docs.py` | **fail**: 2 broken links to the removed `chip-legacy-http` in `docs/product/atomic-agent-technique-audit.md`. Already broken in chip-rs `c14522a` (same output there) and left unchanged |

The 1486 tests are the 1475 from the baseline (chip-rs 1214 + FX 261) plus 11 new ones: 6 in
`product.rs`, 4 in `credential_redaction.rs`, and `fx_never_depends_on_chip_or_the_product`. The
ignored test is `the_recorded_configuration_is_what_selection_chooses`, which is ignored in chip-rs
too.

**Skips that look like passes.** Several existing tests return early and print `SKIPPED` when
their dependency is absent: PAX, `git`, `openssl` (unless `FX_REQUIRE_TLS_TESTS=1`), or a real
model. In this run PAX, `git` and `openssl` were present and TLS tests were required. Real-model
tests (`CHIP_TEST_REAL_MODEL=1`, `CHIP_BENCH_REAL_MODEL=1`) were not run, and none ran against a
live provider. Optional features `local-ml` (needs ONNX Runtime) and `laya` (needs a local
checkpoint) were not built.

**Targets.** Native `x86_64-unknown-linux-gnu`; `wasm32-unknown-unknown` compile-only, except the
`chip-wasm-decision` module, which tests execute under `wasmi`. No macOS or Windows run.

**Clippy** was not part of this verification. fx ran it per crate with `-D warnings`; chip-rs did
not.
