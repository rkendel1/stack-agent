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

Exit codes, one contract for `work`, `verify`, `serve` and the one-shot prompt:

- `0`: verified (for the one-shot prompt, the turn completed; that path makes no verification
  claim).
- `1`: ran correctly but not verified: blocked, a limit, cancelled, or a grounded inspection.
- `2`: usage.
- `3`: a required dependency was unavailable and nothing was done.
- `4`: runtime failure.

The full table is in [`docs/known-issues.md`](docs/known-issues.md) (A).

Durable state: every `work` and `serve` work is persisted in FeltDB by default (the project's
database under the runtime data directory, or `--state-db FILE`; `--no-state` turns it off). Each
work has its own id (`work_<16 hex>`), and its intents, observations, every event with whether the
host observed it, the model inferred it or Chip decided it, and the host's verdict are recorded as
they happen. A run after a crash reports the interrupted work; `stack-agent agent-state list|show`
inspects it. See [`docs/product/agent-state.md`](docs/product/agent-state.md).

Cancellation and deadlines:

- `stack-agent work --deadline-secs N` and `stack-agent serve --work-deadline-secs N` bound the
  whole work.
- Ctrl-C (or SIGTERM) asks a running `work` to stop at its next boundary. A second Ctrl-C exits at
  once.
- An execution already running is not interrupted.

`serve` shows a work's events while it runs (`GET /v1/work/{id}/events?after=N`).

Experiments (opt-in; no model key or network): `stack-agent experiment list | validate <id> | run <id>`
compares a candidate strategy with a baseline on controlled tasks, judged by host-observed evidence.
`work`, `verify` and `serve` never run a candidate. See
[`docs/product/experimentation.md`](docs/product/experimentation.md).

Automatic configuration (opt-in): `stack-agent work --auto "<goal>"` profiles the project and
selects a built-in configuration (which existing capabilities to offer, which context policy),
ranked by recorded verified outcomes. It refuses work that nothing here could verify; for example,
`change` on a non-Cargo project exits 3 before any model call. Records are returned with `--json`
and stored nowhere. See [`docs/product/automatic-configuration.md`](docs/product/automatic-configuration.md).

## Known limitations

> **RETRYING A TURN MAY REPEAT EXTERNAL EFFECTS.** stack-agent does not guarantee exactly-once side
> effects, does not resume an interrupted model response, and does not automatically replay anything.
> If a `stack-agent` process is killed or a work is interrupted, a *new* run starts from the beginning
> and the model may ask for the same capability again, which Chip will execute again. Before you retry
> interrupted work, account for effects that may already have happened (`git status`, `git diff`).

What a model may touch in the project: credential files (`.env*`, `.aws`, `.ssh`, private keys,
`secrets.*` data files and others) are withheld from every capability; build scripts, Cargo and
toolchain configuration, CI pipelines, Git hook managers, agent instruction files, editor settings
and shell start-up files may be read but are never written; and one work at a time holds a lock on
the directory. **Verifying a repository still runs its own code** (its build script, any compiler
wrapper its `.cargo/` configures, and its tests) with your permissions: point stack-agent only at
repositories you would build yourself. Details: [`docs/known-issues.md`](docs/known-issues.md) (J).

The ten behaviors of the Zig `fx` agent that the Rust release does not support, and what happens
when one is requested, are in
[`docs/product/release-limitations.md`](docs/product/release-limitations.md). This repository does
not by itself prove parity with the Zig application. The full list of carried-forward gaps
(streaming, cancellation, persistence, delivery certainty, completion evidence, credentials and
transport, WASM, performance, legacy retirement) is
[`docs/known-issues.md`](docs/known-issues.md). What `work` records about each run, in the project
and in the runtime data directory, and how `--auto` uses it, is in
[`docs/product/learning.md`](docs/product/learning.md). Release notes carried from chip-rs:
[`RELEASE_NOTES.md`](RELEASE_NOTES.md).

## Tests and CI

```sh
cargo fmt --all -- --check
cargo check --workspace --locked --all-targets
cargo test --workspace --locked                           # needs `pax` on PATH for the PAX tests
cargo test --locked -p stack-agent --test product         # Chip -> FX product path, tool round trip
cargo test --locked -p stack-agent --test credential_redaction
cargo test --locked -p stack-agent --test prompt_injection   # injection corpus through the product loop
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
