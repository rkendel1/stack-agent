# Releases, installation and `doctor`

How `stack-agent` is built into release artifacts, how to install and verify one, how to diagnose a
machine before running work, and what is not claimed. Everything here is about distribution. None
of it changes what the runtime does: the model still has no authority, and `doctor` has none either.

## Supported platforms

| Platform | Target triple | Status |
| --- | --- | --- |
| macOS, Apple Silicon | `aarch64-apple-darwin` | built and tested by `release.yml` |
| Linux x86_64, **glibc 2.34 or newer** (Ubuntu 22.04+, Debian 12+) | `x86_64-unknown-linux-gnu` | built on Ubuntu 22.04; see the measurement below |

**The Linux floor was measured, and it is not what was first assumed.** The executable built in an
`ubuntu:22.04` container imports `GLIBC_2.34` symbols. Run unchanged (in amd64 containers on an Apple
Silicon Mac, so emulated, not on a GitHub runner):

| Image | glibc | Result |
| --- | --- | --- |
| `ubuntu:22.04` | 2.35 | `--version`, `--test` and `doctor` work |
| `debian:12-slim` | 2.36 | works |
| `ubuntu:24.04` | 2.39 | works |
| `debian:11-slim`, `ubuntu:20.04` | 2.31 | **does not run** (`GLIBC_2.32`/`2.33`/`2.34` not found) |
| `debian:10-slim`, `ubuntu:18.04` | 2.28 | **does not run** |

So the declared floor is glibc 2.34. `release.yml` holds the build to it (it fails if the executable
ever needs a newer glibc), runs the artifact on the images that should work, and checks that the
installer refuses the ones below it. A lower floor means building on an older base image or against
an older glibc; that is a separate decision, as is musl.

Not supported yet: Linux arm64 (after validation), macOS Intel, Windows, and musl-based Linux
(Alpine). The installer refuses them before it downloads anything. A musl build is a separate
investigation.

## Install

```sh
curl -fsSL https://github.com/rkendel1/stack-agent/releases/latest/download/install.sh | sh
stack-agent --version
stack-agent doctor
```

To read the installer first: download `install.sh`, read it, then `sh install.sh`. Options:
`--version X.Y.Z`, `--force`, `--dry-run`, `--verify-attestation`. Environment:
`STACK_AGENT_INSTALL_DIR` (default `~/.local/bin`), `STACK_AGENT_SHA256`.

What the installer does, in order:

1. Works out the platform and stops, before any download, on an unsupported one (including a Linux
   whose glibc is older than 2.34, or that uses musl).
2. Downloads `SHA256SUMS` and the one archive for this platform from the same release.
3. Verifies the archive's SHA-256 **before extracting anything**. A mismatch installs nothing.
4. Rejects an archive whose members are absolute paths or contain `..`, extracts it into a private
   temporary directory, and requires the manifest's target and version and the executable's own
   `--version` to agree.
5. Installs one file into `~/.local/bin`. It refuses to replace an existing `stack-agent` unless
   `--force` is given; `--force` replaces it with a rename in the same directory. A directory at the
   destination is never replaced.

It never uses `sudo`, edits a shell profile (it tells you the `export PATH=...` line if the
directory is not on `PATH`), installs a dependency, reads your project or credentials, runs
`doctor`, or touches stack-agent's durable state. Tested by `scripts/test-install.sh`.

Uninstall: delete `~/.local/bin/stack-agent`. State stays where `stack-agent doctor` reports it.

## What verification proves

| Check | Shows | Does not show |
| --- | --- | --- |
| `SHA256SUMS` (the installer always does this) | the archive is intact and is the one the release lists | who published the release: the list comes from the same place as the archive |
| `STACK_AGENT_SHA256=<digest>` | the archive is the one whose digest you obtained somewhere else | that the digest you were given is right |
| `--verify-attestation` / `gh attestation verify <archive> --repo rkendel1/stack-agent` | GitHub's build-provenance attestation says this repository's release workflow produced the archive | that the source is free of defects; the executable is still unsigned |

