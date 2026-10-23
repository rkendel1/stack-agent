#!/usr/bin/env python3
"""Checks the properties of .github/workflows/release.yml that the release depends on.

    python3 -I scripts/check-release-workflow.py [workflow.yml]
    python3 -I scripts/check-release-workflow.py --self-test

It reads the file as text (no YAML library is assumed) and enforces:

  1. every `uses:` is pinned to a full 40-character commit SHA (a comment may name the tag);
  2. the top-level `permissions:` grants nothing (`{}`), and every job declares its own;
  3. only the `publish` job can write: `build` and `linux-compat` hold exactly `contents: read`,
     and `publish` holds exactly `contents: write`, `id-token: write`, `attestations: write`;
  4. `publish` runs only for a pushed tag, and `workflow_dispatch` is a trigger (so the pipeline
     can run without publishing);
  5. a release is only ever created as a draft, only in `publish`, and never edited to publish;
  6. every checkout drops its credentials (`persist-credentials: false`);
  7. the Linux glibc floor declared here equals the one in scripts/install.sh, and `publish` waits for
     the job that checks the installer refuses glibc below it.

`--self-test` applies one defect at a time to the real workflow and requires each to be caught.
It checks the text of a workflow; it does not prove GitHub will behave as the text says.
"""

import re
import sys
from pathlib import Path

DEFAULT = Path(__file__).resolve().parent.parent / ".github" / "workflows" / "release.yml"
SHA = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(/[A-Za-z0-9_./-]+)?@[0-9a-f]{40}$")
WRITE_ALLOWED = {"publish": {"contents": "write", "id-token": "write", "attestations": "write"}}


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
    for name in ("build", "linux-compat", "publish"):
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
        if name != "publish" and re.search(r"gh release (create|edit|upload|delete)", body):
            problems.append(f"job '{name}' touches a release; only 'publish' may")

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
        "publish not waiting for the floor check": lambda t: t.replace(
            "needs: [build, linux-compat, linux-floor]", "needs: [build, linux-compat]", 1),
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
    print(f"{path}: every action is pinned to a commit SHA; only 'publish' can write; releases are drafts, for tags only")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
