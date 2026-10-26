"""Small shared helpers: hashing, JSON files, redaction, time, subprocess capture."""

from __future__ import annotations

import fnmatch
import hashlib
import json
import os
import re
import shutil
import subprocess
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
BENCH = REPO / "bench"


def now_ms() -> int:
    return int(time.time() * 1000)


def iso(ms: int | None = None) -> str:
    t = (ms if ms is not None else now_ms()) / 1000
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t))


def read_json(path: Path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def write_json(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(value, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp, path)


def append_jsonl(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps(value, sort_keys=True) + "\n")


def read_jsonl(path: Path) -> list:
    if not path.exists():
        return []
    out = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# Never part of a fixture, a checkout's scope, or a digest: build output and Stack-Agent's own
# project-local state.
IGNORED_DIRS = {".git", "target", ".stack-agent"}


def tree_files(root: Path) -> list[str]:
    """Every regular file under `root`, project-relative, sorted, skipping ignored directories."""
    out = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in IGNORED_DIRS)
        for name in filenames:
            p = Path(dirpath) / name
            if p.is_file() and not p.is_symlink():
                out.append(p.relative_to(root).as_posix())
    return sorted(out)


def tree_digest(root: Path) -> str:
    """sha256 over (path, content) of every file: the fixture digest."""
    h = hashlib.sha256()
    for rel in tree_files(root):
        h.update(rel.encode())
        h.update(b"\0")
        h.update(hashlib.sha256((root / rel).read_bytes()).digest())
    return h.hexdigest()


def tree_hashes(root: Path) -> dict[str, str]:
    return {rel: sha256_bytes((root / rel).read_bytes()) for rel in tree_files(root)}


def copy_tree(src: Path, dst: Path) -> None:
    """Copies every file with a fresh modification time. Cargo fingerprints by mtime and shares
    artifacts between copies of one workspace, so a preserved (older) mtime can make it reuse a
    build of different source."""
    for rel in tree_files(src):
        target = dst / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src / rel, target)
        shutil.copymode(src / rel, target)


def matches_any(path: str, patterns: list[str]) -> bool:
    return any(fnmatch.fnmatchcase(path, p) for p in patterns)


def run(cmd: list[str], cwd: Path | None = None, env: dict | None = None,
        timeout: float | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, env=env, timeout=timeout, capture_output=True, text=True)


# Secrets: every value of these variables, and anything shaped like a key, is replaced before
# anything is written.
SECRET_VARS = ("OPENAI_API_KEY", "BENCH_API_KEY", "AI_GATEWAY_API_KEY", "ANTHROPIC_API_KEY",
               "CHIP_API_KEY", "BAKEOFF_UPSTREAM_KEY")
KEY_SHAPES = re.compile(r"(sk-[A-Za-z0-9_\-]{12,}|vck_[A-Za-z0-9_\-]{12,}|Bearer\s+[A-Za-z0-9_\-\.]{12,})")


def redact(text: str) -> str:
    for var in SECRET_VARS:
        v = os.environ.get(var)
        if v and len(v) >= 8:
            text = text.replace(v, f"<redacted:{var}>")
    return KEY_SHAPES.sub("<redacted>", text)


def redact_value(value):
    """Redacts secrets anywhere in a JSON value, and replaces the host's absolute paths."""
    s = json.dumps(value)
    s = redact(s)
    for prefix in sorted({str(REPO), str(Path.home()), "/tmp"}, key=len, reverse=True):
        s = s.replace(prefix, "<host>")
    return json.loads(s)


def git_identity(path: Path) -> dict:
    """The commit and whether the tree differs from it, for a binary's source checkout."""
    head = run(["git", "rev-parse", "HEAD"], cwd=path)
    if head.returncode != 0:
        return {"commit": None, "dirty": None}
    dirty = run(["git", "status", "--porcelain", "--untracked-files=no"], cwd=path)
    return {"commit": head.stdout.strip(), "dirty": bool(dirty.stdout.strip())}
