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

## Install

Prebuilt releases for macOS on Apple Silicon and Linux x86_64 with glibc 2.34 or newer (details, verification and what is
not claimed: [`docs/product/release.md`](docs/product/release.md)):

```sh
curl -fsSL https://github.com/rkendel1/stack-agent/releases/latest/download/install.sh | sh
stack-agent --version
stack-agent doctor        # checks the model setup, PAX, git and the project; changes nothing
```

The installer checks the archive's SHA-256 before extracting it, puts one file in `~/.local/bin`, and
never uses `sudo`, edits a profile or replaces an existing `stack-agent` without `--force`. The
executable is not code-signed or notarized. `pax` is a separate dependency that is never installed
for you; `doctor` says what is missing and how to get it. Until a release is published, build from
source as below.

## Quick start

Build once, and install [`pax`](https://github.com/rkendel1/pax) (v0.4.1 or newer) on your `PATH`:
`stack-agent` uses it to run the project's tests, and only its result can verify a change.

```sh
cargo build --locked --release --bin stack-agent      # the binary is target/release/stack-agent
stack-agent --test                                    # offline self-test; needs no model
```

Select a model once per shell (see [Configuration](#configuration)), check the machine, then run a
coding task in the project's directory:

```sh
cd my-project
export CHIP_MODEL=<model> CHIP_ENDPOINT=<URL ending in /chat/completions>   # any OpenAI-compatible server
stack-agent doctor                                  # is a model selected and reachable; are pax and the project usable
stack-agent work "Fix the failing test"
```

`stack-agent doctor` makes no model call and changes nothing. A passing `doctor` does not show the
key is valid; `stack-agent "Say hello"` makes one call to check that.

While it runs, one line per model call and per operation goes to stderr:

```text
turn 1: asking the model
  ok      project.read src/lib.rs
turn 2: asking the model
  ok      project.write src/lib.rs
turn 3: asking the model
  ok      pax.test (tests passed: 1 passed, 0 failed)
```

Then a report. Read it in this order:

- **The first line** says how the work ended (`Work completed.`, `Work blocked.`, `Work stopped at a
  limit.`, `Work cancelled.`, `Work failed.`, `Work completed, not verified.`).
- **`Verified: yes`** means `pax.test` passed *after the last change Chip saw written*. A model saying
  it is done never counts, and the exit status is 0 only when verified.
- **`Failed operations:`** lists every operation that did not succeed, with its error code
  (`parent_missing`, `stale_base`, failing tests, ...).
- **`Next steps:`** comes last: review the change with `git diff`, run the tests yourself with
  `pax test`, and see every step with `stack-agent agent-state show --task-id <id>`.

`stack-agent work` changes files in place in the current directory; it does not commit. Review
`git diff` before you commit. `stack-agent work --help` repeats this and lists every option.

### Configuration

Each setting is read from its flag, then its environment variable, then its default. Nothing else
is consulted: there is no configuration file, and no other provider or model is ever tried.

| What | Flag | Variable | Default |
| --- | --- | --- | --- |
| Provider | `--provider` | `CHIP_PROVIDER` | `openai-compatible` (or `anthropic`) |
| Model | `--model` | `CHIP_MODEL` | none: required, never guessed |
| Endpoint | `--endpoint` | `CHIP_ENDPOINT` | required for `openai-compatible` (a URL ending in `/chat/completions`); `https://api.anthropic.com/v1/messages` for `anthropic` |
| Key | none | `CHIP_API_KEY`, else `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` | none (a local server may need none). Never printed |
| Task mode | `--kind` | none | `change` (edit until the tests pass); `verify`; `inspect` |
| Limits | `--max-turns`, `--max-executions`, `--deadline-secs` | none | 12 turns, 8 operations (each at most 50); no deadline |
| Output | `--json`, `--print-reply` | none | text report, progress lines on stderr |
| Generation | `--max-output-tokens`, `--temperature`, `--structured-output`, `--token-limit-field`, `--provider-profile`, `--call-timeout-secs`, `--context-budget-bytes` | `CHIP_MAX_OUTPUT_TOKENS`, `CHIP_TEMPERATURE`, `CHIP_STRUCTURED_OUTPUT`, `CHIP_TOKEN_LIMIT_FIELD`, `CHIP_PROVIDER_PROFILE`, `CHIP_CALL_TIMEOUT_SECS`, `CHIP_CONTEXT_BUDGET_BYTES` | 2048 tokens, temperature 0.0, `schema` (`none` on Anthropic unless set), `max_tokens`, no profile, 30 s, no budget |

Examples:

```sh
# Any OpenAI-compatible server, here a local Ollama:
export CHIP_MODEL=<model> CHIP_ENDPOINT=http://localhost:11434/v1/chat/completions
# A hosted OpenAI-compatible server that needs a key:
export CHIP_MODEL=<model> CHIP_ENDPOINT=<URL ending in /chat/completions> CHIP_API_KEY=<key>
# Anthropic:
export CHIP_PROVIDER=anthropic CHIP_MODEL=<model> ANTHROPIC_API_KEY=<key>
```

`stack-agent "Say hello"` makes one call to the selected model, to check it answers before a work.
Proxies, certificates and the transport are covered in
[`docs/product/native-provider.md`](docs/product/native-provider.md); every remaining flag is in
`stack-agent work --help`.

### Use it from Atomic Chat

[Atomic Chat](https://github.com/rkendel1/Atomic-Chat) lists stack-agent on its Launch page. Pick a
model there and press Run: it installs stack-agent if it is missing (the release installer, else
`cargo install` from this repository), writes `CHIP_PROVIDER`, `CHIP_ENDPOINT`, `CHIP_MODEL`,
`CHIP_API_KEY` and `CHIP_CALL_TIMEOUT_SECS=600` into a marked block of your shell's start-up file
(`setx` on Windows) pointing at its local server, and opens a terminal that runs `stack-agent
doctor`. From there, `cd` into a project and run `stack-agent work "<goal>"`. `pax` is still yours
to install.

### When a run goes wrong

| You see | It means | Do next |
| --- | --- | --- |
| `error: no model is selected ...` (exit 3) | no model or endpoint is set; nothing ran | set `CHIP_MODEL` and `CHIP_ENDPOINT` (the error shows how) |
| `error: the selected model did not answer ...` | the endpoint refused, failed or was unreachable: exit 3 if nothing had run yet, otherwise `Work failed.` and exit 4 | check the endpoint is up, the model name and the key; `git diff` if work had started |
| `error: PAX unavailable ...` (exit 3) | `pax` is not on `PATH` and the work is `change` or `verify`, which complete only through a passing `pax.test`; nothing ran. `--kind inspect` does not need `pax`: it lists, reads and searches the project without it | install `pax` 0.4.1 or newer, or use `--kind inspect` |
| `Work failed.` with `not a valid decision` | the model's reply was not the JSON decision Chip asks for | rerun with `--print-reply`; try a model that follows JSON instructions |
| `Failed operations:` | an operation was refused or failed; the model saw the reason | read the error code; `git diff` shows what did change |
| `Unresolved write conflicts:` | a file changed after the work read it; it was not overwritten | review that file, then run the work again |
| `Work stopped at a limit.` / `Work cancelled.` | the turn, operation or time limit, or Ctrl-C, ended it | `git diff` for partial edits; raise a limit if the work was progressing |
| `Verified: no` with a passing last test | completion was refused (e.g. tests were edited, or the project changed after the run) | see `Evidence not accepted:` in the report |

## Build and run

The toolchain is pinned by `rust-toolchain.toml` (Rust 1.97.0 with the `wasm32-unknown-unknown`
target). `stack-agent --help` lists every command by what it is for. The same work, for a script:

```sh
CHIP_PROVIDER=openai-compatible CHIP_MODEL=<model> CHIP_ENDPOINT=<chat-completions URL> \
CHIP_API_KEY=<key> stack-agent work "Make the tests pass." --json
```

`--json` prints one JSON report on stdout and no progress lines. The default build needs no model
API key; a real model is needed only when you run real work.

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

Episodes: each finished `work` and `serve` work is also recorded in that database as an episode, with an outcome
decided from the host's verdict and references to its records. A later work of the same project is
told of at most 3 relevant verified episodes as evidence in its model requests; nothing is replayed
or authorized by them, and `--no-recall` turns recall off. Each episode a work was told of gets a separate,
observational feedback record of what happened after (helpful, not helpful, misleading or
inconclusive, from host evidence only); episodes themselves never change. `stack-agent agent-state
episodes` shows what is recorded, why each episode could or could not be recalled, and its feedback. See
[`docs/product/episodic-memory.md`](docs/product/episodic-memory.md).

Telemetry: with `OTEL_EXPORTER_OTLP_ENDPOINT` (a collector on loopback) or `STACK_AGENT_OTLP_FILE`
set, each finished work's durable record is exported as an OpenTelemetry trace under the GenAI
semantic conventions, from an allowlist: no goal, prompt, reply, reason, file content, output, path
or credential is ever in it. A failed export never changes the outcome. `stack-agent agent-state
export` exports a recorded work afterwards. See [`docs/product/telemetry.md`](docs/product/telemetry.md).

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