The pipeline attests the archives with `actions/attest` and checks the attestation with
`gh attestation verify` in the same job. As of this change no release has been built, so no
attestation exists yet and none is claimed.

## The executable is not code-signed or notarized

Signing and notarization are deferred until a broader public release. Measured on the machine this
was written on (macOS 27.0, Apple Silicon, a local `cargo build --release`):

- `codesign -dv`: `Signature=adhoc`, flags `adhoc,linker-signed`, `TeamIdentifier=not set`. The
  toolchain's linker signs every arm64 executable ad hoc so that it can run at all.
- `spctl --assess --type execute`: `rejected`. Gatekeeper does not consider it a notarized program.
- An executable copied with `curl` had no `com.apple.quarantine` attribute (macOS added
  `com.apple.provenance`). The installer downloads with `curl`. A file fetched by a browser is
  quarantined and can be blocked by Gatekeeper until you allow it; that was **not** tested here, so
  do not assume either way for browser downloads.

The release workflow records the same two commands in each macOS run's summary, so the behavior of
the actual artifact is on record. Revisit signing before telling people outside the project to
install it.

## `stack-agent doctor`

```sh
stack-agent doctor [--kind change|verify|inspect] [--dir DIR] [--no-network] [--json]
                   [--provider P] [--model M] [--endpoint URL] [--state-db FILE | --no-state]
```

It diagnoses the environment `work` would run in. It is not an agent run. It does not select a model
or provider, send the model anything, validate a key, install or repair anything, run a project's
code, or write to the project.

Each check is **required** (`work` would refuse to start, or could never complete, without it),
**advisory** (`work` starts regardless: a `warn`), or **not required** for the kind of work you asked
about (never reported as missing). Verdicts come from the same code `work` uses to decide whether it
can start, so they cannot drift; `crates/stack-agent/tests/doctor.rs` compares them with `work`.

| Check | Required for | If not satisfied |
| --- | --- | --- |
| `binary` | always | never fails |
| `model`: provider, model, endpoint, and a key if the endpoint needs one | always | `fail`, `config_missing` (nothing selected; prints how to select one) or `config_invalid` |
| `endpoint`: a TCP connection to the endpoint's host and port, opened and closed with no bytes sent | always, unless `--no-network` | `fail`, `endpoint_unreachable`. `unverified` with `--no-network`, and when `CHIP_PROXY` is set (`work` goes through the proxy; `doctor` neither tests it nor connects around it) |
| `git` | never: only the optional `project.git.*` capabilities use it | `warn`, `dependency_missing` |
| `project`: the directory exists and the project capabilities are available | always | `fail`, `project_unsupported` |
| `pax`: located, then identified with `pax --version`, version checked | `change`, `verify` | `fail`, `dependency_missing` or `dependency_unsuitable`. `not_required` for `inspect` |
| `verification`: a `Cargo.toml` is present | `change`, `verify` (informational) | `unverified`, `project_unsupported`: whether PAX can verify the project is PAX's decision |
| `state`: the durable-state directory | only with `--state-db` | `warn`, `environment` (`work` runs and says it was not persisted); `fail` with `--state-db` |

Exit status follows the existing contract and adds nothing: **0** no required check failed, **2**
usage, **3** a required prerequisite is unavailable and nothing was done. The category (missing
configuration, unreachable endpoint, missing dependency, unsupported project) is in the output, in
`--json` as `category`, not in the exit code. `--json` is `stack-agent.doctor.v1`.

### What a passing `doctor` does not mean

It does not show the key is accepted, that the model exists, or that the endpoint speaks the
protocol: no request is made. `stack-agent "Say hello"` makes one call to check that, and you run it
deliberately. It does not show the tests can run: it does not ask PAX whether the project is
supported.

### Why `doctor` never runs `pax --dry-run`

PAX's `--dry-run` is documented as a preview, but its project detection runs `cargo metadata` on the
project. Tested while writing this: in a project whose `rust-toolchain.toml` named a toolchain that
was not installed, `pax --dry-run --json test` made `rustup` download and install that toolchain.
That is project-controlled input causing a download and a write outside the project, so `doctor`
runs only `pax --version`, as `work` does. (The toolchain that test installed was removed again.)

