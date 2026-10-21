# Stack-Agent × LayerFS: architecture and experimental evaluation

Evaluated 2026-10-21. Evidence-gathering only: no production code was changed.

| Repository | Revision examined |
| --- | --- |
| `rkendel1/stack-agent` | `235524a8630967b96356b8d8c466a7d92f14d2f8` (branch `claude/determined-brahmagupta-iw1ui3`, clean tree) |
| `Ephemeral-AI-Lab/layerfs` | `5fb68ddc1acc3163e3ac3486133bde014c5fbed3` (`main`, 17 commits after tag `v1.0.0`) |

## A. Executive verdict

**Reject LayerFS. A simpler alternative is sufficient.**

LayerFS's useful ideas are cheaper to get from Git: isolated candidates, forking from a known checkpoint, and conflict-checked promotion. Its implementation does not fit Stack-Agent, for four reasons, each verified in source and three also measured:

1. **It cannot run project code without Docker, FUSE and root.** Stack-Agent's acceptance evidence is a real `pax test` run in a real directory. LayerFS has no export (materialise) to a host directory. The only way to execute anything is a FUSE mount inside a Docker Sandbox with `CAP_SYS_ADMIN`.
2. **Publish is overwrite-only.** It has no compare-and-swap against what `main` gained since the Branch's base. A concurrently published change is silently replaced: measured 10/10 times. Only `branch.merge` detects the conflict.
3. **There is no crash durability.** The only permitted profile is `Disposable`, which is WAL with `synchronous=OFF`. Garbage collection of history is not implemented, and restart recovery is not supported.
4. **The Store-level read API is text-only and windowed.** It refuses binary content and returns at most about 48 KiB or 2000 lines per call. It therefore cannot serve as the source of exact-byte promotion.

The measured advantage is real: forking two candidates takes 0.53 ms against about 1–2.7 s for copies, worktrees or clones. That advantage does not matter to Stack-Agent today. Stack-Agent runs one work per directory (`isolation_capacity() == 1`), and each work spends seconds to minutes on inference and `pax test`.

What Stack-Agent *does* lack is real, and LayerFS would not fix it:

- No file-level rollback.
- No isolation between attempts.
- No lost-update precondition on `project.write` (roadmap **P0-09**, still "missing").

The smallest fix is in Stack-Agent's own write path (§G).

## B. Source-verified findings

### Stack-Agent, current behaviour

**File operations**
- `chip-project` `ProjectExecutor` performs every file operation: `project.read/write/list/search` (`crates/chip-project/src/lib.rs:76-79`), plus four read-only Git observations (`git.rs:36-39`).
- There is no generic shell. Processes are started only for:
  - `git` with a fixed argv (`git.rs:92-210`)
  - `pax test` (`crates/chip-pax/src/lib.rs:834-839`)
  - pax observe
  - `compute`

**Authority over paths**
- The root is canonicalised once and held as a directory fd (`chip-project/src/lib.rs:256-269`). For the CLI it is `current_dir()` (`software_work.rs:2579`).
- Lexical rules apply (`components()`, `lib.rs:174-200`), and paths are walked with `openat(O_NOFOLLOW)` (`locate()`, `lib.rs:286-338`, `confine.rs`).

**Write path** (`write_with`, `lib.rs:876-948`)
- The write is atomic: temp file with `O_EXCL|O_NOFOLLOW`, then `sync_all`, `renameat`, and a directory fsync (`atomic_write`, `lib.rs:1514-1538`).
- Keyed writes are journaled (`journal.rs:189-250`).
- A byte-equal read-back is enforced (`readback_mismatch`, `lib.rs:935`).
- The observation carries `sha256` and `changed`.
- The previous content is read (`lib.rs:897-903`) but only used to compute `changed`. There is **no precondition**.

