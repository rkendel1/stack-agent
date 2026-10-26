"""Independent verification of a finished trial: the harness's verdict, never the agent's.

The final tree is copied out of the agent's checkout, the hidden acceptance tests are added to the
copy, and every test target is run. The verdict compares that run with the task's frozen
validation: the acceptance tests the reference fix passes, and the visible tests that passed at the
start (a visible test that passed at the start and does not now is a regression). Scope is checked
against the task's allowed and protected paths. Nothing the agent reported is an input.
"""

from __future__ import annotations

import shutil
from pathlib import Path

from .suite import SUITE_ROOT, Task, cargo_tests, git, install_acceptance, split_results
from .util import copy_tree, matches_any, read_json, sha256_bytes

MAX_DIFF_BYTES = 60_000


def validation_for(task: Task) -> dict:
    path = SUITE_ROOT / task.suite_version / "validation.json"
    data = read_json(path)
    return data["tasks"][task.id]


def changed_files(repo: Path, start_revision: str) -> dict[str, str]:
    """{path: status} for every file that differs from the starting revision (A, M, D)."""
    out: dict[str, str] = {}
    git(repo, "add", "-A", "--intent-to-add", ".")
    for line in git(repo, "diff", "--name-status", "--no-renames", start_revision).splitlines():
        if not line.strip():
            continue
        status, path = line.split("\t", 1)
        out[path] = status[0]
    return out


def diff_text(repo: Path, start_revision: str) -> tuple[str, dict]:
    text = git(repo, "diff", "--no-renames", "--no-color", start_revision)
    added = removed = 0
    for line in git(repo, "diff", "--numstat", "--no-renames", start_revision).splitlines():
        parts = line.split("\t")
        if len(parts) == 3 and parts[0].isdigit() and parts[1].isdigit():
            added += int(parts[0])
            removed += int(parts[1])
    stats = {"added_lines": added, "removed_lines": removed, "digest": sha256_bytes(text.encode())[:16],
             "bytes": len(text.encode())}
    if len(text.encode()) > MAX_DIFF_BYTES:
        text = text.encode()[:MAX_DIFF_BYTES].decode(errors="ignore") + "\n[truncated]\n"
        stats["truncated"] = True
    return text, stats


def verify(task: Task, repo: Path, start_revision: str, rundir: Path) -> dict:
    v = validation_for(task)
    changed = changed_files(repo, start_revision)
    diff, stats = diff_text(repo, start_revision)
    allowed = task["allowed_paths"]
    protected = task["protected_paths"]
    out_of_scope = sorted(p for p in changed if not matches_any(p, allowed))
    protected_hit = sorted(p for p in changed if matches_any(p, protected))
    result = {
        "changed_files": changed, "diff_stats": stats, "diff": diff,
        "out_of_scope": out_of_scope, "protected_changed": protected_hit,
    }
    verdict = task["verdict"]["type"]
    if verdict != "change" and not changed:
        result.update(tests_run=False, acceptance=None, regressions=[], visible_unfixed=[], compiled=None)
        return result
    evaldir = rundir / "eval"
    if evaldir.exists():
        shutil.rmtree(evaldir)
    evaldir.mkdir()
    copy_tree(repo, evaldir)
    if verdict == "change":
        install_acceptance(task, evaldir)
    res = cargo_tests(evaldir, rundir / "target-eval")
    shutil.rmtree(rundir / "target-eval", ignore_errors=True)
    acc, vis = split_results(task, res["tests"])
    expected_acc = v["reference"]["acceptance"]
    base_visible = v["start"]["visible"]
    regressions = sorted(k for k, s in base_visible.items() if s == "ok" and vis.get(k) != "ok")
    # Visible tests the reference fix makes pass that did not pass at the start: the task's own
    # failing tests. Not regressions; still required.
    unfixed = sorted(k for k, s in v["reference"]["visible"].items()
                     if s == "ok" and base_visible.get(k) != "ok" and vis.get(k) != "ok")
    acceptance = None
    if verdict == "change":
        missing = sorted(k for k in expected_acc if k not in acc)
        failing = sorted(k for k, s in acc.items() if s != "ok")
        acceptance = {
            "passed": res["compiled"] and not missing and not failing and len(acc) >= len(expected_acc),
            "total": len(expected_acc), "passing": sum(1 for s in acc.values() if s == "ok"),
            "failing": failing, "missing": missing,
        }
    result.update(tests_run=True, compiled=res["compiled"], compile_error=res.get("error"),
                  acceptance=acceptance, regressions=regressions, visible_unfixed=unfixed,
                  visible_failing=sorted(k for k, s in vis.items() if s == "FAILED"))
    return result


def validate_task(task: Task, workdir: Path) -> dict:
    """Freezes the facts a task's verdict rests on: at the start the acceptance tests fail; with the
    reference fix every acceptance test and every visible test passes."""
    from .suite import apply_overlay, materialize
    repo = workdir / "repo"
    ident = materialize(task, repo)
    # A change made after the task's history was recorded is part of the trial's starting state.
    if task.get("history", {}).get("mutate_after"):
        apply_overlay(repo, task.part(task["history"]["mutate_after"]), task["history"].get("remove_after", []))
    facts: dict = {"revision": ident["revision"], "fixture_digest": ident["fixture_digest"]}
    for name, overlay in (("start", None), ("reference", task.part("reference"))):
        tree = workdir / f"tree-{name}"
        if tree.exists():
            shutil.rmtree(tree)
        tree.mkdir()
        copy_tree(repo, tree)
        if overlay is not None and overlay.is_dir():
            apply_overlay(tree, overlay)
        if task["verdict"]["type"] == "change":
            install_acceptance(task, tree)
        res = cargo_tests(tree, workdir / f"target-{name}")
        acc, vis = split_results(task, res["tests"])
        facts[name] = {"compiled": res["compiled"], "error": res.get("error"),
                       "acceptance": acc, "visible": vis}
        shutil.rmtree(workdir / f"target-{name}", ignore_errors=True)
    problems = []
    if task["verdict"]["type"] == "change":
        s, r = facts["start"], facts["reference"]
        if s["compiled"] and s["acceptance"] and all(x == "ok" for x in s["acceptance"].values()):
            problems.append("every acceptance test already passes at the start")
        if not r["compiled"]:
            problems.append(f"the reference fix does not compile: {r['error']}")
        elif not r["acceptance"] or any(x != "ok" for x in r["acceptance"].values()):
            problems.append("the reference fix fails acceptance: "
                            + ", ".join(k for k, x in r["acceptance"].items() if x != "ok"))
        bad = [k for k, x in r["visible"].items() if x == "FAILED"]
        if bad:
            problems.append("the reference fix fails visible tests: " + ", ".join(bad))
        regress = [k for k, x in s["visible"].items() if x == "ok" and r["visible"].get(k) != "ok"]
        if regress:
            problems.append("the reference fix regresses: " + ", ".join(regress))
    facts["problems"] = problems
    facts["valid"] = not problems
    return facts
