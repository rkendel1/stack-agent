"""The hard-task suite: loading task definitions, building checkouts, and running test commands.

A task's starting repository is its fixture with the task's overlay applied (files replaced or added)
and its removals deleted, committed once with a fixed author and date, so the same task always
starts at the same commit. The commit id is the task's *repository revision*; the digest of the
files is its *fixture digest*.

Everything the evaluator alone may see (`acceptance/`, `reference/`) stays in this repository and is
never copied into a checkout the agent works in.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

from . import SUITE_SCHEMA
from .util import BENCH, copy_tree, read_json, run, tree_digest, tree_files

SUITE_ROOT = BENCH / "hard-tasks"

GIT_ENV = {
    "GIT_AUTHOR_NAME": "bakeoff",
    "GIT_AUTHOR_EMAIL": "bakeoff@invalid",
    "GIT_COMMITTER_NAME": "bakeoff",
    "GIT_COMMITTER_EMAIL": "bakeoff@invalid",
    "GIT_AUTHOR_DATE": "2026-10-26T00:00:00Z",
    "GIT_COMMITTER_DATE": "2026-10-26T00:00:00Z",
}

REQUIRED = (
    "schema", "id", "family", "difficulty", "split", "fixture", "kind", "goal", "acceptance",
    "budgets", "allowed_capabilities", "allowed_paths", "protected_paths", "expected_failure_modes",
    "safety", "verdict",
)
FAMILIES = {
    "trace-failing-test", "multi-module", "workspace-config", "preserve-contract",
    "misleading-first-cause", "tests-over-docs", "interpret-test-output", "unfamiliar-repo",
    "budgeted-multistep", "recover-failed-attempt", "resume-interrupted", "capability-unavailable",
    "misleading-history", "unsafe-or-impossible", "preserve-unrelated", "memory",
}
VERDICTS = {"change", "refusal", "safe_stop"}


@dataclass
class Task:
    id: str
    dir: Path
    spec: dict
    suite_version: str
    # A variant task (`"inherits": "<task>"`) takes its parent's overlay, acceptance tests and
    # reference fix unless it has its own.
    parent_dir: Path | None = None

    def part(self, name: str) -> Path:
        own = self.dir / name
        if own.exists() or self.parent_dir is None:
            return own
        return self.parent_dir / name

    def __getitem__(self, key):
        return self.spec[key]

    def get(self, key, default=None):
        return self.spec.get(key, default)

    @property
    def fixture_dir(self) -> Path:
        return SUITE_ROOT / self.suite_version / "fixtures" / self.spec["fixture"]


@dataclass
class Suite:
    version: str
    manifest: dict
    tasks: list[Task] = field(default_factory=list)

    def task(self, task_id: str) -> Task:
        for t in self.tasks:
            if t.id == task_id:
                return t
        raise KeyError(f"no task {task_id} in suite {self.version}")


def load_suite(version: str = "v1") -> Suite:
    root = SUITE_ROOT / version
    manifest = read_json(root / "suite.json")
    suite = Suite(version=version, manifest=manifest)
    for task_id in manifest["tasks"]:
        tdir = root / "tasks" / task_id
        spec = read_json(tdir / "task.json")
        parent_dir = None
        if spec.get("inherits"):
            parent_dir = root / "tasks" / spec["inherits"]
            parent = read_json(parent_dir / "task.json")
            spec = {**{k: v for k, v in parent.items() if k not in ("id", "split", "family", "tags")}, **spec}
        problems = check_spec(spec, tdir, parent_dir)
        if problems:
            raise ValueError(f"task {task_id}: " + "; ".join(problems))
        if spec["id"] != task_id:
            raise ValueError(f"task directory {task_id} holds task {spec['id']}")
        suite.tasks.append(Task(id=task_id, dir=tdir, spec=spec, suite_version=version, parent_dir=parent_dir))
    return suite


def check_spec(spec: dict, tdir: Path, parent_dir: Path | None = None) -> list[str]:
    def part(name: str) -> Path:
        own = tdir / name
        return own if own.exists() or parent_dir is None else parent_dir / name

    problems = [f"missing {k}" for k in REQUIRED if k not in spec]
    if problems:
        return problems
    if spec["schema"] != SUITE_SCHEMA:
        problems.append(f"schema {spec['schema']}")
    if spec["family"] not in FAMILIES:
        problems.append(f"unknown family {spec['family']}")
    if spec["split"] not in ("dev", "heldout"):
        problems.append(f"unknown split {spec['split']}")
    if spec["verdict"]["type"] not in VERDICTS:
        problems.append(f"unknown verdict {spec['verdict']['type']}")
    if len(spec["goal"].encode()) > 1000:
        problems.append("goal over 1000 bytes (the durable store refuses objectives over 1024)")
    for name in ("max_turns", "max_executions", "deadline_secs"):
        if not isinstance(spec["budgets"].get(name), int):
            problems.append(f"budgets.{name} missing")
    acc = spec["acceptance"]
    for src in acc.get("files", {}):
        if not part(src).is_file():
            problems.append(f"acceptance file {src} missing")
    if spec["verdict"]["type"] == "change":
        if not acc.get("files"):
            problems.append("a change task needs hidden acceptance tests")
        if not part("reference").is_dir():
            problems.append("a change task needs a reference fix")
    return problems


def apply_overlay(root: Path, overlay: Path, removals: list[str] = ()) -> None:
    if overlay.is_dir():
        copy_tree(overlay, root)
    for rel in removals:
        p = root / rel
        if p.is_dir():
            shutil.rmtree(p)
        elif p.exists():
            p.unlink()


def git(root: Path, *args: str) -> str:
    env = {**os.environ, **GIT_ENV}
    out = run(["git", *args], cwd=root, env=env)
    if out.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {out.stderr.strip()}")
    return out.stdout.strip()


def materialize(task: Task, dest: Path) -> dict:
    """Builds the task's starting repository at `dest` and commits it. Returns its identity."""
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    copy_tree(task.fixture_dir, dest)
    apply_overlay(dest, task.part("overlay"), task.get("remove", []))
    digest = tree_digest(dest)
    (dest / ".gitignore").write_text("/target\n/.stack-agent\n")
    git(dest, "init", "-q", "-b", "main")
    git(dest, "add", "-A")
    git(dest, "commit", "-q", "-m", f"{task.id}: starting state")
    return {"revision": git(dest, "rev-parse", "HEAD"), "fixture_digest": digest}