**Isolation**
- There is none at the file level.
- `LocalEnvironmentProvider::isolation_capacity() == 1` (`crates/stack-agent/src/local_environment.rs:143-145`). Its doc states: "Chip does not clone, branch or sandbox the directory" (`:10-12`).
- `chip-project` "does not sandbox the project" (`lib.rs:36-40`).
- A whole-directory `flock` excludes a second stack-agent process (`chip-project/src/lock.rs:63-88`). It does not exclude editors or other tools.
- Network, rlimit, namespace and seccomp restrictions are absent. `pax test` inherits the full environment.

**Durable state (FeltDB)**
- FeltDB is the crates.io crate `feltdb = "0.2.0"` (`crates/chip-agent-state/Cargo.toml:15`). Note that `production-boundary.md:74` calls it "vendored", which is wrong.
- `DurableExecutor::perform` (`chip-agent-state/src/lib.rs:3108-3189`) works like this:
  - It records the intent first. If that fails, nothing runs.
  - It records an observation and then an outcome.
  - On `OutcomeUnknown`, it leaves the intent open.
- `begin()` (`lib.rs:1365-1665`) reconciles open intents. It either re-observes `Effect::Observation` capabilities, reissues `Deduplicated` ones, or refuses to resume with `OutcomeUncertain`.
- `project.write` is `Consequential` + `Carried` (`chip-project/src/lib.rs:552-556`), so an interrupted write is never silently repeated. The write journal answers Committed / NotApplied / Unknown (`stack-agent/src/project_reconciler.rs`).
- **Nothing restores files.** The journal does not keep prior content. "Restoration" means replaying recorded observations (`lib.rs:681`).

**Retries**
- There is one model loop over the live directory: 12 turns and 8 executions by default (`software_work.rs:43-44`).
- A failed `pax test` or a bad edit is just another observation, and the next turn continues on the modified tree.
- `revert_and_retry` exists only as a shadow-mode nomination that is never executed (`micro.rs`).

**Verification**
- A work is verified when `VerifiedChange` holds: the last changing write is followed by `PaxTestPassed` (`software_work.rs:661-684`).
- `PaxTestPassed` requires PAX `status == passed` with `failed == 0, passed > 0`, regardless of exit code (`chip-pax/src/lib.rs:391-393, 527-545`).
- Two acceptance-time checks only *reject* results:
  - `tampered_test_files` (`software_work.rs:619-655`)
  - `stale_test_evidence` (project fingerprint, `:2287-2304`)

**Git**
- Git is observation-only: `status`, `diff`, `log` and `rev-parse` are allow-listed, and mutating subcommands are not expressible (`chip-project/src/git.rs:10-12, 69`).
- The product makes no commits.

**Documented gaps the evaluation confirms**
- Roadmap **P0-09**: a human edit made after Chip read a file is silently overwritten, and the run still ends `verified` (`docs/product/coding-agent-production-roadmap.md:174-179`).
- **P0-08**: the tree keeps a partial edit after a kill.
- **P0-06 / D-07**: no sandbox.
- `release-limitations.md`: "retrying a turn may repeat external effects".

**The existing integration boundary**
- `chip_core::environment::EnvironmentProvider` (`crates/chip-core/src/environment.rs:100-111`) defines `isolation_capacity`, `acquire(work)` and `release`.
- Any isolating workspace (worktree, copy or LayerFS) would plug in **here**, and no new interface is needed.

### LayerFS, as implemented

**Store and content model**
- One SQLite file holds every object, Branch, Commit and Layer (`core/docs/architecture/84-global-sqlite-store.md`).
- Objects are content-addressed with BLAKE3 (`layerfs-content/src/object/id.rs`). Files under 128 KiB are whole objects; larger files are CDC-chunked into 8/16/32 KiB pieces. Packs are zstd-compressed and append-only.

