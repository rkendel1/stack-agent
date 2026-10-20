#!/usr/bin/env python3
"""Prove the acceptance tests discriminate: unmodified fixture fails F3-F5, reference passes.
Runs against disposable copies; the committed fixture is never touched."""
import os, shutil, subprocess, sys, tempfile
HERE = os.path.dirname(os.path.abspath(__file__))
TARGET = os.environ.get("CARGO_TARGET_DIR", os.path.join(tempfile.gettempdir(), "fb-validate-target"))

def fresh():
    d = tempfile.mkdtemp(prefix="fbval-")
    shutil.copytree(os.path.join(HERE, "fixture"), d, dirs_exist_ok=True)
    return d

def test(d):
    r = subprocess.run(["cargo", "test", "--offline", "--quiet"], cwd=d, capture_output=True, text=True,
                       env={**os.environ, "CARGO_TARGET_DIR": TARGET})
    return r.returncode == 0

def put(d, rel, src):
    os.makedirs(os.path.dirname(os.path.join(d, rel)), exist_ok=True)
    shutil.copy(os.path.join(HERE, src), os.path.join(d, rel))

cases = {}
# F3: acceptance is an exact-diff check on greeting.rs (see harness); here: unmodified differs, reference differs only there
d = fresh(); cases["f3_unmodified_greeting_is_hello"] = "hello" in open(os.path.join(d, "src/greeting.rs")).read()
put(d, "src/greeting.rs", "reference/f3/greeting.rs")
cases["f3_reference_builds_and_passes_baseline"] = test(d)
# F4
d = fresh(); put(d, "tests/f4.rs", "acceptance/f4.rs"); cases["f4_unmodified_fails"] = not test(d)
put(d, "src/math.rs", "reference/f4/math.rs"); cases["f4_reference_passes"] = test(d)
# F5
d = fresh(); put(d, "tests/percent.rs", "overlays/f5/tests/percent.rs"); put(d, "tests/f5.rs", "acceptance/f5.rs")
cases["f5_unmodified_fails"] = not test(d)
put(d, "src/percent.rs", "reference/f5/percent.rs"); cases["f5_reference_passes"] = test(d)
for k, v in cases.items(): print(("PASS " if v else "FAIL ") + k)
sys.exit(0 if all(cases.values()) else 1)
