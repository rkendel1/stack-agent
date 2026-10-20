"""E2 supplement: no owner/fence/lease. After a W1 crash, two fresh processes resume the same thread.

Process A resumes and is held (file barrier) inside `charge` before its ledger write; process B resumes
the same thread to completion; then A is released. Ground truth: ledger charge lines.
usage: python -I e2_concurrent.py <workdir>  -> <workdir>/e2_concurrent.json
"""
import json
import os
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from common import read_jsonl  # noqa: E402

work = sys.argv[1]
d = os.path.join(work, "e2_concurrent")
shutil.rmtree(d, ignore_errors=True)
os.makedirs(d)
base = dict(os.environ, LEDGER=f"{d}/ledger.jsonl", CALLS=f"{d}/calls.jsonl", EVENTS=f"{d}/events.jsonl")
base.pop("KILL_AT", None)
child = [sys.executable, "-I", os.path.join(HERE, "e2_child.py"), f"{d}/state.sqlite", "t-c"]
r0 = subprocess.run(child + ["start", "sync"], env=dict(base, KILL_AT="W1"), capture_output=True, text=True)
hold = f"{d}/hold"
a = subprocess.Popen(child + ["resume", "sync"], env=dict(base, HOLD_IN_CHARGE=hold),
                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
while not os.path.exists(hold + ".entered"):  # condition wait, not a timing assumption
    if a.poll() is not None:
        break
    time.sleep(0.01)
b = subprocess.run(child + ["resume", "sync"], env=base, capture_output=True, text=True)
open(hold + ".release", "w").close()
a_out, a_err = a.communicate(timeout=120)
insp = subprocess.run(child + ["inspect", "sync"], env=base, capture_output=True, text=True)
ledger = read_jsonl(base["LEDGER"])
last = lambda s: next((json.loads(l) for l in s.splitlines() if l.startswith("{")), None)  # noqa: E731
res = {
    "start_returncode": r0.returncode,
    "A": last(a_out), "B": last(b.stdout), "final_inspect": last(insp.stdout),
    "charges_in_ledger": sum(l["tool"] == "charge" for l in ledger),
    "charge_pids": [l["pid"] for l in ledger if l["tool"] == "charge"],
    "charge_task_ids": [l["task_id"] for l in ledger if l["tool"] == "charge"],
    "A_stderr_tail": a_err[-500:],
}
fs = (res["final_inspect"] or {}).get("state", {})
res["final_state_charge_toolmsgs"] = fs.get("charge_toolmsgs")
res["outcome"] = "duplicate_effect" if res["charges_in_ledger"] > 1 else "correct"
json.dump(res, open(os.path.join(work, "e2_concurrent.json"), "w"), indent=1, default=str)
print(res["outcome"], res["charges_in_ledger"], res["charge_task_ids"], "final charge msgs", res["final_state_charge_toolmsgs"],
      "A err", (res["A"] or {}).get("error"), "B err", (res["B"] or {}).get("error"))
