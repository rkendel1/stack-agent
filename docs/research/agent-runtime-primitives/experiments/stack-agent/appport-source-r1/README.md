# AppPort source R1: AppPort capabilities in Chip's work loop, in replay

**Question.** Can Stack-Agent consume capabilities from an AppPort host through its existing
`CapabilitySource` hook, with the host remaining the authority for discovery, authorization,
execution and evidence?

**Answer (replay): yes.** A real host run by the **published** `appport@1.1.20` CLI was recorded
serving the AppPort Todos application. The new experiment-only crate `crates/chip-appport-source`
turns that recording into a Chip capability source. In Chip's ordinary work loop the (recorded) model
fixes a README link with the project capabilities, lists the todos through AppPort, completes the
matching one with the id the host returned, and the work is verified. Every AppPort observation is
checked against the host's own durable records. Missing scopes, an input the contract forbids, and a
failed call all end honestly. Live invocation and a live model choosing the capability are **not**
shown here (see "What replay does not show").

Nothing here is on a product path. `stack-agent work` cannot select a source, and no product crate
depends on the new crate (`scripts/audit-dependencies.sh`, `crates/stack-agent/tests/inventory.rs`).

## What was built

| Piece | Where | Role |
| --- | --- | --- |
| Recorder | `crates/chip-appport-source/recorder/record.mjs` | Operator-run. Installs the pinned published packages `appport@1.1.20` and `@appport/sdk@1.3.0` from npm, builds and packages the application with `appport app build` / `app package`, hosts it with `appport host add` / `host start`, and talks to the host only over HTTP: discovery, `POST /apps/todos/capabilities/<name>@<v>` with the grant in `x-appport-scopes`, and the host's usage API. Writes what the host returned, unmodified, plus its durable invocation and usage records. Records two hosts: one granted `todos.read todos.write`, one granted `todos.read` |
| Application | AppBoundry `examples/todos` (66-line `src/application.ts` and its pre-built `appport/wasm/application.wasm`, sha256 `e2e29af8…3ea`, pinned by the recorder) | The published CLI packages a pre-built AppPort-ABI WASM; it does not compile one. Package `sha256:984c4693…dae4`, contract `96de06a5…25d5` |
| Snapshots | `crates/chip-appport-source/fixtures/todos.{authorized,read-only}.snapshot.json` | Discovery, the package contract, every call and response, and the host's records |
| Source | `crates/chip-appport-source/src/lib.rs` | `AppPortSource` (a `CapabilitySource`), `Succeeded` and `EvidenceCorrelated` (criteria) |
| Tests | `crates/chip-appport-source/tests/replay.rs` | 15 tests; the project capabilities' answers are produced by the real `chip-project` backend |
| Evidence trail | [`evidence-trail.json`](evidence-trail.json) | Written by the trail test (`CHIP_APPPORT_TRAIL=<file>`) |

Why the Todos application and not the Markdown parser of `../capability-selection-s1/`: the published
`appport@1.1.20` runs only the AppPort JSON ABI. It has no Synapse skill ABI v2 (every one of the 302
ready Synapse modules), no MCP host and no `appboundry/evidence`; those exist only in unpublished
AppBoundry source carrying the same version number. Todos is the deterministic application the
published packages build, package and host unaided.

### Mapping the host's contract to Chip's

| Host | Chip | Rule |
| --- | --- | --- |
| discovery `protocol` | accepted only if `AppPort/host-discovery/1` | an unknown protocol builds no source |
| application `selector`, `status` | the source `appport.<selector>` | refused unless running, and unless the selector is a valid lower-case Chip id segment (never folded) |
| the package contract | used only if its `packageIdentity` and `contractFingerprint`, and the WASM digest, equal discovery's | the published host serves no schemas; the package's own contract, pinned by what discovery advertises, is the only source of them |
| each request capability | `appport.<selector>.<name>` (`appport.todos.todo.complete`) | offered only if discovery's `authorization` scopes for it are within the operator's grant; otherwise not offered, with the reason |
| `inputSchema.properties`, `required`, `maxLength` | `CapabilityInput`s, `required`, `max_input_bytes` (4 × characters) | string, integer and boolean only; any other type means not offered |
| (no effect is declared by the host) | `Effect::Consequential`, `Confinement::Unconfined` | not a provider label: the host is another process reached over the network (nothing here confines it), and it meters every call, some of which change application state |

