#!/usr/bin/env python3
"""License gate for rust/fx-native-provider.

Every crate in the resolved dependency graph (normal and build edges) on each supported native
target must carry a license expression that can be satisfied with permissive licenses only.
`OR` lets any one alternative suffice; `AND` requires every part. A crate with no license, or a
license outside ALLOWED, fails the gate. Standard library only; reads `cargo metadata`.

    python3 check-licenses.py
"""

import json
import re
import subprocess
import sys
from pathlib import Path

ALLOWED = {
    "Apache-2.0",
    "MIT",
    "ISC",
    "BSD-3-Clause",
    "BSD-2-Clause",
    "Unicode-3.0",
    "Zlib",
    "Unlicense",
    "CDLA-Permissive-2.0",
}

# The native platforms the release builds for. The graph is resolved per target.
TARGETS = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
]


def satisfied(expression: str) -> bool:
    tokens = re.findall(r"\(|\)|[A-Za-z0-9.+-]+", expression)
    position = 0

    def peek():
        return tokens[position] if position < len(tokens) else None

    def take():
        nonlocal position
        position += 1
        return tokens[position - 1]

    def atom() -> bool:
        token = take()
        if token == "(":
            value = either()
            take()  # ")"
            return value
        return token in ALLOWED

    def both() -> bool:
        value = atom()
        while peek() == "AND":
            take()
            value = atom() and value
        return value

    def either() -> bool:
        value = both()
        while peek() == "OR":
            take()
            value = both() or value
        return value

    return either()


def main() -> int:
    root = Path(__file__).resolve().parent
    problems: list[str] = []
    seen: dict[str, str] = {}
    for target in TARGETS:
        out = subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target],
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        meta = json.loads(out)
        nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
        packages = {p["id"]: p for p in meta["packages"]}
        # Only normal and build edges count; dev edges would not ship.
        reachable, todo = set(), [meta["resolve"]["root"]]
        while todo:
            pid = todo.pop()
            if pid in reachable:
                continue
            reachable.add(pid)
            for dep in nodes[pid]["deps"]:
                if any(k["kind"] in (None, "build") for k in dep["dep_kinds"]):
                    todo.append(dep["pkg"])
        for pid in reachable:
            package = packages[pid]
            seen[f'{package["name"]} {package["version"]}'] = package["license"] or ""
    for name, expression in sorted(seen.items()):
        if not expression:
            problems.append(f"{name}: no license expression")
        elif not satisfied(expression):
            problems.append(f"{name}: {expression} is not satisfiable with the allowed licenses")
    if problems:
        print("license gate failed:", *problems, sep="\n  ", file=sys.stderr)
        return 1
    print(f"licenses ok ({len(seen)} crates across {len(TARGETS)} targets)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
