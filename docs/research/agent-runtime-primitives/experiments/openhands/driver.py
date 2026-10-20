"""Runs every E2 case as two real processes (run -> SIGKILL, then resume) and saves raw output.

    python -I driver.py <python-of-venv> <runs-dir> <logs-dir>
"""

import json
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
HARNESS = str(HERE / "harness.py")

# (case name, window, path, extra resume args)
CASES = []
for _path in ("sdk", "server"):
    for _w in ("W1", "W2", "W3", "W4a", "W4b", "W6"):
        for _rule, _args in (("count", []), ("visible", ["--visible"])):
            CASES.append((f"{_w}-{_path}-{_rule}", _w, _path, _args))
CASES += [
    ("W6-sdk-count-reject", "W6", "sdk", ["--reject"]),
    ("W2-server-count-retry-on-error", "W2", "server", ["--script", "retry-on-error"]),
    ("W1-server-count-retry-on-error", "W1", "server", ["--script", "retry-on-error"]),
    # W2 under confirmation mode: the person approved charge, then the process died inside it.
    ("W2c-sdk-count", "W2c", "sdk", []),
    ("W2c-sdk-visible", "W2c", "sdk", ["--visible"]),
    ("W2c-server-count", "W2c", "server", []),
    ("W2c-server-visible", "W2c", "server", ["--visible"]),
]


def run(py: str, args: list[str], cwd: Path) -> tuple[int, str, str]:
    p = subprocess.run([py, "-I", HARNESS, *args], cwd=cwd, capture_output=True, text=True,
                       timeout=300)
    return p.returncode, p.stdout, p.stderr


def main() -> None:
    py, runs, logs = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])
    only = set(sys.argv[4:])
    logs.mkdir(parents=True, exist_ok=True)
    out = {}
    for name, window, path, extra in CASES:
        if only and name not in only:
            continue
        wd = runs / name
        shutil.rmtree(wd, ignore_errors=True)
        wd.mkdir(parents=True)
        rc1, so1, se1 = run(py, ["run", str(wd), window, path], runs)
        rc2, so2, se2 = run(py, ["resume", str(wd), window, path, *extra], runs)
        try:
            res = json.loads(so2.strip().splitlines()[-1])
        except Exception:  # noqa: BLE001
            res = {"unparsed_stdout": so2[-2000:], "stderr_tail": se2[-3000:]}
        trace = [json.loads(x) for x in open(wd / "trace.jsonl")] if (wd / "trace.jsonl").exists() else []
        ledger = [json.loads(x) for x in open(wd / "ledger.jsonl")] if (wd / "ledger.jsonl").exists() else []
        rec = {"case": name, "window": window, "path": path, "resume_args": extra,
               "run_returncode": rc1, "killed_by_sigkill": rc1 == -9,
               "resume_returncode": rc2, "resume": res, "trace": trace, "ledger": ledger}
        if rc1 != -9:
            rec["run_stderr_tail"] = se1[-3000:]
        if rc2 != 0:
            rec["resume_stderr_tail"] = se2[-3000:]
        (logs / f"{name}.json").write_text(json.dumps(rec, indent=1))
        out[name] = {"run_rc": rc1, "resume_rc": rc2,
                     "charges": res.get("ledger", {}).get("charge"),
                     "status_after": (res.get("after") or {}).get("status")}
        print(name, out[name], flush=True)


if __name__ == "__main__":
    main()