**Fork, publish and merge**
- Fork is a metadata row: tested, and measured below.
- Publish is "Overwrite-only: the new Layer sits on the stack head, whatever main gained since the Branch's base" (`layerfs-persistence/src/history/layerstack.rs:318-319`; `layerfs-store-service/src/history/branch.rs:7, 570`).
- `branch.merge` is a three-way merge that refuses `Conflict` and creates nothing. Binary files take "ours" without a conflict marker (`81-compare-and-merge.md`).
- `main` is read-only except through publish (measured: `fs.write` to `main` → `ReadOnly`).

**Access and platform**
- Store calls (`fs.*`, `branch.*`) run in-process, with no daemon. They are Linux-only on ext4, xfs or btrfs (`layerfs-store-service/src/kernel_guard.rs`).
- `workspace.mount/exec` requires Docker: a Sandbox container with `CAP_SYS_ADMIN`, `/dev/fuse` and `apparmor=unconfined` (`layerfs-sandbox/src/backend/docker/container.rs`), FUSE with `allow_other`, and the root-only Docker volumes directory.
- macOS is supported only through DSH's Linux container. No Windows support was found.
- There is no export or materialise operation, even though `docs/general/concepts.md` describes one.

**Isolation**
- Branch heads are independent, and each Workspace has a private overlay.
- `exec` runs `bash -c` via a Docker exec. Up to 16 Workspaces share one container, and no network restriction is set.
- No test of Workspace-to-Workspace isolation was found.

**Durability**
- Init and install refuse any profile other than `Disposable` (`layerfs-api/src/project/init.rs:98`; `layerfs-persistence/src/backend/sqlite/profile.rs:19-23`, where `synchronous = 0`).
- FUSE `fsync` returns ENOSYS by design.
- The README says "no crash or power-loss durability".
- Restart recovery is not supported.
- The upstream SIGKILL test (`layerfs-persistence/tests/sqlite_recovery.rs`) uses the default Durable profile, not the one that ships.

**Reads, growth and release status**
- `fs.read` is a UTF-8 text window of at most 2000 lines per call (`layerfs-store-service/src/read/window.rs:12`). Binary content is refused.
- History GC is not implemented, so the Store only grows (84 §8).
- The `v1.0.0` commit bumps version strings only, and its message says "No build or test run". The repository has no CI (`core/README.md`), and `docs/roadmap/1.0/README.md` still lists open release decisions.

## C. Hypothesis results

