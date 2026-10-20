#!/usr/bin/env python3
"""Offline check of harness acceptance + classification (no model, no trial)."""
import json, os, shutil, subprocess, sys, tempfile
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import harness as h
T = os.path.join(tempfile.gettempdir(), "fb-trial-target")

def mk(task, ref=None):
    d = tempfile.mkdtemp(prefix="fbt-"); p = os.path.join(d, "p")
    shutil.copytree(os.path.join(h.HERE, "fixture"), p)
    if task == "F5": shutil.copytree(os.path.join(h.HERE, "overlays/f5/tests"), os.path.join(p, "tests"), dirs_exist_ok=True)
    open(os.path.join(p, ".gitignore"), "w").write("target/\n")
    h.sh(["cargo", "generate-lockfile", "--offline"], p)
    for c in (["git","init","-q"],["git","add","-A"],["git","-c","user.email=h@x","-c","user.name=h","commit","-qm","b"]): h.sh(c, p)
    if ref: shutil.copy(os.path.join(h.HERE, "reference", ref[0], ref[1]), os.path.join(p, "src", ref[1]))
    return p

ok = True
def check(name, cond):
    global ok; ok &= bool(cond); print(("PASS " if cond else "FAIL ") + name)

done = {"terminal_state": "completed"}
for task, ref in (("F3", ("f3", "greeting.rs")), ("F4", ("f4", "math.rs")), ("F5", ("f5", "percent.rs"))):
    p = mk(task, ref); acc, d = h.accept(task, p, done, [], T); check(f"{task} reference accepted", acc)
    p = mk(task); acc, d = h.accept(task, p, done, [], T); check(f"{task} unmodified rejected", not acc)
# F3: an extra unrelated change is rejected even if the edit is right
p = mk("F3", ("f3", "greeting.rs")); open(os.path.join(p, "src/util.rs"), "a").write("// x\n")
check("F3 unrelated change rejected", not h.accept("F3", p, done, [], T)[0])
# F2 / F1
p = mk("F2"); a = {**done, "answer": "triple in src/math.rs"}
check("F2 correct answer accepted", h.accept("F2", p, a, [], T)[0])
check("F2 wrong answer rejected", not h.accept("F2", p, {**done, "answer": "add in src/math.rs"}, [], T)[0])
rep = [{"decision": "request_capability", "capability": "project.list", "inputs": {"path": "src"}}]
check("F1 list src accepted", h.accept("F1", p, done, rep, T)[0])
check("F1 list . only rejected", not h.accept("F1", p, done, [{"decision":"request_capability","capability":"project.list","inputs":{"path":"."}}], T)[0])
# classification from the audit report
audit = os.environ.get("AUDIT_REPORT")
if audit:
    r = json.load(open(audit)); check("audit run classifies length_limited", h.classify("F2", False, r, [], p, T, False) == "length_limited")
sys.exit(0 if ok else 1)
