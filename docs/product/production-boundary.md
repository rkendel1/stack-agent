# Production boundary: what ships, what is test-only, what is experimental

A short, evidence-backed map. It does not replace [`crates.md`](crates.md) (the per-crate inventory,
validation levels and rationale) or [`capabilities.md`](capabilities.md) (what the product can do);
it answers one question quickly: **what is actually in the production execution path?** Every claim
carries the command or call site that shows it, so it can be re-checked rather than believed.

Labels used throughout: **measured** (a command or test in this repository shows it), **read** (from
reading the code; no test pins it), **reported** (stated by maintainers, not reproducible here),
**proposed** (a plan, not behaviour).

Last checked against the tree at the commit that introduced this file (2026-10-09), on Linux x86_64.

**Status note (2026-10-17).** Since that check the executable is `stack-agent` (crate
`crates/stack-agent`, which was `chip-cli` in chip-rs; the library keeps the name `chip_cli`), and the
model transport changed: `fx-provider-http` was removed. Model calls go through
`provider_selection.rs` to `chip-fx-native` (`NativeModelProvider`) and the authoritative FX executor
with its native provider `fx-native-provider` (ureq on rustls; one request per attempt, no retry). That
provider supports an explicit HTTP `CONNECT` proxy (`CHIP_PROXY`, `CHIP_PROXY_USERNAME` /
`CHIP_PROXY_PASSWORD`, `CHIP_NO_PROXY`; ambient `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY` are
ignored) and a custom root bundle (`SSL_CERT_FILE`), with no way to turn TLS verification off; see
[`native-provider.md`](native-provider.md) and `docs/known-issues.md` section F. Paths below that still
say `chip-cli` mean `crates/stack-agent`; the transport lines have been updated.

## 1. The production path (measured, read)

There is one shipped executable, `chip` (`[[bin]]` of `chip-cli`; no other workspace crate has a
shipped binary, see section 3). Its production subcommands are dispatched in
`crates/chip-cli/src/main.rs`:

```text
chip work "<goal>"   -> software_work::work           (crates/chip-cli/src/software_work.rs)
chip serve           -> service::serve                (crates/chip-cli/src/service.rs)
chip verify          -> verify::verify                (crates/chip-cli/src/verify.rs)
chip capability-exec -> chip_remote_env::worker::run  (a worker run inside an external environment)
chip --version

work and serve share one runtime; verify is a narrower sibling (see below):
  provider_selection  --env CHIP_PROVIDER/MODEL/ENDPOINT-->  chip-fx-native::NativeModelProvider
                      -> chip-fx -> fx-core::execute -> fx-native-provider (one HTTPS request)
  WorkRuntime::prepare -> WorkRuntime::run                    (software_work.rs)
    -> Agent::run_work_with_context_policy                    (chip-core/src/work.rs)
         model reply -> ModelDecisionBoundary (strict chip.work-decision.v1, no repair)
         -> capability + input validation -> CapabilitySet (one declared backend, no fallback)
         -> execution in an Environment:  LocalEnvironment (chip-cli)  |  RemoteEnvironment (chip-remote-env)
              project.* / project.git.*  -> chip-project        pax.test / project.observe -> chip-pax -> external `pax`
         -> observation -> evidence -> goal evaluation (GoalKind: change | verify | inspect)
         -> Continue | Complete | Block | Escalate | Fail  (WorkLimits; LocalWorkPolicy = CompleteWhenVerified)
    -> render_json / WorkEvents -> exit status (0 verified; 1 not; 2 usage; 3 unavailable; 4 runtime failure)
```

Call-site facts behind it: `chip work` and `chip serve` call `Agent::run_work_with_context_policy`
with `&DeduplicatedEscalationContext` and `CompleteWhenVerified` (`software_work.rs`; `service.rs`
calls the same `WorkRuntime::run`). `chip verify` calls `Agent::run_work` with the `ReactToObservation`
policy and a single capability, `pax.test` (`verify.rs`). The product modules construct their `Agent`
without a local reasoner, and `crates/chip-cli/tests/product_path.rs` pins that they name no reasoner
and use only product crates and modules.

Production behaviour added most recently, so it is not mistaken for experiment output: **bounded
retention of finished work in `chip serve`** (`--max-retained-work`, default 256, 410
`work_expired` for evicted ids; `service.rs`, README "Retention of finished work"). It is a
production change. It was motivated by the session-store assessment but is independent of it and
adds no storage dependency.

## 2. What is linked into the shipped binary

Measured with `cargo tree -p chip-cli -e normal` (now `-p stack-agent`):

