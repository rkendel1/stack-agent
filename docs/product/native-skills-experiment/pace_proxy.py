#!/usr/bin/env python3
"""Request pacer for the native-skill experiment (PREREGISTRATION.md, Amendment 2).

An HTTP CONNECT proxy (Stack-Agent's documented `CHIP_PROXY`) that admits at most one model request
per `--interval` seconds to one allowed host, so a gateway's per-minute request limit is never hit.

Why it terminates TLS: Stack-Agent's client opens a fresh connection per model call, and its 10 s
connect timeout covers TCP, CONNECT and the TLS handshake. So the wait cannot happen before the
handshake. The proxy completes the handshake at once with a certificate from its own local CA (which
only the trial processes trust, through `SSL_CERT_FILE`), reads the complete HTTP request, waits for
the request's slot (the client is then awaiting the response, under its 120 s call deadline), and
forwards the request **byte for byte** over a new TLS connection to the real host. The response is
relayed back byte for byte. Nothing is added, removed, retried or cached.

Credentials: requests carry the API key in a header; the proxy holds it only in memory to forward
it and never logs or stores it. The log records, per request: admission time, wait, request size,
and the response status line.

Usage:
    pace_proxy.py --listen 127.0.0.1:18080 --allow ai-gateway.vercel.sh:443 --interval 15 \
        --cert leaf.pem --key leaf.key --upstream-ca /root/.ccr/ca-bundle.crt --log pace.log
"""
import argparse
import json
import socket
import ssl
import threading
import time

parser = argparse.ArgumentParser()
parser.add_argument("--listen", required=True)
parser.add_argument("--allow", required=True, help="the only host:port a CONNECT may name")
parser.add_argument("--interval", type=float, required=True, help="seconds between admitted requests")
parser.add_argument("--cert", required=True)
parser.add_argument("--key", required=True)
parser.add_argument("--upstream-ca", required=True)
parser.add_argument("--log", required=True)
args = parser.parse_args()

allow_host, allow_port = args.allow.rsplit(":", 1)
allow_port = int(allow_port)
server_ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
server_ctx.load_cert_chain(args.cert, args.key)
upstream_ctx = ssl.create_default_context(cafile=args.upstream_ca)
gate = threading.Lock()
last_admitted = [0.0]
log_lock = threading.Lock()
counter = [0]


def log(entry):
    with log_lock:
        with open(args.log, "a") as f:
            f.write(json.dumps(entry) + "\n")


def admit():
    """Blocks until this request's slot; returns the seconds waited."""
    with gate:
        now = time.monotonic()
        wait = max(0.0, last_admitted[0] + args.interval - now)
        if wait:
            time.sleep(wait)
        last_admitted[0] = time.monotonic()
        return wait


def read_request(conn):
    """The complete HTTP/1.1 request (headers and a Content-Length body), as raw bytes."""
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(65536)
        if not chunk:
            return None
        data += chunk
    head, _, body = data.partition(b"\r\n\r\n")
    length = 0
    for line in head.split(b"\r\n")[1:]:
        name, _, value = line.partition(b":")
        if name.strip().lower() == b"content-length":
            length = int(value.strip())
        if name.strip().lower() == b"transfer-encoding":
            raise ValueError("chunked requests are not supported")
    while len(body) < length:
        chunk = conn.recv(65536)
        if not chunk:
            return None
        body += chunk
    return head + b"\r\n\r\n" + body


def relay(src, dst):
    try:
        while True:
            chunk = src.recv(65536)
            if not chunk:
                break
            dst.sendall(chunk)
    except OSError:
        pass


def handle(client):
    n = None
    try:
        line = b""
        while b"\r\n\r\n" not in line:
            chunk = client.recv(4096)
            if not chunk:
                return
            line += chunk
        target = line.split(b"\r\n")[0].decode("latin-1").split()
        if len(target) < 2 or target[0] != "CONNECT" or target[1] != f"{allow_host}:{allow_port}":
            client.sendall(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            log({"refused": target[:2]})
            return
        client.sendall(b"HTTP/1.1 200 Connection established\r\n\r\n")
        tls = server_ctx.wrap_socket(client, server_side=True)
        request = read_request(tls)
        if request is None:
            return
        with log_lock:
            counter[0] += 1
            n = counter[0]
        wait = admit()
        admitted = time.time()
        up = socket.create_connection((allow_host, allow_port), timeout=130)
        up = upstream_ctx.wrap_socket(up, server_hostname=allow_host)
        up.sendall(request)
        first = up.recv(65536)
        status = first.split(b"\r\n")[0].decode("latin-1") if first else ""
        tls.sendall(first)
        log({"n": n, "admitted_unix": round(admitted, 3), "waited_s": round(wait, 3),
             "request_bytes": len(request), "status": status})
        t = threading.Thread(target=relay, args=(tls, up), daemon=True)
        t.start()
        relay(up, tls)
        for s in (up, tls):
            try:
                s.close()
            except OSError:
                pass
    except Exception as e:  # noqa: BLE001 - every failure is logged, by type only
        log({"n": n, "error": type(e).__name__})
        try:
            client.close()
        except OSError:
            pass


host, port = args.listen.rsplit(":", 1)
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind((host, int(port)))
srv.listen(16)
log({"started_unix": round(time.time(), 3), "interval_s": args.interval, "allow": args.allow})
while True:
    c, _ = srv.accept()
    threading.Thread(target=handle, args=(c,), daemon=True).start()
