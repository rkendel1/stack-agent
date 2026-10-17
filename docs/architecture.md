# stack-agent architecture

stack-agent is one Rust product: **Chip** agent orchestration over **FX** inference, built from one
checkout, one Cargo workspace and one `Cargo.lock` into one executable, `stack-agent`. Chip and FX
are components of it, not separately released dependencies. Where the code came from:
[`migration-provenance.md`](migration-provenance.md). Its gaps: [`known-issues.md`](known-issues.md).

## Responsibilities

| Component | Owns | Crates |
| --- | --- | --- |
| **stack-agent** | Product entry point, CLI composition, configuration wiring, provider selection, the local environment, the `work` / `verify` / `serve` commands | `stack-agent` (was `chip-cli`; library `chip_cli`) |
| **Chip** | Agent loop, model-decision boundary, capability validation and tool orchestration, session/turn control, observation -> evidence, goal evaluation, bounded recovery | `chip-core`, `chip-inference` (Chip's model port), `chip-project`, `chip-pax`, `chip-remote-env`, `chip-session`, `chip-turns`, `chip-compute`, and the frozen/experimental crates listed in [`product/crates.md`](product/crates.md) |
| **Chip -> FX adapters** | The only Chip crates that link FX: map Chip's model port onto the FX executor and its native provider | `chip-fx`, `chip-fx-native`, `chip-wasm-host` |
| **FX** | Provider-neutral inference contract, single-attempt execution, provider wire formats, stream/response normalization, inference cancellation, typed delivery outcomes | `fx-core`, `fx-single-attempt`, `fx-native-provider`, `fx-host`, `fx-wasm-abi` |
| **Tool / backend implementations** | The actual operation and last-mile authorization | `chip-project` (files, git), `chip-pax` + an external `pax` (tests, observation), an `EnvironmentProvider` |
| **Durable authority** | Durable state, where one is configured | None inside stack-agent. See [`known-issues.md`](known-issues.md) (C) |

The crate layout is flat (`crates/<crate>`) because that is how chip-rs laid out its crates and
its tests locate sibling crates and the repository root by that layout. The `fx-` / `chip-` prefix
is the component boundary. FX's `rust/wasm-abi` directory became `crates/fx-wasm-abi` so that the
directory matches its package name.

## Dependency direction

```text
stack-agent (binary + chip_cli library)
   |-> chip-core -> chip-inference            (Chip: no FX, no HTTP)
   |-> chip-fx-native -> chip-fx -> fx-core, fx-single-attempt
   |                  \-> fx-native-provider  (the one HTTP client: ureq + rustls)
   \-> chip-project, chip-pax, chip-remote-env, ...

chip-wasm-host -> chip-fx, fx-core, fx-wasm-abi
fx-host        -> fx-core, fx-single-attempt, fx-wasm-abi
```

Enforced by tests rather than convention:

* `crates/chip-fx/tests/architecture.rs`: only the adapter crates link FX; `chip-core` and
  `chip-inference` never do; each FX crate exists exactly once in the workspace; the only HTTP client
  is `ureq` in `fx-native-provider`; FX never depends on a Chip crate or on `stack-agent`.
* `crates/chip-fx/tests/retirement.rs`: the removed second executor (`chip-legacy-http`) stays gone.
* `crates/chip-remote-env/tests/architecture.rs`: `stack-agent` is the one executable.
* `crates/stack-agent/tests/inventory.rs`: every workspace crate is classified in
  [`product/crates.md`](product/crates.md).
* `crates/fx-*/check-boundaries.sh`: FX's own dependency allowlists and source rules (run in CI).

## The product path

`stack-agent work "<goal>"` (and the same runtime behind `serve`):

1. `provider_selection` builds the one model provider: `chip-fx-native` wraps `fx-native-provider`
   (OpenAI-compatible or Anthropic wire format, chosen by `CHIP_PROVIDER` / `--provider`) behind FX's
   single-attempt executor. There is no other executor and no fallback provider.
2. Chip's work loop (`chip-core::run_work`) asks the model for one `chip.work-decision.v1` decision
   per turn, parsed strictly with no repair.
3. A capability request is validated and authorized by Chip, executed by a host-side capability
   (`project.read`, `project.write`, `pax.test`, ...), and its real result becomes an observation in
   the next model request.
4. Completion is decided by Chip from host-side evidence (`pax.test` results for change/verify
   work; grounded observations for inspection), never from the model's words.
5. Every model call is a single attempt. A failure carries FX's typed delivery certainty and is not
   retried.

`crates/stack-agent/tests/product.rs` exercises this path end to end through the built binary with
a scripted local endpoint, including a tool round trip; `crates/stack-agent/tests/credential_redaction.rs`
checks that a sentinel credential reaches only the provider.

## Rules this consolidation keeps

* A model's proposed tool call is not authorization to execute it.
* One provider abstraction (FX), one agent loop (Chip), one capability registry, one executable.
* No silent retry of an operation whose delivery or side effects are uncertain.
* Errors, cancellation outcomes and completion evidence stay explicit.
* No AppPort, plugin framework, persistence subsystem or speculative execution module was added.

## Release and change policy

* This repository is the development and release boundary for the combined product. Chip and FX
  changes that depend on each other land in the same change set.
* Internal crates depend on each other only by workspace path. Nothing pins `rkendel1/chip-rs` or
  `rkendel1/fx` by git; developing or testing here never needs a release of either.
* Third-party dependencies keep their normal version constraints and are resolved by the one
  `Cargo.lock`. FX's exact pins (`serde =1.0.229`, `serde_json =1.0.154`) are unchanged.
* The toolchain is pinned in `rust-toolchain.toml` (1.97.0, the version FX already pinned).
* The original repositories are untouched by this repository's existence; retiring them is a
  separate decision (see [`known-issues.md`](known-issues.md) (I)).

## Licensing

Chip's code is MIT ([`../LICENSE`](../LICENSE)); FX's crates are Apache-2.0
([`../LICENSE-APACHE`](../LICENSE-APACHE)), as declared in each crate's `Cargo.toml`. The licences
were carried over unchanged.