| | Status | Evidence | Limitation | Cost | Acceptance criterion |
| --- | --- | --- | --- | --- | --- |
| **H1** cheap isolated forks | **Partially supported** | Measured: two forks in 0.53 ms median (n=10), against `cp -a` 1.70 s, `git worktree` 2.03 s and `git clone --shared` 2.68 s. Contamination: 0/10 in every mechanism. | A LayerFS fork is not a directory, so `pax test` cannot run in it without Docker+FUSE. Init re-imports every byte (0.90 s for 2,113 entries). | Docker, FUSE, root and a 199 MB build | Two candidates that are runnable directories with 0 contamination over n≥10. Worktrees already pass this. |
| **H2** checkpoint and restore | **Supported, for files only** | Fork from a Commit id restored the exact checkpoint in 0.21 ms, with later files absent, 10/10. Git commit plus `reset --hard`/`clean -fd` restored in about 24 ms, 10/10. | Restores bytes only. Says nothing about whether `pax test`, the network or any external effect happened. Under `Disposable` there is no power-loss guarantee. | as H1 | Restored tree digest equals the checkpoint digest, 10/10 |
| **H3** explicit promotion without overwriting concurrent changes | **Unsupported, as LayerFS ships it** | `branch.publish` returned `Added` and replaced a concurrently published user change: 10/10 silent overwrites. `branch.merge` refused `Conflict` 10/10. A disjoint merge succeeded 10/10. With Git, `git apply --check` of the candidate diff against the changed user tree refused 30/30 and the user change was kept 30/30. | LayerFS can only detect conflicts inside its Store. It never compares against the host directory after import, and nothing promotes to the host. | You would build promotion yourself | A concurrent user edit is refused, never overwritten, 10/10 (Git: met) |
| **H4** content addressing and dedup | **Supported for storage; irrelevant at this scale** | Store after Init is 5.18 MB for a 25 MB tree, and 5.75 MB after all trial changes. Two worktrees take 40.3 MB on disk plus 0.78 MB of `.git` growth. | No GC, so the Store only grows. The relevant scale (one work per directory) does not need it. | — | Storage of N retained versions vs N worktrees, measured at the N Stack-Agent actually keeps (today: 0) |
| **H5** execution isolation | **Unsupported** | The source shows a shared Docker container per 16 Workspaces, `bash -c` exec, no network policy, and seccomp only for mount truthfulness. The Store API isolates filesystem state only. | Process, network and resource isolation would still need a sandbox, which is roadmap P0-06. | — | Not applicable to LayerFS |
| **H6** failure recovery | **Partially supported** | SIGKILL of a writer process, 20/20: every acknowledged write survived, the last file had the exact expected size, the Store reopened in about 4–6 ms and accepted writes. An in-flight unacknowledged write was simply absent. | This was a process kill, not a crash or power-loss test. `Disposable` explicitly disclaims power loss. Uncertain-outcome reconciliation still belongs to FeltDB intents and the write journal. | — | Same test under a durable profile plus a VM or power-cut harness. Not attempted. |
| **H7** better task completion | **Not tested** | No end-to-end run. See §D. | Nothing in the evidence links fork speed to model success. Stack-Agent's documented failure modes (P0-08, P0-09) are about rollback and preconditions, not fork latency. | — | Verified-completion rate and contamination incidents, A/B on the agent-bench ladder with an isolating provider |

## D. Experimental results

### Environment
- Linux 6.18 (Firecracker VM), 4 vCPU, 15 GiB RAM, ext4, running as root.
- `/dev/fuse` is present, but there is no `fusermount` and no Docker daemon.
- Rust 1.97.0. LayerFS declares 1.85, and the build succeeded on 1.97.

### Setup
- LayerFS was cloned and built into the session scratch directory. Its own `core/.cargo/config.toml` was used, and no global configuration was touched.
- The fixture was `git archive` of stack-agent `235524a`: 2,113 entries, 25 MB.
- The target file was `crates/chip-core/src/lib.rs`.

### LayerFS route used
This is the in-process Store API: `layerfs_api::call` over a `StoreService` on an installed `Disposable` Store, the same harness as upstream's `tests/support/sdk.rs`. No daemon, FUSE or Docker was involved. `workspace.mount/exec` could not be tested because no Docker daemon was available, and starting one would mean changing system configuration.

The harness is [`experiment/stackagent_eval.rs`](experiment/stackagent_eval.rs). It was placed at `core/crates/layerfs-api/tests/` in a disposable LayerFS clone only.

### Baselines
[`experiment/baseline.py`](experiment/baseline.py) ran on a scratch clone of stack-agent. It never touches the user's checkout.

The current Stack-Agent behaviour (in place, no workspace) costs nothing to create and has no isolation by construction. It is listed for reference and was not run as a mechanism.

### Commands
```sh
CARGO_TARGET_DIR=$SCRATCH/target-layerfs cargo test --release \
  --manifest-path core/Cargo.toml -p layerfs-api --test stackagent_eval --no-run
TMPDIR=$SCRATCH/tmp EVAL_SOURCE=$SCRATCH/fixture/src EVAL_RUNS=10 \
  $BIN --ignored --exact eval_trials --nocapture --test-threads=1
TMPDIR=$SCRATCH/tmp $BIN --ignored --exact eval_binary_probe --nocapture
TMPDIR=$SCRATCH/tmp EVAL_RUNS=20 $BIN --ignored --exact eval_kill_parent --nocapture --test-threads=1
python3 -I experiment/baseline.py $SCRATCH 10
```

