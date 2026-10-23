#!/usr/bin/env python3
"""Checks the properties of .github/workflows/release.yml that the release depends on.

    python3 -I scripts/check-release-workflow.py [workflow.yml]
    python3 -I scripts/check-release-workflow.py --self-test

It reads the file as text (no YAML library is assumed) and enforces:

  1. every `uses:` is pinned to a full 40-character commit SHA (a comment may name the tag);
  2. the top-level `permissions:` grants nothing (`{}`), and every job declares its own;
  3. only `publish` can write contents; only `publish` and `provenance-check` may hold `id-token` or
     `attestations`; every other job holds exactly `contents: read`;
  4. `publish` runs only for a pushed tag, and `workflow_dispatch` is a trigger (so the pipeline
     can run without publishing);
  5. a release is only ever created as a draft, only in `publish`, and never edited to publish;
  6. every checkout drops its credentials (`persist-credentials: false`);
  7. `provenance-check` runs only on workflow_dispatch, cannot touch a release, is not a dependency of
     `publish`, and no step of it or `publish` can hide a failure (`continue-on-error`);
  8. every `actions/attest` names its subjects, and only the release archives and SHA256SUMS: leaving
     `subject-path` out would make the action attest whatever the runner lists;
  9. `publish` verifies pinned to the tag (`refs/tags/v*`) and `provenance-check` to the branch ref,
     both with the repository and signer workflow, through scripts/verify-provenance.sh;
 10. the Linux glibc floor declared here equals the one in scripts/install.sh, and `publish` waits for
     the job that checks the installer refuses glibc below it.

`--self-test` applies one defect at a time to the real workflow and requires each to be caught.
It checks the text of a workflow; it does not prove GitHub will behave as the text says.
"""

import re
import sys
from pathlib import Path

DEFAULT = Path(__file__).resolve().parent.parent / ".github" / "workflows" / "release.yml"
SHA = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(/[A-Za-z0-9_./-]+)?@[0-9a-f]{40}$")
WRITE_ALLOWED = {
    "publish": {"contents": "write", "id-token": "write", "attestations": "write"},
    # Dispatch-only: may sign and store attestations, can never write contents.
    "provenance-check": {"contents": "read", "id-token": "write", "attestations": "write"},
}
ATTEST_SUBJECTS = {"dist/stack-agent-*.tar.gz", "dist/SHA256SUMS"}


def jobs(text):
    """Job name -> its block of lines, by indentation under `jobs:`."""
    lines = text.splitlines()
    start = next((i for i, l in enumerate(lines) if l == "jobs:"), None)
    if start is None:
        return {}
    blocks, name = {}, None
    for line in lines[start + 1 :]:
        m = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if m:
            name = m.group(1)
            blocks[name] = []
        elif name is not None:
            blocks[name].append(line)
    return blocks


def permissions(block):
    """The `permissions:` mapping of a job block, or None when it has none."""
    for i, line in enumerate(block):
        if re.match(r"^    permissions:\s*$", line):
            out = {}
            for nxt in block[i + 1 :]:
                m = re.match(r"^      ([a-z-]+):\s*([a-z]+)", nxt)
                if not m:
                    break
                out[m.group(1)] = m.group(2)
            return out
    return None


