# lldb -b -o "command script import lldb_kill.py" -o run -- stack-agent work ...
# The macOS counterpart of gdb_kill.py for the commit-boundary windows (gdb is not available on
# macOS). SIGKILLs the agent at an exact FeltDB commit boundary, without any code change.
# FeltDB appends one JSON line per commit and then syncs the file; on macOS Rust's sync is
# fcntl(fd, F_FULLFSYNC = 51). Commits are numbered 1.. in order on the state-db file.
#   LLDBKILL="commit:N:after"     when commit N's sync has returned
#   LLDBKILL="commit:N:before"    when commit N-1's sync has returned (no byte of commit N)
#   LLDBKILL="commit:N:unsynced"  at entry of commit N's sync (line written, not yet synced)
#   LLDBKILL="commit:N:torn:K"    at entry of commit N's K-th write() (K-1 writes landed: a torn line)
# LLDBKILL_DB is the db path (the sync's fd is matched to it with lsof, so only that file counts).
# Reports to the file LLDBKILL_REPORT (JSON). A torn window counts write() calls on the db's fd, which
# is learned from the first db sync (so commit 1 cannot be torn); the counts are the same as gdb's,
# since both count the same serde_json writes to an unbuffered File.
import json
import os
import subprocess

import lldb

SPEC = os.environ["LLDBKILL"].split(":")
DB = os.path.realpath(os.environ["LLDBKILL_DB"])
REPORT = os.environ["LLDBKILL_REPORT"]
F_FULLFSYNC = 51
N = int(SPEC[1])
WHERE = SPEC[2]
K = int(SPEC[3]) if WHERE == "torn" else None
STATE = {"synced": 0, "fired": None, "fd": None, "writes": 0, "write_bp": None}


def _write_report():
    with open(REPORT, "w") as f:
        json.dump({"spec": os.environ["LLDBKILL"], "fired": STATE["fired"],
                   "commits_synced": STATE["synced"]}, f)


def _fd_path(pid, fd):
    out = subprocess.run(["lsof", "-a", "-p", str(pid), "-d", str(fd), "-Fn"],
                         capture_output=True, text=True).stdout
    for line in out.splitlines():
        if line.startswith("n"):
            return os.path.realpath(line[1:])
    return None


def _kill(process, what):
    STATE["fired"] = what
    _write_report()
    process.Kill()  # SIGKILL


def _after_return(frame, bp_loc, d):
    # The one-shot breakpoint at the sync's return address: the sync has returned.
    STATE["synced"] += 1
    _kill(frame.GetThread().GetProcess(), f"commit {N} sync returned")
    return True


def on_write(frame, bp_loc, d):
    if frame.FindRegister("x0").GetValueAsSigned() != STATE["fd"]:
        return False
    STATE["writes"] += 1
    if STATE["synced"] + 1 == N and STATE["writes"] == K:
        _kill(frame.GetThread().GetProcess(), f"commit {N} write {K} entry")
        return True
    return False


def on_sync(frame, bp_loc, d):
    if frame.FindRegister("x1").GetValueAsSigned() != F_FULLFSYNC:
        return False
    process = frame.GetThread().GetProcess()
    fd = frame.FindRegister("x0").GetValueAsSigned()
    if _fd_path(process.GetProcessID(), fd) != DB:
        return False
    commit = STATE["synced"] + 1  # the commit this sync makes durable
    if WHERE == "torn":
        STATE["fd"] = fd
        STATE["writes"] = 0
        STATE["synced"] += 1
        if STATE["synced"] == N - 1 and STATE["write_bp"] is None:
            bp = process.GetTarget().BreakpointCreateByName("write")
            bp.SetScriptCallbackFunction("lldb_kill.on_write")
            STATE["write_bp"] = bp
        return False
    if WHERE == "unsynced" and commit == N:
        _kill(process, f"commit {N} sync entry")
        return True
    target_commit = N if WHERE == "after" else N - 1
    if WHERE in ("after", "before") and commit == target_commit:
        lr = frame.FindRegister("lr").GetValueAsUnsigned()
        bp = process.GetTarget().BreakpointCreateByAddress(lr)
        bp.SetOneShot(True)
        bp.SetScriptCallbackFunction("lldb_kill._after_return")
        return False
    STATE["synced"] += 1
    return False


def __lldb_init_module(debugger, internal_dict):
    _write_report()
    target = debugger.GetSelectedTarget()
    bp = target.BreakpointCreateByName("fcntl")
    bp.SetScriptCallbackFunction("lldb_kill.on_sync")