The grant is the operator's: the source sends exactly those scopes, the model never supplies them,
and the host enforces them independently (a recorded 403). Because every capability is
`Unconfined`, resolution offers the source only in replay; isolated and observe mode refuse the
definition. The source's own backend executes nothing.

### Observations

A replayed AppPort answer is rendered like Chip's project capabilities, as a JSON first line and then
the host's result:

```text
{"capability":"appport.todos.todo.complete","appport_capability":"todo.complete@1","request":{"id":"todo_1"},"outcome":"success","http_status":200,"execution_unit":"exec_0f545a738f1fa4be","package_identity":"sha256:984c…","artifact_hash":"e2e2…","contract_fingerprint":"96de…"}
--- result ---
{"completed":true,"id":"todo_1","title":"Fix the broken link in README.md"}
```

## Results (tests in `crates/chip-appport-source/tests/replay.rs`)

The task: *"README.md links to a documentation page that does not exist. Point that link at the
existing page in docs/ that covers the same topic, change nothing else, then mark the matching item
complete in the project's Todos application."* The project holds a README linking `docs/setup.md`
(missing) and `docs/usage.md`, the pages `docs/install.md` and `docs/usage.md`, and an
`appport.binding.json` naming the selector and package it expects. Before any run, the operator
created two todos on the host (`todo_1` "Fix the broken link in README.md", `todo_2`); those setup
calls are recorded but never replayed. Both sides run Chip's default behavior: no local decision, so
every step is the model's.

| Required evidence | Shown by | Result |
| --- | --- | --- |
| **Discovery** through the real source | `the_package_contract_becomes_chip_capabilities_within_the_grant`, `detection_needs_the_project_binding_and_the_package_the_host_served`, `without_the_project_binding_the_source_stays_inactive` | Capabilities come from the host's discovery document and the contract it serves. Detection is `Confirmed` only when the project's binding names the package the host served; a different package is `Inconclusive`, no binding is `NotDetected` and the source stays inactive |
| **Contract** interpreted correctly | same, plus `a_contract_or_document_a_client_must_not_trust_builds_no_source`, `a_capability_chip_cannot_request_faithfully_is_not_offered_and_says_why`, `chip_enforces_the_contract_the_published_host_does_not` | `todo.complete`: one required input `id`, scope `todos.write`, consequential. A contract whose identity, fingerprint or artifact differs from discovery's builds no source. An input the contract forbids (`{"todo": …}`) is refused by Chip before replay. The published host itself did **not** validate it: it passed it to the module, which answered 404 "todo undefined not found" |
| **Invocation** of the actual host and WASM | the recorder; `the_evidence_trail_links_the_run_to_the_host_records` | The host executed the module during recording: `todo.list` (`exec_d1d5d3c759cf80f3`) and `todo.complete` (`exec_0f545a738f1fa4be`), each with a completed invocation record whose stored output equals the result the observation carried, and a success usage record |
| **Integration** in a run | `the_agent_uses_the_appport_capabilities_and_the_results_trace_to_the_host` | Candidate: `project.read` → `project.list` → `project.write` → `appport.todos.todo.list` → `appport.todos.todo.complete {"id":"todo_1"}` → `complete`; verdict `Verified`, all three criteria met, audit clean. The id completed is the one the host's list returned. Baseline (no source): the same fix is written, but the Todos requests run nothing (never declared to it), `appport.succeeded` is not met, and its completion is refused |
| **Evidence** traceable to the invocation | `the_evidence_trail_links_the_run_to_the_host_records`, `evidence_that_does_not_match_the_host_records_is_not_met` | Linked by value and digest: discovery → contract → descriptor → decision → executed request (`output_sha256`) → the recorded host answer → `execution_unit` → the host's invocation and usage records, same artifact and contract. A forged unit, another artifact, a different request, or a failure claimed as success is `NotMet` |
| **Authority**: disallowed operations rejected | `the_grant_decides_what_is_offered_and_says_why`, `a_grant_without_the_scope_offers_less_and_the_host_refuses_the_rest`, `a_live_mode_cannot_offer_the_host` | Granted `todos.read`, the source offers `todo.list` only, with a reason for the other three; a definition that needs `todo.complete` cannot be built (`UnknownCapability`). The host independently refuses the call (403, `missingScopes: ["todos.write"]`) and records nothing. Isolated mode refuses the unconfined capabilities |
| **Failure handling**: honest outcome | `a_failed_invocation_is_a_failed_observation_and_no_completion` | Completing `todo_99`: the host answers 404 "todo todo_99 not found" (and meters a failure). Chip records a failed observation, refuses the model's completion claims that follow, and the work ends incomplete |

