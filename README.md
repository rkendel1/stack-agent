# stack-agent

**One Rust agent product: Chip agent orchestration + FX inference, in one executable.**

stack-agent is built from one checkout, one Cargo workspace and one `Cargo.lock`. Chip (the agent
loop, capability validation, tool orchestration, verification) and FX (the provider-neutral
inference contract, provider execution, stream normalization and typed delivery outcomes) are
components of it, changed and released together. There is no Zig runtime, no separately installed
FX or Chip program, and no git pin to `rkendel1/chip-rs` or `rkendel1/fx`.

```text
stack-agent  (CLI, configuration, provider selection)
   |
   +-- Chip   agent loop, decisions, capabilities, evidence, goal evaluation
   |     |
   |     +-- chip-fx / chip-fx-native  (the only Chip crates that link FX)
   |               |
   +---------------+-- FX   fx-core, fx-single-attempt, fx-native-provider, fx-host, fx-wasm-abi
```

Details: [`docs/architecture.md`](docs/architecture.md). Where each crate came from:
[`docs/migration-provenance.md`](docs/migration-provenance.md). What is not done:
[`docs/known-issues.md`](docs/known-issues.md).

## Build and run

```sh
cargo build --locked --release --bin stack-agent
./target/release/stack-agent --help
./target/release/stack-agent --test            # offline self-test, built-in test provider
```

The toolchain is pinned by `rust-toolchain.toml` (Rust 1.97.0 with the `wasm32-unknown-unknown`
target). Bounded autonomous work on the project in the current directory:

```sh
CHIP_PROVIDER=openai-compatible CHIP_MODEL=<model> CHIP_ENDPOINT=<chat-completions URL> \
CHIP_API_KEY=<key> stack-agent work "Make the tests pass." --json
```

`work` and `verify` use an independently installed [`pax`](https://github.com/rkendel1/pax)
(v0.4.1 or newer) as the project's test truth when a work needs it. The default build needs no
model API key; a real model is needed only when you run real work.

Exit codes for `work` and `verify`: `0` verified, `1` not verified (including grounded inspections),
`2` usage, `3` unavailable (provider, model or PAX), `4` runtime failure.

## Known limitations

> **RETRYING A TURN MAY REPEAT EXTERNAL EFFECTS.** stack-agent does not guarantee exactly-once side
> effects, does not resume an interrupted model response, and does not automatically replay anything.
> If a `stack-agent` process is killed or a work is interrupted, a *new* run starts from the beginning
> and the model may ask for the same capability again, which Chip will execute again. Before you retry
> interrupted work, account for effects that may already have happened (`git status`, `git diff`).

The ten behaviors of the Zig `fx` agent that the Rust release does not support, and what happens
when one is requested, are in
[`docs/product/release-limitations.md`](docs/product/release-limitations.md). This repository does
not by itself prove parity with the Zig application. The full list of carried-forward gaps
(streaming, cancellation, persistence, delivery certainty, completion evidence, credentials and
transport, WASM, performance, legacy retirement) is
[`docs/known-issues.md`](docs/known-issues.md). Release notes carried from chip-rs:
[`RELEASE_NOTES.md`](RELEASE_NOTES.md).

## Tests and CI

```sh
cargo fmt --all -- --check
cargo check --workspace --locked --all-targets
cargo test --workspace --locked                           # needs `pax` on PATH for the PAX tests
cargo test --locked -p stack-agent --test product         # Chip -> FX product path, tool round trip
cargo test --locked -p stack-agent --test credential_redaction
```

CI is one workflow, [`.github/workflows/ci.yml`](.github/workflows/ci.yml), using deterministic
local providers only. Real-model runs (the `CHIP_TEST_REAL_MODEL=1` tests and the live benchmarks)
are manual experiments, not part of CI.

## Repository map

| Path | What |
| --- | --- |
| `crates/stack-agent` | The product crate: `stack-agent` binary and the `chip_cli` library (was `chip-cli`) |
| `crates/chip-*` | Chip, from `rkendel1/chip-rs` |
| `crates/fx-*` | FX, from `rkendel1/fx` (`rust/`) |
| `docs/` | Architecture, known issues, provenance; `docs/product/` is Chip's product documentation carried from chip-rs; `docs/chip-README.md` is chip-rs's README |
| `scripts/`, `audit/` | Chip's release packaging, smoke test, dependency audit and audit harnesses |
| `AGENTS.md` | The engineering constitution carried from chip-rs |

## License

Chip's code is MIT ([`LICENSE`](LICENSE)). The FX crates are Apache-2.0
([`LICENSE-APACHE`](LICENSE-APACHE)), as declared in each crate's manifest.