def attest_scope(block):
    """Problems with the `with:` inputs of each `actions/attest` step, read by indentation."""
    problems = []
    lines = [l for l in block if not l.lstrip().startswith("#")]

    def indent(l):
        return len(l) - len(l.lstrip())

    for i, line in enumerate(lines):
        if not re.search(r"uses:\s*actions/attest@", line):
            continue
        base = indent(line)  # the `uses:` key; sibling keys of the step share it
        inputs, current, in_with = {}, None, False
        for l in lines[i + 1 :]:
            if not l.strip():
                continue
            if indent(l) < base or (indent(l) == base and l.lstrip().startswith("- ")):
                break  # the next step
            key = re.match(r"^\s*([a-z-]+):\s*(.*)$", l)
            if indent(l) == base and key:
                in_with = key.group(1) == "with"
            elif in_with and key and indent(l) == base + 2:
                current = key.group(1)
                inputs[current] = [key.group(2)] if key.group(2) not in ("", "|") else []
            elif in_with and current is not None and indent(l) > base + 2:
                inputs[current].append(l.strip())
        if "subject-path" not in inputs:
            problems.append("actions/attest has no subject-path (it would attest whatever the runner lists)")
        other = set(inputs) & {"subject-digest", "subject-checksums", "sbom-path", "predicate", "predicate-path", "predicate-type"}
        if other:
            problems.append(f"attest uses {sorted(other)}; only subject-path is approved")
        subjects = set(inputs.get("subject-path", []))
        if "subject-path" in inputs and (not subjects or not subjects <= ATTEST_SUBJECTS):
            problems.append(f"attests {sorted(subjects - ATTEST_SUBJECTS) or 'nothing'} outside the approved release files")
    return problems


def check(text):
    problems = []
    for n, line in enumerate(text.splitlines(), 1):
        m = re.match(r"^\s*-?\s*uses:\s*(\S+)", line)
        if m and not SHA.match(m.group(1)):
            problems.append(f"line {n}: '{m.group(1)}' is not pinned to a full commit SHA")

    if not re.search(r"^permissions:\s*\{\}\s*$", text, re.M):
        problems.append("the top-level permissions must be `{}`")
    if not re.search(r"^  workflow_dispatch:", text, re.M):
        problems.append("workflow_dispatch must be a trigger, so the pipeline can run without a tag")

    blocks = jobs(text)
    for name in ("build", "linux-compat", "publish", "provenance-check"):
        if name not in blocks:
            problems.append(f"the workflow has no '{name}' job")
    for name, block in blocks.items():
        perms = permissions(block)
        if perms is None:
            problems.append(f"job '{name}' declares no permissions")
            continue
        expected = WRITE_ALLOWED.get(name, {"contents": "read"})
        if perms != expected:
            problems.append(f"job '{name}' has permissions {perms}, expected exactly {expected}")
        body = "\n".join(l for l in block if not l.lstrip().startswith("#"))
        if name not in ("publish",) and re.search(r"gh release (create|edit|upload|delete)", body):
            problems.append(f"job '{name}' touches a release; only 'publish' may")

    prov = "\n".join(l for l in blocks.get("provenance-check", []) if not l.lstrip().startswith("#"))
    if "provenance-check" not in blocks:
        problems.append("the workflow has no 'provenance-check' job")
    else:
        if not re.search(r"^    if:\s*github\.event_name == 'workflow_dispatch'\s*$", prov, re.M):
            problems.append("provenance-check must run only for workflow_dispatch")
        if re.search(r"ref_type|refs/tags|github\.ref_name", prov):
            problems.append("provenance-check must not look at or build on a tag identity")
        for needed in ("verify-provenance.sh verify", "verify-provenance.sh negative", "--signer-workflow", "--source-ref"):
            if needed not in prov:
                problems.append(f"provenance-check lacks `{needed}`")
    for name in ("provenance-check", "publish"):
        if re.search(r"continue-on-error", "\n".join(l for l in blocks.get(name, []) if not l.lstrip().startswith("#"))):
            problems.append(f"job '{name}' uses continue-on-error, which would let a failed provenance step pass")
    if re.search(r"needs:[^\n]*provenance-check", "\n".join(blocks.get("publish", []))):
        problems.append("publish must not depend on provenance-check (it is skipped on a tag)")
    # Attestation scope: explicit, and only the approved files.
    for name in ("provenance-check", "publish"):
        for problem in attest_scope(blocks.get(name, [])):
            problems.append(f"job '{name}': {problem}")
    pub = "\n".join(l for l in blocks.get("publish", []) if not l.lstrip().startswith("#"))
    if blocks.get("publish") is not None:
        if "verify-provenance.sh verify" not in pub or '--source-ref "$EXPECTED_REF"' not in pub or "--signer-workflow" not in pub:
            problems.append("publish must verify through verify-provenance.sh, pinned to repository, signer workflow and ref")
        if "refs/tags/v*" not in pub:
            problems.append("publish must refuse a ref that is not a tag before verifying")

    # Comments may talk about releases; only commands count.
    # The Linux glibc floor is declared in the workflow and in the installer; they must agree.
    declared = re.search(r'^  STACK_AGENT_GLIBC_FLOOR:\s*"([0-9.]+)"', text, re.M)
    installer = DEFAULT.parent.parent.parent / "scripts" / "install.sh"
    in_installer = re.search(r"^GLIBC_FLOOR=([0-9.]+)", installer.read_text(), re.M) if installer.exists() else None
    if not declared:
        problems.append("the workflow does not declare STACK_AGENT_GLIBC_FLOOR")
    elif not in_installer or in_installer.group(1) != declared.group(1):
        problems.append(
            f"the workflow's glibc floor {declared.group(1)} differs from scripts/install.sh "
            f"({in_installer.group(1) if in_installer else 'missing'})")
    if declared and not re.search(r"Supported: macOS on Apple Silicon and Linux x86_64 with glibc \$\{STACK_AGENT_GLIBC_FLOOR\} or newer", text):
        problems.append("the release notes template must state the glibc floor (via STACK_AGENT_GLIBC_FLOOR)")
    if "linux-floor" not in blocks:
        problems.append("the workflow has no 'linux-floor' job")
    if not re.search(r"needs:\s*\[[^\]]*linux-floor", "\n".join(blocks.get("publish", []))):
        problems.append("publish must wait for 'linux-floor'")

    publish = "\n".join(l for l in blocks.get("publish", []) if not l.lstrip().startswith("#"))
    if not re.search(r"^    if:\s*github\.ref_type == 'tag'\s*$", publish, re.M):
        problems.append("publish must run only for a pushed tag (`if: github.ref_type == 'tag'`)")
    creates = re.findall(r"gh release create[^\n]*(?:\\\n[^\n]*)*", publish)
    if len(creates) != 1:
        problems.append(f"publish must create exactly one release, found {len(creates)}")
    elif "--draft" not in creates[0]:
        problems.append("the release must be created with --draft")
    if re.search(r"--draft=false|gh release edit|--latest", text):
        problems.append("nothing may publish a draft release (found --draft=false, `gh release edit` or --latest)")

    for m in re.finditer(r"uses:\s*actions/checkout@\S+[^\n]*\n((?:\s+[a-z-]+:[^\n]*\n)*)", text):
        if not re.search(r"persist-credentials:\s*false", m.group(1)):
            problems.append("a checkout step does not set `persist-credentials: false`")
    return problems