Incidental: the first reply fixture sent `"inputs": {}` to `todo.list`, which declares no inputs.
Chip blocked the work (`AGENTS.md` invariant 3: an empty object is still an invocation member). The
fixture was wrong, and the request now carries no `inputs` member.

### What the published host does differently from AppBoundry's unpublished MCP host

These are findings about `appport@1.1.20`, recorded because they decide what Chip must do itself:

* **No input validation against the contract.** An input the schema forbids reaches the module. On
  this path Chip's validation, from the contract, is the only schema enforcement.
* **No execution unit on a failed call.** A 404 or 400 response has no `x-appboundry-execution-unit`
  header, although the host writes a failure usage record. A failure therefore cannot be linked to
  its usage record by id; `EvidenceCorrelated` checks it by absence (the host completed no invocation
  for that capability and request) and says so.
* **No schemas online.** Discovery and the control plane list capabilities and scopes but no input
  schemas, so the contract is read from the package that discovery's identity pins.
* **Scopes are caller-asserted on loopback** (`x-appport-scopes`, honored for a direct loopback
  request). The grant is therefore the operator's configuration of the source; a live mode must not
  let the model influence it.

## What replay does not show

* **Live invocation inside a run.** The WASM ran in the host during recording; the run replays the
  host's answers. Calling a host from inside a run would make an unconfined, consequential capability
  executable in some mode: new authority, which needs its own case under `AGENTS.md` section 14.
* **A live model choosing the capability.** The model's replies are recorded and identical for both
  sides. Selection and consumption are the recorded model's; what the run proves is that Chip offers,
  validates, routes, observes and verifies them under its normal rules. `chip-experiments` has no
  live model.
* **Recovery and retries.** Declared `Consequential`, an interrupted call would stay uncertain rather
  than be reissued, but replay cannot interrupt a host call. That belongs with the live mode, which
  must also handle the published host's missing execution unit on failures.
* **Effectiveness.** Whether a capability helps a model finish work is a separate question
  (`../capability-selection-s1/`).

## Reproducing

```sh
# The tests (no network, no model, no credential)
cargo test --locked -p chip-appport-source

# The evidence trail
CHIP_APPPORT_TRAIL=$PWD/docs/research/agent-runtime-primitives/experiments/stack-agent/appport-source-r1/evidence-trail.json \
  cargo test --locked -p chip-appport-source --test replay the_evidence_trail

# Re-recording (Node 22 and npm access; the application directory holds src/application.ts and
# appport/wasm/application.wasm, for example an AppBoundry checkout's examples/todos)
node crates/chip-appport-source/recorder/record.mjs /path/to/appboundry/examples/todos crates/chip-appport-source/fixtures
```

A re-recording produces new execution units and timestamps; the tests read them from the snapshot.
Packaging is deterministic: two independent builds gave the same package identity.
