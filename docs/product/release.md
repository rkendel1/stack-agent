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
| `--verify-attestation` / `gh attestation verify <archive> --repo rkendel1/stack-agent --signer-workflow rkendel1/stack-agent/.github/workflows/release.yml --source-ref refs/tags/v<version>` | GitHub's build-provenance attestation says this repository's release workflow produced the archive | that the source is free of defects; the executable is still unsigned |

See [Provenance](#provenance) for what is attested, what becomes public, and the difference between
the branch check and a release.

## Provenance

A build-provenance attestation is a signed statement, made by GitHub Actions for this repository's
release workflow, that an archive with a given SHA-256 was built by that workflow from a given commit
and ref. `actions/attest` creates it; **`gh attestation verify` is the only verifier used here**. No
verification is implemented in this repository: `scripts/verify-provenance.sh` runs `gh`, requires the
repository, signer workflow and git ref to be named, and additionally compares the attested digest with
the file's own SHA-256.

### What is attested, and what is public

Exactly three kinds of file, named explicitly in `subject-path` (an `actions/attest` step that omits
`subject-path` would attest whatever the runner lists, so `scripts/check-release-workflow.py` fails if
one does): the two release archives (`dist/stack-agent-*.tar.gz`) and the `dist/SHA256SUMS` manifest.
Never source archives, logs, environment dumps, credentials or other build output.

This repository is public, so the attestations are signed with Sigstore's **public-good** instance: a
short-lived certificate from Fulcio and an entry in the Rekor transparency log. Those records are
**public and cannot be deleted**. They contain: the file names and SHA-256 digests; this repository's
name and owner; the signer workflow path and the commit SHA; the git ref (a branch name for the dispatch
check, a tag for a release); the trigger (`workflow_dispatch` or `push`); the run URL; and the runner
environment (GitHub-hosted). They contain no file contents, no secrets and no log output. The first real
run's certificate fields are reproduced below from GitHub's own output.

### Branch check and release are different claims

| | `provenance-check` (dispatch) | `publish` (a pushed `v*` tag) |
| --- | --- | --- |
| Runs | only for `workflow_dispatch` | only for a pushed `v*` tag, after all validation passed |
| Identity pinned | repository, `release.yml`, `refs/heads/<branch>` | repository, `release.yml`, `refs/tags/<tag>` |
| Proves | this workflow can attest and verify build artifacts, and that the verifier rejects what it should | the published archives were built from that tag by this workflow |
| Does **not** prove | anything about a future release's identity | that the source is free of defects, or anything about signing |
| Can write contents | no (`contents: read`) | yes, to create a **draft** release only |

A successful branch attestation is not evidence for a release: the tag check pins `--source-ref
refs/tags/<tag>`, and the dispatch run demonstrates that a branch attestation is **rejected** by a tag
pin. To verify a release yourself:

```sh
gh attestation verify stack-agent-<v>-<target>.tar.gz --repo rkendel1/stack-agent \
  --signer-workflow rkendel1/stack-agent/.github/workflows/release.yml --source-ref refs/tags/v<v>
```

`install.sh --verify-attestation` runs exactly that. `SHA256SUMS` stays required either way: the
attestation shows who built an archive, the checksum shows the file is intact.

### What the dispatch job tests with the real tool

After attesting, `provenance-check` verifies every file by digest on GitHub, and requires `gh` to
**reject**, each for its own stated reason: a modified artifact (by lookup, and against the genuine
bundle); a different file, whose digest is not a subject of the attestation, offered with the genuine
bundle (digest); an unexpected repository; an
unexpected signer workflow; an unexpected git ref (what a tag pin does to a branch attestation); a file
never attested; a bundle with a corrupted signature; and a file that is not a bundle. Two positive
controls run first, so a rejection is not the environment failing. If any case is accepted, or any
control or verification fails, the job fails.

`actions/attest` makes **one** statement naming all the files it is given as subjects (here the two
archives and `SHA256SUMS`), so the genuine bundle for one archive also verifies the other two, rightly:
their digests are subjects. The first real run showed this: an earlier version of the digest case
offered the other archive with the bundle, `gh` accepted it, and the harness reported the acceptance as
a failure. The case was wrong, not the tool, and it now uses a file that is not a subject. The harness
prints the attested subjects, read from the bundle GitHub returns, so the scope is visible in every run.
`scripts/test-provenance.sh` tests only the wiring around `gh` with a stand-in (pins passed, failures
fail closed, digests compared, the harness notices a verifier that accepts what it should reject). It
does not show cryptographic rejection and is not offered as evidence of it.

### Evidence from the first real run

Run [39976159926](https://github.com/rkendel1/stack-agent/actions/runs/39976159926) (`workflow_dispatch`
on `claude/provenance-check`, commit `fb66c51`; the release job was skipped, and no tag or release
exists). `actions/attest` reported `Attestation created for 3 subjects`, signed with a certificate from
the Public Good Sigstore instance, uploaded to
<https://github.com/rkendel1/stack-agent/attestations/59129916> and logged in Rekor
(`search.sigstore.dev?logIndex=3420625588`). The certificate fields `gh` read back:

| Field | Value |
| --- | --- |
| issuer | `https://token.actions.githubusercontent.com` |
| trigger | `workflow_dispatch` |
| repository | `rkendel1/stack-agent` |
| workflow ref / source ref | `refs/heads/claude/provenance-check` (a branch, not a tag) |
| commit | `fb66c512197f5ef6230a31b884cf63e504f58f91` |
| signer | `https://github.com/rkendel1/stack-agent/.github/workflows/release.yml@refs/heads/claude/provenance-check` |
| runner | `github-hosted` |

The three subjects, whose digests were read from the bundle GitHub returned and matched the files'
own SHA-256: the macOS archive, the Linux archive and `SHA256SUMS`.

The same files, downloaded from the run and checked from outside Actions with `gh` 2.81 on a Mac:
the three files each verified (exit 0) pinned to the repository, `release.yml` and the branch ref.
Rejected (exit 1) were: a byte appended to the archive (no attestation for its digest, and
`verifying with issuer "sigstore.dev"` against the genuine bundle); an unrelated file with the genuine
bundle; the right bundle with `--repo rkendel1/pax` (`expected SourceRepositoryURI to be
https://github.com/rkendel1/pax, got https://github.com/rkendel1/stack-agent`); the right bundle with
`ci.yml` as the signer workflow; the branch attestation against a **tag** pin (`expected
SourceRepositoryRef to be refs/tags/v0.1.0, got refs/heads/claude/provenance-check`) and against
`refs/heads/main`; a never-attested file; a bundle with one signature byte flipped; and a file that
is not a bundle.

**A limit of that evidence.** For four of those rejections (altered bytes against the bundle, an
unrelated file, the wrong signer workflow, the flipped signature) `gh` 2.81 prints only the wrapper
`Error: verifying with issuer "sigstore.dev"`, not the specific mismatch. They are attributed by
controlled change: the control (genuine bundle, genuine artifact, genuine pins) exits 0, and each case
alters exactly one of those things and exits 1. The cause is therefore the one thing changed, but it
is inferred from that, not read from `gh`'s message.

**Public records left by validating.** This validation took three dispatch runs on this branch that
reached the attestation step (the first two failed later, in the negative cases, because of defects in
this harness that were then fixed). Each created attestations for its own archives and `SHA256SUMS`
under a branch identity. Those records, and their Rekor entries, are public and permanent, and none
is a release.

## The executable is not code-signed or notarized

Signing and notarization are deferred until a broader public release. Measured on the machine this
was written on (macOS 27.0, Apple Silicon, a local `cargo build --release`):

- `codesign -dv`: `Signature=adhoc`, flags `adhoc,linker-signed`, `TeamIdentifier=not set`. The
  toolchain's linker signs every arm64 executable ad hoc so that it can run at all.
- `spctl --assess --type execute`: `rejected`. Gatekeeper does not consider it a notarized program.
- An executable copied with `curl` (or fetched with `gh`) had no `com.apple.quarantine` attribute
  (macOS added `com.apple.provenance`), and it ran. The installer downloads with `curl`.
- The same GitHub-built executable with a `com.apple.quarantine` attribute written onto it by hand,
  as Safari writes it, was **killed on launch from a shell** (exit 137, no output). Removing that one
  attribute (`xattr -d com.apple.quarantine <file>`) made it run again. That is an emulation of a
  browser download on macOS 27.0, not a real browser download, and what a real "Open Anyway" prompt
  offers was not tried.

**What this means for users:** install with the installer (or `curl`/`gh`), not by downloading the
archive in a browser. If you did download in a browser, run `xattr -d com.apple.quarantine
<path to stack-agent>` yourself, after checking the archive against `SHA256SUMS`; that is your
decision about that one file, and nothing here disables Gatekeeper. Signing and notarization would
remove this step and are the thing to revisit before telling people outside the project to download
manually.

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
3. **`provenance-check`**, only for `workflow_dispatch`: attest the archives and `SHA256SUMS` under the
   branch identity and verify them, including the negative cases (see Provenance). No release command.
4. **`publish`**, only for a pushed tag and only if all of the validation jobs passed: attest the archives
   and `SHA256SUMS`, verify them pinned to the tag, and create a **draft** release with the archives, per-archive checksums,
   `SHA256SUMS` and `install.sh`. It is never published by the workflow. A person reads the draft and
   publishes it.

`scripts/check-release-workflow.py` (run in CI, with a self-test that injects defects) enforces:
every action pinned to a full commit SHA; the top-level `permissions: {}`; only `publish` can write
contents and only `publish` and `provenance-check` can sign; `provenance-check` is dispatch-only and
not a dependency of `publish`; no provenance step can be allowed to fail; attestation subjects are
named and limited to the release files; releases are drafts and tag-only; checkouts keep no
credentials.

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