### Measured results
Values are medians, with [min–max]. Raw lines are in [`results/`](results/).

| Metric | LayerFS Store (n=10) | `cp -a` (n=10) | `git worktree` (n=10) | `git clone --shared` (n=10) |
| --- | --- | --- | --- | --- |
| Create base | Init 897 ms [745–1115], once per project | — | — | — |
| Create 2 candidates | 0.53 ms [0.47–0.95] | 1696 ms [370–2260] | 2031 ms [1735–2308] | 2683 ms [2566–3012] |
| First read | 2.45 ms | 0.12 ms | 0.10 ms | 0.10 ms |
| Exact write | 12.7 ms (one Commit) | 3.0 ms (tmp + fsync + rename) | 2.9 ms | 2.5 ms |
| Read-back with digest compare | 2.2 ms, BLAKE3 equal 10/10 | 0.42 ms, SHA-256 equal 10/10 | 0.42 ms | 0.42 ms |
| Checkpoint | implicit (each write is a Commit) | 135 ms (`git commit -a`) | 167 ms | 172 ms |
| Restore | 0.21 ms (fork from Commit) | 25 ms (`reset --hard` + `clean`) | 24 ms | 28 ms |
| Cleanup of all candidates | 2.2 ms (the whole Store) | 246 ms | 243 ms | 292 ms |
| Storage | 5.18 MB Store; +0.58 MB after changes | 50.8 MB for 2 | 40.3 MB for 2, +0.78 MB `.git` | 50.8 MB for 2 |
| Isolation failures | 0/10 | 0/10 | 0/10 | 0/10 |
| Failed attempt, next attempt clean | 10/10 | 10/10 | 10/10 | 10/10 |
| Concurrent user change, promotion | publish **silently overwrote** 10/10; merge refused 10/10 | `git apply --check` refused 10/10, user change kept 10/10 | same | same |
| Residual files after cleanup | 0 | 0 | 0 | 0 |

### Other probes
- **Binary file (4 KiB, bytes 0–255):** `fs.stat` size was correct, but `fs.read` refused it with `Binary` ("text contains NUL or invalid UTF-8").
- **5.5 MB text file:** one `fs.read` returned 49,115 bytes (lines 1–893). Reading the rest requires paging.
- **Write to `main`:** refused with `ReadOnly`.
- **SIGKILL** (n=20; a child process writes 64 KiB commits and acknowledges each one, and the parent kills it after acknowledgement k):
  - The acknowledged value survived 20/20.
  - The size was exact 20/20.
  - The Store reopened 20/20, with median reopen about 4.7 ms.
  - Writes after reopen succeeded 20/20.
  - This is a process-kill result only. It is not a crash- or power-loss-durability result.

### Limitations
- Each sample came from one machine; variance is shown, and no significance claims are made.
- Baseline timings include `git` process start-up.
- The LayerFS numbers cover the Store API only. They exclude mount/exec and the cost of the Docker Sandbox, which is the path a coding task would actually need.
- The baseline promotion check uses a copy of the source tree to stand in for a dirty user checkout.

### End-to-end coding-agent comparison: not run
1. There is no LayerFS arm that can execute: no Docker daemon, and no export to a directory where `pax test` could run.
2. Stack-Agent has no isolating `EnvironmentProvider` to compare with.
3. The model, prompt and policy would be identical in both arms, so any difference would come from the filesystem rather than from model behaviour, and the filesystem-level tests already show it.

No inference was performed and no model credentials were used.

## E. Architecture recommendation

The responsibilities stay where they are:

| Concern | Owner |
| --- | --- |
| Planning, retries, acceptance | Chip |
| Durable intents, outcomes and reconciliation | FeltDB |
| Version control and reviewable changes | Git |

If isolation is needed later, the boundary already exists: `EnvironmentProvider::acquire/release` (`chip-core/src/environment.rs:100-111`). A Git-worktree provider would:

1. On `acquire`: create a worktree at the recorded HEAD and record the base digest in the work's FeltDB record.
2. Run the work and `pax test` there.
3. On acceptance: promote with a 3-way check against the *current* user tree. Refuse on conflict and never force.
4. On `release`: remove the worktree.

Some guarantees stay out of scope for every mechanism:

- None of this proves or reverses external effects.
- Restoring files is not reconciliation. Intents whose outcome is unknown still go through FeltDB `begin()` and the write journal.

## F. Decision table

| Idea | Decision | Concrete benefit | Evidence | Cost | Acceptance criterion |
| --- | --- | --- | --- | --- | --- |
| LayerFS as a workspace provider | **Reject** | Sub-ms forks | §D H1; needs Docker, FUSE and root; no export | High: daemon, privileged container, 199 MB build, no GC, `Disposable` only | — |
| LayerFS `publish` for promotion | **Reject** | — | Silent overwrite 10/10; overwrite-only by design (`layerstack.rs:318`) | — | — |
| Base-digest precondition on writes (P0-09) | **Adopt** (next step) | Stops a concurrent human edit from being silently overwritten while the run ends `verified` | Roadmap P0-09 is "missing"; `write_with` already reads the previous bytes (`lib.rs:897`) | Small: one optional input and one failure code in `chip-project` | The roadmap's `c1` scenario ends with the external edit intact, and the model sees a `stale_base` failure |
| Git-worktree `EnvironmentProvider` (isolated candidate, refuse-on-conflict promotion) | **Defer**, pending a measurement | Attempts that do not contaminate each other or the user tree; file rollback | Worktree 0/10 contamination and 10/10 conflict refusal in §D; P0-08 partial-edit-after-kill | Medium: one provider, about 1 s per work for 1.8k files | Agent-bench shows contamination or partial-edit incidents in the in-place mode at a rate worth about 1 s per work |
| Content-addressed dedup of retained versions | **Reject** for now | Storage for many retained snapshots | 5.2 MB vs 40 MB, but Stack-Agent retains none | — | Revisit only if retaining more than about 10 snapshots per work becomes a requirement |
| Checkpoint per write | **Defer** (Git commits in a worktree give it) | Restore to last verified state | H2 | Part of the worktree provider | Restored digest equals the checkpoint digest |

## G. Next step

The smallest action that can be reviewed on its own is to **implement roadmap P0-09 as a write precondition**. It does not depend on LayerFS.

| | |
| --- | --- |
| Repository | `rkendel1/stack-agent` |
| Branch | a new branch, not this one, with a separate PR only when authorised |
| Scope | `crates/chip-project/src/lib.rs` `write_with` and the `project.write` input schema |

**Change**
- Add an optional `expected_sha256` input: the digest of the content the model read. `project.read` already reports this digest (`chip-project/src/lib.rs:823`).
- Compare it with the `previous` bytes that `write_with` already reads.
- On a mismatch, refuse with `stale_base` before staging.
- A missing file must match an explicit "absent" expectation.

**Tests**
- `chip-project/tests/project.rs`: a matching digest writes; a mismatched digest refuses and leaves the file untouched; an absent-expected file that now exists refuses.
- The `c1` scenario from the hostile audit.

**Acceptance**
- The external edit is intact.
- The run does not end `verified` on a stale write.
- All existing `chip-project` and `stack-agent` tests pass.
- The residual TOCTOU window between the precondition read and the `renameat` is documented. Closing it fully needs an advisory lock, and the exact rule is left to the PR.

## Files in this directory

| File | What it is |
| --- | --- |
| `README.md` | this report |
| `experiment/stackagent_eval.rs` | the LayerFS Store-API harness; disposable, runs only inside a LayerFS checkout |
| `experiment/baseline.py` | the copy, worktree and clone baselines |
| `results/layerfs-trials.log`, `results/layerfs-sigkill.log`, `results/baseline-trials.log` | raw output |
