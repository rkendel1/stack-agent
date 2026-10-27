"""A stand-in `pax` the harness puts in front of the real one (through `PAX_BIN`).

It runs the real PAX and logs each invocation (arguments, start, end, exit status) to a JSONL file.
For fault injection it can hold the first test invocation after the harness arms its gate, in
which case it signals the harness and waits to be killed. With `before`, it holds before the real
test run starts, so the verification never happened. With `after`, it holds after the real run
finished, with the result unreported, so the action happened but its receipt was never recorded.

The log is the ground truth for whether a test run really happened, which the operator settlement
in a recovery uses.
"""

from __future__ import annotations

import json
import stat
from pathlib import Path

TEMPLATE = r'''#!/usr/bin/env python3
import json, os, subprocess, sys, time
real = os.environ["BAKEOFF_PAX_REAL"]
log = os.environ["BAKEOFF_PAX_LOG"]
mode = os.environ.get("BAKEOFF_PAX_MODE", "normal")
gate = os.environ.get("BAKEOFF_PAX_GATE", "")
args = sys.argv[1:]
# chip-pax runs the tests as `pax --dir DIR --json`, observes as `... observe`, and asks `--version`.
kind = ("version" if "--version" in args else "observe" if "observe" in args
        else "test" if "--dir" in args else "other")
is_test = kind == "test"
count_file = log + ".tests"
n = 0
if is_test:
    try:
        n = int(open(count_file).read() or 0)
    except Exception:
        n = 0
    n += 1
    open(count_file, "w").write(str(n))
entry = {"kind": kind, "test_index": n if is_test else None, "start_ms": int(time.time() * 1000)}
def note(**kw):
    entry.update(kw)
    with open(log, "a") as f:
        f.write(json.dumps(entry) + "\n")
# The harness arms the gate when it wants the next test run held (after the agent's first write).
hold = (is_test and mode in ("hold_before", "hold_after", "signal_after") and gate
        and os.path.exists(os.path.join(gate, "arm")) and not os.path.exists(os.path.join(gate, "held")))
if hold and mode == "hold_before":
    note(held="before", ran=False)
    open(os.path.join(gate, "held"), "w").write("before")
    while True:
        time.sleep(1)
p = subprocess.run([real] + args, capture_output=True)
if hold and mode == "hold_after":
    note(held="after", ran=True, exit=p.returncode, end_ms=int(time.time() * 1000))
    open(os.path.join(gate, "held"), "w").write("after")
    while True:
        time.sleep(1)
note(ran=True, exit=p.returncode, end_ms=int(time.time() * 1000), held="signal_after" if hold else None)
if hold and mode == "signal_after" and p.returncode == 0:
    # Report normally, and tell the harness the moment this process is gone.
    import atexit
    atexit.register(lambda: open(os.path.join(gate, "held"), "w").write("after_exit"))
sys.stdout.buffer.write(p.stdout)
sys.stderr.buffer.write(p.stderr)
sys.exit(p.returncode)
'''


def install(dest_dir: Path) -> Path:
    """Writes the wrapper as `dest_dir/pax` and returns its path."""
    dest_dir.mkdir(parents=True, exist_ok=True)
    path = dest_dir / "pax"
    path.write_text(TEMPLATE)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


def env(real: str, log: Path, mode: str = "normal", gate: Path | None = None) -> dict:
    out = {"BAKEOFF_PAX_REAL": real, "BAKEOFF_PAX_LOG": str(log), "BAKEOFF_PAX_MODE": mode}
    if gate:
        out["BAKEOFF_PAX_GATE"] = str(gate)
    return out


def invocations(log: Path) -> list[dict]:
    if not log.exists():
        return []
    return [json.loads(l) for l in log.read_text().splitlines() if l.strip()]
