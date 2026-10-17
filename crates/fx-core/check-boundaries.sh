#!/usr/bin/env bash
# Dependency and source boundary gate for rust/fx-core.
#
# Fails when the core gains a crate outside deps-allowlist.txt, a provider SDK,
# HTTP/TLS client or async runtime, an unpinned or path/git dependency, or source
# that reaches the network, filesystem, processes, environment, threads, wall or
# monotonic clocks, global mutable state, or code outside the crate.
set -euo pipefail

crate_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$crate_dir"
status=0

fail() {
  printf 'boundary violation: %s\n' "$1" >&2
  status=1
}

# 1. Dependency graph against the allowlist.
allowed="$(grep -Ev '^\s*(#|$)' deps-allowlist.txt | sort -u)"
graph="$(cargo tree --locked --edges normal,build --prefix none --format '{p}' \
  | sed -E 's/ \(.*//; s/ v[0-9].*$//' | sort -u)"
unexpected="$(comm -13 <(printf '%s\n' "$allowed") <(printf '%s\n' "$graph"))"
if [[ -n "$unexpected" ]]; then
  fail "crates outside deps-allowlist.txt: $(tr '\n' ' ' <<<"$unexpected")"
fi

# 2. Explicitly forbidden crate families (clearer message than the allowlist).
forbidden='^(reqwest|hyper.*|h2|http|http-body|tokio.*|async-std|smol|futures.*|mio|ureq|curl.*|isahc|surf|openssl.*|native-tls|rustls.*|ring|aws-.*|async-openai|openai.*|anthropic.*|llm.*|easy[-_]llm.*|libc|nix|wasm-bindgen.*|js-sys|web-sys|napi.*)$'
if bad="$(grep -E "$forbidden" <<<"$graph")"; then
  fail "forbidden crates in graph: $(tr '\n' ' ' <<<"$bad")"
fi

# 3. Manifest hygiene: exact pins, no path or git dependencies, no dev-dependencies
#    that could leak into the core build.
deps_section="$(awk '/^\[dependencies\]/{f=1;next} /^\[/{f=0} f' Cargo.toml)"
while IFS= read -r line; do
  [[ -z "$line" || "$line" =~ ^# ]] && continue
  if ! grep -Eq 'version = "=' <<<"$line"; then
    fail "dependency is not pinned with '=': $line"
  fi
  if grep -Eq '\b(path|git)\s*=' <<<"$line"; then
    fail "path or git dependency: $line"
  fi
done <<<"$deps_section"

# 4. Source scan: no ambient authority, no async runtime, no unsafe, nothing outside the crate.
patterns=(
  'std::net'
  'std::fs'
  'std::process'
  'std::env'
  'std::thread'
  'std::os::'
  'Instant'
  'SystemTime'
  'UNIX_EPOCH'
  'async fn'
  '\.await'
  'unsafe'
  'extern crate'
  'extern "'
  '#\[path'
  'include!'
  'include_str!'
  'include_bytes!'
  'env!'
  'option_env!'
  # Global mutable state would couple independent executions (BM-11).
  'static mut'
  'thread_local!'
  'std::sync'
  'OnceLock'
  'LazyLock'
  'lazy_static'
  'once_cell'
)
for pattern in "${patterns[@]}"; do
  if hits="$(grep -RnE -- "$pattern" src/)"; then
    fail "forbidden source pattern '$pattern':"$'\n'"$hits"
  fi
done

if [[ "$status" -eq 0 ]]; then
  printf 'fx-core boundaries ok (%s crates)\n' "$(wc -l <<<"$graph" | tr -d ' ')"
fi
exit "$status"
