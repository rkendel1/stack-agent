"""The experiment registry: every earlier experiment, as it was run, with its limitations.

One JSON file per experiment under `bench/registry/experiments/`. An entry preserves the original
setup, tasks, models, configuration, results and artifacts as their own documents report them, and
says how far they are comparable with anything else. Entries are historical baselines: nothing here
re-scores them. `registry validate` checks every entry's shape and that every path it cites exists;
`registry export` writes one JSONL row per entry.
"""

from __future__ import annotations

import json
from pathlib import Path

from . import REGISTRY_SCHEMA
from .util import BENCH, REPO, read_json

REGISTRY_DIR = BENCH / "registry" / "experiments"

STATUSES = {"completed", "superseded", "invalid", "harness-only", "not-run", "blocked", "not-found",
            "reproduced"}
AREAS = {"model-inference", "context-retrieval", "control-loop", "memory-workflows", "verification",
         "recovery-lifecycle", "performance", "coding-tasks", "other"}
REQUIRED = ("schema", "id", "title", "areas", "status", "summary", "setup", "results", "limitations",
            "artifacts", "reproduce", "comparability")


def entries() -> list[dict]:
    return [read_json(p) for p in sorted(REGISTRY_DIR.glob("*.json"))]


def validate_registry() -> list[str]:
    problems = []
    seen = set()
    for path in sorted(REGISTRY_DIR.glob("*.json")):
        try:
            e = read_json(path)
        except Exception as err:  # unreadable JSON
            problems.append(f"{path.name}: {err}")
            continue
        missing = [k for k in REQUIRED if k not in e]
        if missing:
            problems.append(f"{path.name}: missing {', '.join(missing)}")
            continue
        if e["schema"] != REGISTRY_SCHEMA:
            problems.append(f"{path.name}: schema {e['schema']}")
        if path.stem != e["id"]:
            problems.append(f"{path.name}: id {e['id']} differs from the file name")
        if e["id"] in seen:
            problems.append(f"{path.name}: duplicate id")
        seen.add(e["id"])
        if e["status"] not in STATUSES:
            problems.append(f"{path.name}: status {e['status']}")
        for a in e["areas"]:
            if a not in AREAS:
                problems.append(f"{path.name}: area {a}")
        for kind in ("artifacts", "harness", "docs"):
            for rel in e.get(kind, []):
                if not (REPO / rel).exists():
                    problems.append(f"{path.name}: {kind} path {rel} does not exist")
        if not e["limitations"] and e["status"] not in ("not-found",):
            problems.append(f"{path.name}: no limitations recorded")
    return problems


def export_registry(out: Path) -> None:
    rows = entries()
    out.parent.mkdir(parents=True, exist_ok=True)
    with open(out, "w", encoding="utf-8") as f:
        for e in rows:
            f.write(json.dumps(e, sort_keys=True) + "\n")
    print(f"{len(rows)} entries -> {out}")
