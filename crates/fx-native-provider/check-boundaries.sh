#!/usr/bin/env bash
# Dependency and source boundary gate for rust/fx-native-provider.
#
# This crate is the one place native provider I/O lives. It must stay a thin transport:
#   - the graph is limited to deps-allowlist.txt (no async runtime, no provider SDK, no second
#     HTTP stack, no OpenSSL);
#   - the only FX dependencies are fx-core and fx-single-attempt, by path;
#   - the source cannot read the environment or filesystem, spawn processes, hold global
#     mutable state, or make more than one HTTP request;
#   - nothing in the Wasm-bound FX graph may depend on it.
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

# 2. Forbidden families (a clearer message than the allowlist).
forbidden='^(reqwest|hyper.*|h2|tokio.*|async-std|smol|futures.*|mio|curl.*|isahc|surf|openssl.*|native-tls|aws-.*|async-openai|openai.*|anthropic.*|llm.*|easy[-_]llm.*|wasm-bindgen.*|js-sys|web-sys|napi.*)$'
if bad="$(grep -E "$forbidden" <<<"$graph")"; then
  fail "forbidden crates in graph: $(tr '\n' ' ' <<<"$bad")"
fi

# 3. Manifest hygiene: exact dependency set, no git sources, no dev or build dependencies,
#    and the HTTP client stays on rustls with default features off.
deps_section="$(awk '/^\[dependencies\]/{f=1;next} /^\[/{f=0} f' Cargo.toml | grep -Ev '^\s*(#|$)' || true)"
expected='fx-core = { path = "../fx-core" }
fx-single-attempt = { path = "../fx-single-attempt" }
serde_json = "=1.0.154"
ureq = { version = "3", default-features = false, features = ["rustls"] }'
if [[ "$deps_section" != "$expected" ]]; then
  fail "[dependencies] changed; expected exactly:"$'\n'"$expected"$'\n'"found:"$'\n'"${deps_section:-nothing}"
fi
if grep -Eq '^\[(dev-|build-)dependencies\]' Cargo.toml; then
  fail "dev-dependencies and build-dependencies are not allowed"
fi
if grep -Eq 'git\s*=' Cargo.toml; then
  fail "git dependencies are not allowed"
fi

# 4. Source scan: no ambient authority beyond the one socket, no global state, no async.
patterns=(
  'std::env'
  'std::fs'
  'std::process'
  'std::os::'
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
  'static mut'
  'thread_local!'
  'OnceLock'
  'LazyLock'
  'lazy_static'
  'once_cell'
  'Mutex'
  'RwLock'
  # No retry, fallback or routing vocabulary in code paths.
  'fn retry'
  'fn fallback'
  'fn route'
  'max_attempts'
  'backoff'
  # No way to turn TLS verification off (ureq's switch, rustls' dangerous configuration).
  'disable_verification'
  'dangerous'
)
for pattern in "${patterns[@]}"; do
  if hits="$(grep -RnE -- "$pattern" src/)"; then
    fail "forbidden source pattern '$pattern':"$'\n'"$hits"
  fi
done

# 5. Exactly one place makes an HTTP request, and it is the single POST.
posts="$(grep -RnE '\.post\(' src/ | wc -l | tr -d ' ')"
if [[ "$posts" -ne 1 ]]; then
  fail "expected exactly one HTTP request site (.post) in src/, found $posts"
fi
if hits="$(grep -RnE '\.(put|patch|delete|head)\(|agent\s*\.get\(' src/)"; then
  fail "unexpected HTTP request in src/:"$'\n'"$hits"
fi

# 6. No crate in the Wasm-bound FX graph depends on this one.
for other in ../fx-core ../fx-single-attempt ../fx-host ../fx-wasm-abi; do
  if grep -q 'fx-native-provider' "$other/Cargo.toml"; then
    fail "$other depends on fx-native-provider"
  fi
done

if [[ "$status" -eq 0 ]]; then
  printf 'fx-native-provider boundaries ok (%s crates)\n' "$(wc -l <<<"$graph" | tr -d ' ')"
fi
exit "$status"
