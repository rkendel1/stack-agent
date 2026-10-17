#!/usr/bin/env bash
# Dependency and source boundary gate for crates/fx-wasm-abi (rust/wasm-abi in rkendel1/fx).
#
# The crate has no dependencies and no ambient authority. The macro body is the one
# place raw-pointer code and `extern "C"` appear for every module that ships the ABI
# (fx-host here, Chip's host session in rkendel1/chip-rs), so this gate pins exactly
# that: unsafe tokens and extern exports only in src/lib.rs, nowhere else, and no
# `allow(unsafe_code)` in the library.
set -euo pipefail

crate_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$crate_dir"
status=0

fail() {
  printf 'boundary violation: %s\n' "$1" >&2
  status=1
}

allowed="$(grep -Ev '^\s*(#|$)' deps-allowlist.txt | sort -u)"
graph="$(cargo tree --locked --offline --edges normal,build --prefix none --format '{p}' \
  | sed -E 's/ \(.*//; s/ v[0-9].*$//' | sort -u)"
unexpected="$(comm -13 <(printf '%s\n' "$allowed") <(printf '%s\n' "$graph"))"
if [[ -n "$unexpected" ]]; then
  fail "crates outside deps-allowlist.txt: $(tr '\n' ' ' <<<"$unexpected")"
fi

deps_section="$(awk '/^\[dependencies\]/{f=1;next} /^\[/{f=0} f' Cargo.toml | grep -Ev '^\s*(#|$)' || true)"
if [[ -n "$deps_section" ]]; then
  fail "[dependencies] must be empty, found: $deps_section"
fi
if grep -Eq '^\[(dev-|build-)dependencies\]' Cargo.toml; then
  fail "dev-dependencies and build-dependencies are not allowed"
fi

# Ambient authority never appears, in the macro or anywhere else.
patterns=(
  'std::net' 'std::fs' 'std::process' 'std::env' 'std::thread' 'std::os::'
  'Instant' 'SystemTime' 'UNIX_EPOCH' 'async fn' '\.await'
  'extern crate' '#\[path' 'include!' 'include_str!' 'include_bytes!' 'env!' 'option_env!'
  'static mut' 'thread_local!' 'std::sync' 'OnceLock' 'LazyLock' 'lazy_static' 'once_cell'
)
for pattern in "${patterns[@]}"; do
  if hits="$(grep -RnE -- "$pattern" src/ || true)"; [[ -n "$hits" ]]; then
    fail "forbidden source pattern '$pattern':"$'\n'"$hits"
  fi
done

# Unsafe tokens and extern exports exist only in the macro body in src/lib.rs.
if hits="$(grep -RnE --exclude=lib.rs -- 'unsafe|extern "' src/ || true)"; [[ -n "$hits" ]]; then
  fail "unsafe or extern outside src/lib.rs:"$'\n'"$hits"
fi
if grep -nE '^\s*#!?\[allow\(unsafe_code\)\]' src/lib.rs >/dev/null; then
  fail "the library must not opt in to unsafe code; only a module that expands the macro does"
fi

if [[ "$status" -eq 0 ]]; then
  printf 'fx-wasm-abi boundaries ok (%s crates)\n' "$(wc -l <<<"$graph" | tr -d ' ')"
fi
exit "$status"
