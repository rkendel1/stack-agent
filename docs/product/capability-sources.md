# Capability sources: an experimental seam

**Status: experiment-only.** Nothing here is on a production path. `stack-agent work` cannot
select a configuration that names a capability source (the catalog refuses one that is not
experimental), and no capability source is registered in production. AppPort is not part of this
change; it is the first consumer this seam is designed to evaluate (see "What comes next").

## The question

Should Chip carry a new capability source, for example an application's discoverable
capabilities, through its work loop? AGENTS.md §14 says not without evidence. Today that evidence
cannot be produced, for a structural reason found while tracing the code:

| Layer | What exists | Where the gap is |
| --- | --- | --- |
| Detection | `ProjectProfile` (`autoconfig/profile.rs`): build systems, tests, tools. It reads no file contents and does not probe the network. | It cannot express "this project offers application capabilities". |
| Selection | autoconfig catalog: versioned `Configuration`s with a `Stage`, ranked from recorded verified outcomes by `resolve::decide` | A configuration can narrow the capabilities and pick a context. It cannot name a capability source. |
| What the model is told | `EscalationContextPolicy`, picked through the closed `ContextChoice` enum | No policy carries guidance for a capability source. |
| Capabilities | `CapabilityBackend` in a `CapabilitySet`. In production the set is fixed (`project_capability_set`) and `RunOverrides` only narrows it. Experiments add backends through `CapabilityFixture`. | **Both sides of an experiment share one allowlist and one set of fixtures.** No experiment can compare "with this source" against "without it". |
| Evaluation | `chip-experiments`: `Strategy` plus `CapabilityFixture`, run in replay, isolated or observe mode. The `autoconfig.change-context` experiment already runs catalog configurations as strategies. | It cannot vary capabilities by side. |

So the path for evaluating behavior exists (catalog configuration, experiment, recorded evidence,
promotion), but it cannot carry a new capability source. That limitation is independent of AppPort.

## The seam

The seam is the smallest change to the existing types. It adds no new registry, configuration
format or authority.

* **`CapabilitySource`** (`chip-experiments/src/plugin.rs`) is statically constructed by the host
  and registered by value in the existing `Registry` (`with_capability_source`). It contributes:
  * **detection**: `detect(&TaskFixture) -> Detection`, which is `Confirmed`, `NotDetected` or
    `Inconclusive`, each with its evidence or reason. Only `Confirmed` activates the source. A
    declared dependency without the source's own confirmation is `Inconclusive`, and
    `Inconclusive` is never reported as `NotDetected`.
  * **capabilities**: a `CapabilityFixture` (the same contract, so the same routing, allowlist,
    input validation and `Confinement` rules), plus a declared `Effect` per capability
    (`Observation`, `Consequential` or `Unknown`).
  * **guidance**: an optional `Guidance { path, sha256 }`. This is a reviewed instruction artifact
    addressed by its project-relative path and pinned by digest, never copied into Chip. It is read
    from the task's project. A missing artifact, a digest mismatch, or more than
    `Guidance::MAX_BYTES` leaves the source inactive, and the reason is recorded.
* **Per-side activation.** `Strategy::sources()` (default: none) names the sources a strategy runs
  with. Only that side gets the source's capabilities and guidance. The definition's
  `capabilities` stays the single authority: a source's capability must be on it, and a source
  whose capabilities are not on it is refused at resolution. Shared capabilities still come from
  fixtures, so a source only adds capabilities on the allowlist, for the side that names it, when
  detection confirms it.
* **Guidance enters context through the existing policy.** `GuidedContext` wraps the strategy's own
  `EscalationContextPolicy` and adds a bounded, labelled guidance section to what one escalation is
  told. It is a pure function, like every policy. Its id is `guided-v1`; the run record names the
  strategy (and so its own policy) and the guidance digest.
* **Boundaries, enforced at resolution:**
  * every source capability is declared with a confinement and an effect;
  * observe and isolated modes refuse `Unconfined` capabilities, as they do for fixtures;
  * outside replay, an `Effect::Unknown` capability is refused. An absent effect is unknown, never
    an observation.
* **Selection through the catalog.** `Configuration.sources` is empty for every current entry.
  `catalog::validate` refuses a configuration that names a source unless it is
  `Stage::Experimental`. `resolve::decide` excludes it unless the `Policy` provides that source, and
  `Policy::work()` provides none. `ConfigurationStrategy` passes a configuration's sources through,
  so an experiment evaluates the configuration exactly as the catalog defines it.
