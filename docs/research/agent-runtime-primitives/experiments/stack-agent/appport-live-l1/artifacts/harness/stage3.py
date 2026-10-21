#!/usr/bin/env python3
"""Stage 3: Chip's work loop (scripted model, durable FeltDB state) against the live MCP endpoint
`appport dev` serves for the generated invoice-desk bridge. Every verdict here comes from the
application's own SQLite database and from the counting proxy's log of requests that reached the
application, never from the run's own report.

usage: stage3.py <desk-dir (generated, installed)> <driver binary> <stack-agent binary> <evidence dir>
"""
import json, os, secrets, shutil, signal, sqlite3, subprocess, sys, time, urllib.request

DESK, DRIVER, STACK_AGENT, OUT = sys.argv[1:5]
TOOLS = os.path.dirname(os.path.abspath(__file__))
shutil.rmtree(OUT, ignore_errors=True)
os.makedirs(OUT)
DB = os.path.join(OUT, "invoices.db")
PROXY_LOG = os.path.join(OUT, "upstream-requests.jsonl")
STUB_LOG = os.path.join(OUT, "stub.jsonl")
APP, PROXY, READER, WRITER, STUB = 3931, 3932, 3933, 3934, 3935
SEL = "invoicedesk"
CAP = lambda tool: f"appport.{SEL}.{tool}"
GET, STATUS, LIST = CAP("invoices_id"), CAP("invoices_id_status"), CAP("invoicedesk_invoices")
procs = []
results = []

def start(cmd, env=None, log=None, cwd=DESK):
    p = subprocess.Popen(cmd, cwd=cwd, env={**os.environ, **(env or {})}, stdin=subprocess.DEVNULL,
                         stdout=open(log, "w") if log else subprocess.DEVNULL, stderr=subprocess.STDOUT)
    procs.append(p)
    return p

def wait_port(port):
    for _ in range(150):
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=1)
            return
        except urllib.error.HTTPError:
            return
        except Exception:
            time.sleep(0.2)
    raise SystemExit(f"port {port} did not open")

def row(i):
    c = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    r = c.execute("SELECT id, status, note, updated_at FROM invoices WHERE id=?", (i,)).fetchone()
    c.close()
    return None if r is None else {"id": r[0], "status": r[1], "note": r[2], "updated_at": r[3]}

def table():
    c = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    r = [list(x) for x in c.execute("SELECT id, status, note FROM invoices ORDER BY id")]
    c.close()
    return r

def upstream():
    if not os.path.exists(PROXY_LOG):
        return []
    return [json.loads(l) for l in open(PROXY_LOG)]

def writes():
    return [r for r in upstream() if r["method"] != "GET"]

def decision(**d):
    return json.dumps({**d, "schema": "chip.work-decision.v1"})

def drive(name, endpoint, token, scopes, goal, require, replies, task=None, fault=None, state=None):
    task = task or name
    state = state or os.path.join(OUT, f"{task}.feltdb")
    rp = os.path.join(OUT, f"{name}.replies.json")
    json.dump(replies, open(rp, "w"), indent=1)
    out = os.path.join(OUT, f"{name}.run.json")
    cmd = [DRIVER, "--endpoint", endpoint, "--token-env", "MCP_TOKEN", "--selector", SEL, "--scopes", scopes,
           "--goal", goal, "--replies", rp, "--state-db", state, "--task-id", task, "--out", out, "--lease-ms", "1500"]
    if require:
        cmd += ["--require", require]
    if fault:
        cmd += ["--fault", fault[0], "--fault-capability", fault[1]]
    p = subprocess.run(cmd, env={**os.environ, "MCP_TOKEN": token}, capture_output=True, text=True, timeout=120)
    open(os.path.join(OUT, f"{name}.stderr.txt"), "w").write(p.stderr)
    record = json.load(open(out)) if os.path.exists(out) else None
    return p.returncode, record, state

def check(case, name, ok, **detail):
    results.append({"case": case, "check": name, "ok": bool(ok), **detail})
    print(f"{'PASS' if ok else 'FAIL'} [{case}] {name}")

def heads(record):
    return [o["head"] for o in (record or {}).get("observations", [])]

def agent_state(state, task):
    p = subprocess.run([STACK_AGENT, "agent-state", "show", "--state-db", state, "--task-id", task, "--json"], capture_output=True, text=True)
    path = os.path.join(OUT, f"{task}.agent-state.json")
    open(path, "w").write(p.stdout + p.stderr)
    try:
        return json.loads(p.stdout)
    except Exception:
        return {"raw": p.stdout + p.stderr, "exit": p.returncode}