def reset_to(dest: Path, revision: str) -> None:
    """Returns the checkout to `revision` exactly; Stack-Agent's own project state is kept."""
    git(dest, "reset", "-q", "--hard", revision)
    git(dest, "clean", "-q", "-fd", "-e", ".stack-agent")


def install_acceptance(task: Task, tree: Path) -> list[str]:
    """Copies the hidden acceptance tests into `tree` (an evaluation copy, never the agent's)."""
    placed = []
    for src, dst in task["acceptance"]["files"].items():
        target = tree / dst
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(task.part(src), target)
        placed.append(dst)
    return placed


# ---- running a project's tests -------------------------------------------------------------------

RUNNING = re.compile(r"^\s*Running (?:unittests )?(\S+) \((?:.*[/\\])?([^/\\)]+)\)")
TEST_LINE = re.compile(r"^test (\S+) \.\.\. (ok|FAILED|ignored)")


def cargo_tests(tree: Path, target_dir: Path, timeout: float = 600) -> dict:
    """Runs every test target of the workspace (no doc tests) and parses each test's result.

    Returns {"compiled": bool, "exit": int, "tests": {"<binary>::<test>": "ok"|"FAILED"|"ignored"}}.
    """
    env = {**os.environ, "CARGO_TARGET_DIR": str(target_dir), "CARGO_NET_OFFLINE": "true",
           "CARGO_INCREMENTAL": "0", "CARGO_TERM_COLOR": "never", "RUST_BACKTRACE": "0"}
    try:
        # One stream, in order: cargo names each test binary on stderr and its results on stdout.
        out = subprocess.run(["cargo", "test", "--workspace", "--offline", "--tests", "--no-fail-fast"],
                             cwd=tree, env=env, timeout=timeout, stdout=subprocess.PIPE,
                             stderr=subprocess.STDOUT, text=True)
    except subprocess.TimeoutExpired as e:
        return {"compiled": False, "exit": None, "tests": {}, "error": f"{type(e).__name__}"}
    tests: dict[str, str] = {}
    binary = "?"
    for line in out.stdout.splitlines():
        m = RUNNING.match(line)
        if m:
            binary = re.sub(r"-[0-9a-f]{8,}$", "", m.group(2))
            continue
        t = TEST_LINE.match(line)
        if t:
            tests[f"{binary}::{t.group(1)}"] = t.group(2)
    compiled = "could not compile" not in out.stdout and not re.search(r"^error\[E\d+\]", out.stdout, re.M)
    if not tests and out.returncode != 0:
        compiled = False  # e.g. a manifest that does not load: nothing was built or run
    return {"compiled": compiled, "exit": out.returncode, "tests": tests,
            "error": None if compiled else _first_error(out.stdout)}


def _first_error(stderr: str) -> str:
    for line in stderr.splitlines():
        if line.startswith("error[") or line.startswith("error: could not compile"):
            return line[:200]
    return stderr.strip().splitlines()[-1][:200] if stderr.strip() else "unknown"


def acceptance_names(task: Task) -> set[str]:
    """The binaries the hidden acceptance tests compile to (integration test file stems)."""
    return {Path(dst).stem for dst in task["acceptance"]["files"].values()}


def split_results(task: Task, tests: dict[str, str]) -> tuple[dict, dict]:
    hidden = acceptance_names(task)
    acc = {k: v for k, v in tests.items() if k.split("::", 1)[0] in hidden}
    vis = {k: v for k, v in tests.items() if k.split("::", 1)[0] not in hidden}
    return acc, vis


def files_of(root: Path) -> list[str]:
    return tree_files(root)
