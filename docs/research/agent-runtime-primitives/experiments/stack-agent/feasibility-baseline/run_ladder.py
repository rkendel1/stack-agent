#!/usr/bin/env python3
"""Run the preregistered ladder for one cell, applying PREREGISTRATION §4 stop rules.
usage: run_ladder.py --bin STACK_AGENT --cell A|B --out results/"""
import argparse, json, os, subprocess, sys
HERE = os.path.dirname(os.path.abspath(__file__))
ap = argparse.ArgumentParser(); ap.add_argument("--bin", required=True); ap.add_argument("--cell", required=True); ap.add_argument("--out", required=True)
a = ap.parse_args()
for task in ("F1", "F2", "F3", "F4", "F5"):
    acc = 0
    for n in (1, 2, 3):
        subprocess.run([sys.executable, os.path.join(HERE, "harness.py"), "--bin", a.bin, "--cell", a.cell, "--task", task, "--trial", str(n), "--out", a.out], check=False)
        t = json.load(open(os.path.join(a.out, f"{a.cell}-{task}-{n}.json")))
        acc += bool(t["acceptance"]["passed"])
    print(f"CELL {a.cell} {task}: {acc}/3 accepted", flush=True)
    if acc == 0:
        print(f"STOP cell {a.cell} at {task} (0/3)", flush=True); break