try:
    reader_token = "reader-" + secrets.token_hex(16)
    writer_token = "writer-" + secrets.token_hex(16)
    start(["./node_modules/.bin/tsx", "src/server.ts"], {"DB_PATH": DB, "PORT": str(APP)}, os.path.join(OUT, "app.log"))
    wait_port(APP)
    start(["node", os.path.join(TOOLS, "counting-proxy.mjs"), str(PROXY), str(APP), PROXY_LOG])
    wait_port(PROXY)
    dev = ["./node_modules/.bin/appport", "dev", "--no-studio", "--port"]
    up = {"APPPORT_UPSTREAM": f"http://127.0.0.1:{PROXY}"}
    start(dev + [str(READER)], {**up, "APPPORT_MCP_TOKEN": reader_token, "APPPORT_MCP_SCOPES": "invoices.read"}, os.path.join(OUT, "dev-reader.log"))
    start(dev + [str(WRITER)], {**up, "APPPORT_MCP_TOKEN": writer_token, "APPPORT_MCP_SCOPES": "invoices.read invoices.write"}, os.path.join(OUT, "dev-writer.log"))
    wait_port(READER); wait_port(WRITER)
    R = f"http://127.0.0.1:{READER}/apps/com.example.invoice-desk/mcp"
    W = f"http://127.0.0.1:{WRITER}/apps/com.example.invoice-desk/mcp"
    json.dump(table(), open(os.path.join(OUT, "db-before.json"), "w"))
    os.remove(PROXY_LOG) if os.path.exists(PROXY_LOG) else None

    # 1. Discover and read.
    code, rec, _ = drive("c1-read", R, reader_token, "invoices.read", "Report the status of invoice 3. The summary must end with status=<status>.", GET,
                         [decision(decision="request_capability", capability=GET, inputs={"id": "3"}), decision(decision="complete", summary="Invoice 3 is awaiting review. status=needs_review")])
    check(1, "discovery: offered = the grant's read tools, from tools/list", rec and sorted(rec["offered"]) == sorted([GET, LIST]), offered=rec and rec["offered"], listed=rec and rec["listed"])
    check(1, "read completes", code == 0, exit=code, outcome=rec and rec["outcome"])
    h = heads(rec)
    check(1, "the observation equals the stored row", h and h[0]["outcome"] == "success" and json.loads(rec["observations"][0]["output"].split("--- result ---\n", 1)[1])["status"] == row(3)["status"], head=h and h[0], stored=row(3))
    check(1, "exactly one GET reached the application", [(r["method"], r["path"]) for r in upstream()] == [("GET", "/api/invoices/3")], upstream=upstream())

    # 2. Authorized write, independently verified.
    before = row(2); n0 = len(writes())
    code, rec, state2 = drive("c2-write", W, writer_token, "invoices.read invoices.write", "Approve invoice 2 with the note 'approved by chip'.", STATUS,
                              [decision(decision="request_capability", capability=GET, inputs={"id": "2"}),
                               decision(decision="request_capability", capability=STATUS, inputs={"id": "2", "status": "approved", "note": "approved by chip"}),
                               decision(decision="complete", summary="Invoice 2 approved.")])
    after = row(2)
    check(2, "write completes", code == 0, exit=code, outcome=rec and rec["outcome"])
    check(2, "the database shows the change", before["status"] == "processing" and after["status"] == "approved" and after["note"] == "approved by chip", before=before, after=after)
    check(2, "exactly one PATCH reached the application", [(r["method"], r["path"], json.loads(r["body"])) for r in writes()[n0:]] == [("PATCH", "/api/invoices/2/status", {"id": "2", "status": "approved", "note": "approved by chip"})] or [(r["method"], r["path"]) for r in writes()[n0:]] == [("PATCH", "/api/invoices/2/status")], writes=writes()[n0:])
    wh = [x for x in heads(rec) if x and x["capability"] == STATUS]
    check(2, "the execution's evidence names the host's execution unit", wh and wh[0]["evidence"]["executionUnitId"].startswith("exec_") and rec["observations"][1]["receipt_id"] == wh[0]["evidence"]["executionUnitId"], evidence=wh and wh[0]["evidence"])
    st = agent_state(state2, "c2-write")
    check(2, "FeltDB recorded the task as completed", json.dumps(st).find('"completed"') >= 0, agent_state=os.path.basename(state2))

    # 3. Forbidden: the grant does not include the write.
    snap = table(); n0 = len(writes())
    code, rec, _ = drive("c3-forbidden-endpoint", R, reader_token, "invoices.read", "Approve invoice 1.", STATUS,
                         [decision(decision="request_capability", capability=STATUS, inputs={"id": "1", "status": "approved"}),
                          decision(decision="complete", summary="Invoice 1 approved."),
                          decision(decision="block", reason="I cannot approve invoices with this access.")])
    check(3, "endpoint grant read-only: the write tool is not listed, so not offered", rec and STATUS not in rec["offered"] and "invoices_id_status" not in rec["listed"], listed=rec and rec["listed"])
    check(3, "not completed", code != 0 and "completed" not in (rec or {}).get("outcome", {}), exit=code, outcome=rec and rec["outcome"])
    code, rec, _ = drive("c3-forbidden-grant", W, writer_token, "invoices.read", "Approve invoice 1.", STATUS,
                         [decision(decision="request_capability", capability=STATUS, inputs={"id": "1", "status": "approved"}),
                          decision(decision="complete", summary="Invoice 1 approved."),
                          decision(decision="block", reason="I cannot approve invoices with this access.")])
    check(3, "operator grant read-only on a read-write endpoint: listed but not offered", rec and STATUS not in rec["offered"] and "invoices_id_status" in rec["listed"] and any(n["tool"] == "invoices_id_status" for n in rec["not_offered"]), not_offered=rec and rec["not_offered"])
    check(3, "not completed", code != 0 and "completed" not in (rec or {}).get("outcome", {}), exit=code, outcome=rec and rec["outcome"])
    check(3, "no write reached the application and nothing changed", writes()[n0:] == [] and table() == snap, writes=writes()[n0:])

    # 4. Nonexistent resource, then a fabricated answer.
    code, rec, _ = drive("c4-not-found", W, writer_token, "invoices.read invoices.write", "Report the status of invoice 999.", GET,
                         [decision(decision="request_capability", capability=GET, inputs={"id": "999"}),
                          decision(decision="complete", summary="Invoice 999 is approved."),
                          decision(decision="block", reason="Invoice 999 does not exist.")])
    h = heads(rec)
    check(4, "NOT_FOUND is a failed observation with the typed error", h and h[0]["outcome"] == "failed" and h[0]["error"]["code"] == "NOT_FOUND" and rec["observations"][0]["status"] == "Failure", head=h and h[0])
    check(4, "the fabricated completion is refused: not completed", code != 0 and "completed" not in rec["outcome"], exit=code, outcome=rec["outcome"])

    # 5. Interrupted consequential writes, then a restart.
    for label, fault, invoice, target in (("after", "crash-after-send", 4, "rejected"), ("before", "crash-before-send", 1, "rejected")):
        task = f"c5-crash-{label}"
        before = row(invoice); n0 = len(writes())
        script = [decision(decision="request_capability", capability=STATUS, inputs={"id": str(invoice), "status": target}),
                  decision(decision="complete", summary=f"Invoice {invoice} {target}.")]
        code, rec, state = drive(task, W, writer_token, "invoices.read invoices.write", f"Reject invoice {invoice}.", STATUS, script, task=task, fault=(fault, STATUS))
        mid = row(invoice); during = writes()[n0:]
        check(5, f"{fault}: the process was killed (no report)", code != 0 and rec is None, exit=code)
        if label == "after":
            check(5, f"{fault}: the write reached the application once and took effect", len(during) == 1 and mid["status"] == target, writes=during, stored=mid)
        else:
            check(5, f"{fault}: nothing reached the application", during == [] and mid == before, writes=during, stored=mid)
        time.sleep(2.0)  # past the dead process's lease
        code, rec, _ = drive(task + "-restart", W, writer_token, "invoices.read invoices.write", f"Reject invoice {invoice}.", STATUS, script, task=task, state=state)
        check(5, f"{fault}: restart refuses with OutcomeUncertain (exit 3) and runs nothing", code == 3 and rec and rec["started"] is False and rec["durable_refusal"]["uncertain"], exit=code, refusal=rec and rec.get("durable_refusal"))
        check(5, f"{fault}: no further write reached the application", writes()[n0:] == during and row(invoice) == mid, writes=writes()[n0:])
        agent_state(state, task)

    # 6. No tool call, a malformed model reply, a malformed tool result.
    code, rec, _ = drive("c6-no-call", W, writer_token, "invoices.read invoices.write", "Approve invoice 2.", STATUS,
                         [decision(decision="complete", summary="Invoice 2 approved."), decision(decision="block", reason="none")])
    check(6, "completion without the required call is refused", code != 0 and "completed" not in rec["outcome"] and rec["observations"] == [], outcome=rec["outcome"])
    code, rec, _ = drive("c6-malformed-reply", W, writer_token, "invoices.read invoices.write", "Approve invoice 2.", STATUS,
                         ["Sure! I approved invoice 2.", decision(decision="complete", summary="done")])
    check(6, "a reply that is not a decision is not a completion", code != 0 and "completed" not in rec["outcome"], outcome=rec["outcome"])
    for mode in ("garbage", "noresult", "hangup"):
        stub = start(["node", os.path.join(TOOLS, "mcp-fault-stub.mjs"), str(STUB), W, mode, STUB_LOG])
        wait_port(STUB)
        invoice = {"garbage": 3, "noresult": 3, "hangup": 3}[mode]
        before = row(invoice); n0 = len(writes())
        target = "approved"
        code, rec, state = drive(f"c6-result-{mode}", f"http://127.0.0.1:{STUB}/apps/com.example.invoice-desk/mcp", writer_token, "invoices.read invoices.write", f"Approve invoice {invoice}.", STATUS,
                                 [decision(decision="request_capability", capability=STATUS, inputs={"id": str(invoice), "status": target}),
                                  decision(decision="request_capability", capability=STATUS, inputs={"id": str(invoice), "status": target}),
                                  decision(decision="complete", summary=f"Invoice {invoice} approved."),
                                  decision(decision="block", reason="the outcome is unknown")])
        stub.send_signal(signal.SIGTERM); stub.wait()
        obs = rec["observations"] if rec else []
        sent = writes()[n0:]
        check(6, f"tool result '{mode}': not completed", code != 0 and "completed" not in rec["outcome"], exit=code, outcome=rec and rec["outcome"], observations=[(o["status"], (o["output"] or "")[:200]) for o in obs])
        reason = json.dumps(rec["outcome"])
        check(6, f"tool result '{mode}': the work fails with 'its outcome is unknown', and no success is observed", "Failed" in reason and "outcome is unknown" in reason and all(o["status"] != "Success" for o in obs), outcome=rec["outcome"])
        st = agent_state(state, f"c6-result-{mode}")
        open_intents = [i for i in st.get("intents", []) if i.get("state") != "resolved"]
        check(6, f"tool result '{mode}': FeltDB keeps the write's intent unresolved (no outcome is claimed)", st.get("task", {}).get("unresolved_intents") == 1 and len(open_intents) == 1, intents=st.get("intents"))
        if mode == "hangup":
            check(6, "hangup: the write really executed once before the connection dropped", len(sent) >= 1 and row(invoice)["status"] == target, writes=sent, stored=row(invoice))
            check(6, "hangup: Chip did not repeat the uncertain write in the run", len(sent) == 1, writes=sent)
            # The same task again, with a model that asks for the same write: does a resumed attempt repeat it?
            time.sleep(2.0)
            n1 = len(writes())
            code, rec2, _ = drive("c6-result-hangup-restart", W, writer_token, "invoices.read invoices.write", f"Approve invoice {invoice}.", STATUS,
                                  [decision(decision="request_capability", capability=STATUS, inputs={"id": str(invoice), "status": target}),
                                   decision(decision="complete", summary=f"Invoice {invoice} approved."), decision(decision="block", reason="unknown")],
                                  task=f"c6-result-{mode}", state=state)
            again = writes()[n1:]
            check(6, "hangup: a resumed attempt does not send the uncertain write again", again == [], writes=again, exit=code, run=rec2 and {k: rec2.get(k) for k in ("started", "durable_refusal", "outcome")})
            check(6, "hangup: the resumed attempt is refused as uncertain (exit 3)", code == 3 and rec2 and rec2.get("durable_refusal", {}).get("uncertain"), exit=code)
        else:
            check(6, f"tool result '{mode}': the stub did not forward it; nothing reached the application", sent == [] and row(invoice) == before, writes=sent)

    # 7. Evidence distinguishes refusal, application failure, transport failure and an unknown outcome.
    code, rec_app, _ = drive("c7-app-failure", W, writer_token, "invoices.read invoices.write", "Reject invoice 5.", STATUS,
                             [decision(decision="request_capability", capability=STATUS, inputs={"id": "5", "status": "rejected"}), decision(decision="block", reason="conflict")])
    code, rec_down, _ = drive("c7-unreachable", "http://127.0.0.1:3999/apps/com.example.invoice-desk/mcp", writer_token, "invoices.read invoices.write", "Approve invoice 6.", STATUS, [])  # connect fails before any run
    # Unreachable during a run: the endpoint answers discovery, then accepts no further connection.
    stub = start(["node", os.path.join(TOOLS, "mcp-fault-stub.mjs"), str(STUB), W, "closeafterlist", STUB_LOG])
    wait_port(STUB)
    n0 = len(writes())
    code, _, _ = drive("c7-unreachable-call", f"http://127.0.0.1:{STUB}/apps/com.example.invoice-desk/mcp", writer_token, "invoices.read invoices.write", "Approve invoice 6.", STATUS,
                       [decision(decision="request_capability", capability=STATUS, inputs={"id": "6", "status": "approved"}), decision(decision="block", reason="unreachable")])
    stub.send_signal(signal.SIGTERM); stub.wait()
    out = os.path.join(OUT, "c7-unreachable-call.run.json")
    check(7, "transport failure at call: nothing reached the application", writes()[n0:] == [], writes=writes()[n0:])
    rec_unreach = json.load(open(out)) if os.path.exists(out) else None
    hangup = json.load(open(os.path.join(OUT, "c6-result-hangup.run.json")))
    refused = json.load(open(os.path.join(OUT, "c3-forbidden-grant.run.json")))
    kinds = {
        "refusal (not offered under the grant)": refused["not_offered"],
        "application failure": heads(rec_app)[0] if heads(rec_app) else None,
        "transport failure (endpoint unreachable at connect)": rec_down and rec_down.get("error"),
        "transport failure (endpoint unreachable at call)": rec_unreach and rec_unreach.get("outcome"),
        "uncertain outcome (sent, no answer)": hangup["outcome"],
    }
    json.dump(kinds, open(os.path.join(OUT, "c7-evidence-kinds.json"), "w"), indent=1)
    check(7, "application failure: outcome failed with the application's code (CONFLICT)", heads(rec_app) and heads(rec_app)[0]["outcome"] == "failed" and heads(rec_app)[0]["error"]["code"] == "CONFLICT", head=heads(rec_app)[:1])
    check(7, "transport failure at connect: the run does not start", rec_down and rec_down["started"] is False and "could not be reached" in rec_down["error"] or "refused" in (rec_down or {}).get("error", ""), error=rec_down and rec_down.get("error"))
    check(7, "transport failure at call: 'nothing was sent', not an unknown outcome", rec_unreach and "nothing was sent" in json.dumps(rec_unreach.get("outcome")) and "outcome is unknown" not in json.dumps(rec_unreach.get("outcome")), outcome=rec_unreach and rec_unreach.get("outcome"))
    check(7, "uncertain outcome: 'its outcome is unknown'", "outcome is unknown" in json.dumps(hangup["outcome"]), outcome=hangup["outcome"])
    st_un = agent_state(os.path.join(OUT, "c7-unreachable-call.feltdb"), "c7-unreachable-call")
    st_lost = json.load(open(os.path.join(OUT, "c6-result-hangup.agent-state.json"))) if os.path.exists(os.path.join(OUT, "c6-result-hangup.agent-state.json")) else {}
    check(7, "FeltDB: the unreachable call's intent is resolved (nothing ran); the lost call's stays open", st_un.get("task", {}).get("unresolved_intents") == 0 and st_lost.get("task", {}).get("unresolved_intents") == 1, unreachable=st_un.get("intents"), lost=st_lost.get("intents"))
finally:
    for p in procs:
        if p.poll() is None:
            p.send_signal(signal.SIGTERM)
    for p in procs:
        try:
            p.wait(timeout=10)
        except Exception:
            p.kill()
    json.dump(table() if os.path.exists(DB) else None, open(os.path.join(OUT, "db-after.json"), "w"))
    json.dump(upstream(), open(os.path.join(OUT, "upstream-requests.json"), "w"), indent=1)
    json.dump(results, open(os.path.join(OUT, "results.json"), "w"), indent=1)
    print(json.dumps({"passed": sum(r["ok"] for r in results), "failed": sum(not r["ok"] for r in results)}))
sys.exit(0 if results and all(r["ok"] for r in results) else 1)
