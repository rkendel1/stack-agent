#!/usr/bin/env bash
# Dependency and source boundary gate for rust/fx-host.
#
# Fails when the host crate gains a crate outside deps-allowlist.txt (fx-core,
# fx-single-attempt, fx-wasm-abi and their allowlisted graph only), a provider SDK, HTTP/TLS client or async runtime, any
# dependency other than fx-core by relative path, or source that reaches the network, filesystem, processes, environment, threads, wall or
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

# 3. Manifest hygiene: the workspace crates by relative path and the two exactly pinned
#    serde crates, nothing else; no git dependencies, dev-dependencies or build-dependencies.
deps_section="$(awk '/^\[dependencies\]/{f=1;next} /^\[/{f=0} f' Cargo.toml | grep -Ev '^\s*(#|$)' || true)"
expected='fx-core = { path = "../fx-core" }
fx-single-attempt = { path = "../fx-single-attempt" }
fx-wasm-abi = { path = "../fx-wasm-abi" }
serde = { version = "=1.0.229", default-features = false, features = ["std", "derive"] }
serde_json = { version = "=1.0.154", default-features = false, features = ["std"] }'
if [[ "$deps_section" != "$expected" ]]; then
  fail "[dependencies] changed; update this gate and docs/rebuild/ARCHITECTURE.md together. Found:"$'\n'"$deps_section"
fi
if grep -Eq '^\[(dev-|build-)dependencies\]' Cargo.toml; then
  fail "dev-dependencies and build-dependencies are not allowed"
fi

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
# The WebAssembly ABI is the only place unsafe code and extern functions may appear, and
# it must compile for wasm32 only.
abi_only=('unsafe' 'extern "')
for pattern in "${patterns[@]}"; do
  skip=false
  for allowed in "${abi_only[@]}"; do
    [[ "$pattern" == "$allowed" ]] && skip=true
  done
  if "$skip"; then
    hits="$(grep -RnE --exclude=abi.rs -- "$pattern" src/ || true)"
  else
    hits="$(grep -RnE -- "$pattern" src/ || true)"
  fi
  if [[ -n "$hits" ]]; then
    fail "forbidden source pattern '$pattern':"$'\n'"$hits"
  fi
done
if ! grep -Eq '^#\[cfg\(target_arch = "wasm32"\)\]$' src/lib.rs || ! grep -Eq '^mod abi;$' src/lib.rs; then
  fail "src/abi.rs must be declared under #[cfg(target_arch = \"wasm32\")] in src/lib.rs"
fi
if [[ "$(grep -c 'allow(unsafe_code)' src/abi.rs)" -ne 1 ]] || grep -RnE --exclude=abi.rs 'allow\(unsafe_code\)' src/ >/dev/null; then
  fail "allow(unsafe_code) must appear exactly once, in src/abi.rs"
fi

if [[ "$status" -eq 0 ]]; then
  printf 'fx-host boundaries ok (%s crates)\n' "$(wc -l <<<"$graph" | tr -d ' ')"
fi
exit "$status"