* **Evidence.** Each `RunRecord` carries a `SourceActivation` per named source: detection, whether
  the source was activated and why not, and the guidance digest and size. The report includes them.
  The existing fields (`executions_by_capability`, `failed_observations`, `model_calls`, `tokens`,
  criteria and verdict) are the measurements to compare.

What the seam does **not** add: runtime plugin loading, a capability catalog, a second
configuration system, a production composition change, a network or process capability, or any
AppPort code.

## AGENTS.md §14

1. **What hypothesis does it test?** That a capability source, activated only on confirmed
   detection and given with its reviewed guidance, changes verified completion, invalid or guessed
   capability requests, or unsupported completion claims, compared with the same tasks without it.
   This change tests a narrower prerequisite: the experiment layer can carry such a source to one
   side under Chip's existing authority rules, deterministically, and record what happened.
2. **What failure does it address?** Without it, the value of a new capability source cannot be
   measured in Chip: both sides of an experiment get the same capabilities, and no strategy can
   carry pinned guidance. Any integration would have to land in production unmeasured, which §14
   rules out.
3. **Why must it exist in Chip?** The measurement has to hold Chip's enforcement constant: the
   allowlist, confinement, the observation invariants, and refusing a completion its observations
   do not support. A higher-level agent integration (for example the AppPort skill in Claude Code)
   can only provide guidance; the AppPort skill says itself that it "is guidance, not enforcement".
   This answers why the *experiment* belongs in Chip. It does not establish that any particular
   source, AppPort included, belongs in Chip's production loop. That decision waits for the
   experiment's evidence, and if a higher-level integration already provides the benefit, the
   answer is no.
4. **What authority does it introduce?** None new:
   * capabilities execute under the existing allowlist, routing and confinement rules;
   * guidance influences the model and grants nothing;
   * detection decides only whether a named source activates.
5. **How is that authority constrained?**
   * Sources are registered statically.
   * Sources are versioned, and their guidance is digest-pinned.
   * The definition's allowlist is the ceiling.
   * Confinement and effect rules are checked at resolution.
   * Guidance is size-bounded.
   * Production cannot select a source-bearing configuration (validation and `Policy::work`), and
     `tests/product_path.rs` keeps `chip-experiments` off the `work` path.
6. **What measurement demonstrates its value?** A paired experiment, with the same tasks, criteria,
   limits and recorded model replies, where the baseline names no source and the candidate names
   one. It compares verdicts, failed or undeclared requests, executions by capability, model calls,
   tokens and guidance bytes. Deterministic (replay or recorded-model) results show the contract
   holds. They are not evidence of real-model effectiveness, which needs real-model paired runs.

## What comes next (not in this change)

* **An AppPort source in its own crate**, outside the product crates, evaluated in replay. Done as
  `crates/chip-appport-source`; record:
  `docs/research/agent-runtime-primitives/experiments/stack-agent/appport-source-r1/`.
  * A recorder outside Chip uses only the published `appport` CLI and `@appport/sdk`, pinned, to
    build, package and host an application, and talks to the host over its HTTP surface (discovery,
    the capability route, the usage API). The Rust crate reimplements no protocol: it reads what the
    host served and maps each capability within the operator's grant to a descriptor.
  * Model output never chooses a URL or a scope: in replay there is no URL, the recorder's origin is
    the operator's, and the grant is the operator's configuration of the source.
  * Recorded from a real host serving the AppPort Todos application, not from `@appport/testing`.
    Tested against a missing scope (not offered, the definition refused; the host's own 403), input
    the contract forbids (refused by Chip; the published host does not validate it), a host-side
    failure that ends the work incomplete, and evidence that does not match the host's records.
    Timeouts and missing resources need the live mode.
* **The same source over MCP**, against an existing campaign bridge (the Vikunja bridge served by
  its own pinned `@appport/mcp`): `chip_appport_source::mcp`; record:
  `docs/research/agent-runtime-primitives/experiments/stack-agent/appport-mcp-m1/`. Real models
  answered, wrote and met the application's refusals in replay; the record lists what is left
  (live mode, stateless replay, the bridge not serving MCP itself).
* **A live mode**: calling an operator-started host from inside a run. Every AppPort capability is
  `Unconfined` and `Consequential`, so this is new authority and needs its own case under
  `AGENTS.md` section 14, including how an interrupted call is recovered.
* **Real-model effectiveness**, through `--agent-bench` or once experiments support real models,
  with enough repetitions to separate an effect from model variance.
* **Promotion**, only on recorded evidence. Promoting would need a production composition change
  (production `work` cannot add a backend today) and a detection attribute in `ProjectProfile`, each
  justified by the experiment's result.