def self_test(text):
    defects = {
        "an unpinned action": lambda t: t.replace(
            "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1", "actions/checkout@v7", 1),
        "a short SHA": lambda t: t.replace(
            "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1", "actions/checkout@3d3c42e", 1),
        "a writable build job": lambda t: t.replace(
            "    permissions:\n      contents: read\n    strategy:", "    permissions:\n      contents: write\n    strategy:", 1),
        "a build job with id-token": lambda t: t.replace(
            "    permissions:\n      contents: read\n    strategy:", "    permissions:\n      contents: read\n      id-token: write\n    strategy:", 1),
        "broad top-level permissions": lambda t: t.replace("permissions: {}", "permissions: write-all", 1),
        "a release created outside publish": lambda t: t.replace(
            "      - name: Package\n", "      - name: Package\n        run: gh release create x\n", 1),
        "publish on every ref": lambda t: t.replace("    if: github.ref_type == 'tag'\n", "", 1),
        "a non-draft release": lambda t: t.replace("--draft --verify-tag", "--verify-tag", 1),
        "a published draft": lambda t: t + "\n      - run: gh release edit v1 --draft=false\n",
        "credentials left in the checkout": lambda t: t.replace("persist-credentials: false", "persist-credentials: true", 1),
        "no manual trigger": lambda t: t.replace("  workflow_dispatch:", "  repository_dispatch:", 1),
        "a glibc floor that differs from the installer's": lambda t: t.replace(
            'STACK_AGENT_GLIBC_FLOOR: "2.34"', 'STACK_AGENT_GLIBC_FLOOR: "2.31"', 1),
        "release notes that drop the glibc floor": lambda t: t.replace(
            "with glibc ${STACK_AGENT_GLIBC_FLOOR} or newer", "(glibc)", 1),
        "provenance-check able to write contents": lambda t: t.replace(
            "      contents: read\n      id-token: write # Sigstore signing certificate for the attestations\n      attestations: write # store the attestations\n    env:",
            "      contents: write\n      id-token: write # Sigstore signing certificate for the attestations\n      attestations: write # store the attestations\n    env:", 1),
        "the build job able to sign": lambda t: t.replace(
            "    permissions:\n      contents: read\n    strategy:\n      fail-fast: false\n      matrix:\n        include:",
            "    permissions:\n      contents: read\n      id-token: write\n    strategy:\n      fail-fast: false\n      matrix:\n        include:", 1),
        "provenance-check reachable on a tag": lambda t: t.replace("    if: github.event_name == 'workflow_dispatch'\n", "", 1),
        "publish depending on provenance-check": lambda t: t.replace(
            "needs: [build, linux-compat, linux-floor]\n    runs-on: ubuntu-22.04\n    timeout-minutes: 15\n    permissions:\n      contents: write",
            "needs: [build, linux-compat, linux-floor, provenance-check]\n    runs-on: ubuntu-22.04\n    timeout-minutes: 15\n    permissions:\n      contents: write", 1),
        "an attestation with no named subjects": lambda t: t.replace(
            "          subject-path: |\n            dist/stack-agent-*.tar.gz\n            dist/SHA256SUMS\n", "", 1),
        "an attestation widened beyond the release files": lambda t: t.replace(
            "            dist/SHA256SUMS\n          create-storage-record: false", "            dist/SHA256SUMS\n            target/**\n          create-storage-record: false", 1),
        "a provenance failure that is allowed to pass": lambda t: t.replace(
            "      - name: Attest the release files\n", "      - name: Attest the release files\n        continue-on-error: true\n", 1),
        "tag verification without the ref pin": lambda t: t.replace('--source-ref "$EXPECTED_REF"', '--source-ref "refs/heads/main"', 1) if False else t.replace(
            "          case \"$EXPECTED_REF\" in refs/tags/v*) ;;", "          case \"$EXPECTED_REF\" in *) ;;", 1),
        "no negative provenance cases": lambda t: t.replace("verify-provenance.sh negative", "verify-provenance.sh verify", 1),
        "an unpinned action in provenance-check": lambda t: t.replace(
            "actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6 # v4.2.2", "actions/attest@v4", 1),
        "publish not waiting for the floor check": lambda t: t.replace(
            "needs: [build, linux-compat, linux-floor]\n    runs-on: ubuntu-22.04\n    timeout-minutes: 15\n    permissions:\n      contents: write",
            "needs: [build, linux-compat]\n    runs-on: ubuntu-22.04\n    timeout-minutes: 15\n    permissions:\n      contents: write", 1),
    }
    failed = False
    if check(text):
        print("the real workflow does not pass:", *check(text), sep="\n  ")
        return 1
    for name, mutate in defects.items():
        mutated = mutate(text)
        if mutated == text:
            print(f"SELF-TEST BROKEN: the defect '{name}' changed nothing")
            failed = True
        elif not check(mutated):
            print(f"SELF-TEST FAILED: '{name}' was not caught")
            failed = True
        else:
            print(f"caught: {name}")
    return 1 if failed else 0


def main(argv):
    if argv[1:] == ["--self-test"]:
        return self_test(DEFAULT.read_text())
    path = Path(argv[1]) if len(argv) > 1 else DEFAULT
    problems = check(path.read_text())
    if problems:
        print(f"{path}: {len(problems)} problem(s)")
        for p in problems:
            print("  -", p)
        return 1
    print(f"{path}: every action is pinned to a commit SHA; only 'publish' writes contents; only it and 'provenance-check' can sign; releases are drafts, for tags only")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
