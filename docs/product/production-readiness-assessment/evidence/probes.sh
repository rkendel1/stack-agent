#!/bin/bash
# Offline, deterministic probes behind two findings of production-readiness-assessment.md.
# No model is called: a loopback stand-in for the Anthropic Messages API replays scripted decisions.
# Keys are sentinel strings, never real credentials.
#
#   probes.sh STACK_AGENT_BINARY WORK_DIR
#
# Probe 1 (key exposure): does repository code started by `pax.test` see the provider key, and does its output reach
#   the model? The project's only test prints CHIP_API_KEY and fails; a `pax` wrapper records which key variables it
#   was started with, then runs the real PAX unchanged.
# Probe 2 (acceptance): does a `change` goal verify when the model makes an unrelated edit to a project whose tests
#   already pass? The goal asks for a function `double`; the scripted model writes NOTES.md, runs the tests, completes.
set -u
BIN=$1; W=$2; HERE=$(cd "$(dirname "$0")" && pwd)
PAX_REAL=$(command -v pax) || { echo "pax not on PATH"; exit 3; }
rm -rf "$W"; mkdir -p "$W"
echo "binary: $BIN ($("$BIN" --version | head -1)); sha256 $(sha256sum "$BIN" | cut -c1-16)…; $("$PAX_REAL" --version)"

project() { # name, lib.rs body
  mkdir -p "$W/$1/src"; printf '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2021"\n' "$1" > "$W/$1/Cargo.toml"
  printf '%s\n' "$2" > "$W/$1/src/lib.rs"
  (cd "$W/$1" && git init -q && git add -A && git -c user.email=p@example -c user.name=probe commit -qm init)
}
run() { # name, port, script, PAX_BIN, goal, extra args...
  local name=$1 port=$2 script=$3 paxbin=$4 goal=$5; shift 5
  node "$HERE/fake-anthropic.mjs" "$port" "$W/$name.requests.jsonl" "$script" & local fake=$!; sleep 0.5
  (cd "$W/$name" && env -i PATH="$(dirname "$PAX_REAL"):$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin" HOME="$HOME" \
    STACK_AGENT_DATA_DIR="$W/data-$name" PAX_BIN="$paxbin" CHIP_PROVIDER=anthropic CHIP_MODEL=probe-model \
    CHIP_ENDPOINT="http://127.0.0.1:$port/v1/messages" CHIP_API_KEY=SENTINEL-chip-key-7f3a \
    ANTHROPIC_API_KEY=SENTINEL-ambient-anthropic-91c2 \
    timeout 300 "$BIN" work --no-state --json "$@" "$goal" > "$W/$name.report.json" 2> "$W/$name.stderr.txt")
  echo "exit $?"; kill "$fake" 2>/dev/null; wait "$fake" 2>/dev/null
}

echo; echo "== Probe 1: provider key visible to repository code run by pax.test"
project keyprobe '#[cfg(test)]
mod tests {
    #[test]
    fn reads_environment() {
        let k = std::env::var("CHIP_API_KEY").unwrap_or_default();
        println!("visible to repository code: CHIP_API_KEY={k}");
        assert_eq!(k, "", "repository test saw the provider key");
    }
}'
cat > "$W/pax-wrapper" <<EOF
#!/bin/bash
env | grep -E '^(CHIP_API_KEY|ANTHROPIC_API_KEY|OPENAI_API_KEY)=' | sed 's/=.*/=<present>/' | sort > "$W/pax-env-keys.txt"
exec "$PAX_REAL" "\$@"
EOF
chmod +x "$W/pax-wrapper"
printf '%s\n' '{"decision":"request_capability","capability":"pax.test"}' '{"decision":"complete","summary":"tests run"}' > "$W/keyprobe.script"
run keyprobe 18601 "$W/keyprobe.script" "$W/pax-wrapper" "Run the tests and report whether they pass." --kind verify
echo "key variables in the environment PAX was started with:"; sed 's/^/  /' "$W/pax-env-keys.txt"
echo "model requests: $(wc -l < "$W/keyprobe.requests.jsonl"); sentinel occurrences in request bodies:"
grep -o 'SENTINEL-[a-z-]*-[0-9a-f]*' "$W/keyprobe.requests.jsonl" | sort | uniq -c | sed 's/^/  /'
echo "sentinel occurrences in stack-agent's own report / stderr: $(grep -c SENTINEL "$W/keyprobe.report.json") / $(grep -c SENTINEL "$W/keyprobe.stderr.txt")"
python3 -I -c "import json,sys;r=json.load(open(sys.argv[1]));print('terminal_state:',r['terminal_state'],'verified:',r.get('verified'))" "$W/keyprobe.report.json"

echo; echo "== Probe 2: an unrelated edit on a green baseline verifies a change goal"
project greenprobe 'pub fn one() -> u32 { 1 }
#[cfg(test)]
mod tests { #[test] fn one_is_one() { assert_eq!(super::one(), 1); } }'
printf '%s\n' '{"decision":"request_capability","capability":"project.write","inputs":{"path":"NOTES.md","content":"unrelated note\n","expect_absent":true}}' \
  '{"decision":"request_capability","capability":"pax.test"}' '{"decision":"complete","summary":"done"}' > "$W/greenprobe.script"
run greenprobe 18602 "$W/greenprobe.script" "$PAX_REAL" "Add a public function double(x: u32) -> u32 that returns 2*x, with a unit test."
python3 -I -c "
import json,sys;r=json.load(open(sys.argv[1]));c=r['completion']
print('terminal_state:',r['terminal_state'],'verified:',r.get('verified'),'acceptance_verified_by_runtime:',c['acceptance_verified_by_runtime'])
print('tests:',json.dumps(c['tests']['last']))" "$W/greenprobe.report.json"
echo "model calls: $(wc -l < "$W/greenprobe.requests.jsonl"); files changed: $(cd "$W/greenprobe" && git status --short --untracked-files=all -- . ':!target' ':!.stack-agent' ':!Cargo.lock' | tr '\n' ' ')"
echo "fn double present in src/lib.rs: $(grep -c 'fn double' "$W/greenprobe/src/lib.rs")"
