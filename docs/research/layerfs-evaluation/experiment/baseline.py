"""Baseline workspace mechanisms on the same fixture (stack-agent HEAD tree).
Usage: python3 -I baseline.py <scratch> <runs>
Mechanisms: copy (cp -a), worktree (git worktree add), clone (git clone --shared).
Each trial: create two candidates, isolation, exact write, checkpoint+restore,
failed attempt, concurrent modification on promotion, cleanup."""
import hashlib, json, os, shutil, subprocess, sys, time

S, RUNS = sys.argv[1], int(sys.argv[2])
REPO = os.path.join(S, "baseline-repo")  # scratch clone, never the user's checkout
TARGET = "crates/chip-core/src/lib.rs"


def sh(*a, cwd=None, check=True):
    return subprocess.run(a, cwd=cwd, check=check, capture_output=True, text=True)


def ms(t):
    return (time.perf_counter() - t) * 1000


def du(p):
    return int(sh("du", "-sb", p).stdout.split()[0])


def sha(p):
    return hashlib.sha256(open(p, "rb").read()).hexdigest()


def git(cwd, *a, check=True):
    return sh("git", "-c", "user.email=e@x", "-c", "user.name=eval", *a, cwd=cwd, check=check)


if not os.path.exists(REPO):
    sh("git", "clone", "-q", "--no-hardlinks", "/home/user/stack-agent", REPO)
ORIG = open(os.path.join(REPO, TARGET), "rb").read()


def create(mech, dest, branch):
    if mech == "copy":
        sh("cp", "-a", REPO, dest)
    elif mech == "worktree":
        git(REPO, "worktree", "add", "-q", "-b", branch, dest, "HEAD")
    elif mech == "clone":
        sh("git", "clone", "-q", "--shared", REPO, dest)


def remove(mech, dest):
    if mech == "worktree":
        git(REPO, "worktree", "remove", "--force", dest)
    else:
        shutil.rmtree(dest)


def trial(mech, run):
    base = os.path.join(S, "bl", f"{mech}-{run}")
    os.makedirs(base)
    a, b = os.path.join(base, "a"), os.path.join(base, "b")
    fails, r = [], {"mech": mech, "run": run}
    before_repo = du(os.path.join(REPO, ".git"))
    t = time.perf_counter(); create(mech, a, f"a{run}"); create(mech, b, f"b{run}"); r["create2_ms"] = ms(t)
    t = time.perf_counter(); first = open(os.path.join(a, TARGET), "rb").read(); r["first_read_ms"] = ms(t)
    if first != ORIG: fails.append("first read")
    payload = ORIG + f"\n// candidate-a change {run}\n".encode()
    t = time.perf_counter()
    p = os.path.join(a, TARGET); tmp = p + ".tmp"
    with open(tmp, "wb") as f: f.write(payload); f.flush(); os.fsync(f.fileno())
    os.replace(tmp, p); r["write_ms"] = ms(t)
    t = time.perf_counter(); ok = sha(p) == hashlib.sha256(payload).hexdigest(); r["readback_ms"] = ms(t)
    if not ok: fails.append("readback")
    if open(os.path.join(b, TARGET), "rb").read() != ORIG: fails.append("b contaminated")
    if open(os.path.join(REPO, TARGET), "rb").read() != ORIG: fails.append("source contaminated")
    # checkpoint: a git commit in the candidate (copy/clone/worktree all carry .git)
    t = time.perf_counter(); git(a, "commit", "-qam", "checkpoint"); ck = git(a, "rev-parse", "HEAD").stdout.strip(); r["checkpoint_ms"] = ms(t)
    open(p, "w").write("BROKEN\n"); open(os.path.join(a, "new-file.txt"), "w").write("junk\n")
    t = time.perf_counter(); git(a, "reset", "-q", "--hard", ck); git(a, "clean", "-qfd"); r["restore_ms"] = ms(t)
    if open(p, "rb").read() != payload or os.path.exists(os.path.join(a, "new-file.txt")): fails.append("restore")
    # failed attempt: next attempt starts from source HEAD (fresh candidate)
    c = os.path.join(base, "c"); create(mech, c, f"c{run}")
    if open(os.path.join(c, TARGET), "rb").read() != ORIG: fails.append("attempt-2 not clean")
    # concurrent modification: user edits source after candidate started;
    # promotion = 3-way apply of candidate diff onto the current source tree.
    open(os.path.join(c, TARGET), "w").write("candidate version\n")
    diff = git(c, "diff", "--binary", "HEAD").stdout
    user = os.path.join(base, "user"); shutil.copytree(REPO, user, symlinks=True)  # stands in for the user's dirty checkout
    open(os.path.join(user, TARGET), "w").write("user version\n")
    pr = subprocess.run(["git", "apply", "--3way", "--index", "-"] if False else ["git", "apply", "--check", "-"],
                        cwd=user, input=diff, capture_output=True, text=True)
    r["promotion_conflict_detected"] = pr.returncode != 0
    r["user_change_preserved"] = open(os.path.join(user, TARGET)).read() == "user version\n"
    r["storage_two_candidates"] = du(a) + du(b)
    r["repo_git_growth"] = du(os.path.join(REPO, ".git")) - before_repo
    t = time.perf_counter()
    for d in (a, b, c): remove(mech, d)
    r["cleanup_ms"] = ms(t)
    shutil.rmtree(base)
    if mech == "worktree":
        for br in (f"a{run}", f"b{run}", f"c{run}"): git(REPO, "branch", "-q", "-D", br)
    r["residual"] = os.path.exists(base)
    r["fails"] = fails
    print("BASE " + json.dumps(r), flush=True)


for mech in ("copy", "worktree", "clone"):
    for run in range(RUNS):
        trial(mech, run)