| Group | Crates | Status |
| --- | --- | --- |
| Production path | `chip-core`, `chip-inference`, `chip-fx`, `chip-fx-native`, `fx-core`, `fx-single-attempt`, `fx-native-provider`, `chip-project`, `chip-pax`, `chip-remote-env` (the transport crates replace the removed `fx-provider-http`) | PRODUCT / INTEGRATION (`crates.md` section 4) |
| Linked but not production | `chip-compute` (legacy `compute exec` adapter; demos, proofs), `chip-graph`, `chip-local-decision`, `chip-decision-corpus`, `chip-reasoning-corpus`, `chip-wasm-decision`, `chip-wasm-decision-host`, `chip-wasm-reasoner` | EXPERIMENT / PROOF, linked **unconditionally** because `chip-cli` hosts their subcommands. This is known debt (`technical-debt-register.md` D-01) |
| Optional features, not in the release build | `chip-local-ml` (`local-ml`), `chip-laya-reasoner` (`laya`) | EXPERIMENT; built only by manual workflows |
| **Production, durable state** | FeltDB (`feltdb`, vendored), through `chip-agent-state` only | PRODUCTION: every `work` and `serve` work is persisted in it by default (`agent-state.md`). No other product crate reaches it (`scripts/audit-dependencies.sh`) |
| **Not linked** | `chip-session-memory`, SQLite (`rusqlite`, `libsqlite3-sys`), `redb`, `durability` | EXPERIMENT. `cargo tree -p stack-agent -e normal -i <name>` finds none of them |

Nothing is a production *dependency* of an experiment's findings: `chip-session-memory` is a leaf (no
workspace crate depends on it; its only workspace dependency, `chip-core`, is a dev-dependency for
the benchmark's baseline arm). `scripts/audit-dependencies.sh` now enforces both directions: the
product crates it lists (today `chip-core`, `chip-remote-env`, `chip-inference`, `chip-fx`,
`chip-session`, `chip-turns`, `chip-project`, `chip-pax`, `stack-agent`) must not reach any of those engines, and nothing in the workspace may depend
on `chip-session-memory`. It was run for this change (section 8).

The shipped subcommand surface, beyond the five production commands, still includes the experiment
and proof subcommands (`chip init|graph|slice|impact|decision-state` and the `--test-*`,
`--benchmark-*`, `--evaluate-*`, `--report-*`, `--horizon-matrix`, `--utility-matrix` flags). They are
classified in `crates.md` section 5.1; none is production.

## 3. Binaries, features, build scripts

* Workspace `[[bin]]` targets (measured, `cargo metadata`): `chip` (**ships**), `chip-decision-corpus`
  (verifier for a checked-in corpus, run by CI, not shipped), `chip-local-decision-train` (trainer,
  not shipped). `cdylib`: `chip-wasm-decision` (Wasm artifact for the rejected local-decision
  experiment; not shipped).
* Benches: `chip-cli/benches/baseline.rs` (performance baseline; run by CI with `--quick`);
  `chip-session-memory/benches/{session_memory,service_retention}.rs` (experiment harnesses, not run by CI).
* Cargo features: `chip-cli`: `laya`, `local-ml` (off by default, off in the release build).
* Build scripts (`build.rs`): none in the workspace.
* Release: `scripts/package-release.sh` builds `cargo build --release -p chip-cli` with default
  features; `scripts/smoke-test.sh` exercises the packaged binary; one target (Linux x86_64) is
  exercised (`crates.md` section 7).

## 4. Test-only code and doubles (read; `product_path.rs` pins that product modules do not use them)

| What | Where | Used by production? |
| --- | --- | --- |
| `TestExecutor`, `ScriptedPolicy`, `TestLocalReasoner` | `chip-core` `src/lib.rs`, `src/work.rs`, `src/reasoning.rs` (public items in a production crate) | No. Used by tests, proof commands and benchmarks |
| In-memory/test model providers | `fx-core`, `chip-core` | No |
| Mock model endpoints, PAX shims, scripted model | `crates/*/tests/`, `fx-native-provider/tests/support`, `chip-cli/tests/coding_agent/` (`script.rs`, `script/*.edits`), `chip-cli/benches/baseline.rs`, `chip-session-memory/benches/service_retention.rs` | No |
| Fixture project | `chip-cli/tests/fixtures/courier/` (about 6,000 lines of Rust) | No |
| `work_demo::measurement_json` | `chip-cli/src/work_demo.rs` | **Yes, one coupling**: product code renders the canonical measurement through a module that lives with the demos (`crates.md` section 8; `technical-debt-register.md` D-03) |

## 5. Evaluation harnesses and reproducibility artifacts

