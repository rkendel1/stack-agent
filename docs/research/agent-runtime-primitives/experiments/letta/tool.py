"""Fake tools, invoked through Letta Code's built-in Bash tool (the scripted
model emits Bash calls whose command runs this file).

  lookup <key>              observation: ledger line, prints PRICE=42
  charge <account> <amount> effect: ledger line (fsynced), prints CHARGED

Deterministic kill points (KILL_AT, once per run via KILL_FLAG):
  W1  inside charge, BEFORE the ledger write (model has chosen charge)
  W2  inside charge, AFTER the ledger line is fsynced, before returning
The kill is SIGKILL to the letta process group (pgid in LETTA_PGID_FILE).
"""
import json, os, signal, sys, time
LEDGER = os.environ["LEDGER"]

def ledger(tool, args):
    fd = os.open(LEDGER, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
    os.write(fd, (json.dumps({"tool": tool, "args": args, "invocation": os.environ.get("INVOCATION", ""),
                              "pid": os.getpid(), "t": time.time()}) + "\n").encode())
    os.fsync(fd); os.close(fd)

def maybe_kill(window):
    flag = os.environ.get("KILL_FLAG")
    if os.environ.get("KILL_AT") == window and flag and not os.path.exists(flag):
        with open(flag, "w") as f:
            f.write(window); f.flush(); os.fsync(f.fileno())
        os.killpg(int(open(os.environ["LETTA_PGID_FILE"]).read()), signal.SIGKILL)
        time.sleep(30)

cmd = sys.argv[1]
if cmd == "lookup":
    ledger("lookup", {"key": sys.argv[2]}); print("PRICE=42")
elif cmd == "charge":
    maybe_kill("W1")
    ledger("charge", {"account": sys.argv[2], "amount": sys.argv[3]})
    maybe_kill("W2")
    print("CHARGED acct-7", sys.argv[3])
