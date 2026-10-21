#!/usr/bin/env bash
# Generated bridge → appport dev MCP endpoint → authorized invocation → application state → independent verification.
# Usage: run-e2e.sh <appboundry-repo> <evidence-dir>
set -euo pipefail
REPO=$1; OUT=$2
TOOLS=$(cd "$(dirname "$0")" && pwd)
rm -rf "$OUT"; mkdir -p "$OUT"; W="$OUT/work"; mkdir -p "$W"
exec > >(tee "$OUT/run.log") 2>&1

echo "== versions"; node --version; npm --version
echo "appboundry $(git -C "$REPO" rev-parse HEAD) dirty=$(git -C "$REPO" status --porcelain --untracked-files=no | wc -l)"

echo "== pack the CLI and SDK from the appboundry tree (repo release staging; nothing published)"
node "$TOOLS/pack-local.mjs" "$REPO" "$W/pack"

echo "== clean application checkout"
git clone -q https://github.com/ranazaeem1/invoice-approval-desk.git "$W/desk"
git -C "$W/desk" checkout -q f7291e8
git -C "$W/desk" log --oneline -1
cd "$W/desk"
# The upstream package-lock.json does not satisfy its package.json (npm ci refuses it), so resolve without it.
npm install --no-package-lock --no-audit --no-fund >"$OUT/npm-install.log" 2>&1
npm install --no-save --no-package-lock --no-audit --no-fund "$W/pack/appport-sdk-1.1.20.tgz" "$W/pack/appport-1.1.20.tgz" >>"$OUT/npm-install.log" 2>&1
echo "tracked files changed by install: $(git status --porcelain --untracked-files=no | wc -l)"

echo "== generate"
./node_modules/.bin/appport add --name "Mini Invoice Approval Desk" --id com.example.invoice-desk --version 1.0.0 \
  --capability invoices,invoices.id,invoices.id.status --no-build --no-start --no-verify </dev/null | tee "$OUT/appport-add.txt"
cp appboundry.contract.json appport.bridge.json "$OUT/"
sha256sum appboundry.contract.json appport.bridge.json appport/server.ts appport/runtime/bridge.ts appport/implementation/index.ts | tee "$OUT/generated.sha256"

echo "== start the application"
DB="$W/invoices.db"
DB_PATH="$DB" PORT=3911 ./node_modules/.bin/tsx src/server.ts >"$OUT/app.log" 2>&1 &
APP=$!
READER_TOKEN=$(head -c 24 /dev/urandom | base64 | tr -d '/+=' ); READER_TOKEN="reader-${READER_TOKEN}xxxxxxxxxxxxxxxx"
WRITER_TOKEN=$(head -c 24 /dev/urandom | base64 | tr -d '/+=' ); WRITER_TOKEN="writer-${WRITER_TOKEN}xxxxxxxxxxxxxxxx"
APPPORT_UPSTREAM=http://127.0.0.1:3911 APPPORT_MCP_TOKEN=$READER_TOKEN APPPORT_MCP_SCOPES="invoices.read" \
  ./node_modules/.bin/appport dev --port 3921 --no-studio </dev/null >"$OUT/dev-reader.log" 2>&1 &
R=$!
APPPORT_UPSTREAM=http://127.0.0.1:3911 APPPORT_MCP_TOKEN=$WRITER_TOKEN APPPORT_MCP_SCOPES="invoices.read invoices.write" \
  ./node_modules/.bin/appport dev --port 3922 --no-studio </dev/null >"$OUT/dev-writer.log" 2>&1 &
WR=$!
trap 'kill $APP $R $WR 2>/dev/null || true' EXIT
for port in 3911 3921 3922; do for i in $(seq 1 100); do curl -s -o /dev/null "http://127.0.0.1:$port/" && break; sleep 0.2; done; done
cat "$OUT/dev-writer.log"

echo "== MCP endpoint refuses a caller without the token"
curl -s -o "$OUT/no-token.json" -w "status=%{http_code}\n" -X POST -H 'content-type: application/json' -H 'accept: application/json, text/event-stream' \
  --data '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' http://127.0.0.1:3922/apps/com.example.invoice-desk/mcp; cat "$OUT/no-token.json"

echo "== database before"
node -e 'const D=require(process.argv[2]);const db=new D(process.argv[1],{readonly:true});console.log(JSON.stringify(db.prepare("SELECT id,status,note,updated_at FROM invoices ORDER BY id").all()))' "$DB" "$TOOLS/node_modules/better-sqlite3" | tee "$OUT/db-before.json"

echo "== MCP cases"
set +e
node "$TOOLS/mcp-client-cases.mjs" http://127.0.0.1:3921/apps/com.example.invoice-desk/mcp "$READER_TOKEN" \
  http://127.0.0.1:3922/apps/com.example.invoice-desk/mcp "$WRITER_TOKEN" "$DB" >"$OUT/mcp-cases.json"
STATUS=$?
set -e
grep -E '^(PASS|FAIL)' "$OUT/mcp-cases.json"

echo "== database after"
node -e 'const D=require(process.argv[2]);const db=new D(process.argv[1],{readonly:true});console.log(JSON.stringify(db.prepare("SELECT id,status,note,updated_at FROM invoices ORDER BY id").all()))' "$DB" "$TOOLS/node_modules/better-sqlite3" | tee "$OUT/db-after.json"
echo "exit=$STATUS"
exit $STATUS