### On macOS, `/usr/bin/pax` is not PAX

macOS ships an unrelated POSIX archive utility named `pax`. Without PAX installed, `work` locates
that file, starts, and refuses `pax.test` only when it first needs it. `doctor` identifies it,
reports `dependency_unsuitable`, names the file for what it is, and tells you to put PAX ahead of
`/usr/bin` on `PATH`.

## PAX

`stack-agent` does not install PAX. `change` and `verify` complete only through a passing `pax.test`.
Get PAX 0.4.1 or newer from <https://github.com/rkendel1/pax/releases> (each release lists
`SHA256SUMS`), and put it on `PATH`. `--kind inspect` does not need it.

Open questions, not decided in this change:

- Library integration is not possible today: the crate's only public entry is the CLI in-process
  (`pax::run(args) -> Result<String, CliError>`), and `chip-pax` deliberately treats PAX as an
  independent executable and checks its identity. Embedding it would change that boundary.
- Bundling an executable is permitted by PAX's MIT license (the same owner), but PAX's release
  builds are not shown to be reproducible, and the licenses of its dependencies (`serde`,
  `serde_json`, `syn`, `proc-macro2`) have not been audited. Audit them before any bundling decision.
- The PAX minimum is stated as 0.4.1 here and in the README, but the code enforces two minimums:
  `pax.test` needs 0.3.0 (`MIN_PAX_VERSION`) and `project.observe` needs 0.4.1
  (`MIN_OBSERVE_PAX_VERSION`). `doctor` checks what `pax.test` enforces, which is what `change` and
  `verify` need to complete. Neither is changed here; see the tracking issue.

## How a release is made

`.github/workflows/release.yml`, on a pushed tag `v<version>`, or by hand with `workflow_dispatch`
(which builds and tests but cannot publish):

1. **`build`** (read-only repository access), for each target: build, package, verify the checksum,
   run `test-package.sh` and `test-install.sh`, smoke-test the **packaged artifact** (extract, serve,
   run a work, run `doctor`), install the artifact with `install.sh` and run it, and check the
   glibc requirement against the declared floor (Linux) or record the code signature (macOS).
2. **`linux-compat`** and **`linux-floor`**: run the Linux artifact unchanged in `ubuntu:22.04`,
   `debian:12-slim` and `ubuntu:24.04`, and check that `install.sh` refuses `debian:11-slim` and
   `ubuntu:20.04` before downloading anything. What passes is the supported set; nothing is claimed
   for an image that is not listed.
3. **`publish`**, only for a pushed tag and only if all of those passed: attest the archives, verify the
   attestations, and create a **draft** release with the archives, per-archive checksums,
   `SHA256SUMS` and `install.sh`. It is never published by the workflow. A person reads the draft and
   publishes it.

`scripts/check-release-workflow.py` (run in CI, with a self-test that injects defects) enforces:
every action pinned to a full commit SHA; the top-level `permissions: {}`; only `publish` can write;
releases are drafts and tag-only; checkouts keep no credentials.

Tag rule: the tag must equal `v<stack-agent crate version>`; `package-release.sh` stops otherwise.
To cut a pre-release such as `v0.1.0-rc1`, first bump the crate version to `0.1.0-rc1` in a release
commit (the executable reports it), then tag that commit. A version containing `-` is marked a
pre-release.

Archives are **deterministic**: the same executable and commit give a byte-identical archive
(sorted entries, no owners, fixed times and modes, no gzip name or time). That is tested. The
**executable** is not claimed to be reproducible from source; `workflow_dispatch` with
`measure_reproducibility` builds twice and reports whether the two are identical, as a measurement
that never gates a release.

Not done by this pipeline: code signing, notarization, Homebrew (a formula follows the first
published release, in a separate tap repository, because it needs a stable URL and checksum),
Linux arm64, macOS Intel, Windows.