| Artifact | What it establishes | Reproduce | Scope and limits |
| --- | --- | --- | --- |
| `crates/chip-cli/tests/coding_agent/` + [`coding-agent-evaluation.md`](coding-agent-evaluation.md) | A scripted-model coding lifecycle on a real project with real PAX, cargo and git | `cargo test -p chip-cli --test coding_agent` | **Scripted judgment, real reality.** Says nothing about any real model |
| `crates/chip-cli/benches/baseline.rs` + [`performance.md`](performance.md), [`performance-baseline.json`](performance-baseline.json) | Chip's own overhead around model and computer | `cargo bench -p chip-cli --bench baseline` | Dated measurements on one machine; not guarantees |
| `crates/chip-session-memory` (tests, benches) + [`session-memory-experiment.md`](session-memory-experiment.md), [`session-store-comparison.md`](session-store-comparison.md), `session-memory-bench.json`, `session-store-bench-{normal,stress}.json`, `service-retention.json` | **EXPERIMENT.** Whether a durable store helps session memory/recovery; outcome: no-go (section 6) | commands in `session-store-comparison.md` section 10 | Synthetic workload, one machine, process-kill not power-loss |
| `service-retention-bounded.json`, `crates/chip-session-memory/benches/service_retention.rs` | **Production verification of the retention fix**: finished-work count plateaus at the limit | `cargo build --release -p chip-cli && cargo bench --manifest-path crates/chip-session-memory/Cargo.toml --bench service_retention -- --limits 64,256,1000000` | The bench lives in the experiment crate because it is a harness around the real binary; the thing it verifies is production |
| `audit/hostile/` + [`hostile-autonomous-agent-audit.md`](hostile-autonomous-agent-audit.md), `audit-evidence/hostile-audit/` | **Evaluation infrastructure, outside the workspace.** An independent adversarial audit through the real binary with a *scripted adversary* (not a model); findings HA-01 to HA-21 feed the roadmap. **No real-model result exists** (blocked) | `audit/hostile/README.md` | Scripted-adversary evidence says nothing about model competence |
| `AGENTS.md` live runs (PR36-38), `.github/workflows/live-*.yml` | Live-model/Compute results | manual workflows | Not reproduced by default CI; raw outputs not stored |

## 6. The session-memory/store experiments: boundary and decision

Experimental, not production; no production crate depends on them (section 2). Findings, with labels:

* **Measured** (`session-store-comparison.md`): SQLite, redb, FeltDB and a `durability`-crate journal
  all satisfy one recovery contract on a synthetic workload; SQLite uses far less memory than FeltDB at
  the stress profile; redb shows no compelling advantage over SQLite; the journal detects injected
  byte-flips but makes Chip own a crash protocol; redb's default write mode loses all unsynced writes
  on SIGKILL.
* **Measured**: `Service.items` retained about 25 KB to 120 KB per finished work, linearly and without
  bound (before the fix).
* **Decision (recorded)**: do **not** introduce durable session storage until there is a concrete
  resume-after-restart requirement, a defined recovery contract and acceptance criteria
  (roadmap P3-01). If one appears, SQLite is the leading candidate (**proposed**, not validated beyond
  this experiment).
* **Not evaluated**: fjall, sled, power-loss durability, multi-process access.

Experimental storage engines are dependencies of the experiment crate only. Benchmarking an engine is
not a reason to adopt it.

## 7. Where each kind of truth lives

| Question | Canonical document |
| --- | --- |
| What is production, per crate? | [`crates.md`](crates.md) (inventory table is test-checked) |
| What can `chip work` do, and what are its gaps? | [`capabilities.md`](capabilities.md) |
| What is left before Chip is a production coding agent? | [`coding-agent-production-roadmap.md`](coding-agent-production-roadmap.md) (**the** backlog) |
| What technical debt and TODOs exist, and where do they go? | [`technical-debt-register.md`](technical-debt-register.md) |
| How well does the scripted lifecycle work end to end? | [`coding-agent-evaluation.md`](coding-agent-evaluation.md) |
| How much does Chip cost per call? | [`performance.md`](performance.md) |
| Where does project-structure observation belong? | [`project-observation-boundary.md`](project-observation-boundary.md) |
| What are the runtime's control, evidence and blockage contracts (design only)? | [`work-contract.md`](work-contract.md), [`evidence-ledger.md`](evidence-ledger.md), [`blockage-classifier.md`](blockage-classifier.md), [`micro-step-proposal-gate.md`](micro-step-proposal-gate.md); tickets RIC-01 to RIC-07 in the roadmap |
| Did a durable session store pay off? | [`session-store-comparison.md`](session-store-comparison.md) (and the earlier [`session-memory-experiment.md`](session-memory-experiment.md)) |
| Research constitution and invariants | `AGENTS.md` |

## 8. Verification snapshot for this change

Recorded in the pull request description and at the end of
[`technical-debt-register.md`](technical-debt-register.md) section 5, with the exact commands, the
results, and each check that could not run and why.
