# gdb -q -batch -x gdb_kill.py --args stack-agent work ...
# SIGKILLs the agent at an exact FeltDB commit boundary, without any code change. FeltDB appends
# one JSON line per commit (open O_APPEND, many small write()s from serde_json::to_writer on an
# unbuffered File, then fdatasync()); commits are numbered 1.. in order on the state-db file.
#   GDBKILL="commit:N:before"    when commit N-1's fdatasync has returned (no byte of commit N)
#   GDBKILL="commit:N:torn:K"    at entry of commit N's K-th write() (K-1 writes landed: a torn line)
#   GDBKILL="commit:N:unsynced"  at entry of commit N's fdatasync (line written, not synced)
#   GDBKILL="commit:N:after"     when commit N's fdatasync has returned
#   GDBKILL="count"              kill nothing; report write() calls per commit
# GDBKILL_DB is the db path; fds are resolved through /proc/<pid>/fd, so only that file counts.
# Reports to the file GDBKILL_REPORT (JSON).
import json
import os
import signal

import gdb

spec = os.environ["GDBKILL"].split(":")
db = os.path.realpath(os.environ["GDBKILL_DB"])
report_path = os.environ["GDBKILL_REPORT"]
NR_WRITE, NR_FDATASYNC = 1, 75  # x86_64
mode = spec[0]
N = int(spec[1]) if mode == "commit" else None
where = spec[2] if mode == "commit" else None
K = int(spec[3]) if where == "torn" else None

for c in ("set pagination off", "set confirm off", "set print thread-events off",
          "set follow-fork-mode parent", "set detach-on-fork on",
          "handle SIGPIPE SIGCHLD SIGUSR1 SIGUSR2 nostop noprint pass"):
    gdb.execute(c)
gdb.execute("catch syscall fdatasync")
write_catch = {"on": False}


def watch_writes():
    if not write_catch["on"]:
        gdb.execute("catch syscall write", to_string=True)
        write_catch["on"] = True


synced = 0          # commits whose fdatasync returned
writes_in_commit = 0
per_commit = []
in_call = {}
fired = None


def fd_path(pid, fd):
    try:
        return os.path.realpath(os.readlink(f"/proc/{pid}/fd/{fd}"))
    except OSError:
        return None


def kill(pid, what):
    global fired
    fired = what
    os.kill(pid, signal.SIGKILL)


if mode == "count" or (mode == "commit" and where == "torn" and N == 1):
    watch_writes()
gdb.execute("run", to_string=True)
while gdb.selected_inferior().pid != 0 and fired is None:
    pid = gdb.selected_inferior().pid
    lwp = gdb.selected_thread().ptid[1]
    if lwp not in in_call:  # syscall entry
        nr, fd = int(gdb.parse_and_eval("$orig_rax")), int(gdb.parse_and_eval("$rdi"))
        in_call[lwp] = (nr, fd)
        if fd_path(pid, fd) == db:
            commit = synced + 1
            if nr == NR_WRITE:
                writes_in_commit += 1
                if where == "torn" and commit == N and writes_in_commit == K:
                    kill(pid, f"commit {N} write {K} entry")
            elif nr == NR_FDATASYNC and where == "unsynced" and commit == N:
                kill(pid, f"commit {N} fdatasync entry")
    else:  # syscall return
        nr, fd = in_call.pop(lwp)
        if nr == NR_FDATASYNC and fd_path(pid, fd) == db:
            synced += 1
            per_commit.append(writes_in_commit)
            writes_in_commit = 0
            if where == "after" and synced == N:
                kill(pid, f"commit {N} fdatasync return")
            elif where == "before" and synced == N - 1:
                kill(pid, f"commit {N-1} fdatasync return (before commit {N})")
            elif where == "torn" and synced == N - 1:
                watch_writes()
    if fired:
        break
    try:
        gdb.execute("continue", to_string=True)
    except gdb.error:
        break
try:
    gdb.execute("continue", to_string=True)  # collect the SIGKILL exit
except gdb.error:
    pass
with open(report_path, "w") as f:
    json.dump({"spec": os.environ["GDBKILL"], "fired": fired, "commits_synced": synced,
               "writes_per_commit": per_commit, "writes_in_open_commit": writes_in_commit}, f)
